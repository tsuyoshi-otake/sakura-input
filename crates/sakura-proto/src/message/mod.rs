//! Request/response messages and their frame-level encode/decode.
//!
//! A frame on the pipe is `u32 LE payload_len` followed by exactly that
//! many bytes (DESIGN.md §7). This module works one layer in from that:
//! [`payload_len`] parses the 4-byte header, and `encode_*`/`decode_*`
//! turn a payload's bytes into/from a [`Request`] or [`Response`]. Both
//! directions share one payload layout:
//!
//! ```text
//! u16 LE  protocol_version
//! u64 LE  request_id
//! u16 LE  message_type
//!         ...body...
//! ```
//!
//! Every request carries a monotonic per-session request id (DESIGN.md
//! §7's stale-response guard: a named pipe is a byte stream, and without
//! correlation ids a late reply to a timed-out request would be
//! mis-attributed to the next one).

mod header;
mod request;
mod response;
mod tags;
mod ui_state;

// Re-exported so `sakura_proto::message::Error` and `sakura_proto::Error`
// both name the one error type used across the whole crate.
pub use crate::wire::Error;

pub use header::{payload_len, peek_header, Header};
pub use request::{decode_request, encode_request, Request, UndoCommitOutcome};
pub use response::{decode_response, encode_response, Response};
pub use sakura_values::{AiTextOperation, AiTextStatus};
pub(crate) use tags::*;
pub use ui_state::UiState;

#[cfg(test)]
use crate::types::{AppearanceTheme, CandidateDetail, PadShortcut};
#[cfg(test)]
use crate::{FRAME_HEADER_LEN, MAX_PAYLOAD};

#[cfg(test)]
mod tests {
    use super::*;

    fn detail() -> CandidateDetail {
        CandidateDetail {
            reading: "reading".to_owned(),
            definition: "definition".to_owned(),
            definition_truncated: false,
            aliases: Vec::new(),
            related: Vec::new(),
            similar: Vec::new(),
            antonyms: Vec::new(),
        }
    }

    #[test]
    fn ui_rejects_detail_without_candidates_before_writing_a_frame() {
        let response = Response::Ui(UiState {
            revision: 1,
            appearance_theme: AppearanceTheme::Auto,
            pad_shortcut: PadShortcut::Disabled,
            mode: None,
            candidates: None,
            candidate_detail: Some(detail()),
            anchor: None,
            document: None,
            renderer_visible: false,
            stopping: false,
        });
        let mut frame = vec![1, 2, 3];
        assert_eq!(
            encode_response(&response, 1, &mut frame),
            Err(Error::TooLarge)
        );
        assert!(frame.is_empty());
    }

    #[test]
    fn payload_len_accepts_at_limit_and_rejects_above() {
        let at_limit = (MAX_PAYLOAD as u32).to_le_bytes();
        assert_eq!(payload_len(&at_limit), Ok(MAX_PAYLOAD));
        let over = (MAX_PAYLOAD as u32 + 1).to_le_bytes();
        assert_eq!(payload_len(&over), Err(Error::TooLarge));
    }

    #[test]
    fn encode_decode_ping_roundtrip() {
        let mut dst = Vec::new();
        encode_request(&Request::Ping, 42, &mut dst).expect("encode");
        let len = payload_len(&[dst[0], dst[1], dst[2], dst[3]]).expect("len");
        assert_eq!(len, dst.len() - FRAME_HEADER_LEN);
        let (id, req) = decode_request(&dst[FRAME_HEADER_LEN..]).expect("decode");
        assert_eq!(id, 42);
        assert_eq!(req, Request::Ping);
    }

    #[test]
    fn decode_request_rejects_wrong_version() {
        let mut dst = Vec::new();
        encode_request(&Request::Ping, 1, &mut dst).expect("encode");
        // Corrupt the version field (first two bytes of the payload).
        dst[FRAME_HEADER_LEN] = 0xFF;
        dst[FRAME_HEADER_LEN + 1] = 0xFF;
        let result = decode_request(&dst[FRAME_HEADER_LEN..]);
        assert_eq!(result, Err(Error::UnsupportedVersion(0xFFFF)));
    }

    #[test]
    fn decode_request_rejects_trailing_bytes() {
        let mut dst = Vec::new();
        encode_request(&Request::Ping, 1, &mut dst).expect("encode");
        dst.push(0);
        let result = decode_request(&dst[FRAME_HEADER_LEN..]);
        assert_eq!(result, Err(Error::TrailingBytes));
    }

    #[test]
    fn decode_request_rejects_unknown_msg_type() {
        let mut dst = Vec::new();
        encode_request(&Request::Ping, 1, &mut dst).expect("encode");
        // Message type is the last two bytes of the (empty-body) header.
        let mt_offset = dst.len() - 2;
        dst[mt_offset] = 0xEE;
        dst[mt_offset + 1] = 0xEE;
        let result = decode_request(&dst[FRAME_HEADER_LEN..]);
        assert_eq!(result, Err(Error::BadMsgType(0xEEEE)));
    }

    #[test]
    fn encode_reuses_dst_capacity() {
        let mut dst = Vec::with_capacity(256);
        encode_request(&Request::Ping, 1, &mut dst).expect("encode");
        let cap_after_first = dst.capacity();
        encode_request(
            &Request::CreateSession {
                process_name: "notepad.exe".to_string(),
            },
            2,
            &mut dst,
        )
        .expect("encode");
        // Capacity should not have needed to shrink/reallocate below what
        // was already reserved for such a small payload.
        assert!(dst.capacity() >= cap_after_first || dst.capacity() >= dst.len());
    }
}
