//! Settings-owned read and offline clear of the DPAPI-framed history file.
//!
//! Frame layout matches the engine writer: `HISTORY_MAGIC`, length/CRC, then a
//! sealed record. This module does not take the writer lock; callers use it
//! only after proving the engine process is absent, or in isolated tests.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use sakura_store::crypto::{DpapiSealer, Sealer};
use sakura_store::input_history::persistence::{
    compaction_transaction_path, MAX_INPUT_HISTORY_BYTES,
};
use sakura_store::input_history::{
    InputHistoryRecord, InputHistorySnapshot, HISTORY_FRAME_HEADER_LEN, HISTORY_HEADER_LEN,
    HISTORY_MAGIC, INPUT_HISTORY_FORMAT_VERSION, INPUT_HISTORY_FORMAT_VERSION_MIN,
    MAX_RECORD_BYTES,
};

pub fn read_snapshot(path: &Path) -> io::Result<InputHistorySnapshot> {
    require_no_compaction_transaction(path)?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_INPUT_HISTORY_BYTES {
        return Err(invalid_data("input history exceeds its hard size bound"));
    }
    let bytes = fs::read(path)?;
    snapshot_from_bytes(&bytes)
}

pub fn clear_path(path: &Path) -> io::Result<u64> {
    require_no_compaction_transaction(path)?;
    if !path.exists() {
        return Ok(0);
    }
    let before = read_snapshot(path)
        .map(|snapshot| snapshot.records.len() as u64)
        .unwrap_or(0);
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.set_len(0)?;
    file.write_all(&header())?;
    file.flush()?;
    Ok(before)
}

#[cfg(test)]
pub fn write_records(path: &Path, records: &[InputHistoryRecord]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = header().to_vec();
    for record in records {
        let payload = record.encode()?;
        let protected = DpapiSealer.seal(&payload)?;
        let length = u32::try_from(protected.len())
            .map_err(|_| invalid_data("protected input history record is too large"))?;
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&crc32(&protected).to_le_bytes());
        bytes.extend_from_slice(&protected);
    }
    fs::write(path, bytes)
}

fn snapshot_from_bytes(bytes: &[u8]) -> io::Result<InputHistorySnapshot> {
    if bytes.len() < HISTORY_HEADER_LEN || bytes[..4] != HISTORY_MAGIC {
        return Err(invalid_data("invalid input history header"));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if !(INPUT_HISTORY_FORMAT_VERSION_MIN..=INPUT_HISTORY_FORMAT_VERSION).contains(&version) {
        return Err(invalid_data("unsupported input history format"));
    }
    let mut offset = HISTORY_HEADER_LEN;
    let mut records = Vec::new();
    while offset + HISTORY_FRAME_HEADER_LEN <= bytes.len() {
        let length = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let expected_crc = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let payload_start = offset + HISTORY_FRAME_HEADER_LEN;
        let Some(payload_end) = payload_start.checked_add(length) else {
            break;
        };
        if length == 0 || length > MAX_RECORD_BYTES * 2 || payload_end > bytes.len() {
            break;
        }
        let encrypted = &bytes[payload_start..payload_end];
        if crc32(encrypted) != expected_crc {
            break;
        }
        let payload = DpapiSealer.open(encrypted)?;
        records.push(InputHistoryRecord::decode(&payload)?);
        offset = payload_end;
    }
    Ok(InputHistorySnapshot {
        format_version: version,
        records,
        ignored_tail_bytes: bytes.len().saturating_sub(offset),
    })
}

fn header() -> [u8; HISTORY_HEADER_LEN] {
    let mut header = [0u8; HISTORY_HEADER_LEN];
    header[..4].copy_from_slice(&HISTORY_MAGIC);
    header[4..6].copy_from_slice(&INPUT_HISTORY_FORMAT_VERSION.to_le_bytes());
    header
}

fn require_no_compaction_transaction(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(compaction_transaction_path(path)?) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::other(
            "input history recovery required: unresolved compaction",
        )),
    }
}

fn crc32(bytes: &[u8]) -> u32 {
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

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
