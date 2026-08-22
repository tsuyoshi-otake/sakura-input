//! Process-wide, bounded prediction worker.
//!
//! Dictionary prediction is indexed once at startup. Every pipe worker then
//! submits a fixed-capacity query to one single-slot mailbox; a newer pending
//! query replaces an older pending query, while the one persistent worker owns
//! all ranking work. Submission, waiting, and result transfer allocate nothing.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use sakura_core::{
    collect_repair_variants, Dictionary, EntryFlags, InputSupport, UserDictionary,
    MAX_PREDICTION_REPAIR_VARIANTS,
};
use sakura_proto::{FixedStr, SessionId};

use crate::dictionary::ConversionService;
use crate::learning::LearningService;

/// The suggest list is intentionally one numbered page, unlike conversion's
/// two-page candidate table.
pub const MAX_SUGGESTIONS: usize = 9;
/// DESIGN caps readings at 128 characters. Four UTF-8 bytes per scalar keeps
/// the hand-off fixed even for the worst case.
pub const MAX_PREDICTION_READING_BYTES: usize = 512;
pub const MAX_PREDICTION_SURFACE_BYTES: usize = 512;
pub const MAX_PREDICTION_ANNOTATION_BYTES: usize = 256;

const RANKED_SCRATCH: usize = MAX_SUGGESTIONS * 4;
// An explicit user entry outranks a system completion at the same prefix.
// The largest curated user-POS default is 2,500, so 3,000 makes that
// structural rather than fixture-dependent while retaining ordering among
// user entries themselves.
const USER_DICTIONARY_BONUS: i64 = 3_000;
const BASE_IT_BIAS_PER_MILLE: u16 = 100;
const MAX_IT_BOOST: i64 = 800;
/// Once the user has typed a word-sized prefix, extending it is weaker
/// evidence than an entry whose reading ends exactly at the caret.  Keep the
/// adjustment bounded so a very long completion cannot overflow or erase the
/// dictionary's base ordering among similarly sized candidates.
const MIN_PREFIX_CHARS_FOR_COMPLETION_DISTANCE: usize = 3;
const COMPLETION_COST_PER_CHAR: i64 = 600;
const MAX_COMPLETION_COST: i64 = 2_400;
/// A reading repair may correct the portion already typed, but it must not
/// also unlock an arbitrarily distant completion. Give short prefixes a small
/// useful window and scale longer prefixes only up to a fixed hot-path bound.
const MIN_REPAIR_COMPLETION_CHARS: usize = 2;
const MAX_REPAIR_COMPLETION_CHARS: usize = 6;
/// Maximum retained-history candidates copied into one prediction result.
///
/// This output boundary is intentionally independent from learning's 128-slot
/// in-memory retention window: history keeps a broader ranking window while a
/// single result reserves room for dictionary and user suggestions.
const MAX_HISTORY_SUGGESTIONS_PER_RESULT: usize = 4;

/// Engine-internal provenance assigned before merged prediction results are
/// deduplicated. It never crosses the protocol boundary: the renderer and TSF
/// deliberately receive only surface/annotation data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PredictionSource {
    /// A durable learned commit; this is the only deletable source.
    History,
    /// A shipped system dictionary completion.
    #[default]
    System,
    /// A user dictionary completion.
    User,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictionCandidate {
    reading: FixedStr<MAX_PREDICTION_READING_BYTES>,
    surface: FixedStr<MAX_PREDICTION_SURFACE_BYTES>,
    annotation: FixedStr<MAX_PREDICTION_ANNOTATION_BYTES>,
    right_id: u16,
    flags: EntryFlags,
    source: PredictionSource,
    /// Exact mapped-system ordinal. History and user candidates never carry
    /// it, so selected-candidate detail cannot be inferred from a surface.
    system_entry_index: Option<u32>,
}

impl PredictionCandidate {
    pub fn reading(&self) -> &str {
        self.reading.as_str()
    }

    pub fn surface(&self) -> &str {
        self.surface.as_str()
    }

    pub fn annotation(&self) -> &str {
        self.annotation.as_str()
    }

    pub const fn right_id(&self) -> u16 {
        self.right_id
    }

    pub const fn flags(&self) -> EntryFlags {
        self.flags
    }

    pub(crate) const fn source(&self) -> PredictionSource {
        self.source
    }

    pub const fn system_entry_index(&self) -> Option<u32> {
        self.system_entry_index
    }
}

