//! Bounded lattice conversion with Viterbi top-1 and A* N-best search.
//!
//! The dictionary stays borrowed and mapped. All arenas are allocated once by
//! [`Converter::new`] and cleared between queries, so steady-state conversion
//! does not grow the heap. Every search is finite: lattice nodes, A* states,
//! candidates, and text are independently bounded.

use std::collections::BinaryHeap;

#[cfg(not(any(feature = "research-top32", feature = "research-wide-candidates")))]
use sakura_values::MAX_CANDIDATES;
use sakura_values::{FixedStr, FixedVec, MAX_PREEDIT_BYTES};

use crate::calendar::CivilDate;
use crate::dictionary::{Dictionary, Entry, EntryFlags};
use crate::input_repair::RepairKind;
use crate::numerals::{
    parse_numeric_prefix, should_emit_numeric_span, NumericCounter, NumericSpan, NUMERIC_STYLES,
};
use crate::user_dictionary::UserDictionary;
use crate::TextSink;

mod bridge;
mod candidates;
mod evidence;
mod input;
mod options;
mod ranking;
mod repair_plan;
mod result;
mod search;
mod synthesis;

pub(in crate::conversion) use bridge::{
    BridgeBoundaryKind, CommitBridgeTailStorage, NO_COMMIT_BRIDGE_ENTRY, NO_SYSTEM_ENTRY_INDEX,
};
pub use bridge::{CommitBridgeTail, CrossCommitBridge, LeftContextId, RightContextId};
pub use candidates::{ConversionCandidate, ConversionSegment};
pub use evidence::{
    CandidateAuthority, CandidateEvidence, CandidateEvidenceClass, CandidateOrigin, PathEvidence,
    RepairTier,
};
pub use input::{ConversionInput, ConversionInputClass, LiteralPolicy};
pub use options::{candidate_budget, ConversionOptions};
pub use repair_plan::{
    CorrectionMap, CorrectionMapError, CorrectionRun, CorrectionRunKind, RawRepairBudget,
    RawRepairPlan,
};
pub use result::{
    ConversionDiagnostics, ConversionError, ConversionResult, ConversionSearchTerminal,
};
pub(in crate::conversion) use search::{
    char_class, CharClass, DictionaryEdgeBudget, HeapItem, Node, NodeSpec, SearchRun, SearchState,
};

const NONE: usize = usize::MAX;
const NONE_STATE: u32 = u32::MAX;
const MAX_LATTICE_NODES: usize = 32_768;
const MAX_SEARCH_STATES: usize = 65_536;
/// Preserve the historical twelve cheapest system edges for each exact
/// reading span. Additional edges may only add a surface those baseline rows
/// did not expose, so POS variants keep their old paths without consuming the
/// whole candidate vocabulary.
const BASE_DICTIONARY_EDGES_PER_READING: usize = 12;
/// A whole-reading span may expose as many distinct surfaces as the protocol
/// can carry. The former twelve mirrored the build-time trim cap and hid
/// affordable homophones the shipped dictionary already held: きかん stopped at
/// き澗 without ever reaching 気管 or 旗艦, and きゅう spent its twelfth surface
/// on the rare name kanji 邱 and dropped the digit spelling 9 (Issue #94).
/// The budget tracks the conversion candidate limit rather than the wire
/// constant: a research build that raises the limit must widen the dictionary
/// span with it, or the extra slots fill with multi-morpheme paths instead of
/// the homophones the sweep is measuring (Issue #95).
const MAX_DICTIONARY_SURFACES_PER_READING: usize = MAX_CONVERSION_CANDIDATES;
/// Cross-commit context is deliberately word-sized. It exists to recover a
/// lexical edge split by an explicit commit, not to replay an unbounded
/// document prefix on every Space press.
pub const MAX_CROSS_COMMIT_TAIL_BYTES: usize = 48;
pub const MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES: usize = 96;
pub const MAX_CROSS_COMMIT_CURRENT_BYTES: usize = 96;
/// A one-character tail is usually a particle and supplies too little
/// lexical evidence to justify replaying text across an explicit commit.
pub const MIN_CROSS_COMMIT_TAIL_CHARS: usize = 2;
const MAX_CROSS_COMMIT_LATTICE_NODES: usize = 4_096;
const MAX_CROSS_COMMIT_SEARCH_STATES: usize = 8_192;
#[cfg(not(any(feature = "research-top32", feature = "research-wide-candidates")))]
pub const MAX_CONVERSION_CANDIDATES: usize = MAX_CANDIDATES;
#[cfg(all(feature = "research-top32", not(feature = "research-wide-candidates")))]
pub const MAX_CONVERSION_CANDIDATES: usize = 32;
/// Isolated sweep bound for Issue #95. It exists to measure how conversion
/// latency and homophone coverage scale with the candidate limit before the
/// shipping bound moves. Shipping targets never enable this feature.
#[cfg(feature = "research-wide-candidates")]
pub const MAX_CONVERSION_CANDIDATES: usize = 512;
const GENERATED_DATE_VARIANTS: usize = 4;
const GENERATED_VARIANT_SLACK: usize = GENERATED_DATE_VARIANTS;
const FALLBACK_WORD_COST: i64 = 8_000;
const RUN_BASE_COST: i64 = 6_000;
const RUN_COST_PER_CHAR: i64 = 2_500;
const KATAKANA_BASE_COST: i64 = 7_000;
const KATAKANA_COST_PER_CHAR: i64 = 2_800;
const COUNTER_WORD_COST: i64 = 3_500;
// Explicit decimal input and counter/calendar forms are authoritative.
const NUMBER_FORM_COST: i64 = 800;
/// A generated calendar day after a lexical edge is weaker evidence than a
/// whole-reading lexical path. Keep the edge available for real compounds,
/// but price this ambiguous non-initial splice like an ordinary counter edge.
const GENERATED_DAY_SUFFIX_PENALTY: i64 = COUNTER_WORD_COST - NUMBER_FORM_COST;
// A bare spoken number still offers numeric forms, but common lexical
// homophones should win: せん -> 線, にじゅう -> 二重, さんぜん -> 産前.
const BARE_KANA_NUMBER_FORM_COST: i64 = 5_000;
const MAX_GENERATED_SURFACES: usize = 64;
const DEFAULT_NOUN_ID: u16 = 1_851;
/// Annotation for an appended character the pinned variant rules do not
/// relate to another. It exists so the tail reads as a character list
/// rather than as more ranked conversions.
const SINGLE_KANJI_ANNOTATION: &str = "単漢字";
/// The conversion-side repair metadata is deliberately bounded.  These are
/// heap-backed scratch limits (rather than stack arrays) because the engine
/// worker may run with a small stack.
pub const MAX_CORRECTION_RUNS: usize = 32;
pub const MAX_RAW_REPAIR_PLANS: usize = 8;
pub const DEFAULT_MAX_RAW_REPAIR_PASSES: usize = 4;
pub const DEFAULT_MAX_RAW_REPAIR_CANDIDATES: usize = MAX_CONVERSION_CANDIDATES;
pub const DEFAULT_MAX_RAW_REPAIR_LATTICE_NODES: usize = MAX_LATTICE_NODES;
pub const DEFAULT_MAX_RAW_REPAIR_SEARCH_STATES: usize = MAX_SEARCH_STATES;

const COUNTER_FORMS: [(&str, &str); 15] = [
    ("いっぽん", "1本"),
    ("にほん", "2本"),
    ("さんぼん", "3本"),
    ("よんほん", "4本"),
    ("ごほん", "5本"),
    ("ろっぽん", "6本"),
    ("ななほん", "7本"),
    ("はっぽん", "8本"),
    ("きゅうほん", "9本"),
    ("じゅっぽん", "10本"),
    ("いっぴき", "1匹"),
    ("さんびき", "3匹"),
    ("ろっぴき", "6匹"),
    ("いっかい", "1回"),
    ("さんかい", "3回"),
];

