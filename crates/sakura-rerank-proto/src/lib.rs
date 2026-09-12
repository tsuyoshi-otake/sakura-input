//! Shared bounded Sakura reranker protocol v1.

use sakura_values::Fingerprint;
use std::borrow::Cow;
use std::io::{self, Read, Write};

#[derive(Debug)]
pub struct Limits;
impl Limits {
    pub const REQUEST_MAGIC: u32 = 0x524e_4b53;
    pub const RESPONSE_MAGIC: u32 = 0x534e_4b53;
    pub const VERSION: u16 = 1;
    pub const MAX_FRAME: usize = 32 * 1024;
    pub const MAX_CANDIDATES: usize = 6;
    pub const MAX_CANDIDATE_BYTES: usize = 3 * 1024;
    pub const MAX_CONTEXT_BYTES: usize = 1024;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub fingerprint: Fingerprint,
    pub local_cost: i32,
    pub text: Cow<'a, str>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    pub fingerprint: Fingerprint,
    pub value: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Frame<'a> {
    Request {
        id: u64,
        context: Cow<'a, [u8]>,
        candidates: Vec<Candidate<'a>>,
    },
    Response {
        id: u64,
        status: u16,
        legacy_tier_reserved: u16,
        scores: Vec<Score>,
    },
}

pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Frame<'static>>> {
    let mut length = [0; 4];
    let first = input.read(&mut length)?;
    if first == 0 {
        return Ok(None);
    }
    input.read_exact(&mut length[first..])?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > Limits::MAX_FRAME {
        return invalid_data("invalid frame length");
    }
    let mut payload = vec![0; length];
    input.read_exact(&mut payload)?;
    // A complete request frame with a truncated payload field was InvalidData
    // in the worker codec. Truncated transport reads above remain UnexpectedEof.
    let request = payload.get(..4) == Some(Limits::REQUEST_MAGIC.to_le_bytes().as_slice());
    decode(&payload).map(Some).map_err(|error| {
        if request && error.kind() == io::ErrorKind::UnexpectedEof {
            io::Error::new(io::ErrorKind::InvalidData, error)
        } else {
            error
        }
    })
}

pub fn write_frame(output: &mut impl Write, frame: &Frame<'_>) -> io::Result<()> {
    let payload = encode(frame)?;
    output.write_all(&(payload.len() as u32).to_le_bytes())?;
    output.write_all(&payload)?;
    output.flush()
}

fn encode(frame: &Frame<'_>) -> io::Result<Vec<u8>> {
    let mut payload = Vec::new();
    match frame {
        Frame::Request {
            id,
            context,
            candidates,
        } => {
            if context.len() > Limits::MAX_CONTEXT_BYTES
                || candidates.is_empty()
                || candidates.len() > Limits::MAX_CANDIDATES
            {
                return invalid_input("invalid request bounds");
            }
            put32(&mut payload, Limits::REQUEST_MAGIC);
            put16(&mut payload, Limits::VERSION);
            put16(&mut payload, 0);
            put64(&mut payload, *id);
            put32(&mut payload, context.len() as u32);
            put32(&mut payload, candidates.len() as u32);
            payload.extend_from_slice(context);
            for candidate in candidates {
                if candidate.text.is_empty() || candidate.text.len() > Limits::MAX_CANDIDATE_BYTES {
                    return invalid_input("invalid candidate length");
                }
                put64(&mut payload, candidate.fingerprint);
                payload.extend_from_slice(&candidate.local_cost.to_le_bytes());
                put32(&mut payload, candidate.text.len() as u32);
                payload.extend_from_slice(candidate.text.as_bytes());
            }
        }
        Frame::Response {
            id,
            status,
            legacy_tier_reserved,
            scores,
        } => {
            if scores.len() > Limits::MAX_CANDIDATES
                || scores.iter().any(|score| !score.value.is_finite())
            {
                return invalid_input("invalid response scores");
            }
            put32(&mut payload, Limits::RESPONSE_MAGIC);
            put16(&mut payload, Limits::VERSION);
            put16(&mut payload, *status);
            put64(&mut payload, *id);
            put16(&mut payload, *legacy_tier_reserved);
            put16(&mut payload, 0);
            put32(&mut payload, scores.len() as u32);
            for score in scores {
                put64(&mut payload, score.fingerprint);
                put32(&mut payload, score.value.to_bits());
            }
        }
    }
    if payload.len() > Limits::MAX_FRAME {
        return invalid_input("frame too large");
    }
    Ok(payload)
}

