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
//!
//! The disconnect log keeps its newest records in a ring of the same byte
//! ceiling (`ring`, #313) instead of refusing new events once full: resets are
//! frequent enough to fill the append-only layout in weeks, after which it
//! could no longer answer any question about the present. One reason,
//! `cancelled-state-terminalized`, is reached from several triggers, so its
//! detail field carries a bounded [`CancellationTrigger`].

mod ring;

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Win32::System::Threading::GetCurrentThreadId;

pub use self::ring::Retention;

const MAGIC: &[u8; 4] = b"SKTO";
const MAGIC_DISCONNECT: &[u8; 4] = b"SKDC";
const FORMAT_VERSION: u16 = 1;
const RECORD_BYTES: usize = 32;
pub const MAX_TIMEOUT_LOG_BYTES: u64 = 1024 * 1024;
pub const MAX_DISCONNECT_LOG_BYTES: u64 = 1024 * 1024;

/// Value stored in a record's detail field when the writer had nothing to put
/// there: every record an older build wrote, and every disconnect record
/// except a classified `cancelled-state-terminalized` one.
///
/// Zero is not a possible measurement because [`encode_elapsed_ms`] floors a
/// real one at 1 ms, so a reader can tell "not measured" from "measured as
/// fast as this field can express" without a second format version.
const DETAIL_ABSENT: u32 = 0;

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
    /// Passive candidate-click polling. Older builds counted this as code 7,
    /// so historical ui-placement records cannot be split retrospectively.
    CandidatePoll = 11,
}

impl TimeoutOperation {
    pub const ALL: [Self; 11] = [
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
        Self::CandidatePoll,
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
            Self::CandidatePoll => "candidate-poll",
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

/// What made the frontend terminalize its cancelled writes and reset the link,
/// for the one reason several triggers share:
/// [`DisconnectReason::CancelledStateTerminalized`].
///
/// #309 needed to tell a host that replaced the document context from one that
/// refused the edit request, and the reason alone could not. The trigger is
/// stored in the record's detail field as one of these bounded values; no text,
/// context pointer, or free-form host error is ever stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationTrigger {
    /// The pending write's document context was replaced.
    ContextReplaced,
    /// The edit request was refused, and the class says how.
    Rejected(RejectionClass),
    /// Any other cancellation: a focus or lifecycle boundary, a revision
    /// mismatch, a stale callback, an unavailable engine.
    Other,
}

/// A refused edit request, reduced to a bounded class so a record never
/// carries a raw host error. The frontend maps the host's HRESULT onto these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RejectionClass {
    /// Refused with no host HRESULT: Sakura's own validation refused the
    /// write, or the request never reached the host.
    WithoutHresult = 1,
    /// The host could not grant the document lock.
    LockUnavailable = 2,
    /// The document is read-only.
    ReadOnly = 3,
    /// The context or the process behind it is gone.
    Disconnected = 4,
    /// `E_INVALIDARG`.
    InvalidArgument = 5,
    /// `E_UNEXPECTED` or `E_FAIL`.
    Unexpected = 6,
    /// Any other failure HRESULT.
    OtherFailure = 7,
}

/// Detail values. Zero stays [`DETAIL_ABSENT`], which is what every
/// `cancelled-state-terminalized` record written before #313 holds.
const TRIGGER_CONTEXT_REPLACED: u32 = 1;
const TRIGGER_OTHER: u32 = 2;
const TRIGGER_REJECTED: u32 = 0x100;

