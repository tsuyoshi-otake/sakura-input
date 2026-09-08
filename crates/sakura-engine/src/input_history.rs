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
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_proto::{AiTextOperation, AiTextStatus, InputScope};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
};
use windows::Win32::Storage::FileSystem::{
    ReplaceFileW, FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_OPEN_REPARSE_POINT, REPLACE_FILE_FLAGS,
};

const MAGIC: &[u8; 4] = b"SKIH";
const HEADER_LEN: usize = 8;
const FRAME_HEADER_LEN: usize = 8;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const QUEUE_CAPACITY: usize = 1024;
const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const COMPACTION_INTERVAL: Duration = Duration::from_secs(60);
const COMPACTION_APPEND_LIMIT: u32 = 256;

pub const INPUT_HISTORY_FORMAT_VERSION: u16 = 2;
const INPUT_HISTORY_FORMAT_VERSION_MIN: u16 = 1;
pub const MAX_INPUT_HISTORY_BYTES: u64 = 64 * 1024 * 1024;
pub const ENGINE_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
const VERSION_BUILD_ID_LENGTH: usize = 16;

const RECORD_KEY: u8 = 1;
const RECORD_COMMIT: u8 = 2;
const RECORD_AI_TEXT: u8 = 3;
const RECORD_ENGINE: u8 = 4;

/// Scope classification attached to every persisted record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScopeClass {
    Unclassified = 0,
    Normal = 1,
    Sensitive = 2,
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

    fn from_u8(value: u8) -> io::Result<Self> {
        match value {
            0 => Ok(Self::Unclassified),
            1 => Ok(Self::Normal),
            2 => Ok(Self::Sensitive),
            _ => Err(invalid_data("unknown input history scope")),
        }
    }
}

const fn ai_operation_name(operation: AiTextOperation) -> &'static str {
    match operation {
        AiTextOperation::Transform => "transform",
        AiTextOperation::Proofread => "proofread",
    }
}