fn numeric_form_cost(source: &str, span: NumericSpan) -> i64 {
    let has_explicit_digit = source
        .chars()
        .any(|character| character.is_ascii_digit() || ('０'..='９').contains(&character));
    if span.counter.is_some() || has_explicit_digit {
        NUMBER_FORM_COST
    } else {
        BARE_KANA_NUMBER_FORM_COST
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Surface {
    Dictionary {
        entry: Entry,
        entry_index: u32,
        repair: Option<RepairKind>,
    },
    User(usize),
    Reading,
    Katakana,
    Literal(&'static str),
    Generated(u16),
}

/// Reusable conversion arenas. Construct once per engine worker and reset for
/// each query.
#[derive(Debug)]
pub struct Converter {
    nodes: Vec<Node>,
    starts_at: Box<[usize; MAX_PREEDIT_BYTES + 1]>,
    ends_at: Box<[usize; MAX_PREEDIT_BYTES + 1]>,
    states: Vec<SearchState>,
    queue: BinaryHeap<HeapItem>,
    path: Vec<usize>,
    candidates: Vec<ConversionCandidate>,
    /// Direct candidates are copied here before the corrected pass resets the
    /// normal lattice. Both vectors are slot-owned heap scratch, so the
    /// sequential passes never acquire another converter slot or recurse.
    raw_direct_scratch: Vec<ConversionCandidate>,
    raw_repair_scratch: Vec<ConversionCandidate>,
    /// Current-only candidates retained while one bounded combined pass
    /// reuses the normal lattice in the same converter slot.
    cross_commit_scratch: Vec<ConversionCandidate>,
    sequence: u64,
    initial_right_id: u16,
    /// Present only while materializing the bounded tail+current pass.
    cross_commit_reading_boundary: Option<usize>,
    lattice_node_budget: usize,
    search_state_budget: usize,
    cross_commit_lattice_node_budget: usize,
    cross_commit_search_state_budget: usize,
    /// Alternate readings taken from commit history for the current query.
    /// Cleared on every convert; populated by the engine before convert when
    /// `InputSupport::commit_based` is active.
    commit_repair_readings: Vec<FixedStr<MAX_PREEDIT_BYTES>>,
    /// Local civil day supplied by the engine for this query only.
    civil_date: Option<CivilDate>,
    generated: Vec<GeneratedSurface>,
    /// Per-reading-length dictionary edge budget. Slot-owned so a lattice can
    /// reset it per reading start without allocating and without spending the
    /// worker's stack; see [`DictionaryEdgeBudget`].
    edge_budget: DictionaryEdgeBudget,
}

#[derive(Debug, Clone)]
struct GeneratedSurface {
    text: FixedStr<MAX_PREEDIT_BYTES>,
    annotation: FixedStr<MAX_PREEDIT_BYTES>,
    counter: Option<NumericCounter>,
}

impl Converter {
    pub fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(MAX_LATTICE_NODES),
            starts_at: Box::new([NONE; MAX_PREEDIT_BYTES + 1]),
            ends_at: Box::new([NONE; MAX_PREEDIT_BYTES + 1]),
            states: Vec::with_capacity(MAX_SEARCH_STATES),
            queue: BinaryHeap::with_capacity(MAX_SEARCH_STATES),
            path: Vec::with_capacity(MAX_PREEDIT_BYTES),
            candidates: Vec::with_capacity(MAX_CONVERSION_CANDIDATES + GENERATED_VARIANT_SLACK),
            // Lazily allocated: ordinary direct conversion must not pay for
            // raw-repair scratch it never uses.
            raw_direct_scratch: Vec::new(),
            raw_repair_scratch: Vec::new(),
            cross_commit_scratch: Vec::with_capacity(
                MAX_CONVERSION_CANDIDATES + GENERATED_VARIANT_SLACK,
            ),
            sequence: 0,
            initial_right_id: 0,
            cross_commit_reading_boundary: None,
            lattice_node_budget: MAX_LATTICE_NODES,
            search_state_budget: MAX_SEARCH_STATES,
            cross_commit_lattice_node_budget: MAX_CROSS_COMMIT_LATTICE_NODES,
            cross_commit_search_state_budget: MAX_CROSS_COMMIT_SEARCH_STATES,
            commit_repair_readings: Vec::new(),
            civil_date: None,
            generated: Vec::with_capacity(MAX_GENERATED_SURFACES),
            edge_budget: DictionaryEdgeBudget::new(),
        }
    }

    /// Supplies the local civil date used to generate 今日-style date surfaces
    /// for the next conversion. `None` keeps the lexical candidates unchanged.
    pub fn set_civil_date(&mut self, date: Option<CivilDate>) {
        self.civil_date = date;
    }

    /// Supplies commit-history repair readings for the next conversion only.
    /// Each reading is looked up in the dictionary and attached to the typed
    /// span with [`COMMIT_HISTORY_PENALTY`].
    pub fn set_commit_repair_readings(&mut self, readings: &[&str]) {
        self.commit_repair_readings.clear();
        for reading in readings {
            if reading.is_empty() {
                continue;
            }
            let mut text = FixedStr::new();
            if text.push_str(reading).is_err() {
                continue;
            }
            if self
                .commit_repair_readings
                .iter()
                .any(|existing| existing.as_str() == *reading)
            {
                continue;
            }
            self.commit_repair_readings.push(text);
        }
    }

    /// Reduces the search budget only in test-support builds. Production always
    /// uses the fixed `MAX_SEARCH_STATES` arena.
    #[cfg(any(test, feature = "conversion-test-support"))]
    pub fn set_search_state_budget_for_test(&mut self, budget: usize) {
        self.search_state_budget = budget.min(MAX_SEARCH_STATES);
    }

    /// Reduces only the optional cross-commit pass. The ordinary conversion
    /// still has its production arena, allowing fail-closed budget tests to
    /// prove that current-only reachability is preserved.
    #[cfg(any(test, feature = "conversion-test-support"))]
    pub fn set_cross_commit_budgets_for_test(
        &mut self,
        lattice_nodes: usize,
        search_states: usize,
    ) {
        self.cross_commit_lattice_node_budget = lattice_nodes.min(MAX_CROSS_COMMIT_LATTICE_NODES);
        self.cross_commit_search_state_budget = search_states.min(MAX_CROSS_COMMIT_SEARCH_STATES);
    }

    #[cfg(any(test, feature = "conversion-test-support"))]
    pub fn set_lattice_node_budget_for_test(&mut self, budget: usize) {
        self.lattice_node_budget = budget.min(MAX_LATTICE_NODES);
    }

    pub fn convert<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<&'a [ConversionCandidate], ConversionError> {
        self.convert_input(dictionary, ConversionInput::ordinary(reading), options)
    }

    /// Converts a classified input while preserving the caller's original
    /// literal surface for exact policies.
    pub fn convert_input<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        input: ConversionInput<'_>,
        options: ConversionOptions,
    ) -> Result<&'a [ConversionCandidate], ConversionError> {
        Ok(self
            .convert_input_detailed(dictionary, input, options)?
            .candidates())
    }

    pub fn convert_detailed<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        self.convert_input_detailed(dictionary, ConversionInput::ordinary(reading), options)
    }

    /// Detailed conversion for a classified input without a user dictionary.
    pub fn convert_input_detailed<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        input: ConversionInput<'_>,
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        self.convert_with_user_dictionary_input_detailed(dictionary, None, input, options)
    }

    /// Converts against the mapped system dictionary plus an optional
    /// process-shared user trie. Both feed the same bounded lattice and
    /// connection-cost matrix, so user entries participate grammatically
    /// instead of being spliced into the final candidate list afterwards.
    pub fn convert_with_user_dictionary<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<&'a [ConversionCandidate], ConversionError> {
        self.convert_with_user_dictionary_input(
            dictionary,
            user_dictionary,
            ConversionInput::ordinary(reading),
            options,
        )
    }

    /// Converts a classified input against the mapped system dictionary and
    /// an optional process-shared user trie.
    pub fn convert_with_user_dictionary_input<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        input: ConversionInput<'_>,
        options: ConversionOptions,
    ) -> Result<&'a [ConversionCandidate], ConversionError> {
        Ok(self
            .convert_with_user_dictionary_input_detailed(
                dictionary,
                user_dictionary,
                input,
                options,
            )?
            .candidates())
    }

    pub fn convert_with_user_dictionary_detailed<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        // These are request-scoped inputs. Consume them before validation so
        // an invalid or rejected request cannot leak them into a later one.
        let civil_date = self.civil_date.take();
        let commit_repairs = std::mem::take(&mut self.commit_repair_readings);
        if reading.is_empty() {
            return Err(ConversionError::EmptyReading);
        }
        if reading.len() > MAX_PREEDIT_BYTES {
            return Err(ConversionError::ReadingTooLong);
        }
        if options.max_candidates == 0
            || options.max_candidates > MAX_CONVERSION_CANDIDATES
            || options.it_bias_per_mille > 1_000
            || options.max_it_boost < 0
        {
            return Err(ConversionError::InvalidOptions);
        }
        // Only a short reading's candidate list pays for a wide ceiling
        // (Issue #95; see `candidate_budget`'s doc comment for the p95
        // numbers). Clamp down, never up, so a caller that already asked
        // for fewer keeps its own smaller number. The research feature
        // disables this so `tools/candidate-sweep` can still see the raw
        // requested limit; clamping there would hide what it measures.
        #[cfg(not(feature = "research-wide-candidates"))]
        let options = ConversionOptions {
            max_candidates: options.max_candidates.min(candidate_budget(reading)),
            ..options
        };

        self.reset(reading.len());
        self.initial_right_id = options.initial_right_id;
        let fallback = make_lossless_fallback(dictionary, reading, self.initial_right_id)?;
        let mut search = SearchRun {
            terminal: ConversionSearchTerminal::SearchExhausted,
            states_pushed: 0,
            incoherent_prefixes_pruned: 0,
        };
        match self.build_lattice(
            dictionary,
            user_dictionary,
            reading,
            options,
            &commit_repairs,
        ) {
            Ok(()) => {
                self.compute_suffix_costs(dictionary, reading.len());
                if let Ok(best_node) = self.best_final_node(dictionary, reading.len()) {
                    if self.viterbi_path_is_coherent(best_node)? {
                        match self.build_viterbi_candidate(
                            dictionary,
                            user_dictionary,
                            reading,
                            best_node,
                        ) {
                            Ok(())
                            | Err(ConversionError::OutputTooLong)
                            | Err(ConversionError::TooManySegments) => {}
                            Err(error) => return Err(error),
                        }
                    }
                    if self.candidates.len() < options.max_candidates {
                        search = self.search_n_best(
                            dictionary,
                            user_dictionary,
                            reading,
                            options.max_candidates,
                        )?;
                    } else {
                        search.terminal = ConversionSearchTerminal::CandidateLimitReached;
                    }
                    self.apply_it_completion_coherence(dictionary, reading, options)?;
                    self.apply_it_compound_coherence(reading, options);
                    self.add_date_candidates(reading, civil_date)?;
                    self.demote_generated_day_suffixes(reading.len());
                    self.prefer_numeric_forms(reading)?;
                    self.drop_jitsu_day_counts(reading);
                    self.apply_exact_lexical_quality_gate(reading);
                    self.drop_kana_fragment_prefix_splits();
                }
            }
            Err(ConversionError::LatticeFull) => {
                search.terminal = ConversionSearchTerminal::LatticeBudgetReached;
            }
            Err(error) => return Err(error),
        }
        self.candidates.sort_by_key(|candidate| candidate.cost);
        self.candidates.truncate(options.max_candidates);
        let lossless_fallback_inserted =
            self.ensure_lossless_fallback(fallback, options.max_candidates);
        self.candidates.sort_by_key(|candidate| candidate.cost);
        self.append_single_kanji(dictionary, reading, options.max_candidates)?;
        self.append_punctuation_family(reading, options.punctuation, options.max_candidates)?;
        debug_assert!(!self.candidates.is_empty());
        Ok(ConversionResult {
            candidates: &self.candidates,
            diagnostics: ConversionDiagnostics {
                terminal: search.terminal,
                lattice_nodes: self.nodes.len(),
                states_pushed: search.states_pushed,
                incoherent_prefixes_pruned: search.incoherent_prefixes_pruned,
                lossless_fallback_inserted,
                raw_repair_passes: 0,
                raw_repair_candidates_added: 0,
                raw_repair_candidates_examined: 0,
                raw_repair_candidates_rejected: 0,
                raw_repair_lattice_nodes: 0,
                raw_repair_search_states: 0,
                cross_commit_bridge_attempted: false,
                cross_commit_bridge_candidates_examined: 0,
                cross_commit_bridge_candidates_rescored: 0,
                cross_commit_bridge_spanning_paths: 0,
                cross_commit_bridge_frontier_paths: 0,
                cross_commit_bridge_lattice_nodes: 0,
                cross_commit_bridge_search_states: 0,
                cross_commit_bridge_terminal: None,
            },
        })
    }

    /// Detailed conversion for a classified input. Exact policies bypass the
    /// ordinary lattice path before repair, spelling, and commit-history hints
    /// are consulted. Request-scoped civil-date and commit-hint state is taken
    /// before validation, so rejected input cannot leak it into a later query.
    pub fn convert_with_user_dictionary_input_detailed<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        input: ConversionInput<'_>,
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        let civil_date = self.civil_date.take();
        let commit_repairs = std::mem::take(&mut self.commit_repair_readings);
        input.validate()?;
        if options.max_candidates == 0
            || options.max_candidates > MAX_CONVERSION_CANDIDATES
            || options.it_bias_per_mille > 1_000
            || options.max_it_boost < 0
        {
            return Err(ConversionError::InvalidOptions);
        }
        // Same reading-length clamp as `convert_with_user_dictionary_detailed`
        // (see its comment and `candidate_budget`'s doc comment). The
        // `LiteralPolicy::Ranked` arm below delegates to that function, which
        // clamps again on the same reading -- harmless, since the clamp only
        // narrows and is idempotent, so do not "fix" the apparent duplicate.
        #[cfg(not(feature = "research-wide-candidates"))]
        let options = ConversionOptions {
            max_candidates: options
                .max_candidates
                .min(candidate_budget(input.lookup_reading)),
            ..options
        };
        match input.literal_policy {
            LiteralPolicy::Ranked => {
                // Keep the legacy implementation in one place. Restore the
                // request state it expects; it consumes it before its own
                // validation and then uses it for calendar/hint edges.
                self.civil_date = civil_date;
                self.commit_repair_readings = commit_repairs;
                self.convert_with_user_dictionary_detailed(
                    dictionary,
                    user_dictionary,
                    input.lookup_reading,
                    options,
                )
            }
            LiteralPolicy::ExactTop1 => self.convert_exact_top1_with_user_dictionary_detailed(
                dictionary,
                user_dictionary,
                input,
                options,
            ),
            LiteralPolicy::ExactOnly => {
                self.convert_exact_only_detailed(dictionary, input, options)
            }
        }
    }

    /// Converts the current reading normally, then optionally replays one
    /// bounded lexical tail in the same slot. The combined pass never emits a
    /// candidate of its own: it can only lower the cost of an already
    /// reachable current-only system candidate with the same surface and
    /// terminal right ID. This preserves current segmentation, provenance,
    /// learning keys, and lossless fallback semantics.
    pub fn convert_with_user_dictionary_input_bridge_detailed<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        input: ConversionInput<'_>,
        options: ConversionOptions,
        bridge: Option<CrossCommitBridge<'_>>,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        let direct_diagnostics = {
            let direct = self.convert_with_user_dictionary_input_detailed(
                dictionary,
                user_dictionary,
                input,
                options,
            )?;
            direct.diagnostics()
        };

        // An exact user-dictionary entry is an explicit user instruction,
        // while the bridge is only implicit contextual evidence. Keep the
        // entire current-only list (including costs and order) unchanged.
        if self
            .candidates
            .iter()
            .any(|candidate| is_exact_user_candidate(candidate, input.lookup_reading.len()))
        {
            return Ok(ConversionResult {
                candidates: &self.candidates,
                diagnostics: direct_diagnostics,
            });
        }

        let Some(bridge) = bridge.filter(|bridge| {
            input.class == ConversionInputClass::Ordinary
                && input.literal_policy == LiteralPolicy::Ranked
                && !bridge.tail_reading.is_empty()
                && !bridge.tail_surface.is_empty()
                && bridge.tail_reading.chars().count() >= MIN_CROSS_COMMIT_TAIL_CHARS
                && bridge.tail_reading.len() <= MAX_CROSS_COMMIT_TAIL_BYTES
                && bridge.tail_surface.len() <= MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES
                && input.lookup_reading.len() <= MAX_CROSS_COMMIT_CURRENT_BYTES
                && bridge
                    .tail_reading
                    .len()
                    .checked_add(input.lookup_reading.len())
                    .is_some_and(|len| len <= MAX_PREEDIT_BYTES)
                && bridge.prefix_cost >= 0
                && bridge.prefix_cost < i64::MAX
                && usize::from(bridge.prefix_right_id.raw()) < dictionary.class_count()
        }) else {
            return Ok(ConversionResult {
                candidates: &self.candidates,
                diagnostics: direct_diagnostics,
            });
        };

        let mut combined = FixedStr::<MAX_PREEDIT_BYTES>::new();
        if combined.push_str(bridge.tail_reading).is_err()
            || combined.push_str(input.lookup_reading).is_err()
        {
            return Ok(ConversionResult {
                candidates: &self.candidates,
                diagnostics: direct_diagnostics,
            });
        }

        self.cross_commit_scratch.clear();
        self.cross_commit_scratch
            .extend(self.candidates.iter().cloned());

        let saved_lattice_budget = self.lattice_node_budget;
        let saved_search_budget = self.search_state_budget;
        self.lattice_node_budget = self
            .lattice_node_budget
            .min(self.cross_commit_lattice_node_budget);
        self.search_state_budget = self
            .search_state_budget
            .min(self.cross_commit_search_state_budget);
        let bridge_options = ConversionOptions {
            initial_right_id: bridge.prefix_right_id.raw(),
            // Cross-boundary evidence is lexical only. Repair, spelling, and
            // history hints belong to the current-only pass and are rejected
            // from this score source even if a future caller installs them.
            skip_input_repair: true,
            ..options
        };
        let saved_bridge_boundary = self.cross_commit_reading_boundary;
        self.cross_commit_reading_boundary = Some(bridge.tail_reading.len());
        let combined_diagnostics = self
            .convert_with_user_dictionary_detailed(
                dictionary,
                None,
                combined.as_str(),
                bridge_options,
            )
            .map(|result| result.diagnostics());
        self.cross_commit_reading_boundary = saved_bridge_boundary;
        self.lattice_node_budget = saved_lattice_budget;
        self.search_state_budget = saved_search_budget;

        let mut diagnostics = direct_diagnostics;
        diagnostics.cross_commit_bridge_attempted = true;
        if let Ok(combined_diagnostics) = combined_diagnostics {
            diagnostics.cross_commit_bridge_candidates_examined = self.candidates.len();
            diagnostics.cross_commit_bridge_lattice_nodes = combined_diagnostics.lattice_nodes;
            diagnostics.cross_commit_bridge_search_states = combined_diagnostics.states_pushed;
            diagnostics.cross_commit_bridge_terminal = Some(combined_diagnostics.terminal);

            let complete_search = matches!(
                combined_diagnostics.terminal,
                ConversionSearchTerminal::SearchExhausted
                    | ConversionSearchTerminal::CandidateLimitReached
            );
            if complete_search {
                const BRIDGE_CANDIDATE_CAPACITY: usize =
                    MAX_CONVERSION_CANDIDATES + GENERATED_VARIANT_SLACK;
                let mut bridge_costs = [i64::MAX; BRIDGE_CANDIDATE_CAPACITY];
                let mut proposed_costs = [i64::MAX; BRIDGE_CANDIDATE_CAPACITY];
                for (index, candidate) in self.cross_commit_scratch.iter().enumerate() {
                    proposed_costs[index] = candidate.cost;
                }

                // Fold every admissible combined path once into the cheapest
                // normalized evidence for each current-only candidate. This
                // is O(K²), with K independently bounded by the candidate
                // limit, and avoids nested scans during sibling transfer.
                for combined_candidate in &self.candidates {
                    if combined_candidate.origin() != CandidateOrigin::Direct
                        || !combined_candidate.path_evidence().is_system_only()
                    {
                        continue;
                    }
                    let Some(boundary_kind) = combined_candidate.bridge_boundary_kind() else {
                        continue;
                    };
                    let Some(current_surface) =
                        combined_candidate.text().strip_prefix(bridge.tail_surface)
                    else {
                        continue;
                    };
                    if current_surface.is_empty() {
                        continue;
                    }
                    match boundary_kind {
                        BridgeBoundaryKind::SpanningEdge => {
                            diagnostics.cross_commit_bridge_spanning_paths = diagnostics
                                .cross_commit_bridge_spanning_paths
                                .saturating_add(1);
                        }
                        BridgeBoundaryKind::TypedFrontier => {
                            diagnostics.cross_commit_bridge_frontier_paths = diagnostics
                                .cross_commit_bridge_frontier_paths
                                .saturating_add(1);
                        }
                    }
                    let Some(bridge_cost) = combined_candidate.cost.checked_sub(bridge.prefix_cost)
                    else {
                        continue;
                    };
                    // A negative delta means the combined pass reanalysed the
                    // retained tail into a cheaper path than the one the user
                    // actually committed. That is not a calibrated score in
                    // the current-only domain, so it fails closed.
                    if bridge_cost < 0 {
                        continue;
                    }
                    let combined_right_id = combined_candidate
                        .segments()
                        .last()
                        .map_or(0, |segment| segment.right_id);
                    let Some(anchor_index) = self.cross_commit_scratch.iter().position(|current| {
                        let current_right_id = current
                            .segments()
                            .last()
                            .map_or(0, |segment| segment.right_id);
                        current.origin() == CandidateOrigin::Direct
                            && !current.is_synthetic_exact()
                            && current.path_evidence().is_system_only()
                            && current.text() == current_surface
                            && current_right_id == combined_right_id
                    }) else {
                        continue;
                    };
                    bridge_costs[anchor_index] = bridge_costs[anchor_index].min(bridge_cost);
                }

                for anchor_index in 0..self.cross_commit_scratch.len() {
                    let bridge_cost = bridge_costs[anchor_index];
                    let anchor_cost = self.cross_commit_scratch[anchor_index].cost;
                    if bridge_cost >= anchor_cost {
                        continue;
                    }
                    proposed_costs[anchor_index] = proposed_costs[anchor_index].min(bridge_cost);
                    let contextual_gain = anchor_cost - bridge_cost;
                    let anchor = &self.cross_commit_scratch[anchor_index];
                    let combined_right_id = anchor
                        .segments()
                        .last()
                        .map_or(0, |segment| segment.right_id);
                    let current_surface = anchor.text();

                    // Orthographic transfer starts only from the exact kana
                    // reading the user entered. A contextually helped kanji
                    // lexeme must not become an anchor that promotes other
                    // same-ending lexemes merely because their spelling looks
                    // similar.
                    if current_surface != input.lookup_reading {
                        continue;
                    }

                    // A reviewed lexical edge commonly uses hiragana for an
                    // inflected ending while the dictionary also carries a
                    // kanji spelling (ないか / 無いか). Transfer the measured
                    // contextual gain only to a system candidate that keeps
                    // the same terminal class, preserves a majority kana
                    // suffix, and has its own exact full-context path. This
                    // does not turn unrelated homophones such as 内科 or the
                    // one-kana overlap 内か into members of the family.
                    for index in 0..self.cross_commit_scratch.len() {
                        if index == anchor_index {
                            continue;
                        }
                        let variant = &self.cross_commit_scratch[index];
                        let variant_right_id = variant
                            .segments()
                            .last()
                            .map_or(0, |segment| segment.right_id);
                        if variant.origin() != CandidateOrigin::Direct
                            || variant.is_synthetic_exact()
                            || !variant.path_evidence().is_system_only()
                            || variant_right_id != combined_right_id
                            || !is_contextual_orthographic_sibling(current_surface, variant.text())
                        {
                            continue;
                        }
                        let full_context_cost = bridge_costs[index];
                        if full_context_cost == i64::MAX {
                            continue;
                        }
                        let Some(full_context_cost) =
                            full_context_cost.checked_sub(contextual_gain)
                        else {
                            continue;
                        };
                        let transferred_cost = full_context_cost.max(bridge_cost.saturating_add(1));
                        proposed_costs[index] = proposed_costs[index].min(transferred_cost);
                    }
                }
                for (index, candidate) in self.cross_commit_scratch.iter_mut().enumerate() {
                    if proposed_costs[index] < candidate.cost {
                        candidate.cost = proposed_costs[index];
                        candidate.cross_commit_rescored = true;
                        diagnostics.cross_commit_bridge_candidates_rescored = diagnostics
                            .cross_commit_bridge_candidates_rescored
                            .saturating_add(1);
                    }
                }
                self.cross_commit_scratch
                    .sort_by_key(|candidate| candidate.cost);
            }
        }

        // Whether the optional pass succeeded, exhausted a budget, or failed
        // validation internally, the externally visible list is always the
        // complete current-only list retained before the replay.
        std::mem::swap(&mut self.candidates, &mut self.cross_commit_scratch);
        Ok(ConversionResult {
            candidates: &self.candidates,
            diagnostics,
        })
    }

    fn convert_exact_only_detailed<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        input: ConversionInput<'_>,
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        self.reset(input.lookup_reading.len());
        self.initial_right_id = options.initial_right_id;
        self.candidates.push(make_synthetic_exact(
            dictionary,
            input.lookup_reading,
            input.exact_surface,
            self.initial_right_id,
        )?);
        Ok(ConversionResult {
            candidates: &self.candidates,
            diagnostics: ConversionDiagnostics {
                terminal: ConversionSearchTerminal::SearchExhausted,
                lattice_nodes: 0,
                states_pushed: 0,
                incoherent_prefixes_pruned: 0,
                lossless_fallback_inserted: true,
                raw_repair_passes: 0,
                raw_repair_candidates_added: 0,
                raw_repair_candidates_examined: 0,
                raw_repair_candidates_rejected: 0,
                raw_repair_lattice_nodes: 0,
                raw_repair_search_states: 0,
                cross_commit_bridge_attempted: false,
                cross_commit_bridge_candidates_examined: 0,
                cross_commit_bridge_candidates_rescored: 0,
                cross_commit_bridge_spanning_paths: 0,
                cross_commit_bridge_frontier_paths: 0,
                cross_commit_bridge_lattice_nodes: 0,
                cross_commit_bridge_search_states: 0,
                cross_commit_bridge_terminal: None,
            },
        })
    }

    fn convert_exact_top1_with_user_dictionary_detailed<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        input: ConversionInput<'_>,
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        self.reset(input.lookup_reading.len());
        self.initial_right_id = options.initial_right_id;
        let exact = make_synthetic_exact(
            dictionary,
            input.lookup_reading,
            input.exact_surface,
            self.initial_right_id,
        )?;
        let alternative_limit = options.max_candidates.saturating_sub(1);
        let mut search = SearchRun {
            terminal: ConversionSearchTerminal::SearchExhausted,
            states_pushed: 0,
            incoherent_prefixes_pruned: 0,
        };

        if alternative_limit > 0 {
            match self.build_exact_top1_lattice(
                dictionary,
                user_dictionary,
                input.lookup_reading,
                options,
            ) {
                Ok(()) => {
                    self.compute_suffix_costs(dictionary, input.lookup_reading.len());
                    if let Ok(best_node) =
                        self.best_final_node(dictionary, input.lookup_reading.len())
                    {
                        match self.build_viterbi_candidate(
                            dictionary,
                            user_dictionary,
                            input.lookup_reading,
                            best_node,
                        ) {
                            Ok(())
                            | Err(ConversionError::OutputTooLong)
                            | Err(ConversionError::TooManySegments) => {}
                            Err(error) => return Err(error),
                        }
                        if self.candidates.len() < alternative_limit {
                            search = self.search_n_best(
                                dictionary,
                                user_dictionary,
                                input.lookup_reading,
                                alternative_limit,
                            )?;
                        } else if !self.candidates.is_empty() {
                            search.terminal = ConversionSearchTerminal::CandidateLimitReached;
                        }
                    }
                }
                Err(ConversionError::LatticeFull) => {
                    search.terminal = ConversionSearchTerminal::LatticeBudgetReached;
                }
                Err(error) => return Err(error),
            }
        }

        self.candidates.sort_by_key(|candidate| candidate.cost);
        // A dictionary entry rendering exactly like the raw literal is already
        // represented by the policy-owned candidate. Do not duplicate it.
        self.candidates
            .retain(|candidate| candidate.text() != input.exact_surface);
        self.candidates.truncate(alternative_limit);
        self.candidates.insert(0, exact);
        Ok(ConversionResult {
            candidates: &self.candidates,
            diagnostics: ConversionDiagnostics {
                terminal: search.terminal,
                lattice_nodes: self.nodes.len(),
                states_pushed: search.states_pushed,
                incoherent_prefixes_pruned: search.incoherent_prefixes_pruned,
                lossless_fallback_inserted: true,
                raw_repair_passes: 0,
                raw_repair_candidates_added: 0,
                raw_repair_candidates_examined: 0,
                raw_repair_candidates_rejected: 0,
                raw_repair_lattice_nodes: 0,
                raw_repair_search_states: 0,
                cross_commit_bridge_attempted: false,
                cross_commit_bridge_candidates_examined: 0,
                cross_commit_bridge_candidates_rescored: 0,
                cross_commit_bridge_spanning_paths: 0,
                cross_commit_bridge_frontier_paths: 0,
                cross_commit_bridge_lattice_nodes: 0,
                cross_commit_bridge_search_states: 0,
                cross_commit_bridge_terminal: None,
            },
        })
    }

    /// Builds only exact full-query lexical edges for an opaque identifier.
    /// Every admitted edge starts at zero and ends at the full reading, and
    /// spelling-correction entries are excluded.
    fn build_exact_top1_lattice(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<(), ConversionError> {
        let reading_len = reading.len();
        let exact_edge_limit = options.max_candidates.saturating_sub(1);
        let mut failure = None;
        let mut exact_edges_added = 0usize;
        dictionary
            .common_prefix_search(reading, |matched| {
                if matched.matched_bytes != reading_len
                    || matched
                        .entry
                        .flags
                        .contains(EntryFlags::SPELLING_CORRECTION)
                    || matched.entry.flags.contains(EntryFlags::NON_INITIAL)
                    || exact_edges_added >= exact_edge_limit
                {
                    return true;
                }
                let boost = if matched.entry.flags.contains(EntryFlags::IT) {
                    let proportional = i64::from(matched.entry.word_cost.max(0))
                        .saturating_mul(i64::from(options.it_bias_per_mille))
                        / 1_000;
                    proportional.min(i64::from(options.max_it_boost))
                } else {
                    0
                };
                let local_cost = i64::from(matched.entry.word_cost).saturating_sub(boost);
                let Ok(entry_index) = u32::try_from(matched.entry_index) else {
                    return true;
                };
                match self.add_node(
                    dictionary,
                    NodeSpec {
                        start: 0,
                        end: reading_len,
                        left_id: matched.entry.left_id,
                        right_id: matched.entry.right_id,
                        local_cost,
                        surface: Surface::Dictionary {
                            entry: matched.entry,
                            entry_index,
                            repair: None,
                        },
                    },
                ) {
                    Ok(()) => {
                        exact_edges_added = exact_edges_added.saturating_add(1);
                        true
                    }
                    Err(error) => {
                        failure = Some(error);
                        false
                    }
                }
            })
            .map_err(ConversionError::Dictionary)?;
        if let Some(error) = failure {
            return Err(error);
        }

        if let Some(user_dictionary) = user_dictionary {
            let mut user_failure = None;
            user_dictionary.common_prefix_search(reading, |matched_bytes, entry_index| {
                if matched_bytes != reading_len || exact_edges_added >= exact_edge_limit {
                    return true;
                }
                let Some(entry) = user_dictionary.entry(entry_index) else {
                    return true;
                };
                let left_id = if usize::from(entry.left_id()) < dictionary.class_count() {
                    entry.left_id()
                } else {
                    0
                };
                let right_id = if usize::from(entry.right_id()) < dictionary.class_count() {
                    entry.right_id()
                } else {
                    0
                };
                match self.add_node(
                    dictionary,
                    NodeSpec {
                        start: 0,
                        end: reading_len,
                        left_id,
                        right_id,
                        local_cost: i64::from(entry.word_cost()),
                        surface: Surface::User(entry_index),
                    },
                ) {
                    Ok(()) => {
                        exact_edges_added = exact_edges_added.saturating_add(1);
                        true
                    }
                    Err(error) => {
                        user_failure = Some(error);
                        false
                    }
                }
            });
            if let Some(error) = user_failure {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Runs an original direct conversion and a bounded sequence of corrected
    /// readings while retaining this one [`Converter`] arena.  The corrected
    /// passes intentionally use only the system dictionary and set
    /// `skip_input_repair`; they can therefore never recursively acquire a
    /// conversion slot or feed another repair pass.
    ///
    /// Direct candidates are copied to slot-owned heap scratch before the
    /// first corrected pass. They retain their original order and authority.
    /// When they already fill the public cap, one admitted local repair may
    /// replace only the lowest-priority evictable direct tail; candidate zero,
    /// exact literals, and user-backed candidates are never displaced.
    pub fn convert_input_with_raw_repair_plans<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        original_input: ConversionInput<'_>,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        // The direct pass consumes the one-shot civil date and commit-history
        // hints. Corrected passes must not recreate either piece of request
        // state while they reuse the converter arena.
        let direct_diagnostics = {
            let result = self.convert_with_user_dictionary_input_detailed(
                dictionary,
                user_dictionary,
                original_input,
                options,
            )?;
            result.diagnostics()
        };
        self.raw_direct_scratch.clear();
        self.raw_direct_scratch
            .extend(self.candidates.iter().cloned());
        self.raw_repair_scratch.clear();

        let mut diagnostics = direct_diagnostics;
        let direct_fills_cap = self.raw_direct_scratch.len() >= options.max_candidates;
        let direct_eviction_index = direct_fills_cap.then(|| {
            self.raw_direct_scratch
                .iter()
                .enumerate()
                .skip(1)
                .rfind(|(_, candidate)| {
                    !candidate.is_synthetic_exact() && candidate.path_evidence().user_edges == 0
                })
                .map(|(index, _)| index)
        });
        if plans.is_empty() || (direct_fills_cap && direct_eviction_index.flatten().is_none()) {
            return Ok(ConversionResult {
                candidates: &self.candidates,
                diagnostics,
            });
        }
        let direct_eviction_index = direct_eviction_index.flatten();

        let budget = options.raw_repair_budget;
        let max_passes = budget.max_corrected_passes.min(MAX_RAW_REPAIR_PLANS);
        let max_repair_candidates = budget.max_repair_candidates.min(MAX_CONVERSION_CANDIDATES);
        let repair_slot_limit = if direct_fills_cap {
            1
        } else {
            options
                .max_candidates
                .saturating_sub(self.raw_direct_scratch.len())
        }
        .min(max_repair_candidates);
        let max_lattice_nodes = budget.max_lattice_nodes.min(MAX_LATTICE_NODES);
        let max_search_states = budget.max_search_states.min(MAX_SEARCH_STATES);
        let mut passes = 0usize;
        let mut aggregate_candidates_examined = 0usize;
        let mut rejected = 0usize;
        let mut aggregate_lattice_nodes = 0usize;
        let mut aggregate_search_states = 0usize;
        let plan_count = plans.len().min(MAX_RAW_REPAIR_PLANS);
        for (plan_index, plan) in plans.iter().take(MAX_RAW_REPAIR_PLANS).enumerate() {
            if repair_slot_limit == 0 {
                break;
            }
            if passes >= max_passes
                || aggregate_candidates_examined >= max_repair_candidates
                || (!direct_fills_cap && self.raw_repair_scratch.len() >= repair_slot_limit)
            {
                break;
            }
            // Phase 1 is structural local completion only.  Keep the general
            // insertion tier representable for the later experiment, but do
            // not run or admit it from this production-facing API.
            if plan.tier() != RepairTier::LocalCompletion {
                rejected = rejected.saturating_add(1);
                continue;
            }
            if !plan.is_valid_for(original_input.lookup_reading)
                || plan
                    .map
                    .validate_for_readings(original_input.lookup_reading, plan.corrected_reading())
                    .is_err()
            {
                rejected = rejected.saturating_add(1);
                continue;
            }

            if aggregate_lattice_nodes >= max_lattice_nodes
                || aggregate_search_states >= max_search_states
            {
                rejected = rejected.saturating_add(1);
                break;
            }
            let remaining_repair_candidates =
                max_repair_candidates.saturating_sub(aggregate_candidates_examined);
            if remaining_repair_candidates == 0 {
                break;
            }
            let mut corrected_options = options;
            // Divide the remaining repair output fairly across the remaining
            // bounded local plans.  A first spelling can otherwise consume
            // all 18 dictionary rows and prevent a later key (for example
            // `e` in nazka) from ever being examined.  With one plan this is
            // the old full breadth; with a full direct list the reservation
            // naturally yields one row per plan.
            let plans_remaining = plan_count.saturating_sub(plan_index).max(1);
            let remaining_output_slots =
                repair_slot_limit.saturating_sub(self.raw_repair_scratch.len());
            let fair_share = remaining_output_slots.div_ceil(plans_remaining);
            corrected_options.max_candidates = fair_share.min(remaining_repair_candidates).max(1);
            corrected_options.skip_input_repair = true;
            // Do not pass a user dictionary to a Phase 1 corrected pass.  A
            // user edge would make the lexical evidence non-admissible.
            let previous_lattice_budget = self.lattice_node_budget;
            let previous_search_budget = self.search_state_budget;
            self.lattice_node_budget = previous_lattice_budget
                .min(max_lattice_nodes.saturating_sub(aggregate_lattice_nodes));
            self.search_state_budget = previous_search_budget
                .min(max_search_states.saturating_sub(aggregate_search_states));
            passes = passes.saturating_add(1);
            let pass = match self.convert_with_user_dictionary_input_detailed(
                dictionary,
                None,
                ConversionInput::ordinary(plan.corrected_reading()),
                corrected_options,
            ) {
                Ok(result) => result.diagnostics(),
                Err(_) => {
                    aggregate_lattice_nodes =
                        aggregate_lattice_nodes.saturating_add(self.nodes.len());
                    aggregate_search_states =
                        aggregate_search_states.saturating_add(self.states.len());
                    aggregate_candidates_examined =
                        aggregate_candidates_examined.saturating_add(self.candidates.len());
                    self.lattice_node_budget = previous_lattice_budget;
                    self.search_state_budget = previous_search_budget;
                    rejected = rejected.saturating_add(1);
                    continue;
                }
            };
            aggregate_lattice_nodes = aggregate_lattice_nodes.saturating_add(pass.lattice_nodes);
            aggregate_search_states = aggregate_search_states.saturating_add(pass.states_pushed);
            aggregate_candidates_examined =
                aggregate_candidates_examined.saturating_add(self.candidates.len());
            self.lattice_node_budget = previous_lattice_budget;
            self.search_state_budget = previous_search_budget;
            if !matches!(
                pass.terminal,
                ConversionSearchTerminal::SearchExhausted
                    | ConversionSearchTerminal::CandidateLimitReached
            ) {
                rejected = rejected.saturating_add(1);
                continue;
            }

            rejected = rejected.saturating_add(self.admit_repair_pass(
                plan,
                repair_slot_limit,
                direct_fills_cap,
            ));
        }

        self.candidates.clear();
        self.candidates
            .extend(self.raw_direct_scratch.iter().cloned());
        if !self.raw_repair_scratch.is_empty() {
            if let Some(index) = direct_eviction_index {
                self.candidates.remove(index);
            }
        }
        let repair_candidates_added = self.merge_repair_scratch(options.max_candidates);
        diagnostics.raw_repair_passes = passes;
        diagnostics.raw_repair_candidates_added = repair_candidates_added;
        diagnostics.raw_repair_candidates_examined = aggregate_candidates_examined;
        diagnostics.raw_repair_candidates_rejected = rejected;
        diagnostics.raw_repair_lattice_nodes = aggregate_lattice_nodes;
        diagnostics.raw_repair_search_states = aggregate_search_states;
        Ok(ConversionResult {
            candidates: &self.candidates,
            diagnostics,
        })
    }

    /// Admits one corrected pass's fully-covered candidates into the repair
    /// scratch and reports how many rows it rejected.
    ///
    /// This is a sibling frame on purpose, not an inlined block.
    /// `ConversionCandidate` is 4,152 bytes, an unoptimised build gives every
    /// by-value move and every temporary its own stack slot, and ten of them
    /// sat in `convert_input_with_raw_repair_plans` — a 41,456-byte frame that
    /// stayed live across the corrected pass's entire conversion subtree. Held
    /// here they are live only while nothing deep is running.
    /// `raw_multi_pass_core_path_fits_128_kib_thread_stack` is what fails when
    /// this moves back inline; keep the orchestrating frame free of candidates.
    #[inline(never)]
    fn admit_repair_pass(
        &mut self,
        plan: &RawRepairPlan,
        repair_slot_limit: usize,
        direct_fills_cap: bool,
    ) -> usize {
        let accepted: Vec<ConversionCandidate> = self
            .candidates
            .iter()
            .filter(|candidate| candidate.has_full_system_coverage(plan.corrected_reading().len()))
            .cloned()
            .collect();
        if accepted.is_empty() {
            return 1;
        }
        let mut rejected = 0usize;
        for mut candidate in accepted {
            candidate.origin = CandidateOrigin::RawRepair {
                plan_id: plan.plan_id(),
                tier: plan.tier(),
            };
            if self
                .raw_direct_scratch
                .iter()
                .any(|direct| direct.text() == candidate.text())
            {
                rejected = rejected.saturating_add(1);
                continue;
            }
            if let Some(existing) = self
                .raw_repair_scratch
                .iter_mut()
                .find(|existing| existing.text() == candidate.text())
            {
                if candidate.authority().rank() > existing.authority().rank()
                    || (candidate.authority() == existing.authority()
                        && candidate.cost < existing.cost)
                {
                    *existing = candidate;
                }
                continue;
            }
            if self.raw_repair_scratch.len() < repair_slot_limit {
                self.raw_repair_scratch.push(candidate);
            } else if direct_fills_cap {
                let worst = self
                    .raw_repair_scratch
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, existing)| existing.cost)
                    .map(|(index, _)| index)
                    .expect("a full reservation has a candidate");
                if candidate.cost < self.raw_repair_scratch[worst].cost {
                    self.raw_repair_scratch[worst] = candidate;
                } else {
                    rejected = rejected.saturating_add(1);
                }
            } else {
                rejected = rejected.saturating_add(1);
            }
        }
        rejected
    }

    /// Appends the repair scratch to the finished candidate list, skipping
    /// surfaces the direct pass already produced, and reports how many rows it
    /// added. Split out for the same stack reason as [`Self::admit_repair_pass`].
    #[inline(never)]
    fn merge_repair_scratch(&mut self, max_candidates: usize) -> usize {
        let mut added = 0usize;
        for candidate in self.raw_repair_scratch.iter().cloned() {
            if self.candidates.len() >= max_candidates {
                break;
            }
            if self
                .candidates
                .iter()
                .any(|existing| existing.text() == candidate.text())
            {
                continue;
            }
            self.candidates.push(candidate);
            added = added.saturating_add(1);
        }
        added
    }

    /// Backward-compatible ordinary-input wrapper. Classified production
    /// callers should use [`Self::convert_input_with_raw_repair_plans`] so the
    /// direct pass cannot lose an exact literal policy at the repair boundary.
    pub fn convert_with_raw_repair_plans<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        original_reading: &str,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        self.convert_input_with_raw_repair_plans(
            dictionary,
            user_dictionary,
            ConversionInput::ordinary(original_reading),
            plans,
            options,
        )
    }

    /// Input-aware closure wrapper for production callers that must consume
    /// the candidate slice before the converter slot is released.
    pub fn with_raw_repair_input_conversion<R>(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        original_input: ConversionInput<'_>,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConversionError> {
        let result = self.convert_input_with_raw_repair_plans(
            dictionary,
            user_dictionary,
            original_input,
            plans,
            options,
        )?;
        let diagnostics = result.diagnostics();
        Ok(consume(result.candidates(), diagnostics))
    }

    /// Closure-shaped convenience wrapper for callers that must consume the
    /// candidate slice before the converter slot is released.
    pub fn with_raw_repair_conversion<R>(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        original_reading: &str,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConversionError> {
        let result = self.convert_input_with_raw_repair_plans(
            dictionary,
            user_dictionary,
            ConversionInput::ordinary(original_reading),
            plans,
            options,
        )?;
        let diagnostics = result.diagnostics();
        Ok(consume(result.candidates(), diagnostics))
    }

    fn ensure_lossless_fallback(&mut self, fallback: ConversionCandidate, wanted: usize) -> bool {
        if self
            .candidates
            .iter()
            .any(|candidate| candidate.text() == fallback.text())
        {
            return false;
        }
        if self.candidates.len() >= wanted {
            return false;
        }
        self.candidates.push(fallback);
        true
    }

    /// A generated day at a non-zero reading offset is only an ambiguous
    /// compound edge when a lexical node can actually precede it. Synthetic
    /// reading/katakana nodes are intentionally ignored: they do not provide
    /// the lexical evidence this admission guard is meant to qualify.
    fn has_lexical_predecessor(&self, start: usize) -> bool {
        let mut previous = self.ends_at.get(start).copied().unwrap_or(NONE);
        while previous != NONE {
            let node = self.nodes[previous];
            if matches!(node.surface, Surface::Dictionary { .. } | Surface::User(_)) {
                return true;
            }
            previous = node.next_at_end;
        }
        false
    }

    fn add_numeric_forms(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        start: usize,
        synthetic_id: u16,
    ) -> Result<(), ConversionError> {
        let Some(span) = parse_numeric_prefix(&reading[start..]) else {
            return Ok(());
        };
        if !should_emit_numeric_span(span) {
            return Ok(());
        }
        let end = start + span.bytes;
        if span.counter.is_none() && end < reading.len() {
            // A bare number prefix must not split ordinary words. せんじつ is
            // 先日, not 1000 followed by 日.
            return Ok(());
        }
        let lexical_predecessor = span.counter == Some(NumericCounter::Day)
            && start > 0
            && self.has_lexical_predecessor(start);
        let form_cost =
            numeric_form_cost(&reading[start..end], span).saturating_add(if lexical_predecessor {
                GENERATED_DAY_SUFFIX_PENALTY
            } else {
                0
            });
        for (index, style) in NUMERIC_STYLES.into_iter().enumerate() {
            if self.generated.len() >= MAX_GENERATED_SURFACES {
                break;
            }
            let mut text = FixedStr::new();
            if style.write(span, &mut text).is_err() {
                continue;
            }
            let mut annotation = FixedStr::new();
            if annotation.push_str(style.annotation()).is_err() {
                continue;
            }
            if dictionary_has_exact_surface(dictionary, &reading[start..end], text.as_str())? {
                // Preserve the dictionary edge and its cost/provenance. Adding
                // an identical cheaper generated edge here would make N-best
                // surface deduplication discard the lexical candidate.
                continue;
            }
            let Ok(generated_index) = u16::try_from(self.generated.len()) else {
                break;
            };
            self.generated.push(GeneratedSurface {
                text,
                annotation,
                counter: span.counter,
            });
            self.add_node(
                dictionary,
                NodeSpec {
                    start,
                    end,
                    left_id: synthetic_id,
                    right_id: synthetic_id,
                    local_cost: form_cost.saturating_add(i64::try_from(index).unwrap_or(0)),
                    surface: Surface::Generated(generated_index),
                },
            )?;
        }
        Ok(())
    }
}

