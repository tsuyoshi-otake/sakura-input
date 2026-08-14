use std::io::{self, Read, Write};

pub const MAX_FRAME: usize = 32 * 1024;
pub const MAX_CANDIDATES: usize = 6;
pub const MAX_CANDIDATE_BYTES: usize = 3 * 1024;
const REQUEST_MAGIC: u32 = 0x524e_4b53;
const RESPONSE_MAGIC: u32 = 0x534e_4b53;
const VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub fingerprint: u64,
    pub local_cost: i32,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub id: u64,
    pub candidates: Vec<Candidate>,
}

fn take<const N: usize>(b: &[u8], p: &mut usize) -> Result<[u8; N], &'static str> {
    let end = p.checked_add(N).ok_or("frame overflow")?;
    let value = b
        .get(*p..end)
        .ok_or("truncated frame")?
        .try_into()
        .map_err(|_| "truncated frame")?;
    *p = end;
    Ok(value)
}
fn u16_at(b: &[u8], p: &mut usize) -> Result<u16, &'static str> {
    Ok(u16::from_le_bytes(take(b, p)?))
}
fn u32_at(b: &[u8], p: &mut usize) -> Result<u32, &'static str> {
    Ok(u32::from_le_bytes(take(b, p)?))
}
fn u64_at(b: &[u8], p: &mut usize) -> Result<u64, &'static str> {
    Ok(u64::from_le_bytes(take(b, p)?))
}

pub(crate) fn decode(payload: &[u8]) -> Result<Request, &'static str> {
    let mut p = 0;
    if u32_at(payload, &mut p)? != REQUEST_MAGIC
        || u16_at(payload, &mut p)? != VERSION
        || u16_at(payload, &mut p)? != 0
    {
        return Err("invalid request header");
    }
    let id = u64_at(payload, &mut p)?;
    let context = u32_at(payload, &mut p)? as usize;
    let count = u32_at(payload, &mut p)? as usize;
    if context > 1024 || count == 0 || count > MAX_CANDIDATES {
        return Err("invalid request bounds");
    }
    p = p.checked_add(context).ok_or("frame overflow")?;
    if p > payload.len() {
        return Err("truncated context");
    }
    let mut candidates = Vec::with_capacity(count);
    for _ in 0..count {
        let fingerprint = u64_at(payload, &mut p)?;
        let local_cost = i32::from_le_bytes(take(payload, &mut p)?);
        let n = u32_at(payload, &mut p)? as usize;
        if n == 0 || n > MAX_CANDIDATE_BYTES {
            return Err("invalid candidate length");
        }
        let raw = payload
            .get(p..p.checked_add(n).ok_or("frame overflow")?)
            .ok_or("truncated candidate")?;
        p += n;
        let text = std::str::from_utf8(raw)
            .map_err(|_| "candidate is not UTF-8")?
            .to_owned();
        candidates.push(Candidate {
            fingerprint,
            local_cost,
            text,
        });
    }
    if p != payload.len() {
        return Err("trailing request bytes");
    }
    Ok(Request { id, candidates })
}
pub(crate) fn read(input: &mut impl Read) -> io::Result<Option<Request>> {
    let mut len = [0; 4];
    let first = input.read(&mut len)?;
    if first == 0 {
        return Ok(None);
    }
    if first < len.len() {
        input.read_exact(&mut len[first..])?;
    }
    let n = u32::from_le_bytes(len) as usize;
    if n == 0 || n > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request length",
        ));
    }
    let mut p = vec![0; n];
    input.read_exact(&mut p)?;
    decode(&p)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