fn decode_ai_operation(value: u8) -> io::Result<AiTextOperation> {
    match value {
        1 => Ok(AiTextOperation::Transform),
        2 => Ok(AiTextOperation::Proofread),
        _ => Err(invalid_data("unknown AI text operation")),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyHistoryRecord {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub session: u64,
    pub scope: ScopeClass,
    pub key_code: u16,
    pub character: Option<char>,
    pub modifiers: u8,
    pub repeat: bool,
    pub consumed: bool,
    pub state_before: u8,
    pub state_after: u8,
    pub mode_before: u8,
    pub mode_after: u8,
    pub preedit_before: String,
    pub preedit_after: String,
    pub commit: String,
    pub delete_before: u16,
    pub beep: bool,
    pub action: String,
    pub dropped_before: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitHistoryRecord {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub session: u64,
    pub scope: ScopeClass,
    pub reading: String,
    pub surface: String,
    pub left_context: u16,
    pub right_context: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTextHistoryRecord {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub session: u64,
    pub scope: ScopeClass,
    pub operation: AiTextOperation,
    pub status: AiTextStatus,
    pub source: String,
    pub result: String,
    pub model: String,
    pub provider: String,
    pub style: String,
    pub error_code: String,
    pub latency_ms: u64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub attempts: u32,
}

/// Marks which engine build wrote the following history records.
///
/// Emitted once when the developer-history service starts so `history show`
/// and exports can attribute a log stream to a package version and, for
/// installed builds, the `versions/<version>-<build-id>` release label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineHistoryRecord {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub session: u64,
    pub scope: ScopeClass,
    pub package_version: String,
    pub release_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputHistoryRecord {
    Key(KeyHistoryRecord),
    Commit(CommitHistoryRecord),
    AiText(AiTextHistoryRecord),
    Engine(EngineHistoryRecord),
}

impl InputHistoryRecord {
    fn session(&self) -> u64 {
        match self {
            Self::Key(record) => record.session,
            Self::Commit(record) => record.session,
            Self::AiText(record) => record.session,
            Self::Engine(record) => record.session,
        }
    }

    fn sequence(&self) -> u64 {
        match self {
            Self::Key(record) => record.sequence,
            Self::Commit(record) => record.sequence,
            Self::AiText(record) => record.sequence,
            Self::Engine(record) => record.sequence,
        }
    }

    fn timestamp_ms(&self) -> u64 {
        match self {
            Self::Key(record) => record.timestamp_ms,
            Self::Commit(record) => record.timestamp_ms,
            Self::AiText(record) => record.timestamp_ms,
            Self::Engine(record) => record.timestamp_ms,
        }
    }

    fn encode(&self) -> io::Result<Vec<u8>> {
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

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut reader = Reader::new(bytes);
        let kind = reader.u8()?;
        let sequence = reader.u64()?;
        let timestamp_ms = reader.u64()?;
        let session = reader.u64()?;
        let scope = ScopeClass::from_u8(reader.u8()?)?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputHistorySnapshot {
    pub format_version: u16,
    pub records: Vec<InputHistoryRecord>,
    pub ignored_tail_bytes: usize,
}

impl InputHistorySnapshot {
    /// Applies the public viewing/export retention policy without rewriting
    /// storage. Raw snapshots deliberately remain unfiltered for recovery and
    /// identifier accounting. `now_ms` also permits deterministic boundary tests.
    pub fn retain_current_records(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(RETENTION.as_millis() as u64);
        self.records
            .retain(|record| record.timestamp_ms() >= cutoff);
    }

    /// Latest engine identity marker in the snapshot, if any.
    pub fn last_engine_identity(&self) -> Option<(&str, &str)> {
        self.records.iter().rev().find_map(|record| match record {
            InputHistoryRecord::Engine(record) => Some((
                record.package_version.as_str(),
                record.release_label.as_str(),
            )),
            _ => None,
        })
    }

    pub fn to_tsv(&self) -> String {
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

#[derive(Debug, Default)]
pub struct InputHistoryStats {
    dropped_events: AtomicU64,
    persistence_failures: AtomicU64,
    excluded_unclassified_events: AtomicU64,
    excluded_sensitive_events: AtomicU64,
    excluded_test_only_events: AtomicU64,
    ai_requests: AtomicU64,
    ai_attempts: AtomicU64,
    ai_input_tokens: AtomicU64,
    ai_output_tokens: AtomicU64,
    ai_cached_tokens: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputHistoryStatsSnapshot {
    pub dropped_events: u64,
    pub persistence_failures: u64,
    pub excluded_unclassified_events: u64,
    pub excluded_sensitive_events: u64,
    pub excluded_test_only_events: u64,
    pub ai_requests: u64,
    pub ai_attempts: u64,
    pub ai_input_tokens: u64,
    pub ai_output_tokens: u64,
    pub ai_cached_tokens: u64,
}

impl InputHistoryStats {
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    pub fn persistence_failures(&self) -> u64 {
        self.persistence_failures.load(Ordering::Relaxed)
    }

    pub fn excluded_unclassified_events(&self) -> u64 {
        self.excluded_unclassified_events.load(Ordering::Relaxed)
    }

    pub fn excluded_sensitive_events(&self) -> u64 {
        self.excluded_sensitive_events.load(Ordering::Relaxed)
    }

    pub fn excluded_test_only_events(&self) -> u64 {
        self.excluded_test_only_events.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> InputHistoryStatsSnapshot {
        InputHistoryStatsSnapshot {
            dropped_events: self.dropped_events(),
            persistence_failures: self.persistence_failures(),
            excluded_unclassified_events: self.excluded_unclassified_events(),
            excluded_sensitive_events: self.excluded_sensitive_events(),
            excluded_test_only_events: self.excluded_test_only_events(),
            ai_requests: self.ai_requests.load(Ordering::Relaxed),
            ai_attempts: self.ai_attempts.load(Ordering::Relaxed),
            ai_input_tokens: self.ai_input_tokens.load(Ordering::Relaxed),
            ai_output_tokens: self.ai_output_tokens.load(Ordering::Relaxed),
            ai_cached_tokens: self.ai_cached_tokens.load(Ordering::Relaxed),
        }
    }

    fn excludes(&self, scope: ScopeClass, test_only: bool) -> bool {
        if test_only {
            self.excluded_test_only_events
                .fetch_add(1, Ordering::Relaxed);
            return true;
        }
        match scope {
            ScopeClass::Normal => false,
            ScopeClass::Unclassified => {
                self.excluded_unclassified_events
                    .fetch_add(1, Ordering::Relaxed);
                true
            }
            ScopeClass::Sensitive => {
                self.excluded_sensitive_events
                    .fetch_add(1, Ordering::Relaxed);
                true
            }
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
        formatter
            .debug_struct("InputHistoryService")
            .field("path", &self.path)
            .field("dropped_events", &self.stats.dropped_events())
            .field("persistence_failures", &self.stats.persistence_failures())
            .field(
                "excluded_unclassified_events",
                &self.stats.excluded_unclassified_events(),
            )
            .field(
                "excluded_sensitive_events",
                &self.stats.excluded_sensitive_events(),
            )
            .field(
                "excluded_test_only_events",
                &self.stats.excluded_test_only_events(),
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
                self.stats
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    // Zero is reserved for engine markers and unavailable history sessions.
    // Ordinary protocol input can continue after history allocation fails.
    fn excludes_session(&self, session: u64) -> bool {
        if session == 0 {
            self.stats.dropped_events.fetch_add(1, Ordering::Relaxed);
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
                scope: ScopeClass::Normal,
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
            self.stats
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
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
                self.stats.dropped_events.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.stats
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
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
        if self.stats.excludes(scope, test_only) {
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
        let dropped_before = self.stats.dropped_events.load(Ordering::Relaxed);
        let record = InputHistoryRecord::Key(KeyHistoryRecord {
            sequence,
            timestamp_ms: now_ms(),
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
        if self.stats.excludes(scope, false) || reading.is_empty() || surface.is_empty() {
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
            scope,
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
        if self.stats.excludes(scope, test_only) || source.is_empty() {
            return;
        }
        if self.excludes_session(session) {
            return;
        }
        self.stats.ai_requests.fetch_add(1, Ordering::Relaxed);
        self.stats
            .ai_attempts
            .fetch_add(u64::from(attempts), Ordering::Relaxed);
        self.stats
            .ai_input_tokens
            .fetch_add(u64::from(input_tokens), Ordering::Relaxed);
        self.stats
            .ai_output_tokens
            .fetch_add(u64::from(output_tokens), Ordering::Relaxed);
        self.stats
            .ai_cached_tokens
            .fetch_add(u64::from(cached_tokens), Ordering::Relaxed);
        let Some(sequence) = self.allocate_counter(&self.next_sequence, Ordering::Relaxed) else {
            return;
        };
        let record = InputHistoryRecord::AiText(AiTextHistoryRecord {
            sequence,
            timestamp_ms: now_ms(),
            session,
            scope,
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
            self.stats
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
            io::Error::new(io::ErrorKind::BrokenPipe, "input history writer stopped")
        })?;
        receiver.recv().map_err(|_| {
            self.stats
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
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
                self.stats
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
                io::Error::new(io::ErrorKind::BrokenPipe, "input history writer stopped")
            })?;
        receiver.recv().map_err(|_| {
            self.stats
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
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
                self.stats
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "input history writer stopped",
                ))
            }),
            Err(_) => {
                self.stats
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "input history writer stopped",
                ))
            }
        };
        if let Some(worker) = shutdown.handle.take() {
            if worker.join().is_err() {
                self.stats
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
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

pub fn default_path() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for the developer input history",
        )
    })?;
    Ok(PathBuf::from(local)
        .join("SakuraInput")
        .join("history")
        .join("input.bin"))
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
                            stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
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
                                        stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                            appends_since_compaction = 0;
                            last_compaction = Instant::now();
                        }
                    }
                    Err(_) => {
                        append_failed = true;
                        stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Command::Flush { reply } => {
                let result = sync_writer_file(&file, append_failed);
                if result.is_err() {
                    stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
                }
                let _ = reply.send(result);
            }
            Command::Clear { epoch, reply } => {
                cleared_epoch = cleared_epoch.max(epoch);
                let result = clear_writer_file(&path, &mut file);
                if result.is_err() {
                    stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
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
                    stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
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
    if current_len.saturating_add(frame_len) > MAX_INPUT_HISTORY_BYTES {
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
    if current_len.saturating_add(frame_len) > MAX_INPUT_HISTORY_BYTES {
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
    let cutoff = now_ms().saturating_sub(RETENTION.as_millis() as u64);
    let mut records: Vec<_> = snapshot
        .records
        .into_iter()
        .filter(|record| record.timestamp_ms() >= cutoff)
        .collect();
    records.sort_by_key(InputHistoryRecord::sequence);
    let mut encoded = Vec::with_capacity(records.len());
    let mut total = HEADER_LEN as u64;
    let mut retention = RetentionPlan::default();
    for record in records.into_iter().rev() {
        let payload = record.encode()?;
        let protected = protect(&payload)?;
        let frame_len = FRAME_HEADER_LEN as u64 + protected.len() as u64;
        if total.saturating_add(frame_len) > MAX_INPUT_HISTORY_BYTES {
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

fn compaction_transaction_path(path: &Path) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "history path has no file name")
        })?
        .to_os_string();
    name.push(".compaction");
    Ok(path.with_file_name(name))
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
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let mut encoded: Vec<_> = path.as_os_str().encode_wide().collect();
        if encoded.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "history path contains NUL",
            ));
        }
        encoded.push(0);
        Ok(encoded)
    };
    let canonical = wide(path)?;
    let replacement = wide(temp)?;
    let previous = wide(backup)?;
    // SAFETY: all paths are live NUL-terminated buffers, and the caller owns
    // the cooperative store lock. Backup and replacement are in the newly
    // created same-volume transaction directory. Do not ignore ACL errors or
    // use the unsupported REPLACEFILE_WRITE_THROUGH flag.
    unsafe {
        ReplaceFileW(
            PCWSTR(canonical.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR(previous.as_ptr()),
            REPLACE_FILE_FLAGS::default(),
            None,
            None,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))
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
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| invalid_data("input history payload too large"))?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: `input` borrows `bytes` for the duration of the call and
    // `output` is writable. DPAPI allocates `output.pbData`; it is copied
    // before being released exactly once with LocalFree.
    unsafe {
        CryptProtectData(
            &input,
            windows::core::PCWSTR::null(),
            None,
            None,
            None,
            0,
            &mut output,
        )
        .map_err(|error| io::Error::other(format!("DPAPI protect: {error}")))?;
        let protected = if output.pbData.is_null() {
            Err(io::Error::other("DPAPI returned an empty payload"))
        } else {
            Ok(std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec())
        };
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        protected
    }
}

fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| invalid_data("protected payload too large"))?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: `input` borrows the protected bytes for the call and `output`
    // is writable. DPAPI owns the returned allocation until the matching
    // LocalFree after the plaintext has been copied.
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
            .map_err(|error| io::Error::other(format!("DPAPI unprotect: {error}")))?;
        let plain = if output.pbData.is_null() {
            Err(io::Error::other("DPAPI returned an empty payload"))
        } else {
            Ok(std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec())
        };
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        plain
    }
}

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
mod tests {
    thread_local! {
        // Only unit-test binaries contain this rendezvous; release/default
        // library artifacts cannot pause a producer through this hook.
        pub(super) static BEFORE_ENQUEUE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn temporary_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sakura-input-history-{}-{name}-{}.bin",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn key_record(sequence: u64, timestamp_ms: u64) -> InputHistoryRecord {
        InputHistoryRecord::Key(KeyHistoryRecord {
            sequence,
            timestamp_ms,
            session: 1,
            scope: ScopeClass::Normal,
            key_code: 1,
            character: Some('x'),
            modifiers: 0,
            repeat: false,
            consumed: true,
            state_before: 0,
            state_after: 1,
            mode_before: 1,
            mode_after: 1,
            preedit_before: String::new(),
            preedit_after: "x".to_owned(),
            commit: String::new(),
            delete_before: 0,
            beep: false,
            action: "char".to_owned(),
            dropped_before: 0,
        })
    }

    fn append_records(path: &Path, records: &[InputHistoryRecord]) {
        ensure_file(path).expect("ensure history file");
        let mut file = OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open history file");
        for record in records {
            let protected = protect(&record.encode().expect("encode record")).expect("protect");
            append_encrypted(&mut file, &protected).expect("append record");
        }
    }

    struct ReadFailureFixture(PathBuf);

    impl Drop for ReadFailureFixture {
        fn drop(&mut self) {
            for path in [&self.0, &self.0.with_extension("compact.tmp")] {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => panic!("remove synthetic history fixture: {error}"),
                }
            }
        }
    }

    fn assert_complete_frame_failure_preserves_store(encrypted: &[u8]) {
        let fixture = ReadFailureFixture(temporary_path("read-failure"));
        let path = &fixture.0;
        append_records(path, &[key_record(1, now_ms())]);
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        append_encrypted(&mut file, encrypted).unwrap();
        drop(file);
        append_records(path, &[key_record(3, now_ms())]);
        let original = fs::read(path).unwrap();

        // Repair must not transform a complete opaque frame into a torn tail.
        let repair = repair_file(path);
        assert_eq!(
            fs::read(path).unwrap(),
            original,
            "repair erased opaque data"
        );
        assert!(repair.is_err(), "repair must report unavailable content");
        assert!(
            read_snapshot(path).is_err(),
            "snapshot must not claim completeness"
        );
        assert!(
            compact_file(path).is_err(),
            "compaction must not omit opaque data"
        );
        assert_eq!(fs::read(path).unwrap(), original);
        let service = InputHistoryService::open(path);
        if let Ok(service) = &service {
            service.stop().unwrap();
        }
        assert!(
            service.is_err(),
            "startup must fail before spawning the writer"
        );
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn complete_frame_decryption_failure_preserves_store() {
        let ciphertext = b"synthetic invalid DPAPI blob";
        assert!(unprotect(ciphertext).is_err());
        assert_complete_frame_failure_preserves_store(ciphertext);
    }

    #[test]
    fn complete_frame_unknown_record_preserves_store() {
        // Valid DPAPI and frame checksum; the record type is unsupported.
        // Include all common fields so decoding reaches the unknown-kind arm,
        // rather than failing earlier while reading a truncated header.
        let mut payload = vec![0; 1 + 8 + 8 + 8 + 1];
        payload[0] = 255;
        payload[25] = ScopeClass::Normal as u8;
        assert_eq!(
            InputHistoryRecord::decode(&payload)
                .unwrap_err()
                .to_string(),
            "unknown input history record"
        );
        assert_complete_frame_failure_preserves_store(&protect(&payload).unwrap());
    }

    #[test]
    fn complete_frame_malformed_record_preserves_store() {
        assert_complete_frame_failure_preserves_store(&protect(&[RECORD_KEY]).unwrap());
    }

    #[test]
    fn future_history_format_preserves_store() {
        let fixture = ReadFailureFixture(temporary_path("future-format"));
        append_records(&fixture.0, &[key_record(1, now_ms())]);
        let mut original = fs::read(&fixture.0).unwrap();
        original[4..6].copy_from_slice(&(INPUT_HISTORY_FORMAT_VERSION + 1).to_le_bytes());
        fs::write(&fixture.0, &original).unwrap();
        assert!(repair_file(&fixture.0).is_err());
        assert!(compact_file(&fixture.0).is_err());
        assert!(InputHistoryService::open(&fixture.0).is_err());
        assert_eq!(fs::read(&fixture.0).unwrap(), original);
    }

    #[test]
    fn structural_tail_damage_still_repairs_to_verified_prefix() {
        for tail in [vec![1, 2, 3], vec![1, 0, 0, 0, 0, 0, 0, 0, 42]] {
            let fixture = ReadFailureFixture(temporary_path("structural-tail"));
            append_records(&fixture.0, &[key_record(1, now_ms())]);
            let original = fs::read(&fixture.0).unwrap();
            let mut file = OpenOptions::new().append(true).open(&fixture.0).unwrap();
            file.write_all(&tail).unwrap();
            drop(file);
            repair_file(&fixture.0).unwrap();
            assert_eq!(fs::read(&fixture.0).unwrap(), original);
            assert_eq!(read_snapshot(&fixture.0).unwrap().records.len(), 1);
        }
    }

    fn assert_control_barrier_preserves_bytes(flush_first: bool) {
        let fixture = ReadFailureFixture(temporary_path("control-barrier"));
        let now = now_ms();
        let old = now.saturating_sub(RETENTION.as_millis() as u64 + 1);
        append_records(&fixture.0, &[key_record(1, old), key_record(2, now)]);
        let original = fs::read(&fixture.0).unwrap();
        let (sender, receiver) = mpsc::sync_channel(2);
        let (flush_reply, flush_result) = mpsc::channel();
        if flush_first {
            sender.send(Command::Flush { reply: flush_reply }).unwrap();
        }
        let (reply, stopped) = mpsc::channel();
        sender.send(Command::Shutdown { reply }).unwrap();
        // Prequeued controls execute deterministically, without a sleep or
        // spawned thread whose ownership could be lost on assertion failure.
        writer_loop_with_interval(
            fixture.0.clone(),
            receiver,
            Arc::new(InputHistoryStats::default()),
            Duration::from_secs(3600),
        );
        if flush_first {
            flush_result.try_recv().unwrap().unwrap();
        }
        stopped.try_recv().unwrap().unwrap();
        assert!(
            fs::read(&fixture.0).unwrap() == original,
            "a control barrier must not rewrite or expire existing frames"
        );
    }

    #[test]
    fn flush_barrier_preserves_existing_ciphertext() {
        assert_control_barrier_preserves_bytes(true);
    }

    #[test]
    fn shutdown_barrier_preserves_existing_ciphertext() {
        assert_control_barrier_preserves_bytes(false);
    }

    #[test]
    fn startup_preserves_ciphertext_and_recovers_ids_before_retention() {
        let fixture = ReadFailureFixture(temporary_path("startup-ids"));
        let now = now_ms();
        let mut expired = key_record(900, now.saturating_sub(RETENTION.as_millis() as u64 + 1));
        if let InputHistoryRecord::Key(record) = &mut expired {
            record.session = 700;
        }
        append_records(&fixture.0, &[expired, key_record(3, now)]);
        let original = fs::read(&fixture.0).unwrap();
        let service = InputHistoryService::open(&fixture.0).unwrap();
        let session = service.allocate_session_id().unwrap();
        service.stop().unwrap();
        let after = fs::read(&fixture.0).unwrap();
        let snapshot = read_snapshot(&fixture.0).unwrap();
        let sequence = snapshot.records.last().unwrap().sequence();
        let prefix_preserved = after.starts_with(&original);
        assert!(prefix_preserved && session == 701 && sequence == 901,
            "startup prefix={prefix_preserved}, session={session} (expected 701), sequence={sequence} (expected 901)");
    }

    thread_local! {
        pub(super) static FAIL_AFTER_APPEND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static AFTER_MAINTENANCE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn after_maintenance_check() {
        AFTER_MAINTENANCE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
    }

    #[test]
    fn maintenance_expires_record_after_uncertain_append_failure() {
        let fixture = PublicationFixture(ReadFailureFixture(temporary_path("uncertain-expiry")));
        let path = fixture.0 .0.clone();
        ensure_file(&path).unwrap();
        let file = open_append(&path).unwrap();
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (checked, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        sender
            .send(Command::Append {
                epoch: 0,
                timestamp_ms: 1,
                payload: key_record(1, 1).encode().unwrap(),
            })
            .unwrap();
        let worker = thread::spawn(move || {
            FAIL_AFTER_APPEND.with(|fail| fail.set(true));
            AFTER_MAINTENANCE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    checked.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(10)).unwrap();
                }));
            });
            writer_loop_with_file(
                path,
                receiver,
                Arc::new(InputHistoryStats::default()),
                Some(file),
                Duration::from_millis(1),
                RetentionPlan::default(),
            );
        });
        let checked = observed.recv_timeout(Duration::from_secs(10));
        let snapshot = read_snapshot(&fixture.0 .0);
        let (reply, stopped) = mpsc::channel();
        let sent = sender.send(Command::Shutdown { reply });
        let _ = release.send(());
        let joined = worker.join();
        assert!(checked.is_ok() && sent.is_ok() && joined.is_ok());
        assert!(
            stopped.recv().unwrap().is_err(),
            "append loss remains observable"
        );
        assert!(
            snapshot.unwrap().records.is_empty(),
            "uncertain expired append must be removed"
        );
    }

    #[test]
    fn maintenance_prewrite_rejection_does_not_schedule_expiry() {
        let fixture = ReadFailureFixture(temporary_path("rejected-expiry"));
        let now = now_ms();
        append_records(&fixture.0, &[key_record(1, now)]);
        let mut file = Some(open_append(&fixture.0).unwrap());
        let mut retention = RetentionPlan::default();
        retention.observe(now);
        assert!(append_payload(
            &fixture.0,
            &mut file,
            &vec![0; MAX_RECORD_BYTES + 1],
            1,
            &mut retention
        )
        .is_err());
        assert_eq!(retention.oldest_timestamp_ms, Some(now));
        assert!(!retention.is_due(now));
    }

    #[test]
    fn maintenance_unexpired_idle_store_preserves_ciphertext() {
        let fixture = ReadFailureFixture(temporary_path("clean-idle"));
        append_records(&fixture.0, &[key_record(1, now_ms())]);
        let original = fs::read(&fixture.0).unwrap();
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (checked, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let path = fixture.0.clone();
        let worker = thread::spawn(move || {
            AFTER_MAINTENANCE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    checked.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(10)).unwrap();
                }))
            });
            writer_loop_with_interval(
                path,
                receiver,
                Arc::new(InputHistoryStats::default()),
                Duration::from_millis(1),
            );
        });
        let checked = observed.recv_timeout(Duration::from_secs(10));
        let unchanged = fs::read(&fixture.0).unwrap() == original;
        let (reply, stopped) = mpsc::channel();
        let sent = sender.send(Command::Shutdown { reply });
        let _ = release.send(());
        let joined = worker.join();
        assert!(checked.is_ok());
        assert!(sent.is_ok());
        assert!(joined.is_ok());
        stopped.recv().unwrap().unwrap();
        assert!(
            unchanged,
            "idle unexpired store was re-encrypted and rewritten"
        );
    }

    #[test]
    fn maintenance_plan_has_inclusive_retention_and_observes_older_appends() {
        let now = RETENTION.as_millis() as u64 + 10;
        let mut plan = RetentionPlan::default();
        assert!(!plan.is_due(now));
        plan.observe(10);
        assert!(!plan.is_due(now), "record on inclusive cutoff is retained");
        assert!(plan.is_due(now + 1));
        plan.observe(5);
        assert!(plan.is_due(now));
        assert!(!plan.is_due(0), "clock rollback cannot underflow cutoff");
        plan = RetentionPlan::default();
        assert!(
            !plan.is_due(u64::MAX),
            "successful empty Clear leaves no expiry work"
        );
        plan.observe(u64::MAX);
        assert!(!plan.is_due(u64::MAX));
    }

    #[test]
    fn maintenance_plan_is_recovered_in_scan_and_refreshed_by_compaction() {
        let fixture = ReadFailureFixture(temporary_path("retention-hint"));
        let now = now_ms();
        append_records(&fixture.0, &[key_record(2, now), key_record(1, 1)]);
        let recovered = repair_file(&fixture.0).unwrap();
        assert_eq!(recovered.retention.oldest_timestamp_ms, Some(1));
        let retained = compact_file(&fixture.0).unwrap();
        assert_eq!(retained.oldest_timestamp_ms, Some(now));
        assert!(!retained.is_due(now));
        clear_path(&fixture.0).unwrap();
        let cleared = compact_file(&fixture.0).unwrap();
        assert_eq!(cleared.oldest_timestamp_ms, None);
    }

    fn shutdown_fixture(
        action: impl FnOnce(Receiver<Command>) + Send + 'static,
    ) -> InputHistoryService {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        InputHistoryService {
            path: temporary_path("shutdown-only"),
            sender,
            stats: Arc::new(InputHistoryStats::default()),
            next_sequence: AtomicU64::new(0),
            next_session_id: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            worker: Mutex::new(WriterShutdown {
                handle: Some(thread::spawn(move || action(receiver))),
                outcome: None,
            }),
        }
    }

    #[test]
    fn stop_outcome_keeps_durable_failure_for_later_callers() {
        let service = shutdown_fixture(|receiver| {
            let Command::Shutdown { reply } = receiver.recv().unwrap() else {
                panic!("shutdown expected")
            };
            reply
                .send(Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "synthetic sync failure",
                )))
                .unwrap();
        });
        let first = service.stop();
        let second = service.stop();
        assert_eq!(first.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(second.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn stop_outcome_joins_worker_after_reply_disconnect() {
        let service = shutdown_fixture(|receiver| {
            let Command::Shutdown { reply } = receiver.recv().unwrap() else {
                panic!("shutdown expected")
            };
            drop(reply);
        });
        let result = service.stop();
        let joined = service.worker.lock().unwrap().handle.is_none();
        // Clean up the old implementation before asserting its counterexample.
        let _ = service.stop();
        assert!(result.is_err());
        assert!(
            joined,
            "lost reply returned before worker join was owned/completed"
        );
    }

    #[test]
    fn stop_outcome_rejects_success_reply_followed_by_worker_panic() {
        let service = shutdown_fixture(|receiver| {
            let Command::Shutdown { reply } = receiver.recv().unwrap() else {
                panic!("shutdown expected")
            };
            reply.send(Ok(())).unwrap();
            panic!("synthetic worker exit failure");
        });
        assert!(
            service.stop().is_err(),
            "worker panic was reported as a successful stop"
        );
        assert!(
            service.stop().is_err(),
            "later stop lost the worker failure"
        );
    }

    #[test]
    fn stop_outcome_concurrent_callers_share_one_shutdown_and_failure() {
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let service = Arc::new(shutdown_fixture(move |receiver| {
            let Command::Shutdown { reply } = receiver.recv().unwrap() else {
                panic!("shutdown expected")
            };
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(10)).unwrap();
            reply.send(Err(io::Error::from_raw_os_error(5))).unwrap();
            assert!(receiver.try_recv().is_err(), "duplicate Shutdown enqueued");
        }));
        let start = Arc::new(std::sync::Barrier::new(9));
        let callers: Vec<_> = (0..8)
            .map(|_| {
                let service = Arc::clone(&service);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    service.stop().unwrap_err().raw_os_error()
                })
            })
            .collect();
        start.wait();
        let ready = observed.recv_timeout(Duration::from_secs(10));
        let _ = release.send(());
        let results: Vec<_> = callers.into_iter().map(|caller| caller.join()).collect();
        assert!(ready.is_ok());
        for result in results {
            assert_eq!(result.unwrap(), Some(5));
        }
        assert!(service.worker.lock().unwrap().handle.is_none());
    }

    thread_local! {
        static LOCK_REPLACEMENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        pub(super) static PARTIAL_REPLACE_ERROR: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
        static PUBLICATION_CUT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    }

    pub(super) fn publication_cut(point: &'static str) -> io::Result<()> {
        PUBLICATION_CUT.with(|cut| {
            if cut.get() == Some(point) {
                cut.set(None);
                Err(io::Error::from_raw_os_error(112))
            } else {
                Ok(())
            }
        })
    }

    pub(super) fn lock_replacement_if_requested(path: &Path) -> Option<File> {
        LOCK_REPLACEMENT.with(|requested| {
            requested.replace(false).then(|| {
                OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .open(path)
                    .unwrap()
            })
        })
    }

    struct PublicationFixture(ReadFailureFixture);

    impl PublicationFixture {
        fn new(label: &str) -> Self {
            Self(ReadFailureFixture(temporary_path(label)))
        }
        fn path(&self) -> &Path {
            &self.0 .0
        }
        fn transaction(&self) -> PathBuf {
            let mut name = self.path().file_name().unwrap().to_os_string();
            name.push(".compaction");
            self.path().with_file_name(name)
        }
    }

    impl Drop for PublicationFixture {
        fn drop(&mut self) {
            let directory = self.transaction();
            for name in ["replacement.bin", "previous.bin"] {
                match fs::remove_file(directory.join(name)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => panic!("remove synthetic transaction file: {error}"),
                }
            }
            match fs::remove_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove synthetic transaction directory: {error}"),
            }
        }
    }

    #[test]
    fn publication_partial_windows_outcomes_preserve_candidates_and_refuse_reopen() {
        for code in [1175, 1176, 1177, 5] {
            let fixture = PublicationFixture::new(&format!("partial-{code}"));
            append_records(fixture.path(), &[key_record(1, now_ms())]);
            let original = fs::read(fixture.path()).unwrap();
            PARTIAL_REPLACE_ERROR.with(|failure| failure.set(Some(code)));
            assert_eq!(
                compact_file(fixture.path()).unwrap_err().raw_os_error(),
                Some(code)
            );
            assert!(fixture.transaction().join("replacement.bin").exists());
            if code == 1177 {
                assert!(!fixture.path().exists());
                assert!(fs::read(fixture.transaction().join("previous.bin")).unwrap() == original);
            } else {
                assert!(fs::read(fixture.path()).unwrap() == original);
            }
            assert!(open_append(fixture.path()).is_err());
            assert!(InputHistoryService::open(fixture.path()).is_err());
            assert!(clear_path(fixture.path()).is_err());
            assert_eq!(fixture.path().exists(), code != 1177);
        }
    }

    #[test]
    fn publication_injected_stage_errors_remain_unavailable_without_cleanup() {
        for point in [
            "temp_written",
            "temp_synced",
            "replaced",
            "canonical_synced",
            "backup_removed",
        ] {
            let fixture = PublicationFixture::new(point);
            append_records(fixture.path(), &[key_record(1, now_ms())]);
            PUBLICATION_CUT.with(|cut| cut.set(Some(point)));
            let mut writer = Some(open_append(fixture.path()).unwrap());
            assert!(compact_writer_file(fixture.path(), &mut writer).is_err());
            assert!(
                writer.is_none(),
                "failed transaction reopened an append handle"
            );
            assert!(fixture.transaction().exists());
            assert!(fixture.path().exists());
            validate_compaction_file(fixture.path()).unwrap();
            if point == "backup_removed" {
                assert!(!fixture.transaction().join("previous.bin").exists());
                let bytes = fs::read(fixture.path()).unwrap();
                assert_eq!(scan_snapshot(&bytes).unwrap().records.len(), 1);
            }
            assert!(read_snapshot(fixture.path()).is_err());
            assert!(clear_path(fixture.path()).is_err());
        }
    }

    #[test]
    fn publication_success_removes_transaction_for_empty_and_retained_stores() {
        for retained in [false, true] {
            let fixture = PublicationFixture::new("publish-success");
            let timestamp = if retained { now_ms() } else { 1 };
            append_records(fixture.path(), &[key_record(1, timestamp)]);
            compact_file(fixture.path()).unwrap();
            assert!(!fixture.transaction().exists());
            assert_eq!(
                read_snapshot(fixture.path()).unwrap().records.len(),
                usize::from(retained)
            );
            clear_path(fixture.path()).unwrap();
            assert!(read_snapshot(fixture.path()).unwrap().records.is_empty());
        }
    }

    #[test]
    fn publication_real_sharing_failure_preserves_canonical() {
        let fixture = PublicationFixture::new("publication-sharing");
        append_records(fixture.path(), &[key_record(1, now_ms())]);
        let original = fs::read(fixture.path()).unwrap();
        LOCK_REPLACEMENT.with(|requested| requested.set(true));
        let result = compact_file(fixture.path());
        assert!(
            result.is_err(),
            "real replacement sharing conflict must fail"
        );
        assert!(
            fs::read(fixture.path()).ok().as_deref() == Some(original.as_slice()),
            "failed publication lost the original canonical"
        );
    }

    #[test]
    fn publication_pending_transaction_prevents_empty_recreation() {
        let fixture = PublicationFixture::new("pending-missing");
        fs::create_dir(fixture.transaction()).unwrap();
        fs::write(fixture.transaction().join("replacement.bin"), header()).unwrap();
        let opened = InputHistoryService::open(fixture.path());
        let rejected = opened.is_err();
        if let Ok(service) = opened {
            service.stop().unwrap();
        }
        assert!(
            rejected,
            "unresolved publication was treated as a new store"
        );
        assert!(!fixture.path().exists(), "empty canonical was manufactured");
    }

    #[test]
    fn publication_pending_transaction_prevents_false_clear_success() {
        let fixture = PublicationFixture::new("pending-clear");
        append_records(fixture.path(), &[key_record(1, now_ms())]);
        let original = fs::read(fixture.path()).unwrap();
        fs::create_dir(fixture.transaction()).unwrap();
        fs::write(fixture.transaction().join("previous.bin"), &original).unwrap();
        let cleared = clear_path(fixture.path());
        assert!(
            cleared.is_err(),
            "Clear succeeded while recoverable history remained"
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), original);
    }

    #[test]
    fn publication_preserves_preexisting_legacy_temp() {
        let fixture = PublicationFixture::new("legacy-temp-collision");
        append_records(fixture.path(), &[key_record(1, now_ms())]);
        let temp = fixture.path().with_extension("compact.tmp");
        fs::write(&temp, b"synthetic unrelated legacy temp").unwrap();
        let result = compact_file(fixture.path());
        assert!(result.is_ok());
        assert_eq!(
            fs::read(&temp).ok().as_deref(),
            Some(b"synthetic unrelated legacy temp".as_slice())
        );
    }

    #[test]
    fn store_owner_rejects_second_service_without_mutation() {
        let fixture = ReadFailureFixture(temporary_path("two-writers"));
        let owner = InputHistoryService::open(&fixture.0).unwrap();
        owner.flush().unwrap();
        let original = fs::read(&fixture.0).unwrap();
        let second = InputHistoryService::open(&fixture.0);
        let rejected = second.is_err();
        if let Ok(second) = second {
            second.stop().unwrap();
        }
        owner.stop().unwrap();
        assert!(rejected, "second history writer was admitted");
        assert_eq!(fs::read(&fixture.0).unwrap(), original);
        // A stopped service may still have callers holding its Arc. Only
        // the worker's actual lifetime owns the store, so handoff now works.
        let successor = InputHistoryService::open(&fixture.0).unwrap();
        successor.stop().unwrap();
    }

    #[test]
    fn store_owner_excludes_offline_clear() {
        let fixture = ReadFailureFixture(temporary_path("offline-clear-owner"));
        let owner = InputHistoryService::open(&fixture.0).unwrap();
        owner.flush().unwrap();
        let original = fs::read(&fixture.0).unwrap();
        let cleared = clear_path(&fixture.0);
        owner.stop().unwrap();
        assert!(cleared.is_err(), "offline Clear bypassed the live writer");
        assert_eq!(fs::read(&fixture.0).unwrap(), original);
        clear_path(&fixture.0).unwrap();
        assert!(read_snapshot(&fixture.0).unwrap().records.is_empty());
    }

    #[test]
    fn store_owner_survives_canonical_replacement_and_paths_are_independent() {
        let fixture = ReadFailureFixture(temporary_path("replace-owner"));
        let previous = ReadFailureFixture(temporary_path("previous-owner"));
        let owner = InputHistoryService::open(&fixture.0).unwrap();
        owner.flush().unwrap();
        let original = fs::read(&fixture.0).unwrap();
        fs::rename(&fixture.0, &previous.0).unwrap();
        fs::write(&fixture.0, original).unwrap();
        let alias = fixture
            .0
            .parent()
            .unwrap()
            .join(".")
            .join(fixture.0.file_name().unwrap());
        assert!(InputHistoryService::open(&alias).is_err());
        let independent = ReadFailureFixture(temporary_path("independent-owner"));
        let other = InputHistoryService::open(&independent.0).unwrap();
        other.stop().unwrap();
        owner.stop().unwrap();
        let successor = InputHistoryService::open(&fixture.0).unwrap();
        successor.stop().unwrap();
    }

    #[test]
    fn store_owner_does_not_delete_a_preexisting_lock_path() {
        let fixture = ReadFailureFixture(temporary_path("lock-collision"));
        append_records(&fixture.0, &[key_record(1, now_ms())]);
        let original = fs::read(&fixture.0).unwrap();
        let mut name = fixture.0.file_name().unwrap().to_os_string();
        name.push(".writer.lock");
        let lock_path = fixture.0.with_file_name(name);
        fs::write(&lock_path, b"synthetic preexisting file").unwrap();
        let opened = InputHistoryService::open(&fixture.0);
        let rejected = opened.is_err();
        if let Ok(service) = opened {
            service.stop().unwrap();
        }
        let lock_contents = fs::read(&lock_path).ok();
        let _ = fs::remove_file(&lock_path);
        assert!(
            rejected,
            "existing lock path must not be adopted for deletion"
        );
        assert_eq!(
            lock_contents.as_deref(),
            Some(b"synthetic preexisting file".as_slice())
        );
        assert_eq!(fs::read(&fixture.0).unwrap(), original);
    }

    #[test]
    fn store_owner_preserves_preexisting_symlink_and_target() {
        let fixture = ReadFailureFixture(temporary_path("lock-symlink"));
        let target = ReadFailureFixture(temporary_path("lock-target"));
        fs::write(&target.0, b"synthetic target").unwrap();
        let mut name = fixture.0.file_name().unwrap().to_os_string();
        name.push(".writer.lock");
        let lock_path = fixture.0.with_file_name(name);
        std::os::windows::fs::symlink_file(&target.0, &lock_path).unwrap();
        let opened = InputHistoryService::open(&fixture.0);
        let rejected = opened.is_err();
        if let Ok(service) = opened {
            service.stop().unwrap();
        }
        let link_preserved = fs::symlink_metadata(&lock_path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false);
        let target_contents = fs::read(&target.0).ok();
        let _ = fs::remove_file(&lock_path);
        assert!(rejected);
        assert!(link_preserved);
        assert_eq!(
            target_contents.as_deref(),
            Some(b"synthetic target".as_slice())
        );
        assert!(!fixture.0.exists());
    }

    #[test]
    fn clear_rejects_content_prepared_before_its_epoch() {
        let mut resurrected = Vec::new();
        for kind in 0..3 {
            let fixture = ReadFailureFixture(temporary_path("paused-clear"));
            let service = InputHistoryService::open(&fixture.0).unwrap();
            let producer_service = Arc::clone(&service);
            let (paused, pause_observed) = mpsc::channel();
            let (resume, resumed) = mpsc::channel();
            let producer = thread::spawn(move || {
                BEFORE_ENQUEUE.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        paused.send(()).unwrap();
                        resumed.recv_timeout(Duration::from_secs(10)).unwrap();
                    }));
                });
                match kind {
                    0 => producer_service.record_key(
                        1,
                        ScopeClass::Normal,
                        1,
                        Some('x'),
                        0,
                        false,
                        false,
                        true,
                        0,
                        1,
                        1,
                        1,
                        "",
                        "x",
                        "",
                        0,
                        false,
                        "char",
                    ),
                    1 => producer_service.record_commit(1, ScopeClass::Normal, "x", "X", 0, 0),
                    _ => producer_service.record_ai_text(
                        1,
                        ScopeClass::Normal,
                        AiTextOperation::Transform,
                        AiTextStatus::Applied,
                        "x",
                        "X",
                        "synthetic",
                        "synthetic",
                        "",
                        "",
                        1,
                        1,
                        1,
                        0,
                        1,
                        false,
                    ),
                }
            });
            let paused_ok = pause_observed.recv_timeout(Duration::from_secs(10)).is_ok();
            let first_clear = service.clear();
            let second_clear = service.clear();
            // Always release/join the owned producer before assertions.
            let _ = resume.send(());
            let producer_result = producer.join();
            service.record_commit(2, ScopeClass::Normal, "new", "NEW", 0, 0);
            let flushed = service.flush();
            let stopped = service.stop();
            assert!(paused_ok);
            first_clear.unwrap();
            second_clear.unwrap();
            producer_result.unwrap();
            flushed.unwrap();
            stopped.unwrap();
            let snapshot = read_snapshot(&fixture.0).unwrap();
            assert!(snapshot.records.iter().any(|record| record.session() == 2));
            if snapshot.records.iter().any(|record| record.session() == 1) {
                resurrected.push(kind);
            }
        }
        assert!(
            resurrected.is_empty(),
            "old content survived Clear for variants {resurrected:?}"
        );
    }

    #[test]
    fn startup_rejects_exhausted_stored_ids_without_mutating_history() {
        for session_exhausted in [false, true] {
            let fixture = ReadFailureFixture(temporary_path("exhausted-ids"));
            let mut record = key_record(if session_exhausted { 1 } else { u64::MAX }, now_ms());
            if session_exhausted {
                if let InputHistoryRecord::Key(record) = &mut record {
                    record.session = u64::MAX;
                }
            }
            append_records(&fixture.0, &[record]);
            let original = fs::read(&fixture.0).unwrap();
            assert_eq!(
                InputHistoryService::open(&fixture.0).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(fs::read(&fixture.0).unwrap(), original);
        }
    }

    #[test]
    fn legacy_startup_migrates_before_appending_current_markers() {
        let fixture = ReadFailureFixture(temporary_path("legacy-startup"));
        append_records(&fixture.0, &[key_record(17, now_ms())]);
        let mut bytes = fs::read(&fixture.0).unwrap();
        bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
        fs::write(&fixture.0, bytes).unwrap();
        let service = InputHistoryService::open(&fixture.0).unwrap();
        service.stop().unwrap();
        let snapshot = read_snapshot(&fixture.0).unwrap();
        assert_eq!(snapshot.format_version, INPUT_HISTORY_FORMAT_VERSION);
        assert_eq!(snapshot.records[0].sequence(), 17);
        assert_eq!(snapshot.records.last().unwrap().sequence(), 18);
    }

    #[test]
    fn viewing_retention_has_inclusive_cutoff_and_keeps_order() {
        let now = RETENTION.as_millis() as u64 + 100;
        let mut snapshot = InputHistorySnapshot {
            format_version: INPUT_HISTORY_FORMAT_VERSION,
            records: vec![key_record(3, 101), key_record(1, 99), key_record(2, 100)],
            ignored_tail_bytes: 7,
        };
        snapshot.retain_current_records(now);
        assert_eq!(
            snapshot.records,
            vec![key_record(3, 101), key_record(2, 100)]
        );
        assert_eq!(snapshot.ignored_tail_bytes, 7);
        let before = snapshot.clone();
        snapshot.retain_current_records(0);
        assert_eq!(
            snapshot, before,
            "early clock must not underflow the cutoff"
        );
    }

    #[test]
    fn barriers_report_missing_handle_and_real_sync_failure() {
        assert_eq!(
            sync_writer_file(&None, false).unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
        let fixture = ReadFailureFixture(temporary_path("sync-denied"));
        ensure_file(&fixture.0).unwrap();
        // Windows FlushFileBuffers requires write access. Exercise the real
        // failure, not an injected helper that simply returns its expectation.
        let read_only = File::open(&fixture.0).unwrap();
        assert!(sync_writer_file(&Some(read_only), false).is_err());
    }

    #[test]
    fn barriers_keep_failed_append_observable_until_explicit_clear() {
        let fixture = ReadFailureFixture(temporary_path("barrier-loss"));
        ensure_file(&fixture.0).unwrap();
        let (sender, receiver) = mpsc::sync_channel(6);
        sender
            .send(Command::Append {
                epoch: 0,
                timestamp_ms: now_ms(),
                payload: vec![0; MAX_RECORD_BYTES + 1],
            })
            .unwrap();
        let (first, first_result) = mpsc::channel();
        sender.send(Command::Flush { reply: first }).unwrap();
        let (retry, retry_result) = mpsc::channel();
        sender.send(Command::Flush { reply: retry }).unwrap();
        let (clear, clear_result) = mpsc::channel();
        sender
            .send(Command::Clear {
                epoch: 1,
                reply: clear,
            })
            .unwrap();
        let (after, after_result) = mpsc::channel();
        sender.send(Command::Flush { reply: after }).unwrap();
        let (stop, stop_result) = mpsc::channel();
        sender.send(Command::Shutdown { reply: stop }).unwrap();
        writer_loop_with_interval(
            fixture.0.clone(),
            receiver,
            Arc::new(InputHistoryStats::default()),
            Duration::from_secs(3600),
        );
        assert!(first_result.try_recv().unwrap().is_err());
        assert!(retry_result.try_recv().unwrap().is_err());
        clear_result.try_recv().unwrap().unwrap();
        after_result.try_recv().unwrap().unwrap();
        stop_result.try_recv().unwrap().unwrap();
    }

    #[test]
    fn control_barrier_size_matrix() {
        // Repeated synthetic frames isolate storage-size cost, not ID or
        // language quality. Build outside the timed region, sync first, and
        // measure the same Flush+Shutdown pair at all three sizes.
        let timestamp = now_ms();
        let protected = protect(&key_record(1, timestamp).encode().unwrap()).unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&(protected.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc32(&protected).to_le_bytes());
        frame.extend_from_slice(&protected);
        let block = frame.repeat(128);
        for mib in [0u64, 16, 64] {
            let fixture = ReadFailureFixture(temporary_path("barrier-size"));
            ensure_file(&fixture.0).unwrap();
            let mut file = OpenOptions::new().append(true).open(&fixture.0).unwrap();
            let mut remaining = (mib * 1024 * 1024).saturating_sub(HEADER_LEN as u64);
            while remaining >= block.len() as u64 {
                file.write_all(&block).unwrap();
                remaining -= block.len() as u64;
            }
            while remaining >= frame.len() as u64 {
                file.write_all(&frame).unwrap();
                remaining -= frame.len() as u64;
            }
            file.sync_all().unwrap();
            let bytes = file.metadata().unwrap().len();
            drop(file);
            let (sender, receiver) = mpsc::sync_channel(2);
            let (flushed, flush_result) = mpsc::channel();
            let (stopped, stop_result) = mpsc::channel();
            sender.send(Command::Flush { reply: flushed }).unwrap();
            sender.send(Command::Shutdown { reply: stopped }).unwrap();
            // Actor/control benchmark: fixture encoding already establishes
            // this hint; do not time the test helper's startup scan.
            let append = open_append(&fixture.0).unwrap();
            let retention = RetentionPlan {
                oldest_timestamp_ms: (mib != 0).then_some(timestamp),
            };
            let started = Instant::now();
            writer_loop_with_file(
                fixture.0.clone(),
                receiver,
                Arc::new(InputHistoryStats::default()),
                Some(append),
                Duration::from_secs(3600),
                retention,
            );
            let elapsed = started.elapsed();
            flush_result.try_recv().unwrap().unwrap();
            stop_result.try_recv().unwrap().unwrap();
            assert_eq!(fs::metadata(&fixture.0).unwrap().len(), bytes);
            println!(
                "history-control-size target_mib={mib} bytes={bytes} flush_shutdown_us={}",
                elapsed.as_micros()
            );
        }
    }

    #[test]
    fn key_and_commit_records_roundtrip_through_dpapi() {
        let path = temporary_path("roundtrip");
        let service = InputHistoryService::open(&path).expect("open");
        service.record_key(
            7,
            ScopeClass::Normal,
            1,
            Some('\t'),
            0,
            false,
            false,
            true,
            0,
            1,
            1,
            1,
            "",
            "か",
            "",
            0,
            false,
            "char",
        );
        service.record_commit(7, ScopeClass::Normal, "かな", "仮名", 3, 4);
        service.flush().expect("flush");
        let snapshot = read_snapshot(&path).expect("snapshot");
        assert_eq!(snapshot.records.len(), 3);
        assert!(matches!(snapshot.records[0], InputHistoryRecord::Engine(_)));
        assert!(matches!(snapshot.records[1], InputHistoryRecord::Key(_)));
        assert!(matches!(snapshot.records[2], InputHistoryRecord::Commit(_)));
        let InputHistoryRecord::Engine(engine) = &snapshot.records[0] else {
            panic!("expected engine record");
        };
        assert_eq!(engine.package_version, ENGINE_PACKAGE_VERSION);
        assert!(!engine.release_label.is_empty());
        let InputHistoryRecord::Key(key) = &snapshot.records[1] else {
            panic!("expected key record");
        };
        assert_eq!(key.sequence, 2);
        assert_eq!(key.session, 7);
        assert_eq!(key.scope, ScopeClass::Normal);
        assert_eq!(key.key_code, 1);
        assert_eq!(key.character, Some('\t'));
        assert!(key.consumed);
        assert_eq!(key.state_before, 0);
        assert_eq!(key.state_after, 1);
        assert_eq!(key.mode_before, 1);
        assert_eq!(key.mode_after, 1);
        assert!(key.preedit_before.is_empty());
        assert!(!key.preedit_after.is_empty());
        assert!(key.commit.is_empty());
        assert_eq!(key.action, "char");
        let InputHistoryRecord::Commit(commit) = &snapshot.records[2] else {
            panic!("expected commit record");
        };
        assert_eq!(commit.sequence, 3);
        assert_eq!(commit.session, 7);
        assert_eq!(commit.scope, ScopeClass::Normal);
        assert_eq!(commit.left_context, 3);
        assert_eq!(commit.right_context, 4);
        assert!(!commit.reading.is_empty());
        assert!(!commit.surface.is_empty());
        let tsv = snapshot.to_tsv();
        assert!(tsv.contains(&format!("# package-version: {ENGINE_PACKAGE_VERSION}")));
        assert!(tsv.contains("# release-label:"));
        assert!(tsv.contains("engine-package-version\tengine-release-label"));
        let tsv_lines: Vec<_> = tsv.lines().collect();
        // 4 comment lines + header + engine + key + commit
        assert_eq!(tsv_lines.len(), 8);
        assert_eq!(tsv_lines[4].split('\t').count(), 40);
        assert!(tsv_lines[6].contains("\t\\t\t"));
        for line in &tsv_lines[5..] {
            assert_eq!(line.split('\t').count(), 40);
        }
        service.stop().expect("stop");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ai_records_roundtrip_and_aggregate_logical_requests_attempts_and_tokens() {
        let path = temporary_path("ai-roundtrip");
        let service = InputHistoryService::open(&path).expect("open");
        service.record_ai_text(
            9,
            ScopeClass::Normal,
            AiTextOperation::Proofread,
            AiTextStatus::Applied,
            "元",
            "結果",
            "gpt-5.6-luna",
            "openai",
            "technical",
            "",
            123,
            17,
            5,
            3,
            2,
            false,
        );
        let stats = service.stats().snapshot();
        assert_eq!(stats.ai_requests, 1);
        assert_eq!(stats.ai_attempts, 2);
        assert_eq!(stats.ai_input_tokens, 17);
        assert_eq!(stats.ai_output_tokens, 5);
        assert_eq!(stats.ai_cached_tokens, 3);
        service.flush().expect("flush");
        let snapshot = read_snapshot(&path).expect("read");
        let ai = snapshot
            .records
            .iter()
            .find_map(|record| match record {
                InputHistoryRecord::AiText(record) => Some(record),
                _ => None,
            })
            .expect("AI record");
        assert_eq!(ai.operation, AiTextOperation::Proofread);
        assert_eq!(ai.status, AiTextStatus::Applied);
        assert_eq!(ai.model, "gpt-5.6-luna");
        assert_eq!(ai.attempts, 2);
        assert_eq!(
            snapshot
                .to_tsv()
                .lines()
                .last()
                .expect("TSV")
                .split('\t')
                .count(),
            40
        );
        service.stop().expect("stop");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn repair_reads_at_most_the_size_cap_and_truncates_an_over_cap_history_file() {
        // Appends enforce MAX_INPUT_HISTORY_BYTES, so an over-cap file can
        // only come from corruption or external tampering. Repair must not
        // load the oversized tail (the read is capped) and must still
        // truncate the file back to its last valid frame.
        let path = temporary_path("overcap");
        append_records(&path, &[key_record(1, 1), key_record(2, 2)]);
        let valid_len = fs::metadata(&path).expect("metadata").len();
        let file = OpenOptions::new().write(true).open(&path).expect("open");
        file.set_len(MAX_INPUT_HISTORY_BYTES + 4096).expect("grow");
        drop(file);

        repair_file(&path).expect("repair");

        assert_eq!(fs::metadata(&path).expect("metadata").len(), valid_len);
        let snapshot = read_snapshot(&path).expect("snapshot");
        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(snapshot.ignored_tail_bytes, 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn scope_classification_is_fail_closed_for_unclassified_and_sensitive_scopes() {
        assert_eq!(
            ScopeClass::from_scope(InputScope::Normal, false),
            ScopeClass::Unclassified
        );
        assert_eq!(
            ScopeClass::from_scope(InputScope::Normal, true),
            ScopeClass::Normal
        );
        for scope in [
            InputScope::Password,
            InputScope::Url,
            InputScope::Email,
            InputScope::Digits,
        ] {
            assert_eq!(ScopeClass::from_scope(scope, true), ScopeClass::Sensitive);
            assert_eq!(ScopeClass::from_scope(scope, false), ScopeClass::Sensitive);
        }
    }

    #[test]
    fn counter_exhaustion_last_session_id_is_allocated_once_concurrently() {
        let fixture = ReadFailureFixture(temporary_path("last-session"));
        let service = InputHistoryService::open(&fixture.0).unwrap();
        service
            .next_session_id
            .store(u64::MAX - 1, Ordering::Relaxed);
        let results = thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| service.allocate_session_id()))
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        service.stop().unwrap();
        assert_eq!(
            results.iter().filter(|id| **id == Some(u64::MAX)).count(),
            1
        );
        assert_eq!(results.iter().filter(|id| id.is_none()).count(), 7);
        assert_eq!(service.allocate_session_id(), None);
        assert_eq!(service.stats.persistence_failures(), 8);
        assert_eq!(service.next_session_id.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn counter_exhaustion_last_sequence_and_clear_epoch_are_terminal() {
        let fixture = ReadFailureFixture(temporary_path("last-sequence-epoch"));
        let service = InputHistoryService::open(&fixture.0).unwrap();
        service.next_sequence.store(u64::MAX - 1, Ordering::Relaxed);
        service.record_commit(1, ScopeClass::Normal, "a", "a", 0, 0);
        service.record_commit(1, ScopeClass::Normal, "b", "b", 0, 0);
        service.flush().unwrap();
        let snapshot = read_snapshot(&fixture.0).unwrap();
        service.epoch.store(u64::MAX - 1, Ordering::Release);
        let last_clear = service.clear();
        let rejected_clear = service.clear();
        service.record_commit(1, ScopeClass::Normal, "c", "c", 0, 0);
        service.stop().unwrap();
        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(snapshot.records[1].sequence(), u64::MAX);
        assert_eq!(last_clear.unwrap(), 2);
        assert!(rejected_clear.is_err());
        assert!(read_snapshot(&fixture.0).unwrap().records.is_empty());
        assert_eq!(service.next_sequence.load(Ordering::Relaxed), u64::MAX);
        assert_eq!(service.epoch.load(Ordering::Acquire), u64::MAX);
        assert_eq!(service.stats.persistence_failures(), 3);
    }

    #[test]
    fn counter_exhaustion_unavailable_session_rejects_all_content_variants() {
        let fixture = ReadFailureFixture(temporary_path("unavailable-session"));
        let service = InputHistoryService::open(&fixture.0).unwrap();
        service.record_commit(0, ScopeClass::Normal, "a", "a", 0, 0);
        service.record_key(
            0,
            ScopeClass::Normal,
            1,
            Some('a'),
            0,
            false,
            false,
            true,
            1,
            1,
            1,
            1,
            "",
            "a",
            "",
            0,
            false,
            "char",
        );
        service.record_ai_text(
            0,
            ScopeClass::Normal,
            AiTextOperation::Proofread,
            AiTextStatus::Applied,
            "a",
            "b",
            "model",
            "provider",
            "style",
            "",
            1,
            1,
            1,
            0,
            1,
            false,
        );
        service.stop().unwrap();
        let snapshot = read_snapshot(&fixture.0).unwrap();
        assert_eq!(
            snapshot.records.len(),
            1,
            "only engine marker may use session zero"
        );
        assert!(matches!(snapshot.records[0], InputHistoryRecord::Engine(_)));
        assert_eq!(service.stats.dropped_events(), 3);
        assert_eq!(service.next_sequence.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn counter_exhaustion_session_never_panics_or_wraps() {
        let fixture = ReadFailureFixture(temporary_path("session-exhausted"));
        let service = InputHistoryService::open(&fixture.0).unwrap();
        service.next_session_id.store(u64::MAX, Ordering::Relaxed);
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            service.allocate_session_id()
        }));
        let after = service.next_session_id.load(Ordering::Relaxed);
        service.stop().unwrap();
        assert!(attempt.is_ok(), "optional history allocation panicked");
        assert_eq!(after, u64::MAX, "session ID counter wrapped");
    }

    #[test]
    fn counter_exhaustion_record_variants_never_panic_or_wrap() {
        let fixture = ReadFailureFixture(temporary_path("sequence-exhausted"));
        let service = InputHistoryService::open(&fixture.0).unwrap();
        service.flush().unwrap();
        let before = fs::read(&fixture.0).unwrap();
        let mut outcomes = Vec::new();
        for variant in 0..4 {
            service.next_sequence.store(u64::MAX, Ordering::Relaxed);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match variant {
                0 => service.record_engine_start(),
                1 => service.record_commit(1, ScopeClass::Normal, "a", "a", 0, 0),
                2 => service.record_key(
                    1,
                    ScopeClass::Normal,
                    1,
                    Some('a'),
                    0,
                    false,
                    false,
                    true,
                    1,
                    1,
                    1,
                    1,
                    "",
                    "a",
                    "",
                    0,
                    false,
                    "char",
                ),
                _ => service.record_ai_text(
                    1,
                    ScopeClass::Normal,
                    AiTextOperation::Proofread,
                    AiTextStatus::Applied,
                    "a",
                    "b",
                    "model",
                    "provider",
                    "style",
                    "",
                    1,
                    1,
                    1,
                    0,
                    1,
                    false,
                ),
            }));
            outcomes.push((
                result.is_ok(),
                service.next_sequence.load(Ordering::Relaxed),
            ));
        }
        service.stop().unwrap();
        assert!(
            outcomes.iter().all(|&(ok, n)| ok && n == u64::MAX),
            "record counter outcomes {outcomes:?}"
        );
        assert_eq!(fs::read(&fixture.0).unwrap(), before);
    }

    #[test]
    fn counter_exhaustion_clear_preserves_bytes_and_epoch() {
        let fixture = ReadFailureFixture(temporary_path("epoch-exhausted"));
        let service = InputHistoryService::open(&fixture.0).unwrap();
        service.flush().unwrap();
        let before = fs::read(&fixture.0).unwrap();
        service.epoch.store(u64::MAX, Ordering::Release);
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| service.clear()));
        let after = service.epoch.load(Ordering::Acquire);
        service.stop().unwrap();
        assert!(
            matches!(attempt, Ok(Err(_))),
            "exhausted Clear must return an error"
        );
        assert_eq!(after, u64::MAX);
        assert_eq!(fs::read(&fixture.0).unwrap(), before);
    }

    #[test]
    fn session_ids_are_shared_by_the_history_service() {
        let path = temporary_path("session");
        let service = InputHistoryService::open(&path).expect("open");
        let first = service.allocate_session_id();
        let second = service.allocate_session_id();
        assert_eq!(first, Some(1));
        assert_eq!(second, Some(2));
        service.stop().expect("stop");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn sensitive_scope_is_rejected_and_clear_is_race_safe() {
        let path = temporary_path("clear");
        let service = InputHistoryService::open(&path).expect("open");
        service.record_key(
            1,
            ScopeClass::Unclassified,
            1,
            Some('u'),
            0,
            false,
            true,
            true,
            1,
            1,
            1,
            1,
            "",
            "u",
            "",
            0,
            false,
            "char",
        );
        service.record_commit(1, ScopeClass::Unclassified, "u", "u", 0, 0);
        service.record_key(
            1,
            ScopeClass::Sensitive,
            1,
            Some('x'),
            0,
            false,
            false,
            true,
            1,
            1,
            1,
            1,
            "",
            "x",
            "",
            0,
            false,
            "char",
        );
        service.record_key(
            1,
            ScopeClass::Normal,
            1,
            Some('x'),
            0,
            false,
            false,
            true,
            1,
            1,
            1,
            1,
            "",
            "x",
            "",
            0,
            false,
            "char",
        );
        let stats = service.stats().snapshot();
        assert_eq!(stats.excluded_unclassified_events, 1);
        assert_eq!(stats.excluded_sensitive_events, 1);
        assert_eq!(stats.excluded_test_only_events, 1);
        service.flush().expect("flush before clear");
        assert_eq!(service.clear().expect("clear"), 2);
        service.flush().expect("flush");
        assert!(read_snapshot(&path).expect("snapshot").records.is_empty());
        service.stop().expect("stop");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn retention_removes_old_records_even_when_the_file_is_small() {
        let path = temporary_path("retention");
        let now = now_ms();
        let old = now.saturating_sub(RETENTION.as_millis() as u64 + 1);
        append_records(&path, &[key_record(1, old), key_record(2, now)]);

        compact_file(&path).expect("compact retention");

        let snapshot = read_snapshot(&path).expect("snapshot");
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.records[0].sequence(), 2);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn idle_writer_compacts_old_records_without_new_commands() {
        let path = temporary_path("idle-retention");
        let now = now_ms();
        let old = now.saturating_sub(RETENTION.as_millis() as u64 + 1);
        append_records(&path, &[key_record(1, old), key_record(2, now)]);

        let (sender, receiver) = mpsc::sync_channel(1);
        let stats = Arc::new(InputHistoryStats::default());
        let writer_stats = Arc::clone(&stats);
        let writer_path = path.clone();
        let worker = thread::spawn(move || {
            writer_loop_with_interval(
                writer_path,
                receiver,
                writer_stats,
                Duration::from_millis(20),
            )
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(snapshot) = read_snapshot(&path) {
                if snapshot.records.len() == 1 {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "idle retention did not run");
            thread::sleep(Duration::from_millis(10));
        }

        drop(sender);
        worker.join().expect("idle writer");
        let snapshot = read_snapshot(&path).expect("snapshot");
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.records[0].sequence(), 2);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn persistence_failures_remain_visible_after_the_writer_stops() {
        let path = temporary_path("failure-stats");
        let service = InputHistoryService::open(&path).expect("open");
        service.stop().expect("stop");
        service.record_key(
            1,
            ScopeClass::Normal,
            1,
            Some('x'),
            0,
            false,
            false,
            true,
            0,
            1,
            1,
            1,
            "",
            "x",
            "",
            0,
            false,
            "char",
        );
        assert_eq!(service.stats().persistence_failures(), 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_format_v1_files_remain_readable() {
        let path = temporary_path("legacy-v1");
        append_records(&path, &[key_record(1, now_ms())]);
        let mut bytes = fs::read(&path).expect("read");
        bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
        fs::write(&path, &bytes).expect("downgrade header");

        let snapshot = read_snapshot(&path).expect("read v1");
        assert_eq!(snapshot.format_version, 1);
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.last_engine_identity(), None);
        let tsv = snapshot.to_tsv();
        assert!(tsv.contains("# sakura-input-history-format: 1"));
        assert!(tsv.contains("# package-version: -"));
        assert!(tsv.contains("# release-label: -"));
        repair_file(&path).expect("repair v1");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        compact_file(&path).expect("compact v1");
        assert_eq!(read_snapshot(&path).unwrap().records, snapshot.records);
        let service = InputHistoryService::open(&path).expect("open compacted v1");
        service.stop().expect("stop compacted v1");
        assert!(read_snapshot(&path)
            .unwrap()
            .records
            .contains(&snapshot.records[0]));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn installed_release_dir_names_require_version_and_hex_build_id() {
        assert!(is_installed_release_dir("1.0.11-932aee9cf49964eb"));
        assert!(!is_installed_release_dir("1.0.11"));
        assert!(!is_installed_release_dir("1.0.11-not-hex-buildid"));
        assert!(!is_installed_release_dir("target"));
    }
}
