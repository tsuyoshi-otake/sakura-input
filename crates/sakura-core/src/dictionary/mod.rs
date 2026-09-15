//! Borrowed, hostile-input-safe views over Sakura's compiled dictionary image.
//!
//! The engine maps `system.dic` read-only and gives this module the mapped byte
//! slice. Parsing never copies a table or deserializes entries into owned
//! containers: every lookup reads fixed-width little-endian records directly
//! from that slice. `dictc` is the allocating half of the format and uses the
//! constants in [`image_format`] to produce the exact layout read here.

use core::fmt;

use sakura_values::{FixedStr, MAX_PREEDIT_BYTES};

use crate::TextSink;

#[path = "format.rs"]
pub mod image_format;
mod parse;

/// The deliberately small, source-backed relationship vocabulary shown with a
/// dictionary detail.  The compiler never infers synonym or antonym edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DetailRelationKind {
    Alias = 1,
    Related = 2,
    Synonym = 3,
    Antonym = 4,
}

impl DetailRelationKind {
    fn from_byte(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Alias),
            2 => Some(Self::Related),
            3 => Some(Self::Synonym),
            4 => Some(Self::Antonym),
            _ => None,
        }
    }
}

/// Domain and prediction metadata packed into an entry record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EntryFlags(u16);

impl EntryFlags {
    pub const NONE: Self = Self(0);
    pub const IT: Self = Self(1 << 0);
    pub const PREDICTION: Self = Self(1 << 1);
    pub const SPELLING_CORRECTION: Self = Self(1 << 2);
    /// A lexical fragment that is valid after preceding text but must not be
    /// offered at the beginning of an independent conversion query.
    pub const NON_INITIAL: Self = Self(1 << 3);

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for EntryFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// One packed dictionary value. Text remains an image offset until a caller
/// explicitly writes it into a supplied sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub surface_id: u32,
    pub left_id: u16,
    pub right_id: u16,
    pub word_cost: i32,
    /// `i32::MAX` means this entry is not prediction-worthy.
    pub prediction_cost: i32,
    pub flags: EntryFlags,
    /// v1 stores an annotation id; v2 stores the exact ENTR ordinal used to
    /// search AIDX. Keeping this private prevents cross-image construction.
    annotation_locator: u32,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            surface_id: 0,
            left_id: 0,
            right_id: 0,
            word_cost: 0,
            prediction_cost: 0,
            flags: EntryFlags::default(),
            // No valid v1 annotation id or v2 entry ordinal is inferred by
            // default construction. Parsed entries always replace this value.
            annotation_locator: image_format::NO_ANNOTATION,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageVersion {
    V1,
    V2,
}

impl ImageVersion {
    const fn node_len(self) -> usize {
        match self {
            Self::V1 => image_format::NODE_LEN,
            Self::V2 => image_format::NODE_LEN_V2,
        }
    }

    const fn entry_len(self) -> usize {
        match self {
            Self::V1 => image_format::ENTRY_LEN,
            Self::V2 => image_format::ENTRY_LEN_V2,
        }
    }
}

/// One dictionary value whose reading is a prefix of the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixMatch {
    /// UTF-8 byte length of the matched reading prefix.
    pub matched_bytes: usize,
    /// Stable ordinal into this dictionary image's ENTR table.  Optional detail
    /// records are keyed by this exact ordinal, never by surface text alone.
    pub entry_index: usize,
    pub entry: Entry,
}

/// A malformed or unsupported dictionary image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    BadMagic,
    UnsupportedVersion(u16),
    BadHeader,
    BadDirectory,
    MissingTable([u8; 4]),
    DuplicateTable([u8; 4]),
    BadTable([u8; 4]),
    BadTree,
    BadEntry,
    BadUtf8,
    TextOverflow,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("truncated dictionary image"),
            Error::BadMagic => f.write_str("bad dictionary magic"),
            Error::UnsupportedVersion(version) => {
                write!(f, "unsupported dictionary version {version}")
            }
            Error::BadHeader => f.write_str("invalid dictionary header"),
            Error::BadDirectory => f.write_str("invalid dictionary table directory"),
            Error::MissingTable(tag) => write!(f, "missing dictionary table {}", Tag(*tag)),
            Error::DuplicateTable(tag) => {
                write!(f, "duplicate dictionary table {}", Tag(*tag))
            }
            Error::BadTable(tag) => write!(f, "invalid dictionary table {}", Tag(*tag)),
            Error::BadTree => f.write_str("invalid dictionary LOUDS tree"),
            Error::BadEntry => f.write_str("invalid dictionary entry"),
            Error::BadUtf8 => f.write_str("invalid UTF-8 in dictionary text"),
            Error::TextOverflow => f.write_str("dictionary text exceeds the fixed output bound"),
        }
    }
}

impl std::error::Error for Error {}

struct Tag([u8; 4]);

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            let c = if byte.is_ascii_graphic() {
                char::from(byte)
            } else {
                '?'
            };
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Table<'a> {
    tag: [u8; 4],
    bytes: &'a [u8],
    count: usize,
}

#[derive(Debug, Clone, Copy)]
struct Details<'a> {
    index: &'a [u8],
    index_count: usize,
    records: &'a [u8],
    record_count: usize,
    relations: &'a [u8],
    relation_count: usize,
    text_offsets: &'a [u8],
    text_count: usize,
    text: &'a [u8],
}

/// A borrowed detail record for one exact candidate entry.
#[derive(Debug, Clone, Copy)]
pub struct DictionaryDetail<'a> {
    details: Details<'a>,
    record_index: usize,
}

#[derive(Debug, Clone, Copy)]
struct SingleKanji<'a> {
    index: &'a [u8],
    index_count: usize,
    readings: &'a [u8],
    chars: &'a [u8],
    char_count: usize,
    variants: &'a [u8],
    variant_count: usize,
}

/// How a single kanji relates to the character it is a variant of.
///
/// These are the rule groups of the pinned Mozc `variant_rule.txt`, kept as
/// distinct values rather than one "variant" flag because the distinction is
/// exactly what makes the note useful: a reader choosing between 噓 and 嘘
/// needs to know which one is the 印刷標準字体.  The discriminants are an
/// on-disk encoding and must stay stable; zero is reserved so a zeroed record
/// can never decode as a real kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum SingleKanjiVariantKind {
    /// 異体字 — a variant form of the same character.
    Itaiji = 1,
    /// 印刷標準字体 — the form the printing standard prescribes.
    PrintStandard = 2,
    /// 簡易慣用字体 — the simplified form conventional in print.
    SimplifiedConventional = 3,
    /// 旧字体 — the pre-reform form.
    OldForm = 4,
    /// 略字 — an abbreviated form.
    Abbreviated = 5,
    /// 正字 — the orthodox form.
    OrthodoxForm = 6,
    /// 俗字 — a popular non-standard form.
    PopularForm = 7,
    /// 別字 — a distinct character used interchangeably.
    DistinctCharacter = 8,
    /// 本字 — the original form.
    OriginalForm = 9,
}

