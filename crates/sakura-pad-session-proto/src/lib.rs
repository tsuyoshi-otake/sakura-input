//! Bounded, length-prefixed wire protocol for one isolated Pad crypto session.
//! All integers are little-endian. A framing failure receives one invalid response
//! and terminates the stream; operation failures are terminal responses per request.

use std::io::{self, Read, Write};
use zeroize::Zeroizing;

pub const MAX_PLAINTEXT_BYTES: usize = 8 * 1024 * 1024;

const REQUEST_MAGIC: &[u8; 8] = b"SKRPSS01";
const RESPONSE_MAGIC: &[u8; 8] = b"SKRPSR01";
const REQUEST_HEADER: usize = 55;
const RESPONSE_HEADER: usize = 8 + 8 + 8 + 1 + 4;
const MAX_PASSWORD_BYTES: usize = 1024;
/// V2 overhead is a 102-byte header, two 48-byte wraps, and a 16-byte tag.
pub const V2_ENVELOPE_OVERHEAD: usize = 214;
pub const MAX_ENVELOPE_BYTES: usize = MAX_PLAINTEXT_BYTES + V2_ENVELOPE_OVERHEAD;
const RECOVERY_KEY_BYTES: usize = 77;
const CREATED_HEADER: usize = 8 + 4;
const CREATED_MAGIC: &[u8; 8] = b"SKRCR001";
/// Includes the length-delimited envelope and the one-time display key.
pub const MAX_PAYLOAD_BYTES: usize = MAX_ENVELOPE_BYTES + CREATED_HEADER + RECOVERY_KEY_BYTES;
const MAX_REQUEST_FRAME: usize = REQUEST_HEADER + MAX_PASSWORD_BYTES + MAX_PAYLOAD_BYTES;

