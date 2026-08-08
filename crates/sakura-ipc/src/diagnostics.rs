//! Durable, bounded diagnostics for client-side IPC timeouts.
//!
//! A timeout is exceptional and already consumed the caller's deadline, so
//! recording one synchronously does not add work to the steady-state key path.
//! Each event is one checksummed fixed-size append. Separate host processes can
//! append without sharing an in-process lock, settings can inspect the file
//! while hosts remain open, and a torn final write is reported rather than
//! counted. The hard byte ceiling prevents a broken provider from turning a
//! timeout storm into an unbounded disk load path.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Threading::GetCurrentThreadId;

const MAGIC: &[u8; 4] = b"SKTO";
const FORMAT_VERSION: u16 = 1;
const RECORD_BYTES: usize = 32;
pub const MAX_TIMEOUT_LOG_BYTES: u64 = 1024 * 1024;

/// The bounded operation whose deadline expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum TimeoutOperation {
    Connect = 1,
    Handshake = 2,
    Key = 3,
    Commit = 4,
    Reconvert = 5,
    Revert = 6,
    UiPlacement = 7,
    Resynchronize = 8,
    Administration = 9,
}

impl TimeoutOperation {
    pub const ALL: [Self; 9] = [
        Self::Connect,
        Self::Handshake,
        Self::Key,
        Self::Commit,
        Self::Reconvert,
        Self::Revert,
        Self::UiPlacement,
        Self::Resynchronize,
        Self::Administration,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Handshake => "handshake",
            Self::Key => "key",
            Self::Commit => "commit",
            Self::Reconvert => "reconvert",
            Self::Revert => "revert",
            Self::UiPlacement => "ui-placement",
            Self::Resynchronize => "resynchronize",
            Self::Administration => "administration",
        }
    }

    fn from_wire(value: u16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|operation| *operation as u16 == value)
    }

    const fn index(self) -> usize {
        self as usize - 1
    }
}

/// Aggregate shown by settings and captured by dogfood evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeoutDiagnostics {
    counts: [u64; TimeoutOperation::ALL.len()],
    pub valid_events: u64,
    pub invalid_records: u64,
    pub ignored_tail_bytes: u64,
    pub first_timestamp_ms: Option<u64>,
    pub last_timestamp_ms: Option<u64>,
    pub reached_capacity: bool,
}

impl Default for TimeoutDiagnostics {
    fn default() -> Self {
        Self {
            counts: [0; TimeoutOperation::ALL.len()],
            valid_events: 0,
            invalid_records: 0,
            ignored_tail_bytes: 0,
            first_timestamp_ms: None,
            last_timestamp_ms: None,
            reached_capacity: false,
        }
    }
}

impl TimeoutDiagnostics {
    pub fn count(&self, operation: TimeoutOperation) -> u64 {
        self.counts[operation.index()]
    }
}

pub fn default_timeout_log_path() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for IPC timeout diagnostics",
        )
    })?;
    Ok(PathBuf::from(local)
        .join("SakuraInput")
        .join("diagnostics")
        .join("ipc-timeouts.bin"))
}

/// Best-effort public entry point for DLL callers. Callers intentionally
/// discard this result: diagnostics must never turn a recoverable timeout into
/// a host-application failure.
pub fn record_timeout(operation: TimeoutOperation) -> io::Result<()> {
    record_timeout_at(&default_timeout_log_path()?, operation)
}

pub fn record_timeout_at(path: &Path, operation: TimeoutOperation) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let current = file.metadata()?.len();
    if current.saturating_add(RECORD_BYTES as u64) > MAX_TIMEOUT_LOG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "IPC timeout diagnostics reached their hard byte ceiling",
        ));
    }

    let mut record = [0u8; RECORD_BYTES];
    record[..4].copy_from_slice(MAGIC);
    record[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    record[6..8].copy_from_slice(&(operation as u16).to_le_bytes());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });
    record[8..16].copy_from_slice(&timestamp.to_le_bytes());
    record[16..20].copy_from_slice(&std::process::id().to_le_bytes());
    // SAFETY: GetCurrentThreadId has no preconditions and returns a scalar.
    let thread = unsafe { GetCurrentThreadId() };
    record[20..24].copy_from_slice(&thread.to_le_bytes());
    // Bytes 24..28 are reserved for a future request-id discriminator.
    let checksum = crc32(&record[..28]);
    record[28..].copy_from_slice(&checksum.to_le_bytes());

    // Keep this as one OS-facing write. Retrying a partial append could place
    // the remainder after a different process's record and make two records
    // look valid when neither is.
    match file.write(&record)? {
        RECORD_BYTES => Ok(()),
        written => Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!("IPC timeout record was only {written}/{RECORD_BYTES} bytes"),
        )),
    }
}

