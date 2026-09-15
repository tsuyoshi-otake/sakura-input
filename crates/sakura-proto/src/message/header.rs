//! The fixed payload header and frame assembly shared by both directions.

use crate::wire::{Reader, Sink, VecSink};
use crate::{RequestId, FRAME_HEADER_LEN, MAX_PAYLOAD, PROTOCOL_VERSION};

use super::Error;

/// The fixed-layout header shared by every payload, decoded without
/// interpreting the message body. Useful for triage (e.g. logging or
/// routing) before committing to a full [`decode_request`](super::decode_request)/
/// [`decode_response`](super::decode_response) call, which additionally validates the protocol
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
pub(super) fn encode_frame(
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
