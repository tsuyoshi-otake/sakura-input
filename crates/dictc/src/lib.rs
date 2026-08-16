//! Deterministic compiler for Sakura's read-only dictionary image.
//!
//! This crate is a build tool and may allocate freely. The runtime half lives
//! in `sakura_core::dictionary`, which borrows the compiled bytes without
//! deserializing them.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use sakura_core::dictionary::{image_format as format, Dictionary, EntryFlags};
use sakura_proto::MAX_PREEDIT_BYTES;

pub mod category;
pub mod context_corpus;
pub mod context_dataset;
pub mod context_rerank_import;
pub mod glossary;
pub mod llm_detail_targets;
pub mod llm_details;
pub mod segmenter;
pub mod wordnet;

/// Connection taxonomy from the pinned Mozc dictionary revision. The source
/// has 2,672 classes (not the approximately 1,300 claimed by the initial
/// design notes), so the image stores exact per-row modes plus sorted
/// exceptions instead of spending 13.6 MiB on a flat matrix.
pub const FROZEN_CLASS_COUNT: u16 = 2_672;
pub const MAX_MATRIX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
/// Release gate for the complete categorized Sakura system image.  The
/// expanded lexical corpus is intentionally mapped read-only, and a 128 MiB
/// cap leaves room for 1.2M conservative system entries without allowing
/// unbounded artifacts.
pub const MAX_DICTIONARY_IMAGE_BYTES: usize = 128 * 1024 * 1024;

pub const MOZC_UPSTREAM_COMMIT: &str = "3f235b4eb6fcff7d14ef5f0fb8ee56de7ee4c732";

const ALLOWED_LICENSES: [&str; 7] = [
    "BSD-3-Clause",
    "Apache-2.0",
    "CC0-1.0",
    "MIT",
    "LicenseRef-Sakura-InHouse",
    "LicenseRef-Mozc-Dictionary",
    "LicenseRef-ATOK36-LGPL",
];

/// One row from a human-editable dictionary TSV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    pub reading: String,
    pub surface: String,
    pub left_id: u16,
    pub right_id: u16,
    pub word_cost: i32,
    pub prediction_cost: i32,
    pub flags: EntryFlags,
    pub annotation: String,
    source: Arc<str>,
    line: usize,
}

/// A source-backed relationship for a detail panel.  Only explicit source data
/// may use `Synonym` or `Antonym`; the compiler never derives either from text
/// similarity or shared spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDetailRelation {
    pub kind: sakura_core::dictionary::DetailRelationKind,
    pub target: String,
}

/// Optional sparse detail attached to one exact dictionary entry.  The source
/// description is retained verbatim; visual line limits belong to the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDetail {
    pub reading: String,
    pub surface: String,
    pub left_id: u16,
    pub right_id: u16,
    pub description: String,
    pub relations: Vec<SourceDetailRelation>,
}

/// The dense `right_id × left_id` connection-cost matrix written to the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionMatrix {
    class_count: u16,
    costs: Vec<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimPolicy {
    pub max_word_cost: i32,
    pub max_candidates_per_reading: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrimReport {
    pub input_entries: usize,
    pub cost_eligible: usize,
    pub duplicate_entries: usize,
    pub capped_entries: usize,
    pub output_entries: usize,
}

/// Bounded-memory accumulator for Mozc shards.
///
/// Thresholding happens as each shard arrives. Finalization performs the only
/// global sort required for deterministic deduplication/ranking, so memory is
/// proportional to the selected lexicon rather than all upstream rows.
#[derive(Debug)]
pub struct MozcTrimmer {
    policy: TrimPolicy,
    eligible: Vec<SourceEntry>,
    report: TrimReport,
}

impl MozcTrimmer {
    pub fn new(policy: TrimPolicy) -> Result<Self, Error> {
        if policy.max_word_cost < 0 {
            return Err(Error::build("trim max_word_cost must be non-negative"));
        }
        if policy.max_candidates_per_reading == 0 {
            return Err(Error::build(
                "trim max_candidates_per_reading must be greater than zero",
            ));
        }
        Ok(Self {
            policy,
            eligible: Vec::new(),
            report: TrimReport::default(),
        })
    }

    pub fn push_shard(&mut self, entries: Vec<SourceEntry>) {
        self.report.input_entries = self.report.input_entries.saturating_add(entries.len());
        for entry in entries {
            if entry.word_cost <= self.policy.max_word_cost {
                self.report.cost_eligible = self.report.cost_eligible.saturating_add(1);
                self.eligible.push(entry);
            }
        }
    }

    pub fn finish(mut self) -> (Vec<SourceEntry>, TrimReport) {
        self.eligible.sort_by(|a, b| {
            (
                &a.reading,
                &a.surface,
                a.left_id,
                a.right_id,
                a.word_cost,
                a.prediction_cost,
                a.flags.bits(),
            )
                .cmp(&(
                    &b.reading,
                    &b.surface,
                    b.left_id,
                    b.right_id,
                    b.word_cost,
                    b.prediction_cost,
                    b.flags.bits(),
                ))
        });
        let mut deduplicated: Vec<SourceEntry> = Vec::with_capacity(self.eligible.len());
        for entry in self.eligible {
            let duplicate = deduplicated.last().is_some_and(|previous| {
                previous.reading == entry.reading
                    && previous.surface == entry.surface
                    && previous.left_id == entry.left_id
                    && previous.right_id == entry.right_id
            });
            if duplicate {
                self.report.duplicate_entries = self.report.duplicate_entries.saturating_add(1);
            } else {
                deduplicated.push(entry);
            }
        }
        deduplicated.sort_by(|a, b| {
            (
                &a.reading,
                a.word_cost,
                &a.surface,
                a.left_id,
                a.right_id,
                a.flags.bits(),
            )
                .cmp(&(
                    &b.reading,
                    b.word_cost,
                    &b.surface,
                    b.left_id,
                    b.right_id,
                    b.flags.bits(),
                ))
        });

        let mut selected: Vec<SourceEntry> = Vec::with_capacity(deduplicated.len());
        let mut candidates_for_reading = 0usize;
        for entry in deduplicated {
            if selected
                .last()
                .is_none_or(|previous| previous.reading != entry.reading)
            {
                candidates_for_reading = 0;
            }
            if candidates_for_reading < self.policy.max_candidates_per_reading {
                candidates_for_reading += 1;
                selected.push(entry);
            } else {
                self.report.capped_entries = self.report.capped_entries.saturating_add(1);
            }
        }
        self.report.output_entries = selected.len();
        (selected, self.report)
    }
}

impl ConnectionMatrix {
    pub fn class_count(&self) -> u16 {
        self.class_count
    }

    pub fn cost(&self, previous_right_id: u16, next_left_id: u16) -> Option<u16> {
        let classes = usize::from(self.class_count);
        let row = usize::from(previous_right_id);
        let column = usize::from(next_left_id);
        if row >= classes || column >= classes {
            return None;
        }
        self.costs.get(row * classes + column).copied()
    }
}

/// A source or image-build error with enough location data to fix the TSV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    source: String,
    line: Option<usize>,
    message: String,
}

