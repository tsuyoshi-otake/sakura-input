use std::io;

use super::format::{
    FORMAT_VERSION_1, FORMAT_VERSION_2, HEADER_LEN, LEARNING_FORMAT_VERSION, MAX_RECORD_BYTES,
    RECORD_COMMIT, RECORD_ENVELOPE_LEN,
};

const MAGIC: &[u8; 4] = b"SKLR";

pub fn header(version: u16) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(MAGIC);
    header[4..6].copy_from_slice(&version.to_le_bytes());
    header
}

pub fn read_header(bytes: &[u8]) -> io::Result<u16> {
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid learning log header",
        ));
    }
    Ok(u16::from_le_bytes([bytes[4], bytes[5]]))
}

pub fn encode_record(
    reading: &str,
    surface: &str,
    left_context: u16,
    right_context: u16,
    day: u32,
) -> io::Result<Vec<u8>> {
    let reading_len = u16::try_from(reading.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "reading too long"))?;
    let surface_len = u16::try_from(surface.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "surface too long"))?;
    let capacity = 13usize
        .checked_add(reading.len())
        .and_then(|size| size.checked_add(surface.len()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "record too large"))?;
    if capacity > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "learning record exceeds its bound",
        ));
    }
    let mut payload = Vec::with_capacity(capacity);
    payload.push(RECORD_COMMIT);
    payload.extend_from_slice(&day.to_le_bytes());
    payload.extend_from_slice(&left_context.to_le_bytes());
    payload.extend_from_slice(&right_context.to_le_bytes());
    payload.extend_from_slice(&reading_len.to_le_bytes());
    payload.extend_from_slice(&surface_len.to_le_bytes());
    payload.extend_from_slice(reading.as_bytes());
    payload.extend_from_slice(surface.as_bytes());
    Ok(payload)
}

#[derive(Debug, Clone, Copy)]
pub struct DecodedRecord<'a> {
    pub day: u32,
    pub left_context: u16,
    pub right_context: u16,
    pub reading: &'a str,
    pub surface: &'a str,
}

pub fn decode_record(payload: &[u8], version: u16) -> io::Result<DecodedRecord<'_>> {
    let (day_offset, left_context, right_context, lengths_offset) = match version {
        FORMAT_VERSION_1 => (0usize, 0u16, 0u16, 4usize),
        FORMAT_VERSION_2 if payload.first() == Some(&RECORD_COMMIT) => {
            if payload.len() < 11 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "short record"));
            }
            (
                1usize,
                u16::from_le_bytes([payload[5], payload[6]]),
                0u16,
                7usize,
            )
        }
        LEARNING_FORMAT_VERSION if payload.first() == Some(&RECORD_COMMIT) => {
            if payload.len() < 13 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "short record"));
            }
            (
                1usize,
                u16::from_le_bytes([payload[5], payload[6]]),
                u16::from_le_bytes([payload[7], payload[8]]),
                9usize,
            )
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown learning record",
            ));
        }
    };
    if payload.len() < lengths_offset + 4 || payload.len() < day_offset + 4 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short record"));
    }
    let day = u32::from_le_bytes(
        payload[day_offset..day_offset + 4]
            .try_into()
            .expect("checked four bytes"),
    );
    let reading_len = usize::from(u16::from_le_bytes([
        payload[lengths_offset],
        payload[lengths_offset + 1],
    ]));
    let surface_len = usize::from(u16::from_le_bytes([
        payload[lengths_offset + 2],
        payload[lengths_offset + 3],
    ]));
    let text_start = lengths_offset + 4;
    let reading_end = text_start
        .checked_add(reading_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "record overflow"))?;
    let surface_end = reading_end
        .checked_add(surface_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "record overflow"))?;
    if surface_end != payload.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "learning record length mismatch",
        ));
    }
    let reading = core::str::from_utf8(&payload[text_start..reading_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid reading UTF-8"))?;
    let surface = core::str::from_utf8(&payload[reading_end..surface_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid surface UTF-8"))?;
    Ok(DecodedRecord {
        day,
        left_context,
        right_context,
        reading,
        surface,
    })
}

pub fn record_at(bytes: &[u8], version: u16, offset: usize) -> Option<(usize, DecodedRecord<'_>)> {
    if offset > bytes.len() || bytes.len() - offset < RECORD_ENVELOPE_LEN {
        return None;
    }
    let length = usize::try_from(u32::from_le_bytes(
        bytes[offset..offset + 4].try_into().ok()?,
    ))
    .ok()?;
    if length > MAX_RECORD_BYTES {
        return None;
    }
    let expected_crc = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().ok()?);
    let payload_start = offset + RECORD_ENVELOPE_LEN;
    let payload_end = payload_start.checked_add(length)?;
    let payload = bytes.get(payload_start..payload_end)?;
    if crc32(payload) != expected_crc {
        return None;
    }
    decode_record(payload, version)
        .ok()
        .map(|record| (payload_end, record))
}

pub fn scan_records(bytes: &[u8], version: u16) -> io::Result<(usize, u64)> {
    if read_header(bytes)? != version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "learning log header version mismatch",
        ));
    }
    let mut offset = HEADER_LEN;
    let mut records = 0u64;
    while let Some((next, _)) = record_at(bytes, version, offset) {
        offset = next;
        records = records.saturating_add(1);
    }
    Ok((offset, records))
}

pub fn upgrade_to_current(bytes: &[u8], source_version: u16) -> io::Result<Vec<u8>> {
    let mut upgraded = header(LEARNING_FORMAT_VERSION).to_vec();
    let mut offset = HEADER_LEN;
    while offset < bytes.len() {
        let Some((payload_end, record)) = record_at(bytes, source_version, offset) else {
            break;
        };
        let current = encode_record(
            record.reading,
            record.surface,
            record.left_context,
            record.right_context,
            record.day,
        )?;
        upgraded.extend_from_slice(
            &u32::try_from(current.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record too large"))?
                .to_le_bytes(),
        );
        upgraded.extend_from_slice(&crc32(&current).to_le_bytes());
        upgraded.extend_from_slice(&current);
        offset = payload_end;
    }
    Ok(upgraded)
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
