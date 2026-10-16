/// Current durable learning log format.
pub const LEARNING_FORMAT_VERSION: u16 = 3;
pub const FORMAT_VERSION_1: u16 = 1;
pub const FORMAT_VERSION_2: u16 = 2;
pub const HEADER_LEN: usize = 8;
pub const RECORD_ENVELOPE_LEN: usize = 8;
pub const MAX_RECORD_BYTES: usize = 16 * 1024;
pub const MAX_LEARNING_LOG_BYTES: u64 = 20 * 1024 * 1024;
pub const RECORD_COMMIT: u8 = 1;
/// Marker context pair for a repair-suppress record encoded as a COMMIT frame.
/// Keeps the learning format version unchanged while still using the CRC envelope.
pub const REPAIR_SUPPRESS_CONTEXT: u16 = u16::MAX;
pub const REPAIR_SUPPRESS_SURFACE: &str = "\u{E000}sakura-repair-suppress";
/// A correction tombstone (#305) is a COMMIT frame with an empty reading,
/// which no commit has. Its surface is `REJECTION_PREFIX`, the reading,
/// `REJECTION_SEPARATOR` and the rejected surface; its contexts are the left
/// context of the rejection and the rejected surface's right ID. An engine
/// that predates tombstones replays it as a commit of the empty reading,
/// which no conversion or prediction looks up.
pub const REJECTION_PREFIX: char = '\u{E001}';
pub const REJECTION_SEPARATOR: char = '\u{E002}';

/// One durable learning event exposed to the settings viewer/exporter.
///
/// Events intentionally remain an append-order history instead of an
/// aggregation: exporting and clearing must be auditable, and retaining the
/// original context ids lets diagnostics explain why an exact-context choice
/// won over a general one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearningRecord {
    pub sequence: u64,
    pub day: u32,
    pub left_context: u16,
    pub right_context: u16,
    pub reading: String,
    pub surface: String,
}

/// A verified prefix of the checksummed learning log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearningSnapshot {
    pub format_version: u16,
    pub records: Vec<LearningRecord>,
    /// Correction tombstones (#305) in log order, holding the decoded
    /// reading and rejected surface. They are not commits, so they never
    /// appear in `records`.
    pub rejections: Vec<LearningRecord>,
    /// Bytes after the last checksummed record. The engine repairs this tail
    /// on open; a concurrent settings read reports it instead of presenting
    /// an incomplete event as valid data.
    pub ignored_tail_bytes: u64,
}