impl Default for Converter {
    fn default() -> Self {
        Self::new()
    }
}

fn connection_cost(
    dictionary: &Dictionary<'_>,
    right_id: RightContextId,
    left_id: LeftContextId,
) -> i64 {
    i64::from(
        dictionary
            .connection_cost(right_id.raw(), left_id.raw())
            .unwrap_or(u16::MAX),
    )
}

fn is_contextual_orthographic_sibling(anchor: &str, candidate: &str) -> bool {
    let anchor_chars = anchor.chars().count();
    let candidate_chars = candidate.chars().count();
    if anchor_chars == 0 || candidate_chars == 0 || anchor == candidate {
        return false;
    }
    let shared = anchor
        .chars()
        .rev()
        .zip(candidate.chars().rev())
        .take_while(|(left, right)| left == right && matches!(*left, '\u{3041}'..='\u{3096}'))
        .count();
    shared >= 2
        && shared.saturating_mul(2) >= anchor_chars
        && shared.saturating_mul(2) >= candidate_chars
}

fn is_exact_user_candidate(candidate: &ConversionCandidate, reading_len: usize) -> bool {
    let evidence = candidate.path_evidence();
    candidate.origin() == CandidateOrigin::Direct
        && !candidate.is_synthetic_exact()
        && evidence.system_edges == 0
        && evidence.user_edges == 1
        && evidence.fallback_edges == 0
        && evidence.generated_edges == 0
        && evidence.spelling_edges == 0
        && candidate.segments().len() == 1
        && candidate.segments()[0].reading_start == 0
        && usize::from(candidate.segments()[0].reading_end) == reading_len
}

