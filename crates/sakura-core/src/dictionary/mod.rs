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

mod detail;
#[path = "format.rs"]
pub mod image_format;
mod lookup;
mod louds;
mod parse;
mod validate;

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
