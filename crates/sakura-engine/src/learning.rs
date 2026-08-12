//! Bounded persistent personalization (DESIGN §5.4).
//!
//! The on-disk log is the source of truth. Each record is length- and CRC32-
//! prefixed; startup truncates only a torn/corrupt tail after the last verified
//! record. The in-memory index is a fixed four-way set-associative table, so
//! both memory and lookup work stay O(1) as history grows. A joined maintenance
//! thread flushes and compacts the source log before its hard disk ceiling.

#[cfg(test)]
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use sakura_proto::FixedStr;

use crate::session::text_hash;

const MAGIC: &[u8; 4] = b"SKLR";
const HEADER_LEN: usize = 8;
pub const LEARNING_FORMAT_VERSION: u16 = 3;
const FORMAT_VERSION_1: u16 = 1;
const FORMAT_VERSION_2: u16 = 2;
const RECORD_ENVELOPE_LEN: usize = 8;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const BUCKETS: usize = 32_768;
const WAYS: usize = 3;
const SLOT_COUNT: usize = BUCKETS * WAYS;
pub const MAX_LEARNING_ENTRIES: usize = SLOT_COUNT;
pub const MAX_LEARNING_LOG_BYTES: u64 = 20 * 1024 * 1024;
/// Fixed number of exact `(reading, surface)` entries retained for prediction.
///
/// This is a storage and ranking window, not a per-result display limit.  The
/// prediction module independently caps how many retained entries can appear
/// in one suggestion result.
const MAX_PREDICTION_HISTORY_ENTRIES: usize = 128;
const MAX_HISTORY_TEXT_BYTES: usize = 512;
const COMPACTION_TRIGGER_BYTES: u64 = 16 * 1024 * 1024;
const COMPACTION_TARGET_BYTES: usize = 8 * 1024 * 1024;
const MAX_LOG_RECORDS: u64 = 50_000;
const TARGET_LOG_RECORDS: u64 = 40_000;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5);
const RECORD_COMMIT: u8 = 1;
/// A learned choice loses half of its effective evidence every 30 days.
///
/// The same decay applies to exact-context and general preferences.  Exact
/// context remains more specific, but an old one-off choice must not override
/// the converter's current grammatical ranking indefinitely.
const LEARNING_HALF_LIFE_DAYS: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LearningPreference {
    pub exact: Option<usize>,
    pub general: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct LearningKey {
    general: bool,
    left_context: u16,
    right_context: u16,
    reading_hash: u64,
    reading_len: u16,
    surface_hash: u64,
    surface_len: u16,
}

#[derive(Debug, Clone, Copy)]
struct PreferenceQuery {
    general: bool,
    left_context: u16,
    reading_hash: u64,
    reading_len: u16,
    day: u32,
    exact: bool,
}

/// How much evidence a learned choice has after recency decay.
///
/// Candidate indices are the converter's unpersonalized order, so this is a
/// guardrail rather than another independent score scale.  A weak choice may
/// affect only an already-near candidate; repeated, recent exact-context
/// choices earn a wider influence.  General (context-free) learning is always
/// more conservative because it has less evidence about the current sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LearningStrength {
    Weak,
    Medium,
    Strong,
}

impl LearningStrength {
    fn from_effective_frequency(frequency: u32) -> Option<Self> {
        match frequency {
            0 => None,
            1 => Some(Self::Weak),
            2 => Some(Self::Medium),
            _ => Some(Self::Strong),
        }
    }

    /// Highest zero-based base-candidate index this strength may select.
    ///
    /// Exact context has the previous grammatical connection and the learned
    /// candidate's right connection, so repeated evidence can eventually win.
    /// General learning deliberately remains bounded: it must never transplant
    /// a far-down candidate into an unrelated context solely on global history.
    const fn maximum_candidate_index(self, exact_context: bool) -> usize {
        match (exact_context, self) {
            (true, Self::Weak) => 1,
            (true, Self::Medium) => 3,
            (true, Self::Strong) => usize::MAX,
            (false, Self::Weak) => 0,
            (false, Self::Medium) => 2,
            (false, Self::Strong) => 5,
        }
    }
}

fn effective_learning_frequency(frequency: u32, last_seen_day: u32, day: u32) -> u32 {
    let half_lives = day.saturating_sub(last_seen_day) / LEARNING_HALF_LIFE_DAYS;
    frequency.checked_shr(half_lives).unwrap_or(0)
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    occupied: bool,
    general: bool,
    left_context: u16,
    right_context: u16,
    reading_len: u16,
    surface_len: u16,
    reading_hash: u64,
    surface_hash: u64,
    frequency: u32,
    last_seen_day: u32,
    sequence: u64,
}

impl Slot {
    fn matches(self, key: LearningKey) -> bool {
        self.occupied
            && self.general == key.general
            && self.left_context == key.left_context
            && self.right_context == key.right_context
            && self.reading_hash == key.reading_hash
            && self.reading_len == key.reading_len
            && self.surface_hash == key.surface_hash
            && self.surface_len == key.surface_len
    }
}

struct Index {
    slots: Box<[Slot]>,
    len: usize,
}

impl core::fmt::Debug for Index {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Index").field("len", &self.len).finish()
    }
}

impl Index {
    fn new() -> Self {
        let mut slots = Vec::with_capacity(SLOT_COUNT);
        slots.resize(SLOT_COUNT, Slot::default());
        Self {
            slots: slots.into_boxed_slice(),
            len: 0,
        }
    }

    fn bucket(key: LearningKey) -> usize {
        let mut hash = key.reading_hash.rotate_left(17) ^ key.surface_hash.rotate_right(11);
        hash ^= u64::from(key.left_context) << 32;
        hash ^= u64::from(key.right_context) << 48;
        hash ^= u64::from(key.general) << 63;
        hash ^= u64::from(key.reading_len) << 16 | u64::from(key.surface_len);
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
        hash ^= hash >> 33;
        (hash as usize) & (BUCKETS - 1)
    }

    fn learn(
        &mut self,
        left_context: u16,
        right_context: u16,
        reading: &str,
        surface: &str,
        day: u32,
        sequence: u64,
    ) {
        let Ok(reading_len) = u16::try_from(reading.len()) else {
            return;
        };
        let Ok(surface_len) = u16::try_from(surface.len()) else {
            return;
        };
        let reading_hash = text_hash(reading);
        let surface_hash = text_hash(surface);
        self.learn_key(
            false,
            left_context,
            right_context,
            reading_hash,
            reading_len,
            surface_hash,
            surface_len,
            day,
            sequence,
        );
        self.learn_key(
            true,
            0,
            0,
            reading_hash,
            reading_len,
            surface_hash,
            surface_len,
            day,
            sequence,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn learn_key(
        &mut self,
        general: bool,
        left_context: u16,
        right_context: u16,
        reading_hash: u64,
        reading_len: u16,
        surface_hash: u64,
        surface_len: u16,
        day: u32,
        sequence: u64,
    ) {
        let key = LearningKey {
            general,
            left_context,
            right_context,
            reading_hash,
            reading_len,
            surface_hash,
            surface_len,
        };
        let bucket = Self::bucket(key);
        let start = bucket * WAYS;
        let ways = &mut self.slots[start..start + WAYS];
        if let Some(slot) = ways.iter_mut().find(|slot| slot.matches(key)) {
            slot.frequency = slot.frequency.saturating_add(1);
            slot.last_seen_day = day;
            slot.sequence = sequence;
            return;
        }

        let target_index = ways
            .iter()
            .position(|slot| !slot.occupied)
            .unwrap_or_else(|| {
                ways.iter()
                    .enumerate()
                    .min_by_key(|(_, slot)| slot.sequence)
                    .map(|(index, _)| index)
                    .expect("every bucket has at least one way")
            });
        let target = &mut ways[target_index];
        if !target.occupied {
            self.len += 1;
        }
        *target = Slot {
            occupied: true,
            general,
            left_context,
            right_context,
            reading_len,
            surface_len,
            reading_hash,
            surface_hash,
            frequency: 1,
            last_seen_day: day,
            sequence,
        };
    }

    fn preference<'a>(
        &self,
        reading: &str,
        left_context: u16,
        candidates: impl IntoIterator<Item = (&'a str, u16)> + Clone,
        day: u32,
    ) -> LearningPreference {
        let Ok(reading_len) = u16::try_from(reading.len()) else {
            return LearningPreference {
                exact: None,
                general: None,
            };
        };
        let reading_hash = text_hash(reading);
        let exact = self.best_candidate(
            PreferenceQuery {
                general: false,
                left_context,
                reading_hash,
                reading_len,
                day,
                exact: true,
            },
            candidates.clone(),
        );
        let general = self.best_candidate(
            PreferenceQuery {
                general: true,
                left_context: 0,
                reading_hash,
                reading_len,
                day,
                exact: false,
            },
            candidates,
        );
        LearningPreference { exact, general }
    }

    fn best_candidate<'a>(
        &self,
        query: PreferenceQuery,
        candidates: impl IntoIterator<Item = (&'a str, u16)>,
    ) -> Option<usize> {
        let mut best = None::<(usize, u64, u64)>;
        for (candidate_index, (surface, right_context)) in candidates.into_iter().enumerate() {
            let Ok(surface_len) = u16::try_from(surface.len()) else {
                continue;
            };
            let surface_hash = text_hash(surface);
            let key = LearningKey {
                general: query.general,
                left_context: query.left_context,
                right_context: if query.general { 0 } else { right_context },
                reading_hash: query.reading_hash,
                reading_len: query.reading_len,
                surface_hash,
                surface_len,
            };
            let bucket = Self::bucket(key);
            let start = bucket * WAYS;
            let Some(slot) = self.slots[start..start + WAYS]
                .iter()
                .find(|slot| slot.matches(key))
            else {
                continue;
            };
            let effective_frequency =
                effective_learning_frequency(slot.frequency, slot.last_seen_day, query.day);
            let Some(strength) = LearningStrength::from_effective_frequency(effective_frequency)
            else {
                continue;
            };
            if candidate_index > strength.maximum_candidate_index(query.exact) {
                continue;
            }

            // Frequency (after decay) wins before recency.  The old
            // exact-context path used sequence alone, which let one unusual
            // confirmation unconditionally beat the base converter forever.
            // A repeated choice is stronger evidence; sequence still makes the
            // latest choice deterministic when evidence is tied.
            let ranked = (
                candidate_index,
                u64::from(effective_frequency),
                slot.sequence,
            );
            if best.is_none_or(|(_, best_score, best_sequence)| {
                (u64::from(effective_frequency), slot.sequence) > (best_score, best_sequence)
            }) {
                best = Some(ranked);
            }
        }
        best.map(|(index, _, _)| index)
    }
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    occupied: bool,
    reading: FixedStr<MAX_HISTORY_TEXT_BYTES>,
    surface: FixedStr<MAX_HISTORY_TEXT_BYTES>,
    right_context: u16,
    frequency: u32,
    last_seen_day: u32,
    sequence: u64,
}

impl Default for HistoryEntry {
    fn default() -> Self {
        Self {
            occupied: false,
            reading: FixedStr::new(),
            surface: FixedStr::new(),
            right_context: 0,
            frequency: 0,
            last_seen_day: 0,
            sequence: 0,
        }
    }
}

#[derive(Debug)]
struct PredictionHistory {
    entries: Box<[HistoryEntry]>,
}

impl PredictionHistory {
    fn new() -> Self {
        let mut entries = Vec::with_capacity(MAX_PREDICTION_HISTORY_ENTRIES);
        entries.resize_with(MAX_PREDICTION_HISTORY_ENTRIES, HistoryEntry::default);
        Self {
            entries: entries.into_boxed_slice(),
        }
    }

    fn learn(&mut self, reading: &str, surface: &str, right_context: u16, day: u32, sequence: u64) {
        if reading.is_empty()
            || surface.is_empty()
            || reading.len() > MAX_HISTORY_TEXT_BYTES
            || surface.len() > MAX_HISTORY_TEXT_BYTES
        {
            return;
        }
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            entry.occupied && entry.reading.as_str() == reading && entry.surface.as_str() == surface
        }) {
            entry.right_context = right_context;
            entry.frequency = entry.frequency.saturating_add(1);
            entry.last_seen_day = day;
            entry.sequence = sequence;
            return;
        }

        let target = self
            .entries
            .iter_mut()
            .min_by_key(|entry| (entry.occupied, entry.sequence))
            .expect("history prediction window is non-empty");
        target.reading.clear();
        target.surface.clear();
        let _ = target.reading.push_str(reading);
        let _ = target.surface.push_str(surface);
        target.occupied = true;
        target.right_context = right_context;
        target.frequency = 1;
        target.last_seen_day = day;
        target.sequence = sequence;
    }

