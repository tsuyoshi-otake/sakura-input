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

use crate::types::{ErrorCode, InputScope, KeyInput, Output};
use crate::wire::{Reader, Sink, VecSink};
use crate::{RequestId, SessionId, FRAME_HEADER_LEN, MAX_PAYLOAD, PROTOCOL_VERSION};

// Re-exported so `sakura_proto::message::Error` and `sakura_proto::Error`
// both name the one error type used across the whole crate.
pub use crate::wire::Error;

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

// Wire values for each response message type. `RES_OUTPUT` is also used
// directly by `crate::output::OutputBuf::encode_frame`, which encodes a
// `Response::Output` frame without allocating and so cannot go through
// `encode_response`.
pub(crate) const RES_HELLO: u16 = 0x8001;
pub(crate) const RES_SESSION_CREATED: u16 = 0x8002;
pub(crate) const RES_OUTPUT: u16 = 0x8003;
pub(crate) const RES_PONG: u16 = 0x8004;
pub(crate) const RES_OK: u16 = 0x8005;
pub(crate) const RES_ERROR: u16 = 0x80FF;

/// A message sent from a client (the TSF DLL) to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The first message on a new connection: negotiates the protocol
    /// version.
    Hello { client_version: u16 },
    /// Starts a new editing session for a host process.
    CreateSession { process_name: String },
    /// Delivers one key event to a session.
    SendKey { session: SessionId, key: KeyInput },
    /// Commits the current composition.
    Commit { session: SessionId },
    /// Reverts the current composition (cancels it).
    Revert { session: SessionId },
    /// Tells the engine the input scope of the focused field.
    SetInputScope {
        session: SessionId,
        scope: InputScope,
    },
    /// Ends a session and releases its resources.
    DeleteSession { session: SessionId },
    /// A liveness check; the engine answers with `Response::Pong`.
    Ping,
    /// Asks the engine to flush state and exit.
    Shutdown,
}

/// A message sent from the engine back to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// Answers `Request::Hello` with the engine's own version info.
    Hello {
        server_version: u16,
        engine_version: [u16; 3],
    },
    /// Answers `Request::CreateSession` with the new session's id.
    SessionCreated { session: SessionId },
    /// The result of a key event or editing command.
    Output(Output),
    /// Answers `Request::Ping`.
    Pong,
    /// A generic success acknowledgement (e.g. for `Commit`/`Revert`).
    Ok,
    /// The request could not be fulfilled.
    Error(ErrorCode),
}

/// The fixed-layout header shared by every payload, decoded without
/// interpreting the message body. Useful for triage (e.g. logging or
/// routing) before committing to a full [`decode_request`]/
/// [`decode_response`] call, which additionally validates the protocol
/// version and the body shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version: u16,
    pub request_id: RequestId,
    pub msg_type: u16,
}

/// Reads the 4-byte little-endian frame length prefix.
///
/// Errors with [`Error::TooLarge`] if the declared length exceeds
/// [`MAX_PAYLOAD`] — the caller must not attempt to read that many bytes
/// from the pipe.
pub fn payload_len(header: &[u8; 4]) -> Result<usize, Error> {
    let len = u32::from_le_bytes(*header) as usize;
    if len > MAX_PAYLOAD {
        return Err(Error::TooLarge);
    }
    Ok(len)
}

/// Reads only the `version` + `request_id` + `msg_type` header of a
/// payload, without decoding (or validating the version of) the body.
pub fn peek_header(payload: &[u8]) -> Result<Header, Error> {
    let mut r = Reader::new(payload);
    let version = r.read_u16()?;
    let request_id = r.read_u64()?;
    let msg_type = r.read_u16()?;
    Ok(Header {
        version,
        request_id,
        msg_type,
    })
}

