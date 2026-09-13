//! Bounded persistent personalization (DESIGN §5.4).
//!
//! The on-disk log is the source of truth. Each record is length- and CRC32-
//! prefixed; startup truncates only a torn/corrupt tail after the last verified
//! record. The in-memory index is a fixed four-way set-associative table, so
//! both memory and lookup work stay O(1) as history grows. A joined maintenance
//! thread flushes and compacts the source log before its hard disk ceiling.

use std::collections::HashSet;
#[cfg(test)]
use std::fs::{self, File, OpenOptions};
use std::io;
#[cfg(test)]
use std::io::Write;
#[cfg(all(test, windows))]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_ipc::debug_trace;
use sakura_proto::{EngineTimingSite, FixedStr, MAX_PREEDIT_BYTES};
pub use sakura_store::learning::{
    read_snapshot, LearningRecord, LearningSnapshot, LEARNING_FORMAT_VERSION,
    MAX_LEARNING_LOG_BYTES,
};
#[cfg(test)]
use sakura_store::learning::{
    test_crc32 as crc32, test_encode_record as encode_record, test_header as header,
    test_read_header as read_header, test_read_within_bound as read_within_bound,
    LearningFaultPoint as ForgetFaultPoint, LearningFaultScope as ForgetFaultScope,
    FORMAT_VERSION_1, FORMAT_VERSION_2, HEADER_LEN, RECORD_COMMIT, REPAIR_SUPPRESS_SURFACE,
};
use sakura_store::learning::{
    LearningLog, LogForget, LogMaintenance, OperationReceipt, ReplayEvent, ReplayView,
};

use crate::session::text_hash;
use crate::timing;