pub(crate) fn write_failure(out: &mut impl Write, id: u64, tier: u16) -> io::Result<()> {
    let mut p = Vec::new();
    p.extend_from_slice(&RESPONSE_MAGIC.to_le_bytes());
    p.extend_from_slice(&VERSION.to_le_bytes());
    p.extend_from_slice(&2u16.to_le_bytes());
    p.extend_from_slice(&id.to_le_bytes());
    p.extend_from_slice(&tier.to_le_bytes());
    p.extend_from_slice(&0u16.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    out.write_all(&(p.len() as u32).to_le_bytes())?;
    out.write_all(&p)?;
    out.flush()
}
pub(crate) fn write_success(
    out: &mut impl Write,
    id: u64,
    tier: u16,
    scores: &[(u64, f32)],
) -> io::Result<()> {
    if scores.len() > MAX_CANDIDATES || scores.iter().any(|(_, x)| !x.is_finite()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "non-finite score",
        ));
    }
    let mut p = Vec::new();
    p.extend_from_slice(&RESPONSE_MAGIC.to_le_bytes());
    p.extend_from_slice(&VERSION.to_le_bytes());
    p.extend_from_slice(&0u16.to_le_bytes());
    p.extend_from_slice(&id.to_le_bytes());
    p.extend_from_slice(&tier.to_le_bytes());
    p.extend_from_slice(&0u16.to_le_bytes());
    p.extend_from_slice(&(scores.len() as u32).to_le_bytes());
    for (f, x) in scores {
        p.extend_from_slice(&f.to_le_bytes());
        p.extend_from_slice(&x.to_bits().to_le_bytes())
    }
    out.write_all(&(p.len() as u32).to_le_bytes())?;
    out.write_all(&p)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wire() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&REQUEST_MAGIC.to_le_bytes());
        p.extend_from_slice(&1u16.to_le_bytes());
        p.extend_from_slice(&0u16.to_le_bytes());
        p.extend_from_slice(&7u64.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&9u64.to_le_bytes());
        p.extend_from_slice(&1i32.to_le_bytes());
        p.extend_from_slice(&1u32.to_le_bytes());
        p.push(b'A');
        p
    }
    #[test]
    fn valid_wire() {
        let request = decode(&wire()).unwrap();
        assert_eq!(request.id, 7);
        assert_eq!(request.candidates[0].local_cost, 1);
    }
    #[test]
    fn malformed_is_rejected() {
        assert!(decode(&wire()[..8]).is_err());
        let mut x = wire();
        x.push(0);
        assert!(decode(&x).is_err());
    }
    #[test]
    fn bounds_are_rejected() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&((MAX_FRAME + 1) as u32).to_le_bytes());
        assert!(read(&mut stream.as_slice()).is_err());
    }

    #[test]
    fn truncated_length_prefix_is_not_clean_eof() {
        assert!(read(&mut [1u8, 0].as_slice()).is_err());
    }
    #[test]
    fn success_rejects_nonfinite() {
        assert!(write_success(&mut Vec::new(), 7, 2, &[(1, f32::NAN)]).is_err());
        assert!(write_success(&mut Vec::new(), 7, 2, &[(1, f32::INFINITY)]).is_err())
    }

    #[test]
    fn success_wire_round_trips_exact_fingerprints_and_float_bits() {
        let mut bytes = Vec::new();
        write_success(&mut bytes, 7, 2, &[(9, -1.25), (10, 0.5)]).unwrap();
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(length, bytes.len() - 4);
        let payload = &bytes[4..];
        let mut cursor = 0;
        assert_eq!(u32_at(payload, &mut cursor).unwrap(), RESPONSE_MAGIC);
        assert_eq!(u16_at(payload, &mut cursor).unwrap(), VERSION);
        assert_eq!(u16_at(payload, &mut cursor).unwrap(), 0);
        assert_eq!(u64_at(payload, &mut cursor).unwrap(), 7);
        assert_eq!(u16_at(payload, &mut cursor).unwrap(), 2);
        assert_eq!(u16_at(payload, &mut cursor).unwrap(), 0);
        assert_eq!(u32_at(payload, &mut cursor).unwrap(), 2);
        assert_eq!(u64_at(payload, &mut cursor).unwrap(), 9);
        assert_eq!(f32::from_bits(u32_at(payload, &mut cursor).unwrap()), -1.25);
        assert_eq!(u64_at(payload, &mut cursor).unwrap(), 10);
        assert_eq!(f32::from_bits(u32_at(payload, &mut cursor).unwrap()), 0.5);
        assert_eq!(cursor, payload.len());
    }

    #[test]
    fn generated_arbitrary_frames_never_panic_or_escape_bounds() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for length in 0..=512usize {
            for _case in 0..8 {
                let mut bytes = vec![0u8; length];
                for byte in &mut bytes {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    *byte = state as u8;
                }
                if let Ok(request) = decode(&bytes) {
                    assert!(!request.candidates.is_empty());
                    assert!(request.candidates.len() <= MAX_CANDIDATES);
                    assert!(request
                        .candidates
                        .iter()
                        .all(|candidate| !candidate.text.is_empty()
                            && candidate.text.len() <= MAX_CANDIDATE_BYTES));
                }
            }
        }
    }
}
