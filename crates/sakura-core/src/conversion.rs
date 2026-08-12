//! Bounded lattice conversion with Viterbi top-1 and A* N-best search.
//!
//! The dictionary stays borrowed and mapped. All arenas are allocated once by
//! [`Converter::new`] and cleared between queries, so steady-state conversion
//! does not grow the heap. Every search is finite: lattice nodes, A* states,
//! candidates, and text are independently bounded.

use core::cmp::Ordering;
use std::collections::BinaryHeap;

use sakura_proto::{FixedStr, FixedVec, MAX_CANDIDATES, MAX_PREEDIT_BYTES, MAX_SEGMENTS};

use crate::dictionary::{Dictionary, Entry, EntryFlags};
use crate::editing::{identifier_into, IdentifierStyle};
use crate::user_dictionary::UserDictionary;
use crate::TextSink;

const NONE: usize = usize::MAX;
const NONE_STATE: u32 = u32::MAX;
const MAX_LATTICE_NODES: usize = 32_768;
const MAX_SEARCH_STATES: usize = 65_536;
const MAX_DICTIONARY_EDGES_PER_READING: usize = 12;
pub const MAX_CONVERSION_CANDIDATES: usize = MAX_CANDIDATES;
const GENERATED_IDENTIFIER_VARIANTS: usize = 4;
const FALLBACK_WORD_COST: i64 = 8_000;
const RUN_BASE_COST: i64 = 6_000;
const RUN_COST_PER_CHAR: i64 = 2_500;
const KATAKANA_BASE_COST: i64 = 7_000;
const KATAKANA_COST_PER_CHAR: i64 = 2_800;
const COUNTER_WORD_COST: i64 = 3_500;
const DEFAULT_NOUN_ID: u16 = 1_851;
const MIN_COMPLETION_COHERENCE_CHARS: usize = 4;
const COMPLETION_NODE_BUDGET: usize = 256;
const COMPLETION_ENTRY_BUDGET: usize = 64;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversionOptions {
    pub max_candidates: usize,
    /// Proportional reduction for entries tagged `IT`, in thousandths.
    pub it_bias_per_mille: u16,
    /// Absolute ceiling on the IT reduction, preserving base-cost precedence.
    pub max_it_boost: i32,
    /// Right connection class carried from the previous commit. Zero is the
    /// ordinary beginning-of-sentence class.
    pub initial_right_id: u16,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            max_candidates: MAX_CONVERSION_CANDIDATES,
            it_bias_per_mille: 100,
            max_it_boost: 800,
            initial_right_id: 0,
        }
    }
}

/// Explicit terminal condition for the bounded N-best search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionSearchTerminal {
    CandidateLimitReached,
    SearchExhausted,
    StateBudgetReached,
    LatticeBudgetReached,
}

/// Aggregate, text-free evidence about one conversion attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversionDiagnostics {
    pub terminal: ConversionSearchTerminal,
    pub states_pushed: usize,
    pub incoherent_prefixes_pruned: usize,
    pub lossless_fallback_inserted: bool,
}

/// Candidates and their bounded-search terminal condition.
#[derive(Debug)]
pub struct ConversionResult<'a> {
    candidates: &'a [ConversionCandidate],
    diagnostics: ConversionDiagnostics,
}

impl<'a> ConversionResult<'a> {
    pub fn candidates(&self) -> &'a [ConversionCandidate] {
        self.candidates
    }

    pub const fn diagnostics(&self) -> ConversionDiagnostics {
        self.diagnostics
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionCandidate {
    text: FixedStr<MAX_PREEDIT_BYTES>,
    annotation: FixedStr<MAX_PREEDIT_BYTES>,
    segments: FixedVec<ConversionSegment, MAX_SEGMENTS>,
    /// Exact system-dictionary provenance, present only for a one-edge system
    /// candidate. Composite and generated candidates deliberately have none.
    system_entry_index: Option<u32>,
    pub cost: i64,
}

/// One Viterbi path edge materialized as byte ranges in a candidate.
///
/// Reading and surface ranges let the engine focus and resize segments without
/// copying strings or losing every other boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConversionSegment {
    pub reading_start: u16,
    pub reading_end: u16,
    pub text_start: u16,
    pub text_end: u16,
    pub left_id: u16,
    pub right_id: u16,
    pub flags: EntryFlags,
}