    fn visit(
        &self,
        prefix: &str,
        day: u32,
        newest_sequence: u64,
        mut visit: impl FnMut(&str, &str, u16, i64) -> bool,
    ) {
        const MAX_MATCHES: usize = 9;
        let mut matches = [(i64::MAX, usize::MAX); MAX_MATCHES];
        for (index, entry) in self.entries.iter().enumerate() {
            if !entry.occupied || !entry.reading.as_str().starts_with(prefix) {
                continue;
            }
            let half_lives = day.saturating_sub(entry.last_seen_day) / 30;
            let decayed = entry.frequency.checked_shr(half_lives.min(31)).unwrap_or(0);
            let recency = newest_sequence.saturating_sub(entry.sequence).min(1_000) as i64;
            let score = recency.saturating_sub(i64::from(decayed).saturating_mul(100));
            let mut at = MAX_MATCHES - 1;
            if matches[at].0 <= score {
                continue;
            }
            matches[at] = (score, index);
            while at > 0 && matches[at] < matches[at - 1] {
                matches.swap(at, at - 1);
                at -= 1;
            }
        }
        for (score, index) in matches {
            let Some(entry) = self.entries.get(index) else {
                break;
            };
            if !visit(
                entry.reading.as_str(),
                entry.surface.as_str(),
                entry.right_context,
                score,
            ) {
                break;
            }
        }
    }
}

#[derive(Debug)]
struct Log {
    file: Option<File>,
    path: Option<PathBuf>,
    bytes: u64,
    records: u64,
    dirty_records: u64,
    /// Artifacts from an interrupted exact-prediction deletion. They are
    /// deliberately separate from the canonical log: a recovery backup is
    /// restored only when the canonical path is absent, and stale artifacts
    /// are never allowed to replace a newer canonical log.
    forget_artifacts: ForgetArtifacts,
}

/// Bounded, deterministic recovery state for an exact-prediction deletion.
///
/// `restore_backup` is authoritative only while the canonical path is absent.
/// Once a canonical log exists, every stored path is cleanup-only and is never
/// replayed over that canonical log.
#[derive(Debug, Default)]
struct ForgetArtifacts {
    restore_backup: Option<PathBuf>,
    backup_cleanup: Vec<PathBuf>,
    temporary_cleanup: Vec<PathBuf>,
}

impl ForgetArtifacts {
    fn track_backup_cleanup(&mut self, path: PathBuf) {
        if !self.backup_cleanup.iter().any(|known| known == &path) {
            self.backup_cleanup.push(path);
        }
    }

    fn track_temporary_cleanup(&mut self, path: PathBuf) {
        if !self.temporary_cleanup.iter().any(|known| known == &path) {
            self.temporary_cleanup.push(path);
        }
    }

    fn settle(&mut self, canonical: &Path) -> io::Result<()> {
        if let Some(backup) = self.restore_backup.clone() {
            restore_forget_backup(canonical, &backup)?;
            self.restore_backup = None;
        }
        settle_forget_cleanup(&mut self.backup_cleanup, ForgetArtifactKind::Backup)?;
        settle_forget_cleanup(&mut self.temporary_cleanup, ForgetArtifactKind::Temporary)
    }
}

#[derive(Debug)]
enum AppendFailure {
    AtCapacity,
    Io,
}

impl Log {
    fn memory() -> Self {
        Self {
            file: None,
            path: None,
            bytes: 0,
            records: 0,
            dirty_records: 0,
            forget_artifacts: ForgetArtifacts::default(),
        }
    }

    fn settle_forget_artifacts(&mut self) -> io::Result<()> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        self.forget_artifacts.settle(path)
    }

    fn append(
        &mut self,
        reading: &str,
        surface: &str,
        left_context: u16,
        right_context: u16,
        day: u32,
    ) -> Result<(), AppendFailure> {
        if self.path.is_none() {
            return Ok(());
        }
        let Some(file) = self.file.as_mut() else {
            return Err(AppendFailure::Io);
        };
        let payload = encode_record(reading, surface, left_context, right_context, day)
            .map_err(|_| AppendFailure::Io)?;
        let length = u32::try_from(payload.len()).map_err(|_| AppendFailure::Io)?;
        let frame_bytes = u64::try_from(RECORD_ENVELOPE_LEN + payload.len()).unwrap_or(u64::MAX);
        if self.bytes.saturating_add(frame_bytes) > MAX_LEARNING_LOG_BYTES {
            return Err(AppendFailure::AtCapacity);
        }
        let write_result = (|| -> io::Result<()> {
            file.write_all(&length.to_le_bytes())?;
            file.write_all(&crc32(&payload).to_le_bytes())?;
            file.write_all(&payload)
        })();
        if let Err(error) = write_result {
            // A partial frame may now be present. Disable this handle so no
            // later valid frame is written beyond a torn tail that replay
            // must stop at.
            self.file = None;
            let _ = error;
            return Err(AppendFailure::Io);
        }
        self.bytes = self.bytes.saturating_add(frame_bytes);
        self.records = self.records.saturating_add(1);
        self.dirty_records = self.dirty_records.saturating_add(1);
        Ok(())
    }
}

#[derive(Debug)]
struct State {
    index: Index,
    prediction_history: PredictionHistory,
    log: Log,
    sequence: u64,
}

/// Process-shared learning index and log writer.
#[derive(Debug)]
pub struct LearningService {
    state: Mutex<State>,
    /// Changes after every in-memory ranking mutation. Pipe dispatchers use
    /// this process-wide epoch to invalidate suggestions cached by a different
    /// connection without putting a shared lock on the keystroke fast path.
    generation: AtomicU64,
    skipped_writes: AtomicU64,
    recovered_tail_bytes: AtomicU64,
    maintenance_failures: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceOutcome {
    Idle,
    Busy,
    Flushed,
    Compacted,
}

/// Terminal outcome of removing one learned prediction pair.
///
/// Only [`Self::Removed`] mutates durable or in-memory state. `Unavailable`
/// is the deliberate outcome for an in-memory service, which has no durable
/// source of truth to rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForgetPredictionOutcome {
    Removed,
    NotFound,
    Unavailable,
}

/// One durable learning event exposed to the settings viewer/exporter.
///
/// Events intentionally remain an append-order history instead of an
/// aggregation: exporting and clearing must be auditable, and retaining the
/// original context ids lets diagnostics explain why an exact-context choice
/// won over a general one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearningRecord {
    pub sequence: u64,
    pub day: u32,
    pub left_context: u16,
    pub right_context: u16,
    pub reading: String,
    pub surface: String,
}

/// A verified prefix of the checksummed learning log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearningSnapshot {
    pub format_version: u16,
    pub records: Vec<LearningRecord>,
    /// Bytes after the last checksummed record. The engine repairs this tail
    /// on open; a concurrent settings read reports it instead of presenting
    /// an incomplete event as valid data.
    pub ignored_tail_bytes: u64,
}

impl LearningSnapshot {
    /// Stable, UTF-8 TSV export. Text fields use backslash escaping because a
    /// committed surface may legitimately contain a tab or line break.
    pub fn to_tsv(&self) -> String {
        let mut output = format!(
            "# sakura-learning-format: {}\nsequence\tday\tleft-context\tright-context\treading\tsurface\n",
            self.format_version
        );
        for record in &self.records {
            output.push_str(&record.sequence.to_string());
            output.push('\t');
            output.push_str(&record.day.to_string());
            output.push('\t');
            output.push_str(&record.left_context.to_string());
            output.push('\t');
            output.push_str(&record.right_context.to_string());
            output.push('\t');
            push_tsv_escaped(&mut output, &record.reading);
            output.push('\t');
            push_tsv_escaped(&mut output, &record.surface);
            output.push('\n');
        }
        output
    }
}