const BUCKETS: usize = 32_768;
const WAYS: usize = 3;
const SLOT_COUNT: usize = BUCKETS * WAYS;
pub const MAX_LEARNING_ENTRIES: usize = SLOT_COUNT;
/// Fixed number of exact `(reading, surface)` entries retained for prediction.
///
/// This is a storage and ranking window, not a per-result display limit.  The
/// prediction module independently caps how many retained entries can appear
/// in one suggestion result.
const MAX_PREDICTION_HISTORY_ENTRIES: usize = 128;
const MAX_HISTORY_TEXT_BYTES: usize = 512;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5);
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
            reading,
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
            reading,
            candidates,
        );
        LearningPreference { exact, general }
    }

    fn best_candidate<'a>(
        &self,
        query: PreferenceQuery,
        reading: &str,
        candidates: impl IntoIterator<Item = (&'a str, u16)>,
    ) -> Option<usize> {
        let mut best = None::<(usize, u64, u64)>;
        let mut skipped_identity = 0u64;
        for (candidate_index, (surface, right_context)) in candidates.into_iter().enumerate() {
            if surface == reading {
                skipped_identity = skipped_identity.saturating_add(1);
                continue;
            }
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
        debug_trace::emit(sakura_ipc::debug_trace::TraceEvent {
            component: "engine",
            instance: 0,
            event: "identity_pref",
            decision: if query.exact { "exact" } else { "general" },
            k0: skipped_identity,
            k1: best.map(|(index, _, _)| index as u64 + 1).unwrap_or(0),
            k2: u64::from(query.left_context),
            k3: u64::from(query.general),
        });
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

    fn for_each(&self, mut visit: impl FnMut(&str, &str)) {
        for entry in self.entries.iter() {
            if entry.occupied {
                visit(entry.reading.as_str(), entry.surface.as_str());
            }
        }
    }
}

#[derive(Debug)]
struct State {
    index: Index,
    prediction_history: PredictionHistory,
    /// Readings for which the user rejected automatic repair by resizing
    /// segments. Durable via CRC-framed suppress markers in the learning log.
    repair_suppress: HashSet<u64>,
    log: LearningLog,
    path: Option<PathBuf>,
    sequence: u64,
}

struct PreparedLearning {
    index: Index,
    prediction_history: PredictionHistory,
    repair_suppress: HashSet<u64>,
    sequence: u64,
}

fn prepare_learning(events: ReplayView<'_>) -> PreparedLearning {
    let mut prepared = PreparedLearning {
        index: Index::new(),
        prediction_history: PredictionHistory::new(),
        repair_suppress: HashSet::new(),
        sequence: 0,
    };
    for event in events {
        prepared.sequence = prepared.sequence.saturating_add(1);
        match event {
            ReplayEvent::Commit {
                day,
                left_context,
                right_context,
                reading,
                surface,
            } => {
                prepared.index.learn(
                    left_context,
                    right_context,
                    reading,
                    surface,
                    day,
                    prepared.sequence,
                );
                prepared.prediction_history.learn(
                    reading,
                    surface,
                    right_context,
                    day,
                    prepared.sequence,
                );
            }
            ReplayEvent::RepairSuppress { reading, .. } => {
                prepared.repair_suppress.insert(text_hash(reading));
            }
        }
    }
    prepared
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

impl LearningService {
    /// Takes the store's mutex, recording how long the wait was.
    ///
    /// Every public entry point on this service goes through here so the wait
    /// is measured once, in one place, rather than at nine call sites that
    /// would drift apart. The measurement matters because the holder can be
    /// doing something very long — `compact_state` keeps the lock across a
    /// full log replay and rewrite — and a caller blocked behind that is
    /// indistinguishable, from the outside, from a conversion that was simply
    /// slow. #148 has to be able to tell those apart before it can attribute a
    /// stalled keystroke to anything.
    ///
    /// A poisoned mutex is recovered rather than propagated, which is what
    /// every call site here already did: the learned data behind it is a cache
    /// that can be rebuilt, and refusing to convert because an unrelated
    /// thread panicked would turn a degraded feature into a dead IME.
    fn lock_state(&self) -> MutexGuard<'_, State> {
        let started = Instant::now();
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        timing::observe(EngineTimingSite::LearningLockWait, started.elapsed());
        guard
    }

    pub fn memory() -> Self {
        Self {
            state: Mutex::new(State {
                index: Index::new(),
                prediction_history: PredictionHistory::new(),
                repair_suppress: HashSet::new(),
                log: LearningLog::memory(),
                path: None,
                sequence: 0,
            }),
            generation: AtomicU64::new(0),
            skipped_writes: AtomicU64::new(0),
            recovered_tail_bytes: AtomicU64::new(0),
            maintenance_failures: AtomicU64::new(0),
        }
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        let (log, prepared, receipt) =
            LearningLog::open(path, prepare_learning).map_err(|error| error.source)?;
        Ok(Self {
            state: Mutex::new(State {
                index: prepared.index,
                prediction_history: prepared.prediction_history,
                repair_suppress: prepared.repair_suppress,
                log,
                path: Some(path.to_owned()),
                sequence: prepared.sequence,
            }),
            generation: AtomicU64::new(0),
            skipped_writes: AtomicU64::new(0),
            recovered_tail_bytes: AtomicU64::new(receipt.recovered_tail_bytes),
            maintenance_failures: AtomicU64::new(receipt.maintenance_failure_delta),
        })
    }

    pub fn learn(&self, reading: &str, surface: &str, left_context: u16, right_context: u16) {
        if reading.is_empty() || surface.is_empty() {
            return;
        }
        let day = unix_day();
        let mut state = self.lock_state();
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
        let state = self.lock_state();
        state
            .index
            .preference(reading, left_context, candidates, unix_day())
    }

    /// Remembers that the user rejected automatic repair for this exact reading
    /// by resizing segments. Future conversions of the same reading skip repair.
    pub fn suppress_repair_reading(&self, reading: &str) {
        if reading.is_empty() {
            return;
        }
        let day = unix_day();
        let mut state = self.lock_state();
        let hash = text_hash(reading);
        if !state.repair_suppress.insert(hash) {
            return;
        }
        if state.log.append_repair_suppress(reading, day).is_err() {
            self.skipped_writes.fetch_add(1, Ordering::Relaxed);
        }
        drop(state);
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub fn is_repair_suppressed(&self, reading: &str) -> bool {
        if reading.is_empty() {
            return false;
        }
        let state = self.lock_state();
        state.repair_suppress.contains(&text_hash(reading))
    }

    /// Collects dictionary readings suggested by commit history for a typed
    /// typo. Only an already-known repair variant is used here; reverse
    /// dictionary lookup is deliberately not performed on the synchronous key
    /// path.
    pub fn collect_commit_repair_readings(
        &self,
        typed: &str,
        support: sakura_core::InputSupport,
    ) -> Vec<FixedStr<MAX_PREEDIT_BYTES>> {
        let mut out = Vec::new();
        if !support.is_active() || !support.commit_based || typed.is_empty() {
            return out;
        }
        let variants =
            sakura_core::collect_repair_variants(typed, support, sakura_core::MAX_REPAIR_VARIANTS);
        if variants.is_empty() {
            // A normal reading has no plausible typo-repair target. In
            // particular, do not reverse-convert every exact history entry
            // on the synchronous Space path just to discover that it maps
            // back to the same reading. That lookup can exceed the TSF
            // keystroke budget even though ordinary conversion is cheap.
            return out;
        }
        let state = self.lock_state();
        state.prediction_history.for_each(|reading, _surface| {
            if out.len() >= 8 {
                return;
            }
            if variants
                .iter()
                // A prior commit is evidence for named input rules, not for
                // broad edit-distance guesses. Otherwise merely committing
                // "い" makes "う" -> "い" a history-authorized repair.
                .any(|variant| {
                    variant.kind == sakura_core::RepairKind::Rule
                        && variant.repaired.as_str() == reading
                })
                && !out.iter().any(|existing| existing.as_str() == reading)
            {
                let mut text = FixedStr::new();
                if text.push_str(reading).is_ok() {
                    out.push(text);
                }
            }
        });
        out
    }

    pub(crate) fn visit_prediction_history(
        &self,
        prefix: &str,
        visit: impl FnMut(&str, &str, u16, i64) -> bool,
    ) {
        let state = self.lock_state();
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
        let mut state = self.lock_state();
        let transaction = {
            let State {
                index,
                prediction_history,
                repair_suppress,
                log,
                sequence,
                ..
            } = &mut *state;
            log.forget_exact(reading, surface, prepare_learning, |prepared| {
                *index = prepared.index;
                *prediction_history = prepared.prediction_history;
                *repair_suppress = prepared.repair_suppress;
                *sequence = prepared.sequence;
            })
        };
        match transaction {
            Ok((outcome, receipt)) => {
                self.apply_receipt(receipt);
                match outcome {
                    LogForget::NotFound => Ok(ForgetPredictionOutcome::NotFound),
                    LogForget::Unavailable => Ok(ForgetPredictionOutcome::Unavailable),
                    LogForget::Removed { removed: _ } => {
                        drop(state);
                        self.generation.fetch_add(1, Ordering::Release);
                        Ok(ForgetPredictionOutcome::Removed)
                    }
                }
            }
            Err(error) => {
                let receipt = error.receipt;
                let source = error.source;
                self.apply_receipt(receipt);
                Err(source)
            }
        }
    }

    fn apply_receipt(&self, receipt: OperationReceipt) {
        self.recovered_tail_bytes
            .fetch_add(receipt.recovered_tail_bytes, Ordering::Relaxed);
        self.maintenance_failures
            .fetch_add(receipt.maintenance_failure_delta, Ordering::Relaxed);
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
        match state.log.maintain(prepare_learning) {
            Ok((outcome, receipt)) => {
                self.apply_receipt(receipt);
                match outcome {
                    LogMaintenance::NotDue => Ok(MaintenanceOutcome::Idle),
                    LogMaintenance::Flushed => Ok(MaintenanceOutcome::Flushed),
                    LogMaintenance::Compacted(prepared) => {
                        state.index = prepared.index;
                        state.prediction_history = prepared.prediction_history;
                        state.repair_suppress = prepared.repair_suppress;
                        state.sequence = prepared.sequence;
                        Ok(MaintenanceOutcome::Compacted)
                    }
                }
            }
            Err(error) => {
                let receipt = error.receipt;
                let source = error.source;
                self.apply_receipt(receipt);
                Err(source)
            }
        }
    }

    /// Clears both the live personalization indexes and their durable log
    /// under the same mutex used by commits and maintenance. Every failure
    /// path either restores the old writer or leaves it explicitly disabled;
    /// no caller can observe an empty in-memory index backed by the old log.
    pub fn clear(&self) -> io::Result<u64> {
        let mut state = self.lock_state();
        match state.log.clear(prepare_learning) {
            Ok((cleared_records, prepared, receipt)) => {
                self.apply_receipt(receipt);
                state.index = prepared.index;
                state.prediction_history = prepared.prediction_history;
                state.repair_suppress = prepared.repair_suppress;
                state.sequence = prepared.sequence;
                self.generation.fetch_add(1, Ordering::Release);
                Ok(cleared_records)
            }
            Err(error) => {
                let receipt = error.receipt;
                let source = error.source;
                self.apply_receipt(receipt);
                Err(source)
            }
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.lock_state().path.clone()
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
#[cfg(all(test, feature = "dev-fixtures"))]
pub(crate) struct ForgetPredictionCommittedObservationFault {
    _scope: ForgetFaultScope,
}

#[cfg(all(test, feature = "dev-fixtures"))]
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

fn unix_day() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u32::try_from(elapsed.as_secs() / 86_400).unwrap_or(u32::MAX)
        })
}

#[cfg(test)]
fn compact_state(state: &mut State, target_bytes: usize, target_records: u64) -> io::Result<()> {
    let prepared = state
        .log
        .test_compact(target_bytes, target_records, prepare_learning)
        .map_err(|error| error.source)?;
    state.index = prepared.index;
    state.prediction_history = prepared.prediction_history;
    state.repair_suppress = prepared.repair_suppress;
    state.sequence = prepared.sequence;
    Ok(())
}

#[cfg(test)]
fn forget_temporary_path(path: &Path) -> PathBuf {
    path.with_extension("forget.tmp")
}

#[cfg(test)]
fn forget_recovery_path(path: &Path) -> PathBuf {
    path.with_extension("forget.recovery")
}

#[cfg(test)]
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

#[cfg(test)]
#[path = "learning_tests.rs"]
mod tests;