impl CancellationTrigger {
    pub const ALL: [Self; 9] = [
        Self::ContextReplaced,
        Self::Rejected(RejectionClass::WithoutHresult),
        Self::Rejected(RejectionClass::LockUnavailable),
        Self::Rejected(RejectionClass::ReadOnly),
        Self::Rejected(RejectionClass::Disconnected),
        Self::Rejected(RejectionClass::InvalidArgument),
        Self::Rejected(RejectionClass::Unexpected),
        Self::Rejected(RejectionClass::OtherFailure),
        Self::Other,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::ContextReplaced => "context-replaced",
            Self::Rejected(RejectionClass::WithoutHresult) => "rejected-without-hresult",
            Self::Rejected(RejectionClass::LockUnavailable) => "rejected-lock-unavailable",
            Self::Rejected(RejectionClass::ReadOnly) => "rejected-read-only",
            Self::Rejected(RejectionClass::Disconnected) => "rejected-disconnected",
            Self::Rejected(RejectionClass::InvalidArgument) => "rejected-invalid-argument",
            Self::Rejected(RejectionClass::Unexpected) => "rejected-unexpected",
            Self::Rejected(RejectionClass::OtherFailure) => "rejected-other-failure",
            Self::Other => "other",
        }
    }

    const fn detail(self) -> u32 {
        match self {
            Self::ContextReplaced => TRIGGER_CONTEXT_REPLACED,
            Self::Rejected(class) => TRIGGER_REJECTED | class as u32,
            Self::Other => TRIGGER_OTHER,
        }
    }

    fn from_detail(detail: u32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|trigger| trigger.detail() == detail)
    }

    const fn index(self) -> usize {
        match self {
            Self::ContextReplaced => 0,
            Self::Rejected(class) => class as usize,
            Self::Other => Self::ALL.len() - 1,
        }
    }
}

/// How long the operation had actually been waiting when its deadline
/// expired, summarised over every record that carried a measurement.
///
/// Without this a 51 ms expiry and a 1800 ms one are the same record, so the
/// counters cannot say whether the engine was marginally late or entirely
/// stalled. It is deliberately a summary rather than a per-event series: the
/// log stays a fixed-width bounded append, and nothing about *what* the user
/// typed can be reconstructed from three integers per operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ElapsedSummary {
    /// Records carrying a measurement. Builds before this field existed wrote
    /// none, so this can be below the operation's own count.
    pub samples: u64,
    pub total_ms: u64,
    pub max_ms: u32,
}

impl ElapsedSummary {
    pub fn mean_ms(&self) -> Option<u64> {
        (self.samples > 0).then(|| self.total_ms / self.samples)
    }

    fn observe(&mut self, elapsed_ms: u32) {
        self.samples = self.samples.saturating_add(1);
        self.total_ms = self.total_ms.saturating_add(u64::from(elapsed_ms));
        self.max_ms = self.max_ms.max(elapsed_ms);
    }
}

/// Clamps a measured wait into the record's detail field.
///
/// A real measurement is floored at 1 ms so it can never collide with
/// [`DETAIL_ABSENT`], and saturated at the top so a pathological wait is
/// reported as "at least this long" rather than wrapping into a small number.
fn encode_elapsed_ms(elapsed: Duration) -> u32 {
    u32::try_from(elapsed.as_millis())
        .unwrap_or(u32::MAX)
        .max(1)
}

/// Aggregate shown by settings and captured by dogfood evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeoutDiagnostics {
    counts: [u64; TimeoutOperation::ALL.len()],
    elapsed: [ElapsedSummary; TimeoutOperation::ALL.len()],
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
            elapsed: [ElapsedSummary::default(); TimeoutOperation::ALL.len()],
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

    /// Waiting time behind this operation's expiries. Empty when every record
    /// for it predates the field.
    pub fn elapsed(&self, operation: TimeoutOperation) -> ElapsedSummary {
        self.elapsed[operation.index()]
    }
}

/// Aggregate of engine-link resets, in the same shape as the timeout view so
/// settings can render both without a second presentation path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectDiagnostics {
    counts: [u64; DisconnectReason::ALL.len()],
    triggers: [u64; CancellationTrigger::ALL.len()],
    /// `cancelled-state-terminalized` records whose detail names a trigger
    /// this build does not know, so a newer build wrote them.
    pub unrecognized_triggers: u64,
    pub valid_events: u64,
    pub invalid_records: u64,
    pub ignored_tail_bytes: u64,
    pub first_timestamp_ms: Option<u64>,
    pub last_timestamp_ms: Option<u64>,
    pub retention: Retention,
}

