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
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_proto::InputScope;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
};

const MAGIC: &[u8; 4] = b"SKIH";
const HEADER_LEN: usize = 8;
const FRAME_HEADER_LEN: usize = 8;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const QUEUE_CAPACITY: usize = 1024;
const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const COMPACTION_INTERVAL: Duration = Duration::from_secs(60);
const COMPACTION_APPEND_LIMIT: u32 = 256;

pub const INPUT_HISTORY_FORMAT_VERSION: u16 = 1;
pub const MAX_INPUT_HISTORY_BYTES: u64 = 64 * 1024 * 1024;

const RECORD_KEY: u8 = 1;
const RECORD_COMMIT: u8 = 2;

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
pub enum InputHistoryRecord {
    Key(KeyHistoryRecord),
    Commit(CommitHistoryRecord),
}

impl InputHistoryRecord {
    fn sequence(&self) -> u64 {
        match self {
            Self::Key(record) => record.sequence,
            Self::Commit(record) => record.sequence,
        }
    }

    fn timestamp_ms(&self) -> u64 {
        match self {
            Self::Key(record) => record.timestamp_ms,
            Self::Commit(record) => record.timestamp_ms,
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
    pub fn to_tsv(&self) -> String {
        let mut output = String::from(
            "# sakura-input-history-format: 1\n# records are DPAPI-protected on disk\n\
kind\tsequence\ttimestamp-ms\tsession\tscope\tkey-code\tcharacter\tmodifiers\t\
repeat\tconsumed\tstate-before\tstate-after\tmode-before\tmode-after\tpreedit-before\t\
preedit-after\tcommit\tdelete-before\tbeep\taction\tdropped-before\treading\tsurface\t\
left-context\tright-context\n",
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
                    fields.extend((0..4).map(|_| String::new()));
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
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputHistoryStatsSnapshot {
    pub dropped_events: u64,
    pub persistence_failures: u64,
    pub excluded_unclassified_events: u64,
    pub excluded_sensitive_events: u64,
    pub excluded_test_only_events: u64,
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
    worker: Mutex<Option<JoinHandle<()>>>,
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
        ensure_file(path)?;
        repair_file(path)?;
        compact_file(path)?;

        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let stats = Arc::new(InputHistoryStats::default());
        let writer_stats = Arc::clone(&stats);
        let writer_path = path.to_owned();
        let worker = thread::Builder::new()
            .name("sakura-input-history".to_owned())
            .spawn(move || writer_loop(writer_path, receiver, writer_stats))
            .map_err(|error| io::Error::other(format!("start input history writer: {error}")))?;

        Ok(Arc::new(Self {
            path: path.to_owned(),
            sender,
            stats,
            next_sequence: AtomicU64::new(next_sequence(path).unwrap_or(0)),
            next_session_id: AtomicU64::new(next_session_id(path).unwrap_or(0)),
            epoch: AtomicU64::new(0),
            worker: Mutex::new(Some(worker)),
        }))
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
    pub fn allocate_session_id(&self) -> u64 {
        self.next_session_id.fetch_add(1, Ordering::Relaxed) + 1
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
        if self.stats.excludes(scope, test_only) {
            return;
        }
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed) + 1;
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
        let Ok(payload) = record.encode() else {
            self.stats
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        let command = Command::Append {
            epoch: self.epoch.load(Ordering::Acquire),
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

    pub fn record_commit(
        &self,
        session: u64,
        scope: ScopeClass,
        reading: &str,
        surface: &str,
        left_context: u16,
        right_context: u16,
    ) {
        if self.stats.excludes(scope, false) || reading.is_empty() || surface.is_empty() {
            return;
        }
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed) + 1;
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
        let Ok(payload) = record.encode() else {
            self.stats
                .persistence_failures
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        let command = Command::Append {
            epoch: self.epoch.load(Ordering::Acquire),
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
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
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
        if self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none()
        {
            return Ok(());
        }
        let (reply, receiver) = mpsc::channel();
        let send = self.sender.send(Command::Shutdown { reply });
        let result = match send {
            Ok(()) => receiver.recv().map_err(|_| {
                self.stats
                    .persistence_failures
                    .fetch_add(1, Ordering::Relaxed);
                io::Error::new(io::ErrorKind::BrokenPipe, "input history writer stopped")
            })?,
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
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = worker.join();
        }
        result
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
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_INPUT_HISTORY_BYTES {
        return Err(invalid_data("input history exceeds its hard size bound"));
    }
    let bytes = fs::read(path)?;
    scan_snapshot(&bytes)
}

pub fn clear_path(path: &Path) -> io::Result<u64> {
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

fn writer_loop(path: PathBuf, receiver: Receiver<Command>, stats: Arc<InputHistoryStats>) {
    writer_loop_with_interval(path, receiver, stats, COMPACTION_INTERVAL);
}

fn writer_loop_with_interval(
    path: PathBuf,
    receiver: Receiver<Command>,
    stats: Arc<InputHistoryStats>,
    compaction_interval: Duration,
) {
    let mut file = open_append(&path).ok();
    let mut cleared_epoch = 0;
    let mut last_compaction = Instant::now();
    let mut appends_since_compaction = 0u32;
    loop {
        let wait = compaction_interval.saturating_sub(last_compaction.elapsed());
        let command = match receiver.recv_timeout(wait) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => {
                if compact_writer_file(&path, &mut file).is_err() {
                    stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
                }
                appends_since_compaction = 0;
                last_compaction = Instant::now();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match command {
            Command::Append { epoch, payload } => {
                if epoch < cleared_epoch {
                    continue;
                }
                let result = append_payload(&path, &mut file, &payload);
                match result {
                    Ok(()) => {
                        appends_since_compaction = appends_since_compaction.saturating_add(1);
                        if appends_since_compaction >= COMPACTION_APPEND_LIMIT
                            || last_compaction.elapsed() >= COMPACTION_INTERVAL
                        {
                            if compact_writer_file(&path, &mut file).is_err() {
                                stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
                            }
                            appends_since_compaction = 0;
                            last_compaction = Instant::now();
                        }
                    }
                    Err(_) => {
                        stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Command::Flush { reply } => {
                // A manual flush is also a bounded retention checkpoint. This
                // makes `history export` deterministic even when the writer
                // has seen fewer than COMPACTION_APPEND_LIMIT events.
                let result = compact_writer_file(&path, &mut file).and_then(|()| {
                    if let Some(file) = file.as_mut() {
                        file.flush()
                    } else {
                        Ok(())
                    }
                });
                if result.is_err() {
                    stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
                }
                let _ = reply.send(result);
                appends_since_compaction = 0;
                last_compaction = Instant::now();
            }
            Command::Clear { epoch, reply } => {
                cleared_epoch = cleared_epoch.max(epoch);
                let result = clear_writer_file(&path, &mut file);
                if result.is_err() {
                    stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
                }
                let _ = reply.send(result);
                appends_since_compaction = 0;
                last_compaction = Instant::now();
            }
            Command::Shutdown { reply } => {
                let result = compact_writer_file(&path, &mut file).and_then(|()| {
                    if let Some(file) = file.as_mut() {
                        file.flush()
                    } else {
                        Ok(())
                    }
                });
                if result.is_err() {
                    stats.persistence_failures.fetch_add(1, Ordering::Relaxed);
                }
                let _ = reply.send(result);
                break;
            }
        }
    }
}

/// Compacts while the writer owns the only open handle to the history file.
/// The handle is restored even when compaction fails, so a transient rename or
/// DPAPI error has an explicit failure outcome without permanently disabling
/// later appends.
fn compact_writer_file(path: &Path, file: &mut Option<File>) -> io::Result<()> {
    if let Some(mut previous) = file.take() {
        previous.flush()?;
    }
    let compaction = compact_file(path);
    let reopened = open_append(path);
    match (compaction, reopened) {
        (Ok(()), Ok(handle)) => {
            *file = Some(handle);
            Ok(())
        }
        (Err(error), Ok(handle)) => {
            *file = Some(handle);
            Err(error)
        }
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(_)) => Err(error),
    }
}

fn append_payload(path: &Path, file: &mut Option<File>, payload: &[u8]) -> io::Result<()> {
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
        compact_file(path)?;
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
    append_encrypted(file, &protected)
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
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
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

fn repair_file(path: &Path) -> io::Result<()> {
    let bytes = fs::read(path)?;
    if bytes.is_empty() {
        return ensure_file(path);
    }
    let (_, valid_end) = scan_bytes(&bytes)?;
    if valid_end < bytes.len() {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(valid_end as u64)?;
    }
    Ok(())
}

fn scan_snapshot(bytes: &[u8]) -> io::Result<InputHistorySnapshot> {
    let (records, valid_end) = scan_bytes(bytes)?;
    Ok(InputHistorySnapshot {
        format_version: INPUT_HISTORY_FORMAT_VERSION,
        records,
        ignored_tail_bytes: bytes.len().saturating_sub(valid_end),
    })
}

fn scan_bytes(bytes: &[u8]) -> io::Result<(Vec<InputHistoryRecord>, usize)> {
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err(invalid_data("invalid input history header"));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != INPUT_HISTORY_FORMAT_VERSION {
        return Err(invalid_data("unsupported input history format"));
    }
    let mut records = Vec::new();
    let mut offset = HEADER_LEN;
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
        let Ok(payload) = unprotect(encrypted) else {
            break;
        };
        let Ok(record) = InputHistoryRecord::decode(&payload) else {
            break;
        };
        records.push(record);
        offset = payload_end;
    }
    Ok((records, offset))
}

fn compact_file(path: &Path) -> io::Result<()> {
    let snapshot = match read_snapshot(path) {
        Ok(snapshot) => snapshot,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let cutoff = now_ms().saturating_sub(RETENTION.as_millis() as u64);
    let mut records: Vec<_> = snapshot
        .records
        .into_iter()
        .filter(|record| record.timestamp_ms() >= cutoff)
        .collect();
    records.sort_by_key(InputHistoryRecord::sequence);
    if records.is_empty() {
        return clear_path(path).map(|_| ());
    }

    let mut encoded = Vec::with_capacity(records.len());
    let mut total = HEADER_LEN as u64;
    for record in records.into_iter().rev() {
        let payload = record.encode()?;
        let protected = protect(&payload)?;
        let frame_len = FRAME_HEADER_LEN as u64 + protected.len() as u64;
        if total.saturating_add(frame_len) > MAX_INPUT_HISTORY_BYTES {
            break;
        }
        total += frame_len;
        encoded.push(protected);
    }
    encoded.reverse();
    let temp = path.with_extension("compact.tmp");
    let mut replacement = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temp)?;
    replacement.write_all(&header())?;
    for payload in &encoded {
        append_encrypted(&mut replacement, payload)?;
    }
    replacement.flush()?;
    drop(replacement);
    let _ = fs::remove_file(path);
    fs::rename(temp, path)
}

fn next_sequence(path: &Path) -> io::Result<u64> {
    Ok(read_snapshot(path)?
        .records
        .iter()
        .map(InputHistoryRecord::sequence)
        .max()
        .unwrap_or(0))
}

fn next_session_id(path: &Path) -> io::Result<u64> {
    Ok(read_snapshot(path)?
        .records
        .iter()
        .map(|record| match record {
            InputHistoryRecord::Key(record) => record.session,
            InputHistoryRecord::Commit(record) => record.session,
        })
        .max()
        .unwrap_or(0))
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
        assert_eq!(snapshot.records.len(), 2);
        assert!(matches!(snapshot.records[0], InputHistoryRecord::Key(_)));
        assert!(matches!(snapshot.records[1], InputHistoryRecord::Commit(_)));
        let InputHistoryRecord::Key(key) = &snapshot.records[0] else {
            panic!("expected key record");
        };
        assert_eq!(key.sequence, 1);
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
        let InputHistoryRecord::Commit(commit) = &snapshot.records[1] else {
            panic!("expected commit record");
        };
        assert_eq!(commit.sequence, 2);
        assert_eq!(commit.session, 7);
        assert_eq!(commit.scope, ScopeClass::Normal);
        assert_eq!(commit.left_context, 3);
        assert_eq!(commit.right_context, 4);
        assert!(!commit.reading.is_empty());
        assert!(!commit.surface.is_empty());
        let tsv = snapshot.to_tsv();
        let tsv_lines: Vec<_> = tsv.lines().collect();
        assert_eq!(tsv_lines.len(), 5);
        assert_eq!(tsv_lines[2].split('\t').count(), 25);
        assert!(tsv_lines[3].contains("\t\\t\t"));
        for line in &tsv_lines[3..] {
            assert_eq!(line.split('\t').count(), 25);
        }
        service.stop().expect("stop");
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
    fn session_ids_are_shared_by_the_history_service() {
        let path = temporary_path("session");
        let service = InputHistoryService::open(&path).expect("open");
        let first = service.allocate_session_id();
        let second = service.allocate_session_id();
        assert_eq!(first, 1);
        assert_eq!(second, 2);
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
        assert_eq!(service.clear().expect("clear"), 1);
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
}