fn decode(payload: &[u8]) -> io::Result<Frame<'static>> {
    let mut cursor = 0;
    let magic = get32(payload, &mut cursor)?;
    if get16(payload, &mut cursor)? != Limits::VERSION {
        return invalid_data("invalid version");
    }
    if magic == Limits::REQUEST_MAGIC {
        if get16(payload, &mut cursor)? != 0 {
            return invalid_data("invalid request header");
        }
        let id = get64(payload, &mut cursor)?;
        let context_len = get32(payload, &mut cursor)? as usize;
        let count = get32(payload, &mut cursor)? as usize;
        if context_len > Limits::MAX_CONTEXT_BYTES || count == 0 || count > Limits::MAX_CANDIDATES {
            return invalid_data("invalid request bounds");
        }
        let context = Cow::Owned(take(payload, &mut cursor, context_len)?.to_vec());
        let mut candidates = Vec::with_capacity(count);
        for _ in 0..count {
            let fingerprint = get64(payload, &mut cursor)?;
            let local_cost = i32::from_le_bytes(take(payload, &mut cursor, 4)?.try_into().unwrap());
            let length = get32(payload, &mut cursor)? as usize;
            if length == 0 || length > Limits::MAX_CANDIDATE_BYTES {
                return invalid_data("invalid candidate length");
            }
            let candidate =
                std::str::from_utf8(take(payload, &mut cursor, length)?).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "candidate is not UTF-8")
                })?;
            candidates.push(Candidate {
                fingerprint,
                local_cost,
                text: Cow::Owned(candidate.to_owned()),
            });
        }
        exact(payload, cursor)?;
        Ok(Frame::Request {
            id,
            context,
            candidates,
        })
    } else if magic == Limits::RESPONSE_MAGIC {
        let status = get16(payload, &mut cursor)?;
        let id = get64(payload, &mut cursor)?;
        let legacy_tier_reserved = get16(payload, &mut cursor)?;
        if get16(payload, &mut cursor)? != 0 {
            return invalid_data("invalid response reserved field");
        }
        let count = get32(payload, &mut cursor)? as usize;
        if count > Limits::MAX_CANDIDATES {
            return invalid_data("invalid response count");
        }
        let mut scores = Vec::with_capacity(count);
        for _ in 0..count {
            scores.push(Score {
                fingerprint: get64(payload, &mut cursor)?,
                value: f32::from_bits(get32(payload, &mut cursor)?),
            });
        }
        exact(payload, cursor)?;
        // Decoder deliberately preserves all f32 bit patterns. The engine owns
        // semantic rejection; only the success encoder requires finite scores.
        Ok(Frame::Response {
            id,
            status,
            legacy_tier_reserved,
            scores,
        })
    } else {
        invalid_data("invalid magic")
    }
}

