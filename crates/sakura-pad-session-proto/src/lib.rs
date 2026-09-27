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
/// Maximum v3 overhead uses the maximum supported credential ID length.
pub const V3_ENVELOPE_OVERHEAD: usize = 1252;
pub const V3_MIN_ENVELOPE_OVERHEAD: usize = 229;
pub const MAX_ENVELOPE_BYTES: usize = MAX_PLAINTEXT_BYTES + V3_ENVELOPE_OVERHEAD;
const RECOVERY_KEY_BYTES: usize = 77;
const CREATED_HEADER: usize = 8 + 4;
const CREATED_MAGIC: &[u8; 8] = b"SKRCR001";
const V3_AUTH_MAGIC: &[u8; 8] = b"SKR3AUTH";
const CHANGE_PASSWORD_V2_MAGIC: &[u8; 8] = b"SKRCPW01";
const CHANGE_PASSWORD_V2_HEADER: usize = 8 + 2;
const V3_PRF_BYTES: usize = 32;
const V3_CREDENTIAL_ID_MAX: usize = 1024;
const V3_AUTH_HEADER: usize = 8 + 2 + V3_PRF_BYTES;
const V3_AUTH_OVERHEAD: usize = V3_AUTH_HEADER + V3_CREDENTIAL_ID_MAX;
/// Includes the length-delimited envelope and the one-time display key.
pub const MAX_PAYLOAD_BYTES: usize = MAX_ENVELOPE_BYTES + V3_AUTH_OVERHEAD;
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
    CreatePrfV3 = 10,
    CreatePasswordAndPrfV3 = 11,
    UnlockPrfV3 = 12,
    UnlockPasswordAndPrfV3 = 13,
    UnlockRecoveryV3 = 14,
    /// Replace a whole-Pad v2 password, retaining its recovery-key route.
    ChangePasswordV2 = 15,
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
    /// Used by Create, Unlock, and ChangePasswordV2. Other operations set zero.
    pub vault_id: [u8; 16],
    /// Zero denotes the Pad vault; nonzero denotes an individual memo.
    pub memo_id: u64,
    pub password: SecretBytes,
    /// V3 payload prefix: `SKR3AUTH`, u16 credential ID length, credential
    /// ID, and exactly 32 PRF bytes. The suffix is plaintext (create) or an
    /// envelope (unlock). ChangePasswordV2 uses an independent versioned
    /// new-password/envelope payload; its old password is this request's
    /// separate password field.
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
        Operation::ChangePasswordV2 => {
            let changed = parse_change_password_v2_payload(&request.payload)?;
            if request.vault_id == [0; 16]
                || request.memo_id != 0
                || !(1..=MAX_PASSWORD_BYTES).contains(&request.password.len())
                || !(V2_ENVELOPE_OVERHEAD..=MAX_ENVELOPE_BYTES).contains(&changed.envelope.len())
                || !changed.envelope.starts_with(b"SKRPENV2")
            {
                return Err(invalid());
            }
        }
        Operation::CreatePrfV3 | Operation::CreatePasswordAndPrfV3 => {
            let with_password = request.operation == Operation::CreatePasswordAndPrfV3;
            let Ok(auth) = parse_v3_auth_payload(&request.payload) else {
                return Err(invalid());
            };
            if request.vault_id == [0; 16]
                || (with_password && !(1..=MAX_PASSWORD_BYTES).contains(&request.password.len()))
                || (!with_password && !request.password.is_empty())
                || auth.data.len() > MAX_PLAINTEXT_BYTES
            {
                return Err(invalid());
            }
        }
        Operation::UnlockPrfV3 | Operation::UnlockPasswordAndPrfV3 => {
            let with_password = request.operation == Operation::UnlockPasswordAndPrfV3;
            let Ok(auth) = parse_v3_auth_payload(&request.payload) else {
                return Err(invalid());
            };
            if request.vault_id == [0; 16]
                || (with_password && !(1..=MAX_PASSWORD_BYTES).contains(&request.password.len()))
                || (!with_password && !request.password.is_empty())
                || !(V3_MIN_ENVELOPE_OVERHEAD..=MAX_ENVELOPE_BYTES).contains(&auth.data.len())
            {
                return Err(invalid());
            }
        }
        Operation::UnlockRecoveryV3 => {
            if request.vault_id == [0; 16]
                || request.password.len() != RECOVERY_KEY_BYTES
                || !(V3_MIN_ENVELOPE_OVERHEAD..=MAX_ENVELOPE_BYTES).contains(&request.payload.len())
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
        10 => Operation::CreatePrfV3,
        11 => Operation::CreatePasswordAndPrfV3,
        12 => Operation::UnlockPrfV3,
        13 => Operation::UnlockPasswordAndPrfV3,
        14 => Operation::UnlockRecoveryV3,
        15 => Operation::ChangePasswordV2,
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

/// A versioned request body for whole-Pad v2 password replacement.
/// The old password remains in `Request.password`; this body contains the
/// new password and the complete current envelope. Neither is persisted by
/// this protocol. A successful response contains only the new v2 envelope.
#[allow(missing_debug_implementations)]
pub struct ChangePasswordV2Payload<'a> {
    pub new_password: &'a [u8],
    pub envelope: &'a [u8],
}