impl LearningService {
    pub fn memory() -> Self {
        Self {
            state: Mutex::new(State {
                index: Index::new(),
                prediction_history: PredictionHistory::new(),
                log: Log::memory(),
                sequence: 0,
            }),
            generation: AtomicU64::new(0),
            skipped_writes: AtomicU64::new(0),
            recovered_tail_bytes: AtomicU64::new(0),
            maintenance_failures: AtomicU64::new(0),
        }
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Reconcile a prior exact-prediction deletion before this function is
        // ever allowed to create an empty canonical log. In particular, a
        // fixed recovery backup wins only when the canonical path is absent.
        let forget_artifacts = recover_forget_artifacts_at_startup(path)?;
        create_if_missing(path)?;

        let mut bytes = fs::read(path)?;
        let version = read_header(&bytes)?;
        if matches!(version, FORMAT_VERSION_1 | FORMAT_VERSION_2) {
            bytes = upgrade_to_current(&bytes, version)?;
            publish_upgrade(path, &bytes, version)?;
        } else if version != LEARNING_FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported learning format version {version}"),
            ));
        }

        let mut index = Index::new();
        let mut prediction_history = PredictionHistory::new();
        let (last_good, sequence) = replay(
            &bytes,
            LEARNING_FORMAT_VERSION,
            &mut index,
            &mut prediction_history,
        )?;
        let recovered = bytes.len().saturating_sub(last_good);
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        if recovered > 0 {
            file.set_len(u64::try_from(last_good).unwrap_or(u64::MAX))?;
        }
        let file = open_append(path)?;
        let mut state = State {
            index,
            prediction_history,
            log: Log {
                file: Some(file),
                path: Some(path.to_owned()),
                bytes: u64::try_from(last_good).unwrap_or(u64::MAX),
                records: sequence,
                dirty_records: 0,
                forget_artifacts,
            },
            sequence,
        };
        if state.log.bytes > COMPACTION_TRIGGER_BYTES || state.log.records > MAX_LOG_RECORDS {
            compact_state(&mut state, COMPACTION_TARGET_BYTES, TARGET_LOG_RECORDS)?;
        }
        Ok(Self {
            state: Mutex::new(state),
            generation: AtomicU64::new(0),
            skipped_writes: AtomicU64::new(0),
            recovered_tail_bytes: AtomicU64::new(u64::try_from(recovered).unwrap_or(u64::MAX)),
            maintenance_failures: AtomicU64::new(0),
        })
    }

    pub fn learn(&self, reading: &str, surface: &str, left_context: u16, right_context: u16) {
        if reading.is_empty() || surface.is_empty() {
            return;
        }
        let day = unix_day();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.sequence = state.sequence.saturating_add(1);
        let sequence = state.sequence;
        state
            .index
            .learn(left_context, right_context, reading, surface, day, sequence);
        state
            .prediction_history
            .learn(reading, surface, right_context, day, sequence);
        if state
            .log
            .append(reading, surface, left_context, right_context, day)
            .is_err()
        {
            self.skipped_writes.fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Process-wide personalization epoch used for lock-free cache coherence.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn preference<'a>(
        &self,
        reading: &str,
        left_context: u16,
        candidates: impl IntoIterator<Item = (&'a str, u16)> + Clone,
    ) -> LearningPreference {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .index
            .preference(reading, left_context, candidates, unix_day())
    }

    pub(crate) fn visit_prediction_history(
        &self,
        prefix: &str,
        visit: impl FnMut(&str, &str, u16, i64) -> bool,
    ) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .prediction_history
            .visit(prefix, unix_day(), state.sequence, visit);
    }

    /// Durably forgets every learned event for the exact `(reading, surface)`
    /// pair. This is an explicit, infrequent command, so it may rewrite the
    /// bounded log in O(N); ordinary learning and prediction lookup remain
    /// O(1). The old log and live indexes remain authoritative until the
    /// filtered replacement has been published with its append owner ready.
    pub(crate) fn forget_prediction_exact(
        &self,
        reading: &str,
        surface: &str,
    ) -> io::Result<ForgetPredictionOutcome> {
        if reading.is_empty() || surface.is_empty() {
            return Ok(ForgetPredictionOutcome::NotFound);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(path) = state.log.path.clone() else {
            return Ok(ForgetPredictionOutcome::Unavailable);
        };
        if let Err(error) = state.log.settle_forget_artifacts() {
            self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
        let file = state.log.file.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "learning log writer unavailable")
        })?;
        file.sync_data()?;

        let source = fs::read(&path)?;
        if read_header(&source)? != LEARNING_FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "learning log version changed during prediction deletion",
            ));
        }
        let (last_good, _) = scan_records(&source, LEARNING_FORMAT_VERSION)?;
        if last_good != source.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "learning log has an unverified tail during prediction deletion",
            ));
        }

        let mut rewritten = header(LEARNING_FORMAT_VERSION).to_vec();
        let mut offset = HEADER_LEN;
        let mut removed = false;
        while let Some((next, record)) = record_at(&source, LEARNING_FORMAT_VERSION, offset) {
            if record.reading == reading && record.surface == surface {
                removed = true;
            } else {
                rewritten.extend_from_slice(&source[offset..next]);
            }
            offset = next;
        }
        if !removed {
            return Ok(ForgetPredictionOutcome::NotFound);
        }

        let mut rebuilt_index = Index::new();
        let mut rebuilt_history = PredictionHistory::new();
        let (rebuilt_good, rebuilt_sequence) = replay(
            &rewritten,
            LEARNING_FORMAT_VERSION,
            &mut rebuilt_index,
            &mut rebuilt_history,
        )?;
        if rebuilt_good != rewritten.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "filtered learning log did not replay completely",
            ));
        }

        let temporary = forget_temporary_path(&path);
        let backup = forget_recovery_path(&path);
        ensure_forget_transaction_paths_are_clear(&temporary, &backup)?;
        if let Err(error) = write_forget_temporary(&temporary, &rewritten) {
            state
                .log
                .forget_artifacts
                .track_temporary_cleanup(temporary.clone());
            return match state.log.settle_forget_artifacts() {
                Ok(()) => Err(error),
                Err(cleanup_error) => {
                    self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
                    Err(with_follow_up_error(
                        "prediction deletion temporary write failed",
                        error,
                        cleanup_error,
                    ))
                }
            };
        }

        // The replacement append owner is opened *before* publication with
        // FILE_SHARE_DELETE. The publication state machine can therefore keep
        // an append owner for both logical versions: the old handle survives
        // a failed publish, while this new handle becomes canonical on commit.
        let replacement_file = match open_forget_replacement(&temporary) {
            Ok(file) => file,
            Err(error) => {
                state
                    .log
                    .forget_artifacts
                    .track_temporary_cleanup(temporary.clone());
                return match state.log.settle_forget_artifacts() {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => {
                        self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
                        Err(with_follow_up_error(
                            "prediction deletion replacement preparation failed",
                            error,
                            cleanup_error,
                        ))
                    }
                };
            }
        };

        if let Err(publish_error) = publish_forget_replacement(&path, &temporary, &backup) {
            // Every publication error carries the last rename phase that
            // returned success. Observation may refine an uncertain platform
            // report, but a failed observation falls back to that confirmed
            // phase instead of creating an indeterminate terminal state.
            let ForgetPublishError {
                confirmed_phase,
                error,
            } = publish_error;
            let (publish_state, terminal_error, observation_succeeded) =
                match observe_forget_publish_state(&path, &temporary, &backup, &source, &rewritten)
                {
                    Ok(observed) => {
                        // Once the replacement rename returned success, no
                        // later report may regress the transaction to failure.
                        let resolved =
                            if confirmed_phase == ForgetPublishPhase::ReplacementMovedToCanonical {
                                ForgetPublishState::FilteredCanonical
                            } else {
                                observed
                            };
                        (resolved, error, true)
                    }
                    Err(observation_error) => {
                        self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
                        (
                            confirmed_phase.fallback_state(),
                            with_follow_up_error(
                                "prediction deletion publish failed while observing recovery state",
                                error,
                                observation_error,
                            ),
                            false,
                        )
                    }
                };
            match publish_state {
                ForgetPublishState::FilteredCanonical => {
                    // Keep the unexpected platform report observable while
                    // preserving the only correct logical terminal state.
                    // Observation failures were counted above.
                    if observation_succeeded {
                        self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
                    }
                }
                ForgetPublishState::OldCanonical { backup_present } => {
                    state
                        .log
                        .forget_artifacts
                        .track_temporary_cleanup(temporary);
                    if backup_present {
                        state.log.forget_artifacts.track_backup_cleanup(backup);
                    }
                    return match state.log.settle_forget_artifacts() {
                        Ok(()) => Err(terminal_error),
                        Err(recovery_error) => {
                            self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
                            Err(with_follow_up_error(
                                "prediction deletion publish failed",
                                terminal_error,
                                recovery_error,
                            ))
                        }
                    };
                }
                ForgetPublishState::RecoveryRequired => {
                    // Once the first rename has moved old bytes to the
                    // deterministic backup, a second-rename failure leaves
                    // canonical absent. Record that state before trying
                    // restoration, so a failed restore remains recoverable on
                    // restart and the still-open old append handle can
                    // continue safely in this process.
                    state
                        .log
                        .forget_artifacts
                        .track_temporary_cleanup(temporary);
                    state.log.forget_artifacts.restore_backup = Some(backup);
                    return match state.log.settle_forget_artifacts() {
                        Ok(()) => Err(terminal_error),
                        Err(recovery_error) => {
                            self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
                            Err(with_follow_up_error(
                                "prediction deletion publish failed",
                                terminal_error,
                                recovery_error,
                            ))
                        }
                    };
                }
            }
        }

        // From this point the filtered file is the durable authority. It has
        // an already-open append owner, so no later I/O result can turn this
        // committed deletion into a failure/beep outcome.
        state.index = rebuilt_index;
        state.prediction_history = rebuilt_history;
        state.sequence = rebuilt_sequence;
        state.log = Log {
            file: Some(replacement_file),
            path: Some(path),
            bytes: u64::try_from(rewritten.len()).unwrap_or(u64::MAX),
            records: rebuilt_sequence,
            dirty_records: 0,
            forget_artifacts: ForgetArtifacts {
                restore_backup: None,
                backup_cleanup: vec![backup],
                temporary_cleanup: Vec::new(),
            },
        };
        if state.log.settle_forget_artifacts().is_err() {
            // The old bytes are now only a cleanup artifact. Keep its path in
            // state for retry, count the observable failure, but preserve the
            // committed outcome because canonical and in-memory authority are
            // already the filtered log.
            self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        self.generation.fetch_add(1, Ordering::Release);
        Ok(ForgetPredictionOutcome::Removed)
    }

    pub fn skipped_writes(&self) -> u64 {
        self.skipped_writes.load(Ordering::Relaxed)
    }

    pub fn recovered_tail_bytes(&self) -> u64 {
        self.recovered_tail_bytes.load(Ordering::Relaxed)
    }

    pub fn maintenance_failures(&self) -> u64 {
        self.maintenance_failures.load(Ordering::Relaxed)
    }

    /// Flushes pending records and compacts a large log. The non-blocking
    /// lock makes a busy commit/conversion the owner of the moment; the
    /// maintenance thread simply tries again on its next bounded interval.
    pub fn maintain(&self) -> io::Result<MaintenanceOutcome> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => return Ok(MaintenanceOutcome::Busy),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        if state.log.path.is_none() {
            return Ok(MaintenanceOutcome::Idle);
        }
        if let Err(error) = state.log.settle_forget_artifacts() {
            self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
        if state.log.file.is_none()
            || state.log.bytes > COMPACTION_TRIGGER_BYTES
            || state.log.records > MAX_LOG_RECORDS
        {
            let result = compact_state(&mut state, COMPACTION_TARGET_BYTES, TARGET_LOG_RECORDS);
            if result.is_err() {
                self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
            }
            result.map(|()| MaintenanceOutcome::Compacted)
        } else if state.log.dirty_records > 0 {
            let result = state
                .log
                .file
                .as_ref()
                .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "log writer unavailable"))
                .and_then(File::sync_data);
            if result.is_err() {
                state.log.file = None;
                self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
                return result.map(|()| MaintenanceOutcome::Flushed);
            }
            state.log.dirty_records = 0;
            Ok(MaintenanceOutcome::Flushed)
        } else {
            Ok(MaintenanceOutcome::Idle)
        }
    }

    /// Clears both the live personalization indexes and their durable log
    /// under the same mutex used by commits and maintenance. Every failure
    /// path either restores the old writer or leaves it explicitly disabled;
    /// no caller can observe an empty in-memory index backed by the old log.
    pub fn clear(&self) -> io::Result<u64> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cleared_records = state.log.records;
        let Some(path) = state.log.path.clone() else {
            state.index = Index::new();
            state.prediction_history = PredictionHistory::new();
            state.sequence = 0;
            state.log = Log::memory();
            self.generation.fetch_add(1, Ordering::Release);
            return Ok(cleared_records);
        };

        if let Err(error) = state.log.settle_forget_artifacts() {
            self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }

        if let Some(file) = state.log.file.as_ref() {
            file.sync_data()?;
        }
        let temporary = unique_sibling(&path, "clear.tmp");
        let backup = unique_sibling(&path, "clear.bak");
        write_new_file(&temporary, &header(LEARNING_FORMAT_VERSION))?;

        state.log.file = None;
        if let Err(error) = fs::rename(&path, &backup) {
            state.log.file = open_append(&path).ok();
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            let rollback = fs::rename(&backup, &path);
            state.log.file = open_append(&path).ok();
            let _ = fs::remove_file(&temporary);
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(io::Error::new(
                    rollback_error.kind(),
                    format!("clear publish failed ({error}); rollback failed ({rollback_error})"),
                )),
            };
        }
        let file = match open_append(&path) {
            Ok(file) => file,
            Err(error) => {
                let _ = fs::remove_file(&path);
                let rollback = fs::rename(&backup, &path);
                state.log.file = open_append(&path).ok();
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(io::Error::new(
                        rollback_error.kind(),
                        format!(
                            "cleared log could not be opened ({error}); rollback failed ({rollback_error})"
                        ),
                    )),
                };
            }
        };
        let _ = fs::remove_file(&backup);
        state.index = Index::new();
        state.prediction_history = PredictionHistory::new();
        state.sequence = 0;
        state.log = Log {
            file: Some(file),
            path: Some(path),
            bytes: HEADER_LEN as u64,
            records: 0,
            dirty_records: 0,
            forget_artifacts: ForgetArtifacts::default(),
        };
        self.generation.fetch_add(1, Ordering::Release);
        Ok(cleared_records)
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .log
            .path
            .clone()
    }
}

