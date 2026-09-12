//! Explicit developer-mode interaction history.
//!
//! This store is intentionally separate from `learning.rs` and
//! `event_log.rs`. Learning is a ranking input and may be compacted; the
//! engine event log is content-free diagnostics. This module is the opt-in
//! replay source for input/UX development and therefore keeps key events and
//! conversion commits together in their original record format.
//!
//! The key path only builds a bounded record and performs `try_send` into a
//! bounded writer queue. The writer owns all filesystem and DPAPI work. A
//! failure to enqueue or persist a record is observable through counters and
//! never changes the key reply.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_store::crypto::{DpapiSealer, Sealer};
use sakura_values::{AiTextOperation, AiTextStatus, InputScope};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_OPEN_REPARSE_POINT,
};

const MAGIC: &[u8; 4] = b"SKIH";
const HEADER_LEN: usize = 8;
const FRAME_HEADER_LEN: usize = 8;
const QUEUE_CAPACITY: usize = 1024;
const COMPACTION_INTERVAL: Duration = Duration::from_secs(60);
const COMPACTION_APPEND_LIMIT: u32 = 256;

pub const ENGINE_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
const VERSION_BUILD_ID_LENGTH: usize = 16;

/// Scope classification attached to every persisted record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScopeClass {
    Unclassified = 0,
    Normal = 1,
    Sensitive = 2,
}

impl From<ScopeClass> for HistoryScope {
    fn from(value: ScopeClass) -> Self {
        match value {
            ScopeClass::Unclassified => Self::Unclassified,
            ScopeClass::Normal => Self::Normal,
            ScopeClass::Sensitive => Self::Sensitive,
        }
    }
}

impl ScopeClass {
    /// Converts the engine's current scope plus the positive-classification
    /// bit into a history policy. URL, email, and digit fields are treated as
    /// sensitive because they commonly contain credentials or identifiers.
    pub fn from_scope(scope: InputScope, classified: bool) -> Self {
        if matches!(
            scope,
            InputScope::Password | InputScope::Url | InputScope::Email | InputScope::Digits
        ) {
            Self::Sensitive
        } else if classified {
            Self::Normal
        } else {
            Self::Unclassified
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Unclassified => "unclassified",
            Self::Normal => "normal",
            Self::Sensitive => "sensitive",
        }
    }
}

const fn ai_operation_name(operation: AiTextOperation) -> &'static str {
    match operation {
        AiTextOperation::Transform => "transform",
        AiTextOperation::Proofread => "proofread",
    }
}

const fn ai_status_name(status: AiTextStatus) -> &'static str {
    match status {
        AiTextStatus::Applied => "applied",
        AiTextStatus::Cancelled => "cancelled",
        AiTextStatus::Timeout => "timeout",
        AiTextStatus::MissingKey => "missing-key",
        AiTextStatus::WorkerError => "worker-error",
        AiTextStatus::ApiError => "api-error",
        AiTextStatus::Rejected => "rejected",
    }
}

use sakura_store::input_history::persistence::{
    compaction_transaction_path, exceeds_history_size,
    replace_history_file as store_replace_history_file, retained_records, retention_cutoff,
    RETENTION,
};
pub use sakura_store::input_history::persistence::{default_path, MAX_INPUT_HISTORY_BYTES};
pub use sakura_store::input_history::{
    AiTextHistoryRecord, CommitHistoryRecord, EngineHistoryRecord, HistoryScope,
    InputHistoryRecord, InputHistorySnapshot, InputHistoryStats, InputHistoryStatsSnapshot,
    KeyHistoryRecord, INPUT_HISTORY_FORMAT_VERSION, INPUT_HISTORY_FORMAT_VERSION_MIN,
    MAX_RECORD_BYTES,
};

pub trait InputHistorySnapshotExt {
    fn retain_current_records(&mut self, now_ms: u64);
    fn last_engine_identity(&self) -> Option<(&str, &str)>;
    fn to_tsv(&self) -> String;
}

impl InputHistorySnapshotExt for InputHistorySnapshot {
    /// Applies the public viewing/export retention policy without rewriting
    /// storage. Raw snapshots deliberately remain unfiltered for recovery and
    /// identifier accounting. `now_ms` also permits deterministic boundary tests.
    fn retain_current_records(&mut self, now_ms: u64) {
        let cutoff = retention_cutoff(now_ms);
        self.records
            .retain(|record| record.timestamp_ms() >= cutoff);
    }

    /// Latest engine identity marker in the snapshot, if any.
    fn last_engine_identity(&self) -> Option<(&str, &str)> {
        self.records.iter().rev().find_map(|record| match record {
            InputHistoryRecord::Engine(record) => Some((
                record.package_version.as_str(),
                record.release_label.as_str(),
            )),
            _ => None,
        })
    }