impl SingleKanjiVariantKind {
    /// The on-disk discriminant.
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Decodes a stored discriminant, rejecting zero and unknown values so a
    /// corrupt or future record drops the note instead of inventing one.
    pub const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Itaiji,
            2 => Self::PrintStandard,
            3 => Self::SimplifiedConventional,
            4 => Self::OldForm,
            5 => Self::Abbreviated,
            6 => Self::OrthodoxForm,
            7 => Self::PopularForm,
            8 => Self::DistinctCharacter,
            9 => Self::OriginalForm,
            _ => return None,
        })
    }

    /// The Japanese term shown to the reader, matching the rule group name in
    /// the pinned source.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Itaiji => "異体字",
            Self::PrintStandard => "印刷標準字体",
            Self::SimplifiedConventional => "簡易慣用字体",
            Self::OldForm => "旧字体",
            Self::Abbreviated => "略字",
            Self::OrthodoxForm => "正字",
            Self::PopularForm => "俗字",
            Self::DistinctCharacter => "別字",
            Self::OriginalForm => "本字",
        }
    }
}

/// A single kanji's relation to the character it varies from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SingleKanjiVariant {
    /// The character the variant rule points at, for example 高 for 髙.
    pub original: char,
    pub kind: SingleKanjiVariantKind,
}

/// A validated set of borrowed fixed-layout views over one image.
#[derive(Clone, Copy)]
pub struct Dictionary<'a> {
    version: ImageVersion,
    class_count: usize,
    entry_count: usize,
    node_count: usize,
    louds: &'a [u8],
    louds_bits: usize,
    nodes: &'a [u8],
    labels: &'a [u8],
    entries: &'a [u8],
    surface_offsets: &'a [u8],
    surface_count: usize,
    surfaces: &'a [u8],
    annotation_offsets: &'a [u8],
    annotation_count: usize,
    annotations: &'a [u8],
    annotation_index: Option<&'a [u8]>,
    annotation_index_count: usize,
    matrix: &'a [u8],
    /// Byte-aligned bunsetsu-boundary rows without their table header, or
    /// `None` for images compiled before the segmenter table existed.
    boundaries: Option<&'a [u8]>,
    details: Option<Details<'a>>,
    single_kanji: Option<SingleKanji<'a>>,
}

impl fmt::Debug for Dictionary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dictionary")
            .field("class_count", &self.class_count)
            .field("entry_count", &self.entry_count)
            .field("node_count", &self.node_count)
            .field("surface_count", &self.surface_count)
            .field("annotation_count", &self.annotation_count)
            .field(
                "detail_count",
                &self.details.map(|details| details.record_count),
            )
            .finish()
    }
}

impl<'a> Dictionary<'a> {
    pub const fn class_count(&self) -> usize {
        self.class_count
    }

    pub const fn entry_count(&self) -> usize {
        self.entry_count
    }

    pub const fn node_count(&self) -> usize {
        self.node_count
    }

    /// Whether this image carries the optional bunsetsu-boundary table.
    pub const fn has_bunsetsu_boundaries(&self) -> bool {
        self.boundaries.is_some()
    }

    /// Whether a segment boundary separates a word ending with connection
    /// class `right_id` from a following word starting with class `left_id`.
    ///
    /// `None` means the image predates the boundary table; callers must keep
    /// their existing segment granularity instead of guessing.  Out-of-range
    /// classes report a boundary so corruption can only ever split segments,
    /// never fuse text across a real boundary.
    pub fn bunsetsu_boundary(&self, right_id: u16, left_id: u16) -> Option<bool> {
        let rows = self.boundaries?;
        let (rid, lid) = (usize::from(right_id), usize::from(left_id));
        if rid >= self.class_count || lid >= self.class_count {
            return Some(true);
        }
        let row_bytes = self.class_count.div_ceil(8);
        Some(
            rows.get(rid * row_bytes + lid / 8)
                .is_none_or(|byte| byte & (1u8 << (lid % 8)) != 0),
        )
    }

    /// Whether this image carries the optional single-kanji table.
    pub const fn has_single_kanji(&self) -> bool {
        self.single_kanji.is_some()
    }