impl Error {
    fn at(source: &str, line: usize, message: impl Into<String>) -> Self {
        Self {
            source: source.to_string(),
            line: Some(line),
            message: message.into(),
        }
    }

    fn build(message: impl Into<String>) -> Self {
        Self {
            source: "dictionary image".to_string(),
            line: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(line) = self.line {
            write!(f, "{}:{line}: {}", self.source, self.message)
        } else {
            write!(f, "{}: {}", self.source, self.message)
        }
    }
}

impl std::error::Error for Error {}

const TSV_HEADER: &str =
    "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation";

/// Parses a licensed dictionary TSV source.
pub fn parse_entries(source: &str, text: &str) -> Result<Vec<SourceEntry>, Error> {
    parse_sakura_entries(source, text, true)
}

/// Parses a final category dictionary TSV.
///
/// Category files intentionally start directly with the schema header: their
/// file name already communicates the contents, and they do not carry source
/// or license metadata.  The source files fed into the category builder remain
/// validated with [`parse_entries`] before this representation is emitted.
pub fn parse_category_entries(source: &str, text: &str) -> Result<Vec<SourceEntry>, Error> {
    parse_sakura_entries(source, text, false)
}

fn parse_sakura_entries(
    source: &str,
    text: &str,
    require_license: bool,
) -> Result<Vec<SourceEntry>, Error> {
    let source_name: Arc<str> = Arc::from(source);
    let mut license = None;
    let mut saw_header = false;
    let mut entries = Vec::new();

    for (zero_based, raw) in text.lines().enumerate() {
        let line_number = zero_based + 1;
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            if !require_license {
                return Err(Error::at(
                    source,
                    line_number,
                    "category dictionary files must not contain comments or metadata",
                ));
            }
            if let Some(value) = comment.trim().strip_prefix("license:") {
                if license.replace(value.trim().to_string()).is_some() {
                    return Err(Error::at(
                        source,
                        line_number,
                        "duplicate license declaration",
                    ));
                }
            }
            continue;
        }
        if require_license && license.is_none() {
            return Err(Error::at(
                source,
                line_number,
                "a '# license: SPDX-ID' declaration must precede data",
            ));
        }
        if !saw_header {
            if line != TSV_HEADER {
                return Err(Error::at(
                    source,
                    line_number,
                    format!("unexpected header; expected '{TSV_HEADER}'"),
                ));
            }
            saw_header = true;
            continue;
        }

        // Split the row completely rather than capping the split at eight: a
        // capped split folds every extra column into the annotation instead of
        // failing the count check below, and the annotation is shown to the
        // user as a candidate note.
        let columns: Vec<&str> = line.split('\t').collect();
        if columns.len() != 8 {
            return Err(Error::at(
                source,
                line_number,
                format!("expected 8 tab-separated columns, found {}", columns.len()),
            ));
        }
        let reading = columns[0];
        let surface = columns[1];
        validate_reading(source, line_number, reading)?;
        validate_text(source, line_number, "surface", surface)?;
        validate_text(source, line_number, "annotation", columns[7])?;
        // Licensed sources are edited by hand. A `[calibration]` or `[company]`
        // tag in this column is a developer note, but the column is the
        // candidate note the user sees. Generated category files may still
        // carry a baked tag until the next split; dictc strips those after
        // extracting reviewed details.
        if require_license && columns[7].starts_with('[') {
            return Err(Error::at(
                source,
                line_number,
                "annotation must not start with '['",
            ));
        }
        let left_id = parse_number::<u16>(source, line_number, "left_id", columns[2])?;
        let right_id = parse_number::<u16>(source, line_number, "right_id", columns[3])?;
        let word_cost = parse_number::<i32>(source, line_number, "word_cost", columns[4])?;
        if word_cost < 0 {
            return Err(Error::at(
                source,
                line_number,
                "word_cost must be non-negative",
            ));
        }
        let prediction_cost = if columns[5] == "-" {
            i32::MAX
        } else {
            let cost = parse_number::<i32>(source, line_number, "prediction_cost", columns[5])?;
            if cost < 0 {
                return Err(Error::at(
                    source,
                    line_number,
                    "prediction_cost must be non-negative or '-'",
                ));
            }
            cost
        };
        let flags = parse_flags(source, line_number, columns[6])?;
        entries.push(SourceEntry {
            reading: reading.to_string(),
            surface: surface.to_string(),
            left_id,
            right_id,
            word_cost,
            prediction_cost,
            flags,
            annotation: columns[7].to_string(),
            source: Arc::clone(&source_name),
            line: line_number,
        });
    }

    if require_license {
        validate_license(source, license.as_deref())?;
    }
    if !saw_header {
        return Err(Error::at(source, 1, "missing dictionary TSV header"));
    }
    Ok(entries)
}

/// Parses one of Mozc's pinned five-column OSS dictionary shards.
///
/// Mozc rows are `reading, left_id, right_id, cost, surface`. Licensing is
/// enforced by the upstream manifest/fetch pipeline because the source files
/// carry the IPAdic/Okinawa notices beside, rather than inside, every shard.
pub fn parse_mozc_entries(source: &str, text: &str) -> Result<Vec<SourceEntry>, Error> {
    let source_name: Arc<str> = Arc::from(source);
    let mut entries = Vec::new();
    for (zero_based, raw) in text.lines().enumerate() {
        let line_number = zero_based + 1;
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<&str> = line.split('\t').collect();
        if !(5..=6).contains(&columns.len()) {
            return Err(Error::at(
                source,
                line_number,
                format!(
                    "expected 5 or 6 tab-separated Mozc columns, found {}",
                    columns.len()
                ),
            ));
        }
        validate_mozc_reading(source, line_number, columns[0])?;
        validate_text(source, line_number, "surface", columns[4])?;
        let left_id = parse_number::<u16>(source, line_number, "left_id", columns[1])?;
        let right_id = parse_number::<u16>(source, line_number, "right_id", columns[2])?;
        let word_cost = parse_number::<i32>(source, line_number, "word_cost", columns[3])?;
        if word_cost < 0 {
            return Err(Error::at(
                source,
                line_number,
                "word_cost must be non-negative",
            ));
        }
        let special_flags = match columns.get(5).copied().unwrap_or("") {
            "" => EntryFlags::NONE,
            "SPELLING_CORRECTION" => EntryFlags::SPELLING_CORRECTION,
            label => {
                return Err(Error::at(
                    source,
                    line_number,
                    format!("unsupported Mozc special label '{label}'"),
                ));
            }
        };
        let prediction_worthy = !special_flags.contains(EntryFlags::SPELLING_CORRECTION)
            && word_cost <= 6_000
            && columns[0].chars().count() >= 2;
        entries.push(SourceEntry {
            reading: columns[0].to_string(),
            surface: columns[4].to_string(),
            left_id,
            right_id,
            word_cost,
            prediction_cost: if prediction_worthy {
                word_cost.saturating_add(1_200)
            } else {
                i32::MAX
            },
            flags: special_flags
                | if prediction_worthy {
                    EntryFlags::PREDICTION
                } else {
                    EntryFlags::NONE
                },
            annotation: String::new(),
            source: Arc::clone(&source_name),
            line: line_number,
        });
    }
    Ok(entries)
}