    fn to_tsv(&self) -> String {
        let (package_version, release_label) = self.last_engine_identity().unwrap_or(("-", "-"));
        let mut output = format!(
            "# sakura-input-history-format: {}\n\
# package-version: {package_version}\n\
# release-label: {release_label}\n\
# records are DPAPI-protected on disk\n\
kind\tsequence\ttimestamp-ms\tsession\tscope\tkey-code\tcharacter\tmodifiers\t\
repeat\tconsumed\tstate-before\tstate-after\tmode-before\tmode-after\tpreedit-before\t\
preedit-after\tcommit\tdelete-before\tbeep\taction\tdropped-before\treading\tsurface\t\
left-context\tright-context\tai-operation\tai-status\tai-source\tai-result\tai-model\t\
ai-provider\tai-style\tai-error-code\tai-latency-ms\tai-input-tokens\tai-output-tokens\t\
ai-cached-tokens\tai-http-attempts\tengine-package-version\tengine-release-label\n",
            self.format_version
        );
        for record in &self.records {
            match record {
                InputHistoryRecord::Key(record) => {
                    let mut fields = vec![
                        "key".to_owned(),
                        record.sequence.to_string(),
                        record.timestamp_ms.to_string(),
                        record.session.to_string(),
                        record.scope.name().to_owned(),
                        record.key_code.to_string(),
                        record
                            .character
                            .map_or_else(String::new, |c| escape(&c.to_string())),
                        record.modifiers.to_string(),
                        record.repeat.to_string(),
                        record.consumed.to_string(),
                        record.state_before.to_string(),
                        record.state_after.to_string(),
                        record.mode_before.to_string(),
                        record.mode_after.to_string(),
                        escape(&record.preedit_before),
                        escape(&record.preedit_after),
                        escape(&record.commit),
                        record.delete_before.to_string(),
                        record.beep.to_string(),
                        escape(&record.action),
                        record.dropped_before.to_string(),
                    ];
                    fields.extend((0..17).map(|_| String::new()));
                    fields.push(String::new());
                    fields.push(String::new());
                    output.push_str(&fields.join("\t"));
                    output.push('\n');
                }
                InputHistoryRecord::Commit(record) => {
                    let mut fields = vec![
                        "commit".to_owned(),
                        record.sequence.to_string(),
                        record.timestamp_ms.to_string(),
                        record.session.to_string(),
                        record.scope.name().to_owned(),
                    ];
                    fields.extend((0..16).map(|_| String::new()));
                    fields.extend([
                        escape(&record.reading),
                        escape(&record.surface),
                        record.left_context.to_string(),
                        record.right_context.to_string(),
                    ]);
                    fields.extend((0..13).map(|_| String::new()));
                    fields.push(String::new());
                    fields.push(String::new());
                    output.push_str(&fields.join("\t"));
                    output.push('\n');
                }
                InputHistoryRecord::AiText(record) => {
                    let mut fields = vec![
                        "ai-text".to_owned(),
                        record.sequence.to_string(),
                        record.timestamp_ms.to_string(),
                        record.session.to_string(),
                        record.scope.name().to_owned(),
                    ];
                    fields.extend((0..20).map(|_| String::new()));
                    fields.extend([
                        ai_operation_name(record.operation).to_owned(),
                        ai_status_name(record.status).to_owned(),
                        escape(&record.source),
                        escape(&record.result),
                        escape(&record.model),
                        escape(&record.provider),
                        escape(&record.style),
                        escape(&record.error_code),
                        record.latency_ms.to_string(),
                        record.input_tokens.to_string(),
                        record.output_tokens.to_string(),
                        record.cached_tokens.to_string(),
                        record.attempts.to_string(),
                    ]);
                    fields.push(String::new());
                    fields.push(String::new());
                    output.push_str(&fields.join("\t"));
                    output.push('\n');
                }
                InputHistoryRecord::Engine(record) => {
                    let mut fields = vec![
                        "engine".to_owned(),
                        record.sequence.to_string(),
                        record.timestamp_ms.to_string(),
                        record.session.to_string(),
                        record.scope.name().to_owned(),
                    ];
                    fields.extend((0..33).map(|_| String::new()));
                    fields.push(escape(&record.package_version));
                    fields.push(escape(&record.release_label));
                    output.push_str(&fields.join("\t"));
                    output.push('\n');
                }
            }
        }
        output
    }
}

fn excludes(stats: &InputHistoryStats, scope: ScopeClass, test_only: bool) -> bool {
    if test_only {
        stats.record_excluded_test_only();
        return true;
    }
    match scope {
        ScopeClass::Normal => false,
        ScopeClass::Unclassified => {
            stats.record_excluded_unclassified();
            true
        }
        ScopeClass::Sensitive => {
            stats.record_excluded_sensitive();
            true
        }
    }
}

enum Command {
    Append {
        epoch: u64,
        timestamp_ms: u64,
        payload: Vec<u8>,
    },
    Flush {
        reply: mpsc::Sender<io::Result<()>>,
    },
    Clear {
        epoch: u64,
        reply: mpsc::Sender<io::Result<u64>>,
    },
    Shutdown {
        reply: mpsc::Sender<io::Result<()>>,
    },
}

