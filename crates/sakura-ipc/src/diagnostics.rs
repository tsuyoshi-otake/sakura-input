//! Durable, bounded diagnostics for client-side IPC timeouts and for the
//! link resets that discard an engine session.
//!
//! A timeout is exceptional and already consumed the caller's deadline, so
//! recording one synchronously does not add work to the steady-state key path.
//! Each event is one checksummed fixed-size append. Separate host processes can
//! append without sharing an in-process lock, settings can inspect the file
//! while hosts remain open, and a torn final write is reported rather than
//! counted. The hard byte ceiling prevents a broken provider from turning a
//! timeout storm into an unbounded disk load path.
//!
//! Disconnects use the same record shape in their own file. They are kept
//! apart from timeouts deliberately: a disconnect is not a deadline expiry,
//! most of them are a correct response to a host lifecycle event, and mixing
//! the two would make the timeout counters mean something different than they
//! did before. Issue #102 needs to know *which* of the frontend's ~20 reset
//! paths fires after a conversion, because that reset is what leaves the next
//! Space with no composition to convert.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Threading::GetCurrentThreadId;

const MAGIC: &[u8; 4] = b"SKTO";
const MAGIC_DISCONNECT: &[u8; 4] = b"SKDC";
const FORMAT_VERSION: u16 = 1;
const RECORD_BYTES: usize = 32;
pub const MAX_TIMEOUT_LOG_BYTES: u64 = 1024 * 1024;
pub const MAX_DISCONNECT_LOG_BYTES: u64 = 1024 * 1024;

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
    ProbeKey = 10,
}

impl TimeoutOperation {
    pub const ALL: [Self; 10] = [
        Self::Connect,
        Self::Handshake,
        Self::Key,
        Self::Commit,
        Self::Reconvert,
        Self::Revert,
        Self::UiPlacement,
        Self::Resynchronize,
        Self::Administration,
        Self::ProbeKey,
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
            Self::ProbeKey => "probe-key",
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

/// Why the frontend threw away its engine link and, with it, the engine-side
/// session that owned the composition.
///
/// One variant per call site rather than per `CancelReason`: several sites
/// share a cancellation reason but sit on completely different paths, and the
/// question Issue #102 asks is which *path* runs, not which label it passes
/// to the journal. Names track the enclosing function so a count can be read
/// straight back to the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum DisconnectReason {
    UndoTerminalizationDeferred = 1,
    UndoTerminalizationSettled = 2,
    WriteContextObservationFailed = 3,
    CancelledStateTerminalized = 4,
    CancelledWritesSettled = 5,
    ServiceDetached = 6,
    DocumentContextReset = 7,
    UndoCommitSettleFailed = 8,
    DocumentAccessRevisionMismatch = 9,
    DocumentAccessUndoTerminalized = 10,
    DocumentAccessReconciled = 11,
    CandidateCommitPollFailed = 12,
    KeyContextAuthorityLost = 13,
    KeyPredecessorFailed = 14,
    EngineUnavailableRecovery = 15,
    CompositionProjectionAbandoned = 16,
    StaleQueuedWrite = 17,
    QueuedWriteTerminalFailure = 18,
    AppliedWriteUnacknowledged = 19,
    ReconversionUnavailable = 20,
    ReconversionFailed = 21,
}

impl DisconnectReason {
    pub const ALL: [Self; 21] = [
        Self::UndoTerminalizationDeferred,
        Self::UndoTerminalizationSettled,
        Self::WriteContextObservationFailed,
        Self::CancelledStateTerminalized,
        Self::CancelledWritesSettled,
        Self::ServiceDetached,
        Self::DocumentContextReset,
        Self::UndoCommitSettleFailed,
        Self::DocumentAccessRevisionMismatch,
        Self::DocumentAccessUndoTerminalized,
        Self::DocumentAccessReconciled,
        Self::CandidateCommitPollFailed,
        Self::KeyContextAuthorityLost,
        Self::KeyPredecessorFailed,
        Self::EngineUnavailableRecovery,
        Self::CompositionProjectionAbandoned,
        Self::StaleQueuedWrite,
        Self::QueuedWriteTerminalFailure,
        Self::AppliedWriteUnacknowledged,
        Self::ReconversionUnavailable,
        Self::ReconversionFailed,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::UndoTerminalizationDeferred => "undo-terminalization-deferred",
            Self::UndoTerminalizationSettled => "undo-terminalization-settled",
            Self::WriteContextObservationFailed => "write-context-observation-failed",
            Self::CancelledStateTerminalized => "cancelled-state-terminalized",
            Self::CancelledWritesSettled => "cancelled-writes-settled",
            Self::ServiceDetached => "service-detached",
            Self::DocumentContextReset => "document-context-reset",
            Self::UndoCommitSettleFailed => "undo-commit-settle-failed",
            Self::DocumentAccessRevisionMismatch => "document-access-revision-mismatch",
            Self::DocumentAccessUndoTerminalized => "document-access-undo-terminalized",
            Self::DocumentAccessReconciled => "document-access-reconciled",
            Self::CandidateCommitPollFailed => "candidate-commit-poll-failed",
            Self::KeyContextAuthorityLost => "key-context-authority-lost",
            Self::KeyPredecessorFailed => "key-predecessor-failed",
            Self::EngineUnavailableRecovery => "engine-unavailable-recovery",
            Self::CompositionProjectionAbandoned => "composition-projection-abandoned",
            Self::StaleQueuedWrite => "stale-queued-write",
            Self::QueuedWriteTerminalFailure => "queued-write-terminal-failure",
            Self::AppliedWriteUnacknowledged => "applied-write-unacknowledged",
            Self::ReconversionUnavailable => "reconversion-unavailable",
            Self::ReconversionFailed => "reconversion-failed",
        }
    }

