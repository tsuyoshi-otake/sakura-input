//! Durable learning records and byte-compatible pure codecs.

pub mod codec;
pub mod format;

pub use codec::{
    crc32, decode_record, encode_record, header, read_header, record_at, scan_records,
    upgrade_to_current, DecodedRecord,
};
pub use format::{
    LearningRecord, LearningSnapshot, FORMAT_VERSION_1, FORMAT_VERSION_2, HEADER_LEN,
    LEARNING_FORMAT_VERSION, MAX_LEARNING_LOG_BYTES, MAX_RECORD_BYTES, RECORD_COMMIT,
    RECORD_ENVELOPE_LEN, REPAIR_SUPPRESS_CONTEXT, REPAIR_SUPPRESS_SURFACE,
};