/// Owns the one periodic flush/compaction thread for a learning service.
#[derive(Debug)]
pub struct LearningMaintenance {
    stop: Option<SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl LearningMaintenance {
    pub fn start(service: Arc<LearningService>) -> io::Result<Self> {
        Self::start_with_interval(service, MAINTENANCE_INTERVAL)
    }

    fn start_with_interval(service: Arc<LearningService>, interval: Duration) -> io::Result<Self> {
        let (stop, stopped) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("sakura-learning-maintenance".to_owned())
            .spawn(move || maintenance_loop(&service, stopped, interval))?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) -> thread::Result<()> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> thread::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.try_send(());
        }
        match self.thread.take() {
            Some(thread) => thread.join(),
            None => Ok(()),
        }
    }
}

impl Drop for LearningMaintenance {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn maintenance_loop(service: &LearningService, stop: Receiver<()>, interval: Duration) {
    loop {
        match stop.recv_timeout(interval) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = service.maintain();
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = service.maintain();
            }
        }
    }
}

pub fn default_path() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for the per-user learning store",
        )
    })?;
    Ok(PathBuf::from(local)
        .join("SakuraInput")
        .join("learning")
        .join("log.bin"))
}

/// Reads the verified portion of a learning log without mutating it.
/// Previous supported formats remain viewable even before the engine has had
/// an opportunity to upgrade the file.
pub fn read_snapshot(path: &Path) -> io::Result<LearningSnapshot> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_LEARNING_LOG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "learning log exceeds its hard size bound",
        ));
    }
    let bytes = fs::read(path)?;
    let version = read_header(&bytes)?;
    if !matches!(
        version,
        FORMAT_VERSION_1 | FORMAT_VERSION_2 | LEARNING_FORMAT_VERSION
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported learning format version {version}"),
        ));
    }

    let mut offset = HEADER_LEN;
    let mut sequence = 0u64;
    let mut records = Vec::new();
    while let Some((next, record)) = record_at(&bytes, version, offset) {
        sequence = sequence.saturating_add(1);
        records.push(LearningRecord {
            sequence,
            day: record.day,
            left_context: record.left_context,
            right_context: record.right_context,
            reading: record.reading.to_owned(),
            surface: record.surface.to_owned(),
        });
        offset = next;
    }
    Ok(LearningSnapshot {
        format_version: version,
        records,
        ignored_tail_bytes: u64::try_from(bytes.len().saturating_sub(offset)).unwrap_or(u64::MAX),
    })
}

fn push_tsv_escaped(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '\t' => output.push_str("\\t"),
            '\r' => output.push_str("\\r"),
            '\n' => output.push_str("\\n"),
            other => output.push(other),
        }
    }
}

static NEXT_ARTIFACT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForgetArtifactKind {
    Backup,
    Temporary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForgetPublishState {
    FilteredCanonical,
    OldCanonical { backup_present: bool },
    RecoveryRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForgetPublishPhase {
    BeforeFirstRename,
    OldMovedToRecovery,
    ReplacementMovedToCanonical,
}

impl ForgetPublishPhase {
    fn fallback_state(self) -> ForgetPublishState {
        match self {
            Self::BeforeFirstRename => ForgetPublishState::OldCanonical {
                backup_present: false,
            },
            Self::OldMovedToRecovery => ForgetPublishState::RecoveryRequired,
            Self::ReplacementMovedToCanonical => ForgetPublishState::FilteredCanonical,
        }
    }
}

#[derive(Debug)]
struct ForgetPublishError {
    confirmed_phase: ForgetPublishPhase,
    error: io::Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForgetFaultPoint {
    ReplacementOwnerOpen,
    Publish,
    PublishMovesOldToRecovery,
    PublishCommitsThenErrors,
    PublishObservation,
    RecoveryRestore,
    BackupCleanup,
    TemporaryCleanup,
}

#[cfg(test)]
thread_local! {
    static FORGET_FAULTS: RefCell<Vec<ForgetFaultPoint>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
struct ForgetFaultScope;

#[cfg(test)]
impl ForgetFaultScope {
    fn new(points: &[ForgetFaultPoint]) -> Self {
        FORGET_FAULTS.with(|faults| {
            let mut faults = faults.borrow_mut();
            assert!(
                faults.is_empty(),
                "forget fault queue leaked from another test phase"
            );
            faults.extend_from_slice(points);
        });
        Self
    }
}

#[cfg(test)]
impl Drop for ForgetFaultScope {
    fn drop(&mut self) {
        FORGET_FAULTS.with(|faults| faults.borrow_mut().clear());
    }
}

/// Test-only cross-layer fault for the state where the old canonical log has
/// moved to recovery, publication observation fails, and immediate restoration
/// also fails. Keeping the precise sequence here prevents production callers
/// and sibling tests from depending on the internal publication fault queue.
#[cfg(test)]
pub(crate) struct ForgetPredictionDeepRecoveryFault {
    _scope: ForgetFaultScope,
}

#[cfg(test)]
impl ForgetPredictionDeepRecoveryFault {
    pub(crate) fn install() -> Self {
        Self {
            _scope: ForgetFaultScope::new(&[
                ForgetFaultPoint::PublishMovesOldToRecovery,
                ForgetFaultPoint::PublishObservation,
                ForgetFaultPoint::RecoveryRestore,
            ]),
        }
    }
}

/// Test-only cross-layer fault after the replacement rename is confirmed but
/// both publication and subsequent observation report errors.
#[cfg(test)]
pub(crate) struct ForgetPredictionCommittedObservationFault {
    _scope: ForgetFaultScope,
}

#[cfg(test)]
impl ForgetPredictionCommittedObservationFault {
    pub(crate) fn install() -> Self {
        Self {
            _scope: ForgetFaultScope::new(&[
                ForgetFaultPoint::PublishCommitsThenErrors,
                ForgetFaultPoint::PublishObservation,
            ]),
        }
    }
}

#[cfg(test)]
fn take_forget_fault(point: ForgetFaultPoint) -> bool {
    FORGET_FAULTS.with(|faults| {
        let mut faults = faults.borrow_mut();
        if faults.first().copied() == Some(point) {
            faults.remove(0);
            true
        } else {
            false
        }
    })
}

#[cfg(not(test))]
fn take_forget_fault(_point: ForgetFaultPoint) -> bool {
    false
}

fn injected_forget_error(point: ForgetFaultPoint) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("injected exact-prediction deletion fault at {point:?}"),
    )
}

fn with_follow_up_error(operation: &str, primary: io::Error, follow_up: io::Error) -> io::Error {
    io::Error::new(
        primary.kind(),
        format!("{operation} ({primary}); follow-up failed ({follow_up})"),
    )
}

fn forget_temporary_path(path: &Path) -> PathBuf {
    path.with_extension("forget.tmp")
}

fn forget_recovery_path(path: &Path) -> PathBuf {
    path.with_extension("forget.recovery")
}

/// This was the fixed backup name used by the pre-P1 two-rename flow. Honor
/// it on startup so an interrupted older build is not replaced by a newly
/// created empty canonical log.
fn legacy_forget_backup_path(path: &Path) -> PathBuf {
    path.with_extension("forget.bak")
}

fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Returns the verified prefix of a current-format recovery log. A final
/// incomplete envelope or payload is a torn append and may be discarded, but
/// every complete frame must satisfy the same length, checksum, and payload
/// invariants as a canonical log.
fn scan_repairable_current_recovery_log(bytes: &[u8]) -> io::Result<usize> {
    if read_header(bytes)? != LEARNING_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exact-prediction recovery log has an unsupported format",
        ));
    }

    let mut offset = HEADER_LEN;
    while offset < bytes.len() {
        if bytes.len() - offset < RECORD_ENVELOPE_LEN {
            return Ok(offset);
        }
        let length = usize::try_from(u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("checked complete record envelope"),
        ))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record length overflow"))?;
        if length > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exact-prediction recovery log has an invalid complete record length",
            ));
        }
        let expected_crc = u32::from_le_bytes(
            bytes[offset + 4..offset + RECORD_ENVELOPE_LEN]
                .try_into()
                .expect("checked complete record envelope"),
        );
        let payload_start = offset + RECORD_ENVELOPE_LEN;
        let payload_end = payload_start
            .checked_add(length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "record length overflow"))?;
        if payload_end > bytes.len() {
            return Ok(offset);
        }
        let payload = &bytes[payload_start..payload_end];
        if crc32(payload) != expected_crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exact-prediction recovery log has an invalid complete record checksum",
            ));
        }
        decode_record(payload, LEARNING_FORMAT_VERSION).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("exact-prediction recovery log has an invalid complete record ({error})"),
            )
        })?;
        offset = payload_end;
    }
    Ok(offset)
}

/// Repairs only a torn final append before moving the recovery inode back to
/// its canonical path. Until `sync_all` succeeds, the recovery path remains
/// the sole authority and no canonical file is published.
fn repair_forget_recovery_log(path: &Path) -> io::Result<()> {
    let bytes = fs::read(path)?;
    let last_good = scan_repairable_current_recovery_log(&bytes)?;
    if last_good == bytes.len() {
        return Ok(());
    }

    let file = OpenOptions::new().read(true).write(true).open(path)?;
    file.set_len(u64::try_from(last_good).unwrap_or(u64::MAX))?;
    file.sync_all()
}

