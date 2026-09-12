use std::io::{self, Read, Write};

use sakura_rerank_proto::{Frame, Limits, Score};

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

pub(crate) fn read(input: &mut impl Read) -> io::Result<Option<Request>> {
    match sakura_rerank_proto::read_frame(input)? {
        None => Ok(None),
        Some(Frame::Request { id, candidates, .. }) => Ok(Some(Request {
            id,
            candidates: candidates
                .into_iter()
                .map(|candidate| Candidate {
                    fingerprint: candidate.fingerprint,
                    local_cost: candidate.local_cost,
                    text: candidate.text.into_owned(),
                })
                .collect(),
        })),
        Some(Frame::Response { .. }) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected rerank request",
        )),
    }
}

pub(crate) fn decode(payload: &[u8]) -> io::Result<Request> {
    if payload.is_empty() || payload.len() > Limits::MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request length",
        ));
    }
    let mut framed = Vec::with_capacity(payload.len() + 4);
    framed.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    framed.extend_from_slice(payload);
    read(&mut framed.as_slice())?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing request"))
}

pub(crate) fn write_failure(out: &mut impl Write, id: u64) -> io::Result<()> {
    sakura_rerank_proto::write_frame(
        out,
        &Frame::Response {
            id,
            status: 2,
            legacy_tier_reserved: 0,
            scores: Vec::new(),
        },
    )
}

pub(crate) fn write_success(
    out: &mut impl Write,
    id: u64,
    scores: &[(u64, f32)],
) -> io::Result<()> {
    if scores.len() > Limits::MAX_CANDIDATES || scores.iter().any(|(_, value)| !value.is_finite()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "non-finite score",
        ));
    }
    let scores = scores
        .iter()
        .map(|&(fingerprint, value)| Score { fingerprint, value })
        .collect();
    sakura_rerank_proto::write_frame(
        out,
        &Frame::Response {
            id,
            status: 0,
            legacy_tier_reserved: 0,
            scores,
        },
    )
}

#[cfg(test)]
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
#[cfg(test)]
fn u16_at(b: &[u8], p: &mut usize) -> Result<u16, &'static str> {
    Ok(u16::from_le_bytes(take(b, p)?))
}
#[cfg(test)]
fn u32_at(b: &[u8], p: &mut usize) -> Result<u32, &'static str> {
    Ok(u32::from_le_bytes(take(b, p)?))
}
#[cfg(test)]
fn u64_at(b: &[u8], p: &mut usize) -> Result<u64, &'static str> {
    Ok(u64::from_le_bytes(take(b, p)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wire() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&Limits::REQUEST_MAGIC.to_le_bytes());
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
        stream.extend_from_slice(&((Limits::MAX_FRAME + 1) as u32).to_le_bytes());
        assert!(read(&mut stream.as_slice()).is_err());
    }

    #[test]
    fn truncated_length_prefix_is_not_clean_eof() {
        assert!(read(&mut [1u8, 0].as_slice()).is_err());
    }
    #[test]
    fn success_rejects_nonfinite() {
        assert!(write_success(&mut Vec::new(), 7, &[(1, f32::NAN)]).is_err());
        assert!(write_success(&mut Vec::new(), 7, &[(1, f32::INFINITY)]).is_err())
    }

    #[test]
    fn success_wire_round_trips_exact_fingerprints_and_float_bits() {
        let mut bytes = Vec::new();
        write_success(&mut bytes, 7, &[(9, -1.25), (10, 0.5)]).unwrap();
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(length, bytes.len() - 4);
        let payload = &bytes[4..];
        let mut cursor = 0;
        assert_eq!(
            u32_at(payload, &mut cursor).unwrap(),
            Limits::RESPONSE_MAGIC
        );
        assert_eq!(u16_at(payload, &mut cursor).unwrap(), Limits::VERSION);
        assert_eq!(u16_at(payload, &mut cursor).unwrap(), 0);
        assert_eq!(u64_at(payload, &mut cursor).unwrap(), 7);
        assert_eq!(u16_at(payload, &mut cursor).unwrap(), 0);
        assert_eq!(u16_at(payload, &mut cursor).unwrap(), 0);
        assert_eq!(u32_at(payload, &mut cursor).unwrap(), 2);
        assert_eq!(u64_at(payload, &mut cursor).unwrap(), 9);
        assert_eq!(f32::from_bits(u32_at(payload, &mut cursor).unwrap()), -1.25);
        assert_eq!(u64_at(payload, &mut cursor).unwrap(), 10);
        assert_eq!(f32::from_bits(u32_at(payload, &mut cursor).unwrap()), 0.5);
        assert_eq!(cursor, payload.len());
    }

    #[test]
    fn protocol_v1_success_layout_keeps_both_reserved_slots_and_score_offsets() {
        let mut bytes = Vec::new();
        write_success(&mut bytes, 7, &[(9, -1.25), (10, 0.5)]).unwrap();
        #[rustfmt::skip]
        let expected = [
            0x30, 0x00, 0x00, 0x00, // payload length: 48
            0x53, 0x4b, 0x4e, 0x53, // response magic
            0x01, 0x00,             // protocol version
            0x00, 0x00,             // success status
            0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // request id
            0x00, 0x00,             // legacy tier slot, reserved as zero
            0x00, 0x00,             // second reserved slot
            0x02, 0x00, 0x00, 0x00, // score count
            0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // fingerprint 9
            0x00, 0x00, 0xa0, 0xbf, // -1.25f32
            0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // fingerprint 10
            0x00, 0x00, 0x00, 0x3f, // 0.5f32
        ];
        assert_eq!(bytes, expected);
    }

    #[test]
    fn protocol_v1_failure_layout_keeps_both_reserved_slots_and_count_offset() {
        let mut bytes = Vec::new();
        write_failure(&mut bytes, 7).unwrap();
        #[rustfmt::skip]
        let expected = [
            0x18, 0x00, 0x00, 0x00, // payload length: 24
            0x53, 0x4b, 0x4e, 0x53, // response magic
            0x01, 0x00,             // protocol version
            0x02, 0x00,             // failure status
            0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // request id
            0x00, 0x00,             // legacy tier slot, reserved as zero
            0x00, 0x00,             // second reserved slot
            0x00, 0x00, 0x00, 0x00, // score count
        ];
        assert_eq!(bytes, expected);
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
                    assert!(request.candidates.len() <= Limits::MAX_CANDIDATES);
                    assert!(request
                        .candidates
                        .iter()
                        .all(|candidate| !candidate.text.is_empty()
                            && candidate.text.len() <= Limits::MAX_CANDIDATE_BYTES));
                }
            }
        }
    }
}
