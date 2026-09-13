use std::sync::atomic::{AtomicU64, Ordering};

use sakura_values::{AiTextOperation, AiTextStatus};

pub const MAX_RECORD_BYTES: usize = 16 * 1024;
pub const INPUT_HISTORY_FORMAT_VERSION: u16 = 2;
pub const INPUT_HISTORY_FORMAT_VERSION_MIN: u16 = 1;
pub const HISTORY_MAGIC: [u8; 4] = *b"SKIH";
pub const HISTORY_HEADER_LEN: usize = 8;
pub const HISTORY_FRAME_HEADER_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HistoryScope {
    Unclassified = 0,
    Normal = 1,
    Sensitive = 2,
}

impl HistoryScope {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unclassified => "unclassified",
            Self::Normal => "normal",
            Self::Sensitive => "sensitive",
        }
    }
    pub(crate) fn from_u8(value: u8) -> std::io::Result<Self> {
        match value {
            0 => Ok(Self::Unclassified),
            1 => Ok(Self::Normal),
            2 => Ok(Self::Sensitive),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unknown input history scope",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyHistoryRecord {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub session: u64,
    pub scope: HistoryScope,
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
    pub scope: HistoryScope,
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
    pub scope: HistoryScope,
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
    pub scope: HistoryScope,
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
    pub const fn session(&self) -> u64 {
        match self {
            Self::Key(record) => record.session,
            Self::Commit(record) => record.session,
            Self::AiText(record) => record.session,
            Self::Engine(record) => record.session,
        }
    }

    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Key(record) => record.sequence,
            Self::Commit(record) => record.sequence,
            Self::AiText(record) => record.sequence,
            Self::Engine(record) => record.sequence,
        }
    }

    pub const fn timestamp_ms(&self) -> u64 {
        match self {
            Self::Key(record) => record.timestamp_ms,
            Self::Commit(record) => record.timestamp_ms,
            Self::AiText(record) => record.timestamp_ms,
            Self::Engine(record) => record.timestamp_ms,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputHistorySnapshot {
    pub format_version: u16,
    pub records: Vec<InputHistoryRecord>,
    pub ignored_tail_bytes: usize,
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

    fn excluded_unclassified_events(&self) -> u64 {
        self.excluded_unclassified_events.load(Ordering::Relaxed)
    }

    fn excluded_sensitive_events(&self) -> u64 {
        self.excluded_sensitive_events.load(Ordering::Relaxed)
    }

    fn excluded_test_only_events(&self) -> u64 {
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

    pub fn record_drop(&self) {
        self.dropped_events.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_persistence_failure(&self) {
        self.persistence_failures.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_excluded_unclassified(&self) {
        self.excluded_unclassified_events
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_excluded_sensitive(&self) {
        self.excluded_sensitive_events
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_excluded_test_only(&self) {
        self.excluded_test_only_events
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_ai_usage(&self, attempts: u32, input: u32, output: u32, cached: u32) {
        self.ai_requests.fetch_add(1, Ordering::Relaxed);
        self.ai_attempts
            .fetch_add(u64::from(attempts), Ordering::Relaxed);
        self.ai_input_tokens
            .fetch_add(u64::from(input), Ordering::Relaxed);
        self.ai_output_tokens
            .fetch_add(u64::from(output), Ordering::Relaxed);
        self.ai_cached_tokens
            .fetch_add(u64::from(cached), Ordering::Relaxed);
    }
}