impl Default for PredictionCandidate {
    fn default() -> Self {
        Self {
            reading: FixedStr::new(),
            surface: FixedStr::new(),
            annotation: FixedStr::new(),
            right_id: 0,
            flags: EntryFlags::NONE,
            source: PredictionSource::System,
            system_entry_index: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictionResult {
    sequence: u64,
    session: SessionId,
    generation: u64,
    candidates: Box<[PredictionCandidate]>,
    len: usize,
}

impl PredictionResult {
    fn empty(sequence: u64, session: SessionId, generation: u64) -> Self {
        let mut candidates = Vec::with_capacity(MAX_SUGGESTIONS);
        candidates.resize_with(MAX_SUGGESTIONS, PredictionCandidate::default);
        Self {
            sequence,
            session,
            generation,
            candidates: candidates.into_boxed_slice(),
            len: 0,
        }
    }

    fn reset(&mut self, sequence: u64, session: SessionId, generation: u64) {
        self.sequence = sequence;
        self.session = session;
        self.generation = generation;
        self.len = 0;
    }

    pub const fn session(&self) -> SessionId {
        self.session
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn candidates(&self) -> &[PredictionCandidate] {
        &self.candidates[..self.len]
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn contains_surface(&self, surface: &str) -> bool {
        self.candidates()
            .iter()
            .any(|candidate| candidate.surface() == surface)
    }

    fn push(&mut self, candidate: PredictionCandidate) -> bool {
        if self.len >= self.candidates.len() || self.contains_surface(candidate.surface()) {
            return false;
        }
        self.candidates[self.len] = candidate;
        self.len += 1;
        true
    }

    fn copy_from_result(&mut self, source: &Self) {
        self.sequence = source.sequence;
        self.session = source.session;
        self.generation = source.generation;
        for (destination, candidate) in self
            .candidates
            .iter_mut()
            .zip(source.candidates.iter())
            .take(source.len)
        {
            destination.clone_from(candidate);
        }
        self.len = source.len;
    }
}

impl Default for PredictionResult {
    fn default() -> Self {
        Self::empty(0, 0, 0)
    }
}

#[derive(Debug)]
pub enum StartError {
    Dictionary(sakura_core::dictionary::Error),
    Thread(io::Error),
}

impl core::fmt::Display for StartError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Dictionary(error) => write!(f, "prediction index: {error}"),
            Self::Thread(error) => write!(f, "prediction worker: {error}"),
        }
    }
}

impl std::error::Error for StartError {}

#[derive(Debug, Clone)]
struct Query {
    sequence: u64,
    session: SessionId,
    generation: u64,
    prefix: FixedStr<MAX_PREDICTION_READING_BYTES>,
    domain_it_per_mille: u16,
    input_support: InputSupport,
    skip_input_repair: bool,
    /// Snapshot of the Issue #63 SPELLING_CORRECTION gate at publish time so
    /// a live preference change cannot widen admission mid-worker.
    allow_spelling_correction: bool,
}

#[derive(Debug, Default)]
struct MailboxState {
    pending: Option<Query>,
    result: PredictionResult,
    has_result: bool,
}

#[derive(Debug)]
struct Mailbox {
    state: Mutex<MailboxState>,
    pending_changed: Condvar,
    result_changed: Condvar,
    stopping: AtomicBool,
    next_sequence: AtomicU64,
    coalesced: AtomicU64,
    #[cfg(test)]
    scripted: Mutex<Option<TestPredictionScript>>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct TestPredictionScript {
    available: bool,
    reading: FixedStr<MAX_PREDICTION_READING_BYTES>,
    surface: FixedStr<MAX_PREDICTION_SURFACE_BYTES>,
}

impl Mailbox {
    fn new() -> Self {
        Self {
            state: Mutex::new(MailboxState::default()),
            pending_changed: Condvar::new(),
            result_changed: Condvar::new(),
            stopping: AtomicBool::new(false),
            next_sequence: AtomicU64::new(0),
            coalesced: AtomicU64::new(0),
            #[cfg(test)]
            scripted: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn scripted_request_into(
        &self,
        session: SessionId,
        generation: u64,
        destination: &mut PredictionResult,
    ) -> Option<bool> {
        let script = self
            .scripted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()?;
        let sequence = self.next_sequence()?;
        destination.reset(sequence, session, generation);
        if script.available {
            let mut candidate = PredictionCandidate {
                reading: script.reading,
                surface: script.surface,
                ..PredictionCandidate::default()
            };
            candidate.annotation.push_str("test").ok()?;
            let _ = destination.push(candidate);
        }
        Some(true)
    }

    #[cfg(test)]
    fn set_scripted_prediction(&self, reading: &str, surface: &str) {
        let mut script = TestPredictionScript {
            available: false,
            reading: FixedStr::new(),
            surface: FixedStr::new(),
        };
        script
            .reading
            .push_str(reading)
            .expect("scripted reading fits");
        script
            .surface
            .push_str(surface)
            .expect("scripted surface fits");
        *self
            .scripted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(script);
    }

    #[cfg(test)]
    fn set_scripted_prediction_available(&self, available: bool) {
        if let Some(script) = self
            .scripted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            script.available = available;
        }
    }

    fn next_sequence(&self) -> Option<u64> {
        self.next_sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .ok()
            .and_then(|before| before.checked_add(1))
    }

    fn publish(
        &self,
        session: SessionId,
        generation: u64,
        prefix: &str,
        domain_it_per_mille: u16,
        input_support: InputSupport,
        skip_input_repair: bool,
    ) -> Option<u64> {
        if self.stopping.load(Ordering::Acquire) || prefix.is_empty() {
            return None;
        }
        let mut fixed_prefix = FixedStr::new();
        fixed_prefix.push_str(prefix).ok()?;
        let sequence = self.next_sequence()?;
        let query = Query {
            sequence,
            session,
            generation,
            prefix: fixed_prefix,
            domain_it_per_mille: domain_it_per_mille.min(1_000),
            input_support,
            skip_input_repair,
            allow_spelling_correction: input_support.allows_spelling_correction(skip_input_repair),
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.pending.replace(query).is_some() {
            self.coalesced.fetch_add(1, Ordering::Relaxed);
        }
        self.pending_changed.notify_one();
        Some(sequence)
    }

    fn wait(&self, sequence: u64, timeout: Duration) -> Option<PredictionResult> {
        let mut result = PredictionResult::default();
        self.wait_into(sequence, timeout, &mut result)
            .then_some(result)
    }

    fn wait_into(
        &self,
        sequence: u64,
        timeout: Duration,
        destination: &mut PredictionResult,
    ) -> bool {
        let started = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if state.has_result {
                let result = &state.result;
                if result.sequence == sequence {
                    destination.copy_from_result(result);
                    return true;
                }
                if result.sequence > sequence {
                    return false;
                }
            }
            if self.stopping.load(Ordering::Acquire) {
                return false;
            }
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                return false;
            };
            let (next, wait) = self
                .result_changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if wait.timed_out() && (!state.has_result || state.result.sequence != sequence) {
                return false;
            }
        }
    }

    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        self.pending_changed.notify_all();
        self.result_changed.notify_all();
    }
}

/// Cloneable request side shared by every pipe worker.
#[derive(Debug)]
pub struct PredictionService {
    mailbox: Arc<Mailbox>,
}

impl PredictionService {
    /// Publishes the newest prefix and waits only for the caller's own result.
    /// A coalesced, stopped, oversized, or timed-out request has the explicit
    /// terminal result `None`; callers keep ordinary composition visible.
    #[allow(clippy::too_many_arguments)]
    pub fn request(
        &self,
        session: SessionId,
        generation: u64,
        prefix: &str,
        domain_it_per_mille: u16,
        input_support: InputSupport,
        skip_input_repair: bool,
        timeout: Duration,
    ) -> Option<PredictionResult> {
        let sequence = self.mailbox.publish(
            session,
            generation,
            prefix,
            domain_it_per_mille,
            input_support,
            skip_input_repair,
        )?;
        self.mailbox.wait(sequence, timeout)
    }

    /// Allocation-free and small-stack variant used by pipe workers. The
    /// result is copied straight into caller-owned fixed buffers instead of
    /// returning a large array through every frame on the 128 KiB stack.
    #[allow(clippy::too_many_arguments)]
    pub fn request_into(
        &self,
        session: SessionId,
        generation: u64,
        prefix: &str,
        domain_it_per_mille: u16,
        input_support: InputSupport,
        skip_input_repair: bool,
        timeout: Duration,
        destination: &mut PredictionResult,
    ) -> bool {
        #[cfg(test)]
        if let Some(result) = self
            .mailbox
            .scripted_request_into(session, generation, destination)
        {
            return result;
        }
        let Some(sequence) = self.mailbox.publish(
            session,
            generation,
            prefix,
            domain_it_per_mille,
            input_support,
            skip_input_repair,
        ) else {
            return false;
        };
        self.mailbox.wait_into(sequence, timeout, destination)
    }

    pub fn coalesced_requests(&self) -> u64 {
        self.mailbox.coalesced.load(Ordering::Relaxed)
    }

    /// Number of accepted queries published to the worker mailbox. This test
    /// helper reads the existing monotonic sequence counter; it adds no
    /// production-side instrumentation to the prediction hot path.
    #[cfg(test)]
    pub fn request_count(&self) -> u64 {
        self.mailbox.next_sequence.load(Ordering::Relaxed)
    }

    /// Installs a deterministic unit-test response source. The first mode is
    /// empty, so callers can exercise the bounded explicit retry transition;
    /// `test_set_scripted_prediction_available(true)` exposes the candidate
    /// for the retry without involving the worker thread or a timeout.
    #[cfg(test)]
    pub(crate) fn test_script_prediction(&self, reading: &str, surface: &str) {
        self.mailbox.set_scripted_prediction(reading, surface);
    }

    #[cfg(test)]
    pub(crate) fn test_set_scripted_prediction_available(&self, available: bool) {
        self.mailbox.set_scripted_prediction_available(available);
    }
}

/// Owns the one persistent prediction thread and joins it explicitly.
#[derive(Debug)]
pub struct PredictionRuntime {
    service: Arc<PredictionService>,
    worker: Option<JoinHandle<()>>,
}

impl PredictionRuntime {
    pub fn start(conversion: Arc<ConversionService>) -> Result<Self, StartError> {
        Self::start_inner(conversion, None)
    }

    pub fn start_with_learning(
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
    ) -> Result<Self, StartError> {
        Self::start_inner(conversion, Some(learning))
    }

    fn start_inner(
        conversion: Arc<ConversionService>,
        learning: Option<Arc<LearningService>>,
    ) -> Result<Self, StartError> {
        let index =
            PredictionIndex::build(conversion.dictionary()).map_err(StartError::Dictionary)?;
        let mailbox = Arc::new(Mailbox::new());
        let service = Arc::new(PredictionService {
            mailbox: Arc::clone(&mailbox),
        });
        let worker = thread::Builder::new()
            .name("sakura-predict".to_owned())
            .stack_size(256 * 1024)
            .spawn(move || worker(mailbox, index, conversion, learning))
            .map_err(StartError::Thread)?;
        Ok(Self {
            service,
            worker: Some(worker),
        })
    }

    pub fn service(&self) -> Arc<PredictionService> {
        Arc::clone(&self.service)
    }

    pub fn stop(mut self) -> thread::Result<()> {
        self.service.mailbox.stop();
        match self.worker.take() {
            Some(worker) => worker.join(),
            None => Ok(()),
        }
    }
}

impl Drop for PredictionRuntime {
    fn drop(&mut self) {
        self.service.mailbox.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct IndexedEntry {
    reading_start: u32,
    entry_index: u32,
    reading_len: u16,
}

#[derive(Debug)]
struct PredictionIndex {
    dictionary: Dictionary<'static>,
    readings: Box<[u8]>,
    entries: Box<[IndexedEntry]>,
}

impl PredictionIndex {
    fn build(dictionary: Dictionary<'static>) -> Result<Self, sakura_core::dictionary::Error> {
        let mut readings = Vec::new();
        let mut entries = Vec::new();
        dictionary.visit_indexed_prediction_entries(|reading, entry_index, _| {
            let Ok(reading_start) = u32::try_from(readings.len()) else {
                return false;
            };
            let Ok(entry_index) = u32::try_from(entry_index) else {
                return false;
            };
            let Ok(reading_len) = u16::try_from(reading.len()) else {
                return false;
            };
            let Some(end) = readings.len().checked_add(reading.len()) else {
                return false;
            };
            if end > u32::MAX as usize {
                return false;
            }
            readings.extend_from_slice(reading.as_bytes());
            entries.push(IndexedEntry {
                reading_start,
                entry_index,
                reading_len,
            });
            true
        })?;
        Ok(Self {
            dictionary,
            readings: readings.into_boxed_slice(),
            entries: entries.into_boxed_slice(),
        })
    }

    fn reading(&self, entry: IndexedEntry) -> Option<&str> {
        let start = entry.reading_start as usize;
        let end = start.checked_add(usize::from(entry.reading_len))?;
        core::str::from_utf8(self.readings.get(start..end)?).ok()
    }

    fn lower_bound(&self, prefix: &[u8]) -> usize {
        let mut low = 0usize;
        let mut high = self.entries.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let reading = self
                .entries
                .get(middle)
                .copied()
                .and_then(|entry| self.reading(entry))
                .map(str::as_bytes)
                .unwrap_or_default();
            if reading < prefix {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }

    fn predict_into(
        &self,
        query: &Query,
        user_dictionary: &UserDictionary,
        history: Option<&LearningService>,
        result: &mut PredictionResult,
    ) {
        result.reset(query.sequence, query.session, query.generation);
        let exact_non_predictive_query_known =
            self.query_has_exact_non_predictive_reading(query.prefix.as_str());
        let mut exact_history_query_known = false;
        if let Some(history) = history {
            let mut accepted = 0usize;
            history.visit_prediction_history(
                query.prefix.as_str(),
                |reading, surface, right_context, _score| {
                    let mut candidate = PredictionCandidate::default();
                    if candidate.reading.push_str(reading).is_ok()
                        && candidate.surface.push_str(surface).is_ok()
                        && candidate.annotation.push_str("履歴").is_ok()
                    {
                        candidate.right_id = right_context;
                        candidate.source = PredictionSource::History;
                        if result.push(candidate) {
                            accepted += 1;
                            exact_history_query_known |= reading == query.prefix.as_str();
                        }
                    }
                    accepted < MAX_HISTORY_SUGGESTIONS_PER_RESULT
                },
            );
        }

        // History is already in the result. Direct candidates are ranked in a
        // separate bounded set and materialized before any repair lookup, so a
        // cheap repaired hit can never evict a candidate matching the typed
        // prefix. `Ranked` deduplicates surfaces while inserting, rather than
        // after the 36-entry scratch window has been truncated.
        let direct_complete = {
            let mut ranked = Ranked::new();
            // Dictionary trie DFS emits readings in scalar-value order, which
            // is also UTF-8 byte order. Binary-search the first possible match,
            // then scan only the contiguous prefix range: O(log N + K), not
            // O(N) on every keystroke as the dictionary grows.
            let prefix = query.prefix.as_str().as_bytes();
            let start = self.lower_bound(prefix);
            for index in start..self.entries.len() {
                let indexed = self.entries[index];
                let Some(reading) = self.reading(indexed) else {
                    continue;
                };
                if !reading.as_bytes().starts_with(prefix) {
                    break;
                }
                let Ok(entry) = self.dictionary.entry_at(indexed.entry_index as usize) else {
                    continue;
                };
                if entry.flags.contains(EntryFlags::SPELLING_CORRECTION)
                    && !query.allow_spelling_correction
                {
                    continue;
                }
                let Some(surface) = self.system_surface(entry) else {
                    continue;
                };
                if result.contains_surface(surface.as_str()) {
                    continue;
                }
                ranked.insert(RankedItem::new(
                    prediction_score(
                        entry.prediction_cost,
                        entry.flags,
                        query.domain_it_per_mille,
                    )
                    .saturating_add(completion_cost(query.prefix.as_str(), reading)),
                    DictionarySource::System,
                    u32::try_from(index).unwrap_or(u32::MAX),
                    surface,
                ));
            }
            user_dictionary.predictive_search(query.prefix.as_str(), |index| {
                if let Some(entry) = user_dictionary.entry(index) {
                    if let Some(surface) = user_surface(entry) {
                        if !result.contains_surface(surface.as_str()) {
                            ranked.insert(RankedItem::new(
                                i64::from(entry.word_cost())
                                    .saturating_sub(USER_DICTIONARY_BONUS)
                                    .saturating_add(completion_cost(
                                        query.prefix.as_str(),
                                        &entry.reading,
                                    )),
                                DictionarySource::User,
                                u32::try_from(index).unwrap_or(u32::MAX),
                                surface,
                            ));
                        }
                    }
                }
                true
            });

            self.materialize_ranked(&ranked, user_dictionary, result);
            result.candidates().len() == MAX_SUGGESTIONS
        };

        if !direct_complete
            && !exact_non_predictive_query_known
            && !exact_history_query_known
            && !query.skip_input_repair
            && query.input_support.is_active()
        {
            let variants = collect_repair_variants(
                query.prefix.as_str(),
                query.input_support,
                MAX_PREDICTION_REPAIR_VARIANTS,
            );
            let mut ranked = Ranked::new();
            for variant in variants.iter() {
                let repaired = variant.repaired.as_str().as_bytes();
                let start = self.lower_bound(repaired);
                for index in start..self.entries.len() {
                    let indexed = self.entries[index];
                    let Some(reading) = self.reading(indexed) else {
                        continue;
                    };
                    if !reading.as_bytes().starts_with(repaired) {
                        break;
                    }
                    if !repair_completion_is_local(
                        query.prefix.as_str(),
                        variant.repaired.as_str(),
                        reading,
                    ) {
                        continue;
                    }
                    let Ok(entry) = self.dictionary.entry_at(indexed.entry_index as usize) else {
                        continue;
                    };
                    if entry.flags.contains(EntryFlags::SPELLING_CORRECTION)
                        && !query.allow_spelling_correction
                    {
                        continue;
                    }
                    let Some(surface) = self.system_surface(entry) else {
                        continue;
                    };
                    if result.contains_surface(surface.as_str()) {
                        continue;
                    }
                    ranked.insert(RankedItem::new(
                        prediction_score(
                            entry.prediction_cost,
                            entry.flags,
                            query.domain_it_per_mille,
                        )
                        .saturating_add(completion_cost(variant.repaired.as_str(), reading))
                        .saturating_add(variant.penalty),
                        DictionarySource::System,
                        u32::try_from(index).unwrap_or(u32::MAX),
                        surface,
                    ));
                }
            }
            self.materialize_ranked(&ranked, user_dictionary, result);
        }
    }

    /// A full-reading entry intentionally excluded from prediction is still
    /// evidence that the typed text is already a real word. In that state,
    /// changing the reading (for example `べ` -> `ぺ`) must not manufacture
    /// unrelated prefix completions. Predictive exact entries retain the
    /// existing direct-then-repair behavior, which is useful for typo support
    /// and already protects the direct candidate tier.
    fn query_has_exact_non_predictive_reading(&self, reading: &str) -> bool {
        let mut system_exact = false;
        let _ = self.dictionary.common_prefix_search(reading, |matched| {
            if matched.matched_bytes == reading.len()
                && !matched
                    .entry
                    .flags
                    .contains(EntryFlags::SPELLING_CORRECTION)
                && !matched.entry.flags.contains(EntryFlags::PREDICTION)
            {
                system_exact = true;
                return false;
            }
            true
        });
        system_exact
    }

    fn system_candidate(&self, index: usize) -> Option<PredictionCandidate> {
        let indexed = *self.entries.get(index)?;
        let entry = self
            .dictionary
            .entry_at(indexed.entry_index as usize)
            .ok()?;
        let reading = self.reading(indexed)?;
        let mut candidate = PredictionCandidate::default();
        candidate.reading.push_str(reading).ok()?;
        self.dictionary
            .write_surface(entry, &mut candidate.surface)
            .ok()?;
        self.dictionary
            .write_annotation(entry, &mut candidate.annotation)
            .ok()?;
        candidate.right_id = entry.right_id;
        candidate.flags = entry.flags;
        candidate.source = PredictionSource::System;
        candidate.system_entry_index = Some(indexed.entry_index);
        Some(candidate)
    }

    fn system_surface(
        &self,
        entry: sakura_core::dictionary::Entry,
    ) -> Option<FixedStr<MAX_PREDICTION_SURFACE_BYTES>> {
        let mut surface = FixedStr::new();
        self.dictionary.write_surface(entry, &mut surface).ok()?;
        Some(surface)
    }

    fn materialize_ranked(
        &self,
        ranked: &Ranked,
        user_dictionary: &UserDictionary,
        result: &mut PredictionResult,
    ) {
        for item in ranked.as_slice() {
            let candidate = match item.source {
                DictionarySource::System => self.system_candidate(item.index as usize),
                DictionarySource::User => user_candidate(user_dictionary, item.index as usize),
            };
            if let Some(candidate) = candidate {
                let _ = result.push(candidate);
                if result.candidates().len() == MAX_SUGGESTIONS {
                    break;
                }
            }
        }
    }
}

fn user_candidate(dictionary: &UserDictionary, index: usize) -> Option<PredictionCandidate> {
    let entry = dictionary.entry(index)?;
    let mut candidate = PredictionCandidate::default();
    candidate.reading.push_str(&entry.reading).ok()?;
    candidate.surface.push_str(&entry.surface).ok()?;
    candidate.annotation.push_str(&entry.comment).ok()?;
    candidate.right_id = entry.right_id();
    candidate.flags = entry.flags();
    candidate.source = PredictionSource::User;
    Some(candidate)
}

fn user_surface(
    entry: &sakura_core::user_dictionary::UserDictionaryEntry,
) -> Option<FixedStr<MAX_PREDICTION_SURFACE_BYTES>> {
    let mut surface = FixedStr::new();
    surface.push_str(&entry.surface).ok()?;
    Some(surface)
}

fn prediction_score(base: i32, flags: EntryFlags, domain_it_per_mille: u16) -> i64 {
    let base = i64::from(base.max(0));
    if !flags.contains(EntryFlags::IT) {
        return base;
    }
    let coherence = domain_it_per_mille / 5;
    let bias = BASE_IT_BIAS_PER_MILLE.saturating_add(coherence.min(150));
    let boost = base
        .saturating_mul(i64::from(bias))
        .checked_div(1_000)
        .unwrap_or(0)
        .min(MAX_IT_BOOST);
    base.saturating_sub(boost)
}

fn completion_cost(prefix: &str, reading: &str) -> i64 {
    if prefix.chars().count() < MIN_PREFIX_CHARS_FOR_COMPLETION_DISTANCE {
        return 0;
    }
    let Some(completion) = reading.strip_prefix(prefix) else {
        return MAX_COMPLETION_COST;
    };
    i64::try_from(completion.chars().count())
        .unwrap_or(i64::MAX)
        .saturating_mul(COMPLETION_COST_PER_CHAR)
        .min(MAX_COMPLETION_COST)
}

fn repair_completion_is_local(typed: &str, repaired: &str, reading: &str) -> bool {
    let Some(completion) = reading.strip_prefix(repaired) else {
        return false;
    };
    let budget = typed
        .chars()
        .count()
        .clamp(MIN_REPAIR_COMPLETION_CHARS, MAX_REPAIR_COMPLETION_CHARS);
    completion.chars().count() <= budget
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
enum DictionarySource {
    #[default]
    User,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RankedItem {
    score: i64,
    source: DictionarySource,
    index: u32,
    surface: FixedStr<MAX_PREDICTION_SURFACE_BYTES>,
}

impl RankedItem {
    fn new(
        score: i64,
        source: DictionarySource,
        index: u32,
        surface: FixedStr<MAX_PREDICTION_SURFACE_BYTES>,
    ) -> Self {
        Self {
            score,
            source,
            index,
            surface,
        }
    }

    fn key(&self) -> (i64, DictionarySource, u32) {
        (self.score, self.source, self.index)
    }
}

#[derive(Debug)]
struct Ranked {
    items: [RankedItem; RANKED_SCRATCH],
    len: usize,
}

impl Ranked {
    fn new() -> Self {
        Self {
            items: std::array::from_fn(|_| RankedItem::default()),
            len: 0,
        }
    }

    fn insert(&mut self, item: RankedItem) {
        if let Some(existing) = self.items[..self.len]
            .iter()
            .position(|current| current.surface.as_str() == item.surface.as_str())
        {
            if self.items[existing].key() <= item.key() {
                return;
            }
            self.items[existing] = item;
            self.bubble_up(existing);
            return;
        }

        let at = self.len.min(self.items.len().saturating_sub(1));
        if self.len < self.items.len() {
            self.len += 1;
        } else if self.items[at].key() <= item.key() {
            return;
        }
        self.items[at] = item;
        self.bubble_up(at);
    }

    fn bubble_up(&mut self, mut at: usize) {
        while at > 0 && self.items[at].key() < self.items[at - 1].key() {
            self.items.swap(at, at - 1);
            at -= 1;
        }
    }

    fn as_slice(&self) -> &[RankedItem] {
        &self.items[..self.len]
    }
}

fn worker(
    mailbox: Arc<Mailbox>,
    index: PredictionIndex,
    conversion: Arc<ConversionService>,
    learning: Option<Arc<LearningService>>,
) {
    let mut result = PredictionResult::default();
    loop {
        let query = {
            let mut state = mailbox
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while state.pending.is_none() && !mailbox.stopping.load(Ordering::Acquire) {
                state = mailbox
                    .pending_changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if mailbox.stopping.load(Ordering::Acquire) {
                mailbox.result_changed.notify_all();
                return;
            }
            state.pending.take()
        };

        let Some(query) = query else {
            continue;
        };
        let user_dictionary = conversion.user_dictionary_snapshot();
        index.predict_into(
            &query,
            user_dictionary.as_ref(),
            learning.as_deref(),
            &mut result,
        );
        let mut state = mailbox
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.result.copy_from_result(&result);
        state.has_result = true;
        mailbox.result_changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_core::{
        ConversionOptions, UserDictionaryEntry, UserPartOfSpeech, MAX_USER_DICTIONARY_ENTRIES,
    };

    #[test]
    fn mapped_entry_index_stays_compact() {
        assert_eq!(core::mem::size_of::<IndexedEntry>(), 12);
    }

    fn conversion() -> Arc<ConversionService> {
        prediction_fixture_conversion(
            "かな\t仮名\t0\t0\t100\t100\tpredict\tcommon\nかんすう\t関数\t0\t0\t200\t50\tit,predict\ttechnical\nかんじ\t感じ\t0\t0\t50\t-\t\tnot predictive\n",
        )
    }

    fn prediction_fixture_conversion(rows: &str) -> Arc<ConversionService> {
        let mut source = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        );
        source.push_str(rows);
        let entries = dictc::parse_entries("fixture.tsv", &source).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        Arc::new(ConversionService::from_static_bytes(bytes).expect("service"))
    }

    fn prediction_query(prefix: &str) -> Query {
        let mut fixed_prefix = FixedStr::new();
        fixed_prefix.push_str(prefix).expect("prediction prefix");
        Query {
            sequence: 1,
            session: 1,
            generation: 1,
            prefix: fixed_prefix,
            domain_it_per_mille: 0,
            input_support: InputSupport::default(),
            skip_input_repair: false,
            allow_spelling_correction: true,
        }
    }

    fn predict_fixture(
        conversion: &Arc<ConversionService>,
        prefix: &str,
        history: Option<&LearningService>,
    ) -> PredictionResult {
        let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
        let user_dictionary = conversion.user_dictionary_snapshot();
        let query = prediction_query(prefix);
        let mut result = PredictionResult::default();
        index.predict_into(&query, user_dictionary.as_ref(), history, &mut result);
        result
    }

    fn capacity_user_dictionary(prefix: &str) -> UserDictionary {
        const HIRAGANA_DIGITS: [char; 10] =
            ['あ', 'い', 'う', 'え', 'お', 'か', 'き', 'く', 'け', 'こ'];

        let entries = (0..MAX_USER_DICTIONARY_ENTRIES)
            .map(|number| {
                let mut reading = prefix.to_owned();
                let mut value = number;
                for _ in 0..4 {
                    reading.push(HIRAGANA_DIGITS[value % HIRAGANA_DIGITS.len()]);
                    value /= HIRAGANA_DIGITS.len();
                }
                UserDictionaryEntry {
                    reading,
                    surface: if number < 2 {
                        "shared".to_owned()
                    } else {
                        format!("user-{number:04}")
                    },
                    part_of_speech: UserPartOfSpeech::Noun,
                    comment: String::new(),
                }
            })
            .collect();
        UserDictionary::from_entries(entries).expect("capacity user dictionary")
    }

    fn benchmark_user_dictionary(size: usize, matching_entries: usize) -> UserDictionary {
        const HIRAGANA_DIGITS: [char; 10] =
            ['あ', 'い', 'う', 'え', 'お', 'か', 'き', 'く', 'け', 'こ'];

        assert!(matching_entries <= size);
        let entries = (0..size)
            .map(|number| {
                let (prefix, mut value) = if number < matching_entries {
                    ("さ", number)
                } else {
                    ("あ", number - matching_entries)
                };
                let mut reading = prefix.to_owned();
                for _ in 0..4 {
                    reading.push(HIRAGANA_DIGITS[value % HIRAGANA_DIGITS.len()]);
                    value /= HIRAGANA_DIGITS.len();
                }
                UserDictionaryEntry {
                    reading,
                    surface: format!("bench-{number:05}"),
                    part_of_speech: UserPartOfSpeech::Noun,
                    comment: String::new(),
                }
            })
            .collect();
        UserDictionary::from_entries(entries).expect("benchmark user dictionary")
    }

    fn nanos_each(mut body: impl FnMut()) -> f64 {
        const ROUNDS: usize = 1_000;

        for _ in 0..ROUNDS / 10 {
            body();
        }
        let started = Instant::now();
        for _ in 0..ROUNDS {
            body();
        }
        started.elapsed().as_secs_f64() * 1e9 / ROUNDS as f64
    }

    fn sample_nanos(mut body: impl FnMut()) -> Vec<u128> {
        const WARMUP: usize = 200;
        const SAMPLES: usize = 2_000;

        for _ in 0..WARMUP {
            body();
        }
        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = Instant::now();
            body();
            samples.push(started.elapsed().as_nanos());
        }
        samples
    }

    fn percentile(samples: &mut [u128], percentile: usize) -> u128 {
        assert!(!samples.is_empty());
        assert!(percentile <= 100);
        samples.sort_unstable();
        let index = ((samples.len() - 1) * percentile).div_ceil(100);
        samples[index]
    }

    #[test]
    #[ignore = "timing evaluation: run with --release --ignored --nocapture and record the table"]
    fn prediction_latency_percentiles() {
        const HIRAGANA_DIGITS: [char; 10] = [
            '\u{3042}', '\u{3044}', '\u{3046}', '\u{3048}', '\u{304A}', '\u{304B}', '\u{304D}',
            '\u{304F}', '\u{3051}', '\u{3053}',
        ];
        let conversion = conversion();
        let entries = (0..1_000)
            .map(|number| {
                let mut value = number;
                let mut reading = String::from('\u{3055}');
                for _ in 0..4 {
                    reading.push(HIRAGANA_DIGITS[value % HIRAGANA_DIGITS.len()]);
                    value /= HIRAGANA_DIGITS.len();
                }
                UserDictionaryEntry {
                    reading,
                    surface: format!("candidate-{number:04}"),
                    part_of_speech: UserPartOfSpeech::Noun,
                    comment: String::new(),
                }
            })
            .collect();
        let user_dictionary = UserDictionary::from_entries(entries).expect("user dictionary");
        conversion.replace_user_dictionary(user_dictionary);
        let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
        let mut prefix = FixedStr::new();
        prefix.push_str("\u{3055}").expect("benchmark prefix");
        let query = Query {
            sequence: 1,
            session: 1,
            generation: 1,
            prefix,
            domain_it_per_mille: 1_000,
            input_support: InputSupport::default(),
            skip_input_repair: false,
            allow_spelling_correction: true,
        };
        let user_dictionary = conversion.user_dictionary_snapshot();

        let mut ranked = PredictionResult::default();
        let mut ranking = sample_nanos(|| {
            index.predict_into(&query, user_dictionary.as_ref(), None, &mut ranked);
            std::hint::black_box(ranked.candidates().len());
        });

        let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
        let service = runtime.service();
        let mut worker_result = PredictionResult::default();
        let mut generation = 1u64;
        let mut worker = sample_nanos(|| {
            generation += 1;
            assert!(service.request_into(
                1,
                generation,
                query.prefix.as_str(),
                query.domain_it_per_mille,
                sakura_core::InputSupport::default(),
                false,
                Duration::from_secs(1),
                &mut worker_result,
            ));
            std::hint::black_box(worker_result.candidates().len());
        });
        runtime.stop().expect("joined worker");

        println!("prediction latency percentiles: 2,000 samples, release");
        println!("path       p50 ns  p95 ns  p99 ns  max ns");
        for (name, samples) in [("ranking", &mut ranking), ("worker", &mut worker)] {
            println!(
                "{name:8} {:>7} {:>7} {:>7} {:>7}",
                percentile(samples, 50),
                percentile(samples, 95),
                percentile(samples, 99),
                percentile(samples, 100),
            );
        }
    }

    #[test]
    fn persistent_worker_merges_system_and_user_predictions_and_joins() {
        let conversion = conversion();
        conversion.replace_user_dictionary(
            UserDictionary::parse_tsv("reading\tsurface\tpos\tcomment\nかなた\t彼方\tnoun\tuser\n")
                .expect("user dictionary"),
        );
        let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
        let service = runtime.service();

        let result = service
            .request(
                7,
                3,
                "かな",
                1_000,
                sakura_core::InputSupport::default(),
                false,
                Duration::from_millis(100),
            )
            .expect("result");

        assert_eq!(result.session(), 7);
        assert_eq!(result.generation(), 3);
        assert_eq!(
            result
                .candidates()
                .iter()
                .map(PredictionCandidate::surface)
                .collect::<Vec<_>>(),
            ["彼方", "仮名"]
        );
        runtime.stop().expect("joined worker");
    }

    #[test]
    fn capacity_user_dictionary_preserves_prediction_order_deduplication_and_limit() {
        let conversion = conversion();
        let user_dictionary = capacity_user_dictionary("か");
        let mut expected = Vec::new();
        for entry in user_dictionary.entries() {
            if !expected.iter().any(|surface| surface == &entry.surface) {
                expected.push(entry.surface.clone());
            }
            if expected.len() == MAX_SUGGESTIONS {
                break;
            }
        }
        conversion.replace_user_dictionary(user_dictionary);

        let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
        let result = runtime
            .service()
            .request(
                8,
                5,
                "か",
                1_000,
                sakura_core::InputSupport::default(),
                false,
                Duration::from_secs(1),
            )
            .expect("capacity prediction result");

        assert_eq!(result.candidates().len(), MAX_SUGGESTIONS);
        assert!(result
            .candidates()
            .iter()
            .all(|candidate| candidate.source() == PredictionSource::User));
        assert_eq!(
            result
                .candidates()
                .iter()
                .map(|candidate| candidate.surface())
                .collect::<Vec<_>>(),
            expected
        );
        runtime.stop().expect("joined worker");
    }

    #[test]
    #[ignore = "timing evaluation: run with --release --ignored --nocapture and record the table"]
    fn user_dictionary_prediction_evaluation() {
        const SIZES: [usize; 4] = [0, 100, 1_000, MAX_USER_DICTIONARY_ENTRIES];
        const QUERIES: [(&str, usize); 5] = [
            ("no-match", 0),
            ("one-match", 1),
            ("nine-match", 9),
            ("hundred-match", 100),
            ("ten-thousand-match", MAX_USER_DICTIONARY_ENTRIES),
        ];

        let conversion = conversion();
        let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
        let mut query_prefix = FixedStr::new();
        query_prefix.push_str("さ").expect("benchmark prefix");
        let query = Query {
            sequence: 1,
            session: 1,
            generation: 1,
            prefix: query_prefix,
            domain_it_per_mille: 1_000,
            input_support: InputSupport::default(),
            skip_input_repair: false,
            allow_spelling_correction: true,
        };

        println!("user dictionary prediction evaluation: 1,000 rounds per row");
        println!("entries  query                matches  search ns  ranking ns  worker ns");
        for size in SIZES {
            for (name, matching_entries) in QUERIES {
                if matching_entries > size {
                    continue;
                }
                let user_dictionary = benchmark_user_dictionary(size, matching_entries);
                let search_ns = nanos_each(|| {
                    let mut visited = 0usize;
                    user_dictionary.predictive_search("さ", |_| {
                        visited += 1;
                        true
                    });
                    std::hint::black_box(visited);
                });

                let mut ranked = PredictionResult::default();
                let ranking_ns = nanos_each(|| {
                    index.predict_into(&query, &user_dictionary, None, &mut ranked);
                    std::hint::black_box(ranked.candidates().len());
                });

                conversion.replace_user_dictionary(user_dictionary);
                let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
                let service = runtime.service();
                let mut worker_result = PredictionResult::default();
                let mut generation = 1u64;
                let worker_ns = nanos_each(|| {
                    generation += 1;
                    assert!(service.request_into(
                        1,
                        generation,
                        "さ",
                        1_000,
                        sakura_core::InputSupport::default(),
                        false,
                        Duration::from_secs(1),
                        &mut worker_result,
                    ));
                    std::hint::black_box(worker_result.candidates().len());
                });
                runtime.stop().expect("joined worker");

                println!(
                    "{size:7}  {name:19}  {matching_entries:7}  {search_ns:9.1}  {ranking_ns:10.1}  {worker_ns:9.1}"
                );
            }
        }
    }

    #[test]
    fn learned_history_precedes_dictionary_results_and_deduplicates_surfaces() {
        let conversion = conversion();
        let learning = Arc::new(LearningService::memory());
        learning.learn("かなた", "彼方", 0, 7);
        learning.learn("かな", "仮名", 0, 3);
        learning.learn("かな", "仮名", 0, 3);
        let runtime =
            PredictionRuntime::start_with_learning(Arc::clone(&conversion), Arc::clone(&learning))
                .expect("runtime");

        let result = runtime
            .service()
            .request(
                9,
                4,
                "か",
                1_000,
                sakura_core::InputSupport::default(),
                false,
                Duration::from_millis(100),
            )
            .expect("result");

        assert_eq!(result.candidates()[0].surface(), "仮名");
        assert_eq!(result.candidates()[0].annotation(), "履歴");
        assert_eq!(
            result
                .candidates()
                .iter()
                .filter(|candidate| candidate.surface() == "仮名")
                .count(),
            1
        );
        assert!(result
            .candidates()
            .iter()
            .any(|candidate| candidate.surface() == "関数"));
        runtime.stop().expect("joined worker");
    }

    #[test]
    fn direct_candidates_are_not_evicted_by_a_cheaper_advanced_repair() {
        let conversion = prediction_fixture_conversion(
            "かが\t直接候補\t0\t0\t5000\t5000\tpredict\tdirect\nいが\t高度補正\t0\t0\t1\t1000\tpredict\tadvanced\n",
        );
        let result = predict_fixture(&conversion, "かが", None);

        assert_eq!(result.candidates()[0].surface(), "直接候補");
        assert_eq!(result.candidates()[0].reading(), "かが");
        assert!(result
            .candidates()
            .iter()
            .any(|candidate| candidate.surface() == "高度補正"));
    }

    #[test]
    fn word_sized_exact_reading_outranks_a_cheaper_longer_completion() {
        let conversion = prediction_fixture_conversion(
            "おらくる\tオラクル\t0\t0\t5000\t5000\tpredict\texact\n\
おらくるさぽーと\tOracleサポート\t0\t0\t4000\t4000\tpredict\tcompletion\n",
        );

        let result = predict_fixture(&conversion, "おらくる", None);

        assert_eq!(result.candidates()[0].surface(), "オラクル");
        assert!(result
            .candidates()
            .iter()
            .any(|candidate| candidate.surface() == "Oracleサポート"));
    }

    #[test]
    fn a_known_non_predictive_word_does_not_trigger_reading_repairs() {
        let conversion = prediction_fixture_conversion(
            "すべき\tすべき\t0\t0\t7000\t-\t\tknown word\n\
すぺきゅれーしょんるーるず\t投機的ルール\t0\t0\t100\t100\tpredict\trepair trap\n",
        );

        let result = predict_fixture(&conversion, "すべき", None);

        assert!(
            result.candidates().is_empty(),
            "known exact word exposed repaired predictions: {:?}",
            result
                .candidates()
                .iter()
                .map(|candidate| (candidate.reading(), candidate.surface()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_repair_only_unlocks_local_completions() {
        let conversion = prediction_fixture_conversion(
            "すぺきあ\t近い補正\t0\t0\t200\t200\tpredict\tlocal repair\n\
すぺきゅれーしょんるーるず\t投機的ルール\t0\t0\t100\t100\tpredict\tdistant repair trap\n",
        );

        let result = predict_fixture(&conversion, "すべき", None);
        let surfaces = result
            .candidates()
            .iter()
            .map(PredictionCandidate::surface)
            .collect::<Vec<_>>();

        assert!(
            surfaces.contains(&"近い補正"),
            "local repair was lost: {surfaces:?}"
        );
        assert!(
            !surfaces.contains(&"投機的ルール"),
            "a repair unlocked a disproportionate completion: {surfaces:?}"
        );
    }

    #[test]
    fn an_exact_history_word_does_not_trigger_reading_repairs() {
        let conversion = prediction_fixture_conversion(
            "すぺきあ\t近い補正\t0\t0\t200\t200\tpredict\trepair trap\n",
        );
        let learning = LearningService::memory();
        learning.learn("すべき", "すべき", 0, 1);

        let result = predict_fixture(&conversion, "すべき", Some(&learning));
        assert_eq!(result.candidates()[0].surface(), "すべき");
        assert!(
            result
                .candidates()
                .iter()
                .all(|candidate| candidate.reading().starts_with("すべき")),
            "an exact history word exposed repaired predictions: {:?}",
            result
                .candidates()
                .iter()
                .map(|candidate| (candidate.reading(), candidate.surface()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn completion_distance_is_inactive_for_one_character_prefixes() {
        assert_eq!(completion_cost("か", "か"), 0);
        assert_eq!(completion_cost("か", "かなた"), 0);
    }

    #[test]
    fn decimal_counter_predictions_never_lookup_kana_repair_prefixes() {
        let conversion = prediction_fixture_conversion(
            "あかい\t赤い\t0\t0\t1\t1\tpredict\trepair trap\n\
あかい\t赤井\t0\t0\t2\t2\tpredict\trepair trap\n\
あかい\t紅い\t0\t0\t3\t3\tpredict\trepair trap\n",
        );
        let learning = LearningService::memory();
        learning.learn("2かいひょう", "2回表", 0, 2);
        learning.learn("5かい", "5回", 0, 5);

        for (prefix, expected_history) in [("2かい", "2回表"), ("5かい", "5回")] {
            let result = predict_fixture(&conversion, prefix, Some(&learning));
            let candidates = result.candidates();
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.surface() == expected_history),
                "{prefix} lost its history candidate: {:?}",
                candidates
                    .iter()
                    .map(PredictionCandidate::surface)
                    .collect::<Vec<_>>()
            );
            assert!(
                candidates
                    .iter()
                    .all(|candidate| candidate.reading().starts_with(prefix)),
                "{prefix} looked up an unrelated kana repair: {:?}",
                candidates
                    .iter()
                    .map(|candidate| (candidate.reading(), candidate.surface()))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn repair_ranking_uses_each_variant_penalty() {
        let conversion = prediction_fixture_conversion(
            "かが\t直接候補\t0\t0\t5000\t5000\tpredict\tdirect\nかか\t規則補正\t0\t0\t1\t3000\tpredict\trule\nいが\t高度補正\t0\t0\t1\t2500\tpredict\tadvanced\n",
        );
        let result = predict_fixture(&conversion, "かが", None);
        let surfaces: Vec<&str> = result
            .candidates()
            .iter()
            .map(PredictionCandidate::surface)
            .collect();

        assert_eq!(&surfaces[..3], ["直接候補", "規則補正", "高度補正"]);
    }

    #[test]
    fn direct_system_provenance_wins_a_repair_surface_collision() {
        let conversion = prediction_fixture_conversion(
            "かが\t共有表面\t0\t0\t5000\t5000\tpredict\tdirect\nいが\t共有表面\t0\t0\t1\t1\tpredict\tadvanced\n",
        );
        let result = predict_fixture(&conversion, "かが", None);

        assert_eq!(
            result
                .candidates()
                .iter()
                .filter(|candidate| candidate.surface() == "共有表面")
                .count(),
            1
        );
        let shared = result
            .candidates()
            .iter()
            .find(|candidate| candidate.surface() == "共有表面")
            .expect("shared candidate");
        assert_eq!(shared.reading(), "かが");
        assert_eq!(shared.source(), PredictionSource::System);
    }

    #[test]
    fn direct_user_provenance_wins_over_a_system_repair() {
        let conversion =
            prediction_fixture_conversion("いが\tシステム補正\t0\t0\t1\t1000\tpredict\tadvanced\n");
        conversion.replace_user_dictionary(
            UserDictionary::parse_tsv(
                "reading\tsurface\tpos\tcomment\nかが\tユーザー直接\tnoun\tdirect\n",
            )
            .expect("user dictionary"),
        );
        let result = predict_fixture(&conversion, "かが", None);

        assert_eq!(result.candidates()[0].surface(), "ユーザー直接");
        assert_eq!(result.candidates()[0].source(), PredictionSource::User);
    }

    #[test]
    fn input_repair_never_looks_up_user_dictionary_entries() {
        let conversion = prediction_fixture_conversion("");
        conversion.replace_user_dictionary(
            UserDictionary::parse_tsv(
                "reading\tsurface\tpos\tcomment\nいが\tユーザー補正\tnoun\trepair-only\n",
            )
            .expect("user dictionary"),
        );

        let result = predict_fixture(&conversion, "かが", None);

        assert!(result
            .candidates()
            .iter()
            .all(|candidate| candidate.surface() != "ユーザー補正"));
    }

    #[test]
    fn direct_unique_surfaces_survive_more_than_the_ranked_scratch_window() {
        const KANA: [char; 10] = ['あ', 'い', 'う', 'え', 'お', 'か', 'き', 'く', 'け', 'こ'];
        let mut rows = String::new();
        let mut reading_index = 0usize;
        for first in KANA {
            for second in KANA {
                if reading_index >= 36 {
                    break;
                }
                rows.push_str(&format!(
                    "か{first}{second}\t共有表面\t0\t0\t{}\t{}\tpredict\tduplicate\n",
                    reading_index + 1,
                    reading_index + 1
                ));
                reading_index += 1;
            }
            if reading_index >= 36 {
                break;
            }
        }
        for unique in 0..8 {
            rows.push_str(&format!(
                "かさ{unique}\t直接候補{unique}\t0\t0\t{}\t{}\tpredict\tdirect\n",
                100 + unique,
                100 + unique
            ));
        }

        let conversion = prediction_fixture_conversion(&rows);
        let result = predict_fixture(&conversion, "か", None);

        assert_eq!(result.candidates().len(), MAX_SUGGESTIONS);
        assert_eq!(
            result
                .candidates()
                .iter()
                .filter(|candidate| candidate.surface() == "共有表面")
                .count(),
            1
        );
        for unique in 0..8 {
            let surface = format!("直接候補{unique}");
            assert!(
                result
                    .candidates()
                    .iter()
                    .any(|candidate| candidate.surface() == surface),
                "{surface} was lost: {:?}",
                result
                    .candidates()
                    .iter()
                    .map(PredictionCandidate::surface)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn history_four_plus_direct_five_fills_without_repair_candidates() {
        let conversion = prediction_fixture_conversion(
            "かあ\t直接0\t0\t0\t100\t100\tpredict\tdirect\nかい\t直接1\t0\t0\t101\t101\tpredict\tdirect\nかう\t直接2\t0\t0\t102\t102\tpredict\tdirect\nかえ\t直接3\t0\t0\t103\t103\tpredict\tdirect\nかお\t直接4\t0\t0\t104\t104\tpredict\tdirect\n",
        );
        let learning = LearningService::memory();
        for index in 0..4 {
            learning.learn(
                &format!("か履歴{index}"),
                &format!("履歴{index}"),
                0,
                100 - index,
            );
        }

        let result = predict_fixture(&conversion, "か", Some(&learning));

        assert_eq!(result.candidates().len(), MAX_SUGGESTIONS);
        assert_eq!(
            result
                .candidates()
                .iter()
                .filter(|candidate| candidate.source() == PredictionSource::History)
                .count(),
            4
        );
        assert_eq!(
            result
                .candidates()
                .iter()
                .filter(|candidate| candidate.surface().starts_with("直接"))
                .count(),
            5
        );
        assert!(result
            .candidates()
            .iter()
            .all(|candidate| !candidate.surface().contains("補正")));
    }

    #[test]
    fn history_four_plus_direct_four_leaves_one_repair_slot() {
        let conversion = prediction_fixture_conversion(
            "かが\t直接0\t0\t0\t100\t100\tpredict\tdirect\nかがあ\t直接1\t0\t0\t101\t101\tpredict\tdirect\nかがい\t直接2\t0\t0\t102\t102\tpredict\tdirect\nかがう\t直接3\t0\t0\t103\t103\tpredict\tdirect\nかか\t規則補正\t0\t0\t1000\t1000\tpredict\trule\nいが\t高度補正\t0\t0\t1000\t1000\tpredict\tadvanced\n",
        );
        let learning = LearningService::memory();
        for index in 0..4 {
            learning.learn(
                &format!("かが履歴{index}"),
                &format!("履歴{index}"),
                0,
                100 - index,
            );
        }

        let result = predict_fixture(&conversion, "かが", Some(&learning));

        assert_eq!(result.candidates().len(), MAX_SUGGESTIONS);
        assert_eq!(
            result
                .candidates()
                .iter()
                .filter(|candidate| candidate.source() == PredictionSource::History)
                .count(),
            4
        );
        assert_eq!(
            result
                .candidates()
                .iter()
                .filter(|candidate| candidate.surface().starts_with("直接"))
                .count(),
            4
        );
        assert_eq!(
            result
                .candidates()
                .iter()
                .filter(|candidate| candidate.surface().contains("補正"))
                .count(),
            1
        );
    }

    fn ranked_surface(surface: &str) -> FixedStr<MAX_PREDICTION_SURFACE_BYTES> {
        let mut fixed = FixedStr::new();
        fixed.push_str(surface).expect("ranked surface");
        fixed
    }

    #[test]
    fn ranked_surface_invariants_hold_for_bounded_exhaustive_sequences() {
        const SURFACES: [&str; 3] = ["surface-a", "surface-b", "surface-c"];
        const STEPS: usize = 7;
        let cases = 3usize.pow(STEPS as u32);

        for mut encoded in 0..cases {
            let mut ranked = Ranked::new();
            let mut best: [Option<(i64, DictionarySource, u32)>; SURFACES.len()] =
                [None; SURFACES.len()];
            for step in 0..STEPS {
                let surface_index = encoded % SURFACES.len();
                encoded /= SURFACES.len();
                let source = if (step + surface_index).is_multiple_of(2) {
                    DictionarySource::System
                } else {
                    DictionarySource::User
                };
                let score = ((step * 7 + surface_index * 3) % 11) as i64;
                let index = u32::try_from(step).expect("step");
                let key = (score, source, index);
                if best[surface_index].is_none_or(|current| key < current) {
                    best[surface_index] = Some(key);
                }
                ranked.insert(RankedItem::new(
                    score,
                    source,
                    index,
                    ranked_surface(SURFACES[surface_index]),
                ));
            }

            assert!(ranked
                .as_slice()
                .windows(2)
                .all(|items| items[0].key() <= items[1].key()));
            assert!(ranked
                .as_slice()
                .windows(2)
                .all(|items| items[0].surface.as_str() != items[1].surface.as_str()));
            for item in ranked.as_slice() {
                let surface_index = SURFACES
                    .iter()
                    .position(|surface| *surface == item.surface.as_str())
                    .expect("known surface");
                assert_eq!(item.key(), best[surface_index].expect("best item"));
            }
        }
    }

    #[test]
    fn prediction_limits_displayed_history_candidates_without_trimming_retention() {
        let conversion = conversion();
        let learning = Arc::new(LearningService::memory());
        for index in 0..(MAX_HISTORY_SUGGESTIONS_PER_RESULT + 2) {
            learning.learn(
                &format!("history-{index}"),
                &format!("history-surface-{index}"),
                0,
                0,
            );
        }

        let mut retained = Vec::new();
        learning.visit_prediction_history("history-", |reading, surface, _, _| {
            retained.push((reading.to_owned(), surface.to_owned()));
            true
        });
        assert_eq!(
            retained.len(),
            MAX_HISTORY_SUGGESTIONS_PER_RESULT + 2,
            "the retained history window remains broader than one displayed result"
        );

        let runtime =
            PredictionRuntime::start_with_learning(Arc::clone(&conversion), Arc::clone(&learning))
                .expect("runtime");
        let result = runtime
            .service()
            .request(
                1,
                1,
                "history-",
                0,
                sakura_core::InputSupport::default(),
                false,
                Duration::from_millis(100),
            )
            .expect("prediction");

        assert_eq!(
            result
                .candidates()
                .iter()
                .filter(|candidate| candidate.source() == PredictionSource::History)
                .count(),
            MAX_HISTORY_SUGGESTIONS_PER_RESULT
        );
        assert_eq!(
            result.candidates().len(),
            MAX_HISTORY_SUGGESTIONS_PER_RESULT,
            "no dictionary fixture entry matches the dedicated history prefix"
        );
        runtime.stop().expect("joined worker");
    }

    #[test]
    fn spelling_correction_follows_the_unified_prediction_gate() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nあい\t藍\t0\t0\t10\t1\tcorrection,predict\t\nあい\t愛\t0\t0\t100\t50\tpredict\t\nあいう\t愛う\t0\t0\t90\t40\tpredict\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let conversion = Arc::new(ConversionService::from_static_bytes(bytes).expect("service"));
        let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
        let user = conversion.user_dictionary_snapshot();

        let mut prefix = FixedStr::new();
        prefix.push_str("あい").expect("prefix");

        let open = Query {
            sequence: 1,
            session: 1,
            generation: 1,
            prefix: prefix.clone(),
            domain_it_per_mille: 0,
            input_support: InputSupport::default(),
            skip_input_repair: false,
            allow_spelling_correction: true,
        };
        let mut ranked = PredictionResult::default();
        index.predict_into(&open, user.as_ref(), None, &mut ranked);
        assert!(
            ranked
                .candidates()
                .iter()
                .any(|candidate| candidate.surface() == "藍"),
            "positive control: SPELLING_CORRECTION must predict when the gate is open"
        );

        for (label, support, skip, allow) in [
            (
                "master-off",
                InputSupport {
                    enabled: false,
                    ..InputSupport::default()
                },
                false,
                false,
            ),
            ("skip", InputSupport::default(), true, false),
            (
                "fuzzy-off",
                InputSupport {
                    fuzzy_proper_nouns: false,
                    ..InputSupport::default()
                },
                false,
                false,
            ),
        ] {
            assert_eq!(
                support.allows_spelling_correction(skip),
                allow,
                "{label} admission snapshot"
            );
            let query = Query {
                sequence: 1,
                session: 1,
                generation: 1,
                prefix: prefix.clone(),
                domain_it_per_mille: 0,
                input_support: support,
                skip_input_repair: skip,
                allow_spelling_correction: allow,
            };
            let mut ranked = PredictionResult::default();
            index.predict_into(&query, user.as_ref(), None, &mut ranked);
            assert!(
                ranked
                    .candidates()
                    .iter()
                    .all(|candidate| candidate.surface() != "藍"),
                "{label}: SPELLING_CORRECTION must stay out of the main prediction path"
            );
            assert!(
                ranked
                    .candidates()
                    .iter()
                    .any(|candidate| candidate.surface() == "愛"),
                "{label}: normal predictive entries must remain"
            );
        }
    }

    #[test]
    fn gated_spelling_correction_does_not_pollute_prediction_budget() {
        let mut tsv = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        );
        for index in 0..16 {
            tsv.push_str(&format!(
                "あい\t藍{index}\t0\t0\t1\t1\tcorrection,predict\t\n"
            ));
        }
        for index in 0..9 {
            tsv.push_str(&format!(
                "あい\t愛{index}\t0\t0\t100\t{}\tpredict\t\n",
                10 + index
            ));
        }
        let entries = dictc::parse_entries("fixture.tsv", &tsv).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let conversion = Arc::new(ConversionService::from_static_bytes(bytes).expect("service"));
        let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
        let user = conversion.user_dictionary_snapshot();
        let mut prefix = FixedStr::new();
        prefix.push_str("あい").expect("prefix");
        let query = Query {
            sequence: 1,
            session: 1,
            generation: 1,
            prefix,
            domain_it_per_mille: 0,
            input_support: InputSupport::default(),
            skip_input_repair: true,
            allow_spelling_correction: false,
        };
        let mut ranked = PredictionResult::default();
        index.predict_into(&query, user.as_ref(), None, &mut ranked);
        let surfaces: Vec<&str> = ranked
            .candidates()
            .iter()
            .map(PredictionCandidate::surface)
            .collect();
        assert!(
            surfaces.iter().all(|surface| !surface.starts_with('藍')),
            "gated SPELLING_CORRECTION must not occupy prediction slots: {surfaces:?}"
        );
        assert!(
            surfaces.iter().any(|surface| surface.starts_with('愛')),
            "ordinary predictive entries must fill the budget: {surfaces:?}"
        );
        assert_eq!(surfaces.len(), MAX_SUGGESTIONS);
    }

    #[test]
    fn the_single_pending_slot_coalesces_to_the_newest_query() {
        let mailbox = Mailbox::new();
        let first = mailbox
            .publish(1, 1, "か", 0, InputSupport::default(), false)
            .expect("first");
        let second = mailbox
            .publish(2, 2, "かな", 0, InputSupport::default(), false)
            .expect("second");
        assert!(second > first);
        assert_eq!(mailbox.coalesced.load(Ordering::Relaxed), 1);
        let state = mailbox.state.lock().expect("mailbox");
        let pending = state.pending.as_ref().expect("newest pending");
        assert_eq!(pending.session, 2);
        assert_eq!(pending.prefix.as_str(), "かな");
    }

    #[test]
    fn prediction_reads_do_not_consume_a_conversion_arena() {
        let conversion = conversion();
        let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
        let _ = runtime
            .service()
            .request(
                1,
                1,
                "か",
                0,
                sakura_core::InputSupport::default(),
                false,
                Duration::from_millis(100),
            )
            .expect("prediction");
        let converted = conversion
            .with_candidates("かな", ConversionOptions::default(), |candidates| {
                candidates
                    .first()
                    .map(|candidate| candidate.text().to_owned())
            })
            .expect("conversion slot");
        assert_eq!(converted.as_deref(), Some("仮名"));
        runtime.stop().expect("joined worker");
    }
}
