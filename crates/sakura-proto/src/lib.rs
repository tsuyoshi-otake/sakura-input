//! Sakura Input IPC protocol: hand-rolled, fixed-layout, versioned.
//!
//! This crate is the **stable contract** between the TSF DLL, the engine,
//! and the renderer (DESIGN.md §5.5, §7). It has zero external
//! dependencies (std only), contains no `unsafe`, and every decode path
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

pub mod fixed;
pub mod message;
pub mod output;
pub mod types;
pub mod wire;

pub use fixed::{FixedStr, FixedVec, Overflow};
pub use message::{
    decode_request, decode_response, encode_request, encode_response, payload_len, peek_header,
    Header, Request, Response, UiState, UndoCommitOutcome,
};
pub use output::{OutputBuf, SegSpan};
pub use types::{
    Candidate, CandidateKind, CandidateList, ErrorCode, InputScope, KeyCode, KeyInput, Mode,
    Modifiers, Output, Preedit, ScreenRect, Segment, UnderlineKind,
};
pub use wire::Error;

/// The protocol version this crate implements. Carried in every payload;
/// a decoder rejects any other value with `Error::UnsupportedVersion`.
pub const PROTOCOL_VERSION: u16 = 11;

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

/// The capacity, in UTF-8 bytes, of [`OutputBuf`]'s preedit text buffer.
pub const MAX_PREEDIT_BYTES: usize = 1536;

/// The capacity, in UTF-8 bytes, of [`OutputBuf`]'s commit text buffer.
pub const MAX_COMMIT_BYTES: usize = 1536;

/// The maximum number of segments a preedit composition or an
/// [`OutputBuf`] may hold.
pub const MAX_SEGMENTS: usize = 64;

/// Candidates shown on one numbered page.
pub const CANDIDATE_PAGE_SIZE: usize = 9;

/// Maximum candidates carried in one output frame.
///
/// Two bounded pages make page navigation observable while keeping the
/// allocation-free engine hand-off compact.
pub const MAX_CANDIDATES: usize = CANDIDATE_PAGE_SIZE * 2;

/// Fixed storage for all candidate surfaces or annotations in an `OutputBuf`.
pub const MAX_CANDIDATE_TEXT_BYTES: usize = MAX_PREEDIT_BYTES * CANDIDATE_PAGE_SIZE;

/// Identifies one editing session on the engine.
pub type SessionId = u64;

/// Identifies one request, for stale-response correlation (see the crate
/// docs' "Wire format" section).
pub type RequestId = u64;

/// Identifies one version of the UI state the renderer draws (see
/// [`UiState`]). Monotonic for the life of the engine.
pub type Revision = u64;