fn restore_forget_backup(canonical: &Path, backup: &Path) -> io::Result<()> {
    if path_exists(canonical)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot restore exact-prediction recovery over an existing canonical log",
        ));
    }
    repair_forget_recovery_log(backup)?;
    if take_forget_fault(ForgetFaultPoint::RecoveryRestore) {
        return Err(injected_forget_error(ForgetFaultPoint::RecoveryRestore));
    }
    fs::rename(backup, canonical)
}

fn recover_forget_artifacts_at_startup(path: &Path) -> io::Result<ForgetArtifacts> {
    let mut artifacts = ForgetArtifacts::default();
    let temporary = forget_temporary_path(path);
    if path_exists(&temporary)? {
        artifacts.track_temporary_cleanup(temporary);
    }

    let recovery = forget_recovery_path(path);
    let legacy = legacy_forget_backup_path(path);
    let mut backups = Vec::with_capacity(2);
    for backup in [recovery, legacy] {
        if path_exists(&backup)? {
            backups.push(backup);
        }
    }

    if path_exists(path)? {
        for backup in backups {
            artifacts.track_backup_cleanup(backup);
        }
        return Ok(artifacts);
    }

    match backups.as_slice() {
        [] => Ok(artifacts),
        [backup] => {
            restore_forget_backup(path, backup)?;
            Ok(artifacts)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "multiple exact-prediction recovery logs exist while canonical is absent",
        )),
    }
}

fn settle_forget_cleanup(paths: &mut Vec<PathBuf>, kind: ForgetArtifactKind) -> io::Result<()> {
    let index = 0;
    while index < paths.len() {
        match remove_forget_artifact(&paths[index], kind) {
            Ok(()) => {
                paths.swap_remove(index);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn remove_forget_artifact(path: &Path, kind: ForgetArtifactKind) -> io::Result<()> {
    let fault = match kind {
        ForgetArtifactKind::Backup => ForgetFaultPoint::BackupCleanup,
        ForgetArtifactKind::Temporary => ForgetFaultPoint::TemporaryCleanup,
    };
    if take_forget_fault(fault) {
        return Err(injected_forget_error(fault));
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_forget_transaction_paths_are_clear(temporary: &Path, backup: &Path) -> io::Result<()> {
    for (kind, path) in [("temporary", temporary), ("recovery", backup)] {
        if path_exists(path)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("exact-prediction deletion {kind} artifact still requires settlement"),
            ));
        }
    }
    Ok(())
}

fn observe_forget_publish_state(
    canonical: &Path,
    temporary: &Path,
    backup: &Path,
    source: &[u8],
    filtered: &[u8],
) -> io::Result<ForgetPublishState> {
    if take_forget_fault(ForgetFaultPoint::PublishObservation) {
        return Err(injected_forget_error(ForgetFaultPoint::PublishObservation));
    }
    let backup_present = path_exists(backup)?;
    let temporary_present = path_exists(temporary)?;
    let canonical = match fs::read(canonical) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    match canonical.as_deref() {
        Some(bytes) if bytes == filtered && !temporary_present => {
            Ok(ForgetPublishState::FilteredCanonical)
        }
        Some(bytes) if bytes == source => Ok(ForgetPublishState::OldCanonical { backup_present }),
        None if backup_present => Ok(ForgetPublishState::RecoveryRequired),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exact-prediction deletion publish ended in an unrecognised filesystem state",
        )),
    }
}

fn write_forget_temporary(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn open_forget_replacement(path: &Path) -> io::Result<File> {
    if take_forget_fault(ForgetFaultPoint::ReplacementOwnerOpen) {
        return Err(injected_forget_error(
            ForgetFaultPoint::ReplacementOwnerOpen,
        ));
    }
    open_append(path)
}

fn publish_forget_replacement(
    canonical: &Path,
    replacement: &Path,
    backup: &Path,
) -> Result<(), ForgetPublishError> {
    let mut confirmed_phase = ForgetPublishPhase::BeforeFirstRename;
    if take_forget_fault(ForgetFaultPoint::Publish) {
        return Err(ForgetPublishError {
            confirmed_phase,
            error: injected_forget_error(ForgetFaultPoint::Publish),
        });
    }
    fs::rename(canonical, backup).map_err(|error| ForgetPublishError {
        confirmed_phase,
        error,
    })?;
    confirmed_phase = ForgetPublishPhase::OldMovedToRecovery;
    if take_forget_fault(ForgetFaultPoint::PublishMovesOldToRecovery) {
        return Err(ForgetPublishError {
            confirmed_phase,
            error: injected_forget_error(ForgetFaultPoint::PublishMovesOldToRecovery),
        });
    }
    fs::rename(replacement, canonical).map_err(|error| ForgetPublishError {
        confirmed_phase,
        error,
    })?;
    confirmed_phase = ForgetPublishPhase::ReplacementMovedToCanonical;
    if take_forget_fault(ForgetFaultPoint::PublishCommitsThenErrors) {
        return Err(ForgetPublishError {
            confirmed_phase,
            error: injected_forget_error(ForgetFaultPoint::PublishCommitsThenErrors),
        });
    }
    Ok(())
}

fn create_if_missing(path: &Path) -> io::Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(&header(LEARNING_FORMAT_VERSION))?;
            file.sync_all()
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        OpenOptions::new()
            .append(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        OpenOptions::new().append(true).open(path)
    }
}

fn unique_sibling(path: &Path, suffix: &str) -> PathBuf {
    let preferred = path.with_extension(suffix);
    if !preferred.exists() {
        return preferred;
    }
    let id = NEXT_ARTIFACT.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("{suffix}.{}.{id}", std::process::id()))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn publish_upgrade(path: &Path, bytes: &[u8], source_version: u16) -> io::Result<()> {
    let temporary = unique_sibling(path, "upgrade.tmp");
    let backup = unique_sibling(path, &format!("v{source_version}.bak"));
    write_new_file(&temporary, bytes)?;
    if let Err(error) = fs::rename(path, &backup) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let rollback = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(io::Error::new(
                rollback_error.kind(),
                format!("upgrade publish failed ({error}); rollback failed ({rollback_error})"),
            )),
        };
    }
    Ok(())
}

fn compact_state(state: &mut State, target_bytes: usize, target_records: u64) -> io::Result<()> {
    state.log.settle_forget_artifacts()?;
    let path = state
        .log
        .path
        .clone()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "learning log has no path"))?;
    if let Some(file) = state.log.file.as_ref() {
        file.sync_data()?;
    }
    let source = fs::read(&path)?;
    if read_header(&source)? != LEARNING_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "learning log version changed during compaction",
        ));
    }
    let (last_good, total_records) = scan_records(&source, LEARNING_FORMAT_VERSION)?;
    let mut first = HEADER_LEN;
    let mut retained_records = total_records;
    while retained_records > 1
        && (retained_records > target_records || last_good.saturating_sub(first) > target_bytes)
    {
        let Some((next, _)) = record_at(&source, LEARNING_FORMAT_VERSION, first) else {
            break;
        };
        first = next;
        retained_records -= 1;
    }

    let retained_len = last_good.saturating_sub(first);
    let mut compacted = Vec::with_capacity(HEADER_LEN + retained_len);
    compacted.extend_from_slice(&header(LEARNING_FORMAT_VERSION));
    compacted.extend_from_slice(&source[first..last_good]);
    let mut rebuilt = Index::new();
    let mut rebuilt_history = PredictionHistory::new();
    let (compacted_good, sequence) = replay(
        &compacted,
        LEARNING_FORMAT_VERSION,
        &mut rebuilt,
        &mut rebuilt_history,
    )?;
    if compacted_good != compacted.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "internally compacted learning log did not replay completely",
        ));
    }

    let temporary = unique_sibling(&path, "compact.tmp");
    let backup = unique_sibling(&path, "compact.bak");
    write_new_file(&temporary, &compacted)?;
    state.log.file = None;
    if let Err(error) = fs::rename(&path, &backup) {
        state.log.file = open_append(&path).ok();
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        let rollback = fs::rename(&backup, &path);
        state.log.file = open_append(&path).ok();
        let _ = fs::remove_file(&temporary);
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(io::Error::new(
                rollback_error.kind(),
                format!("compaction publish failed ({error}); rollback failed ({rollback_error})"),
            )),
        };
    }
    let file = match open_append(&path) {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&path);
            let _ = fs::rename(&backup, &path);
            state.log.file = open_append(&path).ok();
            return Err(error);
        }
    };
    let _ = fs::remove_file(&backup);
    state.index = rebuilt;
    state.prediction_history = rebuilt_history;
    state.sequence = sequence;
    state.log = Log {
        file: Some(file),
        path: Some(path),
        bytes: u64::try_from(compacted.len()).unwrap_or(u64::MAX),
        records: sequence,
        dirty_records: 0,
        forget_artifacts: ForgetArtifacts::default(),
    };
    Ok(())
}

fn unix_day() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u32::try_from(elapsed.as_secs() / 86_400).unwrap_or(u32::MAX)
        })
}

fn header(version: u16) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(MAGIC);
    header[4..6].copy_from_slice(&version.to_le_bytes());
    header
}

fn read_header(bytes: &[u8]) -> io::Result<u16> {
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid learning log header",
        ));
    }
    Ok(u16::from_le_bytes([bytes[4], bytes[5]]))
}

fn encode_record(
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
struct DecodedRecord<'a> {
    day: u32,
    left_context: u16,
    right_context: u16,
    reading: &'a str,
    surface: &'a str,
}

fn decode_record(payload: &[u8], version: u16) -> io::Result<DecodedRecord<'_>> {
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

fn replay(
    bytes: &[u8],
    version: u16,
    index: &mut Index,
    prediction_history: &mut PredictionHistory,
) -> io::Result<(usize, u64)> {
    if read_header(bytes)? != version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "learning log header version mismatch",
        ));
    }
    let mut offset = HEADER_LEN;
    let mut sequence = 0u64;
    while offset < bytes.len() {
        let Some((payload_end, record)) = record_at(bytes, version, offset) else {
            break;
        };
        sequence = sequence.saturating_add(1);
        index.learn(
            record.left_context,
            record.right_context,
            record.reading,
            record.surface,
            record.day,
            sequence,
        );
        prediction_history.learn(
            record.reading,
            record.surface,
            record.right_context,
            record.day,
            sequence,
        );
        offset = payload_end;
    }
    Ok((offset, sequence))
}