    fn from_wire(value: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| *reason as u16 == value)
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

/// Aggregate of engine-link resets, in the same shape as the timeout view so
/// settings can render both without a second presentation path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectDiagnostics {
    counts: [u64; DisconnectReason::ALL.len()],
    pub valid_events: u64,
    pub invalid_records: u64,
    pub ignored_tail_bytes: u64,
    pub first_timestamp_ms: Option<u64>,
    pub last_timestamp_ms: Option<u64>,
    pub reached_capacity: bool,
}

impl Default for DisconnectDiagnostics {
    fn default() -> Self {
        Self {
            counts: [0; DisconnectReason::ALL.len()],
            valid_events: 0,
            invalid_records: 0,
            ignored_tail_bytes: 0,
            first_timestamp_ms: None,
            last_timestamp_ms: None,
            reached_capacity: false,
        }
    }
}

impl DisconnectDiagnostics {
    pub fn count(&self, reason: DisconnectReason) -> u64 {
        self.counts[reason.index()]
    }
}

fn diagnostics_dir() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for IPC diagnostics",
        )
    })?;
    Ok(PathBuf::from(local).join("SakuraInput").join("diagnostics"))
}

pub fn default_timeout_log_path() -> io::Result<PathBuf> {
    Ok(diagnostics_dir()?.join("ipc-timeouts.bin"))
}

pub fn default_disconnect_log_path() -> io::Result<PathBuf> {
    Ok(diagnostics_dir()?.join("ipc-disconnects.bin"))
}

/// Totals that every bounded diagnostics log carries, whatever its records
/// happen to mean. Keeping them apart from the per-kind counters lets the
/// append and scan code below be written once and shared by both logs, so a
/// change to the record format cannot land in one log and miss the other.
#[derive(Debug, Clone, Copy, Default)]
struct EventTotals {
    valid_events: u64,
    invalid_records: u64,
    ignored_tail_bytes: u64,
    first_timestamp_ms: Option<u64>,
    last_timestamp_ms: Option<u64>,
    reached_capacity: bool,
}

/// Appends one fixed-width record.
///
/// `magic` is what keeps the two logs from ever being read as each other: a
/// record written for one kind fails the header check in the reader for the
/// other, so even a misdirected write is counted as invalid rather than
/// silently inflating a counter that means something else.
fn append_event(
    path: &Path,
    magic: &[u8; 4],
    ceiling: u64,
    code: u16,
    kind: &str,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let current = file.metadata()?.len();
    if current.saturating_add(RECORD_BYTES as u64) > ceiling {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            format!("IPC {kind} diagnostics reached their hard byte ceiling"),
        ));
    }

    let mut record = [0u8; RECORD_BYTES];
    record[..4].copy_from_slice(magic);
    record[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    record[6..8].copy_from_slice(&code.to_le_bytes());
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
            format!("IPC {kind} record was only {written}/{RECORD_BYTES} bytes"),
        )),
    }
}