/// Process-wide developer history service shared by all pipe workers.
pub struct InputHistoryService {
    path: PathBuf,
    sender: SyncSender<Command>,
    stats: Arc<InputHistoryStats>,
    next_sequence: AtomicU64,
    next_session_id: AtomicU64,
    epoch: AtomicU64,
    worker: Mutex<WriterShutdown>,
}

/// Only stop callers take this mutex. Key-path admission never waits for it.
struct WriterShutdown {
    handle: Option<JoinHandle<()>>,
    outcome: Option<Result<(), ShutdownFailure>>,
}

struct ShutdownFailure {
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
    message: String,
}

impl ShutdownFailure {
    fn capture(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
            message: error.to_string(),
        }
    }

    fn error(&self) -> io::Error {
        self.raw_os_error.map_or_else(
            || io::Error::new(self.kind, self.message.clone()),
            io::Error::from_raw_os_error,
        )
    }
}

impl fmt::Debug for InputHistoryService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stats = self.stats.snapshot();
        formatter
            .debug_struct("InputHistoryService")
            .field("path", &self.path)
            .field("dropped_events", &self.stats.dropped_events())
            .field("persistence_failures", &self.stats.persistence_failures())
            .field(
                "excluded_unclassified_events",
                &stats.excluded_unclassified_events,
            )
            .field(
                "excluded_sensitive_events",
                &stats.excluded_sensitive_events,
            )
            .field(
                "excluded_test_only_events",
                &stats.excluded_test_only_events,
            )
            .finish()
    }
}

impl InputHistoryService {
    pub fn open(path: &Path) -> io::Result<Arc<Self>> {
        let store_owner = acquire_store_owner(path)?;
        ensure_file(path)?;
        let recovered = repair_file(path)?;
        if recovered.last_sequence == u64::MAX || recovered.last_session == u64::MAX {
            return Err(invalid_data("input history stored identifiers exhausted"));
        }
        // Legacy migration is the explicit exception to no startup rewrite:
        // do not append current-format markers beneath a legacy header.
        // Recover IDs before migration can remove expired records.
        let retention = if recovered.format_version != INPUT_HISTORY_FORMAT_VERSION {
            compact_file(path)?
        } else {
            recovered.retention
        };
        // Complete every fallible mandatory step before starting a worker.
        // Retention belongs to viewing/maintenance, not ID recovery: expired
        // records still carry high-watermarks in this existing store.
        let append_file = open_append(path)?;

        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let stats = Arc::new(InputHistoryStats::default());
        let writer_stats = Arc::clone(&stats);
        let writer_path = path.to_owned();
        let worker = thread::Builder::new()
            .name("sakura-input-history".to_owned())
            .spawn(move || {
                // Ownership follows the actual worker, including a delayed
                // exit or unwind, rather than a caller's stop observation.
                let _store_owner = store_owner;
                writer_loop(writer_path, receiver, writer_stats, append_file, retention);
            })
            .map_err(|error| io::Error::other(format!("start input history writer: {error}")))?;

        let service = Arc::new(Self {
            path: path.to_owned(),
            sender,
            stats,
            next_sequence: AtomicU64::new(recovered.last_sequence),
            next_session_id: AtomicU64::new(recovered.last_session),
            epoch: AtomicU64::new(0),
            worker: Mutex::new(WriterShutdown {
                handle: Some(worker),
                outcome: None,
            }),
        });
        service.record_engine_start();
        Ok(service)
    }

