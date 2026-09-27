//! One-request, bounded stdio contract for the experimental Pad crypto worker.
//!
//! The parent closes stdin after one request, reads one terminal response and
//! reaps the child. Cancellation is process termination by that owning parent;
//! it must invalidate its request generation before accepting later responses.
//! No credentials belong in command-line arguments, environment or diagnostics.

use std::io::{self, Read, Write};
use zeroize::Zeroizing;

const REQUEST_MAGIC: &[u8; 8] = b"SKRPWR01";
const RESPONSE_MAGIC: &[u8; 8] = b"SKRPWS01";
pub const MAX_PASSWORD_BYTES: usize = 1024;
pub const MAX_PAYLOAD_BYTES: usize = 24 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Seal,
    Open,
}

/// Deliberately has no Debug implementation that could expose secrets.
#[allow(missing_debug_implementations)]
pub struct Request {
    pub id: u64,
    pub operation: Operation,
    pub vault_id: [u8; 16],
    /// Zero denotes the whole Pad; nonzero denotes exactly one memo.
    pub memo_id: u64,
    pub password: Zeroizing<Vec<u8>>,
    pub payload: Zeroizing<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Success = 0,
    Rejected = 1,
    InvalidRequest = 2,
    Unavailable = 3,
}

#[allow(missing_debug_implementations)]
pub struct Response {
    pub id: u64,
    pub status: Status,
    /// Empty on every failure. Success may contain plaintext; erase on drop.
    pub payload: Zeroizing<Vec<u8>>,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid Pad worker frame")
}