impl Default for DisconnectDiagnostics {
    fn default() -> Self {
        Self {
            counts: [0; DisconnectReason::ALL.len()],
            triggers: [0; CancellationTrigger::ALL.len()],
            unrecognized_triggers: 0,
            valid_events: 0,
            invalid_records: 0,
            ignored_tail_bytes: 0,
            first_timestamp_ms: None,
            last_timestamp_ms: None,
            retention: Retention::Empty,
        }
    }
}

impl DisconnectDiagnostics {
    pub fn count(&self, reason: DisconnectReason) -> u64 {
        self.counts[reason.index()]
    }

    /// `cancelled-state-terminalized` records naming this trigger.
    pub fn trigger_count(&self, trigger: CancellationTrigger) -> u64 {
        self.triggers[trigger.index()]
    }

    /// `cancelled-state-terminalized` records with no trigger at all: written
    /// before #313, when that reason did not say which trigger reached it.
    /// They are reported as unclassified rather than guessed at.
    pub fn unclassified_triggers(&self) -> u64 {
        self.count(DisconnectReason::CancelledStateTerminalized)
            .saturating_sub(self.triggers.iter().sum())
            .saturating_sub(self.unrecognized_triggers)
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

/// Encodes one fixed-width record. Both logs and both layouts store exactly
/// this, so a change to the record format cannot land in one and miss another.
///
/// `magic` is what keeps the two logs from ever being read as each other: a
/// record written for one kind fails the header check in the reader for the
/// other, so even a misdirected write is counted as invalid rather than
/// silently inflating a counter that means something else.
fn encode_record(magic: &[u8; 4], code: u16, detail: u32) -> [u8; RECORD_BYTES] {
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
    // Bytes 24..28 carry the kind-specific detail: elapsed milliseconds for a
    // timeout, a `CancellationTrigger` for a cancelled-state reset, and
    // `DETAIL_ABSENT` for every other disconnect. The checksum below is taken
    // over the bytes actually written, and older records hold zeros there
    // under their own valid checksum, so populating this field neither
    // invalidates an existing log nor needs a new `FORMAT_VERSION`.
    record[24..28].copy_from_slice(&detail.to_le_bytes());
    let checksum = crc32(&record[..28]);
    record[28..].copy_from_slice(&checksum.to_le_bytes());
    record
}

/// Appends one fixed-width record to an append-only log, refusing once the
/// log reaches its hard byte ceiling.
fn append_event(
    path: &Path,
    magic: &[u8; 4],
    ceiling: u64,
    code: u16,
    detail: u32,
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
    let record = encode_record(magic, code, detail);

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
    observe_detail: impl FnMut(usize, u32),
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
    tally(
        bytes.chunks_exact(RECORD_BYTES),
        magic,
        &mut totals,
        counts,
        resolve,
        observe_detail,
    );
    Ok(totals)
}

/// Counts records into `counts` and `totals`, whichever layout held them.
fn tally<'a>(
    records: impl Iterator<Item = &'a [u8]>,
    magic: &[u8; 4],
    totals: &mut EventTotals,
    counts: &mut [u64],
    resolve: impl Fn(u16) -> Option<usize>,
    mut observe_detail: impl FnMut(usize, u32),
) {
    for record in records {
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
        let detail = u32::from_le_bytes(record[24..28].try_into().expect("fixed record"));
        if detail != DETAIL_ABSENT {
            observe_detail(index, detail);
        }
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
///
/// `elapsed` is how long the caller had been waiting when it gave up. It is
/// the wait itself, not anything about the request's content.
pub fn record_timeout(operation: TimeoutOperation, elapsed: Duration) -> io::Result<()> {
    record_timeout_at(&default_timeout_log_path()?, operation, elapsed)
}

pub fn record_timeout_at(
    path: &Path,
    operation: TimeoutOperation,
    elapsed: Duration,
) -> io::Result<()> {
    append_event(
        path,
        MAGIC,
        MAX_TIMEOUT_LOG_BYTES,
        operation as u16,
        encode_elapsed_ms(elapsed),
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
        |index, elapsed_ms| diagnostics.elapsed[index].observe(elapsed_ms),
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

/// Disconnect events this process failed to store. The next one it does store
/// adds them to the ring's drop counter, so a storage failure stays visible
/// without anything retrying the lost event.
static DISCONNECT_DROPS_PENDING: AtomicU64 = AtomicU64::new(0);

/// Best-effort public entry point for the frontend, under the same discipline
/// as `record_timeout`: a diagnostics failure must never turn a link reset,
/// which is already the recovery path, into a host-application failure.
///
/// A `cancelled-state-terminalized` reset goes through
/// [`record_cancelled_state`] instead, which keeps its trigger; recorded here
/// it would be reported as unclassified.
pub fn record_disconnect(reason: DisconnectReason) -> io::Result<()> {
    record_disconnect_at(&default_disconnect_log_path()?, reason)
}

pub fn record_disconnect_at(path: &Path, reason: DisconnectReason) -> io::Result<()> {
    append_disconnect(
        path,
        reason as u16,
        DETAIL_ABSENT,
        &DISCONNECT_DROPS_PENDING,
    )
}

/// Records a `cancelled-state-terminalized` reset with the trigger behind it.
pub fn record_cancelled_state(trigger: CancellationTrigger) -> io::Result<()> {
    record_cancelled_state_at(&default_disconnect_log_path()?, trigger)
}

pub fn record_cancelled_state_at(path: &Path, trigger: CancellationTrigger) -> io::Result<()> {
    append_disconnect(
        path,
        DisconnectReason::CancelledStateTerminalized as u16,
        trigger.detail(),
        &DISCONNECT_DROPS_PENDING,
    )
}

fn append_disconnect(
    path: &Path,
    code: u16,
    detail: u32,
    pending_drops: &AtomicU64,
) -> io::Result<()> {
    ring::append(
        path,
        MAGIC_DISCONNECT,
        MAX_DISCONNECT_LOG_BYTES,
        &encode_record(MAGIC_DISCONNECT, code, detail),
        "disconnect",
        pending_drops,
    )
}

pub fn read_disconnect_diagnostics(path: &Path) -> io::Result<DisconnectDiagnostics> {
    let snapshot = ring::read(
        path,
        MAGIC_DISCONNECT,
        MAX_DISCONNECT_LOG_BYTES,
        "disconnect",
    )?;
    let mut diagnostics = DisconnectDiagnostics {
        retention: snapshot.retention,
        ignored_tail_bytes: snapshot.ignored_tail_bytes,
        ..DisconnectDiagnostics::default()
    };
    let mut totals = EventTotals::default();
    let cancelled_state = DisconnectReason::CancelledStateTerminalized.index();
    tally(
        snapshot.records(),
        MAGIC_DISCONNECT,
        &mut totals,
        &mut diagnostics.counts,
        |code| DisconnectReason::from_wire(code).map(DisconnectReason::index),
        // Only a cancelled-state reset carries a detail. A legacy one holds
        // `DETAIL_ABSENT`, never reaches this closure, and is reported as
        // unclassified by subtraction.
        |index, detail| {
            if index != cancelled_state {
                return;
            }
            match CancellationTrigger::from_detail(detail) {
                Some(trigger) => {
                    diagnostics.triggers[trigger.index()] =
                        diagnostics.triggers[trigger.index()].saturating_add(1);
                }
                None => {
                    diagnostics.unrecognized_triggers =
                        diagnostics.unrecognized_triggers.saturating_add(1);
                }
            }
        },
    );
    diagnostics.valid_events = totals.valid_events;
    diagnostics.invalid_records = totals.invalid_records;
    diagnostics.first_timestamp_ms = totals.first_timestamp_ms;
    diagnostics.last_timestamp_ms = totals.last_timestamp_ms;
    Ok(diagnostics)
}

pub fn clear_disconnect_diagnostics(path: &Path) -> io::Result<()> {
    ring::clear(path, "disconnect")
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
        record_timeout_at(&path, TimeoutOperation::Key, Duration::from_millis(60))
            .expect("key timeout");
        record_timeout_at(&path, TimeoutOperation::Key, Duration::from_millis(120))
            .expect("second key timeout");
        record_timeout_at(
            &path,
            TimeoutOperation::Reconvert,
            Duration::from_millis(75),
        )
        .expect("reconversion timeout");

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
    fn waiting_time_is_summarised_per_operation() {
        let path = temporary_file("elapsed");
        record_timeout_at(&path, TimeoutOperation::Key, Duration::from_millis(60))
            .expect("first key timeout");
        record_timeout_at(&path, TimeoutOperation::Key, Duration::from_millis(1_800))
            .expect("second key timeout");
        record_timeout_at(&path, TimeoutOperation::Commit, Duration::from_millis(52))
            .expect("commit timeout");
        let result = read_timeout_diagnostics(&path);
        let _ = fs::remove_file(path);
        let diagnostics = result.expect("diagnostics");

        // The point of the field: two `key` records that used to be
        // indistinguishable now say that one was marginal and one was a stall.
        let key = diagnostics.elapsed(TimeoutOperation::Key);
        assert_eq!(key.samples, 2);
        assert_eq!(key.max_ms, 1_800);
        assert_eq!(key.total_ms, 1_860);
        assert_eq!(key.mean_ms(), Some(930));

        let commit = diagnostics.elapsed(TimeoutOperation::Commit);
        assert_eq!(commit.samples, 1);
        assert_eq!(commit.max_ms, 52);

        // An operation that never expired reports nothing rather than zero.
        assert_eq!(
            diagnostics.elapsed(TimeoutOperation::Connect),
            ElapsedSummary::default()
        );
        assert_eq!(
            diagnostics.elapsed(TimeoutOperation::Connect).mean_ms(),
            None
        );
    }

    #[test]
    fn a_sub_millisecond_wait_is_still_distinguishable_from_an_unmeasured_one() {
        let path = temporary_file("elapsed-floor");
        // Written the way a build from before this field wrote it: a valid
        // record whose detail bytes are zero, under its own valid checksum.
        append_event(
            &path,
            MAGIC,
            MAX_TIMEOUT_LOG_BYTES,
            TimeoutOperation::Key as u16,
            DETAIL_ABSENT,
            "timeout",
        )
        .expect("legacy record");
        record_timeout_at(&path, TimeoutOperation::Key, Duration::from_micros(400))
            .expect("sub-millisecond timeout");
        let result = read_timeout_diagnostics(&path);
        let _ = fs::remove_file(path);
        let diagnostics = result.expect("diagnostics");

        // Both records count. Only the measured one contributes a sample, so
        // an old log is reported as unmeasured instead of as instantaneous.
        assert_eq!(diagnostics.valid_events, 2);
        assert_eq!(diagnostics.invalid_records, 0);
        assert_eq!(diagnostics.count(TimeoutOperation::Key), 2);
        let key = diagnostics.elapsed(TimeoutOperation::Key);
        assert_eq!(key.samples, 1);
        assert_eq!(key.max_ms, 1);
    }

    #[test]
    fn an_implausibly_long_wait_saturates_rather_than_wrapping() {
        assert_eq!(
            encode_elapsed_ms(Duration::from_secs(60 * 60 * 24 * 365)),
            u32::MAX
        );
        assert_eq!(encode_elapsed_ms(Duration::ZERO), 1);
        assert_ne!(encode_elapsed_ms(Duration::ZERO), DETAIL_ABSENT);
    }

    #[test]
    fn candidate_poll_records_do_not_relabel_legacy_placement_records() {
        let path = temporary_file("candidate-poll");
        record_timeout_at(
            &path,
            TimeoutOperation::UiPlacement,
            Duration::from_millis(12),
        )
        .expect("legacy record");
        record_timeout_at(
            &path,
            TimeoutOperation::CandidatePoll,
            Duration::from_millis(11),
        )
        .expect("poll record");
        let result = read_timeout_diagnostics(&path);
        let _ = fs::remove_file(path);
        let diagnostics = result.expect("read both versions' operation codes");
        assert_eq!(TimeoutOperation::UiPlacement as u16, 7);
        assert_eq!(TimeoutOperation::CandidatePoll as u16, 11);
        assert_eq!(diagnostics.valid_events, 2);
        assert_eq!(diagnostics.invalid_records, 0);
        assert_eq!(diagnostics.count(TimeoutOperation::UiPlacement), 1);
        assert_eq!(diagnostics.count(TimeoutOperation::CandidatePoll), 1);
    }

    #[test]
    fn corrupt_and_torn_records_are_visible_but_never_counted() {
        let path = temporary_file("corrupt");
        record_timeout_at(&path, TimeoutOperation::Commit, Duration::from_millis(55))
            .expect("valid record");
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
        // The ring header, the record and every empty slot: nothing in a
        // disconnect ring passes as a timeout record.
        assert_eq!(
            as_timeouts.invalid_records,
            MAX_DISCONNECT_LOG_BYTES / RECORD_BYTES as u64
        );
        assert_eq!(as_timeouts.count(TimeoutOperation::Key), 0);

        let timeouts = temporary_file("cross-timeout");
        record_timeout_at(&timeouts, TimeoutOperation::Key, Duration::from_millis(60))
            .expect("timeout record");
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
        // Inside the checksummed prefix of the first slot, and deliberately
        // not the reason code: a record can be corrupt while still naming a
        // reason we recognise.
        bytes[RECORD_BYTES + 20] ^= 0xff;
        bytes.extend_from_slice(b"tail");
        fs::write(&path, bytes).expect("corrupt fixture");

        let diagnostics = read_disconnect_diagnostics(&path).expect("diagnostics");
        assert_eq!(diagnostics.valid_events, 0);
        assert_eq!(diagnostics.invalid_records, 1);
        assert_eq!(diagnostics.ignored_tail_bytes, 4);
        assert_eq!(diagnostics.count(DisconnectReason::ServiceDetached), 0);
        let _ = fs::remove_file(path);
    }

    fn legacy_disconnect_log(path: &Path, records: &[(DisconnectReason, u32)]) {
        let bytes: Vec<u8> = records
            .iter()
            .flat_map(|(reason, detail)| encode_record(MAGIC_DISCONNECT, *reason as u16, *detail))
            .collect();
        fs::write(path, bytes).expect("legacy fixture");
    }

    #[test]
    fn a_full_log_keeps_recording_the_newest_events_within_its_cap() {
        // The #313 failure: an append-only log that reached 1 MiB refused every
        // later event, so `diagnostics show` stopped answering for weeks.
        let path = temporary_file("disconnect-full");
        let capacity = MAX_DISCONNECT_LOG_BYTES / RECORD_BYTES as u64;
        // Distinct details keep the fixture's records distinct, as records
        // written at different times are; the reader ignores them here.
        let old: Vec<_> = (1..=capacity as u32)
            .map(|detail| (DisconnectReason::ServiceDetached, detail))
            .collect();
        legacy_disconnect_log(&path, &old);
        let before = read_disconnect_diagnostics(&path).expect("full legacy read");
        assert_eq!(
            before.retention,
            Retention::Legacy {
                reached_capacity: true
            }
        );
        assert_eq!(before.valid_events, capacity);

        let pending = AtomicU64::new(0);
        let newer = 100u64;
        for _ in 0..newer {
            append_disconnect(
                &path,
                DisconnectReason::KeyPredecessorFailed as u16,
                DETAIL_ABSENT,
                &pending,
            )
            .expect("an event after the log filled");
        }
        let after = read_disconnect_diagnostics(&path).expect("ring read");
        let len = fs::metadata(&path).expect("metadata").len();
        let _ = fs::remove_file(&path);

        let slots = capacity - 1;
        assert_eq!(len, MAX_DISCONNECT_LOG_BYTES);
        assert_eq!(
            after.retention,
            Retention::Ring {
                slots,
                recorded: slots + newer,
                dropped: 1
            }
        );
        assert_eq!(after.retention.overwritten(), newer);
        assert_eq!(after.valid_events, slots);
        assert_eq!(after.invalid_records, 0);
        assert_eq!(after.count(DisconnectReason::KeyPredecessorFailed), newer);
        assert_eq!(
            after.count(DisconnectReason::ServiceDetached),
            slots - newer
        );
    }

    #[test]
    fn cancellation_triggers_have_unique_bounded_details_and_names() {
        let mut details: Vec<u32> = CancellationTrigger::ALL
            .iter()
            .map(|trigger| trigger.detail())
            .collect();
        details.sort_unstable();
        details.dedup();
        assert_eq!(details.len(), CancellationTrigger::ALL.len());
        assert!(!details.contains(&DETAIL_ABSENT));

        let mut names: Vec<&str> = CancellationTrigger::ALL
            .iter()
            .map(|trigger| trigger.name())
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), CancellationTrigger::ALL.len());

        for (position, trigger) in CancellationTrigger::ALL.into_iter().enumerate() {
            assert_eq!(trigger.index(), position, "{}", trigger.name());
            assert_eq!(
                CancellationTrigger::from_detail(trigger.detail()),
                Some(trigger)
            );
        }
        assert_eq!(CancellationTrigger::from_detail(DETAIL_ABSENT), None);
        assert_eq!(CancellationTrigger::from_detail(TRIGGER_REJECTED), None);
        assert_eq!(CancellationTrigger::from_detail(TRIGGER_REJECTED | 8), None);
    }

    #[test]
    fn every_cancellation_trigger_is_counted_apart() {
        let path = temporary_file("disconnect-triggers");
        for (repeat, trigger) in CancellationTrigger::ALL.into_iter().enumerate() {
            for _ in 0..=repeat {
                record_cancelled_state_at(&path, trigger).expect("trigger record");
            }
        }
        let diagnostics = read_disconnect_diagnostics(&path).expect("diagnostics");
        let _ = fs::remove_file(&path);

        let total: u64 = (1..=CancellationTrigger::ALL.len() as u64).sum();
        assert_eq!(diagnostics.valid_events, total);
        assert_eq!(
            diagnostics.count(DisconnectReason::CancelledStateTerminalized),
            total
        );
        for (repeat, trigger) in CancellationTrigger::ALL.into_iter().enumerate() {
            assert_eq!(
                diagnostics.trigger_count(trigger),
                repeat as u64 + 1,
                "{}",
                trigger.name()
            );
        }
        assert_eq!(diagnostics.unclassified_triggers(), 0);
        assert_eq!(diagnostics.unrecognized_triggers, 0);
    }

    #[test]
    fn legacy_and_unknown_trigger_details_are_classified_conservatively() {
        let path = temporary_file("disconnect-legacy-triggers");
        legacy_disconnect_log(
            &path,
            &[
                // Written before #313: no trigger at all.
                (DisconnectReason::CancelledStateTerminalized, DETAIL_ABSENT),
                (DisconnectReason::CancelledStateTerminalized, DETAIL_ABSENT),
                // A detail on another reason is not a trigger.
                (DisconnectReason::ServiceDetached, TRIGGER_CONTEXT_REPLACED),
            ],
        );
        let legacy = read_disconnect_diagnostics(&path).expect("legacy read");
        assert_eq!(legacy.valid_events, 3);
        assert_eq!(legacy.unclassified_triggers(), 2);
        assert_eq!(
            legacy.trigger_count(CancellationTrigger::ContextReplaced),
            0
        );

        // A newer build's trigger this one cannot name, and a known one.
        let pending = AtomicU64::new(0);
        append_disconnect(
            &path,
            DisconnectReason::CancelledStateTerminalized as u16,
            TRIGGER_REJECTED | 0x80,
            &pending,
        )
        .expect("unknown trigger");
        record_cancelled_state_at(&path, CancellationTrigger::ContextReplaced)
            .expect("known trigger");
        let mixed = read_disconnect_diagnostics(&path).expect("converted read");
        let _ = fs::remove_file(&path);
        assert_eq!(mixed.valid_events, 5);
        assert_eq!(mixed.unclassified_triggers(), 2);
        assert_eq!(mixed.unrecognized_triggers, 1);
        assert_eq!(mixed.trigger_count(CancellationTrigger::ContextReplaced), 1);
        assert_eq!(
            mixed.retention,
            Retention::Ring {
                slots: MAX_DISCONNECT_LOG_BYTES / RECORD_BYTES as u64 - 1,
                recorded: 5,
                dropped: 0
            }
        );
    }
}