    pub fn path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }

    pub fn stats(&self) -> &InputHistoryStats {
        &self.stats
    }

    /// Allocates an ID shared by every dispatcher using this process-wide
    /// history service. The ordinary protocol session ID is local to one pipe
    /// worker and can otherwise collide in a multi-client history stream.
    /// Returns None on exhaustion; it never wraps or repeats the final ID.
    pub fn allocate_session_id(&self) -> Option<u64> {
        self.allocate_counter(&self.next_session_id, Ordering::Relaxed)
    }

    fn allocate_counter(&self, counter: &AtomicU64, order: Ordering) -> Option<u64> {
        match counter.fetch_update(order, Ordering::Relaxed, |value| value.checked_add(1)) {
            Ok(previous) => Some(previous + 1),
            Err(_) => {
                self.stats.record_persistence_failure();
                None
            }
        }
    }

    // Zero is reserved for engine markers and unavailable history sessions.
    // Ordinary protocol input can continue after history allocation fails.
    fn excludes_session(&self, session: u64) -> bool {
        if session == 0 {
            self.stats.record_drop();
            true
        } else {
            false
        }
    }

    /// Records which engine build is about to append developer-history events.
    ///
    /// Called once when the service starts. Not subject to scope exclusion:
    /// this marker contains only package/release identity, never key content.
    fn record_engine_start(&self) {
        let epoch = self.epoch.load(Ordering::Acquire);
        let (package_version, release_label) = current_engine_identity();
        let Some(sequence) = self.allocate_counter(&self.next_sequence, Ordering::Relaxed) else {
            return;
        };
        self.enqueue(
            epoch,
            InputHistoryRecord::Engine(EngineHistoryRecord {
                sequence,
                timestamp_ms: now_ms(),
                session: 0,
                scope: HistoryScope::Normal,
                package_version,
                release_label,
            }),
        );
    }

    fn enqueue(&self, epoch: u64, record: InputHistoryRecord) {
        #[cfg(test)]
        tests::BEFORE_ENQUEUE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let timestamp_ms = record.timestamp_ms();
        let Ok(payload) = record.encode() else {
            self.stats.record_persistence_failure();
            return;
        };
        let command = Command::Append {
            epoch,
            timestamp_ms,
            payload,
        };
        match self.sender.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.stats.record_drop();
            }
            Err(TrySendError::Disconnected(_)) => {
                self.stats.record_persistence_failure();
            }
        }
    }

    /// Enqueues a key event without performing disk I/O or waiting for the
    /// writer. Only positively classified normal scopes are recorded. Both
    /// sensitive and unclassified scopes are rejected here as a second
    /// defense even when a caller accidentally bypasses the dispatcher guard.
    #[allow(clippy::too_many_arguments)]
    pub fn record_key(
        &self,
        session: u64,
        scope: ScopeClass,
        key_code: u16,
        character: Option<char>,
        modifiers: u8,
        repeat: bool,
        test_only: bool,
        consumed: bool,
        state_before: u8,
        state_after: u8,
        mode_before: u8,
        mode_after: u8,
        preedit_before: &str,
        preedit_after: &str,
        commit: &str,
        delete_before: u16,
        beep: bool,
        action: &str,
    ) {
        let epoch = self.epoch.load(Ordering::Acquire);
        if excludes(&self.stats, scope, test_only) {
            return;
        }
        if self.excludes_session(session) {
            return;
        }
        let Some(sequence) = self.allocate_counter(&self.next_sequence, Ordering::Relaxed) else {
            return;
        };
        // Keep this cumulative. A swap before encoding/enqueueing loses the
        // count when the record itself is malformed or the bounded queue is
        // full. The live stats endpoint and every later record remain able to
        // explain what was dropped.
        let dropped_before = self.stats.dropped_events();
        let record = InputHistoryRecord::Key(KeyHistoryRecord {
            sequence,
            timestamp_ms: now_ms(),
            session,
            scope: scope.into(),
            key_code,
            character,
            modifiers,
            repeat,
            consumed,
            state_before,
            state_after,
            mode_before,
            mode_after,
            preedit_before: preedit_before.to_owned(),
            preedit_after: preedit_after.to_owned(),
            commit: commit.to_owned(),
            delete_before,
            beep,
            action: action.to_owned(),
            dropped_before,
        });
        self.enqueue(epoch, record);
    }

    pub fn record_commit(
        &self,
        session: u64,
        scope: ScopeClass,
        reading: &str,
        surface: &str,
        left_context: u16,
        right_context: u16,
    ) {
        let epoch = self.epoch.load(Ordering::Acquire);
        if excludes(&self.stats, scope, false) || reading.is_empty() || surface.is_empty() {
            return;
        }
        if self.excludes_session(session) {
            return;
        }
        let Some(sequence) = self.allocate_counter(&self.next_sequence, Ordering::Relaxed) else {
            return;
        };
        let record = InputHistoryRecord::Commit(CommitHistoryRecord {
            sequence,
            timestamp_ms: now_ms(),
            session,
            scope: scope.into(),
            reading: reading.to_owned(),
            surface: surface.to_owned(),
            left_context,
            right_context,
        });
        self.enqueue(epoch, record);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_ai_text(
        &self,
        session: u64,
        scope: ScopeClass,
        operation: AiTextOperation,
        status: AiTextStatus,
        source: &str,
        result: &str,
        model: &str,
        provider: &str,
        style: &str,
        error_code: &str,
        latency_ms: u64,
        input_tokens: u32,
        output_tokens: u32,
        cached_tokens: u32,
        attempts: u32,
        test_only: bool,
    ) {
        let epoch = self.epoch.load(Ordering::Acquire);
        if excludes(&self.stats, scope, test_only) || source.is_empty() {
            return;
        }
        if self.excludes_session(session) {
            return;
        }
        self.stats
            .record_ai_usage(attempts, input_tokens, output_tokens, cached_tokens);
        let Some(sequence) = self.allocate_counter(&self.next_sequence, Ordering::Relaxed) else {
            return;
        };
        let record = InputHistoryRecord::AiText(AiTextHistoryRecord {
            sequence,
            timestamp_ms: now_ms(),
            session,
            scope: scope.into(),
            operation,
            status,
            source: source.to_owned(),
            result: result.to_owned(),
            model: model.to_owned(),
            provider: provider.to_owned(),
            style: style.to_owned(),
            error_code: error_code.to_owned(),
            latency_ms,
            input_tokens,
            output_tokens,
            cached_tokens,
            attempts,
        });
        self.enqueue(epoch, record);
    }

    /// FIFO barrier for preceding accepted appends, followed by explicit file
    /// synchronization. Does not compact; prior queued maintenance can still
    /// delay completion. Admission drops remain separately observable in stats.
    pub fn flush(&self) -> io::Result<()> {
        let (reply, receiver) = mpsc::channel();
        self.sender.send(Command::Flush { reply }).map_err(|_| {
            self.stats.record_persistence_failure();
            io::Error::new(io::ErrorKind::BrokenPipe, "input history writer stopped")
        })?;
        receiver.recv().map_err(|_| {
            self.stats.record_persistence_failure();
            io::Error::new(io::ErrorKind::BrokenPipe, "input history writer stopped")
        })?
    }

    /// Clears all queued and durable records. Epoch tagging prevents a record
    /// that raced with the clear command from being written afterwards.
    pub fn clear(&self) -> io::Result<u64> {
        let epoch = self
            .allocate_counter(&self.epoch, Ordering::AcqRel)
            .ok_or_else(|| io::Error::other("input history Clear epoch exhausted"))?;
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(Command::Clear { epoch, reply })
            .map_err(|_| {
                self.stats.record_persistence_failure();
                io::Error::new(io::ErrorKind::BrokenPipe, "input history writer stopped")
            })?;
        receiver.recv().map_err(|_| {
            self.stats.record_persistence_failure();
            io::Error::new(io::ErrorKind::BrokenPipe, "input history writer stopped")
        })?
    }

    pub fn stop(&self) -> io::Result<()> {
        // One caller sends Shutdown and joins; duplicates wait for and replay
        // the same terminal outcome. A lost reply must never skip the join.
        let mut shutdown = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(outcome) = &shutdown.outcome {
            return outcome.as_ref().copied().map_err(ShutdownFailure::error);
        }
        let (reply, receiver) = mpsc::channel();
        let send = self.sender.send(Command::Shutdown { reply });
        let mut result = match send {
            Ok(()) => receiver.recv().unwrap_or_else(|_| {
                self.stats.record_persistence_failure();
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "input history writer stopped",
                ))
            }),
            Err(_) => {
                self.stats.record_persistence_failure();
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "input history writer stopped",
                ))
            }
        };
        if let Some(worker) = shutdown.handle.take() {
            if worker.join().is_err() {
                self.stats.record_persistence_failure();
                result = Err(io::Error::other("input history writer panicked"));
            }
        }
        shutdown.outcome = Some(result.map_err(ShutdownFailure::capture));
        shutdown
            .outcome
            .as_ref()
            .expect("joined terminal outcome")
            .as_ref()
            .copied()
            .map_err(ShutdownFailure::error)
    }
}