fn record_at(bytes: &[u8], version: u16, offset: usize) -> Option<(usize, DecodedRecord<'_>)> {
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

fn scan_records(bytes: &[u8], version: u16) -> io::Result<(usize, u64)> {
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

fn upgrade_to_current(bytes: &[u8], source_version: u16) -> io::Result<Vec<u8>> {
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

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    fn temporary_log(name: &str) -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("tmp")
            .join(format!(
                "sakura-learning-{}-{name}-{id}",
                std::process::id()
            ));
        fs::create_dir_all(&directory).expect("temporary directory");
        directory.join("log.bin")
    }

    fn history_contains(service: &LearningService, reading: &str, surface: &str) -> bool {
        let mut found = false;
        service.visit_prediction_history(reading, |candidate_reading, candidate_surface, _, _| {
            found = candidate_reading == reading && candidate_surface == surface;
            !found
        });
        found
    }

    fn snapshot_contains(path: &Path, reading: &str, surface: &str) -> bool {
        read_snapshot(path)
            .expect("verified snapshot")
            .records
            .iter()
            .any(|record| record.reading == reading && record.surface == surface)
    }

    fn append_test_frame(path: &Path, payload: &[u8], checksum: u32, payload_bytes: usize) {
        assert!(payload_bytes <= payload.len(), "test frame payload prefix");
        let mut file = open_append(path).expect("open test frame writer");
        file.write_all(
            &u32::try_from(payload.len())
                .expect("bounded test frame")
                .to_le_bytes(),
        )
        .expect("write test frame length");
        file.write_all(&checksum.to_le_bytes())
            .expect("write test frame checksum");
        file.write_all(&payload[..payload_bytes])
            .expect("write test frame payload");
        file.sync_data().expect("sync test frame");
    }

    #[test]
    fn exact_context_wins_and_general_frequency_decays() {
        let mut index = Index::new();
        index.learn(7, 11, "かえる", "帰る", 100, 1);
        index.learn(8, 12, "かえる", "変える", 100, 2);
        index.learn(8, 12, "かえる", "変える", 100, 3);
        let surfaces = [("蛙", 10), ("帰る", 11), ("変える", 12)];

        let context_seven = index.preference("かえる", 7, surfaces, 100);
        assert_eq!(context_seven.exact, Some(1));
        assert_eq!(context_seven.general, Some(2));

        let context_eight = index.preference("かえる", 8, surfaces, 100);
        assert_eq!(context_eight.exact, Some(2));

        let wrong_right_context =
            index.preference("かえる", 7, [("蛙", 10), ("帰る", 12), ("変える", 12)], 100);
        assert_eq!(wrong_right_context.exact, None);
        assert_eq!(wrong_right_context.general, Some(2));

        index.learn(u16::MAX, u16::MAX, "しるし", "印", 100, 4);
        assert_eq!(
            index
                .preference("しるし", u16::MAX, [("標", 0), ("印", u16::MAX)], 100)
                .exact,
            Some(1),
            "the maximum class id must not collide with general learning"
        );
    }

    #[test]
    fn learning_strength_rejects_one_off_far_choices_and_decays_stale_context() {
        let mut index = Index::new();
        let candidates = [
            ("top", 9),
            ("near", 9),
            ("third", 9),
            ("fourth", 9),
            ("fifth", 9),
            ("sixth", 9),
            ("far", 9),
        ];

        // A single far-down selection remains recorded, but weak evidence must
        // not displace the converter's base ranking in the next conversion.
        index.learn(7, 9, "reading", "far", 100, 1);
        let one_confirmation = index.preference("reading", 7, candidates, 100);
        assert_eq!(one_confirmation.exact, None);
        assert_eq!(one_confirmation.general, None);

        // Two confirmations are medium evidence, still bounded to the first
        // four exact-context candidates.  Three recent confirmations are a
        // deliberate user preference and may select the exact-context choice.
        index.learn(7, 9, "reading", "far", 100, 2);
        assert_eq!(index.preference("reading", 7, candidates, 100).exact, None);
        index.learn(7, 9, "reading", "far", 100, 3);
        assert_eq!(
            index.preference("reading", 7, candidates, 100).exact,
            Some(6)
        );

        // Context-free history is never allowed to carry this far-down choice
        // into a different grammatical context, even after repetition.
        let different_context = index.preference("reading", 8, candidates, 100);
        assert_eq!(different_context.exact, None);
        assert_eq!(different_context.general, None);

        // Three 30-day half-lives reduce three confirmations to zero evidence.
        assert_eq!(index.preference("reading", 7, candidates, 190).exact, None);
    }

    #[test]
    fn packed_index_and_public_entry_cap_stay_inside_the_memory_budget() {
        assert_eq!(MAX_LEARNING_ENTRIES, 98_304);
        assert!(
            core::mem::size_of::<Slot>() * SLOT_COUNT <= 8 * 1024 * 1024,
            "packed learning index exceeds its 8 MiB budget"
        );
    }

    #[test]
    fn prediction_history_is_frequency_ranked_and_replayed_after_restart() {
        let path = temporary_log("prediction-history");
        {
            let service = LearningService::open(&path).expect("open");
            service.learn("かながわ", "神奈川", 1, 11);
            service.learn("かなざわ", "金沢", 2, 12);
            service.learn("かなざわ", "金沢", 2, 13);
        }

        let reopened = LearningService::open(&path).expect("reopen");
        let mut matches = Vec::new();
        reopened.visit_prediction_history("かな", |reading, surface, right_context, score| {
            matches.push((reading.to_owned(), surface.to_owned(), right_context, score));
            true
        });

        assert_eq!(matches.len(), 2);
        assert_eq!(&matches[0].0, "かなざわ");
        assert_eq!(&matches[0].1, "金沢");
        assert_eq!(matches[0].2, 13);
        assert_eq!(&matches[1].0, "かながわ");
        assert_eq!(&matches[1].1, "神奈川");
        assert_eq!(matches[1].2, 11);
        assert!(matches[0].3 < matches[1].3);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn prediction_history_retains_128_entries_and_evicts_the_oldest() {
        let mut history = PredictionHistory::new();
        for sequence in 1..=MAX_PREDICTION_HISTORY_ENTRIES as u64 {
            history.learn(
                &format!("reading-{sequence:03}"),
                &format!("surface-{sequence:03}"),
                0,
                0,
                sequence,
            );
        }

        assert_eq!(history.entries.len(), MAX_PREDICTION_HISTORY_ENTRIES);
        assert!(
            history.entries.iter().all(|entry| entry.occupied),
            "the fixed window is full"
        );

        history.learn(
            "reading-overflow",
            "surface-overflow",
            0,
            0,
            MAX_PREDICTION_HISTORY_ENTRIES as u64 + 1,
        );

        assert_eq!(
            history.entries.len(),
            MAX_PREDICTION_HISTORY_ENTRIES,
            "retention remains bounded independently of prediction display limits"
        );
        assert!(!history.entries.iter().any(|entry| {
            entry.occupied
                && entry.reading.as_str() == "reading-001"
                && entry.surface.as_str() == "surface-001"
        }));
        assert!(history.entries.iter().any(|entry| {
            entry.occupied
                && entry.reading.as_str() == "reading-overflow"
                && entry.surface.as_str() == "surface-overflow"
        }));
    }

    #[test]
    fn torn_tail_is_truncated_and_verified_records_survive_restart() {
        let path = temporary_log("torn");
        {
            let service = LearningService::open(&path).expect("open");
            service.learn("かな", "加奈", 3, 4);
        }
        let good_len = fs::metadata(&path).expect("metadata").len();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append")
            .write_all(&[9, 0, 0])
            .expect("torn tail");

        let recovered = LearningService::open(&path).expect("recover");
        assert_eq!(recovered.recovered_tail_bytes(), 3);
        assert_eq!(fs::metadata(&path).expect("metadata").len(), good_len);
        let preference = recovered.preference("かな", 3, [("仮名", 4), ("加奈", 4)]);
        assert_eq!(preference.exact, Some(1));
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn previous_format_upgrades_with_unknown_context_defaulted() {
        let path = temporary_log("upgrade");
        let reading = "かな";
        let surface = "加奈";
        let mut payload = Vec::new();
        payload.extend_from_slice(&unix_day().to_le_bytes());
        payload.extend_from_slice(&(reading.len() as u16).to_le_bytes());
        payload.extend_from_slice(&(surface.len() as u16).to_le_bytes());
        payload.extend_from_slice(reading.as_bytes());
        payload.extend_from_slice(surface.as_bytes());
        let mut old = header(FORMAT_VERSION_1).to_vec();
        old.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        old.extend_from_slice(&crc32(&payload).to_le_bytes());
        old.extend_from_slice(&payload);
        fs::write(&path, &old).expect("old log");

        let upgraded = LearningService::open(&path).expect("upgrade");
        assert_eq!(
            read_header(&fs::read(&path).expect("read")).unwrap(),
            LEARNING_FORMAT_VERSION
        );
        assert_eq!(
            upgraded
                .preference(reading, 0, [("仮名", 0), (surface, 0)])
                .exact,
            Some(1)
        );
        assert!(path.with_extension("v1.bak").exists());
        assert_eq!(
            fs::read(path.with_extension("v1.bak")).expect("backup"),
            old
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn second_previous_format_preserves_left_context_and_existing_backup() {
        let path = temporary_log("upgrade-v2");
        let reading = "いった";
        let surface = "行った";
        let mut payload = Vec::new();
        payload.push(RECORD_COMMIT);
        payload.extend_from_slice(&unix_day().to_le_bytes());
        payload.extend_from_slice(&7u16.to_le_bytes());
        payload.extend_from_slice(&(reading.len() as u16).to_le_bytes());
        payload.extend_from_slice(&(surface.len() as u16).to_le_bytes());
        payload.extend_from_slice(reading.as_bytes());
        payload.extend_from_slice(surface.as_bytes());
        let mut old = header(FORMAT_VERSION_2).to_vec();
        old.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        old.extend_from_slice(&crc32(&payload).to_le_bytes());
        old.extend_from_slice(&payload);
        fs::write(&path, &old).expect("old log");
        fs::write(path.with_extension("v2.bak"), b"retain me").expect("prior backup");

        let upgraded = LearningService::open(&path).expect("upgrade");

        assert_eq!(
            upgraded
                .preference(reading, 7, [("言った", 0), (surface, 0)])
                .exact,
            Some(1)
        );
        assert_eq!(
            fs::read(path.with_extension("v2.bak")).expect("prior backup"),
            b"retain me"
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn compaction_keeps_a_bounded_recent_source_of_truth_across_restart() {
        let path = temporary_log("compact");
        let service = LearningService::open(&path).expect("open");
        for sequence in 0..100 {
            let surface = if sequence == 99 { "加奈" } else { "仮名" };
            service.learn("かな", surface, 3, 4);
        }
        {
            let mut state = service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            compact_state(&mut state, 1_024, 10).expect("compaction");
            assert!(state.log.records <= 10);
            assert!(state.log.bytes <= 1_024 + HEADER_LEN as u64);
        }
        drop(service);

        let reopened = LearningService::open(&path).expect("restart");
        // Compaction retains the bounded recent records. The nine retained
        // normal choices are stronger evidence than the one last anomalous
        // choice, so the frequency-aware preference is the base candidate.
        assert_eq!(
            reopened
                .preference("かな", 3, [("仮名", 4), ("加奈", 4)])
                .exact,
            Some(0)
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn hard_log_ceiling_skips_a_write_without_extending_the_file() {
        let path = temporary_log("ceiling");
        let service = LearningService::open(&path).expect("open");
        let before = fs::metadata(&path).expect("metadata").len();
        {
            let mut state = service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.log.bytes = MAX_LEARNING_LOG_BYTES;
        }

        service.learn("かな", "加奈", 3, 4);

        assert_eq!(service.skipped_writes(), 1);
        assert_eq!(fs::metadata(&path).expect("metadata").len(), before);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn maintenance_thread_flushes_and_reaches_an_explicit_join() {
        let path = temporary_log("maintenance");
        let service = Arc::new(LearningService::open(&path).expect("open"));
        service.learn("かな", "加奈", 3, 4);
        let maintenance = LearningMaintenance::start_with_interval(
            Arc::clone(&service),
            Duration::from_millis(10),
        )
        .expect("maintenance");
        for _ in 0..200 {
            let dirty = service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .log
                .dirty_records;
            if dirty == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .log
                .dirty_records,
            0
        );
        maintenance.stop().expect("maintenance thread joined");
        assert_eq!(service.maintenance_failures(), 0);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn settings_snapshot_exports_verified_records_and_reports_a_torn_tail() {
        let path = temporary_log("settings-snapshot");
        let service = LearningService::open(&path).expect("open");
        service.learn("さくら", "Sakura\tInput", 7, 9);
        service.maintain().expect("flush");
        drop(service);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append tail")
            .write_all(b"torn")
            .expect("torn tail");

        let snapshot = read_snapshot(&path).expect("snapshot");

        assert_eq!(snapshot.format_version, LEARNING_FORMAT_VERSION);
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.records[0].reading, "さくら");
        assert_eq!(snapshot.records[0].surface, "Sakura\tInput");
        assert_eq!(snapshot.ignored_tail_bytes, 4);
        let export = snapshot.to_tsv();
        assert!(export.contains("さくら\tSakura\\tInput\n"));
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn clear_replaces_live_and_durable_learning_as_one_terminal_operation() {
        let path = temporary_log("settings-clear");
        let service = LearningService::open(&path).expect("open");
        service.learn("かな", "仮名", 3, 4);

        assert_eq!(service.clear().expect("clear"), 1);
        assert!(read_snapshot(&path)
            .expect("empty snapshot")
            .records
            .is_empty());
        assert_eq!(
            service.preference("かな", 3, [("加奈", 4), ("仮名", 4)]),
            LearningPreference {
                exact: None,
                general: None,
            }
        );

        service.learn("かな", "加奈", 3, 4);
        drop(service);
        let reopened = LearningService::open(&path).expect("reopen");
        assert_eq!(
            reopened
                .preference("かな", 3, [("仮名", 4), ("加奈", 4)])
                .exact,
            Some(1)
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn generation_advances_after_learning_and_after_successful_clear() {
        let service = LearningService::memory();
        let initial = service.generation();
        service.learn("かな", "仮名", 3, 4);
        let learned = service.generation();
        assert_ne!(learned, initial);
        service.clear().expect("clear");
        assert_ne!(service.generation(), learned);
    }

    #[test]
    fn exact_prediction_forget_rewrites_history_and_allows_future_relearning() {
        let path = temporary_log("forget-prediction");
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        service.learn("reading", "surface", 5, 6);
        service.learn("reading", "other", 7, 8);
        let before_generation = service.generation();

        assert_eq!(
            service
                .forget_prediction_exact("reading", "surface")
                .expect("durable forget"),
            ForgetPredictionOutcome::Removed
        );
        assert_eq!(service.generation(), before_generation + 1);
        let snapshot = read_snapshot(&path).expect("snapshot after forget");
        assert!(snapshot
            .records
            .iter()
            .all(|record| !(record.reading == "reading" && record.surface == "surface")));
        assert!(snapshot
            .records
            .iter()
            .any(|record| record.reading == "reading" && record.surface == "other"));
        let mut remembered = Vec::new();
        service.visit_prediction_history("reading", |reading, surface, _, _| {
            remembered.push((reading.to_owned(), surface.to_owned()));
            true
        });
        assert_eq!(remembered, vec![("reading".to_owned(), "other".to_owned())]);
        drop(service);

        let reopened = LearningService::open(&path).expect("reopen after forget");
        let mut after_restart = Vec::new();
        reopened.visit_prediction_history("reading", |reading, surface, _, _| {
            after_restart.push((reading.to_owned(), surface.to_owned()));
            true
        });
        assert_eq!(
            after_restart,
            vec![("reading".to_owned(), "other".to_owned())]
        );

        reopened.learn("reading", "surface", 9, 10);
        reopened.maintain().expect("flush relearn");
        drop(reopened);
        let relearned = LearningService::open(&path).expect("reopen after relearn");
        assert!(read_snapshot(&path)
            .expect("snapshot after relearn")
            .records
            .iter()
            .any(|record| record.reading == "reading" && record.surface == "surface"));
        drop(relearned);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_forget_second_publish_failure_restores_old_state_and_append_owner() {
        let path = temporary_log("forget-second-publish-failure");
        let temporary = forget_temporary_path(&path);
        let recovery = forget_recovery_path(&path);
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        service.learn("reading", "other", 5, 6);
        let before_bytes = fs::read(&path).expect("old canonical");
        let before_generation = service.generation();

        {
            let _fault = ForgetFaultScope::new(&[ForgetFaultPoint::PublishMovesOldToRecovery]);
            let error = service
                .forget_prediction_exact("reading", "surface")
                .expect_err("second publish failure");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }

        assert_eq!(fs::read(&path).expect("restored canonical"), before_bytes);
        assert!(!temporary.exists(), "failed replacement temp is removed");
        assert!(
            !recovery.exists(),
            "old log was restored instead of stranded"
        );
        assert_eq!(service.generation(), before_generation);
        assert!(history_contains(&service, "reading", "surface"));
        assert!(history_contains(&service, "reading", "other"));

        service.learn("continued", "append-owner", 7, 8);
        service.maintain().expect("flush old append owner");
        drop(service);

        let reopened = LearningService::open(&path).expect("restart from old authority");
        assert!(history_contains(&reopened, "reading", "surface"));
        assert!(history_contains(&reopened, "continued", "append-owner"));
        drop(reopened);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_forget_replacement_owner_failure_never_publishes_filtered_bytes() {
        let path = temporary_log("forget-replacement-owner-failure");
        let temporary = forget_temporary_path(&path);
        let recovery = forget_recovery_path(&path);
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        let before_bytes = fs::read(&path).expect("old canonical");
        let before_generation = service.generation();

        {
            let _fault = ForgetFaultScope::new(&[ForgetFaultPoint::ReplacementOwnerOpen]);
            let error = service
                .forget_prediction_exact("reading", "surface")
                .expect_err("replacement owner preparation failure");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }

        assert_eq!(fs::read(&path).expect("old canonical"), before_bytes);
        assert!(!temporary.exists(), "unpublished replacement is cleaned");
        assert!(
            !recovery.exists(),
            "no recovery is needed before publication"
        );
        assert_eq!(service.generation(), before_generation);
        assert!(history_contains(&service, "reading", "surface"));

        service.learn("continued", "append-owner", 5, 6);
        service.maintain().expect("flush original append owner");
        drop(service);
        let reopened = LearningService::open(&path).expect("restart");
        assert!(history_contains(&reopened, "reading", "surface"));
        assert!(history_contains(&reopened, "continued", "append-owner"));
        drop(reopened);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_forget_publish_cleanup_failure_keeps_old_canonical_authoritative() {
        let path = temporary_log("forget-publish-cleanup-failure");
        let temporary = forget_temporary_path(&path);
        let recovery = forget_recovery_path(&path);
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        let before_bytes = fs::read(&path).expect("old canonical");
        let before_generation = service.generation();

        {
            let _fault = ForgetFaultScope::new(&[
                ForgetFaultPoint::Publish,
                ForgetFaultPoint::TemporaryCleanup,
            ]);
            let error = service
                .forget_prediction_exact("reading", "surface")
                .expect_err("publish and temporary cleanup failure");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("follow-up failed"));
        }

        assert_eq!(fs::read(&path).expect("old canonical"), before_bytes);
        assert!(temporary.exists(), "failed cleanup remains explicit state");
        assert!(!recovery.exists());
        assert_eq!(service.generation(), before_generation);
        assert_eq!(service.maintenance_failures(), 1);
        assert!(history_contains(&service, "reading", "surface"));

        service.learn("continued", "append-owner", 5, 6);
        drop(service);
        let reopened = LearningService::open(&path).expect("restart keeps canonical authority");
        assert!(history_contains(&reopened, "reading", "surface"));
        assert!(history_contains(&reopened, "continued", "append-owner"));
        reopened.maintain().expect("retry temporary cleanup");
        assert!(!temporary.exists());
        drop(reopened);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_forget_observation_failure_before_first_rename_keeps_old_authority() {
        let path = temporary_log("forget-observe-before-first-rename");
        let temporary = forget_temporary_path(&path);
        let recovery = forget_recovery_path(&path);
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        service.learn("reading", "other", 5, 6);
        let before_bytes = fs::read(&path).expect("old canonical");
        let before_generation = service.generation();

        {
            let _fault = ForgetFaultScope::new(&[
                ForgetFaultPoint::Publish,
                ForgetFaultPoint::PublishObservation,
            ]);
            let error = service
                .forget_prediction_exact("reading", "surface")
                .expect_err("publication and observation fail before the first rename");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("PublishObservation"));
        }

        assert_eq!(fs::read(&path).expect("old canonical"), before_bytes);
        assert!(!temporary.exists(), "unpublished replacement is cleaned");
        assert!(!recovery.exists(), "the first rename never ran");
        assert_eq!(service.generation(), before_generation);
        assert_eq!(service.maintenance_failures(), 1);
        assert!(history_contains(&service, "reading", "surface"));
        assert!(history_contains(&service, "reading", "other"));

        service.learn("continued", "append-owner", 7, 8);
        service.maintain().expect("flush original append owner");
        drop(service);

        let reopened = LearningService::open(&path).expect("restart from old authority");
        assert!(history_contains(&reopened, "reading", "surface"));
        assert!(history_contains(&reopened, "reading", "other"));
        assert!(history_contains(&reopened, "continued", "append-owner"));
        drop(reopened);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_forget_commits_when_old_backup_cleanup_is_deferred() {
        let path = temporary_log("forget-backup-cleanup-failure");
        let temporary = forget_temporary_path(&path);
        let recovery = forget_recovery_path(&path);
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        service.learn("reading", "other", 5, 6);
        let before_generation = service.generation();

        {
            let _fault = ForgetFaultScope::new(&[ForgetFaultPoint::BackupCleanup]);
            assert_eq!(
                service
                    .forget_prediction_exact("reading", "surface")
                    .expect("durable filtered replacement is committed"),
                ForgetPredictionOutcome::Removed
            );
        }

        assert_eq!(service.generation(), before_generation + 1);
        assert!(!history_contains(&service, "reading", "surface"));
        assert!(history_contains(&service, "reading", "other"));
        assert!(!snapshot_contains(&path, "reading", "surface"));
        assert!(snapshot_contains(&path, "reading", "other"));
        assert!(
            !temporary.exists(),
            "successful rename consumes replacement temp"
        );
        assert!(
            recovery.exists(),
            "unremoved old bytes remain an explicit artifact"
        );
        assert_eq!(service.maintenance_failures(), 1);

        service.learn("newer", "canonical", 7, 8);
        drop(service);

        // A stale old backup is cleanup-only once canonical exists. It must
        // not roll back a newer canonical log on restart.
        let reopened = LearningService::open(&path).expect("restart prefers canonical");
        assert!(!history_contains(&reopened, "reading", "surface"));
        assert!(history_contains(&reopened, "reading", "other"));
        assert!(history_contains(&reopened, "newer", "canonical"));
        reopened.maintain().expect("retry deferred backup cleanup");
        assert!(!recovery.exists(), "deferred backup cleanup completed");
        drop(reopened);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_forget_returns_removed_when_publish_reports_after_filtered_commit() {
        let path = temporary_log("forget-publish-after-commit-error");
        let temporary = forget_temporary_path(&path);
        let recovery = forget_recovery_path(&path);
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        service.learn("reading", "other", 5, 6);
        let before_generation = service.generation();

        {
            let _fault = ForgetFaultScope::new(&[ForgetFaultPoint::PublishCommitsThenErrors]);
            assert_eq!(
                service
                    .forget_prediction_exact("reading", "surface")
                    .expect("physical filtered canonical is a committed delete"),
                ForgetPredictionOutcome::Removed
            );
        }

        assert_eq!(service.generation(), before_generation + 1);
        assert!(!history_contains(&service, "reading", "surface"));
        assert!(history_contains(&service, "reading", "other"));
        assert!(!snapshot_contains(&path, "reading", "surface"));
        assert!(!temporary.exists());
        assert!(!recovery.exists());
        assert_eq!(service.maintenance_failures(), 1);

        service.learn("reading", "surface", 7, 8);
        service.maintain().expect("flush relearning");
        drop(service);
        let reopened = LearningService::open(&path).expect("restart after relearning");
        assert!(history_contains(&reopened, "reading", "surface"));
        drop(reopened);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_recovery_repairs_a_torn_tail_after_failed_restore() {
        let path = temporary_log("forget-recovery-torn-tail");
        let temporary = forget_temporary_path(&path);
        let recovery = forget_recovery_path(&path);
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        let before_bytes = fs::read(&path).expect("old canonical");
        let before_generation = service.generation();

        {
            let _fault = ForgetPredictionDeepRecoveryFault::install();
            let error = service
                .forget_prediction_exact("reading", "surface")
                .expect_err("old canonical reaches recovery and immediate restore fails");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }

        assert!(!path.exists(), "failed restore leaves canonical absent");
        assert_eq!(fs::read(&recovery).expect("old recovery"), before_bytes);
        assert!(
            temporary.exists(),
            "unpublished filtered replacement is retained"
        );
        assert_eq!(service.generation(), before_generation);
        assert!(history_contains(&service, "reading", "surface"));

        // The live service still owns the old inode after the first rename.
        // Its valid append must survive startup recovery along with the old
        // target history that failed to delete.
        service.learn("continued", "backup-owner", 5, 6);
        {
            let state = service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .log
                .file
                .as_ref()
                .expect("preserved old append owner")
                .sync_data()
                .expect("sync preserved old append owner");
        }
        let verified_recovery_len = fs::metadata(&recovery)
            .expect("verified recovery metadata")
            .len();

        // Simulate a crash after the next frame's envelope and all but one
        // payload byte reached the recovery inode.
        let torn_payload = encode_record("discarded", "torn", 7, 8, 9).expect("torn payload");
        append_test_frame(
            &recovery,
            &torn_payload,
            crc32(&torn_payload),
            torn_payload.len() - 1,
        );
        assert!(
            fs::metadata(&recovery)
                .expect("torn recovery metadata")
                .len()
                > verified_recovery_len,
            "test injected a durable incomplete final frame"
        );
        drop(service);

        let reopened = LearningService::open(&path).expect("restart repairs and restores recovery");
        assert_eq!(
            fs::metadata(&path)
                .expect("repaired canonical metadata")
                .len(),
            verified_recovery_len,
            "startup retained exactly the verified recovery prefix"
        );
        assert!(
            !recovery.exists(),
            "recovery inode was restored to canonical"
        );
        assert!(history_contains(&reopened, "reading", "surface"));
        assert!(history_contains(&reopened, "continued", "backup-owner"));
        assert!(snapshot_contains(&path, "reading", "surface"));
        assert!(snapshot_contains(&path, "continued", "backup-owner"));
        assert!(!snapshot_contains(&path, "discarded", "torn"));

        reopened
            .maintain()
            .expect("settle stale filtered temporary after recovery");
        assert!(!temporary.exists(), "stale filtered temporary was settled");

        reopened.learn("after", "restart", 9, 10);
        assert_eq!(
            reopened.maintain().expect("flush post-recovery learning"),
            MaintenanceOutcome::Flushed
        );
        drop(reopened);

        let restarted = LearningService::open(&path).expect("restart after recovery learning");
        assert!(history_contains(&restarted, "reading", "surface"));
        assert!(history_contains(&restarted, "continued", "backup-owner"));
        assert!(history_contains(&restarted, "after", "restart"));
        drop(restarted);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_recovery_rejects_complete_semantic_corruption() {
        let path = temporary_log("forget-recovery-semantic-corruption");
        let recovery = forget_recovery_path(&path);
        {
            let service = LearningService::open(&path).expect("open");
            service.learn("reading", "surface", 3, 4);
            service.maintain().expect("flush canonical");
        }
        fs::rename(&path, &recovery).expect("move old authority to recovery");
        let malformed_payload = [0xff];
        append_test_frame(
            &recovery,
            &malformed_payload,
            crc32(&malformed_payload),
            malformed_payload.len(),
        );
        let before = fs::read(&recovery).expect("complete malformed recovery");

        let error = LearningService::open(&path)
            .expect_err("a complete semantic error must not be repaired as a torn tail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("invalid complete record"));
        assert!(
            !path.exists(),
            "canonical was not published from corrupt recovery"
        );
        assert_eq!(
            fs::read(&recovery).expect("recovery remains intact"),
            before
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_recovery_rejects_complete_checksum_corruption() {
        let path = temporary_log("forget-recovery-checksum-corruption");
        let recovery = forget_recovery_path(&path);
        {
            let service = LearningService::open(&path).expect("open");
            service.learn("reading", "surface", 3, 4);
            service.maintain().expect("flush canonical");
        }
        fs::rename(&path, &recovery).expect("move old authority to recovery");
        let payload = encode_record("complete", "checksum", 5, 6, 7).expect("complete payload");
        append_test_frame(&recovery, &payload, crc32(&payload) ^ 1, payload.len());
        let before = fs::read(&recovery).expect("complete checksum-corrupt recovery");

        let error = LearningService::open(&path)
            .expect_err("a complete checksum error must not be repaired as a torn tail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error
            .to_string()
            .contains("invalid complete record checksum"));
        assert!(
            !path.exists(),
            "canonical was not published from corrupt recovery"
        );
        assert_eq!(
            fs::read(&recovery).expect("recovery remains intact"),
            before
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_recovery_rejects_invalid_header_or_version() {
        for (name, recovery_bytes) in [
            ("invalid-header", b"not a learning log".to_vec()),
            (
                "unsupported-version",
                header(LEARNING_FORMAT_VERSION + 1).to_vec(),
            ),
        ] {
            let path = temporary_log(name);
            let recovery = forget_recovery_path(&path);
            fs::write(&recovery, &recovery_bytes).expect("write invalid recovery");

            let error = LearningService::open(&path)
                .expect_err("invalid recovery header or version must fail closed");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(
                !path.exists(),
                "canonical was not created after recovery failure"
            );
            assert_eq!(
                fs::read(&recovery).expect("recovery remains intact"),
                recovery_bytes
            );
            let _ = fs::remove_dir_all(path.parent().expect("parent"));
        }
    }

    #[test]
    fn exact_prediction_forget_restore_failure_keeps_a_restart_recovery_log_and_append_owner() {
        let path = temporary_log("forget-restore-failure");
        let temporary = forget_temporary_path(&path);
        let recovery = forget_recovery_path(&path);
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        let before_bytes = fs::read(&path).expect("old canonical");
        let before_generation = service.generation();

        {
            let _fault = ForgetFaultScope::new(&[
                ForgetFaultPoint::PublishMovesOldToRecovery,
                ForgetFaultPoint::RecoveryRestore,
            ]);
            let error = service
                .forget_prediction_exact("reading", "surface")
                .expect_err("restore failure is surfaced");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }

        assert!(
            !path.exists(),
            "canonical is never recreated during failed recovery"
        );
        assert_eq!(fs::read(&recovery).expect("recovery bytes"), before_bytes);
        assert!(
            temporary.exists(),
            "filtered temp is retained only for tracked cleanup"
        );
        assert_eq!(service.generation(), before_generation);
        assert!(history_contains(&service, "reading", "surface"));

        // The original shared writer now owns the backup inode, so durable
        // relearning can continue until restart performs the deterministic
        // restore. Sync it directly without invoking maintenance recovery.
        service.learn("continued", "backup-owner", 5, 6);
        {
            let state = service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .log
                .file
                .as_ref()
                .expect("backup append owner")
                .sync_data()
                .expect("sync backup append owner");
        }
        assert!(snapshot_contains(&recovery, "reading", "surface"));
        assert!(snapshot_contains(&recovery, "continued", "backup-owner"));
        drop(service);

        // Startup restores the verified old recovery before create_if_missing
        // can run. The filtered temp is cleanup-only and cannot override it.
        let reopened = LearningService::open(&path).expect("restart restores old authority");
        assert!(path.exists());
        assert!(!recovery.exists(), "recovery was renamed back to canonical");
        assert!(history_contains(&reopened, "reading", "surface"));
        assert!(history_contains(&reopened, "continued", "backup-owner"));
        reopened.maintain().expect("clean stale filtered temp");
        assert!(
            !temporary.exists(),
            "stale filtered temp was never published"
        );
        drop(reopened);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn exact_prediction_forget_durable_failure_keeps_authoritative_state() {
        let path = temporary_log("forget-failure");
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        let before_file = fs::read(&path).expect("read old log");
        let before_generation = service.generation();
        {
            let mut state = service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.log.file = None;
        }

        let error = service
            .forget_prediction_exact("reading", "surface")
            .expect_err("missing writer is a durable failure");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(fs::read(&path).expect("old log survives"), before_file);
        assert_eq!(service.generation(), before_generation);
        let mut remembered = false;
        service.visit_prediction_history("reading", |reading, surface, _, _| {
            remembered = reading == "reading" && surface == "surface";
            false
        });
        assert!(
            remembered,
            "the old in-memory history remains authoritative"
        );
        assert_eq!(
            LearningService::memory()
                .forget_prediction_exact("reading", "surface")
                .expect("memory outcome"),
            ForgetPredictionOutcome::Unavailable
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }
}