fn dictionary_has_exact_surface(
    dictionary: &Dictionary<'_>,
    reading: &str,
    expected_surface: &str,
) -> Result<bool, ConversionError> {
    let mut found = false;
    let mut write_error = None;
    dictionary
        .common_prefix_search(reading, |matched| {
            if matched.matched_bytes != reading.len() {
                return true;
            }
            let mut surface = FixedStr::<MAX_PREEDIT_BYTES>::new();
            match dictionary.write_surface(matched.entry, &mut surface) {
                Ok(()) => {
                    found = surface.as_str() == expected_surface;
                    !found
                }
                Err(error) => {
                    write_error = Some(error);
                    false
                }
            }
        })
        .map_err(ConversionError::Dictionary)?;
    if let Some(error) = write_error {
        return Err(ConversionError::Dictionary(error));
    }
    Ok(found)
}

fn make_lossless_fallback(
    dictionary: &Dictionary<'_>,
    reading: &str,
    initial_right_id: u16,
) -> Result<ConversionCandidate, ConversionError> {
    let synthetic_id = if dictionary.class_count() > usize::from(DEFAULT_NOUN_ID) {
        DEFAULT_NOUN_ID
    } else {
        0
    };
    let characters = reading.chars().count();
    let local_cost = if characters == 1 {
        FALLBACK_WORD_COST
    } else {
        synthetic_run_cost(RUN_BASE_COST, RUN_COST_PER_CHAR, characters)
    };
    let cost = connection_cost(
        dictionary,
        RightContextId::new(initial_right_id),
        LeftContextId::new(synthetic_id),
    )
    .saturating_add(local_cost)
    .saturating_add(connection_cost(
        dictionary,
        RightContextId::new(synthetic_id),
        LeftContextId::new(0),
    ));
    let mut text = FixedStr::new();
    text.push_str(reading)
        .map_err(|_| ConversionError::OutputTooLong)?;
    let mut segments = FixedVec::new();
    segments
        .push(ConversionSegment {
            reading_start: 0,
            reading_end: u16::try_from(reading.len())
                .map_err(|_| ConversionError::ReadingTooLong)?,
            text_start: 0,
            text_end: u16::try_from(text.len()).map_err(|_| ConversionError::OutputTooLong)?,
            left_id: synthetic_id,
            right_id: synthetic_id,
            flags: EntryFlags::NONE,
            word_count: 1,
            it_word_count: 0,
        })
        .map_err(|_| ConversionError::TooManySegments)?;
    Ok(ConversionCandidate {
        text,
        annotation: FixedStr::new(),
        segments,
        system_entry_index: NO_SYSTEM_ENTRY_INDEX,
        synthetic_exact: false,
        origin: CandidateOrigin::Direct,
        path_evidence: PathEvidence {
            fallback_edges: 1,
            ..PathEvidence::default()
        },
        generated_day_suffix: false,
        bridge_boundary_kind: None,
        commit_bridge_tail: CommitBridgeTailStorage::default(),
        cross_commit_rescored: false,
        cost,
    })
}