impl Drop for InputHistoryService {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub fn read_snapshot(path: &Path) -> io::Result<InputHistorySnapshot> {
    require_no_compaction_transaction(path)?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_INPUT_HISTORY_BYTES {
        return Err(invalid_data("input history exceeds its hard size bound"));
    }
    let bytes = fs::read(path)?;
    scan_snapshot(&bytes)
}

pub fn clear_path(path: &Path) -> io::Result<u64> {
    let _store_owner = acquire_store_owner(path)?;
    clear_owned_path(path)
}

/// Caller already owns the separate writer lock (or an isolated unit fixture).
fn clear_owned_path(path: &Path) -> io::Result<u64> {
    require_no_compaction_transaction(path)?;
    if !path.exists() {
        return Ok(0);
    }
    let before = read_snapshot(path)
        .map(|snapshot| snapshot.records.len() as u64)
        .unwrap_or(0);
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header())?;
    file.flush()?;
    Ok(before)
}

/// The lock object is independent of the replaceable history file. Windows
/// denies other opens/removal while this exclusive handle exists and removes
/// the lock only after its final handle closes, including process termination.
/// This protocol coordinates updated writers using the same logical path.
fn acquire_store_owner(path: &Path) -> io::Result<File> {
    let mut name = path
        .file_name()
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "history path has no file name")
        })?
        .to_os_string();
    name.push(".writer.lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        // Never attach delete-on-close to a preexisting object (including a
        // reparse target). A surviving lock after OS/storage failure requires
        // explicit recovery, rather than deleting an object we did not create.
        .create_new(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path.with_file_name(name))
}

fn writer_loop(
    path: PathBuf,
    receiver: Receiver<Command>,
    stats: Arc<InputHistoryStats>,
    file: File,
    retention: RetentionPlan,
) {
    writer_loop_with_file(
        path,
        receiver,
        stats,
        Some(file),
        COMPACTION_INTERVAL,
        retention,
    );
}

#[cfg(test)]
fn writer_loop_with_interval(
    path: PathBuf,
    receiver: Receiver<Command>,
    stats: Arc<InputHistoryStats>,
    compaction_interval: Duration,
) {
    let file = open_append(&path).ok();
    let snapshot = read_snapshot(&path).expect("validated synthetic worker fixture");
    let mut retention = RetentionPlan::default();
    for record in snapshot.records {
        retention.observe(record.timestamp_ms());
    }
    writer_loop_with_file(path, receiver, stats, file, compaction_interval, retention);
}