/// Owned wire bytes whose initialized contents are overwritten on drop.
/// Pipe/OS/allocator copies are outside this guarantee.
pub type SecretBytes = Zeroizing<Vec<u8>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Operation {
    Unlock = 1,
    Reseal = 2,
    Lock = 3,
    Shutdown = 4,
    Verify = 5,
    Create = 6,
    CreateRecoverable = 7,
    UnlockPasswordV2 = 8,
    UnlockRecoveryV2 = 9,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Status {
    Success = 0,
    Rejected = 1,
    InvalidRequest = 2,
    Unavailable = 3,
    Locked = 4,
    Stale = 5,
}

#[allow(missing_debug_implementations)]
pub struct Request {
    pub id: u64,
    pub generation: u64,
    pub operation: Operation,
    /// Used by Create and Unlock variants. Other operations must set this to zero.
    pub vault_id: [u8; 16],
    /// Zero denotes the Pad vault; nonzero denotes an individual memo.
    pub memo_id: u64,
    pub password: SecretBytes,
    pub payload: SecretBytes,
}

#[allow(missing_debug_implementations)]
pub struct Response {
    pub id: u64,
    pub generation: u64,
    pub status: Status,
    pub payload: SecretBytes,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid Pad session frame")
}

fn validate_request(request: &Request) -> io::Result<()> {
    if request.id == 0 || request.generation == 0 || request.payload.len() > MAX_PAYLOAD_BYTES {
        return Err(invalid());
    }
    match request.operation {
        Operation::Unlock
        | Operation::Create
        | Operation::CreateRecoverable
        | Operation::UnlockPasswordV2 => {
            if request.vault_id == [0; 16]
                || !(1..=MAX_PASSWORD_BYTES).contains(&request.password.len())
                || (matches!(
                    request.operation,
                    Operation::Create | Operation::CreateRecoverable
                ) && request.payload.len() > MAX_PLAINTEXT_BYTES)
                || (matches!(
                    request.operation,
                    Operation::Unlock | Operation::UnlockPasswordV2
                ) && request.payload.len() > MAX_ENVELOPE_BYTES)
            {
                return Err(invalid());
            }
        }
        Operation::UnlockRecoveryV2 => {
            if request.vault_id == [0; 16]
                || request.password.len() != RECOVERY_KEY_BYTES
                || request.payload.len() > MAX_ENVELOPE_BYTES
            {
                return Err(invalid());
            }
        }
        Operation::Reseal | Operation::Verify => {
            if request.vault_id != [0; 16]
                || request.memo_id != 0
                || !request.password.is_empty()
                || (request.operation == Operation::Verify
                    && request.payload.len() > MAX_ENVELOPE_BYTES)
                || (request.operation == Operation::Reseal
                    && request.payload.len() > MAX_PLAINTEXT_BYTES)
            {
                return Err(invalid());
            }
        }
        Operation::Lock | Operation::Shutdown => {
            if request.vault_id != [0; 16]
                || request.memo_id != 0
                || !request.password.is_empty()
                || !request.payload.is_empty()
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

pub fn write_request(mut writer: impl Write, request: &Request) -> io::Result<()> {
    validate_request(request)?;
    let len = REQUEST_HEADER + request.password.len() + request.payload.len();
    writer.write_all(&(len as u32).to_le_bytes())?;
    writer.write_all(REQUEST_MAGIC)?;
    writer.write_all(&request.id.to_le_bytes())?;
    writer.write_all(&request.generation.to_le_bytes())?;
    writer.write_all(&[request.operation as u8])?;
    writer.write_all(&request.vault_id)?;
    writer.write_all(&request.memo_id.to_le_bytes())?;
    writer.write_all(&(request.password.len() as u16).to_le_bytes())?;
    writer.write_all(&(request.payload.len() as u32).to_le_bytes())?;
    writer.write_all(&request.password)?;
    writer.write_all(&request.payload)
}

/// None is a clean EOF at a frame boundary. A partial or oversized frame fails.
pub fn read_request(mut reader: impl Read) -> io::Result<Option<Request>> {
    let mut first = [0u8; 1];
    if reader.read(&mut first)? == 0 {
        return Ok(None);
    }
    let mut length = [0u8; 4];
    length[0] = first[0];
    reader.read_exact(&mut length[1..])?;
    let len = u32::from_le_bytes(length) as usize;
    if !(REQUEST_HEADER..=MAX_REQUEST_FRAME).contains(&len) {
        return Err(invalid());
    }
    let mut frame = SecretBytes::new(vec![0u8; len]);
    reader.read_exact(&mut frame)?;
    if &frame[..8] != REQUEST_MAGIC {
        return Err(invalid());
    }
    let id = u64::from_le_bytes(frame[8..16].try_into().map_err(|_| invalid())?);
    let generation = u64::from_le_bytes(frame[16..24].try_into().map_err(|_| invalid())?);
    let operation = match frame[24] {
        1 => Operation::Unlock,
        2 => Operation::Reseal,
        3 => Operation::Lock,
        4 => Operation::Shutdown,
        5 => Operation::Verify,
        6 => Operation::Create,
        7 => Operation::CreateRecoverable,
        8 => Operation::UnlockPasswordV2,
        9 => Operation::UnlockRecoveryV2,
        _ => return Err(invalid()),
    };
    let vault_id = frame[25..41].try_into().map_err(|_| invalid())?;
    let memo_id = u64::from_le_bytes(frame[41..49].try_into().map_err(|_| invalid())?);
    let password_len =
        u16::from_le_bytes(frame[49..51].try_into().map_err(|_| invalid())?) as usize;
    let payload_len = u32::from_le_bytes(frame[51..55].try_into().map_err(|_| invalid())?) as usize;
    if password_len > MAX_PASSWORD_BYTES
        || payload_len > MAX_PAYLOAD_BYTES
        || REQUEST_HEADER + password_len + payload_len != len
    {
        return Err(invalid());
    }
    let request = Request {
        id,
        generation,
        operation,
        vault_id,
        memo_id,
        password: SecretBytes::new(frame[REQUEST_HEADER..REQUEST_HEADER + password_len].to_vec()),
        payload: SecretBytes::new(frame[REQUEST_HEADER + password_len..].to_vec()),
    };
    validate_request(&request)?;
    Ok(Some(request))
}

/// Borrows the two fields from a successful CreateRecoverable response.
/// The caller owns the response and must display the key before discarding it.
#[allow(missing_debug_implementations)]
pub struct CreatedRecovery<'a> {
    pub envelope: &'a [u8],
    pub recovery_key: &'a str,
}

fn valid_recovery_key(bytes: &[u8]) -> bool {
    bytes.len() == RECOVERY_KEY_BYTES
        && bytes.starts_with(b"SPRK1-")
        && bytes[6..].iter().enumerate().all(|(index, byte)| {
            if index % 9 == 8 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'A'..=b'F').contains(byte)
            }
        })
}

/// Build the one-time creation payload: magic, u32 envelope length, envelope, key.
pub fn encode_created_recovery(envelope: &[u8], recovery_key: &str) -> io::Result<SecretBytes> {
    if !(V2_ENVELOPE_OVERHEAD..=MAX_ENVELOPE_BYTES).contains(&envelope.len())
        || !envelope.starts_with(b"SKRPENV2")
        || !valid_recovery_key(recovery_key.as_bytes())
    {
        return Err(invalid());
    }
    let mut payload = SecretBytes::new(Vec::with_capacity(
        CREATED_HEADER + envelope.len() + RECOVERY_KEY_BYTES,
    ));
    payload.extend_from_slice(CREATED_MAGIC);
    payload.extend_from_slice(&(envelope.len() as u32).to_le_bytes());
    payload.extend_from_slice(envelope);
    payload.extend_from_slice(recovery_key.as_bytes());
    Ok(payload)
}

/// Parse only the exact successful creation layout; reject truncation or trailing data.
pub fn parse_created_recovery(payload: &[u8]) -> io::Result<CreatedRecovery<'_>> {
    if payload.len() < CREATED_HEADER + V2_ENVELOPE_OVERHEAD + RECOVERY_KEY_BYTES
        || !payload.starts_with(CREATED_MAGIC)
    {
        return Err(invalid());
    }
    let envelope_len =
        u32::from_le_bytes(payload[8..12].try_into().map_err(|_| invalid())?) as usize;
    if !(V2_ENVELOPE_OVERHEAD..=MAX_ENVELOPE_BYTES).contains(&envelope_len)
        || payload.len() != CREATED_HEADER + envelope_len + RECOVERY_KEY_BYTES
        || !payload[CREATED_HEADER..].starts_with(b"SKRPENV2")
    {
        return Err(invalid());
    }
    let key_bytes = &payload[CREATED_HEADER + envelope_len..];
    if !valid_recovery_key(key_bytes) {
        return Err(invalid());
    }
    Ok(CreatedRecovery {
        envelope: &payload[CREATED_HEADER..CREATED_HEADER + envelope_len],
        recovery_key: std::str::from_utf8(key_bytes).map_err(|_| invalid())?,
    })
}

