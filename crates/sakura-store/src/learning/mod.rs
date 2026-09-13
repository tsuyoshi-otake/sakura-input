//! Durable learning records and byte-compatible pure codecs.

mod codec;
pub mod format;
mod log;
mod replay;

#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn test_crc32(bytes: &[u8]) -> u32 {
    codec::crc32(bytes)
}

#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn test_header(version: u16) -> [u8; HEADER_LEN] {
    codec::header(version)
}

#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn test_read_header(bytes: &[u8]) -> std::io::Result<u16> {
    codec::read_header(bytes)
}

#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn test_encode_record(
    reading: &str,
    surface: &str,
    left_context: u16,
    right_context: u16,
    day: u32,
) -> std::io::Result<Vec<u8>> {
    codec::encode_record(reading, surface, left_context, right_context, day)
}
pub use format::{
    LearningRecord, LearningSnapshot, FORMAT_VERSION_1, FORMAT_VERSION_2, HEADER_LEN,
    LEARNING_FORMAT_VERSION, MAX_LEARNING_LOG_BYTES, MAX_RECORD_BYTES, RECORD_COMMIT,
    RECORD_ENVELOPE_LEN, REPAIR_SUPPRESS_CONTEXT, REPAIR_SUPPRESS_SURFACE,
};
pub use log::{
    read_snapshot, LearningLog, LearningLogError, LogForget, LogMaintenance, OperationReceipt,
};
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub use log::{test_read_within_bound, LearningFaultPoint, LearningFaultScope};
pub use replay::{ReplayEvent, ReplayView};