/// Creates the policy-owned exact literal candidate. Unlike the ordinary
/// lossless fallback, this marker lets callers distinguish intentional raw
/// preservation from an emergency OOV candidate.
fn make_synthetic_exact(
    dictionary: &Dictionary<'_>,
    lookup_reading: &str,
    exact_surface: &str,
    initial_right_id: u16,
) -> Result<ConversionCandidate, ConversionError> {
    let synthetic_id = if dictionary.class_count() > usize::from(DEFAULT_NOUN_ID) {
        DEFAULT_NOUN_ID
    } else {
        0
    };
    let characters = lookup_reading.chars().count();
    let local_cost = if characters == 1 {
        FALLBACK_WORD_COST
    } else {
        synthetic_run_cost(RUN_BASE_COST, RUN_COST_PER_CHAR, characters)
    };
    let cost = connection_cost(
        dictionary,
        RightContextId::new(initial_right_id),
        LeftContextId::new(synthetic_id),
    )
    .saturating_add(local_cost)
    .saturating_add(connection_cost(
        dictionary,
        RightContextId::new(synthetic_id),
        LeftContextId::new(0),
    ));
    let mut text = FixedStr::new();
    text.push_str(exact_surface)
        .map_err(|_| ConversionError::OutputTooLong)?;
    let mut segments = FixedVec::new();
    segments
        .push(ConversionSegment {
            reading_start: 0,
            reading_end: u16::try_from(lookup_reading.len())
                .map_err(|_| ConversionError::ReadingTooLong)?,
            text_start: 0,
            text_end: u16::try_from(text.len()).map_err(|_| ConversionError::OutputTooLong)?,
            left_id: synthetic_id,
            right_id: synthetic_id,
            flags: EntryFlags::NONE,
            word_count: 1,
            it_word_count: 0,
        })
        .map_err(|_| ConversionError::TooManySegments)?;
    Ok(ConversionCandidate {
        text,
        annotation: FixedStr::new(),
        segments,
        system_entry_index: NO_SYSTEM_ENTRY_INDEX,
        synthetic_exact: true,
        origin: CandidateOrigin::Direct,
        path_evidence: PathEvidence {
            fallback_edges: 1,
            ..PathEvidence::default()
        },
        generated_day_suffix: false,
        bridge_boundary_kind: None,
        commit_bridge_tail: CommitBridgeTailStorage::default(),
        cross_commit_rescored: false,
        cost,
    })
}