impl ConversionCandidate {
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    pub fn annotation(&self) -> &str {
        self.annotation.as_str()
    }

    pub fn segments(&self) -> &[ConversionSegment] {
        self.segments.as_slice()
    }

    pub const fn system_entry_index(&self) -> Option<u32> {
        self.system_entry_index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionError {
    EmptyReading,
    ReadingTooLong,
    InvalidOptions,
    Dictionary(crate::dictionary::Error),
    LatticeFull,
    NoPath,
    OutputTooLong,
    TooManySegments,
}

impl core::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyReading => f.write_str("conversion reading is empty"),
            Self::ReadingTooLong => f.write_str("conversion reading exceeds the preedit limit"),
            Self::InvalidOptions => f.write_str("conversion options are outside their bounds"),
            Self::Dictionary(error) => write!(f, "dictionary lookup failed: {error}"),
            Self::LatticeFull => f.write_str("conversion lattice reached its fixed node limit"),
            Self::NoPath => f.write_str("conversion lattice has no complete path"),
            Self::OutputTooLong => f.write_str("converted output exceeds the preedit limit"),
            Self::TooManySegments => f.write_str("converted path exceeds the segment limit"),
        }
    }
}

impl std::error::Error for ConversionError {}

#[derive(Debug, Clone, Copy)]
enum Surface {
    Dictionary { entry: Entry, entry_index: u32 },
    User(usize),
    Reading,
    Katakana,
    Literal(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct Node {
    start: usize,
    end: usize,
    left_id: u16,
    right_id: u16,
    local_cost: i64,
    best_cost: i64,
    best_previous: usize,
    suffix_cost: i64,
    next_from_start: usize,
    next_at_end: usize,
    surface: Surface,
}

#[derive(Debug, Clone, Copy)]
struct SearchState {
    cost: i64,
    node: u32,
    parent: u32,
    class: PathClass,
    depth: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeapItem {
    estimate: i64,
    sequence: u64,
    state: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum PathClass {
    Neutral,
    Lexical,
    Reading,
    Katakana,
}

#[derive(Debug, Clone, Copy)]
struct SearchRun {
    terminal: ConversionSearchTerminal,
    states_pushed: usize,
    incoherent_prefixes_pruned: usize,
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .estimate
            .cmp(&self.estimate)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
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
    sequence: u64,
    initial_right_id: u16,
    lattice_node_budget: usize,
    search_state_budget: usize,
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
            candidates: Vec::with_capacity(
                MAX_CONVERSION_CANDIDATES + GENERATED_IDENTIFIER_VARIANTS,
            ),
            sequence: 0,
            initial_right_id: 0,
            lattice_node_budget: MAX_LATTICE_NODES,
            search_state_budget: MAX_SEARCH_STATES,
        }
    }

    /// Reduces the search budget only in test-support builds. Production always
    /// uses the fixed `MAX_SEARCH_STATES` arena.
    #[cfg(any(test, feature = "conversion-test-support"))]
    pub fn set_search_state_budget_for_test(&mut self, budget: usize) {
        self.search_state_budget = budget.min(MAX_SEARCH_STATES);
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
        Ok(self
            .convert_detailed(dictionary, reading, options)?
            .candidates())
    }

    pub fn convert_detailed<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        self.convert_with_user_dictionary_detailed(dictionary, None, reading, options)
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
        Ok(self
            .convert_with_user_dictionary_detailed(dictionary, user_dictionary, reading, options)?
            .candidates())
    }

    pub fn convert_with_user_dictionary_detailed<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
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

        self.reset(reading.len());
        self.initial_right_id = options.initial_right_id;
        let fallback = make_lossless_fallback(dictionary, reading, self.initial_right_id)?;
        let mut search = SearchRun {
            terminal: ConversionSearchTerminal::SearchExhausted,
            states_pushed: 0,
            incoherent_prefixes_pruned: 0,
        };
        match self.build_lattice(dictionary, user_dictionary, reading, options) {
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
                    self.add_identifier_variants(reading)?;
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
        debug_assert!(!self.candidates.is_empty());
        Ok(ConversionResult {
            candidates: &self.candidates,
            diagnostics: ConversionDiagnostics {
                terminal: search.terminal,
                states_pushed: search.states_pushed,
                incoherent_prefixes_pruned: search.incoherent_prefixes_pruned,
                lossless_fallback_inserted,
            },
        })
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

    /// A longer technical dictionary term is bounded evidence for the spelling
    /// of its already-converted prefix. This resolves compound homophones
    /// without manufacturing prefix entries or globally weakening Mozc costs.
    fn apply_it_completion_coherence(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<(), ConversionError> {
        if reading.chars().count() < MIN_COMPLETION_COHERENCE_CHARS
            || options.it_bias_per_mille == 0
            || options.max_it_boost == 0
        {
            return Ok(());
        }
        let boost = i64::from(options.max_it_boost);
        let mut boosted = 0u32;
        let mut failure = None;
        dictionary
            .visit_descendant_entries(
                reading,
                COMPLETION_NODE_BUDGET,
                COMPLETION_ENTRY_BUDGET,
                |entry| {
                    if !entry.flags.contains(EntryFlags::IT) {
                        return true;
                    }
                    let mut completion = FixedStr::<MAX_PREEDIT_BYTES>::new();
                    if let Err(error) = dictionary.write_surface(entry, &mut completion) {
                        failure = Some(error);
                        return false;
                    }
                    for (index, candidate) in self.candidates.iter_mut().enumerate() {
                        let bit = 1u32.checked_shl(u32::try_from(index).unwrap_or(u32::MAX));
                        if bit.is_none_or(|bit| boosted & bit != 0)
                            || completion.len() <= candidate.text().len()
                            || !completion.as_str().starts_with(candidate.text())
                        {
                            continue;
                        }
                        let bit = bit.unwrap_or(0);
                        candidate.cost = candidate.cost.saturating_sub(boost);
                        boosted |= bit;
                    }
                    true
                },
            )
            .map_err(ConversionError::Dictionary)?;
        if let Some(error) = failure {
            return Err(ConversionError::Dictionary(error));
        }
        Ok(())
    }

    fn add_identifier_variants(&mut self, reading: &str) -> Result<(), ConversionError> {
        let base_count = self.candidates.len();
        for base_index in 0..base_count {
            if self.candidates.len() >= MAX_CONVERSION_CANDIDATES + GENERATED_IDENTIFIER_VARIANTS {
                break;
            }
            let base = self.candidates[base_index].clone();
            for (style_index, style) in IdentifierStyle::ALL.into_iter().enumerate() {
                let mut text = FixedStr::new();
                if !identifier_into(base.text(), style, &mut text)
                    .map_err(|_| ConversionError::OutputTooLong)?
                    || text.as_str() == base.text()
                    || self
                        .candidates
                        .iter()
                        .any(|candidate| candidate.text() == text.as_str())
                {
                    continue;
                }
                let mut annotation = FixedStr::new();
                annotation
                    .push_str(style.annotation())
                    .map_err(|_| ConversionError::OutputTooLong)?;
                let mut segments = FixedVec::new();
                let first = base.segments().first().copied().unwrap_or_default();
                let last = base.segments().last().copied().unwrap_or(first);
                segments
                    .push(ConversionSegment {
                        reading_start: 0,
                        reading_end: u16::try_from(reading.len())
                            .map_err(|_| ConversionError::ReadingTooLong)?,
                        text_start: 0,
                        text_end: u16::try_from(text.len())
                            .map_err(|_| ConversionError::OutputTooLong)?,
                        left_id: first.left_id,
                        right_id: last.right_id,
                        flags: first.flags,
                    })
                    .map_err(|_| ConversionError::TooManySegments)?;
                self.candidates.push(ConversionCandidate {
                    text,
                    annotation,
                    segments,
                    system_entry_index: None,
                    cost: base
                        .cost
                        .saturating_add(100 + i64::try_from(style_index).unwrap_or(0)),
                });
            }
        }
        Ok(())
    }

    fn reset(&mut self, reading_len: usize) {
        self.nodes.clear();
        self.states.clear();
        self.queue.clear();
        self.path.clear();
        self.candidates.clear();
        self.starts_at[..=reading_len].fill(NONE);
        self.ends_at[..=reading_len].fill(NONE);
        self.sequence = 0;
    }

    fn build_lattice(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<(), ConversionError> {
        let synthetic_id = if dictionary.class_count() > usize::from(DEFAULT_NOUN_ID) {
            DEFAULT_NOUN_ID
        } else {
            0
        };
        let mut previous_class = None;
        for (start, character) in reading.char_indices() {
            let class = char_class(character);
            let run = char_run(reading, start);
            // A run of Latin letters is one token the user typed, not a phrase
            // to be segmented. A dictionary entry covering only part of it is
            // no evidence about the whole token, and joining two such entries
            // manufactures mixed-case nonsense: `llvm` used to convert to
            // `lLVM` (`l` + `LVM`) and `goto` to `GoTO` (`go` + `TO`), both
            // ahead of the reading itself. Entries that start at the token and
            // reach at least its end -- `gitlab`, `pytorch`, `microsoft365` --
            // are unaffected.
            let latin_token = class == CharClass::AsciiLetter;
            let at_token_start = previous_class != Some(CharClass::AsciiLetter);
            previous_class = Some(class);
            let mut last_length = 0usize;
            let mut candidates_for_length = 0usize;
            let mut failure = None;
            dictionary
                .common_prefix_search(&reading[start..], |matched| {
                    if latin_token && (!at_token_start || start + matched.matched_bytes < run.end) {
                        return true;
                    }
                    if matched.matched_bytes != last_length {
                        last_length = matched.matched_bytes;
                        candidates_for_length = 0;
                    }
                    if candidates_for_length >= MAX_DICTIONARY_EDGES_PER_READING {
                        return true;
                    }
                    candidates_for_length += 1;
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
                    if let Err(error) = self.add_node(
                        dictionary,
                        NodeSpec {
                            start,
                            end: start + matched.matched_bytes,
                            left_id: matched.entry.left_id,
                            right_id: matched.entry.right_id,
                            local_cost,
                            surface: Surface::Dictionary {
                                entry: matched.entry,
                                entry_index,
                            },
                        },
                    ) {
                        failure = Some(error);
                        return false;
                    }
                    true
                })
                .map_err(ConversionError::Dictionary)?;
            if let Some(error) = failure {
                return Err(error);
            }

            if let Some(user_dictionary) = user_dictionary {
                let mut user_candidates_for_length = 0usize;
                let mut last_user_length = 0usize;
                let mut user_failure = None;
                user_dictionary.common_prefix_search(
                    &reading[start..],
                    |matched_bytes, entry_index| {
                        if matched_bytes != last_user_length {
                            last_user_length = matched_bytes;
                            user_candidates_for_length = 0;
                        }
                        if user_candidates_for_length >= MAX_DICTIONARY_EDGES_PER_READING {
                            return true;
                        }
                        user_candidates_for_length += 1;
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
                        if let Err(error) = self.add_node(
                            dictionary,
                            NodeSpec {
                                start,
                                end: start + matched_bytes,
                                left_id,
                                right_id,
                                local_cost: i64::from(entry.word_cost()),
                                surface: Surface::User(entry_index),
                            },
                        ) {
                            user_failure = Some(error);
                            return false;
                        }
                        true
                    },
                );
                if let Some(error) = user_failure {
                    return Err(error);
                }
            }

            let character_end = start + character.len_utf8();
            self.add_node(
                dictionary,
                NodeSpec {
                    start,
                    end: character_end,
                    left_id: synthetic_id,
                    right_id: synthetic_id,
                    local_cost: FALLBACK_WORD_COST,
                    surface: Surface::Reading,
                },
            )?;

            if run.end > character_end {
                self.add_node(
                    dictionary,
                    NodeSpec {
                        start,
                        end: run.end,
                        left_id: synthetic_id,
                        right_id: synthetic_id,
                        local_cost: synthetic_run_cost(RUN_BASE_COST, RUN_COST_PER_CHAR, run.chars),
                        surface: Surface::Reading,
                    },
                )?;
            }
            if char_class(character) == CharClass::Hiragana && run.end > character_end {
                self.add_node(
                    dictionary,
                    NodeSpec {
                        start,
                        end: run.end,
                        left_id: synthetic_id,
                        right_id: synthetic_id,
                        local_cost: synthetic_run_cost(
                            KATAKANA_BASE_COST,
                            KATAKANA_COST_PER_CHAR,
                            run.chars,
                        ),
                        surface: Surface::Katakana,
                    },
                )?;
            }
            for (counter_reading, surface) in COUNTER_FORMS {
                if reading[start..].starts_with(counter_reading) {
                    self.add_node(
                        dictionary,
                        NodeSpec {
                            start,
                            end: start + counter_reading.len(),
                            left_id: synthetic_id,
                            right_id: synthetic_id,
                            local_cost: COUNTER_WORD_COST,
                            surface: Surface::Literal(surface),
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    fn add_node(
        &mut self,
        dictionary: &Dictionary<'_>,
        spec: NodeSpec,
    ) -> Result<(), ConversionError> {
        if self.nodes.len() >= self.lattice_node_budget {
            return Err(ConversionError::LatticeFull);
        }
        let (best_cost, best_previous) = if spec.start == 0 {
            (
                connection_cost(dictionary, self.initial_right_id, spec.left_id)
                    .saturating_add(spec.local_cost),
                NONE,
            )
        } else {
            let mut previous = self.ends_at[spec.start];
            let mut best_cost = i64::MAX;
            let mut best_previous = NONE;
            while previous != NONE {
                let prior = self.nodes[previous];
                let cost = prior
                    .best_cost
                    .saturating_add(connection_cost(dictionary, prior.right_id, spec.left_id))
                    .saturating_add(spec.local_cost);
                if cost < best_cost {
                    best_cost = cost;
                    best_previous = previous;
                }
                previous = prior.next_at_end;
            }
            if best_previous == NONE {
                return Ok(());
            }
            (best_cost, best_previous)
        };
        let index = self.nodes.len();
        self.nodes.push(Node {
            start: spec.start,
            end: spec.end,
            left_id: spec.left_id,
            right_id: spec.right_id,
            local_cost: spec.local_cost,
            best_cost,
            best_previous,
            suffix_cost: i64::MAX,
            next_from_start: self.starts_at[spec.start],
            next_at_end: self.ends_at[spec.end],
            surface: spec.surface,
        });
        self.starts_at[spec.start] = index;
        self.ends_at[spec.end] = index;
        Ok(())
    }

    fn compute_suffix_costs(&mut self, dictionary: &Dictionary<'_>, reading_len: usize) {
        for index in (0..self.nodes.len()).rev() {
            let node = self.nodes[index];
            self.nodes[index].suffix_cost = if node.end == reading_len {
                connection_cost(dictionary, node.right_id, 0)
            } else {
                let mut next = self.starts_at[node.end];
                let mut best = i64::MAX;
                while next != NONE {
                    let following = self.nodes[next];
                    if following.suffix_cost != i64::MAX {
                        let cost = connection_cost(dictionary, node.right_id, following.left_id)
                            .saturating_add(following.local_cost)
                            .saturating_add(following.suffix_cost);
                        best = best.min(cost);
                    }
                    next = following.next_from_start;
                }
                best
            };
        }
    }

    fn best_final_node(
        &self,
        dictionary: &Dictionary<'_>,
        reading_len: usize,
    ) -> Result<usize, ConversionError> {
        let mut current = self.ends_at[reading_len];
        let mut best = NONE;
        let mut best_cost = i64::MAX;
        while current != NONE {
            let node = self.nodes[current];
            let cost = node
                .best_cost
                .saturating_add(connection_cost(dictionary, node.right_id, 0));
            if cost < best_cost {
                best = current;
                best_cost = cost;
            }
            current = node.next_at_end;
        }
        (best != NONE)
            .then_some(best)
            .ok_or(ConversionError::NoPath)
    }

    fn build_viterbi_candidate(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        mut node: usize,
    ) -> Result<(), ConversionError> {
        self.path.clear();
        loop {
            self.path.push(node);
            let previous = self.nodes[node].best_previous;
            if previous == NONE {
                break;
            }
            node = previous;
        }
        self.path.reverse();
        let final_node = *self.path.last().ok_or(ConversionError::NoPath)?;
        let cost = self.nodes[final_node]
            .best_cost
            .saturating_add(connection_cost(
                dictionary,
                self.nodes[final_node].right_id,
                0,
            ));
        let candidate = make_candidate(
            dictionary,
            user_dictionary,
            reading,
            &self.nodes,
            &self.path,
            cost,
        )?;
        self.candidates.push(candidate);
        Ok(())
    }

    fn viterbi_path_is_coherent(&mut self, mut node: usize) -> Result<bool, ConversionError> {
        self.path.clear();
        loop {
            self.path.push(node);
            let previous = self.nodes[node].best_previous;
            if previous == NONE {
                break;
            }
            node = previous;
        }
        self.path.reverse();
        (!self.path.is_empty())
            .then(|| candidate_path_is_coherent(&self.nodes, &self.path))
            .ok_or(ConversionError::NoPath)
    }

    fn search_n_best(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        wanted: usize,
    ) -> Result<SearchRun, ConversionError> {
        let mut budget_reached = false;
        let mut incoherent_prefixes_pruned = 0usize;
        let mut node = self.starts_at[0];
        while node != NONE {
            let lattice_node = self.nodes[node];
            if lattice_node.suffix_cost != i64::MAX {
                let cost = connection_cost(dictionary, self.initial_right_id, lattice_node.left_id)
                    .saturating_add(lattice_node.local_cost);
                if !self.push_state(
                    node,
                    cost,
                    NONE_STATE,
                    cost.saturating_add(lattice_node.suffix_cost),
                    PathClass::Neutral
                        .extend(lattice_node.surface)
                        .expect("a first edge always defines a coherent class"),
                    1,
                ) {
                    budget_reached = true;
                }
            }
            node = lattice_node.next_from_start;
        }

        while let Some(item) = self.queue.pop() {
            let state = self.states[item.state as usize];
            let lattice_node = self.nodes[state.node as usize];
            if lattice_node.end == reading.len() {
                self.path.clear();
                let mut state_index = item.state;
                loop {
                    let path_state = self.states[state_index as usize];
                    self.path.push(path_state.node as usize);
                    if path_state.parent == NONE_STATE {
                        break;
                    }
                    state_index = path_state.parent;
                }
                self.path.reverse();
                if !candidate_path_is_coherent(&self.nodes, &self.path) {
                    continue;
                }
                let total = state.cost.saturating_add(connection_cost(
                    dictionary,
                    lattice_node.right_id,
                    0,
                ));
                // A later (costlier) path stitching together to more than
                // `MAX_PREEDIT_BYTES` is an unremarkable, expected outcome
                // once enough alternatives exist. Skipping it and continuing
                // the search for a smaller alternative keeps this in the same
                // "degrade gracefully" family as the lattice-node and
                // search-state budgets below.
                let candidate = match make_candidate(
                    dictionary,
                    user_dictionary,
                    reading,
                    &self.nodes,
                    &self.path,
                    total,
                ) {
                    Ok(candidate) => candidate,
                    Err(ConversionError::OutputTooLong | ConversionError::TooManySegments) => {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if self
                    .candidates
                    .iter()
                    .all(|existing| existing.text() != candidate.text())
                {
                    self.candidates.push(candidate);
                    if self.candidates.len() >= wanted {
                        return Ok(SearchRun {
                            terminal: ConversionSearchTerminal::CandidateLimitReached,
                            states_pushed: self.states.len(),
                            incoherent_prefixes_pruned,
                        });
                    }
                }
                continue;
            }

            if usize::from(state.depth) >= MAX_SEGMENTS {
                continue;
            }

            let mut next = self.starts_at[lattice_node.end];
            while next != NONE {
                let following = self.nodes[next];
                if following.suffix_cost != i64::MAX {
                    let Some(class) = state.class.extend(following.surface) else {
                        incoherent_prefixes_pruned = incoherent_prefixes_pruned.saturating_add(1);
                        next = following.next_from_start;
                        continue;
                    };
                    let cost = state
                        .cost
                        .saturating_add(connection_cost(
                            dictionary,
                            lattice_node.right_id,
                            following.left_id,
                        ))
                        .saturating_add(following.local_cost);
                    let estimate = cost.saturating_add(following.suffix_cost);
                    if !self.push_state(
                        next,
                        cost,
                        item.state,
                        estimate,
                        class,
                        state.depth.saturating_add(1),
                    ) {
                        budget_reached = true;
                    }
                }
                next = following.next_from_start;
            }
        }
        Ok(SearchRun {
            terminal: if budget_reached {
                ConversionSearchTerminal::StateBudgetReached
            } else {
                ConversionSearchTerminal::SearchExhausted
            },
            states_pushed: self.states.len(),
            incoherent_prefixes_pruned,
        })
    }

    fn push_state(
        &mut self,
        node: usize,
        cost: i64,
        parent: u32,
        estimate: i64,
        class: PathClass,
        depth: u8,
    ) -> bool {
        if self.states.len() >= self.search_state_budget {
            return false;
        }
        let Ok(node) = u32::try_from(node) else {
            return false;
        };
        let Ok(state) = u32::try_from(self.states.len()) else {
            return false;
        };
        self.states.push(SearchState {
            cost,
            node,
            parent,
            class,
            depth,
        });
        self.queue.push(HeapItem {
            estimate,
            sequence: self.sequence,
            state,
        });
        self.sequence = self.sequence.wrapping_add(1);
        true
    }
}

impl Default for Converter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct NodeSpec {
    start: usize,
    end: usize,
    left_id: u16,
    right_id: u16,
    local_cost: i64,
    surface: Surface,
}

fn connection_cost(dictionary: &Dictionary<'_>, right_id: u16, left_id: u16) -> i64 {
    i64::from(
        dictionary
            .connection_cost(right_id, left_id)
            .unwrap_or(u16::MAX),
    )
}

/// Reject a path that splices a fallback spelling into lexical dictionary
/// entries. A partial match is not evidence that the adjacent unmatched kana
/// belongs to the same word: otherwise an OOV reading such as `ぷろんふと`
/// produces candidates like `プロん富と`. A synthetic fallback must also use
/// one spelling consistently; partial hiragana/katakana mosaics are equally
/// unhelpful. Keep wholly lexical multiword paths (normal Japanese
/// segmentation) and wholly synthetic reading/katakana fallbacks, so
/// conversion always has a safe, lossless result.
fn candidate_path_is_coherent(nodes: &[Node], path: &[usize]) -> bool {
    let mut has_lexical_edge = false;
    let mut fallback = None;
    for &index in path {
        match nodes[index].surface {
            Surface::Dictionary { .. } | Surface::User(_) => has_lexical_edge = true,
            Surface::Reading => {
                if fallback.is_some_and(|surface| surface != SurfaceKind::Reading) {
                    return false;
                }
                fallback = Some(SurfaceKind::Reading);
            }
            Surface::Katakana => {
                if fallback.is_some_and(|surface| surface != SurfaceKind::Katakana) {
                    return false;
                }
                fallback = Some(SurfaceKind::Katakana);
            }
            Surface::Literal(_) => {}
        }
    }
    !(has_lexical_edge && fallback.is_some())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceKind {
    Reading,
    Katakana,
}

impl PathClass {
    fn extend(self, surface: Surface) -> Option<Self> {
        let next = match surface {
            Surface::Dictionary { .. } | Surface::User(_) => Self::Lexical,
            Surface::Reading => Self::Reading,
            Surface::Katakana => Self::Katakana,
            Surface::Literal(_) => Self::Neutral,
        };
        match (self, next) {
            (current, Self::Neutral) => Some(current),
            (Self::Neutral, next) => Some(next),
            (current, next) if current == next => Some(current),
            _ => None,
        }
    }
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
    let cost = connection_cost(dictionary, initial_right_id, synthetic_id)
        .saturating_add(local_cost)
        .saturating_add(connection_cost(dictionary, synthetic_id, 0));
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
        })
        .map_err(|_| ConversionError::TooManySegments)?;
    Ok(ConversionCandidate {
        text,
        annotation: FixedStr::new(),
        segments,
        system_entry_index: None,
        cost,
    })
}

fn make_candidate(
    dictionary: &Dictionary<'_>,
    user_dictionary: Option<&UserDictionary>,
    reading: &str,
    nodes: &[Node],
    path: &[usize],
    cost: i64,
) -> Result<ConversionCandidate, ConversionError> {
    let mut text = FixedStr::new();
    let mut annotation = FixedStr::new();
    let mut segments = FixedVec::new();
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
        }
        segments
            .push(ConversionSegment {
                reading_start: u16::try_from(node.start)
                    .map_err(|_| ConversionError::ReadingTooLong)?,
                reading_end: u16::try_from(node.end)
                    .map_err(|_| ConversionError::ReadingTooLong)?,
                text_start: u16::try_from(text_start)
                    .map_err(|_| ConversionError::OutputTooLong)?,
                text_end: u16::try_from(text.len()).map_err(|_| ConversionError::OutputTooLong)?,
                left_id: node.left_id,
                right_id: node.right_id,
                flags,
            })
            .map_err(|_| ConversionError::TooManySegments)?;
    }
    let system_entry_index = if path.len() == 1 {
        match nodes[path[0]].surface {
            Surface::Dictionary { entry_index, .. } => Some(entry_index),
            _ => None,
        }
    } else {
        None
    };
    Ok(ConversionCandidate {
        text,
        annotation,
        segments,
        system_entry_index,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Hiragana,
    Katakana,
    AsciiDigit,
    AsciiLetter,
    Other,
}

fn char_class(character: char) -> CharClass {
    match character {
        '\u{3040}'..='\u{309f}' | 'ー' => CharClass::Hiragana,
        '\u{30a0}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}' => CharClass::Katakana,
        '0'..='9' => CharClass::AsciiDigit,
        'A'..='Z' | 'a'..='z' => CharClass::AsciiLetter,
        _ => CharClass::Other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CharRun {
    end: usize,
    chars: usize,
}

fn char_run(reading: &str, start: usize) -> CharRun {
    let mut characters = reading[start..].char_indices();
    let Some((_, first)) = characters.next() else {
        return CharRun {
            end: start,
            chars: 0,
        };
    };
    let class = char_class(first);
    let mut end = start + first.len_utf8();
    let mut count = 1usize;
    for (relative, character) in characters {
        if char_class(character) != class {
            break;
        }
        end = start + relative + character.len_utf8();
        count += 1;
    }
    CharRun { end, chars: count }
}

fn synthetic_run_cost(base: i64, per_character: i64, characters: usize) -> i64 {
    base.saturating_add(per_character.saturating_mul(characters as i64))
}