    /// The single kanji the pinned source lists for `reading`, in its
    /// preference order, or an empty iterator when the reading names none.
    ///
    /// This is deliberately not part of conversion search.  One short reading
    /// can name hundreds of characters, so these are appended to a finished
    /// candidate list rather than given lattice edges.
    pub fn single_kanji(&self, reading: &str) -> impl Iterator<Item = char> + '_ {
        let span = self.single_kanji_span(reading);
        let chars = self.single_kanji.map(|table| table.chars).unwrap_or(&[]);
        span.map(move |scalar| {
            let at = scalar * image_format::SINGLE_KANJI_CHAR_LEN;
            // Validated at parse time: every scalar in range decodes.
            read_u32(chars, at)
                .and_then(char::from_u32)
                .unwrap_or('\u{fffd}')
        })
    }

    /// How many single kanji `reading` names, without decoding any of them.
    pub fn single_kanji_count(&self, reading: &str) -> usize {
        self.single_kanji_span(reading).len()
    }

    fn single_kanji_span(&self, reading: &str) -> core::ops::Range<usize> {
        let Some(table) = self.single_kanji else {
            return 0..0;
        };
        let needle = reading.as_bytes();
        let record_at = |record: usize| -> (&[u8], usize, usize) {
            let at = record * image_format::SINGLE_KANJI_INDEX_LEN;
            // Validated at parse time: every record is in range.
            let offset = read_u32(table.index, at).unwrap_or(0) as usize;
            let start = read_u32(table.index, at + 4).unwrap_or(0) as usize;
            let len = usize::from(read_u16(table.index, at + 8).unwrap_or(0));
            let count = usize::from(read_u16(table.index, at + 10).unwrap_or(0));
            (
                table.readings.get(offset..offset + len).unwrap_or(&[]),
                start,
                count,
            )
        };
        let (mut low, mut high) = (0usize, table.index_count);
        while low < high {
            let middle = low + (high - low) / 2;
            let (candidate, start, count) = record_at(middle);
            match candidate.cmp(needle) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => return start..start + count,
            }
        }
        0..0
    }

    /// The variant relation recorded for `kanji`, if the pinned rules name one.
    ///
    /// A character can appear under more than one rule in the source; the
    /// compiler keeps exactly one relation per character so a candidate can
    /// never show two contradictory notes.
    pub fn single_kanji_variant(&self, kanji: char) -> Option<SingleKanjiVariant> {
        let table = self.single_kanji?;
        let needle = u32::from(kanji);
        let (mut low, mut high) = (0usize, table.variant_count);
        while low < high {
            let middle = low + (high - low) / 2;
            let at = middle * image_format::SINGLE_KANJI_VARIANT_LEN;
            let variant = read_u32(table.variants, at)?;
            match variant.cmp(&needle) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => {
                    return Some(SingleKanjiVariant {
                        original: char::from_u32(read_u32(table.variants, at + 4)?)?,
                        kind: SingleKanjiVariantKind::from_code(*table.variants.get(at + 8)?)?,
                    });
                }
            }
        }
        None
    }

    /// Encoded connection-table bytes, exposed for the release size gate.
    pub const fn matrix_bytes_len(&self) -> usize {
        self.matrix.len()
    }

    /// Returns `matrix[previous_right_id][next_left_id]`.
    pub fn connection_cost(&self, previous_right_id: u16, next_left_id: u16) -> Option<u16> {
        let row = usize::from(previous_right_id);
        let column = usize::from(next_left_id);
        if row >= self.class_count || column >= self.class_count {
            return None;
        }
        let modes_at = image_format::MATRIX_HEADER_LEN;
        let mode = read_u16(self.matrix, modes_at.checked_add(row.checked_mul(2)?)?)?;
        let rows_at = align_up_4(modes_at.checked_add(self.class_count.checked_mul(2)?)?)?;
        let descriptor = rows_at.checked_add(row.checked_mul(image_format::MATRIX_ROW_LEN)?)?;
        let start = to_usize(read_u32(self.matrix, descriptor)?).ok()?;
        let count = to_usize(read_u32(self.matrix, descriptor + 4)?).ok()?;
        let overrides_at =
            rows_at.checked_add(self.class_count.checked_mul(image_format::MATRIX_ROW_LEN)?)?;

        let mut low = 0usize;
        let mut high = count;
        while low < high {
            let middle = low + (high - low) / 2;
            let index = start.checked_add(middle)?;
            let at =
                overrides_at.checked_add(index.checked_mul(image_format::MATRIX_OVERRIDE_LEN)?)?;
            let left_id = usize::from(read_u16(self.matrix, at)?);
            match left_id.cmp(&column) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => return read_u16(self.matrix, at + 2),
            }
        }
        Some(mode)
    }

    /// Calls `visit` for every entry whose reading is a prefix of `reading`.
    /// Returning `false` from the callback stops enumeration immediately.
    pub fn common_prefix_search(
        &self,
        reading: &str,
        mut visit: impl FnMut(PrefixMatch) -> bool,
    ) -> Result<(), Error> {
        let mut node_index = 0usize;
        let mut matched_bytes = 0usize;
        for label in reading.chars() {
            let node = self.node(node_index)?;
            let Some(child) = self.find_child(node, label) else {
                break;
            };
            node_index = child;
            matched_bytes += label.len_utf8();
            let terminal = self.node(node_index)?;
            let end = terminal
                .value_start
                .checked_add(terminal.value_count)
                .ok_or(Error::BadTree)?;
            for entry_index in terminal.value_start..end {
                let entry = self.entry(entry_index)?;
                if !visit(PrefixMatch {
                    matched_bytes,
                    entry_index,
                    entry,
                }) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Visits entries below the trie node identified by `reading_prefix`.
    ///
    /// Exact entries on the prefix node are intentionally skipped: this API is
    /// for bounded completion/coherence lookups, not ordinary conversion. Both
    /// node and entry work are caller-bounded, and the fixed traversal stack
    /// keeps the keystroke path allocation-free even for hostile valid images.
    pub fn visit_descendant_entries(
        &self,
        reading_prefix: &str,
        node_budget: usize,
        entry_budget: usize,
        mut visit: impl FnMut(Entry) -> bool,
    ) -> Result<(), Error> {
        if node_budget == 0 || entry_budget == 0 {
            return Ok(());
        }

        let mut node_index = 0usize;
        for label in reading_prefix.chars() {
            let node = self.node(node_index)?;
            let Some(child) = self.find_child(node, label) else {
                return Ok(());
            };
            node_index = child;
        }

        #[derive(Clone, Copy)]
        struct Frame {
            node_index: usize,
            next_child: usize,
            values_visited: bool,
        }
        const EMPTY: Frame = Frame {
            node_index: 0,
            next_child: 0,
            values_visited: false,
        };

        let mut stack = [EMPTY; MAX_PREEDIT_BYTES + 1];
        // Mark the prefix node's values visited so only strict descendants are
        // returned. Its children are still traversed normally.
        stack[0] = Frame {
            node_index,
            next_child: 0,
            values_visited: true,
        };
        let mut depth = 1usize;
        let mut visited_nodes = 0usize;
        let mut visited_entries = 0usize;

        while depth != 0 && visited_nodes < node_budget && visited_entries < entry_budget {
            let frame_index = depth - 1;
            let frame = stack[frame_index];
            let node = self.node(frame.node_index)?;
            if !frame.values_visited {
                stack[frame_index].values_visited = true;
                visited_nodes += 1;
                let end = node
                    .value_start
                    .checked_add(node.value_count)
                    .ok_or(Error::BadTree)?;
                for entry_index in node.value_start..end {
                    if visited_entries >= entry_budget {
                        return Ok(());
                    }
                    visited_entries += 1;
                    if !visit(self.entry(entry_index)?) {
                        return Ok(());
                    }
                }
                continue;
            }

            if frame.next_child < node.child_count {
                let child = node
                    .first_child
                    .checked_add(frame.next_child)
                    .ok_or(Error::BadTree)?;
                stack[frame_index].next_child += 1;
                if depth >= stack.len() {
                    return Err(Error::BadTree);
                }
                stack[depth] = Frame {
                    node_index: child,
                    next_child: 0,
                    values_visited: false,
                };
                depth += 1;
            } else {
                depth -= 1;
            }
        }
        Ok(())
    }

    /// Visits every entry marked for prediction together with its complete
    /// reading. The walk is iterative, so even a valid but pathologically deep
    /// hostile image cannot overflow the process stack.
    ///
    /// This is intentionally an index-construction API, not a keystroke-path
    /// query: callers build one compact process-wide prediction index at
    /// startup and scan that bounded index for each prefix. Returning `false`
    /// stops the walk immediately.
    pub fn visit_prediction_entries(
        &self,
        mut visit: impl FnMut(&str, Entry) -> bool,
    ) -> Result<(), Error> {
        self.visit_indexed_prediction_entries(|reading, _, entry| visit(reading, entry))
    }

    /// Visits every prediction entry together with its image entry index.
    ///
    /// The stable numeric index lets process-wide auxiliary indexes retain a
    /// four-byte reference into the mapped image instead of copying the
    /// materialized runtime entry into private working memory.
    pub fn visit_indexed_prediction_entries(
        &self,
        mut visit: impl FnMut(&str, usize, Entry) -> bool,
    ) -> Result<(), Error> {
        self.visit_entries_indexed(|reading, index, entry| {
            !entry.flags.contains(EntryFlags::PREDICTION)
                || entry.prediction_cost == i32::MAX
                || visit(reading, index, entry)
        })
    }

    /// Reads one entry from the already validated mapped image.
    ///
    /// This complements [`Self::visit_indexed_prediction_entries`]; callers
    /// can keep compact entry indexes and materialize a record only while it is
    /// being ranked or rendered.
    pub fn entry_at(&self, index: usize) -> Result<Entry, Error> {
        self.entry(index)
    }

    /// Visits every dictionary entry with its complete reading. Reconversion
    /// uses this cold-path walk to recover a reading from selected committed
    /// text without carrying a second, multi-megabyte reverse index in the
    /// resident engine. Returning `false` stops immediately.
    pub fn visit_entries(&self, mut visit: impl FnMut(&str, Entry) -> bool) -> Result<(), Error> {
        self.visit_entries_indexed(|reading, _, entry| visit(reading, entry))
    }

    fn visit_entries_indexed(
        &self,
        mut visit: impl FnMut(&str, usize, Entry) -> bool,
    ) -> Result<(), Error> {
        #[derive(Clone, Copy)]
        struct Frame {
            node_index: usize,
            next_child: usize,
            values_visited: bool,
        }

        let mut reading = FixedStr::<MAX_PREEDIT_BYTES>::new();
        let mut stack = Vec::with_capacity(32);
        stack.push(Frame {
            node_index: 0,
            next_child: 0,
            values_visited: false,
        });

        while let Some(frame_index) = stack.len().checked_sub(1) {
            let frame = stack[frame_index];
            let node = self.node(frame.node_index)?;

            if !frame.values_visited {
                stack[frame_index].values_visited = true;
                let end = node
                    .value_start
                    .checked_add(node.value_count)
                    .ok_or(Error::BadTree)?;
                for entry_index in node.value_start..end {
                    let entry = self.entry(entry_index)?;
                    if !visit(reading.as_str(), entry_index, entry) {
                        return Ok(());
                    }
                }
                continue;
            }

            if frame.next_child < node.child_count {
                let child = node
                    .first_child
                    .checked_add(frame.next_child)
                    .ok_or(Error::BadTree)?;
                stack[frame_index].next_child += 1;
                reading
                    .push(self.label(child)?)
                    .map_err(|_| Error::TextOverflow)?;
                stack.push(Frame {
                    node_index: child,
                    next_child: 0,
                    values_visited: false,
                });
                continue;
            }

            stack.pop();
            if !stack.is_empty() {
                let _ = reading.pop_char();
            }
        }
        Ok(())
    }

    /// Reconstructs an entry's front-coded surface into `sink`.
    pub fn write_surface(&self, entry: Entry, sink: &mut impl TextSink) -> Result<(), Error> {
        use image_format::SURFACE_RESTART_INTERVAL;

        let surface_id = to_usize(entry.surface_id)?;
        if surface_id >= self.surface_count {
            return Err(Error::BadEntry);
        }
        let restart = surface_id - (surface_id % SURFACE_RESTART_INTERVAL);
        let mut value = FixedStr::<MAX_PREEDIT_BYTES>::new();
        let mut v2_cursor = match self.version {
            ImageVersion::V1 => None,
            ImageVersion::V2 => Some(
                read_offset(self.surface_offsets, restart / SURFACE_RESTART_INTERVAL)
                    .ok_or(Error::BadTable(image_format::TAG_SURFACE_OFFSETS))?,
            ),
        };
        for index in restart..=surface_id {
            let record = match v2_cursor {
                None => self.text_record(
                    self.surface_offsets,
                    self.surface_count,
                    self.surfaces,
                    index,
                )?,
                Some(cursor) => {
                    let suffix_at = cursor
                        .checked_add(2)
                        .ok_or(Error::BadTable(image_format::TAG_SURFACES))?;
                    let suffix_len = usize::from(
                        read_u16(self.surfaces, suffix_at)
                            .ok_or(Error::BadTable(image_format::TAG_SURFACES))?,
                    );
                    let end = cursor
                        .checked_add(4)
                        .and_then(|at| at.checked_add(suffix_len))
                        .ok_or(Error::BadTable(image_format::TAG_SURFACES))?;
                    let record = self
                        .surfaces
                        .get(cursor..end)
                        .ok_or(Error::BadTable(image_format::TAG_SURFACES))?;
                    v2_cursor = Some(end);
                    record
                }
            };
            if record.len() < 4 {
                return Err(Error::BadTable(image_format::TAG_SURFACES));
            }
            let prefix = usize::from(
                read_u16(record, 0).ok_or(Error::BadTable(image_format::TAG_SURFACES))?,
            );
            let suffix_len = usize::from(
                read_u16(record, 2).ok_or(Error::BadTable(image_format::TAG_SURFACES))?,
            );
            if record.len() != 4 + suffix_len
                || prefix > value.len()
                || !value.as_str().is_char_boundary(prefix)
                || (index == restart && prefix != 0)
            {
                return Err(Error::BadTable(image_format::TAG_SURFACES));
            }
            let suffix = core::str::from_utf8(&record[4..]).map_err(|_| Error::BadUtf8)?;
            let keep_chars = value.as_str()[..prefix].chars().count();
            let remove_chars = value.as_str().chars().count() - keep_chars;
            value.truncate_chars(remove_chars);
            value.push_str(suffix).map_err(|_| Error::TextOverflow)?;
        }
        sink.push_str(value.as_str())
            .map_err(|_| Error::TextOverflow)
    }

    /// Writes the optional annotation for `entry`; a missing annotation writes
    /// nothing and succeeds.
    pub fn write_annotation(&self, entry: Entry, sink: &mut impl TextSink) -> Result<(), Error> {
        let annotation_id = match self.version {
            ImageVersion::V1 => {
                if entry.annotation_locator == image_format::NO_ANNOTATION {
                    return Ok(());
                }
                to_usize(entry.annotation_locator)?
            }
            ImageVersion::V2 => {
                let wanted = entry.annotation_locator;
                let index = self
                    .annotation_index
                    .ok_or(Error::BadTable(image_format::TAG_ANNOTATION_INDEX))?;
                let mut low = 0usize;
                let mut high = self.annotation_index_count;
                let found = loop {
                    if low >= high {
                        break None;
                    }
                    let middle = low + (high - low) / 2;
                    let at = middle
                        .checked_mul(image_format::ANNOTATION_INDEX_LEN_V2)
                        .ok_or(Error::BadTable(image_format::TAG_ANNOTATION_INDEX))?;
                    let ordinal = read_u32(index, at)
                        .ok_or(Error::BadTable(image_format::TAG_ANNOTATION_INDEX))?;
                    match ordinal.cmp(&wanted) {
                        core::cmp::Ordering::Less => low = middle + 1,
                        core::cmp::Ordering::Greater => high = middle,
                        core::cmp::Ordering::Equal => {
                            break Some(to_usize(
                                read_u32(index, at + 4)
                                    .ok_or(Error::BadTable(image_format::TAG_ANNOTATION_INDEX))?,
                            )?)
                        }
                    }
                };
                let Some(annotation_id) = found else {
                    return Ok(());
                };
                annotation_id
            }
        };
        let record = self.text_record(
            self.annotation_offsets,
            self.annotation_count,
            self.annotations,
            annotation_id,
        )?;
        let text = core::str::from_utf8(record).map_err(|_| Error::BadUtf8)?;
        sink.push_str(text).map_err(|_| Error::TextOverflow)
    }

    /// Returns source-backed details for one exact ENTR-table ordinal.  Old
    /// images return `Ok(None)`; surface-only lookup is intentionally absent
    /// because homographs may have unrelated meanings.
    pub fn detail_at(&self, entry_index: usize) -> Result<Option<DictionaryDetail<'a>>, Error> {
        let Some(details) = self.details else {
            return Ok(None);
        };
        if entry_index >= self.entry_count {
            return Err(Error::BadEntry);
        }
        let wanted = entry_index;
        let mut low = 0usize;
        let mut high = details.index_count;
        while low < high {
            let middle = low + (high - low) / 2;
            let at = middle
                .checked_mul(image_format::DETAIL_INDEX_LEN)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_INDEX))?;
            let indexed_entry = to_usize(
                read_u32(details.index, at)
                    .ok_or(Error::BadTable(image_format::TAG_DETAIL_INDEX))?,
            )?;
            match indexed_entry.cmp(&wanted) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => {
                    let record_index = to_usize(
                        read_u32(details.index, at + 4)
                            .ok_or(Error::BadTable(image_format::TAG_DETAIL_INDEX))?,
                    )?;
                    if record_index >= details.record_count {
                        return Err(Error::BadTable(image_format::TAG_DETAIL_INDEX));
                    }
                    return Ok(Some(DictionaryDetail {
                        details,
                        record_index,
                    }));
                }
            }
        }
        Ok(None)
    }

    fn validate_tables(&self) -> Result<(), Error> {
        use image_format as format;

        let expected_bits = self
            .node_count
            .checked_mul(2)
            .and_then(|n| n.checked_sub(1))
            .ok_or(Error::BadTree)?;
        if self.louds_bits != expected_bits {
            return Err(Error::BadTree);
        }
        if self.label(0)? != '\0' {
            return Err(Error::BadTree);
        }

        let mut bit = 0usize;
        let mut edge_count = 0usize;
        for node_index in 0..self.node_count {
            // Validate every stored scalar, including nodes a malformed child
            // range might otherwise leave unobserved.
            self.label(node_index)?;
            let node = self.node(node_index)?;
            if node.value_start > self.entry_count
                || node.value_count > self.entry_count - node.value_start
            {
                return Err(Error::BadTree);
            }
            if node.child_count == 0 {
                if node.first_child != 0 {
                    return Err(Error::BadTree);
                }
            } else {
                if node.first_child == 0
                    || node.first_child > self.node_count
                    || node.child_count > self.node_count - node.first_child
                {
                    return Err(Error::BadTree);
                }
                let mut previous = None;
                for child in node.first_child..node.first_child + node.child_count {
                    let label = self.label(child)?;
                    if previous.is_some_and(|before| before >= label) {
                        return Err(Error::BadTree);
                    }
                    previous = Some(label);
                }
            }
            for _ in 0..node.child_count {
                if !self.louds_bit(bit)? {
                    return Err(Error::BadTree);
                }
                bit += 1;
                edge_count += 1;
            }
            if self.louds_bit(bit)? {
                return Err(Error::BadTree);
            }
            bit += 1;
        }
        if bit != self.louds_bits || edge_count + 1 != self.node_count {
            return Err(Error::BadTree);
        }
        for padding in self.louds_bits..self.louds.len() * 8 {
            if bit_at(self.louds, padding).unwrap_or(true) {
                return Err(Error::BadTree);
            }
        }

        match self.version {
            ImageVersion::V1 => self.validate_offsets(
                self.surface_offsets,
                self.surface_count,
                self.surfaces,
                format::TAG_SURFACES,
            )?,
            ImageVersion::V2 => self.validate_v2_surfaces()?,
        }

        if let Some(details) = self.details {
            self.validate_details(details)?;
        }
        self.validate_offsets(
            self.annotation_offsets,
            self.annotation_count,
            self.annotations,
            format::TAG_ANNOTATIONS,
        )?;
        if self.version == ImageVersion::V2 {
            self.validate_v2_annotations()?;
            self.validate_v2_annotation_index()?;
        }

        for index in 0..self.entry_count {
            let entry = self.entry(index)?;
            if to_usize(entry.surface_id)? >= self.surface_count
                || usize::from(entry.left_id) >= self.class_count
                || usize::from(entry.right_id) >= self.class_count
                || (self.version == ImageVersion::V1
                    && entry.annotation_locator != format::NO_ANNOTATION
                    && to_usize(entry.annotation_locator)? >= self.annotation_count)
            {
                return Err(Error::BadEntry);
            }
        }
        Ok(())
    }

    fn validate_v2_surfaces(&self) -> Result<(), Error> {
        use image_format as format;

        let restart_count = self
            .surface_count
            .checked_add(format::SURFACE_RESTART_INTERVAL - 1)
            .ok_or(Error::BadTable(format::TAG_SURFACE_OFFSETS))?
            / format::SURFACE_RESTART_INTERVAL;
        if restart_count == 0 {
            if !self.surface_offsets.is_empty() || !self.surfaces.is_empty() {
                return Err(Error::BadTable(format::TAG_SURFACES));
            }
            return Ok(());
        }

        let mut previous_offset = None;
        let mut decoded = 0usize;
        for restart_index in 0..restart_count {
            let start = read_offset(self.surface_offsets, restart_index)
                .ok_or(Error::BadTable(format::TAG_SURFACE_OFFSETS))?;
            let end = if restart_index + 1 < restart_count {
                read_offset(self.surface_offsets, restart_index + 1)
                    .ok_or(Error::BadTable(format::TAG_SURFACE_OFFSETS))?
            } else {
                self.surfaces.len()
            };
            if (restart_index == 0 && start != 0)
                || start >= self.surfaces.len()
                || end > self.surfaces.len()
                || start >= end
                || previous_offset.is_some_and(|previous| previous >= start)
            {
                return Err(Error::BadTable(format::TAG_SURFACE_OFFSETS));
            }
            previous_offset = Some(start);

            let remaining = self.surface_count - decoded;
            let records = remaining.min(format::SURFACE_RESTART_INTERVAL);
            let mut cursor = start;
            let mut value = FixedStr::<MAX_PREEDIT_BYTES>::new();
            for record_index in 0..records {
                let prefix = usize::from(
                    read_u16(self.surfaces, cursor).ok_or(Error::BadTable(format::TAG_SURFACES))?,
                );
                let suffix_at = cursor
                    .checked_add(2)
                    .ok_or(Error::BadTable(format::TAG_SURFACES))?;
                let suffix_len = usize::from(
                    read_u16(self.surfaces, suffix_at)
                        .ok_or(Error::BadTable(format::TAG_SURFACES))?,
                );
                let suffix_start = cursor
                    .checked_add(4)
                    .ok_or(Error::BadTable(format::TAG_SURFACES))?;
                let record_end = suffix_start
                    .checked_add(suffix_len)
                    .ok_or(Error::BadTable(format::TAG_SURFACES))?;
                if record_end > end
                    || prefix > value.len()
                    || !value.as_str().is_char_boundary(prefix)
                    || (record_index == 0 && prefix != 0)
                {
                    return Err(Error::BadTable(format::TAG_SURFACES));
                }
                let suffix = core::str::from_utf8(
                    self.surfaces
                        .get(suffix_start..record_end)
                        .ok_or(Error::BadTable(format::TAG_SURFACES))?,
                )
                .map_err(|_| Error::BadUtf8)?;
                let keep_chars = value.as_str()[..prefix].chars().count();
                let remove_chars = value.as_str().chars().count() - keep_chars;
                value.truncate_chars(remove_chars);
                value.push_str(suffix).map_err(|_| Error::TextOverflow)?;
                cursor = record_end;
                decoded += 1;
            }
            if cursor != end {
                return Err(Error::BadTable(format::TAG_SURFACES));
            }
        }
        if decoded != self.surface_count {
            return Err(Error::BadTable(format::TAG_SURFACES));
        }
        Ok(())
    }

    fn validate_v2_annotations(&self) -> Result<(), Error> {
        use image_format as format;

        if self.annotation_count == 0 {
            if !self.annotation_offsets.is_empty() || !self.annotations.is_empty() {
                return Err(Error::BadTable(format::TAG_ANNOTATIONS));
            }
            return Ok(());
        }
        if read_offset(self.annotation_offsets, 0) != Some(0) {
            return Err(Error::BadTable(format::TAG_ANNOTATION_OFFSETS));
        }
        for index in 0..self.annotation_count {
            let record = self.text_record(
                self.annotation_offsets,
                self.annotation_count,
                self.annotations,
                index,
            )?;
            core::str::from_utf8(record).map_err(|_| Error::BadUtf8)?;
        }
        Ok(())
    }

    fn validate_v2_annotation_index(&self) -> Result<(), Error> {
        use image_format as format;

        let index = self
            .annotation_index
            .ok_or(Error::MissingTable(format::TAG_ANNOTATION_INDEX))?;
        let mut previous_entry = None;
        for record_index in 0..self.annotation_index_count {
            let at = record_index
                .checked_mul(format::ANNOTATION_INDEX_LEN_V2)
                .ok_or(Error::BadTable(format::TAG_ANNOTATION_INDEX))?;
            let entry = to_usize(
                read_u32(index, at).ok_or(Error::BadTable(format::TAG_ANNOTATION_INDEX))?,
            )?;
            let annotation = to_usize(
                read_u32(index, at + 4).ok_or(Error::BadTable(format::TAG_ANNOTATION_INDEX))?,
            )?;
            if entry >= self.entry_count
                || annotation >= self.annotation_count
                || previous_entry.is_some_and(|previous| previous >= entry)
            {
                return Err(Error::BadTable(format::TAG_ANNOTATION_INDEX));
            }
            previous_entry = Some(entry);
        }
        Ok(())
    }

    fn validate_details(&self, details: Details<'a>) -> Result<(), Error> {
        use image_format as format;

        self.validate_offsets(
            details.text_offsets,
            details.text_count,
            details.text,
            format::TAG_DETAIL_TEXT,
        )?;
        let mut previous_entry = None;
        for index in 0..details.index_count {
            let at = index * format::DETAIL_INDEX_LEN;
            let entry = to_usize(
                read_u32(details.index, at).ok_or(Error::BadTable(format::TAG_DETAIL_INDEX))?,
            )?;
            let record = to_usize(
                read_u32(details.index, at + 4).ok_or(Error::BadTable(format::TAG_DETAIL_INDEX))?,
            )?;
            if entry >= self.entry_count
                || record >= details.record_count
                || previous_entry.is_some_and(|previous| previous >= entry)
            {
                return Err(Error::BadTable(format::TAG_DETAIL_INDEX));
            }
            previous_entry = Some(entry);
        }
        for index in 0..details.record_count {
            let at = index * format::DETAIL_RECORD_LEN;
            let description = to_usize(
                read_u32(details.records, at).ok_or(Error::BadTable(format::TAG_DETAIL_RECORDS))?,
            )?;
            let display = to_usize(
                read_u32(details.records, at + 4)
                    .ok_or(Error::BadTable(format::TAG_DETAIL_RECORDS))?,
            )?;
            let relation_start = to_usize(
                read_u32(details.records, at + 8)
                    .ok_or(Error::BadTable(format::TAG_DETAIL_RECORDS))?,
            )?;
            let relation_count = to_usize(
                read_u32(details.records, at + 12)
                    .ok_or(Error::BadTable(format::TAG_DETAIL_RECORDS))?,
            )?;
            if description >= details.text_count
                || display >= details.text_count
                || relation_start
                    .checked_add(relation_count)
                    .is_none_or(|end| end > details.relation_count)
            {
                return Err(Error::BadTable(format::TAG_DETAIL_RECORDS));
            }
            self.detail_text(details, description)?;
            self.detail_text(details, display)?;
        }
        for index in 0..details.relation_count {
            let at = index * format::DETAIL_RELATION_LEN;
            if details.relations.get(at + 1..at + 4) != Some(&[0, 0, 0])
                || DetailRelationKind::from_byte(
                    *details
                        .relations
                        .get(at)
                        .ok_or(Error::BadTable(format::TAG_DETAIL_RELATIONS))?,
                )
                .is_none()
            {
                return Err(Error::BadTable(format::TAG_DETAIL_RELATIONS));
            }
            let text = to_usize(
                read_u32(details.relations, at + 4)
                    .ok_or(Error::BadTable(format::TAG_DETAIL_RELATIONS))?,
            )?;
            if text >= details.text_count {
                return Err(Error::BadTable(format::TAG_DETAIL_RELATIONS));
            }
            self.detail_text(details, text)?;
        }
        Ok(())
    }

    fn detail_text(&self, details: Details<'a>, index: usize) -> Result<&'a str, Error> {
        let record = self.text_record(
            details.text_offsets,
            details.text_count,
            details.text,
            index,
        )?;
        core::str::from_utf8(record).map_err(|_| Error::BadUtf8)
    }

    fn validate_offsets(
        &self,
        offsets: &[u8],
        count: usize,
        data: &[u8],
        tag: [u8; 4],
    ) -> Result<(), Error> {
        if count == 0 {
            return Ok(());
        }
        let mut previous = None;
        for index in 0..count {
            let offset = read_offset(offsets, index).ok_or(Error::BadTable(tag))?;
            if offset >= data.len() || previous.is_some_and(|before| before >= offset) {
                return Err(Error::BadTable(tag));
            }
            previous = Some(offset);
        }
        Ok(())
    }

    fn text_record<'b>(
        &self,
        offsets: &'b [u8],
        count: usize,
        data: &'b [u8],
        index: usize,
    ) -> Result<&'b [u8], Error> {
        if index >= count {
            return Err(Error::BadEntry);
        }
        let start = read_offset(offsets, index).ok_or(Error::BadEntry)?;
        let end = if index + 1 < count {
            read_offset(offsets, index + 1).ok_or(Error::BadEntry)?
        } else {
            data.len()
        };
        data.get(start..end).ok_or(Error::BadEntry)
    }

    fn node(&self, index: usize) -> Result<Node, Error> {
        if index >= self.node_count {
            return Err(Error::BadTree);
        }
        let at = index
            .checked_mul(self.version.node_len())
            .ok_or(Error::BadTree)?;
        if self.version == ImageVersion::V1 && read_u32(self.nodes, at + 12) != Some(0) {
            return Err(Error::BadTree);
        }
        Ok(Node {
            first_child: to_usize(read_u32(self.nodes, at).ok_or(Error::BadTree)?)?,
            child_count: usize::from(read_u16(self.nodes, at + 4).ok_or(Error::BadTree)?),
            value_count: usize::from(read_u16(self.nodes, at + 6).ok_or(Error::BadTree)?),
            value_start: to_usize(read_u32(self.nodes, at + 8).ok_or(Error::BadTree)?)?,
        })
    }

    fn label(&self, index: usize) -> Result<char, Error> {
        if index >= self.node_count {
            return Err(Error::BadTree);
        }
        let scalar = match self.version {
            ImageVersion::V1 => {
                let at = index.checked_mul(4).ok_or(Error::BadTree)?;
                read_u32(self.labels, at).ok_or(Error::BadTree)?
            }
            ImageVersion::V2 => {
                let at = index
                    .checked_mul(image_format::NODE_LEN_V2)
                    .and_then(|at| at.checked_add(12))
                    .ok_or(Error::BadTree)?;
                read_u32(self.nodes, at).ok_or(Error::BadTree)?
            }
        };
        char::from_u32(scalar).ok_or(Error::BadTree)
    }

    fn entry(&self, index: usize) -> Result<Entry, Error> {
        if index >= self.entry_count {
            return Err(Error::BadEntry);
        }
        let at = index
            .checked_mul(self.version.entry_len())
            .ok_or(Error::BadEntry)?;
        match self.version {
            ImageVersion::V1 => Ok(Entry {
                surface_id: read_u32(self.entries, at).ok_or(Error::BadEntry)?,
                left_id: read_u16(self.entries, at + 4).ok_or(Error::BadEntry)?,
                right_id: read_u16(self.entries, at + 6).ok_or(Error::BadEntry)?,
                word_cost: read_i32(self.entries, at + 8).ok_or(Error::BadEntry)?,
                prediction_cost: read_i32(self.entries, at + 12).ok_or(Error::BadEntry)?,
                flags: EntryFlags::from_bits(
                    read_u16(self.entries, at + 16).ok_or(Error::BadEntry)?,
                ),
                annotation_locator: read_u32(self.entries, at + 20).ok_or(Error::BadEntry)?,
            }),
            ImageVersion::V2 => {
                let prediction = read_u16(self.entries, at + 10).ok_or(Error::BadEntry)?;
                if read_u16(self.entries, at + 14) != Some(0) {
                    return Err(Error::BadEntry);
                }
                Ok(Entry {
                    surface_id: read_u32(self.entries, at).ok_or(Error::BadEntry)?,
                    left_id: read_u16(self.entries, at + 4).ok_or(Error::BadEntry)?,
                    right_id: read_u16(self.entries, at + 6).ok_or(Error::BadEntry)?,
                    word_cost: i32::from(read_u16(self.entries, at + 8).ok_or(Error::BadEntry)?),
                    prediction_cost: if prediction == u16::MAX {
                        i32::MAX
                    } else {
                        i32::from(prediction)
                    },
                    flags: EntryFlags::from_bits(
                        read_u16(self.entries, at + 12).ok_or(Error::BadEntry)?,
                    ),
                    annotation_locator: u32::try_from(index).map_err(|_| Error::BadEntry)?,
                })
            }
        }
    }

    fn find_child(&self, node: Node, wanted: char) -> Option<usize> {
        let mut low = node.first_child;
        let mut high = node.first_child.checked_add(node.child_count)?;
        while low < high {
            let mid = low + (high - low) / 2;
            let label = self.label(mid).ok()?;
            match label.cmp(&wanted) {
                core::cmp::Ordering::Less => low = mid + 1,
                core::cmp::Ordering::Greater => high = mid,
                core::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    fn louds_bit(&self, index: usize) -> Result<bool, Error> {
        if index >= self.louds_bits {
            return Err(Error::BadTree);
        }
        bit_at(self.louds, index).ok_or(Error::BadTree)
    }
}

impl<'a> DictionaryDetail<'a> {
    /// Writes the complete source description without UI-specific shortening.
    pub fn write_description(&self, sink: &mut impl TextSink) -> Result<(), Error> {
        self.write_text(0, sink)
    }

    /// Copies a bounded UTF-8 preview into a fixed caller buffer and reports
    /// whether source text remains.  This never treats a long source
    /// definition as malformed, allocates no memory, and never splits a UTF-8
    /// scalar.  The caller owns its visible-line policy.
    pub fn write_description_preview<const N: usize>(
        &self,
        sink: &mut FixedStr<N>,
        maximum_bytes: usize,
    ) -> Result<bool, Error> {
        let text = self.description()?;
        let remaining = sink.capacity().saturating_sub(sink.len());
        let mut end = text.len().min(maximum_bytes).min(remaining);
        while end != 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        sink.push_str(&text[..end])
            .map_err(|_| Error::TextOverflow)?;
        Ok(end != text.len())
    }

    /// Writes the complete description for callers that choose a display
    /// channel.  Line limits are applied by the renderer, not the dictionary.
    pub fn write_display_description(&self, sink: &mut impl TextSink) -> Result<(), Error> {
        self.write_text(4, sink)
    }

    /// Visits explicit relationships in source order.  The target text is
    /// borrowed from the mapped image and remains valid for the dictionary's
    /// lifetime.
    pub fn visit_relations(
        &self,
        mut visit: impl FnMut(DetailRelationKind, &str) -> bool,
    ) -> Result<(), Error> {
        let at = self.record_at()?;
        let start = to_usize(
            read_u32(self.details.records, at + 8)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?,
        )?;
        let count = to_usize(
            read_u32(self.details.records, at + 12)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?,
        )?;
        for index in start
            ..start
                .checked_add(count)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?
        {
            let relation_at = index
                .checked_mul(image_format::DETAIL_RELATION_LEN)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RELATIONS))?;
            let kind = DetailRelationKind::from_byte(
                *self
                    .details
                    .relations
                    .get(relation_at)
                    .ok_or(Error::BadTable(image_format::TAG_DETAIL_RELATIONS))?,
            )
            .ok_or(Error::BadTable(image_format::TAG_DETAIL_RELATIONS))?;
            let text_id = to_usize(
                read_u32(self.details.relations, relation_at + 4)
                    .ok_or(Error::BadTable(image_format::TAG_DETAIL_RELATIONS))?,
            )?;
            let text = self.text(text_id)?;
            if !visit(kind, text) {
                break;
            }
        }
        Ok(())
    }

    fn write_text(&self, field_offset: usize, sink: &mut impl TextSink) -> Result<(), Error> {
        let at = self.record_at()?;
        let text_id = to_usize(
            read_u32(self.details.records, at + field_offset)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?,
        )?;
        sink.push_str(self.text(text_id)?)
            .map_err(|_| Error::TextOverflow)
    }

    fn description(&self) -> Result<&'a str, Error> {
        let at = self.record_at()?;
        let text_id = to_usize(
            read_u32(self.details.records, at)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?,
        )?;
        self.text(text_id)
    }

    fn record_at(&self) -> Result<usize, Error> {
        self.record_index
            .checked_mul(image_format::DETAIL_RECORD_LEN)
            .filter(|at| *at + image_format::DETAIL_RECORD_LEN <= self.details.records.len())
            .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))
    }

    fn text(&self, index: usize) -> Result<&'a str, Error> {
        if index >= self.details.text_count {
            return Err(Error::BadTable(image_format::TAG_DETAIL_TEXT));
        }
        let start = read_offset(self.details.text_offsets, index)
            .ok_or(Error::BadTable(image_format::TAG_DETAIL_TEXT))?;
        let end = if index + 1 < self.details.text_count {
            read_offset(self.details.text_offsets, index + 1)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_TEXT))?
        } else {
            self.details.text.len()
        };
        let bytes = self
            .details
            .text
            .get(start..end)
            .ok_or(Error::BadTable(image_format::TAG_DETAIL_TEXT))?;
        core::str::from_utf8(bytes).map_err(|_| Error::BadUtf8)
    }
}