fn invalid_data<T>(message: &'static str) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidData, message))
}
fn invalid_input<T>(message: &'static str) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidInput, message))
}
fn exact(payload: &[u8], cursor: usize) -> io::Result<()> {
    if cursor == payload.len() {
        Ok(())
    } else {
        invalid_data("trailing bytes")
    }
}
fn take<'a>(payload: &'a [u8], cursor: &mut usize, length: usize) -> io::Result<&'a [u8]> {
    let end = cursor
        .checked_add(length)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame overflow"))?;
    let bytes = payload
        .get(*cursor..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated frame"))?;
    *cursor = end;
    Ok(bytes)
}
fn get16(payload: &[u8], cursor: &mut usize) -> io::Result<u16> {
    Ok(u16::from_le_bytes(
        take(payload, cursor, 2)?.try_into().unwrap(),
    ))
}
fn get32(payload: &[u8], cursor: &mut usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(
        take(payload, cursor, 4)?.try_into().unwrap(),
    ))
}
fn get64(payload: &[u8], cursor: &mut usize) -> io::Result<u64> {
    Ok(u64::from_le_bytes(
        take(payload, cursor, 8)?.try_into().unwrap(),
    ))
}
fn put16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes())
}
fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes())
}
fn put64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_candidate_request() -> Frame<'static> {
        Frame::Request {
            id: 7,
            context: Cow::Borrowed(&[]),
            candidates: vec![
                Candidate {
                    fingerprint: 9,
                    local_cost: 1,
                    text: Cow::Borrowed("A"),
                },
                Candidate {
                    fingerprint: 10,
                    local_cost: -2,
                    text: Cow::Borrowed("日本"),
                },
            ],
        }
    }

    #[test]
    fn truncated_transport_and_malformed_request_keep_distinct_errors() {
        let mut transport = vec![24, 0, 0, 0];
        transport.extend_from_slice(&Limits::REQUEST_MAGIC.to_le_bytes());
        assert_eq!(
            read_frame(&mut transport.as_slice()).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        transport[..4].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(
            read_frame(&mut transport.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn protocol_v1_two_candidate_request_bytes_are_fixed() {
        let mut actual = Vec::new();
        write_frame(&mut actual, &two_candidate_request()).unwrap();
        #[rustfmt::skip]
        let expected = [
            0x3f, 0, 0, 0,
            0x53, 0x4b, 0x4e, 0x52, 1, 0, 0, 0,
            7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0,
            9, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, b'A',
            10, 0, 0, 0, 0, 0, 0, 0, 0xfe, 0xff, 0xff, 0xff,
            6, 0, 0, 0, 0xe6, 0x97, 0xa5, 0xe6, 0x9c, 0xac,
        ];
        assert_eq!(actual, expected);
        assert_eq!(
            read_frame(&mut actual.as_slice()).unwrap(),
            Some(two_candidate_request())
        );
    }

    #[test]
    fn protocol_v1_response_bytes_keep_reserved_slots_and_float_bits() {
        let frame = Frame::Response {
            id: 7,
            status: 0,
            legacy_tier_reserved: 0,
            scores: vec![
                Score {
                    fingerprint: 9,
                    value: -1.25,
                },
                Score {
                    fingerprint: 10,
                    value: 0.5,
                },
            ],
        };
        let mut actual = Vec::new();
        write_frame(&mut actual, &frame).unwrap();
        #[rustfmt::skip]
        let expected = [
            0x30, 0, 0, 0, 0x53, 0x4b, 0x4e, 0x53, 1, 0, 0, 0,
            7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0,
            9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xa0, 0xbf,
            10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3f,
        ];
        assert_eq!(actual, expected);
        assert_eq!(read_frame(&mut actual.as_slice()).unwrap(), Some(frame));
    }

    #[test]
    fn response_decoder_preserves_nonfinite_bits_but_encoder_rejects_them() {
        let mut bytes = vec![
            0x24, 0, 0, 0, 0x53, 0x4b, 0x4e, 0x53, 1, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            1, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0,
        ];
        bytes.extend_from_slice(&f32::NAN.to_bits().to_le_bytes());
        match read_frame(&mut bytes.as_slice()).unwrap().unwrap() {
            Frame::Response { scores, .. } => assert!(scores[0].value.is_nan()),
            _ => panic!("expected response"),
        }
        let nonfinite = Frame::Response {
            id: 7,
            status: 0,
            legacy_tier_reserved: 0,
            scores: vec![Score {
                fingerprint: 9,
                value: f32::NAN,
            }],
        };
        assert_eq!(
            write_frame(&mut Vec::new(), &nonfinite).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
