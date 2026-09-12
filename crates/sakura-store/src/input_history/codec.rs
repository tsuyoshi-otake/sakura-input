use std::io;

use sakura_values::{AiTextOperation, AiTextStatus};

use super::format::*;

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_string(bytes: &mut Vec<u8>, value: &str) -> io::Result<()> {
    let length =
        u16::try_from(value.len()).map_err(|_| invalid_data("input history text too long"))?;
    put_u16(bytes, length);
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

const RECORD_KEY: u8 = 1;
const RECORD_COMMIT: u8 = 2;
const RECORD_AI_TEXT: u8 = 3;
const RECORD_ENGINE: u8 = 4;

fn decode_ai_operation(value: u8) -> io::Result<AiTextOperation> {
    match value {
        1 => Ok(AiTextOperation::Transform),
        2 => Ok(AiTextOperation::Proofread),
        _ => Err(invalid_data("unknown AI text operation")),
    }
}
fn decode_ai_status(value: u8) -> io::Result<AiTextStatus> {
    match value {
        1 => Ok(AiTextStatus::Applied),
        2 => Ok(AiTextStatus::Cancelled),
        3 => Ok(AiTextStatus::Timeout),
        4 => Ok(AiTextStatus::MissingKey),
        5 => Ok(AiTextStatus::WorkerError),
        6 => Ok(AiTextStatus::ApiError),
        7 => Ok(AiTextStatus::Rejected),
        _ => Err(invalid_data("unknown AI text status")),
    }
}

impl InputHistoryRecord {
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(256);
        match self {
            Self::Key(record) => {
                bytes.push(RECORD_KEY);
                put_u64(&mut bytes, record.sequence);
                put_u64(&mut bytes, record.timestamp_ms);
                put_u64(&mut bytes, record.session);
                bytes.push(record.scope as u8);
                put_u16(&mut bytes, record.key_code);
                put_u32(
                    &mut bytes,
                    record.character.map_or(0, |character| character as u32),
                );
                bytes.push(record.modifiers);
                bytes.push(u8::from(record.repeat));
                bytes.push(u8::from(record.consumed));
                bytes.push(record.state_before);
                bytes.push(record.state_after);
                bytes.push(record.mode_before);
                bytes.push(record.mode_after);
                put_string(&mut bytes, &record.preedit_before)?;
                put_string(&mut bytes, &record.preedit_after)?;
                put_string(&mut bytes, &record.commit)?;
                put_u16(&mut bytes, record.delete_before);
                bytes.push(u8::from(record.beep));
                put_string(&mut bytes, &record.action)?;
                put_u64(&mut bytes, record.dropped_before);
            }
            Self::Commit(record) => {
                bytes.push(RECORD_COMMIT);
                put_u64(&mut bytes, record.sequence);
                put_u64(&mut bytes, record.timestamp_ms);
                put_u64(&mut bytes, record.session);
                bytes.push(record.scope as u8);
                put_u16(&mut bytes, record.left_context);
                put_u16(&mut bytes, record.right_context);
                put_string(&mut bytes, &record.reading)?;
                put_string(&mut bytes, &record.surface)?;
            }
            Self::AiText(record) => {
                bytes.push(RECORD_AI_TEXT);
                put_u64(&mut bytes, record.sequence);
                put_u64(&mut bytes, record.timestamp_ms);
                put_u64(&mut bytes, record.session);
                bytes.push(record.scope as u8);
                bytes.push(record.operation as u8);
                bytes.push(record.status as u8);
                put_string(&mut bytes, &record.source)?;
                put_string(&mut bytes, &record.result)?;
                put_string(&mut bytes, &record.model)?;
                put_string(&mut bytes, &record.provider)?;
                put_string(&mut bytes, &record.style)?;
                put_string(&mut bytes, &record.error_code)?;
                put_u64(&mut bytes, record.latency_ms);
                put_u32(&mut bytes, record.input_tokens);
                put_u32(&mut bytes, record.output_tokens);
                put_u32(&mut bytes, record.cached_tokens);
                put_u32(&mut bytes, record.attempts);
            }
            Self::Engine(record) => {
                bytes.push(RECORD_ENGINE);
                put_u64(&mut bytes, record.sequence);
                put_u64(&mut bytes, record.timestamp_ms);
                put_u64(&mut bytes, record.session);
                bytes.push(record.scope as u8);
                put_string(&mut bytes, &record.package_version)?;
                put_string(&mut bytes, &record.release_label)?;
            }
        }
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(invalid_data("input history record is too large"));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut reader = Reader::new(bytes);
        let kind = reader.u8()?;
        let sequence = reader.u64()?;
        let timestamp_ms = reader.u64()?;
        let session = reader.u64()?;
        let scope = HistoryScope::from_u8(reader.u8()?)?;
        let record = match kind {
            RECORD_KEY => {
                let key_code = reader.u16()?;
                let character = match reader.u32()? {
                    0 => None,
                    value => Some(
                        char::from_u32(value)
                            .ok_or_else(|| invalid_data("invalid key character"))?,
                    ),
                };
                let modifiers = reader.u8()?;
                let repeat = reader.bool()?;
                let consumed = reader.bool()?;
                let state_before = reader.u8()?;
                let state_after = reader.u8()?;
                let mode_before = reader.u8()?;
                let mode_after = reader.u8()?;
                let preedit_before = reader.string()?;
                let preedit_after = reader.string()?;
                let commit = reader.string()?;
                let delete_before = reader.u16()?;
                let beep = reader.bool()?;
                let action = reader.string()?;
                let dropped_before = reader.u64()?;
                Self::Key(KeyHistoryRecord {
                    sequence,
                    timestamp_ms,
                    session,
                    scope,
                    key_code,
                    character,
                    modifiers,
                    repeat,
                    consumed,
                    state_before,
                    state_after,
                    mode_before,
                    mode_after,
                    preedit_before,
                    preedit_after,
                    commit,
                    delete_before,
                    beep,
                    action,
                    dropped_before,
                })
            }
            RECORD_COMMIT => Self::Commit(CommitHistoryRecord {
                sequence,
                timestamp_ms,
                session,
                scope,
                left_context: reader.u16()?,
                right_context: reader.u16()?,
                reading: reader.string()?,
                surface: reader.string()?,
            }),
            RECORD_AI_TEXT => Self::AiText(AiTextHistoryRecord {
                sequence,
                timestamp_ms,
                session,
                scope,
                operation: decode_ai_operation(reader.u8()?)?,
                status: decode_ai_status(reader.u8()?)?,
                source: reader.string()?,
                result: reader.string()?,
                model: reader.string()?,
                provider: reader.string()?,
                style: reader.string()?,
                error_code: reader.string()?,
                latency_ms: reader.u64()?,
                input_tokens: reader.u32()?,
                output_tokens: reader.u32()?,
                cached_tokens: reader.u32()?,
                attempts: reader.u32()?,
            }),
            RECORD_ENGINE => Self::Engine(EngineHistoryRecord {
                sequence,
                timestamp_ms,
                session,
                scope,
                package_version: reader.string()?,
                release_label: reader.string()?,
            }),
            _ => return Err(invalid_data("unknown input history record")),
        };
        reader.finish()?;
        Ok(record)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| invalid_data("input history record overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| invalid_data("truncated input history record"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(*self.take(1)?.first().expect("one byte"))
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn bool(&mut self) -> io::Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid_data("invalid input history boolean")),
        }
    }

    fn string(&mut self) -> io::Result<String> {
        let length = self.u16()? as usize;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| invalid_data("input history text is not UTF-8"))
    }

    fn finish(self) -> io::Result<()> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(invalid_data("trailing input history record bytes"))
        }
    }
}