pub fn encode_change_password_v2_payload(
    new_password: &[u8],
    envelope: &[u8],
) -> io::Result<SecretBytes> {
    if !(1..=MAX_PASSWORD_BYTES).contains(&new_password.len())
        || !(V2_ENVELOPE_OVERHEAD..=MAX_ENVELOPE_BYTES).contains(&envelope.len())
        || !envelope.starts_with(b"SKRPENV2")
    {
        return Err(invalid());
    }
    let mut payload = SecretBytes::new(Vec::with_capacity(
        CHANGE_PASSWORD_V2_HEADER + new_password.len() + envelope.len(),
    ));
    payload.extend_from_slice(CHANGE_PASSWORD_V2_MAGIC);
    payload.extend_from_slice(&(new_password.len() as u16).to_le_bytes());
    payload.extend_from_slice(new_password);
    payload.extend_from_slice(envelope);
    Ok(payload)
}

pub fn parse_change_password_v2_payload(payload: &[u8]) -> io::Result<ChangePasswordV2Payload<'_>> {
    if payload.len() < CHANGE_PASSWORD_V2_HEADER + 1 + V2_ENVELOPE_OVERHEAD
        || payload.len() > MAX_PAYLOAD_BYTES
        || !payload.starts_with(CHANGE_PASSWORD_V2_MAGIC)
    {
        return Err(invalid());
    }
    let password_len =
        u16::from_le_bytes(payload[8..10].try_into().map_err(|_| invalid())?) as usize;
    if !(1..=MAX_PASSWORD_BYTES).contains(&password_len) {
        return Err(invalid());
    }
    let envelope = payload
        .get(CHANGE_PASSWORD_V2_HEADER + password_len..)
        .ok_or_else(invalid)?;
    if !(V2_ENVELOPE_OVERHEAD..=MAX_ENVELOPE_BYTES).contains(&envelope.len())
        || !envelope.starts_with(b"SKRPENV2")
    {
        return Err(invalid());
    }
    Ok(ChangePasswordV2Payload {
        new_password: &payload[CHANGE_PASSWORD_V2_HEADER..CHANGE_PASSWORD_V2_HEADER + password_len],
        envelope,
    })
}

/// Borrowed v3 authenticator bundle. The containing request payload is a
/// `Zeroizing` frame, so this view never owns or copies the PRF result.
#[allow(missing_debug_implementations)]
pub struct V3AuthPayload<'a> {
    pub credential_id: &'a [u8],
    pub prf: &'a [u8; V3_PRF_BYTES],
    pub data: &'a [u8],
}

pub fn encode_v3_auth_payload(
    credential_id: &[u8],
    prf: &[u8; V3_PRF_BYTES],
    data: &[u8],
) -> io::Result<SecretBytes> {
    if credential_id.is_empty() || credential_id.len() > V3_CREDENTIAL_ID_MAX {
        return Err(invalid());
    }
    let mut payload = SecretBytes::new(Vec::with_capacity(
        V3_AUTH_HEADER + credential_id.len() + data.len(),
    ));
    payload.extend_from_slice(V3_AUTH_MAGIC);
    payload.extend_from_slice(&(credential_id.len() as u16).to_le_bytes());
    payload.extend_from_slice(credential_id);
    payload.extend_from_slice(prf);
    payload.extend_from_slice(data);
    Ok(payload)
}