/// Parses Mozc's row-major `connection_single_column.txt` without changing a
/// single cost. Shipping builds require the taxonomy from
/// [`MOZC_UPSTREAM_COMMIT`]; small fixtures may opt out.
pub fn parse_mozc_connection(
    source: &str,
    text: &str,
    require_frozen_taxonomy: bool,
) -> Result<ConnectionMatrix, Error> {
    let mut lines = text.lines().enumerate().filter_map(|(zero_based, raw)| {
        let line = raw.trim_end_matches('\r').trim();
        (!line.is_empty() && !line.starts_with('#')).then_some((zero_based + 1, line))
    });
    let (header_line, header) = lines
        .next()
        .ok_or_else(|| Error::at(source, 1, "missing Mozc matrix size"))?;
    let class_count = parse_number::<u16>(source, header_line, "classes", header)?;
    if class_count == 0 {
        return Err(Error::at(
            source,
            header_line,
            "classes must be greater than zero",
        ));
    }
    if require_frozen_taxonomy && class_count != FROZEN_CLASS_COUNT {
        return Err(Error::at(
            source,
            header_line,
            format!(
                "shipping taxonomy is frozen at {FROZEN_CLASS_COUNT} classes, found {class_count}"
            ),
        ));
    }
    let classes = usize::from(class_count);
    let cells = classes
        .checked_mul(classes)
        .ok_or_else(|| Error::at(source, header_line, "connection matrix size overflow"))?;
    let mut costs = Vec::with_capacity(cells);
    for index in 0..cells {
        let (line, value) = lines.next().ok_or_else(|| {
            Error::at(
                source,
                header_line,
                format!("Mozc matrix is truncated at cell {index} of {cells}"),
            )
        })?;
        costs.push(parse_number::<u16>(source, line, "cost", value)?);
    }
    if let Some((line, _)) = lines.next() {
        return Err(Error::at(
            source,
            line,
            format!("Mozc matrix has more than {cells} cells"),
        ));
    }
    Ok(ConnectionMatrix { class_count, costs })
}

/// Parses the connection source. Rows are `cost<tab>previous_right<tab>next_left<tab>value`.
pub fn parse_connection(
    source: &str,
    text: &str,
    require_frozen_taxonomy: bool,
) -> Result<ConnectionMatrix, Error> {
    let mut license = None;
    let mut class_count = None;
    let mut default_cost = None;
    let mut overrides = Vec::new();

    for (zero_based, raw) in text.lines().enumerate() {
        let line_number = zero_based + 1;
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            if let Some(value) = comment.trim().strip_prefix("license:") {
                if license.replace(value.trim().to_string()).is_some() {
                    return Err(Error::at(
                        source,
                        line_number,
                        "duplicate license declaration",
                    ));
                }
            }
            continue;
        }
        if license.is_none() {
            return Err(Error::at(
                source,
                line_number,
                "a '# license: SPDX-ID' declaration must precede data",
            ));
        }
        let columns: Vec<&str> = line.split('\t').collect();
        match columns.as_slice() {
            ["classes", value] => {
                if class_count.is_some() {
                    return Err(Error::at(
                        source,
                        line_number,
                        "duplicate classes directive",
                    ));
                }
                class_count = Some(parse_number::<u16>(source, line_number, "classes", value)?);
            }
            ["default", value] => {
                if default_cost.is_some() {
                    return Err(Error::at(
                        source,
                        line_number,
                        "duplicate default directive",
                    ));
                }
                default_cost = Some(parse_number::<u16>(source, line_number, "default", value)?);
            }
            ["cost", right, left, value] => {
                overrides.push((
                    line_number,
                    parse_number::<u16>(source, line_number, "right_id", right)?,
                    parse_number::<u16>(source, line_number, "left_id", left)?,
                    parse_number::<u16>(source, line_number, "cost", value)?,
                ));
            }
            _ => {
                return Err(Error::at(
                    source,
                    line_number,
                    "expected classes, default, or four-column cost row",
                ));
            }
        }
    }

    validate_license(source, license.as_deref())?;
    let class_count =
        class_count.ok_or_else(|| Error::at(source, 1, "missing classes directive"))?;
    if class_count == 0 {
        return Err(Error::at(source, 1, "classes must be greater than zero"));
    }
    if require_frozen_taxonomy && class_count != FROZEN_CLASS_COUNT {
        return Err(Error::at(
            source,
            1,
            format!(
                "shipping taxonomy is frozen at {FROZEN_CLASS_COUNT} classes, found {class_count}"
            ),
        ));
    }
    let default_cost =
        default_cost.ok_or_else(|| Error::at(source, 1, "missing default directive"))?;
    let classes = usize::from(class_count);
    let cells = classes
        .checked_mul(classes)
        .ok_or_else(|| Error::build("connection matrix size overflow"))?;
    let mut costs = vec![default_cost; cells];
    let mut seen = vec![false; cells];
    for (line, right, left, cost) in overrides {
        if right >= class_count || left >= class_count {
            return Err(Error::at(
                source,
                line,
                format!("connection id is outside 0..{class_count}"),
            ));
        }
        let index = usize::from(right) * classes + usize::from(left);
        if seen[index] {
            return Err(Error::at(source, line, "duplicate connection-cost cell"));
        }
        seen[index] = true;
        costs[index] = cost;
    }
    Ok(ConnectionMatrix { class_count, costs })
}

/// Compiles entries and a dense connection matrix to one byte-deterministic image.
/// This compatibility entry point emits no optional details.
pub fn compile(entries: &[SourceEntry], connection: &ConnectionMatrix) -> Result<Vec<u8>, Error> {
    compile_with_details(entries, connection, &[])
}

/// Compiles entries plus source-backed sparse detail records.  Detail lookup is
/// final-entry-ordinal keyed so the fixed 24-byte entry ABI stays unchanged
/// without attaching a description to a same-surface homograph.
pub fn compile_with_details(
    entries: &[SourceEntry],
    connection: &ConnectionMatrix,
    details: &[SourceDetail],
) -> Result<Vec<u8>, Error> {
    compile_with_tables(entries, connection, details, None)
}