fn writer_loop_with_file(
    path: PathBuf,
    receiver: Receiver<Command>,
    stats: Arc<InputHistoryStats>,
    mut file: Option<File>,
    compaction_interval: Duration,
    mut retention: RetentionPlan,
) {
    let mut cleared_epoch = 0;
    let mut last_compaction = Instant::now();
    let mut appends_since_compaction = 0u32;
    // A later successful sync cannot recover an earlier failed append. Keep
    // that loss observable at barriers until a successful explicit Clear.
    let mut append_failed = false;
    loop {
        let wait = compaction_interval.saturating_sub(last_compaction.elapsed());
        let command = match receiver.recv_timeout(wait) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => {
                if retention.is_due(now_ms()) {
                    match compact_writer_file(&path, &mut file) {
                        Ok(updated) => retention = updated,
                        Err(_) => {
                            stats.record_persistence_failure();
                        }
                    }
                }
                appends_since_compaction = 0;
                last_compaction = Instant::now();
                #[cfg(test)]
                tests::after_maintenance_check();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match command {
            Command::Append {
                epoch,
                timestamp_ms,
                payload,
            } => {
                if epoch < cleared_epoch {
                    continue;
                }
                let result =
                    append_payload(&path, &mut file, &payload, timestamp_ms, &mut retention);
                match result {
                    Ok(()) => {
                        appends_since_compaction = appends_since_compaction.saturating_add(1);
                        if appends_since_compaction >= COMPACTION_APPEND_LIMIT
                            || last_compaction.elapsed() >= COMPACTION_INTERVAL
                        {
                            if retention.is_due(now_ms()) {
                                match compact_writer_file(&path, &mut file) {
                                    Ok(updated) => retention = updated,
                                    Err(_) => {
                                        stats.record_persistence_failure();
                                    }
                                }
                            }
                            appends_since_compaction = 0;
                            last_compaction = Instant::now();
                        }
                    }
                    Err(_) => {
                        append_failed = true;
                        stats.record_persistence_failure();
                    }
                }
            }
            Command::Flush { reply } => {
                let result = sync_writer_file(&file, append_failed);
                if result.is_err() {
                    stats.record_persistence_failure();
                }
                let _ = reply.send(result);
            }
            Command::Clear { epoch, reply } => {
                cleared_epoch = cleared_epoch.max(epoch);
                let result = clear_writer_file(&path, &mut file);
                if result.is_err() {
                    stats.record_persistence_failure();
                } else {
                    append_failed = false;
                    retention = RetentionPlan::default();
                }
                let _ = reply.send(result);
                appends_since_compaction = 0;
                last_compaction = Instant::now();
            }
            Command::Shutdown { reply } => {
                let result = sync_writer_file(&file, append_failed);
                if result.is_err() {
                    stats.record_persistence_failure();
                }
                let _ = reply.send(result);
                break;
            }
        }
    }
}

fn sync_writer_file(file: &Option<File>, append_failed: bool) -> io::Result<()> {
    let file = file.as_ref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "input history writer has no file",
        )
    })?;
    file.sync_all()?;
    if append_failed {
        return Err(io::Error::other("input history preceding append failed"));
    }
    Ok(())
}

/// Compacts under store ownership after closing this writer's append handle.
/// Errors before a transaction can reopen the verified canonical; unresolved
/// publication leaves the handle absent and every later open fails closed.
fn compact_writer_file(path: &Path, file: &mut Option<File>) -> io::Result<RetentionPlan> {
    if let Some(mut previous) = file.take() {
        previous.flush()?;
    }
    let compaction = compact_file(path);
    let reopened = open_append(path);
    match (compaction, reopened) {
        (Ok(retention), Ok(handle)) => {
            *file = Some(handle);
            Ok(retention)
        }
        (Err(error), Ok(handle)) => {
            *file = Some(handle);
            Err(error)
        }
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(_)) => Err(error),
    }
}

fn append_payload(
    path: &Path,
    file: &mut Option<File>,
    payload: &[u8],
    timestamp_ms: u64,
    retention: &mut RetentionPlan,
) -> io::Result<()> {
    if payload.len() > MAX_RECORD_BYTES {
        return Err(invalid_data("input history record is too large"));
    }
    let protected = protect(payload)?;
    let frame_len = FRAME_HEADER_LEN as u64 + protected.len() as u64;
    let current_len = file
        .as_ref()
        .and_then(|handle| handle.metadata().ok())
        .map_or(0, |metadata| metadata.len());
    if exceeds_history_size(current_len, frame_len) {
        if let Some(mut previous) = file.take() {
            previous.flush()?;
        }
        *retention = compact_file(path)?;
        *file = Some(open_append(path)?);
    }
    if file.is_none() {
        *file = Some(open_append(path)?);
    }
    let file = file.as_mut().expect("input history writer opened the file");
    let current_len = file.metadata()?.len();
    if exceeds_history_size(current_len, frame_len) {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "input history retention limit reached",
        ));
    }
    // An I/O error can follow a complete write. Track possible expiry once
    // writing begins, but do not schedule maintenance for pre-write rejection.
    retention.observe(timestamp_ms);
    append_encrypted(file, &protected)?;
    #[cfg(test)]
    if tests::FAIL_AFTER_APPEND.with(|fail| fail.replace(false)) {
        return Err(io::Error::other("synthetic post-write failure"));
    }
    Ok(())
}