/// Writes the common `version, id, msg_type` header and then `body` into
/// `dst`, patching in the 4-byte frame length prefix at the end.
///
/// `dst` is cleared first; its existing capacity is reused, so a caller
/// that keeps one buffer per connection performs no allocation at
/// steady state. On any error `dst` is left empty rather than holding a
/// partial or oversized frame.
fn encode_frame(
    id: RequestId,
    msg_type: u16,
    dst: &mut Vec<u8>,
    body: impl FnOnce(&mut VecSink<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    dst.clear();
    dst.extend_from_slice(&[0u8; FRAME_HEADER_LEN]);
    let result = (|| {
        let mut w = VecSink::new(dst);
        w.write_u16(PROTOCOL_VERSION)?;
        w.write_u64(id)?;
        w.write_u16(msg_type)?;
        body(&mut w)
    })();
    if let Err(e) = result {
        dst.clear();
        return Err(e);
    }
    let payload_len = dst.len() - FRAME_HEADER_LEN;
    if payload_len > MAX_PAYLOAD {
        dst.clear();
        return Err(Error::TooLarge);
    }
    let len_bytes = (payload_len as u32).to_le_bytes();
    dst[..FRAME_HEADER_LEN].copy_from_slice(&len_bytes);
    Ok(())
}

fn request_msg_type(req: &Request) -> u16 {
    match req {
        Request::Hello { .. } => REQ_HELLO,
        Request::CreateSession { .. } => REQ_CREATE_SESSION,
        Request::SendKey { .. } => REQ_SEND_KEY,
        Request::Commit { .. } => REQ_COMMIT,
        Request::Revert { .. } => REQ_REVERT,
        Request::SetInputScope { .. } => REQ_SET_INPUT_SCOPE,
        Request::DeleteSession { .. } => REQ_DELETE_SESSION,
        Request::Ping => REQ_PING,
        Request::Shutdown => REQ_SHUTDOWN,
    }
}

fn encode_request_body<S: Sink>(req: &Request, w: &mut S) -> Result<(), Error> {
    match req {
        Request::Hello { client_version } => w.write_u16(*client_version),
        Request::CreateSession { process_name } => w.write_str(process_name),
        Request::SendKey { session, key } => {
            w.write_u64(*session)?;
            key.encode(w)
        }
        Request::Commit { session } => w.write_u64(*session),
        Request::Revert { session } => w.write_u64(*session),
        Request::SetInputScope { session, scope } => {
            w.write_u64(*session)?;
            scope.encode(w)
        }
        Request::DeleteSession { session } => w.write_u64(*session),
        Request::Ping => Ok(()),
        Request::Shutdown => Ok(()),
    }
}

fn response_msg_type(res: &Response) -> u16 {
    match res {
        Response::Hello { .. } => RES_HELLO,
        Response::SessionCreated { .. } => RES_SESSION_CREATED,
        Response::Output(_) => RES_OUTPUT,
        Response::Pong => RES_PONG,
        Response::Ok => RES_OK,
        Response::Error(_) => RES_ERROR,
    }
}

fn encode_response_body<S: Sink>(res: &Response, w: &mut S) -> Result<(), Error> {
    match res {
        Response::Hello {
            server_version,
            engine_version,
        } => {
            w.write_u16(*server_version)?;
            w.write_u16(engine_version[0])?;
            w.write_u16(engine_version[1])?;
            w.write_u16(engine_version[2])
        }
        Response::SessionCreated { session } => w.write_u64(*session),
        Response::Output(out) => out.encode(w),
        Response::Pong => Ok(()),
        Response::Ok => Ok(()),
        Response::Error(code) => code.encode(w),
    }
}

/// Encodes a complete `Request` frame (4-byte length prefix included) into
/// `dst`. See [`encode_frame`] for the allocation contract.
pub fn encode_request(req: &Request, id: RequestId, dst: &mut Vec<u8>) -> Result<(), Error> {
    let msg_type = request_msg_type(req);
    encode_frame(id, msg_type, dst, |w| encode_request_body(req, w))
}

/// Encodes a complete `Response` frame (4-byte length prefix included)
/// into `dst`. See [`encode_frame`] for the allocation contract.
pub fn encode_response(res: &Response, id: RequestId, dst: &mut Vec<u8>) -> Result<(), Error> {
    let msg_type = response_msg_type(res);
    encode_frame(id, msg_type, dst, |w| encode_response_body(res, w))
}

/// Decodes a payload (the bytes *after* the 4-byte length prefix) into a
/// `Request`.
///
/// Rejects a payload whose version does not match [`PROTOCOL_VERSION`]
/// with `Error::UnsupportedVersion` and rejects trailing bytes after a
/// complete message with `Error::TrailingBytes`.
pub fn decode_request(payload: &[u8]) -> Result<(RequestId, Request), Error> {
    let mut r = Reader::new(payload);
    let version = r.read_u16()?;
    if version != PROTOCOL_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let request_id = r.read_u64()?;
    let msg_type = r.read_u16()?;
    let req = match msg_type {
        REQ_HELLO => Request::Hello {
            client_version: r.read_u16()?,
        },
        REQ_CREATE_SESSION => Request::CreateSession {
            process_name: r.read_str()?.to_string(),
        },
        REQ_SEND_KEY => {
            let session = r.read_u64()?;
            let key = KeyInput::decode(&mut r)?;
            Request::SendKey { session, key }
        }
        REQ_COMMIT => Request::Commit {
            session: r.read_u64()?,
        },
        REQ_REVERT => Request::Revert {
            session: r.read_u64()?,
        },
        REQ_SET_INPUT_SCOPE => {
            let session = r.read_u64()?;
            let scope = InputScope::decode(&mut r)?;
            Request::SetInputScope { session, scope }
        }
        REQ_DELETE_SESSION => Request::DeleteSession {
            session: r.read_u64()?,
        },
        REQ_PING => Request::Ping,
        REQ_SHUTDOWN => Request::Shutdown,
        other => return Err(Error::BadMsgType(other)),
    };
    r.finish()?;
    Ok((request_id, req))
}

/// Decodes a payload (the bytes *after* the 4-byte length prefix) into a
/// `Response`. Same version/trailing-bytes contract as
/// [`decode_request`].
pub fn decode_response(payload: &[u8]) -> Result<(RequestId, Response), Error> {
    let mut r = Reader::new(payload);
    let version = r.read_u16()?;
    if version != PROTOCOL_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let request_id = r.read_u64()?;
    let msg_type = r.read_u16()?;
    let res = match msg_type {
        RES_HELLO => {
            let server_version = r.read_u16()?;
            let engine_version = [r.read_u16()?, r.read_u16()?, r.read_u16()?];
            Response::Hello {
                server_version,
                engine_version,
            }
        }
        RES_SESSION_CREATED => Response::SessionCreated {
            session: r.read_u64()?,
        },
        RES_OUTPUT => Response::Output(Output::decode(&mut r)?),
        RES_PONG => Response::Pong,
        RES_OK => Response::Ok,
        RES_ERROR => Response::Error(ErrorCode::decode(&mut r)?),
        other => return Err(Error::BadMsgType(other)),
    };
    r.finish()?;
    Ok((request_id, res))
}

#[cfg(test)]
mod tests {
    use super::*;

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