/// Compiles entries, details, and the optional frozen bunsetsu-boundary
/// table.  The boundary table lets conversion fuse morphemes into bunsetsu
/// segments; omitting it keeps the historical morpheme-granularity image.
pub fn compile_with_tables(
    entries: &[SourceEntry],
    connection: &ConnectionMatrix,
    details: &[SourceDetail],
    boundaries: Option<&segmenter::BunsetsuBoundaries>,
) -> Result<Vec<u8>, Error> {
    let class_count = connection.class_count;
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| {
        (
            &a.reading,
            a.word_cost,
            &a.surface,
            a.left_id,
            a.right_id,
            a.flags.bits(),
            &a.annotation,
        )
            .cmp(&(
                &b.reading,
                b.word_cost,
                &b.surface,
                b.left_id,
                b.right_id,
                b.flags.bits(),
                &b.annotation,
            ))
    });
    for entry in &sorted {
        if entry.left_id >= class_count || entry.right_id >= class_count {
            return Err(Error::at(
                &entry.source,
                entry.line,
                format!("connection id is outside 0..{class_count}"),
            ));
        }
        if entry.annotation.starts_with('[') {
            return Err(Error::at(
                &entry.source,
                entry.line,
                "candidate annotation must not start with '['",
            ));
        }
    }
    for pair in sorted.windows(2) {
        let [before, after] = pair else {
            continue;
        };
        if before.reading == after.reading
            && before.surface == after.surface
            && before.left_id == after.left_id
            && before.right_id == after.right_id
        {
            return Err(Error::at(
                &after.source,
                after.line,
                format!(
                    "duplicate entry for reading '{}' and surface '{}'",
                    after.reading, after.surface
                ),
            ));
        }
    }

    let surfaces = sorted
        .iter()
        .map(|entry| entry.surface.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let surface_ids = surfaces
        .iter()
        .enumerate()
        .map(|(index, value)| (value.as_str(), index as u32))
        .collect::<BTreeMap<_, _>>();
    let annotations = sorted
        .iter()
        .filter(|entry| !entry.annotation.is_empty())
        .map(|entry| entry.annotation.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let annotation_ids = annotations
        .iter()
        .enumerate()
        .map(|(index, value)| (value.as_str(), index as u32))
        .collect::<BTreeMap<_, _>>();

    let trie = build_trie(&sorted)?;
    let built = flatten_trie(&trie, &sorted, &surface_ids, &annotation_ids)?;
    let (surface_offsets, surface_data) = front_code(&surfaces)?;
    let (annotation_offsets, annotation_data) = raw_text_table(&annotations)?;
    let matrix = encode_matrix(connection)?;
    let entry_ordinals = sorted
        .iter()
        .enumerate()
        .map(|(source_index, entry)| {
            let ordinal = *built
                .source_to_image_entry
                .get(source_index)
                .ok_or_else(|| Error::build("compiled entry ordinal disappeared"))?;
            if ordinal == usize::MAX {
                return Err(Error::build("compiled entry ordinal was not emitted"));
            }
            Ok((
                (
                    entry.reading.as_str(),
                    entry.surface.as_str(),
                    entry.left_id,
                    entry.right_id,
                ),
                ordinal,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, Error>>()?;

    let mut tables = vec![
        TableData::new(format::TAG_LOUDS, built.louds, built.louds_bits),
        TableData::new(format::TAG_NODES, built.nodes, built.node_count),
        TableData::new(format::TAG_LABELS, built.labels, built.node_count),
        TableData::new(format::TAG_ENTRIES, built.entries, sorted.len()),
        TableData::new(format::TAG_SURFACE_OFFSETS, surface_offsets, surfaces.len()),
        TableData::new(format::TAG_SURFACES, surface_data, surfaces.len()),
        TableData::new(
            format::TAG_ANNOTATION_OFFSETS,
            annotation_offsets,
            annotations.len(),
        ),
        TableData::new(format::TAG_ANNOTATIONS, annotation_data, annotations.len()),
        TableData::new(format::TAG_MATRIX, matrix, usize::from(class_count)),
    ];
    if !details.is_empty() {
        tables.extend(encode_details(details, &entry_ordinals)?);
    }
    if let Some(boundaries) = boundaries {
        if boundaries.class_count() != class_count {
            return Err(Error::build(format!(
                "bunsetsu boundary table has {} classes, connection matrix has {class_count}",
                boundaries.class_count()
            )));
        }
        tables.push(TableData::new(
            format::TAG_BOUNDARIES,
            encode_boundaries(boundaries),
            usize::from(class_count),
        ));
    }
    assemble_image(class_count, sorted.len(), built.node_count, tables)
}

/// Encodes the frozen boundary matrix as the optional `BNDR` image table.
fn encode_boundaries(boundaries: &segmenter::BunsetsuBoundaries) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(format::BOUNDARY_HEADER_LEN + boundaries.rows().len());
    bytes.extend_from_slice(&format::BOUNDARY_MAGIC);
    bytes.extend_from_slice(&boundaries.class_count().to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(boundaries.rows());
    bytes
}

/// Writes the licensed Sakura TSV consumed by [`parse_entries`].
pub fn entries_to_tsv(entries: &[SourceEntry], license: &str) -> Result<String, Error> {
    use std::fmt::Write as _;

    validate_license("generated TSV", Some(license))?;
    let mut output = String::new();
    writeln!(&mut output, "# license: {license}")
        .map_err(|_| Error::build("failed to write TSV license"))?;
    write_tsv_body(&mut output, entries)?;
    Ok(output)
}

/// Writes a final category TSV with no metadata lines.
///
/// The category builder only calls this after parsing and validating its
/// licensed inputs.  The compiler reads these category files through
/// [`parse_category_entries`].
pub fn entries_to_category_tsv(entries: &[SourceEntry]) -> Result<String, Error> {
    let mut output = String::new();
    write_tsv_body(&mut output, entries)?;
    Ok(output)
}

fn write_tsv_body(output: &mut String, entries: &[SourceEntry]) -> Result<(), Error> {
    use std::fmt::Write as _;

    let mut sorted = entries.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| {
        (
            &a.reading,
            &a.surface,
            a.left_id,
            a.right_id,
            a.word_cost,
            a.prediction_cost,
        )
            .cmp(&(
                &b.reading,
                &b.surface,
                b.left_id,
                b.right_id,
                b.word_cost,
                b.prediction_cost,
            ))
    });
    output.push_str(TSV_HEADER);
    output.push('\n');
    for entry in sorted {
        if entry.reading.contains(['\t', '\r', '\n'])
            || entry.surface.contains(['\t', '\r', '\n'])
            || entry.annotation.contains(['\t', '\r', '\n'])
        {
            return Err(Error::at(
                &entry.source,
                entry.line,
                "TSV fields must not contain tabs or newlines",
            ));
        }
        let known_flags = EntryFlags::IT.bits()
            | EntryFlags::PREDICTION.bits()
            | EntryFlags::SPELLING_CORRECTION.bits();
        if entry.flags.bits() & !known_flags != 0 {
            return Err(Error::at(
                &entry.source,
                entry.line,
                "entry has flags the TSV schema cannot represent",
            ));
        }
        let mut flags = Vec::with_capacity(3);
        if entry.flags.contains(EntryFlags::IT) {
            flags.push("it");
        }
        if entry.flags.contains(EntryFlags::PREDICTION) {
            flags.push("predict");
        }
        if entry.flags.contains(EntryFlags::SPELLING_CORRECTION) {
            flags.push("correction");
        }
        let flags = flags.join(",");
        let prediction = if entry.prediction_cost == i32::MAX {
            "-".to_string()
        } else {
            entry.prediction_cost.to_string()
        };
        writeln!(
            output,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            entry.reading,
            entry.surface,
            entry.left_id,
            entry.right_id,
            entry.word_cost,
            prediction,
            flags,
            entry.annotation
        )
        .map_err(|_| Error::build("failed to write TSV row"))?;
    }
    Ok(())
}

/// Merges a base lexicon with a higher-priority overlay without duplicating an
/// identical `(reading, surface, left_id, right_id)` lattice edge.
///
/// Duplicates inside either layer are source errors. When the two layers share
/// an edge, the overlay replaces the system entry so its domain flags, boosted
/// cost, and prediction cost remain observable at runtime. Overlay annotations
/// still replace at merge so a reviewed detail source can match them; leftover
/// candidate-list notes are cleared before the image is compiled.
pub fn merge_entries(
    mut system: Vec<SourceEntry>,
    mut overlay: Vec<SourceEntry>,
) -> Result<Vec<SourceEntry>, Error> {
    sort_and_validate_layer(&mut system, "system")?;
    sort_and_validate_layer(&mut overlay, "overlay")?;
    let mut system = system.into_iter().peekable();
    let mut overlay = overlay.into_iter().peekable();
    let mut merged = Vec::with_capacity(system.len().saturating_add(overlay.len()));
    loop {
        match (system.peek(), overlay.peek()) {
            (Some(base), Some(extra)) => match entry_identity_cmp(base, extra) {
                Ordering::Less => merged.push(system.next().expect("peeked system entry")),
                Ordering::Greater => merged.push(overlay.next().expect("peeked overlay entry")),
                Ordering::Equal => {
                    let _ = system.next();
                    merged.push(overlay.next().expect("peeked overlay entry"));
                }
            },
            (Some(_), None) => {
                merged.extend(system);
                break;
            }
            (None, Some(_)) => {
                merged.extend(overlay);
                break;
            }
            (None, None) => break,
        }
    }
    Ok(merged)
}

/// Converts reviewed entry annotations into sparse detail records while
/// removing those annotations from the compiled candidate-list payload.
///
/// Every reviewed source row must still match the final merged entry exactly,
/// including costs and flags. This makes category placement and overlay
/// precedence part of the release gate instead of silently attaching a
/// description to a different lattice edge.
pub fn extract_entry_details(
    entries: &mut [SourceEntry],
    reviewed: &[SourceEntry],
) -> Result<Vec<SourceDetail>, Error> {
    let mut entry_by_identity = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let identity = (
            entry.reading.as_str(),
            entry.surface.as_str(),
            entry.left_id,
            entry.right_id,
        );
        if entry_by_identity.insert(identity, index).is_some() {
            return Err(Error::build(format!(
                "duplicate compiled entry for reading '{}' and surface '{}'",
                entry.reading, entry.surface
            )));
        }
    }

    let mut reviewed_identities = BTreeSet::new();
    let mut clear_annotations = Vec::with_capacity(reviewed.len());
    let mut details = Vec::with_capacity(reviewed.len());
    for source in reviewed {
        let identity = (
            source.reading.as_str(),
            source.surface.as_str(),
            source.left_id,
            source.right_id,
        );
        if !reviewed_identities.insert(identity) {
            return Err(Error::at(
                &source.source,
                source.line,
                format!(
                    "duplicate reviewed detail for reading '{}' and surface '{}'",
                    source.reading, source.surface
                ),
            ));
        }
        if source.annotation.trim().is_empty() {
            return Err(Error::at(
                &source.source,
                source.line,
                "reviewed detail description must not be empty",
            ));
        }
        let Some(&index) = entry_by_identity.get(&identity) else {
            return Err(Error::at(
                &source.source,
                source.line,
                format!(
                    "reviewed detail for reading '{}' and surface '{}' is not in the final dictionary",
                    source.reading, source.surface
                ),
            ));
        };
        let entry = &entries[index];
        if entry.word_cost != source.word_cost
            || entry.prediction_cost != source.prediction_cost
            || entry.flags != source.flags
            || entry.annotation != source.annotation
        {
            return Err(Error::at(
                &source.source,
                source.line,
                format!(
                    "reviewed detail source no longer matches the final entry for reading '{}' and surface '{}'",
                    source.reading, source.surface
                ),
            ));
        }
        clear_annotations.push(index);
        details.push(SourceDetail {
            reading: source.reading.clone(),
            surface: source.surface.clone(),
            left_id: source.left_id,
            right_id: source.right_id,
            description: source.annotation.clone(),
            relations: Vec::new(),
        });
    }

    drop(entry_by_identity);
    for index in clear_annotations {
        entries[index].annotation.clear();
    }
    Ok(details)
}

/// Clears every candidate-list note after reviewed descriptions have moved
/// into details.
///
/// Runtime notes such as `履歴` are not stored in the image. Anything left in
/// this column — a `[calibration]` overlay comment, a `[company]` casing
/// label, or a glossary gloss that was never extracted — would show next to
/// the candidate.
pub fn clear_candidate_list_annotations(entries: &mut [SourceEntry]) {
    for entry in entries {
        entry.annotation.clear();
    }
}

fn sort_and_validate_layer(entries: &mut [SourceEntry], label: &str) -> Result<(), Error> {
    entries.sort_by(entry_identity_cmp);
    for pair in entries.windows(2) {
        let [before, after] = pair else {
            continue;
        };
        if entry_identity_cmp(before, after) == Ordering::Equal {
            return Err(Error::at(
                &after.source,
                after.line,
                format!(
                    "duplicate {label} entry for reading '{}' and surface '{}'",
                    after.reading, after.surface
                ),
            ));
        }
    }
    Ok(())
}

fn entry_identity_cmp(left: &SourceEntry, right: &SourceEntry) -> Ordering {
    (&left.reading, &left.surface, left.left_id, left.right_id).cmp(&(
        &right.reading,
        &right.surface,
        right.left_id,
        right.right_id,
    ))
}

fn encode_matrix(connection: &ConnectionMatrix) -> Result<Vec<u8>, Error> {
    let classes = usize::from(connection.class_count);
    let mut modes = Vec::with_capacity(classes);
    let mut rows = Vec::with_capacity(classes);
    let mut overrides = Vec::new();

    for row in 0..classes {
        let start_at = row
            .checked_mul(classes)
            .ok_or_else(|| Error::build("connection row overflow"))?;
        let costs = connection
            .costs
            .get(start_at..start_at + classes)
            .ok_or_else(|| Error::build("connection row is truncated"))?;
        let mut frequencies = BTreeMap::<u16, usize>::new();
        for cost in costs {
            *frequencies.entry(*cost).or_default() += 1;
        }
        let mut mode = 0u16;
        let mut mode_count = 0usize;
        for (cost, count) in frequencies {
            if count > mode_count {
                mode = cost;
                mode_count = count;
            }
        }

        let first = overrides.len();
        for (left_id, cost) in costs.iter().copied().enumerate() {
            if cost != mode {
                overrides.push((as_u16(left_id, "matrix left id")?, cost));
            }
        }
        modes.push(mode);
        rows.push((
            as_u32(first, "matrix row start")?,
            as_u32(overrides.len() - first, "matrix row length")?,
        ));
    }

    let mut matrix = Vec::new();
    matrix.extend_from_slice(&format::MATRIX_MAGIC);
    put_u16(&mut matrix, connection.class_count);
    put_u16(&mut matrix, 0);
    put_u32(
        &mut matrix,
        as_u32(overrides.len(), "matrix override count")?,
    );
    put_u32(&mut matrix, 0);
    for mode in modes {
        put_u16(&mut matrix, mode);
    }
    while matrix.len() % 4 != 0 {
        matrix.push(0);
    }
    for (start, count) in rows {
        put_u32(&mut matrix, start);
        put_u32(&mut matrix, count);
    }
    for (left_id, cost) in overrides {
        put_u16(&mut matrix, left_id);
        put_u16(&mut matrix, cost);
    }
    if matrix.len() > MAX_MATRIX_IMAGE_BYTES {
        return Err(Error::build(format!(
            "encoded connection matrix is {} bytes; budget is {MAX_MATRIX_IMAGE_BYTES}",
            matrix.len()
        )));
    }
    Ok(matrix)
}

#[derive(Debug, Default)]
struct TrieNode {
    children: BTreeMap<char, usize>,
    entries: Vec<usize>,
}

fn build_trie(entries: &[SourceEntry]) -> Result<Vec<TrieNode>, Error> {
    let mut nodes = vec![TrieNode::default()];
    for (entry_index, entry) in entries.iter().enumerate() {
        let mut node_index = 0usize;
        for label in entry.reading.chars() {
            let existing = nodes[node_index].children.get(&label).copied();
            node_index = if let Some(child) = existing {
                child
            } else {
                let child = nodes.len();
                nodes.push(TrieNode::default());
                nodes[node_index].children.insert(label, child);
                child
            };
        }
        nodes[node_index].entries.push(entry_index);
        if nodes[node_index].entries.len() > usize::from(u16::MAX) {
            return Err(Error::at(
                &entry.source,
                entry.line,
                "too many homophones for one reading",
            ));
        }
    }
    Ok(nodes)
}

struct Flattened {
    louds: Vec<u8>,
    louds_bits: usize,
    nodes: Vec<u8>,
    labels: Vec<u8>,
    entries: Vec<u8>,
    node_count: usize,
    source_to_image_entry: Vec<usize>,
}

fn flatten_trie(
    trie: &[TrieNode],
    source_entries: &[SourceEntry],
    surface_ids: &BTreeMap<&str, u32>,
    annotation_ids: &BTreeMap<&str, u32>,
) -> Result<Flattened, Error> {
    let mut queue = VecDeque::new();
    queue.push_back((0usize, '\0'));
    let mut bfs = Vec::with_capacity(trie.len());
    while let Some(node) = queue.pop_front() {
        bfs.push(node);
        for (label, child) in &trie[node.0].children {
            queue.push_back((*child, *label));
        }
    }
    if bfs.len() != trie.len() {
        return Err(Error::build("trie contains unreachable nodes"));
    }

    let mut old_to_new = vec![usize::MAX; trie.len()];
    for (new_index, (old_index, _)) in bfs.iter().enumerate() {
        old_to_new[*old_index] = new_index;
    }

    let mut louds_bits_vec = Vec::with_capacity(trie.len() * 2);
    let mut nodes = Vec::with_capacity(trie.len() * format::NODE_LEN);
    let mut labels = Vec::with_capacity(trie.len() * 4);
    let mut entries = Vec::with_capacity(source_entries.len() * format::ENTRY_LEN);
    let mut emitted_entries = 0usize;
    let mut source_to_image_entry = vec![usize::MAX; source_entries.len()];

    for (old_index, incoming) in &bfs {
        let node = &trie[*old_index];
        let child_count = node.children.len();
        let first_child = node
            .children
            .values()
            .next()
            .map(|old| old_to_new[*old])
            .unwrap_or(0);
        let value_start = emitted_entries;
        for entry_index in &node.entries {
            write_entry(
                &mut entries,
                &source_entries[*entry_index],
                surface_ids,
                annotation_ids,
            )?;
            source_to_image_entry[*entry_index] = emitted_entries;
            emitted_entries += 1;
        }
        put_u32(&mut nodes, as_u32(first_child, "node index")?);
        put_u16(&mut nodes, as_u16(child_count, "child count")?);
        put_u16(
            &mut nodes,
            as_u16(node.entries.len(), "terminal value count")?,
        );
        put_u32(&mut nodes, as_u32(value_start, "entry index")?);
        put_u32(&mut nodes, 0);
        put_u32(&mut labels, *incoming as u32);
        louds_bits_vec.extend(std::iter::repeat_n(true, child_count));
        louds_bits_vec.push(false);
    }
    if emitted_entries != source_entries.len() {
        return Err(Error::build(
            "not every source entry reached a terminal node",
        ));
    }
    let louds_bits = louds_bits_vec.len();
    let mut louds = Vec::with_capacity(4 + louds_bits.div_ceil(8));
    put_u32(&mut louds, as_u32(louds_bits, "LOUDS bit count")?);
    louds.resize(4 + louds_bits.div_ceil(8), 0);
    for (index, set) in louds_bits_vec.into_iter().enumerate() {
        if set {
            louds[4 + index / 8] |= 1 << (index % 8);
        }
    }
    Ok(Flattened {
        louds,
        louds_bits,
        nodes,
        labels,
        entries,
        node_count: trie.len(),
        source_to_image_entry,
    })
}

fn write_entry(
    out: &mut Vec<u8>,
    entry: &SourceEntry,
    surface_ids: &BTreeMap<&str, u32>,
    annotation_ids: &BTreeMap<&str, u32>,
) -> Result<(), Error> {
    let surface_id = surface_ids
        .get(entry.surface.as_str())
        .copied()
        .ok_or_else(|| Error::build("surface id disappeared during compilation"))?;
    let annotation_id = if entry.annotation.is_empty() {
        format::NO_ANNOTATION
    } else {
        annotation_ids
            .get(entry.annotation.as_str())
            .copied()
            .ok_or_else(|| Error::build("annotation id disappeared during compilation"))?
    };
    put_u32(out, surface_id);
    put_u16(out, entry.left_id);
    put_u16(out, entry.right_id);
    put_i32(out, entry.word_cost);
    put_i32(out, entry.prediction_cost);
    put_u16(out, entry.flags.bits());
    put_u16(out, 0);
    put_u32(out, annotation_id);
    Ok(())
}

fn front_code(values: &[String]) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut offsets = Vec::with_capacity(values.len() * 4);
    let mut data = Vec::new();
    let mut previous = "";
    for (index, value) in values.iter().enumerate() {
        put_u32(&mut offsets, as_u32(data.len(), "surface offset")?);
        let prefix = if index % format::SURFACE_RESTART_INTERVAL == 0 {
            0
        } else {
            common_prefix_bytes(previous, value)
        };
        let suffix = &value[prefix..];
        put_u16(&mut data, as_u16(prefix, "surface prefix")?);
        put_u16(&mut data, as_u16(suffix.len(), "surface suffix")?);
        data.extend_from_slice(suffix.as_bytes());
        previous = value;
    }
    Ok((offsets, data))
}

fn raw_text_table(values: &[String]) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut offsets = Vec::with_capacity(values.len() * 4);
    let mut data = Vec::new();
    for value in values {
        put_u32(&mut offsets, as_u32(data.len(), "text offset")?);
        data.extend_from_slice(value.as_bytes());
    }
    Ok((offsets, data))
}

fn encode_details(
    source_details: &[SourceDetail],
    entry_ordinals: &BTreeMap<(&str, &str, u16, u16), usize>,
) -> Result<Vec<TableData>, Error> {
    const MAX_RELATION_TARGET_BYTES: usize = 16 * 1024;
    let mut details = source_details.to_vec();
    details.sort_by(|left, right| {
        (&left.reading, &left.surface, left.left_id, left.right_id).cmp(&(
            &right.reading,
            &right.surface,
            right.left_id,
            right.right_id,
        ))
    });
    for pair in details.windows(2) {
        if pair[0].reading == pair[1].reading
            && pair[0].surface == pair[1].surface
            && pair[0].left_id == pair[1].left_id
            && pair[0].right_id == pair[1].right_id
        {
            return Err(Error::build(format!(
                "duplicate detail for reading '{}' and surface '{}'",
                pair[1].reading, pair[1].surface
            )));
        }
    }

    let mut all_text = BTreeSet::new();
    for detail in &details {
        if detail.reading.is_empty() || detail.surface.is_empty() || detail.description.is_empty() {
            return Err(Error::build(
                "detail identity and description must not be empty",
            ));
        }
        if detail.description.contains('\0') {
            return Err(Error::build("detail description contains NUL"));
        }
        if !entry_ordinals.contains_key(&(
            detail.reading.as_str(),
            detail.surface.as_str(),
            detail.left_id,
            detail.right_id,
        )) {
            return Err(Error::build(format!(
                "detail for reading '{}' and surface '{}' is not in the compiled dictionary",
                detail.reading, detail.surface
            )));
        }
        all_text.insert(detail.description.clone());
        for relation in &detail.relations {
            if relation.target.is_empty()
                || relation.target.contains('\0')
                || relation.target.len() > MAX_RELATION_TARGET_BYTES
            {
                return Err(Error::build(
                    "detail relation target is invalid or exceeds 16 KiB",
                ));
            }
            all_text.insert(relation.target.clone());
        }
    }
    let texts = all_text.into_iter().collect::<Vec<_>>();
    let text_ids = texts
        .iter()
        .enumerate()
        .map(|(index, text)| Ok((text.as_str(), as_u32(index, "detail text id")?)))
        .collect::<Result<BTreeMap<_, _>, Error>>()?;
    let (text_offsets, text) = raw_text_table(&texts)?;

    let mut index_records = Vec::with_capacity(details.len());
    let mut records = Vec::with_capacity(details.len() * format::DETAIL_RECORD_LEN);
    let mut relations = Vec::new();
    for (record_id, detail) in details.iter().enumerate() {
        let image_entry = *entry_ordinals
            .get(&(
                detail.reading.as_str(),
                detail.surface.as_str(),
                detail.left_id,
                detail.right_id,
            ))
            .ok_or_else(|| Error::build("detail identity disappeared during compilation"))?;
        index_records.push((
            as_u32(image_entry, "detail entry ordinal")?,
            as_u32(record_id, "detail record id")?,
        ));
        let description_id = *text_ids
            .get(detail.description.as_str())
            .ok_or_else(|| Error::build("detail description disappeared during compilation"))?;
        let display_id = description_id;
        put_u32(&mut records, description_id);
        put_u32(&mut records, display_id);
        put_u32(
            &mut records,
            as_u32(
                relations.len() / format::DETAIL_RELATION_LEN,
                "detail relation start",
            )?,
        );
        let mut unique = BTreeSet::<(u8, &str)>::new();
        for relation in &detail.relations {
            unique.insert((relation.kind as u8, relation.target.as_str()));
        }
        put_u32(&mut records, as_u32(unique.len(), "detail relation count")?);
        for (kind, target) in unique {
            relations.push(kind);
            relations.extend_from_slice(&[0, 0, 0]);
            let text_id = *text_ids
                .get(target)
                .ok_or_else(|| Error::build("detail relation disappeared during compilation"))?;
            put_u32(&mut relations, text_id);
        }
    }
    // DIDX is searched by final ENTR ordinal, not source spelling.  Detail
    // records may remain source-sorted because their relation offsets are local.
    index_records.sort_unstable_by_key(|(entry, _)| *entry);
    let mut index = Vec::with_capacity(index_records.len() * format::DETAIL_INDEX_LEN);
    for (entry, record) in index_records {
        put_u32(&mut index, entry);
        put_u32(&mut index, record);
    }
    let relation_count = relations.len() / format::DETAIL_RELATION_LEN;
    Ok(vec![
        TableData::new(format::TAG_DETAIL_INDEX, index, details.len()),
        TableData::new(format::TAG_DETAIL_RECORDS, records, details.len()),
        TableData::new(format::TAG_DETAIL_RELATIONS, relations, relation_count),
        TableData::new(format::TAG_DETAIL_TEXT_OFFSETS, text_offsets, texts.len()),
        TableData::new(format::TAG_DETAIL_TEXT, text, texts.len()),
    ])
}

fn common_prefix_bytes(left: &str, right: &str) -> usize {
    let limit = left.len().min(right.len());
    let mut prefix = 0usize;
    for ((left_at, left_char), (right_at, right_char)) in
        left.char_indices().zip(right.char_indices())
    {
        if left_at != right_at || left_char != right_char {
            break;
        }
        let end = left_at + left_char.len_utf8();
        if end > limit {
            break;
        }
        prefix = end;
    }
    prefix
}

struct TableData {
    tag: [u8; 4],
    bytes: Vec<u8>,
    count: usize,
}

impl TableData {
    fn new(tag: [u8; 4], bytes: Vec<u8>, count: usize) -> Self {
        Self { tag, bytes, count }
    }
}

fn assemble_image(
    class_count: u16,
    entry_count: usize,
    node_count: usize,
    tables: Vec<TableData>,
) -> Result<Vec<u8>, Error> {
    if tables.len() > format::MAX_TABLES {
        return Err(Error::build("too many image tables"));
    }
    let directory_bytes = tables
        .len()
        .checked_mul(format::DIRECTORY_ENTRY_LEN)
        .ok_or_else(|| Error::build("directory size overflow"))?;
    let prefix = format::HEADER_LEN
        .checked_add(directory_bytes)
        .ok_or_else(|| Error::build("header size overflow"))?;
    let mut image = vec![0u8; prefix];
    let mut directory = Vec::with_capacity(tables.len());
    for table in tables {
        while image.len() % 8 != 0 {
            image.push(0);
        }
        let offset = image.len();
        image.extend_from_slice(&table.bytes);
        directory.push((table.tag, offset, table.bytes.len(), table.count));
    }
    if image.len() > u32::MAX as usize {
        return Err(Error::build(
            "dictionary image exceeds the 4 GiB format limit",
        ));
    }

    image[0..8].copy_from_slice(&format::MAGIC);
    write_u16_at(&mut image, 8, format::VERSION)?;
    write_u16_at(&mut image, 10, format::HEADER_LEN as u16)?;
    write_u16_at(&mut image, 12, as_u16(directory.len(), "table count")?)?;
    write_u16_at(&mut image, 14, class_count)?;
    write_u32_at(&mut image, 16, as_u32(entry_count, "entry count")?)?;
    write_u32_at(&mut image, 20, as_u32(node_count, "node count")?)?;
    let image_len = as_u32(image.len(), "image length")?;
    write_u32_at(&mut image, 24, image_len)?;
    write_u32_at(&mut image, 28, 0)?;
    for (index, (tag, offset, len, count)) in directory.into_iter().enumerate() {
        let at = format::HEADER_LEN + index * format::DIRECTORY_ENTRY_LEN;
        image[at..at + 4].copy_from_slice(&tag);
        write_u32_at(&mut image, at + 4, as_u32(offset, "table offset")?)?;
        write_u32_at(&mut image, at + 8, as_u32(len, "table length")?)?;
        write_u32_at(&mut image, at + 12, as_u32(count, "table count")?)?;
    }

    Dictionary::parse(&image)
        .map_err(|error| Error::build(format!("writer produced an invalid image: {error}")))?;
    Ok(image)
}

fn validate_license(source: &str, license: Option<&str>) -> Result<(), Error> {
    let license = license.ok_or_else(|| Error::at(source, 1, "missing license declaration"))?;
    if ALLOWED_LICENSES.contains(&license) {
        Ok(())
    } else {
        Err(Error::at(
            source,
            1,
            format!("license '{license}' is not on the dictionary-data allowlist"),
        ))
    }
}

fn validate_reading(source: &str, line: usize, reading: &str) -> Result<(), Error> {
    validate_text(source, line, "reading", reading)?;
    if reading.chars().any(char::is_control) {
        return Err(Error::at(
            source,
            line,
            "reading must not contain control characters",
        ));
    }
    Ok(())
}

fn validate_mozc_reading(source: &str, line: usize, reading: &str) -> Result<(), Error> {
    validate_text(source, line, "reading", reading)?;
    if reading.chars().any(char::is_control) {
        return Err(Error::at(
            source,
            line,
            "Mozc reading must not contain control characters",
        ));
    }
    Ok(())
}

fn validate_text(source: &str, line: usize, field: &str, value: &str) -> Result<(), Error> {
    if field != "annotation" && value.is_empty() {
        return Err(Error::at(
            source,
            line,
            format!("{field} must not be empty"),
        ));
    }
    if value.as_bytes().contains(&0) {
        return Err(Error::at(source, line, format!("{field} contains NUL")));
    }
    if value.len() > MAX_PREEDIT_BYTES {
        return Err(Error::at(
            source,
            line,
            format!("{field} exceeds {MAX_PREEDIT_BYTES} UTF-8 bytes"),
        ));
    }
    Ok(())
}

fn parse_flags(source: &str, line: usize, value: &str) -> Result<EntryFlags, Error> {
    let mut flags = EntryFlags::NONE;
    if value.is_empty() {
        return Ok(flags);
    }
    for flag in value.split(',') {
        let parsed = match flag {
            "it" => EntryFlags::IT,
            "predict" => EntryFlags::PREDICTION,
            "correction" => EntryFlags::SPELLING_CORRECTION,
            _ => return Err(Error::at(source, line, format!("unknown flag '{flag}'"))),
        };
        if flags.contains(parsed) {
            return Err(Error::at(source, line, format!("duplicate flag '{flag}'")));
        }
        flags = flags | parsed;
    }
    Ok(flags)
}

fn parse_number<T>(source: &str, line: usize, field: &str, value: &str) -> Result<T, Error>
where
    T: core::str::FromStr,
{
    value
        .parse::<T>()
        .map_err(|_| Error::at(source, line, format!("invalid {field} '{value}'")))
}

fn as_u16(value: usize, field: &str) -> Result<u16, Error> {
    u16::try_from(value).map_err(|_| Error::build(format!("{field} exceeds u16")))
}

fn as_u32(value: usize, field: &str) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| Error::build(format!("{field} exceeds u32")))
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u16_at(out: &mut [u8], at: usize, value: u16) -> Result<(), Error> {
    let target = out
        .get_mut(at..at + 2)
        .ok_or_else(|| Error::build("header write outside image"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_u32_at(out: &mut [u8], at: usize, value: u32) -> Result<(), Error> {
    let target = out
        .get_mut(at..at + 4)
        .ok_or_else(|| Error::build("header write outside image"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}
