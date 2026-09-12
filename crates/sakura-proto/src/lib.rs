//! Sakura Input IPC protocol: hand-rolled, fixed-layout, versioned.
//!
//! This crate is the **stable contract** between the TSF DLL, the engine,
//! and the renderer (DESIGN.md §5.5, §7). It depends only on the workspace
//! `sakura-values` crate and std, has no third-party dependencies or `unsafe`, and every decode path
//! returns `Result` instead of panicking — the pipe is a hostile boundary
//! and every byte on it is untrusted input.
//!
//! # Wire format
//!
//! A frame is `u32 LE payload_len` followed by exactly that many bytes
//! (capped at [`MAX_PAYLOAD`]). A payload is:
//!
//! ```text
//! u16 LE  protocol_version
//! u64 LE  request_id
//! u16 LE  message_type
//!         ...body...
//! ```
//!
//! `request_id` is a monotonic per-session id that lets a client match a
//! late reply to the request it answers, even across a timeout (DESIGN.md
//! §7). A decoder that reads a version other than [`PROTOCOL_VERSION`]
//! fails with `Error::UnsupportedVersion` rather than guessing at an
//! unknown layout — version negotiation, not silent leniency, is how this
//! protocol evolves.
//!
//! # Modules
//!
//! - [`fixed`] — [`FixedStr`]/[`FixedVec`], the allocation-free containers
//!   the rest of the crate is built from.
//! - [`wire`] — the primitive cursor-based reader/writer and the shared
//!   [`Error`] type.
//! - [`types`] — domain value types (`KeyCode`, `Mode`, `Output`, ...).
//! - [`message`] — [`Request`]/[`Response`] and their frame-level
//!   encode/decode.
//! - [`output`] — [`OutputBuf`], the zero-allocation builder used on the
//!   engine's hot path (DESIGN.md §5.7).

pub mod fixed {
    pub use sakura_values::{FixedStr, FixedVec, Overflow};
}
pub mod message;
pub mod output;
pub mod types;
pub mod wire;
mod wire_types;

pub use message::{
    decode_request, decode_response, encode_request, encode_response, payload_len, peek_header,
    Header, Request, Response, UiState, UndoCommitOutcome,
};
pub use output::{
    CandidateDetailInput, CandidateDetailRef, CandidateDetailTerms, OutputBuf, SegSpan,
};
pub use sakura_values::{
    AiTextOperation, AiTextStatus, AppearanceTheme, FixedStr, FixedVec, InputScope, KeyCode,
    KeyInput, Mode, Modifiers, Overflow, PadShortcut, CANDIDATE_PAGE_SIZE, MAX_CANDIDATES,
    MAX_CANDIDATE_DETAIL_DEFINITION_BYTES, MAX_CANDIDATE_DETAIL_READING_BYTES,
    MAX_CANDIDATE_DETAIL_RELATIONS, MAX_CANDIDATE_DETAIL_RELATION_BYTES,
    MAX_CANDIDATE_DETAIL_RELATION_TEXT_BYTES, MAX_CANDIDATE_TEXT_BYTES, MAX_COMMIT_BYTES,
    MAX_PREEDIT_BYTES, MAX_SEGMENTS,
};
pub use types::{
    Candidate, CandidateDetail, CandidateKind, CandidateList, EngineTimingEntry, EngineTimingSite,
    ErrorCode, FaultInjectionEntry, FaultPoint, Output, Preedit, ScreenRect, Segment,
    UnderlineKind,
};
pub use wire::Error;
pub use wire_types::Wire;

/// The protocol version this crate implements. Carried in every payload;
/// a decoder rejects any other value with `Error::UnsupportedVersion`.
pub const PROTOCOL_VERSION: u16 = 22;

/// The largest payload (the bytes after the 4-byte frame length prefix)
/// this protocol allows. A frame whose declared length exceeds this is
/// rejected before its body is even read.
pub const MAX_PAYLOAD: usize = 64 * 1024;

/// The length, in bytes, of the frame's length prefix (`u32 LE`).
pub const FRAME_HEADER_LEN: usize = 4;

/// The largest complete frame (header + payload) this protocol allows.
pub const MAX_FRAME: usize = FRAME_HEADER_LEN + MAX_PAYLOAD;

/// The largest UTF-8 byte length allowed for any individual `&str` field
/// on the wire (e.g. a process name, a segment's text).
pub const MAX_STRING_BYTES: usize = 4096;

/// Identifies one editing session on the engine.
pub type SessionId = u64;

/// Identifies one request, for stale-response correlation (see the crate
/// docs' "Wire format" section).
pub type RequestId = u64;

/// Identifies one version of the UI state the renderer draws (see
/// [`UiState`]). Monotonic for the life of the engine.
pub type Revision = u64;