struct CandidateMaterialization<'a, 'dictionary> {
    dictionary: &'a Dictionary<'dictionary>,
    user_dictionary: Option<&'a UserDictionary>,
    reading: &'a str,
    initial_right_id: u16,
    bridge_reading_boundary: Option<usize>,
    nodes: &'a [Node],
    path: &'a [usize],
    generated: &'a [GeneratedSurface],
}

fn make_candidate(
    source: CandidateMaterialization<'_, '_>,
    cost: i64,
) -> Result<ConversionCandidate, ConversionError> {
    let CandidateMaterialization {
        dictionary,
        user_dictionary,
        reading,
        initial_right_id,
        bridge_reading_boundary,
        nodes,
        path,
        generated,
    } = source;
    let mut text = FixedStr::new();
    let mut annotation = FixedStr::new();
    let mut segments = FixedVec::new();
    let mut path_evidence = PathEvidence::default();
    let mut generated_day_suffix = false;
    let mut bridge_boundary_kind = None;
    let mut commit_bridge_tail = CommitBridgeTailStorage::default();
    let mut previous_right_id = initial_right_id;
    for index in path {
        let node = nodes[*index];
        let text_start = text.len();
        let flags = match node.surface {
            Surface::Dictionary { entry, .. } => entry.flags,
            Surface::User(index) => user_dictionary
                .and_then(|user| user.entry(index))
                .map_or(EntryFlags::NONE, |entry| entry.flags()),
            _ => EntryFlags::NONE,
        };
        path_evidence.add_surface(
            node.surface,
            flags.contains(EntryFlags::SPELLING_CORRECTION),
        );
        match node.surface {
            Surface::Dictionary { entry, .. } => {
                dictionary
                    .write_surface(entry, &mut text)
                    .map_err(|_| ConversionError::OutputTooLong)?;
                if annotation.is_empty() {
                    dictionary
                        .write_annotation(entry, &mut annotation)
                        .map_err(|_| ConversionError::OutputTooLong)?;
                }
            }
            Surface::User(index) => {
                let entry = user_dictionary
                    .and_then(|user| user.entry(index))
                    .ok_or(ConversionError::NoPath)?;
                text.push_str(&entry.surface)
                    .map_err(|_| ConversionError::OutputTooLong)?;
                if annotation.is_empty() && !entry.comment.is_empty() {
                    annotation
                        .push_str(&entry.comment)
                        .map_err(|_| ConversionError::OutputTooLong)?;
                }
            }
            Surface::Reading => text
                .push_str(&reading[node.start..node.end])
                .map_err(|_| ConversionError::OutputTooLong)?,
            Surface::Katakana => write_katakana(&reading[node.start..node.end], &mut text)?,
            Surface::Literal(value) => text
                .push_str(value)
                .map_err(|_| ConversionError::OutputTooLong)?,
            Surface::Generated(index) => {
                let surface = generated
                    .get(usize::from(index))
                    .ok_or(ConversionError::NoPath)?;
                generated_day_suffix |=
                    node.start > 0 && surface.counter == Some(NumericCounter::Day);
                text.push_str(surface.text.as_str())
                    .map_err(|_| ConversionError::OutputTooLong)?;
                if annotation.is_empty() && !surface.annotation.is_empty() {
                    annotation
                        .push_str(surface.annotation.as_str())
                        .map_err(|_| ConversionError::OutputTooLong)?;
                }
            }
        }
        let reading_start =
            u16::try_from(node.start).map_err(|_| ConversionError::ReadingTooLong)?;
        let reading_end = u16::try_from(node.end).map_err(|_| ConversionError::ReadingTooLong)?;
        if let Some(boundary) = bridge_reading_boundary {
            if node.start < boundary && node.end > boundary {
                bridge_boundary_kind = Some(BridgeBoundaryKind::SpanningEdge);
            } else if node.end == boundary && bridge_boundary_kind.is_none() {
                bridge_boundary_kind = Some(BridgeBoundaryKind::TypedFrontier);
            }
        }
        commit_bridge_tail = match node.surface {
            Surface::Dictionary { entry_index, .. } => {
                CommitBridgeTailStorage::new(entry_index, reading_start, previous_right_id)
            }
            _ => CommitBridgeTailStorage::default(),
        };
        previous_right_id = node.right_id;
        // Fuse this word into the previous segment when the dictionary's
        // segmenter table says no bunsetsu boundary separates them (e.g. an
        // ancillary 助動詞 after a verb).  Images without the table keep
        // one-word segments, exactly as before the table existed.
        let fuse = segments.last().is_some_and(|previous: &ConversionSegment| {
            dictionary.bunsetsu_boundary(previous.right_id, node.left_id) == Some(false)
        });
        let it_word = u8::from(flags.contains(EntryFlags::IT));
        if fuse {
            let last = segments.len() - 1;
            let previous = segments.get_mut(last).ok_or(ConversionError::NoPath)?;
            previous.reading_end =
                u16::try_from(node.end).map_err(|_| ConversionError::ReadingTooLong)?;
            previous.text_end =
                u16::try_from(text.len()).map_err(|_| ConversionError::OutputTooLong)?;
            previous.right_id = node.right_id;
            previous.flags = previous.flags | flags;
            previous.word_count = previous.word_count.saturating_add(1);
            previous.it_word_count = previous.it_word_count.saturating_add(it_word);
        } else {
            segments
                .push(ConversionSegment {
                    reading_start,
                    reading_end,
                    text_start: u16::try_from(text_start)
                        .map_err(|_| ConversionError::OutputTooLong)?,
                    text_end: u16::try_from(text.len())
                        .map_err(|_| ConversionError::OutputTooLong)?,
                    left_id: node.left_id,
                    right_id: node.right_id,
                    flags,
                    word_count: 1,
                    it_word_count: it_word,
                })
                .map_err(|_| ConversionError::TooManySegments)?;
        }
    }
    let system_entry_index = if path.len() == 1 {
        match nodes[path[0]].surface {
            Surface::Dictionary { entry_index, .. } => entry_index,
            _ => NO_SYSTEM_ENTRY_INDEX,
        }
    } else {
        NO_SYSTEM_ENTRY_INDEX
    };
    if !path_evidence.is_system_only() {
        commit_bridge_tail = CommitBridgeTailStorage::default();
    }
    Ok(ConversionCandidate {
        text,
        annotation,
        segments,
        system_entry_index,
        synthetic_exact: false,
        origin: CandidateOrigin::Direct,
        path_evidence,
        generated_day_suffix,
        bridge_boundary_kind,
        commit_bridge_tail,
        cross_commit_rescored: false,
        cost,
    })
}

fn write_katakana(
    reading: &str,
    output: &mut FixedStr<MAX_PREEDIT_BYTES>,
) -> Result<(), ConversionError> {
    for character in reading.chars() {
        let converted = match character {
            '\u{3041}'..='\u{3096}' => {
                char::from_u32(u32::from(character) + 0x60).unwrap_or(character)
            }
            _ => character,
        };
        TextSink::push(output, converted).map_err(|_| ConversionError::OutputTooLong)?;
    }
    Ok(())
}

fn synthetic_run_cost(base: i64, per_character: i64, characters: usize) -> i64 {
    base.saturating_add(per_character.saturating_mul(characters as i64))
}

#[cfg(test)]
#[path = "conversion_tests.rs"]
mod tests;