#[derive(Clone, Copy)]
struct Node {
    first_child: usize,
    child_count: usize,
    value_start: usize,
    value_count: usize,
}

/// Validates the optional bunsetsu-boundary table and returns its row bytes.
///
/// The invariants enforced here let conversion index the bitmap without any
/// further bounds thinking: exact length, zeroed row-tail padding bits, and an
/// unconditional boundary on both sides of BOS/EOS (class 0), matching the
/// Mozc segmenter this table is generated from.
fn validate_boundary_table<'a>(table: Table<'a>, class_count: usize) -> Result<&'a [u8], Error> {
    use image_format as format;

    let bad = || Error::BadTable(format::TAG_BOUNDARIES);
    let row_bytes = class_count.checked_add(7).ok_or_else(bad)? / 8;
    let rows_len = class_count.checked_mul(row_bytes).ok_or_else(bad)?;
    let expected_len = format::BOUNDARY_HEADER_LEN
        .checked_add(rows_len)
        .ok_or_else(bad)?;
    if table.count != class_count
        || table.bytes.len() != expected_len
        || table.bytes.get(..4) != Some(format::BOUNDARY_MAGIC.as_slice())
        || usize::from(read_u16(table.bytes, 4).ok_or_else(bad)?) != class_count
        || read_u16(table.bytes, 6).ok_or_else(bad)? != 0
    {
        return Err(bad());
    }
    let rows = &table.bytes[format::BOUNDARY_HEADER_LEN..];
    let boundary_bit = |rid: usize, lid: usize| {
        rows.get(rid * row_bytes + lid / 8)
            .is_some_and(|byte| byte & (1u8 << (lid % 8)) != 0)
    };
    for rid in 0..class_count {
        if !boundary_bit(rid, 0) || !boundary_bit(0, rid) {
            return Err(bad());
        }
        for padding_bit in class_count..row_bytes * 8 {
            if boundary_bit(rid, padding_bit) {
                return Err(bad());
            }
        }
    }
    Ok(rows)
}

