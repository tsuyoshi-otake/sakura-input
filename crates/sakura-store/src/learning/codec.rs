use std::io;

use super::format::{
    FORMAT_VERSION_1, FORMAT_VERSION_2, HEADER_LEN, LEARNING_FORMAT_VERSION, MAX_RECORD_BYTES,
    RECORD_COMMIT, RECORD_ENVELOPE_LEN, REJECTION_PREFIX, REJECTION_SEPARATOR,
    REPAIR_SUPPRESS_CONTEXT, REPAIR_SUPPRESS_SURFACE,
};

const MAGIC: &[u8; 4] = b"SKLR";

pub(crate) fn header(version: u16) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(MAGIC);
    header[4..6].copy_from_slice(&version.to_le_bytes());
    header
}

pub(crate) fn read_header(bytes: &[u8]) -> io::Result<u16> {
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid learning log header",
        ));
    }
    Ok(u16::from_le_bytes([bytes[4], bytes[5]]))
}

pub(crate) fn encode_record(
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
pub(crate) struct DecodedRecord<'a> {
    pub day: u32,
    pub left_context: u16,
    pub right_context: u16,
    pub reading: &'a str,
    pub surface: &'a str,
}

pub(crate) fn decode_record(payload: &[u8], version: u16) -> io::Result<DecodedRecord<'_>> {
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

/// What one verified COMMIT frame means to replay and to the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordMeaning<'a> {
    Commit,
    RepairSuppress,
    Rejection {
        reading: &'a str,
        surface: &'a str,
    },
    /// An empty-reading frame that is not a well-formed tombstone. No writer
    /// produces one; replay and the viewer skip it.
    Ignored,
}

pub(crate) fn record_meaning<'a>(record: &DecodedRecord<'a>) -> RecordMeaning<'a> {
    if record.left_context == REPAIR_SUPPRESS_CONTEXT
        && record.right_context == REPAIR_SUPPRESS_CONTEXT
        && record.surface == REPAIR_SUPPRESS_SURFACE
    {
        return RecordMeaning::RepairSuppress;
    }
    if !record.reading.is_empty() {
        return RecordMeaning::Commit;
    }
    record
        .surface
        .strip_prefix(REJECTION_PREFIX)
        .and_then(|rest| rest.split_once(REJECTION_SEPARATOR))
        .filter(|(reading, surface)| rejection_parts_are_valid(reading, surface))
        .map_or(RecordMeaning::Ignored, |(reading, surface)| {
            RecordMeaning::Rejection { reading, surface }
        })
}

fn rejection_parts_are_valid(reading: &str, surface: &str) -> bool {
    let reserved = [REJECTION_PREFIX, REJECTION_SEPARATOR];
    !reading.is_empty()
        && !surface.is_empty()
        && !reading.contains(reserved)
        && !surface.contains(reserved)
}

/// The tombstone frame's surface, or `None` when the pair could not round-trip
/// or the frame would exceed the record bound.
pub(crate) fn rejection_surface(reading: &str, surface: &str) -> Option<String> {
    if !rejection_parts_are_valid(reading, surface) {
        return None;
    }
    let encoded = format!("{REJECTION_PREFIX}{reading}{REJECTION_SEPARATOR}{surface}");
    let fits = u16::try_from(encoded.len()).is_ok()
        && 13usize.saturating_add(encoded.len()) <= MAX_RECORD_BYTES;
    fits.then_some(encoded)
}