fn append_encrypted(file: &mut File, protected: &[u8]) -> io::Result<()> {
    let length = u32::try_from(protected.len())
        .map_err(|_| invalid_data("protected input history record is too large"))?;
    file.write_all(&length.to_le_bytes())?;
    file.write_all(&crc32(protected).to_le_bytes())?;
    file.write_all(protected)?;
    file.flush()
}

fn clear_writer_file(path: &Path, file: &mut Option<File>) -> io::Result<u64> {
    require_no_compaction_transaction(path)?;
    if let Some(previous) = file.as_mut() {
        previous.flush()?;
    }
    let cleared = read_snapshot(path)
        .map(|snapshot| snapshot.records.len() as u64)
        .unwrap_or(0);
    let _ = file.take();
    let mut replacement = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    replacement.write_all(&header())?;
    replacement.flush()?;
    *file = Some(replacement);
    Ok(cleared)
}

fn ensure_file(path: &Path) -> io::Result<()> {
    require_no_compaction_transaction(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    if file.metadata()?.len() == 0 {
        file.write_all(&header())?;
        file.flush()?;
    }
    Ok(())
}

fn open_append(path: &Path) -> io::Result<File> {
    ensure_file(path)?;
    OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
}

fn repair_file(path: &Path) -> io::Result<ScanSummary> {
    // Every append enforces MAX_INPUT_HISTORY_BYTES, so bytes past the cap
    // can only be corruption or external tampering. Reading is bounded to
    // the cap so an oversized file cannot force an unbounded allocation at
    // engine startup; the scan then truncates to the last valid frame,
    // which also discards the entire over-cap tail.
    let file_len = fs::metadata(path)?.len();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_INPUT_HISTORY_BYTES)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        ensure_file(path)?;
        return scan_frames(&header(), None);
    }
    // Recover IDs during the same validation pass; do not materialize a full
    // record vector or decrypt the file again merely to recover two maxima.
    let summary = scan_frames(&bytes, None)?;
    if (summary.valid_end as u64) < file_len {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(summary.valid_end as u64)?;
        file.sync_all()?;
    }
    Ok(summary)
}

#[derive(Debug)]
struct ScanSummary {
    format_version: u16,
    valid_end: usize,
    last_sequence: u64,
    last_session: u64,
    retention: RetentionPlan,
}

#[derive(Debug, Default, Clone, Copy)]
struct RetentionPlan {
    oldest_timestamp_ms: Option<u64>,
}

impl RetentionPlan {
    fn observe(&mut self, timestamp_ms: u64) {
        self.oldest_timestamp_ms = Some(
            self.oldest_timestamp_ms
                .map_or(timestamp_ms, |old| old.min(timestamp_ms)),
        );
    }

    fn is_due(self, now: u64) -> bool {
        self.oldest_timestamp_ms
            .is_some_and(|oldest| oldest < now.saturating_sub(RETENTION.as_millis() as u64))
    }
}

fn scan_snapshot(bytes: &[u8]) -> io::Result<InputHistorySnapshot> {
    let (format_version, records, valid_end) = scan_bytes(bytes)?;
    Ok(InputHistorySnapshot {
        format_version,
        records,
        ignored_tail_bytes: bytes.len().saturating_sub(valid_end),
    })
}

fn scan_bytes(bytes: &[u8]) -> io::Result<(u16, Vec<InputHistoryRecord>, usize)> {
    let mut records = Vec::new();
    let summary = scan_frames(bytes, Some(&mut records))?;
    Ok((summary.format_version, records, summary.valid_end))
}

/// Walks the frames and returns the offset one past the last valid one.
///
/// `records` is where the decoded records go when the caller wants them.
/// Passing `None` decodes and drops each one instead, which is what
/// [`repair_file`] needs: it uses only the boundary/ID summary, and materializing a
/// 64 MiB file's worth of records — hundreds of megabytes of `String`s —
/// just to ask where the damage starts is an allocation spike at engine
/// startup for an answer nobody reads. Complete checksum-valid frames whose
/// content cannot be decrypted or decoded return an error: an unavailable
/// key or unsupported record is not evidence of a torn tail. In particular,
/// repair must not truncate opaque data or silently discard later frames.
fn scan_frames(
    bytes: &[u8],
    mut records: Option<&mut Vec<InputHistoryRecord>>,
) -> io::Result<ScanSummary> {
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err(invalid_data("invalid input history header"));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if !(INPUT_HISTORY_FORMAT_VERSION_MIN..=INPUT_HISTORY_FORMAT_VERSION).contains(&version) {
        return Err(invalid_data("unsupported input history format"));
    }
    let mut offset = HEADER_LEN;
    let mut last_sequence = 0;
    let mut last_session = 0;
    let mut retention = RetentionPlan::default();
    while offset + FRAME_HEADER_LEN <= bytes.len() {
        let length = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let expected_crc = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let payload_start = offset + FRAME_HEADER_LEN;
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
        let payload = unprotect(encrypted)?;
        let record = InputHistoryRecord::decode(&payload)?;
        last_sequence = last_sequence.max(record.sequence());
        last_session = last_session.max(record.session());
        retention.observe(record.timestamp_ms());
        if let Some(records) = records.as_deref_mut() {
            records.push(record);
        }
        offset = payload_end;
    }
    Ok(ScanSummary {
        format_version: version,
        valid_end: offset,
        last_sequence,
        last_session,
        retention,
    })
}