pub fn read_timeout_diagnostics(path: &Path) -> io::Result<TimeoutDiagnostics> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(TimeoutDiagnostics::default());
        }
        Err(error) => return Err(error),
    };
    if bytes.len() as u64 > MAX_TIMEOUT_LOG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC timeout diagnostics exceed their hard byte ceiling",
        ));
    }

    let mut diagnostics = TimeoutDiagnostics {
        reached_capacity: bytes.len() as u64 == MAX_TIMEOUT_LOG_BYTES,
        ignored_tail_bytes: (bytes.len() % RECORD_BYTES) as u64,
        ..TimeoutDiagnostics::default()
    };
    for record in bytes.chunks_exact(RECORD_BYTES) {
        let valid_header =
            &record[..4] == MAGIC && u16::from_le_bytes([record[4], record[5]]) == FORMAT_VERSION;
        let expected = u32::from_le_bytes(record[28..32].try_into().expect("fixed record"));
        let operation = u16::from_le_bytes([record[6], record[7]]);
        let Some(operation) = TimeoutOperation::from_wire(operation) else {
            diagnostics.invalid_records = diagnostics.invalid_records.saturating_add(1);
            continue;
        };
        if !valid_header || crc32(&record[..28]) != expected {
            diagnostics.invalid_records = diagnostics.invalid_records.saturating_add(1);
            continue;
        }
        let timestamp = u64::from_le_bytes(record[8..16].try_into().expect("fixed record"));
        diagnostics.valid_events = diagnostics.valid_events.saturating_add(1);
        diagnostics.counts[operation.index()] =
            diagnostics.counts[operation.index()].saturating_add(1);
        diagnostics.first_timestamp_ms = Some(
            diagnostics
                .first_timestamp_ms
                .map_or(timestamp, |first| first.min(timestamp)),
        );
        diagnostics.last_timestamp_ms = Some(
            diagnostics
                .last_timestamp_ms
                .map_or(timestamp, |last| last.max(timestamp)),
        );
    }
    Ok(diagnostics)
}

pub fn clear_timeout_diagnostics(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.sync_all()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

    fn temporary_file(name: &str) -> PathBuf {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "sakura-ipc-diagnostics-{}-{name}-{id}.bin",
            std::process::id()
        ))
    }

    #[test]
    fn fixed_records_are_counted_by_operation_and_clear_to_zero() {
        let path = temporary_file("roundtrip");
        record_timeout_at(&path, TimeoutOperation::Key).expect("key timeout");
        record_timeout_at(&path, TimeoutOperation::Key).expect("second key timeout");
        record_timeout_at(&path, TimeoutOperation::Reconvert).expect("reconversion timeout");

        let diagnostics = read_timeout_diagnostics(&path).expect("diagnostics");
        assert_eq!(diagnostics.valid_events, 3);
        assert_eq!(diagnostics.count(TimeoutOperation::Key), 2);
        assert_eq!(diagnostics.count(TimeoutOperation::Reconvert), 1);
        assert_eq!(diagnostics.invalid_records, 0);
        assert_eq!(diagnostics.ignored_tail_bytes, 0);
        assert!(diagnostics.first_timestamp_ms.is_some());
        assert!(diagnostics.last_timestamp_ms.is_some());

        clear_timeout_diagnostics(&path).expect("clear");
        assert_eq!(
            read_timeout_diagnostics(&path)
                .expect("cleared diagnostics")
                .valid_events,
            0
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn corrupt_and_torn_records_are_visible_but_never_counted() {
        let path = temporary_file("corrupt");
        record_timeout_at(&path, TimeoutOperation::Commit).expect("valid record");
        let mut bytes = fs::read(&path).expect("read");
        bytes[6] = 0xff;
        bytes.extend_from_slice(b"tail");
        fs::write(&path, bytes).expect("corrupt fixture");

        let diagnostics = read_timeout_diagnostics(&path).expect("diagnostics");
        assert_eq!(diagnostics.valid_events, 0);
        assert_eq!(diagnostics.invalid_records, 1);
        assert_eq!(diagnostics.ignored_tail_bytes, 4);
        let _ = fs::remove_file(path);
    }
}