pub(crate) fn record_at(
    bytes: &[u8],
    version: u16,
    offset: usize,
) -> Option<(usize, DecodedRecord<'_>)> {
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

/// Whether the unreadable tail of the current-format log `open` read —
/// everything from `offset` (the end of the valid prefix `scan_records`
/// returned) to `bytes.len()` — is a torn append rather than mid-file
/// corruption (#360 A3). A torn tail is truncated quietly as before; a
/// corrupt middle is quarantined first so no verified-after-the-fact byte
/// is destroyed without a copy. Anything ambiguous is corruption: an
/// extra copy only costs disk space.
///
/// Torn when:
/// - (t1) the rest is shorter than one record envelope, or
/// - (t2) the frame's declared length stays within `MAX_RECORD_BYTES` but
///   runs past the end of what was read, or
/// - (t3) every byte of the rest is zero — what NTFS returns past the
///   valid data length after a power loss.
pub(crate) fn unreadable_tail_is_torn(bytes: &[u8], offset: usize) -> bool {
    let rest = &bytes[offset.min(bytes.len())..];
    if rest.len() < RECORD_ENVELOPE_LEN {
        return true;
    }
    if rest.iter().all(|byte| *byte == 0) {
        return true;
    }
    let length = usize::try_from(u32::from_le_bytes(
        rest[..4].try_into().expect("checked envelope"),
    ))
    .unwrap_or(usize::MAX);
    if length > MAX_RECORD_BYTES {
        return false;
    }
    rest.len() < RECORD_ENVELOPE_LEN + length
}

pub(crate) fn scan_records(bytes: &[u8], version: u16) -> io::Result<(usize, u64)> {
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

pub(crate) fn upgrade_to_current(bytes: &[u8], source_version: u16) -> io::Result<Vec<u8>> {
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

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
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

#[cfg(test)]
mod rejection_tests {
    use super::{decode_record, encode_record, record_meaning, rejection_surface, RecordMeaning};
    use crate::learning::format::{
        LEARNING_FORMAT_VERSION, MAX_RECORD_BYTES, REJECTION_PREFIX, REJECTION_SEPARATOR,
        REPAIR_SUPPRESS_CONTEXT, REPAIR_SUPPRESS_SURFACE,
    };

    /// The meaning of the frame encoded from these fields, as text.
    fn meaning_of(reading: &str, surface: &str, left: u16, right: u16) -> String {
        let payload = encode_record(reading, surface, left, right, 20_000).unwrap();
        let record = decode_record(&payload, LEARNING_FORMAT_VERSION).unwrap();
        match record_meaning(&record) {
            RecordMeaning::Commit => "commit".to_owned(),
            RecordMeaning::RepairSuppress => "repair-suppress".to_owned(),
            RecordMeaning::Rejection { reading, surface } => format!("reject {reading} {surface}"),
            RecordMeaning::Ignored => "ignored".to_owned(),
        }
    }

    #[test]
    fn a_tombstone_round_trips_through_the_commit_frame() {
        let encoded = rejection_surface("かな", "仮名").expect("recordable");
        assert_eq!(meaning_of("", &encoded, 1841, 7), "reject かな 仮名");
        assert_eq!(meaning_of("かな", "仮名", 1841, 7), "commit");
        assert_eq!(
            meaning_of(
                "かな",
                REPAIR_SUPPRESS_SURFACE,
                REPAIR_SUPPRESS_CONTEXT,
                REPAIR_SUPPRESS_CONTEXT
            ),
            "repair-suppress"
        );
    }

    #[test]
    fn a_malformed_tombstone_is_ignored_and_never_a_commit() {
        let malformed = [
            String::new(),
            "仮名".to_owned(),
            format!("{REJECTION_PREFIX}かな仮名"),
            format!("{REJECTION_PREFIX}{REJECTION_SEPARATOR}仮名"),
            format!("{REJECTION_PREFIX}かな{REJECTION_SEPARATOR}"),
            format!("{REJECTION_PREFIX}か{REJECTION_SEPARATOR}な{REJECTION_SEPARATOR}仮名"),
        ];
        for surface in &malformed {
            assert_eq!(meaning_of("", surface, 0, 0), "ignored", "{surface:?}");
        }
    }

    #[test]
    fn an_unrecordable_pair_is_refused() {
        assert_eq!(rejection_surface("", "仮名"), None);
        assert_eq!(rejection_surface("かな", ""), None);
        assert_eq!(
            rejection_surface(&format!("か{REJECTION_SEPARATOR}な"), "仮名"),
            None
        );
        assert_eq!(
            rejection_surface("かな", &format!("{REJECTION_PREFIX}仮名")),
            None
        );
        let oversized = "x".repeat(MAX_RECORD_BYTES);
        assert_eq!(rejection_surface("かな", &oversized), None);
    }
}

#[cfg(test)]
mod tail_classification_tests {
    use super::{crc32, encode_record, header, unreadable_tail_is_torn};
    use crate::learning::format::{
        HEADER_LEN, LEARNING_FORMAT_VERSION, MAX_RECORD_BYTES, RECORD_ENVELOPE_LEN,
    };

    /// A header plus one valid frame; `last_good` is the end of that frame,
    /// so everything appended after it is the unreadable tail.
    fn log_with_tail(tail: &[u8]) -> (Vec<u8>, usize) {
        let payload = encode_record("かな", "加奈", 3, 4, 20_000).unwrap();
        let mut bytes = header(LEARNING_FORMAT_VERSION).to_vec();
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&crc32(&payload).to_le_bytes());
        bytes.extend_from_slice(&payload);
        let last_good = bytes.len();
        bytes.extend_from_slice(tail);
        assert!(last_good > HEADER_LEN);
        (bytes, last_good)
    }

    #[test]
    fn a_rest_shorter_than_an_envelope_is_torn() {
        let (bytes, last_good) = log_with_tail(&[9, 0, 0]);
        assert!(unreadable_tail_is_torn(&bytes, last_good));
    }

    #[test]
    fn a_frame_running_past_the_end_is_torn() {
        let mut tail = 100u32.to_le_bytes().to_vec();
        tail.extend_from_slice(&0u32.to_le_bytes());
        let (bytes, last_good) = log_with_tail(&tail);
        assert!(unreadable_tail_is_torn(&bytes, last_good));
    }

    #[test]
    fn an_all_zero_rest_is_torn() {
        let (bytes, last_good) = log_with_tail(&[0u8; 64]);
        assert!(unreadable_tail_is_torn(&bytes, last_good));
    }

    #[test]
    fn a_complete_frame_with_bad_crc_is_corruption() {
        let payload = encode_record("かな", "仮名", 3, 4, 20_000).unwrap();
        let mut damaged = payload.clone();
        *damaged.last_mut().unwrap() ^= 0xFF;
        let mut tail = (damaged.len() as u32).to_le_bytes().to_vec();
        tail.extend_from_slice(&crc32(&payload).to_le_bytes());
        tail.extend_from_slice(&damaged);
        let (bytes, last_good) = log_with_tail(&tail);
        assert!(!unreadable_tail_is_torn(&bytes, last_good));
    }

    #[test]
    fn a_complete_frame_that_does_not_decode_is_corruption() {
        // Valid CRC over a payload whose text is not UTF-8.
        let mut payload = encode_record("かな", "加奈", 3, 4, 20_000).unwrap();
        *payload.last_mut().unwrap() = 0xFF;
        let mut tail = (payload.len() as u32).to_le_bytes().to_vec();
        tail.extend_from_slice(&crc32(&payload).to_le_bytes());
        tail.extend_from_slice(&payload);
        let (bytes, last_good) = log_with_tail(&tail);
        assert!(!unreadable_tail_is_torn(&bytes, last_good));
    }

    #[test]
    fn an_oversized_length_field_is_corruption() {
        let mut tail = ((MAX_RECORD_BYTES + 1) as u32).to_le_bytes().to_vec();
        tail.extend_from_slice(&[0xAA; RECORD_ENVELOPE_LEN]);
        let (bytes, last_good) = log_with_tail(&tail);
        assert!(!unreadable_tail_is_torn(&bytes, last_good));
    }
}

#[cfg(test)]
mod pre_extraction_fixture_tests {
    use super::{crc32, encode_record, header, read_header, record_at, scan_records};
    use crate::learning::format::{
        HEADER_LEN, LEARNING_FORMAT_VERSION, REPAIR_SUPPRESS_CONTEXT, REPAIR_SUPPRESS_SURFACE,
    };

    #[test]
    fn pre_extraction_writer_fixture_preserves_all_fields_and_exact_bytes() {
        let hex = include_str!("../../tests/fixtures/learning-v3-pre-extraction.hex").trim();
        assert_eq!(hex.len() % 2, 0);
        let bytes: Vec<_> = (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
            .collect();
        assert_eq!(read_header(&bytes).unwrap(), LEARNING_FORMAT_VERSION);
        assert_eq!(
            scan_records(&bytes, LEARNING_FORMAT_VERSION).unwrap(),
            (bytes.len(), 3)
        );
        let expected = [
            ("synthetic-reading", "Synthetic\tSurface", 3, 4),
            (
                "synthetic-suppression",
                REPAIR_SUPPRESS_SURFACE,
                REPAIR_SUPPRESS_CONTEXT,
                REPAIR_SUPPRESS_CONTEXT,
            ),
            ("second-reading", "Second\nSurface", 5, 6),
        ];
        let mut offset = HEADER_LEN;
        let mut encoded = header(LEARNING_FORMAT_VERSION).to_vec();
        for (reading, surface, left, right) in expected {
            let (next, record) = record_at(&bytes, LEARNING_FORMAT_VERSION, offset).unwrap();
            assert_eq!(
                (
                    record.reading,
                    record.surface,
                    record.left_context,
                    record.right_context,
                    record.day
                ),
                (reading, surface, left, right, 20708)
            );
            let payload = encode_record(
                record.reading,
                record.surface,
                record.left_context,
                record.right_context,
                record.day,
            )
            .unwrap();
            encoded.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            encoded.extend_from_slice(&crc32(&payload).to_le_bytes());
            encoded.extend_from_slice(&payload);
            offset = next;
        }
        assert_eq!(offset, bytes.len());
        assert_eq!(encoded, bytes);
    }
}