fn compact_file(path: &Path) -> io::Result<RetentionPlan> {
    let snapshot = match read_snapshot(path) {
        Ok(snapshot) => snapshot,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(RetentionPlan::default())
        }
        Err(error) => return Err(error),
    };
    let records = retained_records(snapshot.records, now_ms());
    let mut encoded = Vec::with_capacity(records.len());
    let mut total = HEADER_LEN as u64;
    let mut retention = RetentionPlan::default();
    for record in records.into_iter().rev() {
        let payload = record.encode()?;
        let protected = protect(&payload)?;
        let frame_len = FRAME_HEADER_LEN as u64 + protected.len() as u64;
        if exceeds_history_size(total, frame_len) {
            break;
        }
        total += frame_len;
        retention.observe(record.timestamp_ms());
        encoded.push(protected);
    }
    encoded.reverse();
    // This exclusive directory is also the unresolved-transaction marker.
    // Never adopt, overwrite or clean up a directory created by another run.
    let transaction = compaction_transaction_path(path)?;
    fs::create_dir(&transaction)?;
    let temp = transaction.join("replacement.bin");
    let backup = transaction.join("previous.bin");
    let mut replacement = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&temp)?;
    replacement.write_all(&header())?;
    for payload in &encoded {
        append_encrypted(&mut replacement, payload)?;
    }
    #[cfg(test)]
    tests::publication_cut("temp_written")?;
    replacement.sync_all()?;
    #[cfg(test)]
    tests::publication_cut("temp_synced")?;
    drop(replacement);
    let expected = validate_compaction_file(&temp)?;
    #[cfg(test)]
    let _publication_guard = tests::lock_replacement_if_requested(&temp);
    replace_history_file(path, &temp, &backup)?;
    #[cfg(test)]
    tests::publication_cut("replaced")?;
    // Preserve candidates until canonical is synced and validated. Subsequent
    // cleanup errors retain the marker and the validated new canonical, even
    // if the obsolete backup has already been removed.
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()?;
    #[cfg(test)]
    tests::publication_cut("canonical_synced")?;
    if validate_compaction_file(path)? != expected {
        return Err(invalid_data("input history published replacement mismatch"));
    }
    fs::remove_file(&backup)?;
    #[cfg(test)]
    tests::publication_cut("backup_removed")?;
    fs::remove_dir(&transaction)?;
    Ok(retention)
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

/// Bounded complete schema/CRC/DPAPI validation; CRC here is an integrity
/// comparison, not cryptographic authentication or a recovery generation.
fn validate_compaction_file(path: &Path) -> io::Result<(usize, u32)> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_INPUT_HISTORY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT_HISTORY_BYTES {
        return Err(invalid_data(
            "input history replacement exceeds hard size bound",
        ));
    }
    let summary = scan_frames(&bytes, None)?;
    if summary.valid_end != bytes.len() || summary.format_version != INPUT_HISTORY_FORMAT_VERSION {
        return Err(invalid_data("input history replacement is incomplete"));
    }
    Ok((bytes.len(), crc32(&bytes)))
}

fn replace_history_file(path: &Path, temp: &Path, backup: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(code) = tests::PARTIAL_REPLACE_ERROR.with(|failure| failure.take()) {
        if code == 1177 {
            fs::rename(path, backup)?;
        }
        return Err(io::Error::from_raw_os_error(code));
    }
    store_replace_history_file(path, temp, backup)
}

/// Package version and installed release label for the running engine.
///
/// Installed builds use the parent directory name
/// `versions/<version>-<16-hex-build-id>`. Unpackaged local builds fall back
/// to `<package> (unpackaged)` so history exports still show an identity.
pub fn current_engine_identity() -> (String, String) {
    let package_version = ENGINE_PACKAGE_VERSION.to_owned();
    let release_label = release_label_from_current_exe()
        .unwrap_or_else(|| format!("{package_version} (unpackaged)"));
    (package_version, release_label)
}

fn release_label_from_current_exe() -> Option<String> {
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    let name = directory.file_name()?.to_str()?;
    is_installed_release_dir(name).then(|| name.to_owned())
}

fn is_installed_release_dir(name: &str) -> bool {
    let Some((version, build_id)) = name.rsplit_once('-') else {
        return false;
    };
    !version.is_empty()
        && build_id.len() == VERSION_BUILD_ID_LENGTH
        && build_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn header() -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(MAGIC);
    header[4..6].copy_from_slice(&INPUT_HISTORY_FORMAT_VERSION.to_le_bytes());
    header
}

fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    DpapiSealer.seal(bytes)
}

fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    DpapiSealer.open(bytes)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
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
#[path = "input_history_tests.rs"]
mod tests;