fn read_array<const N: usize>(reader: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn end_of_frame(reader: &mut impl Read) -> io::Result<()> {
    let mut tail = [0];
    match reader.read(&mut tail)? {
        0 => Ok(()),
        _ => Err(invalid()),
    }
}

pub fn read_request(mut reader: impl Read) -> io::Result<Request> {
    if &read_array::<8>(&mut reader)? != REQUEST_MAGIC {
        return Err(invalid());
    }
    let id = u64::from_le_bytes(read_array(&mut reader)?);
    let operation = match read_array::<1>(&mut reader)?[0] {
        1 => Operation::Seal,
        2 => Operation::Open,
        _ => return Err(invalid()),
    };
    let vault_id = read_array(&mut reader)?;
    let memo_id = u64::from_le_bytes(read_array(&mut reader)?);
    let password_len = u16::from_le_bytes(read_array(&mut reader)?) as usize;
    let payload_len = u32::from_le_bytes(read_array(&mut reader)?) as usize;
    if id == 0
        || vault_id == [0; 16]
        || !(1..=MAX_PASSWORD_BYTES).contains(&password_len)
        || payload_len > MAX_PAYLOAD_BYTES
    {
        return Err(invalid());
    }
    let mut password = Zeroizing::new(vec![0; password_len]);
    reader.read_exact(&mut password)?;
    let mut payload = Zeroizing::new(vec![0; payload_len]);
    reader.read_exact(&mut payload)?;
    end_of_frame(&mut reader)?;
    Ok(Request {
        id,
        operation,
        vault_id,
        memo_id,
        password,
        payload,
    })
}

pub fn write_request(mut writer: impl Write, request: &Request) -> io::Result<()> {
    if request.id == 0
        || request.vault_id == [0; 16]
        || !(1..=MAX_PASSWORD_BYTES).contains(&request.password.len())
        || request.payload.len() > MAX_PAYLOAD_BYTES
    {
        return Err(invalid());
    }
    writer.write_all(REQUEST_MAGIC)?;
    writer.write_all(&request.id.to_le_bytes())?;
    writer.write_all(&[match request.operation {
        Operation::Seal => 1,
        Operation::Open => 2,
    }])?;
    writer.write_all(&request.vault_id)?;
    writer.write_all(&request.memo_id.to_le_bytes())?;
    writer.write_all(&(request.password.len() as u16).to_le_bytes())?;
    writer.write_all(&(request.payload.len() as u32).to_le_bytes())?;
    writer.write_all(&request.password)?;
    writer.write_all(&request.payload)
}

pub fn read_response(mut reader: impl Read) -> io::Result<Response> {
    if &read_array::<8>(&mut reader)? != RESPONSE_MAGIC {
        return Err(invalid());
    }
    let id = u64::from_le_bytes(read_array(&mut reader)?);
    let status = match read_array::<1>(&mut reader)?[0] {
        0 => Status::Success,
        1 => Status::Rejected,
        2 => Status::InvalidRequest,
        3 => Status::Unavailable,
        _ => return Err(invalid()),
    };
    let len = u32::from_le_bytes(read_array(&mut reader)?) as usize;
    if len > MAX_PAYLOAD_BYTES
        || (status != Status::Success && len != 0)
        || ((id == 0) != (status == Status::InvalidRequest))
    {
        return Err(invalid());
    }
    let mut payload = Zeroizing::new(vec![0; len]);
    reader.read_exact(&mut payload)?;
    end_of_frame(&mut reader)?;
    Ok(Response {
        id,
        status,
        payload,
    })
}

pub fn write_response(mut writer: impl Write, response: &Response) -> io::Result<()> {
    if response.payload.len() > MAX_PAYLOAD_BYTES
        || (response.status != Status::Success && !response.payload.is_empty())
        || ((response.id == 0) != (response.status == Status::InvalidRequest))
    {
        return Err(invalid());
    }
    writer.write_all(RESPONSE_MAGIC)?;
    writer.write_all(&response.id.to_le_bytes())?;
    writer.write_all(&[response.status as u8])?;
    writer.write_all(&(response.payload.len() as u32).to_le_bytes())?;
    writer.write_all(&response.payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            id: 42,
            operation: Operation::Seal,
            vault_id: [3; 16],
            memo_id: 7,
            password: Zeroizing::new(b"test password".to_vec()),
            payload: Zeroizing::new("日本語のメモ".as_bytes().to_vec()),
        }
    }

    #[test]
    fn preserves_request_identity_scope_and_unicode_bytes() {
        let mut bytes = Zeroizing::new(Vec::new());
        write_request(&mut *bytes, &request()).unwrap();
        let decoded = read_request(bytes.as_slice()).unwrap();
        assert_eq!(decoded.id, 42);
        assert_eq!(decoded.operation, Operation::Seal);
        assert_eq!(decoded.vault_id, [3; 16]);
        assert_eq!(decoded.memo_id, 7);
        assert_eq!(*decoded.password, b"test password");
        assert_eq!(*decoded.payload, "日本語のメモ".as_bytes());
    }

    #[test]
    fn truncated_trailing_unknown_and_huge_frames_fail_before_crypto() {
        let mut bytes = Zeroizing::new(Vec::new());
        write_request(&mut *bytes, &request()).unwrap();
        for end in 0..bytes.len() {
            assert!(read_request(&bytes[..end]).is_err());
        }
        let mut bad = bytes.clone();
        bad.push(0);
        assert!(read_request(bad.as_slice()).is_err());
        bad = bytes.clone();
        bad[16] = 99;
        assert!(read_request(bad.as_slice()).is_err());
        bad = bytes.clone();
        bad[41..43].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(read_request(bad.as_slice()).is_err());
        bad = bytes.clone();
        bad[43..47].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_request(bad.as_slice()).is_err());
    }

    #[test]
    fn failed_response_cannot_include_plaintext() {
        let mut response = Response {
            id: 42,
            status: Status::Rejected,
            payload: Zeroizing::new(b"secret".to_vec()),
        };
        assert!(write_response(Vec::new(), &response).is_err());
        response.payload.clear();
        let mut bytes = Vec::new();
        write_response(&mut bytes, &response).unwrap();
        let decoded = read_response(bytes.as_slice()).unwrap();
        assert_eq!(decoded.id, 42);
        assert_eq!(decoded.status, Status::Rejected);
        assert!(decoded.payload.is_empty());
        bytes[17..21].copy_from_slice(&6u32.to_le_bytes());
        bytes.extend_from_slice(b"secret");
        assert!(read_response(bytes.as_slice()).is_err());
    }

    #[test]
    fn uncorrelated_success_and_mislabelled_invalid_response_are_rejected() {
        let mut bytes = Vec::new();
        write_response(
            &mut bytes,
            &Response {
                id: 42,
                status: Status::Success,
                payload: Zeroizing::new(Vec::new()),
            },
        )
        .unwrap();
        bytes[8..16].fill(0);
        assert!(read_response(bytes.as_slice()).is_err());
        assert!(write_response(
            Vec::new(),
            &Response {
                id: 42,
                status: Status::InvalidRequest,
                payload: Zeroizing::new(Vec::new())
            }
        )
        .is_err());
    }
}