/// Tallies one log into `counts`.
///
/// `resolve` maps a wire code to its index. A code it does not recognise makes
/// the record invalid rather than dropping it silently, so a build reading a
/// log written by a newer one still reports that something it cannot name was
/// there instead of under-reporting the total.
fn scan_events(
    path: &Path,
    magic: &[u8; 4],
    ceiling: u64,
    kind: &str,
    counts: &mut [u64],
    resolve: impl Fn(u16) -> Option<usize>,
) -> io::Result<EventTotals> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(EventTotals::default());
        }
        Err(error) => return Err(error),
    };
    if bytes.len() as u64 > ceiling {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("IPC {kind} diagnostics exceed their hard byte ceiling"),
        ));
    }

    let mut totals = EventTotals {
        reached_capacity: bytes.len() as u64 == ceiling,
        ignored_tail_bytes: (bytes.len() % RECORD_BYTES) as u64,
        ..EventTotals::default()
    };
    for record in bytes.chunks_exact(RECORD_BYTES) {
        let valid_header =
            &record[..4] == magic && u16::from_le_bytes([record[4], record[5]]) == FORMAT_VERSION;
        let expected = u32::from_le_bytes(record[28..32].try_into().expect("fixed record"));
        let code = u16::from_le_bytes([record[6], record[7]]);
        let Some(index) = resolve(code) else {
            totals.invalid_records = totals.invalid_records.saturating_add(1);
            continue;
        };
        if !valid_header || crc32(&record[..28]) != expected {
            totals.invalid_records = totals.invalid_records.saturating_add(1);
            continue;
        }
        let timestamp = u64::from_le_bytes(record[8..16].try_into().expect("fixed record"));
        totals.valid_events = totals.valid_events.saturating_add(1);
        counts[index] = counts[index].saturating_add(1);
        totals.first_timestamp_ms = Some(
            totals
                .first_timestamp_ms
                .map_or(timestamp, |first| first.min(timestamp)),
        );
        totals.last_timestamp_ms = Some(
            totals
                .last_timestamp_ms
                .map_or(timestamp, |last| last.max(timestamp)),
        );
    }
    Ok(totals)
}