/// Validates the optional single-kanji tables.
///
/// Everything the lookups depend on is established once here so they can index
/// without re-deriving bounds: index records ascend strictly by reading bytes,
/// every reading slice is in range and valid UTF-8, every character span is in
/// range, every stored scalar is a real `char`, and every variant record
/// ascends strictly by variant scalar with a decodable kind.  A table that
/// fails any of these is rejected rather than degraded, because a half-trusted
/// index would surface silently wrong characters.
fn validate_single_kanji_table(table: &SingleKanji<'_>) -> Result<(), Error> {
    use image_format as format;

    let bad_index = || Error::BadTable(format::TAG_SINGLE_KANJI_INDEX);
    let bad_readings = || Error::BadTable(format::TAG_SINGLE_KANJI_READINGS);
    let bad_chars = || Error::BadTable(format::TAG_SINGLE_KANJI_CHARS);
    let bad_variants = || Error::BadTable(format::TAG_SINGLE_KANJI_VARIANTS);

    if table.index_count == 0 || table.char_count == 0 {
        return Err(bad_index());
    }
    let mut previous: Option<&[u8]> = None;
    for record in 0..table.index_count {
        let at = record * format::SINGLE_KANJI_INDEX_LEN;
        let reading_offset = to_usize(read_u32(table.index, at).ok_or_else(bad_index)?)?;
        let char_start = to_usize(read_u32(table.index, at + 4).ok_or_else(bad_index)?)?;
        let reading_len = usize::from(read_u16(table.index, at + 8).ok_or_else(bad_index)?);
        let chars = usize::from(read_u16(table.index, at + 10).ok_or_else(bad_index)?);
        if reading_len == 0 || chars == 0 {
            return Err(bad_index());
        }
        let reading_end = reading_offset
            .checked_add(reading_len)
            .ok_or_else(bad_index)?;
        let reading = table
            .readings
            .get(reading_offset..reading_end)
            .ok_or_else(bad_readings)?;
        if core::str::from_utf8(reading).is_err() {
            return Err(bad_readings());
        }
        if previous.is_some_and(|earlier| earlier >= reading) {
            return Err(bad_index());
        }
        previous = Some(reading);
        let char_end = char_start.checked_add(chars).ok_or_else(bad_index)?;
        if char_end > table.char_count {
            return Err(bad_index());
        }
    }
    for scalar in 0..table.char_count {
        let at = scalar * format::SINGLE_KANJI_CHAR_LEN;
        let value = read_u32(table.chars, at).ok_or_else(bad_chars)?;
        if char::from_u32(value).is_none() {
            return Err(bad_chars());
        }
    }
    let mut previous_variant = None;
    for record in 0..table.variant_count {
        let at = record * format::SINGLE_KANJI_VARIANT_LEN;
        let variant = read_u32(table.variants, at).ok_or_else(bad_variants)?;
        let original = read_u32(table.variants, at + 4).ok_or_else(bad_variants)?;
        let kind = *table.variants.get(at + 8).ok_or_else(bad_variants)?;
        let padding = table
            .variants
            .get(at + 9..at + 12)
            .ok_or_else(bad_variants)?;
        if char::from_u32(variant).is_none()
            || char::from_u32(original).is_none()
            || SingleKanjiVariantKind::from_code(kind).is_none()
            || padding != [0, 0, 0]
            || previous_variant.is_some_and(|earlier| earlier >= variant)
        {
            return Err(bad_variants());
        }
        previous_variant = Some(variant);
    }
    Ok(())
}