pub fn parse_v3_auth_payload(payload: &[u8]) -> io::Result<V3AuthPayload<'_>> {
    if payload.len() < V3_AUTH_HEADER || !payload.starts_with(V3_AUTH_MAGIC) {
        return Err(invalid());
    }
    let credential_len =
        u16::from_le_bytes(payload[8..10].try_into().map_err(|_| invalid())?) as usize;
    if credential_len == 0 || credential_len > V3_CREDENTIAL_ID_MAX {
        return Err(invalid());
    }
    let prf_start = 10 + credential_len;
    let data_start = prf_start + V3_PRF_BYTES;
    if data_start > payload.len() || payload.len() - data_start > MAX_ENVELOPE_BYTES {
        return Err(invalid());
    }
    let prf = payload[prf_start..data_start]
        .try_into()
        .map_err(|_| invalid())?;
    Ok(V3AuthPayload {
        credential_id: &payload[10..prf_start],
        prf,
        data: &payload[data_start..],
    })
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
    let minimum = if envelope.starts_with(b"SKRPENV3") {
        V3_MIN_ENVELOPE_OVERHEAD
    } else {
        V2_ENVELOPE_OVERHEAD
    };
    if !(minimum..=MAX_ENVELOPE_BYTES).contains(&envelope.len())
        || !(envelope.starts_with(b"SKRPENV2") || envelope.starts_with(b"SKRPENV3"))
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
    let magic = &payload[CREATED_HEADER..CREATED_HEADER + 8];
    let minimum = if magic == b"SKRPENV3" {
        V3_MIN_ENVELOPE_OVERHEAD
    } else if magic == b"SKRPENV2" {
        V2_ENVELOPE_OVERHEAD
    } else {
        return Err(invalid());
    };
    if !(minimum..=MAX_ENVELOPE_BYTES).contains(&envelope_len)
        || payload.len() != CREATED_HEADER + envelope_len + RECOVERY_KEY_BYTES
    {
        return Err(invalid());
    }
    let envelope = &payload[CREATED_HEADER..CREATED_HEADER + envelope_len];
    let key_bytes = &payload[CREATED_HEADER + envelope_len..];
    if !valid_recovery_key(key_bytes) {
        return Err(invalid());
    }
    Ok(CreatedRecovery {
        envelope,
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

    #[test]
    fn change_password_v2_body_is_versioned_bounded_and_whole_pad_only() {
        let mut envelope = vec![0; V2_ENVELOPE_OVERHEAD];
        envelope[..8].copy_from_slice(b"SKRPENV2");
        let mut body = encode_change_password_v2_payload(b"new secret", &envelope).unwrap();
        let parsed = parse_change_password_v2_payload(&body).unwrap();
        assert_eq!(parsed.new_password, b"new secret");
        assert_eq!(parsed.envelope, envelope);
        let mut req = request();
        req.operation = Operation::ChangePasswordV2;
        req.vault_id = *b"session-vault-01";
        req.password = SecretBytes::new(b"old secret".to_vec());
        req.payload = body.clone();
        let mut frame = Vec::new();
        write_request(&mut frame, &req).unwrap();
        assert_eq!(
            read_request(frame.as_slice()).unwrap().unwrap().operation,
            Operation::ChangePasswordV2
        );
        req.memo_id = 7;
        assert!(write_request(Vec::new(), &req).is_err());
        req.memo_id = 0;
        req.password.clear();
        assert!(write_request(Vec::new(), &req).is_err());
        body[0] ^= 1;
        assert!(parse_change_password_v2_payload(&body).is_err());
        assert!(encode_change_password_v2_payload(b"", &envelope).is_err());
        assert!(
            encode_change_password_v2_payload(&vec![0; MAX_PASSWORD_BYTES + 1], &envelope).is_err()
        );
        envelope[..8].copy_from_slice(b"SKRPENV3");
        assert!(encode_change_password_v2_payload(b"new", &envelope).is_err());
    }

    #[test]
    fn v3_auth_payload_round_trips_without_mixing_password_and_prf() {
        let prf = [0xA7; V3_PRF_BYTES];
        let data = b"opaque envelope bytes";
        let payload = encode_v3_auth_payload(b"credential", &prf, data).unwrap();
        let parsed = parse_v3_auth_payload(&payload).unwrap();
        assert_eq!(parsed.credential_id, b"credential");
        assert_eq!(parsed.prf, &prf);
        assert_eq!(parsed.data, data);
        let mut req = request();
        req.operation = Operation::UnlockPrfV3;
        req.vault_id = *b"session-vault-01";
        req.payload =
            encode_v3_auth_payload(b"credential", &prf, &vec![0; V3_ENVELOPE_OVERHEAD]).unwrap();
        write_request(Vec::new(), &req).unwrap();
        req.password = SecretBytes::new(b"must stay separate".to_vec());
        assert!(write_request(Vec::new(), &req).is_err());
    }

    #[test]
    fn v3_auth_payload_rejects_malformed_lengths_and_credentials() {
        assert!(encode_v3_auth_payload(&[], &[0; V3_PRF_BYTES], b"x").is_err());
        assert!(encode_v3_auth_payload(
            &vec![0; V3_CREDENTIAL_ID_MAX + 1],
            &[0; V3_PRF_BYTES],
            b"x"
        )
        .is_err());
        let mut payload = encode_v3_auth_payload(b"id", &[0; V3_PRF_BYTES], b"data").unwrap();
        payload[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(parse_v3_auth_payload(&payload).is_err());
    }

    #[test]
    fn v3_create_payload_is_bounded_and_created_recovery_accepts_v3() {
        let auth = encode_v3_auth_payload(b"id", &[1; V3_PRF_BYTES], b"").unwrap();
        let mut req = request();
        req.operation = Operation::CreatePrfV3;
        req.vault_id = *b"session-vault-01";
        req.payload = auth;
        write_request(Vec::new(), &req).unwrap();

        let mut envelope = vec![0; V3_MIN_ENVELOPE_OVERHEAD];
        envelope[..8].copy_from_slice(b"SKRPENV3");
        let key = "SPRK1-00000000-00000000-00000000-00000000-00000000-00000000-00000000-00000000";
        let created = encode_created_recovery(&envelope, key).unwrap();
        let parsed = parse_created_recovery(&created).unwrap();
        assert_eq!(parsed.envelope, envelope);
        assert_eq!(parsed.recovery_key, key);
    }
}