fn truncate_log(path: &Path) -> io::Result<()> {
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

/// Best-effort public entry point for DLL callers. Callers intentionally
/// discard this result: diagnostics must never turn a recoverable timeout into
/// a host-application failure.
pub fn record_timeout(operation: TimeoutOperation) -> io::Result<()> {
    record_timeout_at(&default_timeout_log_path()?, operation)
}

pub fn record_timeout_at(path: &Path, operation: TimeoutOperation) -> io::Result<()> {
    append_event(
        path,
        MAGIC,
        MAX_TIMEOUT_LOG_BYTES,
        operation as u16,
        "timeout",
    )
}

pub fn read_timeout_diagnostics(path: &Path) -> io::Result<TimeoutDiagnostics> {
    let mut diagnostics = TimeoutDiagnostics::default();
    let totals = scan_events(
        path,
        MAGIC,
        MAX_TIMEOUT_LOG_BYTES,
        "timeout",
        &mut diagnostics.counts,
        |code| TimeoutOperation::from_wire(code).map(TimeoutOperation::index),
    )?;
    diagnostics.valid_events = totals.valid_events;
    diagnostics.invalid_records = totals.invalid_records;
    diagnostics.ignored_tail_bytes = totals.ignored_tail_bytes;
    diagnostics.first_timestamp_ms = totals.first_timestamp_ms;
    diagnostics.last_timestamp_ms = totals.last_timestamp_ms;
    diagnostics.reached_capacity = totals.reached_capacity;
    Ok(diagnostics)
}

pub fn clear_timeout_diagnostics(path: &Path) -> io::Result<()> {
    truncate_log(path)
}

/// Best-effort public entry point for the frontend, under the same discipline
/// as `record_timeout`: a diagnostics failure must never turn a link reset,
/// which is already the recovery path, into a host-application failure.
pub fn record_disconnect(reason: DisconnectReason) -> io::Result<()> {
    record_disconnect_at(&default_disconnect_log_path()?, reason)
}

pub fn record_disconnect_at(path: &Path, reason: DisconnectReason) -> io::Result<()> {
    append_event(
        path,
        MAGIC_DISCONNECT,
        MAX_DISCONNECT_LOG_BYTES,
        reason as u16,
        "disconnect",
    )
}

pub fn read_disconnect_diagnostics(path: &Path) -> io::Result<DisconnectDiagnostics> {
    let mut diagnostics = DisconnectDiagnostics::default();
    let totals = scan_events(
        path,
        MAGIC_DISCONNECT,
        MAX_DISCONNECT_LOG_BYTES,
        "disconnect",
        &mut diagnostics.counts,
        |code| DisconnectReason::from_wire(code).map(DisconnectReason::index),
    )?;
    diagnostics.valid_events = totals.valid_events;
    diagnostics.invalid_records = totals.invalid_records;
    diagnostics.ignored_tail_bytes = totals.ignored_tail_bytes;
    diagnostics.first_timestamp_ms = totals.first_timestamp_ms;
    diagnostics.last_timestamp_ms = totals.last_timestamp_ms;
    diagnostics.reached_capacity = totals.reached_capacity;
    Ok(diagnostics)
}

pub fn clear_disconnect_diagnostics(path: &Path) -> io::Result<()> {
    truncate_log(path)
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

    #[test]
    fn every_disconnect_reason_round_trips_and_clears() {
        let path = temporary_file("disconnects");
        for reason in DisconnectReason::ALL {
            record_disconnect_at(&path, reason).expect("disconnect record");
        }
        record_disconnect_at(&path, DisconnectReason::ServiceDetached).expect("repeat");

        let diagnostics = read_disconnect_diagnostics(&path).expect("diagnostics");
        assert_eq!(
            diagnostics.valid_events,
            DisconnectReason::ALL.len() as u64 + 1
        );
        assert_eq!(diagnostics.invalid_records, 0);
        assert_eq!(diagnostics.ignored_tail_bytes, 0);
        for reason in DisconnectReason::ALL {
            let expected = u64::from(reason == DisconnectReason::ServiceDetached) + 1;
            assert_eq!(diagnostics.count(reason), expected, "{}", reason.name());
        }

        clear_disconnect_diagnostics(&path).expect("clear");
        assert_eq!(
            read_disconnect_diagnostics(&path)
                .expect("cleared")
                .valid_events,
            0
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn the_two_logs_never_count_each_other_despite_overlapping_codes() {
        // Wire code 3 is a valid reason and a valid operation, so only the
        // magic keeps a misdirected record out of the wrong counter. Without
        // it this record would be indistinguishable from a real timeout.
        let disconnects = temporary_file("cross-disconnect");
        record_disconnect_at(
            &disconnects,
            DisconnectReason::WriteContextObservationFailed,
        )
        .expect("disconnect record");
        let as_timeouts = read_timeout_diagnostics(&disconnects).expect("timeout read");
        assert_eq!(as_timeouts.valid_events, 0);
        assert_eq!(as_timeouts.invalid_records, 1);
        assert_eq!(as_timeouts.count(TimeoutOperation::Key), 0);

        let timeouts = temporary_file("cross-timeout");
        record_timeout_at(&timeouts, TimeoutOperation::Key).expect("timeout record");
        let as_disconnects = read_disconnect_diagnostics(&timeouts).expect("disconnect read");
        assert_eq!(as_disconnects.valid_events, 0);
        assert_eq!(as_disconnects.invalid_records, 1);
        assert_eq!(
            as_disconnects.count(DisconnectReason::WriteContextObservationFailed),
            0
        );

        let _ = fs::remove_file(disconnects);
        let _ = fs::remove_file(timeouts);
    }

    #[test]
    fn disconnect_reasons_have_unique_codes_and_names() {
        // A count is only readable back to a call site while both the wire
        // code and the printed name identify exactly one reason.
        let mut codes: Vec<u16> = DisconnectReason::ALL
            .iter()
            .map(|reason| *reason as u16)
            .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), DisconnectReason::ALL.len());

        let mut names: Vec<&str> = DisconnectReason::ALL
            .iter()
            .map(|reason| reason.name())
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), DisconnectReason::ALL.len());

        for reason in DisconnectReason::ALL {
            assert_eq!(DisconnectReason::from_wire(reason as u16), Some(reason));
        }
        assert_eq!(DisconnectReason::from_wire(0), None);
        assert_eq!(
            DisconnectReason::from_wire(DisconnectReason::ALL.len() as u16 + 1),
            None
        );
    }

    #[test]
    fn a_corrupt_disconnect_record_is_visible_but_never_counted() {
        let path = temporary_file("disconnect-corrupt");
        record_disconnect_at(&path, DisconnectReason::ServiceDetached).expect("valid record");
        let mut bytes = fs::read(&path).expect("read");
        // Inside the checksummed prefix, and deliberately not the reason code:
        // a record can be corrupt while still naming a reason we recognise.
        bytes[20] ^= 0xff;
        bytes.extend_from_slice(b"tail");
        fs::write(&path, bytes).expect("corrupt fixture");

        let diagnostics = read_disconnect_diagnostics(&path).expect("diagnostics");
        assert_eq!(diagnostics.valid_events, 0);
        assert_eq!(diagnostics.invalid_records, 1);
        assert_eq!(diagnostics.ignored_tail_bytes, 4);
        assert_eq!(diagnostics.count(DisconnectReason::ServiceDetached), 0);
        let _ = fs::remove_file(path);
    }
}
