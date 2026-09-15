//! Wire values of every request and response `message_type`. A value, once
//! shipped, never changes meaning; new messages take the next free value.

// Wire values for each request message type.
pub(crate) const REQ_HELLO: u16 = 0x0001;
pub(crate) const REQ_CREATE_SESSION: u16 = 0x0002;
pub(crate) const REQ_SEND_KEY: u16 = 0x0003;
pub(crate) const REQ_COMMIT: u16 = 0x0004;
pub(crate) const REQ_REVERT: u16 = 0x0005;
pub(crate) const REQ_SET_INPUT_SCOPE: u16 = 0x0006;
pub(crate) const REQ_DELETE_SESSION: u16 = 0x0007;
pub(crate) const REQ_PING: u16 = 0x0008;
pub(crate) const REQ_SHUTDOWN: u16 = 0x0009;
pub(crate) const REQ_WATCH_UI: u16 = 0x000A;
pub(crate) const REQ_SET_UI_PLACEMENT: u16 = 0x000B;
pub(crate) const REQ_RECONVERT: u16 = 0x000C;
pub(crate) const REQ_CLEAR_LEARNING: u16 = 0x000D;
pub(crate) const REQ_CLEAR_INPUT_HISTORY: u16 = 0x000E;
pub(crate) const REQ_FLUSH_INPUT_HISTORY: u16 = 0x000F;
pub(crate) const REQ_INPUT_HISTORY_STATS: u16 = 0x0010;
pub(crate) const REQ_UNDO_COMMIT: u16 = 0x0011;
pub(crate) const REQ_PROBE_KEY: u16 = 0x0012;
pub(crate) const REQ_SET_MODE: u16 = 0x0013;
/// Deletes the exact learned prediction candidate in the renderer's current
/// UI snapshot. This is separate from keyboard-driven deletion because the
/// renderer owns no editing session and can only name a revision-stamped row.
pub(crate) const REQ_DELETE_HISTORY_CANDIDATE: u16 = 0x0014;
pub(crate) const REQ_APPLY_AI_COMPOSITION: u16 = 0x0015;
pub(crate) const REQ_RECORD_AI_TEXT: u16 = 0x0016;
pub(crate) const REQ_START_AI_TEXT: u16 = 0x0017;
pub(crate) const REQ_POLL_AI_TEXT: u16 = 0x0018;
pub(crate) const REQ_CANCEL_AI_TEXT: u16 = 0x0019;
pub(crate) const REQ_QUEUE_CANDIDATE_COMMIT: u16 = 0x001A;
pub(crate) const REQ_POLL_CANDIDATE_COMMIT: u16 = 0x001B;
pub(crate) const REQ_COMMIT_CANDIDATE: u16 = 0x001C;
/// Invalidates document-relative, memory-only context while preserving the
/// session's explicit input mode and profile configuration.
pub(crate) const REQ_RESET_DOCUMENT_CONTEXT: u16 = 0x001D;
/// Reads the engine's per-stage timing accumulators. Content-free: the reply
/// carries counts and durations only.
pub(crate) const REQ_ENGINE_TIMING: u16 = 0x001E;
/// Reads the engine's armed key-path delays. Content-free: points, durations
/// and counts only.
pub(crate) const REQ_FAULT_STATUS: u16 = 0x001F;

// Wire values for each response message type. `RES_OUTPUT` is also used
// directly by `crate::output::OutputBuf::encode_frame`, which encodes a
// `Response::Output` frame without allocating and so cannot go through
// `encode_response`.
pub(crate) const RES_HELLO: u16 = 0x8001;
pub(crate) const RES_SESSION_CREATED: u16 = 0x8002;
pub(crate) const RES_OUTPUT: u16 = 0x8003;
pub(crate) const RES_PONG: u16 = 0x8004;
pub(crate) const RES_OK: u16 = 0x8005;
pub(crate) const RES_UI: u16 = 0x8006;
pub(crate) const RES_INPUT_HISTORY_STATS: u16 = 0x8007;
pub(crate) const RES_INPUT_MODE: u16 = 0x8008;
pub(crate) const RES_HISTORY_CANDIDATE_DELETED: u16 = 0x8009;
pub(crate) const RES_AI_TEXT_STARTED: u16 = 0x800A;
pub(crate) const RES_AI_TEXT_PENDING: u16 = 0x800B;
pub(crate) const RES_AI_TEXT_RESULT: u16 = 0x800C;
pub(crate) const RES_CANDIDATE_COMMIT_QUEUED: u16 = 0x800D;
pub(crate) const RES_CANDIDATE_COMMIT_PENDING: u16 = 0x800E;
pub(crate) const RES_ENGINE_TIMING: u16 = 0x800F;
pub(crate) const RES_FAULT_STATUS: u16 = 0x8010;
pub(crate) const RES_ERROR: u16 = 0x80FF;