pub fn write_response(mut writer: impl Write, response: &Response) -> io::Result<()> {
    if response.payload.len() > MAX_PAYLOAD_BYTES
        || (response.status != Status::Success && !response.payload.is_empty())
        || ((response.id == 0 || response.generation == 0)
            != (response.id == 0
                && response.generation == 0
                && response.status == Status::InvalidRequest))
        || (response.status == Status::InvalidRequest && response.id != 0)
    {
        return Err(invalid());
    }
    let len = RESPONSE_HEADER + response.payload.len();
    writer.write_all(&(len as u32).to_le_bytes())?;
    writer.write_all(RESPONSE_MAGIC)?;
    writer.write_all(&response.id.to_le_bytes())?;
    writer.write_all(&response.generation.to_le_bytes())?;
    writer.write_all(&[response.status as u8])?;
    writer.write_all(&(response.payload.len() as u32).to_le_bytes())?;
    writer.write_all(&response.payload)
}

pub fn read_response(mut reader: impl Read) -> io::Result<Response> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let len = u32::from_le_bytes(length) as usize;
    if !(RESPONSE_HEADER..=RESPONSE_HEADER + MAX_PAYLOAD_BYTES).contains(&len) {
        return Err(invalid());
    }
    let mut frame = SecretBytes::new(vec![0u8; len]);
    reader.read_exact(&mut frame)?;
    if &frame[..8] != RESPONSE_MAGIC {
        return Err(invalid());
    }
    let id = u64::from_le_bytes(frame[8..16].try_into().map_err(|_| invalid())?);
    let generation = u64::from_le_bytes(frame[16..24].try_into().map_err(|_| invalid())?);
    let status = match frame[24] {
        0 => Status::Success,
        1 => Status::Rejected,
        2 => Status::InvalidRequest,
        3 => Status::Unavailable,
        4 => Status::Locked,
        5 => Status::Stale,
        _ => return Err(invalid()),
    };
    let payload_len = u32::from_le_bytes(frame[25..29].try_into().map_err(|_| invalid())?) as usize;
    if RESPONSE_HEADER + payload_len != len
        || (status != Status::Success && payload_len != 0)
        || ((id == 0 || generation == 0)
            != (id == 0 && generation == 0 && status == Status::InvalidRequest))
        || (status == Status::InvalidRequest && id != 0)
    {
        return Err(invalid());
    }
    Ok(Response {
        id,
        generation,
        status,
        payload: SecretBytes::new(frame[RESPONSE_HEADER..].to_vec()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        Request {
            id: 7,
            generation: 11,
            operation: Operation::Verify,
            vault_id: [0; 16],
            memo_id: 0,
            password: SecretBytes::new(Vec::new()),
            payload: SecretBytes::new(b"synthetic envelope".to_vec()),
        }
    }

    #[test]
    fn verify_round_trip_preserves_ids_and_payload() {
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request()).unwrap();
        let decoded = read_request(bytes.as_slice()).unwrap().unwrap();
        assert_eq!((decoded.id, decoded.generation), (7, 11));
        assert_eq!(decoded.operation, Operation::Verify);
        assert_eq!(&*decoded.payload, b"synthetic envelope");
    }

    #[test]
    fn frame_bounds_and_failure_payload_are_enforced() {
        assert!(read_request(u32::MAX.to_le_bytes().as_slice()).is_err());
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request()).unwrap();
        assert!(read_request(&bytes[..bytes.len() - 1]).is_err());
        let invalid = Response {
            id: 7,
            generation: 11,
            status: Status::Rejected,
            payload: SecretBytes::new(b"secret".to_vec()),
        };
        assert!(write_response(Vec::new(), &invalid).is_err());
    }

    #[test]
    fn created_recovery_fields_require_exact_lengths_and_canonical_key() {
        let envelope = vec![b'x'; V2_ENVELOPE_OVERHEAD];
        let mut envelope = envelope;
        envelope[..8].copy_from_slice(b"SKRPENV2");
        let key = "SPRK1-00000000-00000000-00000000-00000000-00000000-00000000-00000000-00000000";
        let payload = encode_created_recovery(&envelope, key).unwrap();
        let parsed = parse_created_recovery(&payload).unwrap();
        assert_eq!(parsed.envelope, envelope);
        assert_eq!(parsed.recovery_key, key);
        let mut trailing = payload.to_vec();
        trailing.push(0);
        assert!(parse_created_recovery(&trailing).is_err());
        let mut bad_length = payload.to_vec();
        bad_length[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_created_recovery(&bad_length).is_err());
        let mut bad_key = payload.to_vec();
        *bad_key.last_mut().unwrap() = b'g';
        assert!(parse_created_recovery(&bad_key).is_err());
    }

    #[test]
    fn recovery_operation_enforces_display_key_length_and_envelope_bound() {
        let mut request = request();
        request.operation = Operation::UnlockRecoveryV2;
        request.vault_id = *b"session-vault-01";
        request.password = SecretBytes::new(vec![b'0'; 76]);
        assert!(write_request(Vec::new(), &request).is_err());
        request.password.push(b'0');
        assert!(write_request(Vec::new(), &request).is_ok());
        request.payload = SecretBytes::new(vec![0; MAX_ENVELOPE_BYTES + 1]);
        assert!(write_request(Vec::new(), &request).is_err());
    }
}