fn validate_matrix_table(table: Table<'_>, class_count: usize) -> Result<(), Error> {
    use image_format as format;

    let bad = || Error::BadTable(format::TAG_MATRIX);
    if table.count != class_count
        || table.bytes.len() < format::MATRIX_HEADER_LEN
        || table.bytes.get(..4) != Some(format::MATRIX_MAGIC.as_slice())
        || usize::from(read_u16(table.bytes, 4).ok_or_else(bad)?) != class_count
        || read_u16(table.bytes, 6).ok_or_else(bad)? != 0
        || read_u32(table.bytes, 12).ok_or_else(bad)? != 0
    {
        return Err(bad());
    }

    let override_count =
        usize::try_from(read_u32(table.bytes, 8).ok_or_else(bad)?).map_err(|_| bad())?;
    let modes_end = format::MATRIX_HEADER_LEN
        .checked_add(class_count.checked_mul(2).ok_or_else(bad)?)
        .ok_or_else(bad)?;
    let rows_at = align_up_4(modes_end).ok_or_else(bad)?;
    if table
        .bytes
        .get(modes_end..rows_at)
        .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
    {
        return Err(bad());
    }
    let rows_end = rows_at
        .checked_add(
            class_count
                .checked_mul(format::MATRIX_ROW_LEN)
                .ok_or_else(bad)?,
        )
        .ok_or_else(bad)?;
    let expected_len = rows_end
        .checked_add(
            override_count
                .checked_mul(format::MATRIX_OVERRIDE_LEN)
                .ok_or_else(bad)?,
        )
        .ok_or_else(bad)?;
    if expected_len != table.bytes.len() {
        return Err(bad());
    }

    let mut next_start = 0usize;
    for row in 0..class_count {
        let mode = read_u16(table.bytes, format::MATRIX_HEADER_LEN + row * 2).ok_or_else(bad)?;
        let descriptor = rows_at + row * format::MATRIX_ROW_LEN;
        let start = usize::try_from(read_u32(table.bytes, descriptor).ok_or_else(bad)?)
            .map_err(|_| bad())?;
        let count = usize::try_from(read_u32(table.bytes, descriptor + 4).ok_or_else(bad)?)
            .map_err(|_| bad())?;
        let end = start.checked_add(count).ok_or_else(bad)?;
        if start != next_start || end > override_count {
            return Err(bad());
        }

        let mut previous_left = None;
        for index in start..end {
            let at = rows_end + index * format::MATRIX_OVERRIDE_LEN;
            let left = usize::from(read_u16(table.bytes, at).ok_or_else(bad)?);
            let cost = read_u16(table.bytes, at + 2).ok_or_else(bad)?;
            if left >= class_count
                || previous_left.is_some_and(|previous| previous >= left)
                || cost == mode
            {
                return Err(bad());
            }
            previous_left = Some(left);
        }
        next_start = end;
    }
    if next_start != override_count {
        return Err(bad());
    }
    Ok(())
}

fn align_up_4(value: usize) -> Option<usize> {
    value.checked_add(3).map(|aligned| aligned & !3)
}

fn read_offset(bytes: &[u8], index: usize) -> Option<usize> {
    let at = index.checked_mul(4)?;
    usize::try_from(read_u32(bytes, at)?).ok()
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let value: [u8; 2] = bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let value: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(value))
}

fn read_i32(bytes: &[u8], at: usize) -> Option<i32> {
    let value: [u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(i32::from_le_bytes(value))
}

fn bit_at(bytes: &[u8], index: usize) -> Option<bool> {
    let byte = *bytes.get(index / 8)?;
    Some(byte & (1 << (index % 8)) != 0)
}

fn to_usize(value: u32) -> Result<usize, Error> {
    usize::try_from(value).map_err(|_| Error::BadHeader)
}

#[cfg(test)]
#[path = "dictionary_tests.rs"]
mod tests;
