//! Bounded lattice conversion with Viterbi top-1 and A* N-best search.
//!
//! The dictionary stays borrowed and mapped. All arenas are allocated once by
//! [`Converter::new`] and cleared between queries, so steady-state conversion
//! does not grow the heap. Every search is finite: lattice nodes, A* states,
//! candidates, and text are independently bounded.

use core::cmp::Ordering;
use std::collections::BinaryHeap;

#[cfg(not(any(feature = "research-top32", feature = "research-wide-candidates")))]
use sakura_proto::MAX_CANDIDATES;
use sakura_proto::{FixedStr, FixedVec, MAX_PREEDIT_BYTES, MAX_SEGMENTS};

use crate::calendar::{date_offset_for_reading, date_surface_specs, CivilDate};
use crate::dictionary::{Dictionary, Entry, EntryFlags, SingleKanjiVariant};
use crate::input_repair::{
    allows_system_entry, collect_repair_variants, english_spelling_katakana_reading, RepairKind,
    COMMIT_HISTORY_PENALTY, ENGLISH_KATAKANA_PENALTY, MAX_REPAIR_VARIANTS,
};
use crate::numerals::{
    is_decorative_numeral_char, is_numeric_day_surface, parse_numeric_prefix,
    should_emit_numeric_span, NumericSpan, NUMERIC_STYLES,
};
use crate::preferences::ConversionMethod;
use crate::user_dictionary::UserDictionary;
use crate::width::PunctuationStyle;
use crate::TextSink;

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
/// Reading lengths, in characters, that split [`candidate_budget`] into its
/// three tiers. Kana readings only, so counted with `chars()`, not bytes.
const CANDIDATE_BUDGET_SHORT_READING_CHARS: usize = 4;
const CANDIDATE_BUDGET_MEDIUM_READING_CHARS: usize = 8;
/// Tier ceilings for [`candidate_budget`]. Deliberately independent of
/// [`MAX_CONVERSION_CANDIDATES`]: a research build may move that ceiling to
/// measure a wider limit, but the per-tier numbers below are measurements
/// against the shipping value of 256 and do not move with it.
const CANDIDATE_BUDGET_SHORT: usize = 256;
const CANDIDATE_BUDGET_MEDIUM: usize = 108;
const CANDIDATE_BUDGET_LONG: usize = 18;
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
// A bare spoken number still offers numeric forms, but common lexical
// homophones should win: せん -> 線, にじゅう -> 二重, さんぜん -> 産前.
const BARE_KANA_NUMBER_FORM_COST: i64 = 5_000;
const MAX_GENERATED_SURFACES: usize = 64;
const DEFAULT_NOUN_ID: u16 = 1_851;
/// Annotation for an appended character the pinned variant rules do not
/// relate to another. It exists so the tail reads as a character list
/// rather than as more ranked conversions.
const SINGLE_KANJI_ANNOTATION: &str = "単漢字";
const MIN_COMPLETION_COHERENCE_CHARS: usize = 4;
const COMPLETION_NODE_BUDGET: usize = 256;
const COMPLETION_ENTRY_BUDGET: usize = 64;
/// A word-sized reading is enough context to reward IT evidence without
/// changing the ranking of a standalone homophone. The bounded per-word
/// adjustment is deliberately smaller than an ordinary word cost and applies
/// only to wholly lexical paths. A reviewed whole-phrase IT entry counts as
/// one word; a compositional path can accumulate evidence from multiple words.
const MIN_IT_COMPOUND_READING_CHARS: usize = 7;
const IT_COMPOUND_WORD_BONUS: i64 = 1_200;
const MAX_IT_COMPOUND_BOOST: i64 = 2_400;
/// Once a trustworthy whole-reading entry exists, a much more expensive
/// all-system segmentation is usually a mosaic of individually valid short
/// words. Word-sized Japanese readings and atomic loanwords use this gate;
/// long Japanese compounds still retain legitimate split alternatives.
const EXACT_LEXICAL_COMPOSITE_COST_WINDOW: i64 = 4_000;
/// A one-character hiragana lead segment carries no lexical evidence of its
/// own: nothing in it says the user meant a phrase to start there. A path that
/// opens with one and then needs a whole kanji word to finish the reading is a
/// splice, not a parse, and those splices were burying real homophones on the
/// first candidate page -- たいあん offered た慰安 above 対案, and きかん put
/// き澗 ahead of 気管 and 旗艦 (Issue #94). This is the same rule as
/// `EXACT_LEXICAL_COMPOSITE_COST_WINDOW`, held much tighter for the one path
/// shape that is almost never a real segmentation. It stays a window rather
/// than a ban so a cheap splice that happens to spell a real word survives:
/// とじょう keeps と場 at +1190 over 途上.
const KANA_FRAGMENT_SPLIT_COST_WINDOW: i64 = 1_500;
/// Prefixes that genuinely attach to a following noun, so a path opening with
/// one is a parse after all: ご意見, お名前, み仏.
const KANA_PREFIX_MORPHEMES: [char; 3] = ['お', 'ご', 'み'];
const MAX_EXACT_WORD_READING_CHARS: usize = 6;
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

#[derive(Debug, Clone, Copy)]
struct DictionaryEdgeBudget {
    baseline_edges: usize,
    surface_count: usize,
    surface_ids: [u32; MAX_DICTIONARY_SURFACES_PER_READING],
}

impl DictionaryEdgeBudget {
    const fn new() -> Self {
        Self {
            baseline_edges: 0,
            surface_count: 0,
            surface_ids: [u32::MAX; MAX_DICTIONARY_SURFACES_PER_READING],
        }
    }

    fn reset(&mut self) {
        self.baseline_edges = 0;
        self.surface_count = 0;
    }

    fn admit(&mut self, surface_id: u32) -> bool {
        let known_surface = self.surface_ids[..self.surface_count].contains(&surface_id);
        if self.baseline_edges < BASE_DICTIONARY_EDGES_PER_READING {
            self.baseline_edges += 1;
            if !known_surface && self.surface_count < MAX_DICTIONARY_SURFACES_PER_READING {
                self.surface_ids[self.surface_count] = surface_id;
                self.surface_count += 1;
            }
            return true;
        }
        if known_surface || self.surface_count >= MAX_DICTIONARY_SURFACES_PER_READING {
            return false;
        }
        self.surface_ids[self.surface_count] = surface_id;
        self.surface_count += 1;
        true
    }
}

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

fn is_atomic_whole_reading_surface(surface: &str) -> bool {
    let mut characters = surface.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if characters.next().is_none() {
        return !first.is_whitespace();
    }
    surface.chars().all(|character| {
        ('\u{30a0}'..='\u{30ff}').contains(&character)
            || ('\u{ff65}'..='\u{ff9f}').contains(&character)
            || character.is_ascii_alphanumeric()
            || matches!(character, ' ' | '-' | '_' | '.' | '+' | '#' | '/')
    })
}

/// Whether `candidate` opens with a bare one-character hiragana fragment and
/// then spends a whole kanji or katakana word to finish the reading.
fn is_kana_fragment_prefix_split(candidate: &ConversionCandidate) -> bool {
    let segments = candidate.segments();
    let (Some(first), Some(second)) = (segments.first(), segments.get(1)) else {
        return false;
    };
    let text = candidate.text();
    let Some(lead) = text.get(usize::from(first.text_start)..usize::from(first.text_end)) else {
        return false;
    };
    let mut lead_characters = lead.chars();
    let (Some(lead_character), None) = (lead_characters.next(), lead_characters.next()) else {
        return false;
    };
    if !('\u{3041}'..='\u{3096}').contains(&lead_character)
        || KANA_PREFIX_MORPHEMES.contains(&lead_character)
    {
        return false;
    }
    let Some(rest) = text.get(usize::from(second.text_start)..usize::from(second.text_end)) else {
        return false;
    };
    rest.chars().next().is_some_and(|character| {
        matches!(char_class(character), CharClass::Katakana)
            || matches!(
                character,
                '\u{3400}'..='\u{4dbf}'
                    | '\u{4e00}'..='\u{9fff}'
                    | '\u{f900}'..='\u{faff}'
                    | '\u{20000}'..='\u{2ffff}'
            )
    })
}

fn is_trustworthy_exact_surface(surface: &str) -> bool {
    let mut characters = surface.chars();
    if characters.next().is_none() || characters.next().is_none() {
        return false;
    }
    is_atomic_whole_reading_surface(surface)
        || surface.chars().all(|character| {
            matches!(
                character,
                '\u{3400}'..='\u{4dbf}'
                    | '\u{4e00}'..='\u{9fff}'
                    | '\u{f900}'..='\u{faff}'
                    | '\u{20000}'..='\u{2ffff}'
            )
        })
}

/// How many candidates a request for `reading` may actually receive, no
/// matter how high the caller's `max_candidates` or
/// [`MAX_CONVERSION_CANDIDATES`] itself goes.
///
/// Issue #95 raised [`MAX_CONVERSION_CANDIDATES`] from 18 to 256 so a short
/// reading could reach the single-kanji and homophone surfaces the old
/// ceiling trimmed away. Benchmarking that change on the shipped dictionary
/// (`tools/candidate-sweep`) showed the benefit is confined to short
/// readings, while the p95 latency cost of a wide list is not (p95 per
/// reading, shipped dictionary):
///
/// | reading length | limit 18  | limit 256 | single kanji / homophone gained |
/// |-----------------|----------:|----------:|-----------------------------------|
/// | 1-4 chars       |    162 us |  1,638 us | yes -- all of it                  |
/// | 5-8 chars       |    595 us |  3,002 us | none                               |
/// | 29 chars        |  1,674 us | 11,458 us | none                               |
/// | 93 chars        |  5,412 us | 36,379 us | none                               |
/// | 221 chars       | 59,984 us | 50,832 us | none (`MAX_SEARCH_STATES` saturates; only 3 candidates come out at any limit) |
/// | 477 chars       | 62,024 us | 62,286 us | none                               |
///
/// A wide list only ever pays for itself on a short reading. Beyond a
/// handful of characters the extra candidates are alternate whole-sentence
/// parses nobody pages through, bought with tens of milliseconds of added
/// Space-key latency -- conversion has no time budget in code, so this is
/// purely about what a user perceives while typing. Hence three tiers
/// instead of one global ceiling: short readings keep the full budget, long
/// readings keep the original pre-#95 bound, and medium readings sit at a
/// compromise between the two.
pub fn candidate_budget(reading: &str) -> usize {
    let chars = reading.chars().count();
    if chars <= CANDIDATE_BUDGET_SHORT_READING_CHARS {
        CANDIDATE_BUDGET_SHORT
    } else if chars <= CANDIDATE_BUDGET_MEDIUM_READING_CHARS {
        CANDIDATE_BUDGET_MEDIUM
    } else {
        CANDIDATE_BUDGET_LONG
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversionOptions {
    pub max_candidates: usize,
    /// Whether candidates may contain several bunsetsu segments.
    pub method: ConversionMethod,
    /// Proportional reduction for entries tagged `IT`, in thousandths.
    pub it_bias_per_mille: u16,
    /// Absolute ceiling on the IT reduction, preserving base-cost precedence.
    pub max_it_boost: i32,
    /// Right connection class carried from the previous commit. Zero is the
    /// ordinary beginning-of-sentence class.
    pub initial_right_id: u16,
    /// ATOK-style input assistance applied while building the lattice.
    pub input_support: crate::preferences::InputSupport,
    /// When true, skip every repair / English-spelling edge. Used after the
    /// user rejects an automatic repair by resizing segments.
    pub skip_input_repair: bool,
    /// Independent aggregate budgets for the optional sequential raw-repair
    /// passes. Ordinary direct conversion does not consume these budgets.
    pub raw_repair_budget: RawRepairBudget,
    /// The reader's configured punctuation marks. The converter uses this
    /// only to decide which member of a punctuation family it offers first
    /// (Issue #99); it never rewrites a surface to match, which stays the
    /// width choke point's job.
    pub punctuation: PunctuationStyle,
}

/// Typed sides of one connection-matrix lookup. Keeping them distinct at the
/// bridge boundary prevents a previous terminal right ID from being mistaken
/// for the left ID of a word that actually starts before the commit boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeftContextId(u16);

impl LeftContextId {
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RightContextId(u16);

impl RightContextId {
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u16 {
        self.0
    }
}

/// A bounded, volatile tail of the immediately preceding committed
/// conversion. `prefix_cost` is the selected tail path from
/// `prefix_right_id` through its final lexical edge, excluding its old EOS
/// connection. A combined tail+current path can therefore be normalized back
/// into the same cost domain as an ordinary current-only candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrossCommitBridge<'a> {
    pub tail_reading: &'a str,
    pub tail_surface: &'a str,
    pub prefix_right_id: RightContextId,
    pub prefix_cost: i64,
}

/// Exact final system edge retained from the selected raw lattice path.
/// Ranges end at the selected candidate's reading/surface end, so only their
/// starts need to be carried. The engine slices them while it still owns the
/// exact commit strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitBridgeTail {
    pub reading_start: u16,
    pub text_start: u16,
    pub prefix_right_id: RightContextId,
    pub prefix_cost: i64,
}

const NO_COMMIT_BRIDGE_ENTRY: u32 = u32::MAX;
const NO_SYSTEM_ENTRY_INDEX: u32 = u32::MAX;

/// Compact identity for the final raw system edge. Keeping only eight bytes
/// in each candidate preserves the 128 KiB conversion-worker stack contract;
/// the mapped dictionary materializes its surface and cost on commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommitBridgeTailStorage {
    entry_index: u32,
    reading_start: u16,
    prefix_right_id: u16,
}

impl CommitBridgeTailStorage {
    const fn new(entry_index: u32, reading_start: u16, prefix_right_id: u16) -> Self {
        Self {
            entry_index,
            reading_start,
            prefix_right_id,
        }
    }
}

impl Default for CommitBridgeTailStorage {
    fn default() -> Self {
        Self {
            entry_index: NO_COMMIT_BRIDGE_ENTRY,
            reading_start: 0,
            prefix_right_id: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BridgeBoundaryKind {
    SpanningEdge,
    TypedFrontier,
}

/// Aggregate limits across all corrected readings in one raw-repair request.
/// Per-pass lattice/search limits are reset by the ordinary converter, so the
/// raw API applies these counters across the whole sequence as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawRepairBudget {
    pub max_corrected_passes: usize,
    pub max_repair_candidates: usize,
    pub max_lattice_nodes: usize,
    pub max_search_states: usize,
}

impl Default for RawRepairBudget {
    fn default() -> Self {
        Self {
            max_corrected_passes: DEFAULT_MAX_RAW_REPAIR_PASSES,
            max_repair_candidates: DEFAULT_MAX_RAW_REPAIR_CANDIDATES,
            max_lattice_nodes: DEFAULT_MAX_RAW_REPAIR_LATTICE_NODES,
            max_search_states: DEFAULT_MAX_RAW_REPAIR_SEARCH_STATES,
        }
    }
}

/// The authority of a candidate source.  The numeric rank is intentionally
/// explicit: a lower-cost repair must never displace a direct candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateAuthority {
    Direct,
    LocalRawCompletion,
    GeneralSingleInsertion,
}

impl CandidateAuthority {
    pub const fn rank(self) -> u8 {
        match self {
            Self::Direct => 3,
            Self::LocalRawCompletion => 2,
            Self::GeneralSingleInsertion => 1,
        }
    }
}

/// The repair tier attached to a raw-repair plan and its accepted candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairTier {
    LocalCompletion,
    GeneralSingleInsertion,
}

impl RepairTier {
    pub const fn authority(self) -> CandidateAuthority {
        match self {
            Self::LocalCompletion => CandidateAuthority::LocalRawCompletion,
            Self::GeneralSingleInsertion => CandidateAuthority::GeneralSingleInsertion,
        }
    }
}

/// Provenance of a materialized candidate.  This is kept in the core object,
/// rather than inferred later from its surface or cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateOrigin {
    Direct,
    RawRepair { plan_id: u8, tier: RepairTier },
}

impl CandidateOrigin {
    pub const fn authority(self) -> CandidateAuthority {
        match self {
            Self::Direct => CandidateAuthority::Direct,
            Self::RawRepair { tier, .. } => tier.authority(),
        }
    }
}

/// Text-free path evidence used by the raw-repair admission gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PathEvidence {
    pub system_edges: u8,
    pub user_edges: u8,
    pub fallback_edges: u8,
    pub generated_edges: u8,
    pub spelling_edges: u8,
    repair_kinds: u8,
}

impl PathEvidence {
    pub const fn is_system_only(self) -> bool {
        self.system_edges > 0
            && self.user_edges == 0
            && self.fallback_edges == 0
            && self.generated_edges == 0
            && self.spelling_edges == 0
            && self.repair_kinds == 0
    }

    fn add_system(&mut self, spelling: bool) {
        if spelling {
            self.spelling_edges = self.spelling_edges.saturating_add(1);
        } else {
            self.system_edges = self.system_edges.saturating_add(1);
        }
    }

    pub const fn has_unconfirmed_repair(self) -> bool {
        self.repair_kinds
            & (repair_kind_bit(RepairKind::Rule)
                | repair_kind_bit(RepairKind::Advanced)
                | repair_kind_bit(RepairKind::EnglishSpelling))
            != 0
    }

    pub const fn has_repair_kind(self, kind: RepairKind) -> bool {
        self.repair_kinds & repair_kind_bit(kind) != 0
    }

    fn add_surface(&mut self, surface: Surface, spelling: bool) {
        match surface {
            Surface::Dictionary {
                repair: Some(kind), ..
            } => self.repair_kinds |= repair_kind_bit(kind),
            Surface::Dictionary { repair: None, .. } => self.add_system(spelling),
            Surface::User(_) => self.user_edges = self.user_edges.saturating_add(1),
            Surface::Reading | Surface::Katakana => {
                self.fallback_edges = self.fallback_edges.saturating_add(1)
            }
            Surface::Literal(_) | Surface::Generated(_) => {
                self.generated_edges = self.generated_edges.saturating_add(1)
            }
        }
    }
}

const fn repair_kind_bit(kind: RepairKind) -> u8 {
    match kind {
        RepairKind::Rule => 1 << 0,
        RepairKind::Advanced => 1 << 1,
        RepairKind::EnglishSpelling => 1 << 2,
        RepairKind::CommitHistory => 1 << 3,
    }
}

/// A single forward edit run.  No reverse/inferred mapping is exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionRunKind {
    Equal,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorrectionRun {
    pub corrected_start: u16,
    pub corrected_end: u16,
    pub original_start: u16,
    pub original_end: u16,
    pub kind: CorrectionRunKind,
}

impl CorrectionRun {
    pub const fn equal(
        corrected_start: u16,
        corrected_end: u16,
        original_start: u16,
        original_end: u16,
    ) -> Self {
        Self {
            corrected_start,
            corrected_end,
            original_start,
            original_end,
            kind: CorrectionRunKind::Equal,
        }
    }

    pub const fn replace(
        corrected_start: u16,
        corrected_end: u16,
        original_start: u16,
        original_end: u16,
    ) -> Self {
        Self {
            corrected_start,
            corrected_end,
            original_start,
            original_end,
            kind: CorrectionRunKind::Replace,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionMapError {
    TooManyRuns,
    EmptyRun,
    NonContiguous,
    InvalidEndpoint,
    InvalidUtf8Boundary,
    EqualRunMismatch,
    ReplaceRunUnchanged,
    SnapshotMismatch,
    LengthOverflow,
}

impl core::fmt::Display for CorrectionMapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::TooManyRuns => "correction map has too many runs",
            Self::EmptyRun => "correction map contains an empty run",
            Self::NonContiguous => "correction map runs are not contiguous",
            Self::InvalidEndpoint => "correction map endpoint is out of range",
            Self::InvalidUtf8Boundary => "correction map endpoint is not a UTF-8 boundary",
            Self::EqualRunMismatch => "equal correction run does not contain equal text",
            Self::ReplaceRunUnchanged => "replace correction run contains unchanged text",
            Self::SnapshotMismatch => "correction map snapshot does not match the request",
            Self::LengthOverflow => "correction map length exceeds the u16 boundary type",
        };
        f.write_str(text)
    }
}

impl std::error::Error for CorrectionMapError {}

/// A bounded, heap-backed forward map from corrected-reading ranges to the
/// original raw-reading ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionMap {
    runs: Vec<CorrectionRun>,
    original_snapshot: String,
    corrected_snapshot: String,
    original_len: u16,
    corrected_len: u16,
}

impl CorrectionMap {
    pub fn new(
        original: &str,
        corrected: &str,
        runs: &[CorrectionRun],
    ) -> Result<Self, CorrectionMapError> {
        if original.len() > MAX_PREEDIT_BYTES || corrected.len() > MAX_PREEDIT_BYTES {
            return Err(CorrectionMapError::LengthOverflow);
        }
        let original_len =
            u16::try_from(original.len()).map_err(|_| CorrectionMapError::LengthOverflow)?;
        let corrected_len =
            u16::try_from(corrected.len()).map_err(|_| CorrectionMapError::LengthOverflow)?;
        if runs.len() > MAX_CORRECTION_RUNS {
            return Err(CorrectionMapError::TooManyRuns);
        }
        let mut previous_corrected = 0u16;
        let mut previous_original = 0u16;
        for run in runs {
            if run.corrected_start != previous_corrected || run.original_start != previous_original
            {
                return Err(CorrectionMapError::NonContiguous);
            }
            if run.corrected_end > corrected_len || run.original_end > original_len {
                return Err(CorrectionMapError::InvalidEndpoint);
            }
            if run.corrected_start >= run.corrected_end || run.original_start >= run.original_end {
                return Err(CorrectionMapError::EmptyRun);
            }
            let corrected_start = usize::from(run.corrected_start);
            let corrected_end = usize::from(run.corrected_end);
            let original_start = usize::from(run.original_start);
            let original_end = usize::from(run.original_end);
            if !corrected.is_char_boundary(corrected_start)
                || !corrected.is_char_boundary(corrected_end)
                || !original.is_char_boundary(original_start)
                || !original.is_char_boundary(original_end)
            {
                return Err(CorrectionMapError::InvalidUtf8Boundary);
            }
            match run.kind {
                CorrectionRunKind::Equal => {
                    if original[original_start..original_end]
                        != corrected[corrected_start..corrected_end]
                    {
                        return Err(CorrectionMapError::EqualRunMismatch);
                    }
                }
                CorrectionRunKind::Replace => {
                    if original[original_start..original_end]
                        == corrected[corrected_start..corrected_end]
                    {
                        return Err(CorrectionMapError::ReplaceRunUnchanged);
                    }
                }
            }
            previous_corrected = run.corrected_end;
            previous_original = run.original_end;
        }
        if previous_corrected != corrected_len || previous_original != original_len {
            return Err(CorrectionMapError::NonContiguous);
        }
        Ok(Self {
            runs: runs.to_vec(),
            original_snapshot: original.to_owned(),
            corrected_snapshot: corrected.to_owned(),
            original_len,
            corrected_len,
        })
    }

    pub const fn original_len(&self) -> u16 {
        self.original_len
    }

    pub const fn corrected_len(&self) -> u16 {
        self.corrected_len
    }

    pub fn runs(&self) -> &[CorrectionRun] {
        &self.runs
    }

    /// Re-validates the map against the exact snapshot used by a conversion
    /// pass.  Length equality alone is insufficient: an equal run created for
    /// a different same-length reading must fail closed.
    pub fn validate_for_readings(
        &self,
        original: &str,
        corrected: &str,
    ) -> Result<(), CorrectionMapError> {
        if self.original_len as usize != original.len()
            || self.corrected_len as usize != corrected.len()
        {
            return Err(CorrectionMapError::InvalidEndpoint);
        }
        let mut previous_corrected = 0u16;
        let mut previous_original = 0u16;
        for run in &self.runs {
            if run.corrected_start != previous_corrected || run.original_start != previous_original
            {
                return Err(CorrectionMapError::NonContiguous);
            }
            let corrected_start = usize::from(run.corrected_start);
            let corrected_end = usize::from(run.corrected_end);
            let original_start = usize::from(run.original_start);
            let original_end = usize::from(run.original_end);
            if run.corrected_start >= run.corrected_end || run.original_start >= run.original_end {
                return Err(CorrectionMapError::EmptyRun);
            }
            if !corrected.is_char_boundary(corrected_start)
                || !corrected.is_char_boundary(corrected_end)
                || !original.is_char_boundary(original_start)
                || !original.is_char_boundary(original_end)
            {
                return Err(CorrectionMapError::InvalidUtf8Boundary);
            }
            match run.kind {
                CorrectionRunKind::Equal
                    if original[original_start..original_end]
                        != corrected[corrected_start..corrected_end] =>
                {
                    return Err(CorrectionMapError::EqualRunMismatch);
                }
                CorrectionRunKind::Replace
                    if original[original_start..original_end]
                        == corrected[corrected_start..corrected_end] =>
                {
                    return Err(CorrectionMapError::ReplaceRunUnchanged);
                }
                _ => {}
            }
            previous_corrected = run.corrected_end;
            previous_original = run.original_end;
        }
        if previous_corrected != self.corrected_len || previous_original != self.original_len {
            return Err(CorrectionMapError::NonContiguous);
        }
        if original != self.original_snapshot || corrected != self.corrected_snapshot {
            return Err(CorrectionMapError::SnapshotMismatch);
        }
        Ok(())
    }

    /// Projects a corrected range only when both endpoints have an exact
    /// forward boundary.  A boundary inside a replacement run is rejected.
    pub fn project_corrected_range(&self, start: u16, end: u16) -> Option<(u16, u16)> {
        if start > end || end > self.corrected_len {
            return None;
        }
        if !self.corrected_snapshot.is_char_boundary(usize::from(start))
            || !self.corrected_snapshot.is_char_boundary(usize::from(end))
        {
            return None;
        }
        let original_start = self.project_boundary(start)?;
        let original_end = self.project_boundary(end)?;
        (original_start <= original_end).then_some((original_start, original_end))
    }

    fn project_boundary(&self, boundary: u16) -> Option<u16> {
        for run in &self.runs {
            if boundary == run.corrected_start {
                return Some(run.original_start);
            }
            if boundary == run.corrected_end {
                return Some(run.original_end);
            }
            if run.kind == CorrectionRunKind::Equal
                && boundary > run.corrected_start
                && boundary < run.corrected_end
            {
                let offset = boundary - run.corrected_start;
                let original_end = run.original_end - run.original_start;
                if offset <= original_end {
                    return Some(run.original_start + offset);
                }
            }
        }
        None
    }
}

/// One corrected-reading pass.  The map is validated before a plan is
/// accepted, and the plan owns only bounded heap data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRepairPlan {
    plan_id: u8,
    corrected_reading: String,
    map: CorrectionMap,
    tier: RepairTier,
}

impl RawRepairPlan {
    pub fn new(
        plan_id: u8,
        corrected_reading: &str,
        map: CorrectionMap,
        tier: RepairTier,
    ) -> Result<Self, CorrectionMapError> {
        if corrected_reading.len() > MAX_PREEDIT_BYTES
            || usize::from(map.corrected_len()) != corrected_reading.len()
            || map.corrected_snapshot != corrected_reading
        {
            return Err(CorrectionMapError::InvalidEndpoint);
        }
        Ok(Self {
            plan_id,
            corrected_reading: corrected_reading.to_owned(),
            map,
            tier,
        })
    }

    pub const fn plan_id(&self) -> u8 {
        self.plan_id
    }

    pub fn corrected_reading(&self) -> &str {
        &self.corrected_reading
    }

    pub const fn map(&self) -> &CorrectionMap {
        &self.map
    }

    pub const fn tier(&self) -> RepairTier {
        self.tier
    }

    fn is_valid_for(&self, original_reading: &str) -> bool {
        self.map.original_len() as usize == original_reading.len()
            && self.map.corrected_len() as usize == self.corrected_reading.len()
    }
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            max_candidates: MAX_CONVERSION_CANDIDATES,
            method: ConversionMethod::MultiSegment,
            it_bias_per_mille: 100,
            max_it_boost: 800,
            initial_right_id: 0,
            input_support: crate::preferences::InputSupport::default(),
            skip_input_repair: false,
            raw_repair_budget: RawRepairBudget::default(),
            punctuation: PunctuationStyle::default(),
        }
    }
}

/// How the converter treats the caller-supplied literal surface.
///
/// `Ranked` is the ordinary N-best path.  The two exact policies are
/// deliberately explicit: they bypass inference paths that could otherwise
/// rewrite an opaque token or an unresolved Latin fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiteralPolicy {
    #[default]
    Ranked,
    ExactTop1,
    ExactOnly,
}

/// The caller's classification for one conversion request.
///
/// The class and [`LiteralPolicy`] form a checked pair. Keeping the class in
/// the conversion input makes the policy boundary visible to every consumer
/// instead of relying on a convention around a raw reading string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionInputClass {
    Ordinary,
    OpaqueAsciiIdentifier,
    MixedUnresolvedLatin,
}

/// A conversion lookup reading together with the literal surface the user
/// typed before lookup normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversionInput<'a> {
    pub lookup_reading: &'a str,
    pub exact_surface: &'a str,
    pub class: ConversionInputClass,
    pub literal_policy: LiteralPolicy,
}

impl<'a> ConversionInput<'a> {
    /// Preserves the legacy conversion contract for callers that supply only a
    /// reading: ordinary lookup and normal cost ranking.
    pub const fn ordinary(reading: &'a str) -> Self {
        Self {
            lookup_reading: reading,
            exact_surface: reading,
            class: ConversionInputClass::Ordinary,
            literal_policy: LiteralPolicy::Ranked,
        }
    }

    pub const fn new(
        lookup_reading: &'a str,
        exact_surface: &'a str,
        class: ConversionInputClass,
        literal_policy: LiteralPolicy,
    ) -> Self {
        Self {
            lookup_reading,
            exact_surface,
            class,
            literal_policy,
        }
    }

    fn validate(self) -> Result<(), ConversionError> {
        if self.lookup_reading.is_empty() {
            return Err(ConversionError::EmptyReading);
        }
        if self.lookup_reading.len() > MAX_PREEDIT_BYTES {
            return Err(ConversionError::ReadingTooLong);
        }
        if self.exact_surface.is_empty() || self.exact_surface.len() > MAX_PREEDIT_BYTES {
            return Err(ConversionError::InvalidOptions);
        }

        match (self.class, self.literal_policy) {
            (ConversionInputClass::Ordinary, LiteralPolicy::Ranked) => Ok(()),
            (ConversionInputClass::OpaqueAsciiIdentifier, LiteralPolicy::ExactTop1) => {
                if self.lookup_reading.len() != self.exact_surface.len()
                    || !self.lookup_reading.eq_ignore_ascii_case(self.exact_surface)
                    || !is_ascii_alpha_digit_identifier(self.lookup_reading)
                    || !is_ascii_alpha_digit_identifier(self.exact_surface)
                {
                    return Err(ConversionError::InvalidOptions);
                }
                Ok(())
            }
            (ConversionInputClass::MixedUnresolvedLatin, LiteralPolicy::ExactOnly) => {
                if self.lookup_reading != self.exact_surface
                    || !is_mixed_unresolved_latin(self.lookup_reading)
                    || !is_mixed_unresolved_latin(self.exact_surface)
                {
                    return Err(ConversionError::InvalidOptions);
                }
                Ok(())
            }
            _ => Err(ConversionError::InvalidOptions),
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
    /// Number of lattice nodes materialized for this pass.
    pub lattice_nodes: usize,
    pub states_pushed: usize,
    pub incoherent_prefixes_pruned: usize,
    pub lossless_fallback_inserted: bool,
    /// Number of corrected passes that were actually attempted by the
    /// one-slot raw-repair API. Ordinary conversion leaves this at zero.
    pub raw_repair_passes: usize,
    /// Number of raw-repair candidates admitted after the source/evidence
    /// gate. Direct candidates are not included.
    pub raw_repair_candidates_added: usize,
    /// Number of candidate objects materialized by corrected passes before
    /// dedupe and evidence filtering. This is the aggregate candidate-budget
    /// consumption, including candidates that were later rejected.
    pub raw_repair_candidates_examined: usize,
    /// Number of plans/candidates rejected by the bounded raw-repair gate.
    pub raw_repair_candidates_rejected: usize,
    /// Aggregate lattice/search consumption across corrected passes only.
    pub raw_repair_lattice_nodes: usize,
    pub raw_repair_search_states: usize,
    /// Whether a validated bounded tail was replayed with the current reading.
    pub cross_commit_bridge_attempted: bool,
    /// Combined candidates inspected before exact surface/right-ID matching.
    pub cross_commit_bridge_candidates_examined: usize,
    /// Ordinary current-only candidates whose cost was improved by the
    /// combined lexical evidence.
    pub cross_commit_bridge_candidates_rescored: usize,
    /// Combined paths backed by a raw dictionary edge spanning the commit.
    pub cross_commit_bridge_spanning_paths: usize,
    /// Combined paths whose raw edges end exactly at the commit, carrying an
    /// alternative typed terminal state together with its retained cost delta.
    pub cross_commit_bridge_frontier_paths: usize,
    pub cross_commit_bridge_lattice_nodes: usize,
    pub cross_commit_bridge_search_states: usize,
    pub cross_commit_bridge_terminal: Option<ConversionSearchTerminal>,
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
    system_entry_index: u32,
    /// Set only for a literal candidate injected by an exact literal policy.
    /// It is distinct from the ordinary lossless fallback so callers can
    /// prevent an emergency/raw-preservation result from entering learning.
    synthetic_exact: bool,
    origin: CandidateOrigin,
    path_evidence: PathEvidence,
    /// Folded from raw path edges while the optional combined pass knows its
    /// exact reading boundary. Display bunsetsu fusion cannot forge it.
    bridge_boundary_kind: Option<BridgeBoundaryKind>,
    /// Exact identity of the final raw system edge, kept separately from the
    /// fused display segments and materialized only if this candidate commits.
    commit_bridge_tail: CommitBridgeTailStorage,
    /// A contextual result is never reused as cost evidence for a later
    /// bridge. This one-bit terminal marker avoids retaining another i64 cost
    /// in every candidate.
    cross_commit_rescored: bool,
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
    /// How many dictionary path words this segment covers. Bunsetsu fusion
    /// OR-merges `flags`, which would otherwise let one flagged word count
    /// as the whole fused segment in per-word statistics.
    pub word_count: u8,
    /// How many of those words carried [`EntryFlags::IT`] on their own.
    pub it_word_count: u8,
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
        if self.system_entry_index == NO_SYSTEM_ENTRY_INDEX {
            None
        } else {
            Some(self.system_entry_index)
        }
    }

    /// Whether this candidate was injected from
    /// [`ConversionInput::exact_surface`] by an exact literal policy.
    pub const fn is_synthetic_exact(&self) -> bool {
        self.synthetic_exact
    }

    pub const fn origin(&self) -> CandidateOrigin {
        self.origin
    }

    pub const fn authority(&self) -> CandidateAuthority {
        self.origin.authority()
    }

    pub const fn path_evidence(&self) -> PathEvidence {
        self.path_evidence
    }

    pub const fn was_cross_commit_rescored(&self) -> bool {
        self.cross_commit_rescored
    }

    /// Materializes bounded bridge evidence from the exact final raw edge.
    /// Surface text is verified against the selected candidate; no later
    /// surface lookup is used to guess dictionary provenance.
    pub fn commit_bridge_tail(&self, dictionary: &Dictionary<'_>) -> Option<CommitBridgeTail> {
        if self.origin != CandidateOrigin::Direct
            || self.synthetic_exact
            || !self.path_evidence.is_system_only()
            || self.commit_bridge_tail.entry_index == NO_COMMIT_BRIDGE_ENTRY
        {
            return None;
        }
        let entry = dictionary
            .entry_at(self.commit_bridge_tail.entry_index as usize)
            .ok()?;
        if self.segments.last()?.right_id != entry.right_id {
            return None;
        }
        let mut final_surface = FixedStr::<MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES>::new();
        dictionary.write_surface(entry, &mut final_surface).ok()?;
        let prefix = self.text().strip_suffix(final_surface.as_str())?;
        let text_start = u16::try_from(prefix.len()).ok()?;
        let connection = connection_cost(
            dictionary,
            RightContextId::new(self.commit_bridge_tail.prefix_right_id),
            LeftContextId::new(entry.left_id),
        );
        let prefix_cost = connection + i64::from(entry.word_cost);
        Some(CommitBridgeTail {
            reading_start: self.commit_bridge_tail.reading_start,
            text_start,
            prefix_right_id: RightContextId::new(self.commit_bridge_tail.prefix_right_id),
            prefix_cost,
        })
    }

    fn bridge_boundary_kind(&self) -> Option<BridgeBoundaryKind> {
        if self.path_evidence.is_system_only() {
            self.bridge_boundary_kind
        } else {
            None
        }
    }

    /// A corrected pass is admitted only when its segments cover the complete
    /// corrected reading (in UTF-8 bytes) and its path is system-dictionary-only.
    pub fn has_full_system_coverage(&self, corrected_reading_len: usize) -> bool {
        if !self.path_evidence.is_system_only() || self.segments.is_empty() {
            return false;
        }
        let mut next = 0u16;
        for segment in self.segments() {
            if segment.reading_start != next || segment.reading_end < segment.reading_start {
                return false;
            }
            next = segment.reading_end;
        }
        usize::from(next) == corrected_reading_len
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
}

#[derive(Debug, Clone)]
struct GeneratedSurface {
    text: FixedStr<MAX_PREEDIT_BYTES>,
    annotation: FixedStr<MAX_PREEDIT_BYTES>,
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

            let accepted: Vec<ConversionCandidate> = self
                .candidates
                .iter()
                .filter(|candidate| {
                    candidate.has_full_system_coverage(plan.corrected_reading().len())
                })
                .cloned()
                .collect();
            if accepted.is_empty() {
                rejected = rejected.saturating_add(1);
                continue;
            }
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
        }

        self.candidates.clear();
        self.candidates
            .extend(self.raw_direct_scratch.iter().cloned());
        if !self.raw_repair_scratch.is_empty() {
            if let Some(index) = direct_eviction_index {
                self.candidates.remove(index);
            }
        }
        let mut repair_candidates_added = 0usize;
        for candidate in self.raw_repair_scratch.iter().cloned() {
            if self.candidates.len() >= options.max_candidates {
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
            repair_candidates_added = repair_candidates_added.saturating_add(1);
        }
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

    /// Appends the pinned single-kanji table to a finished candidate list.
    ///
    /// Single kanji are deliberately not lattice edges. こう alone names 315
    /// characters in the pinned source, so admitting them as edges would spend
    /// a one-mora reading's whole `MAX_LATTICE_NODES` budget on them and would
    /// change the cost of every path that crosses them. Mozc reaches the same
    /// conclusion and runs its single-kanji rewriter after conversion; this is
    /// the same position in the pipeline. The tail therefore cannot move TOP-1
    /// or reorder anything the search produced. It only fills slots the ranked
    /// list left empty, and every appended cost sits above the whole ranked
    /// list so a later re-sort keeps it at the end.
    fn append_single_kanji(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        wanted: usize,
    ) -> Result<(), ConversionError> {
        if self.candidates.len() >= wanted || !dictionary.has_single_kanji() {
            return Ok(());
        }
        let reading_end =
            u16::try_from(reading.len()).map_err(|_| ConversionError::ReadingTooLong)?;
        let synthetic_id = if dictionary.class_count() > usize::from(DEFAULT_NOUN_ID) {
            DEFAULT_NOUN_ID
        } else {
            0
        };
        // Measured against the ranked ceiling rather than the previous tail
        // row: a cheap ranked list must not let its tail overtake anything.
        let ranked_ceiling = self
            .candidates
            .iter()
            .map(|candidate| candidate.cost)
            .max()
            .unwrap_or(0);
        for (index, character) in dictionary.single_kanji(reading).enumerate() {
            if self.candidates.len() >= wanted {
                break;
            }
            let mut text = FixedStr::new();
            if text.push(character).is_err() {
                continue;
            }
            // A character the search already ranked keeps its ranked position
            // and its own annotation.
            if self
                .candidates
                .iter()
                .any(|candidate| candidate.text() == text.as_str())
            {
                continue;
            }
            let Some(annotation) = single_kanji_annotation(dictionary, character) else {
                continue;
            };
            let mut segments = FixedVec::new();
            segments
                .push(ConversionSegment {
                    reading_start: 0,
                    reading_end,
                    text_start: 0,
                    text_end: u16::try_from(text.len())
                        .map_err(|_| ConversionError::OutputTooLong)?,
                    left_id: synthetic_id,
                    right_id: synthetic_id,
                    flags: EntryFlags::NONE,
                    word_count: 1,
                    it_word_count: 0,
                })
                .map_err(|_| ConversionError::TooManySegments)?;
            self.candidates.push(ConversionCandidate {
                text,
                annotation,
                segments,
                system_entry_index: NO_SYSTEM_ENTRY_INDEX,
                synthetic_exact: false,
                origin: CandidateOrigin::Direct,
                // Not system-only, so the cross-commit bridge can neither
                // anchor on an appended character nor transfer a contextual
                // gain to one.
                path_evidence: PathEvidence {
                    generated_edges: 1,
                    ..PathEvidence::default()
                },
                bridge_boundary_kind: None,
                commit_bridge_tail: CommitBridgeTailStorage::default(),
                cross_commit_rescored: false,
                cost: ranked_ceiling.saturating_add(1 + i64::try_from(index).unwrap_or(0)),
            });
        }
        Ok(())
    }

    /// Offers the whole punctuation family for a reading that is a single
    /// punctuation mark, configured glyph first.
    ///
    /// The setting picks the default, not the vocabulary. Before this, a
    /// reader who had chosen the full-width comma could not reach the touten
    /// for one quoted sentence without opening the settings window: the width
    /// choke point re-emits the configured glyph for whichever family member a
    /// candidate carries, so four distinct candidates would all have rendered
    /// as the same character. That collapse happens at display time, which is
    /// why the fix is a candidate bit rather than a normalizer change -- each
    /// appended row carries `synthetic_exact`, which `append_candidate_surface`
    /// and both commit-only surface paths honour ahead of `normalize_into`.
    ///
    /// Rule 4's owned set stays four code points wide. The two half-width kana
    /// marks are offerable without being claimed, and the ASCII pair keeps its
    /// emit-but-never-reclaim direction: nothing here rewrites a character the
    /// reader typed.
    ///
    /// Carrying `synthetic_exact` also suppresses learning and the exact cache
    /// for these rows, which is what this feature wants: one quoted sentence's
    /// touten must not train the ranker to override the reader's configured
    /// mark on every later comma.
    fn append_punctuation_family(
        &mut self,
        reading: &str,
        style: PunctuationStyle,
        wanted: usize,
    ) -> Result<(), ConversionError> {
        let mut characters = reading.chars();
        let (Some(mark), None) = (characters.next(), characters.next()) else {
            return Ok(());
        };
        let Some(family) = style.family_for(mark) else {
            return Ok(());
        };
        let reading_end =
            u16::try_from(reading.len()).map_err(|_| ConversionError::ReadingTooLong)?;
        // Every family member the search already produced has to go: it would
        // otherwise sit in the list without `synthetic_exact` and render as the
        // configured glyph, putting a second copy of one row on the page. The
        // appended set is a superset of what is dropped, so nothing the reader
        // could reach before becomes unreachable.
        self.candidates.retain(|candidate| {
            let mut text = candidate.text().chars();
            !matches!(
                (text.next(), text.next()),
                (Some(existing), None) if family.iter().any(|variant| variant.glyph == existing)
            )
        });
        // Below every surviving candidate, so the configured glyph is TOP-1 and
        // a later re-sort cannot interleave the family with anything else. This
        // is the one appender allowed to take TOP-1, and only for a reading that
        // is itself a single punctuation mark: the character it puts there is
        // the one the page already showed.
        let base = self
            .candidates
            .iter()
            .map(|candidate| candidate.cost)
            .min()
            .unwrap_or(0)
            .saturating_sub(i64::try_from(family.len()).unwrap_or(0));
        for (index, variant) in family.into_iter().enumerate() {
            let mut text = FixedStr::new();
            text.push(variant.glyph)
                .map_err(|_| ConversionError::OutputTooLong)?;
            let mut annotation = FixedStr::new();
            annotation
                .push_str(variant.annotation)
                .map_err(|_| ConversionError::OutputTooLong)?;
            let mut segments = FixedVec::new();
            segments
                .push(ConversionSegment {
                    reading_start: 0,
                    reading_end,
                    text_start: 0,
                    text_end: u16::try_from(text.len())
                        .map_err(|_| ConversionError::OutputTooLong)?,
                    // Neutral connection class in both directions: a
                    // punctuation mark must not hand the next conversion a
                    // noun's right ID.
                    left_id: 0,
                    right_id: 0,
                    flags: EntryFlags::NONE,
                    word_count: 1,
                    it_word_count: 0,
                })
                .map_err(|_| ConversionError::TooManySegments)?;
            self.candidates.insert(
                index,
                ConversionCandidate {
                    text,
                    annotation,
                    segments,
                    system_entry_index: NO_SYSTEM_ENTRY_INDEX,
                    synthetic_exact: true,
                    origin: CandidateOrigin::Direct,
                    path_evidence: PathEvidence {
                        generated_edges: 1,
                        ..PathEvidence::default()
                    },
                    bridge_boundary_kind: None,
                    commit_bridge_tail: CommitBridgeTailStorage::default(),
                    cross_commit_rescored: false,
                    cost: base.saturating_add(i64::try_from(index).unwrap_or(0)),
                },
            );
        }
        self.candidates.truncate(wanted.max(family.len()));
        Ok(())
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

    /// Rewards IT evidence across a complete word-sized reading rather than
    /// globally repricing an ambiguous standalone word. A reviewed IT phrase
    /// receives one unit of support, while a compositional candidate with two
    /// technical words receives two. Short ordinary phrases are left to the
    /// dictionary and connection matrix. This is a candidate-shape rule, not
    /// a list of registered compounds.
    fn apply_it_compound_coherence(&mut self, reading: &str, options: ConversionOptions) {
        if reading.chars().count() < MIN_IT_COMPOUND_READING_CHARS || options.it_bias_per_mille == 0
        {
            return;
        }

        for candidate in &mut self.candidates {
            let evidence = candidate.path_evidence();
            if evidence.fallback_edges != 0
                || evidence.generated_edges != 0
                || evidence.spelling_edges != 0
            {
                continue;
            }
            let it_words = candidate.segments().iter().fold(0u16, |count, segment| {
                count.saturating_add(u16::from(segment.it_word_count))
            });
            if it_words == 0 {
                continue;
            }
            let boost = i64::from(it_words)
                .saturating_mul(IT_COMPOUND_WORD_BONUS)
                .min(MAX_IT_COMPOUND_BOOST);
            candidate.cost = candidate.cost.saturating_sub(boost);
        }
    }

    /// Protects trustworthy whole-reading lexical evidence from speculative
    /// repair paths and the low-information tail of a fully lexical N-best
    /// search. A repair remains available when no direct exact entry exists;
    /// it is only suppressed when the dictionary already answers the query.
    fn apply_exact_lexical_quality_gate(&mut self, reading: &str) {
        let is_word_sized = reading.chars().count() <= MAX_EXACT_WORD_READING_CHARS;
        let suppress_unconfirmed_repairs = self.candidates.iter().any(|candidate| {
            candidate.system_entry_index().is_some()
                && !candidate.path_evidence().has_unconfirmed_repair()
                && is_trustworthy_exact_surface(candidate.text())
        });
        let Some(best_exact_cost) = self
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.system_entry_index().is_some()
                    && !candidate.path_evidence().has_unconfirmed_repair()
                    && (is_word_sized || is_atomic_whole_reading_surface(candidate.text()))
            })
            .map(|candidate| candidate.cost)
            .min()
        else {
            return;
        };
        let maximum_composite_cost =
            best_exact_cost.saturating_add(EXACT_LEXICAL_COMPOSITE_COST_WINDOW);
        self.candidates.retain(|candidate| {
            let evidence = candidate.path_evidence();
            !(suppress_unconfirmed_repairs && evidence.has_unconfirmed_repair())
                && (candidate.system_entry_index().is_some()
                    || evidence.user_edges != 0
                    || evidence.system_edges < 2
                    || evidence.fallback_edges != 0
                    || evidence.generated_edges != 0
                    || candidate.cost <= maximum_composite_cost)
        });
    }

    /// Drops the kana-fragment splices described on
    /// [`KANA_FRAGMENT_SPLIT_COST_WINDOW`]. The window is measured from the
    /// cheapest whole-reading path, so a reading that produced no whole-reading
    /// candidate at all keeps everything it found.
    fn drop_kana_fragment_prefix_splits(&mut self) {
        let Some(best_whole_reading_cost) = self
            .candidates
            .iter()
            .filter(|candidate| candidate.segments().len() == 1)
            .map(|candidate| candidate.cost)
            .min()
        else {
            return;
        };
        let ceiling = best_whole_reading_cost.saturating_add(KANA_FRAGMENT_SPLIT_COST_WINDOW);
        self.candidates.retain(|candidate| {
            candidate.cost <= ceiling || !is_kana_fragment_prefix_split(candidate)
        });
    }

    /// `じつ` is a word ending (先日, 本日, 全日), not the day counter `にち`.
    /// Numeric rewriter output and a 千+日 splice both look like "1000日" and
    /// bury the word the user is typing.
    fn drop_jitsu_day_counts(&mut self, reading: &str) {
        if !reading.ends_with("じつ") {
            return;
        }
        self.candidates
            .retain(|candidate| !is_numeric_day_surface(candidate.text()));
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
        let form_cost = numeric_form_cost(&reading[start..end], span);
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
            self.generated.push(GeneratedSurface { text, annotation });
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

    fn prefer_numeric_forms(&mut self, reading: &str) -> Result<(), ConversionError> {
        let Some(span) = parse_numeric_prefix(reading) else {
            return Ok(());
        };
        if span.bytes != reading.len() || !should_emit_numeric_span(span) {
            return Ok(());
        }
        let form_cost = numeric_form_cost(reading, span);
        let mut forms = Vec::new();
        for style in NUMERIC_STYLES {
            let mut text = FixedStr::<MAX_PREEDIT_BYTES>::new();
            if style.write(span, &mut text).is_err() {
                continue;
            }
            forms.push((text, style));
        }
        // A whole-reading dictionary form carries lexical ranking evidence
        // that the generated numeric rewriter does not. Keep generated forms
        // authoritative when no such entry exists (for example 24日), but do
        // not let a cheap synthetic 1日 displace the dictionary's 一日.
        let lexical_form_cost = self
            .candidates
            .iter()
            .filter(|candidate| candidate.system_entry_index().is_some())
            .filter(|candidate| {
                forms
                    .iter()
                    .any(|(form, _)| candidate.text() == form.as_str())
            })
            .map(|candidate| candidate.cost)
            .min();
        if let Some(lexical_cost) = lexical_form_cost {
            for candidate in &mut self.candidates {
                if candidate.path_evidence().generated_edges == 0 {
                    continue;
                }
                let Some(index) = forms
                    .iter()
                    .position(|(form, _)| candidate.text() == form.as_str())
                else {
                    continue;
                };
                candidate.cost = candidate.cost.max(
                    lexical_cost
                        .saturating_add(1)
                        .saturating_add(i64::try_from(index).unwrap_or(0)),
                );
            }
        }
        self.candidates.retain(|candidate| {
            let text = candidate.text();
            if text.chars().any(is_decorative_numeral_char) {
                return false;
            }
            forms.iter().any(|(form, _)| text == form.as_str())
                || text == reading
                || candidate.system_entry_index().is_some()
        });
        for (index, (text, style)) in forms.iter().enumerate() {
            if self
                .candidates
                .iter()
                .any(|candidate| candidate.text() == text.as_str())
            {
                continue;
            }
            let mut annotation = FixedStr::new();
            if annotation.push_str(style.annotation()).is_err() {
                continue;
            }
            let mut segments = FixedVec::new();
            let _ = segments.push(ConversionSegment {
                reading_start: 0,
                reading_end: u16::try_from(reading.len())
                    .map_err(|_| ConversionError::ReadingTooLong)?,
                text_start: 0,
                text_end: u16::try_from(text.len()).map_err(|_| ConversionError::OutputTooLong)?,
                left_id: 0,
                right_id: 0,
                flags: EntryFlags::NONE,
                word_count: 1,
                it_word_count: 0,
            });
            self.candidates.push(ConversionCandidate {
                text: text.clone(),
                annotation,
                segments,
                system_entry_index: NO_SYSTEM_ENTRY_INDEX,
                synthetic_exact: false,
                origin: CandidateOrigin::Direct,
                path_evidence: PathEvidence {
                    generated_edges: 1,
                    ..PathEvidence::default()
                },
                bridge_boundary_kind: None,
                commit_bridge_tail: CommitBridgeTailStorage::default(),
                cross_commit_rescored: false,
                cost: lexical_form_cost.map_or_else(
                    || form_cost.saturating_add(i64::try_from(index).unwrap_or(0)),
                    |lexical_cost| {
                        lexical_cost
                            .saturating_add(1)
                            .saturating_add(i64::try_from(index).unwrap_or(0))
                    },
                ),
            });
        }
        self.candidates.sort_by_key(|candidate| candidate.cost);
        Ok(())
    }

    fn add_date_candidates(
        &mut self,
        reading: &str,
        civil_date: Option<CivilDate>,
    ) -> Result<(), ConversionError> {
        let Some(offset) = date_offset_for_reading(reading) else {
            return Ok(());
        };
        let Some(date) = civil_date.and_then(|today| today.add_days(offset)) else {
            return Ok(());
        };
        if self.candidates.is_empty() {
            return Ok(());
        }
        let base = self.candidates[0].clone();
        let reading_end =
            u16::try_from(reading.len()).map_err(|_| ConversionError::ReadingTooLong)?;
        for (index, spec) in date_surface_specs(date).enumerate() {
            if self.candidates.len() >= MAX_CONVERSION_CANDIDATES + GENERATED_VARIANT_SLACK {
                break;
            }
            let mut text = FixedStr::new();
            if spec.format.write(date, &mut text).is_err() {
                continue;
            }
            if self
                .candidates
                .iter()
                .any(|candidate| candidate.text() == text.as_str())
            {
                continue;
            }
            let mut annotation = FixedStr::new();
            if annotation.push_str(spec.annotation).is_err() {
                continue;
            }
            let mut segments = FixedVec::new();
            let first = base.segments().first().copied().unwrap_or_default();
            let last = base.segments().last().copied().unwrap_or(first);
            segments
                .push(ConversionSegment {
                    reading_start: 0,
                    reading_end,
                    text_start: 0,
                    text_end: u16::try_from(text.len())
                        .map_err(|_| ConversionError::OutputTooLong)?,
                    left_id: first.left_id,
                    right_id: last.right_id,
                    flags: EntryFlags::NONE,
                    word_count: 1,
                    it_word_count: 0,
                })
                .map_err(|_| ConversionError::TooManySegments)?;
            self.candidates.push(ConversionCandidate {
                text,
                annotation,
                segments,
                system_entry_index: NO_SYSTEM_ENTRY_INDEX,
                synthetic_exact: false,
                origin: CandidateOrigin::Direct,
                path_evidence: PathEvidence {
                    generated_edges: 1,
                    ..PathEvidence::default()
                },
                bridge_boundary_kind: None,
                commit_bridge_tail: CommitBridgeTailStorage::default(),
                cross_commit_rescored: false,
                cost: base
                    .cost
                    .saturating_add(10 + i64::try_from(index).unwrap_or(0)),
            });
        }
        Ok(())
    }

    fn reset(&mut self, reading_len: usize) {
        self.nodes.clear();
        self.states.clear();
        self.queue.clear();
        self.path.clear();
        self.candidates.clear();
        self.generated.clear();
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
        commit_repairs: &[FixedStr<MAX_PREEDIT_BYTES>],
    ) -> Result<(), ConversionError> {
        if options.method == ConversionMethod::SingleSegment {
            return self.build_single_segment_lattice(
                dictionary,
                user_dictionary,
                reading,
                options,
                commit_repairs,
            );
        }
        let synthetic_id = if dictionary.class_count() > usize::from(DEFAULT_NOUN_ID) {
            DEFAULT_NOUN_ID
        } else {
            0
        };
        let mut previous_class = None;
        for (start, character) in reading.char_indices() {
            let class = char_class(character);
            let run = char_run(reading, start);
            // A run of Latin letters or ASCII digits is one token the user
            // typed, not a phrase to be segmented. Splitting `24` into `2` +
            // `4` lets a superscript `²` steal the first digit and produce
            // `²4日`. Entries that start at the token and reach at least its
            // end remain available.
            let atomic_token = matches!(class, CharClass::AsciiLetter | CharClass::AsciiDigit);
            let at_token_start = previous_class != Some(class) || !atomic_token;
            previous_class = Some(class);
            let mut last_length = 0usize;
            let mut candidates_for_length = 0usize;
            let mut edge_budget = DictionaryEdgeBudget::new();
            let mut failure = None;
            dictionary
                .common_prefix_search(&reading[start..], |matched| {
                    if atomic_token && (!at_token_start || start + matched.matched_bytes < run.end)
                    {
                        return true;
                    }
                    if !allows_system_entry(
                        options.input_support,
                        options.skip_input_repair,
                        matched.entry.flags,
                    ) || (start == 0 && matched.entry.flags.contains(EntryFlags::NON_INITIAL))
                    {
                        return true;
                    }
                    if matched.matched_bytes != last_length {
                        last_length = matched.matched_bytes;
                        candidates_for_length = 0;
                        edge_budget.reset();
                    }
                    if !edge_budget.admit(matched.entry.surface_id) {
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
                                repair: None,
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

            // Repair edges are best-effort: a full lattice must not fail the
            // conversion when only a typo-correction path ran out of room.
            let _ = self.add_local_repair_edges(
                dictionary,
                &reading[start..],
                start,
                candidates_for_length,
                options,
            );

            if let Some(user_dictionary) = user_dictionary {
                let mut user_candidates_for_length = 0usize;
                let mut last_user_length = 0usize;
                let mut user_failure = None;
                user_dictionary.common_prefix_search(
                    &reading[start..],
                    |matched_bytes, entry_index| {
                        if atomic_token && (!at_token_start || start + matched_bytes < run.end) {
                            return true;
                        }
                        if matched_bytes != last_user_length {
                            last_user_length = matched_bytes;
                            user_candidates_for_length = 0;
                        }
                        if user_candidates_for_length >= BASE_DICTIONARY_EDGES_PER_READING {
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
            // Keep ASCII letter/digit runs atomic: a per-character fallback
            // for `2` inside `24` is how a superscript numeral can splice in.
            if !(atomic_token && run.end > character_end) {
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
            }

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
            self.add_numeric_forms(dictionary, reading, start, synthetic_id)?;
        }
        // Commit-history repairs are whole-query hints only. Keep them outside
        // the per-character start loop so a full typed match cannot be pasted
        // onto an intermediate suffix span (Issue #63).
        let _ = self.add_commit_repair_edges_for_whole_query(
            dictionary,
            reading,
            options,
            commit_repairs,
        );
        Ok(())
    }

    /// Adds bounded typo-repair and English-spelling edges for one typed span.
    /// Exact dictionary edges keep priority: repair only fills unused slots and
    /// always carries an explicit penalty. Lattice exhaustion drops repair edges
    /// without failing the conversion. Commit-history hints are handled by
    /// [`Self::add_commit_repair_edges_for_whole_query`].
    fn add_local_repair_edges(
        &mut self,
        dictionary: &Dictionary<'_>,
        typed_suffix: &str,
        start: usize,
        exact_edges_for_length: usize,
        options: ConversionOptions,
    ) -> Result<(), ConversionError> {
        let mut support = options.input_support;
        if !support.is_active() || options.skip_input_repair || typed_suffix.is_empty() {
            return Ok(());
        }
        let mut remaining_slots =
            BASE_DICTIONARY_EDGES_PER_READING.saturating_sub(exact_edges_for_length);
        if remaining_slots == 0 {
            return Ok(());
        }

        // Reading-only edit-1 variants are prediction-only until a raw-input
        // provenance path can prove the user's intended key sequence. Keep
        // the shared generator available to prediction, but do not even
        // generate Advanced variants on the ordinary conversion path.
        support.advanced = false;
        let variants = collect_repair_variants(typed_suffix, support, MAX_REPAIR_VARIANTS);
        for variant in variants.iter() {
            if remaining_slots == 0 {
                break;
            }
            let added = self.add_repaired_dictionary_edges(
                dictionary,
                variant.repaired.as_str(),
                start,
                start + usize::from(variant.typed_end).min(typed_suffix.len()),
                variant.penalty,
                remaining_slots,
                options,
                variant.kind,
            )?;
            remaining_slots = remaining_slots.saturating_sub(added);
        }

        if support.english_to_katakana {
            if let Some(katakana) = english_spelling_katakana_reading(typed_suffix) {
                let _ = self.add_repaired_dictionary_edges(
                    dictionary,
                    katakana.as_str(),
                    start,
                    start + typed_suffix.len(),
                    ENGLISH_KATAKANA_PENALTY,
                    remaining_slots,
                    options,
                    RepairKind::EnglishSpelling,
                )?;
            }
        }
        Ok(())
    }

    /// Adds commit-history repair edges for the full query only.
    ///
    /// Invariant (Issue #63): each accepted edge has `start == 0`,
    /// `end == reading.len()`, and was collected for this exact typed reading.
    fn add_commit_repair_edges_for_whole_query(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        options: ConversionOptions,
        commit_repairs: &[FixedStr<MAX_PREEDIT_BYTES>],
    ) -> Result<(), ConversionError> {
        let support = options.input_support;
        if !support.is_active()
            || options.skip_input_repair
            || !support.commit_based
            || reading.is_empty()
            || commit_repairs.is_empty()
        {
            return Ok(());
        }
        let mut remaining_slots = BASE_DICTIONARY_EDGES_PER_READING;
        for repaired in commit_repairs {
            if remaining_slots == 0 {
                break;
            }
            if repaired.as_str() == reading {
                continue;
            }
            let added = self.add_repaired_dictionary_edges(
                dictionary,
                repaired.as_str(),
                0,
                reading.len(),
                COMMIT_HISTORY_PENALTY,
                remaining_slots,
                options,
                RepairKind::CommitHistory,
            )?;
            remaining_slots = remaining_slots.saturating_sub(added);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn add_repaired_dictionary_edges(
        &mut self,
        dictionary: &Dictionary<'_>,
        repaired: &str,
        start: usize,
        typed_end: usize,
        penalty: i64,
        remaining_slots: usize,
        options: ConversionOptions,
        repair: RepairKind,
    ) -> Result<usize, ConversionError> {
        if remaining_slots == 0 || repaired.is_empty() {
            return Ok(0);
        }
        let mut added = 0usize;
        let mut lattice_full = false;
        let result = dictionary.common_prefix_search(repaired, |matched| {
            if matched.matched_bytes != repaired.len() {
                return true;
            }
            if !allows_system_entry(
                options.input_support,
                options.skip_input_repair,
                matched.entry.flags,
            ) || (start == 0 && matched.entry.flags.contains(EntryFlags::NON_INITIAL))
            {
                return true;
            }
            if added >= remaining_slots {
                return false;
            }
            let boost = if matched.entry.flags.contains(EntryFlags::IT) {
                let proportional = i64::from(matched.entry.word_cost.max(0))
                    .saturating_mul(i64::from(options.it_bias_per_mille))
                    / 1_000;
                proportional.min(i64::from(options.max_it_boost))
            } else {
                0
            };
            let local_cost = i64::from(matched.entry.word_cost)
                .saturating_sub(boost)
                .saturating_add(penalty);
            let Ok(entry_index) = u32::try_from(matched.entry_index) else {
                return true;
            };
            match self.add_node(
                dictionary,
                NodeSpec {
                    start,
                    end: typed_end,
                    left_id: matched.entry.left_id,
                    right_id: matched.entry.right_id,
                    local_cost,
                    surface: Surface::Dictionary {
                        entry: matched.entry,
                        entry_index,
                        repair: Some(repair),
                    },
                },
            ) {
                Ok(()) => {
                    added += 1;
                    true
                }
                Err(ConversionError::LatticeFull) => {
                    lattice_full = true;
                    false
                }
                Err(_) => false,
            }
        });
        if lattice_full {
            return Ok(added);
        }
        result.map_err(ConversionError::Dictionary)?;
        Ok(added)
    }

    /// Builds the intentionally narrow single-bunsetsu lattice. Restricting
    /// nodes at construction time keeps N-best, learning metadata, and the
    /// candidate UI honest: no multi-segment path is generated and later
    /// hidden merely for presentation.
    fn build_single_segment_lattice(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        options: ConversionOptions,
        commit_repairs: &[FixedStr<MAX_PREEDIT_BYTES>],
    ) -> Result<(), ConversionError> {
        let reading_len = reading.len();
        let synthetic_id = if dictionary.class_count() > usize::from(DEFAULT_NOUN_ID) {
            DEFAULT_NOUN_ID
        } else {
            0
        };
        let mut failure = None;
        let mut exact_edges_added = 0usize;
        let mut edge_budget = DictionaryEdgeBudget::new();
        dictionary
            .common_prefix_search(reading, |matched| {
                if matched.matched_bytes != reading_len {
                    return true;
                }
                if !allows_system_entry(
                    options.input_support,
                    options.skip_input_repair,
                    matched.entry.flags,
                ) || matched.entry.flags.contains(EntryFlags::NON_INITIAL)
                {
                    return true;
                }
                if !edge_budget.admit(matched.entry.surface_id) {
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
                if let Err(error) = self.add_node(
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
                    failure = Some(error);
                    return false;
                }
                exact_edges_added = exact_edges_added.saturating_add(1);
                true
            })
            .map_err(ConversionError::Dictionary)?;
        if let Some(error) = failure {
            return Err(error);
        }
        let _ = self.add_local_repair_edges(dictionary, reading, 0, exact_edges_added, options);
        let _ = self.add_commit_repair_edges_for_whole_query(
            dictionary,
            reading,
            options,
            commit_repairs,
        );

        if let Some(user_dictionary) = user_dictionary {
            let mut user_failure = None;
            user_dictionary.common_prefix_search(reading, |matched_bytes, entry_index| {
                if matched_bytes != reading_len {
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
                if let Err(error) = self.add_node(
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
                    user_failure = Some(error);
                    return false;
                }
                true
            });
            if let Some(error) = user_failure {
                return Err(error);
            }
        }

        let chars = reading.chars().count();
        self.add_node(
            dictionary,
            NodeSpec {
                start: 0,
                end: reading_len,
                left_id: synthetic_id,
                right_id: synthetic_id,
                local_cost: synthetic_run_cost(RUN_BASE_COST, RUN_COST_PER_CHAR, chars),
                surface: Surface::Reading,
            },
        )?;
        if reading
            .chars()
            .all(|character| char_class(character) == CharClass::Hiragana)
        {
            self.add_node(
                dictionary,
                NodeSpec {
                    start: 0,
                    end: reading_len,
                    left_id: synthetic_id,
                    right_id: synthetic_id,
                    local_cost: synthetic_run_cost(
                        KATAKANA_BASE_COST,
                        KATAKANA_COST_PER_CHAR,
                        chars,
                    ),
                    surface: Surface::Katakana,
                },
            )?;
        }
        for (counter_reading, surface) in COUNTER_FORMS {
            if counter_reading == reading {
                self.add_node(
                    dictionary,
                    NodeSpec {
                        start: 0,
                        end: reading_len,
                        left_id: synthetic_id,
                        right_id: synthetic_id,
                        local_cost: COUNTER_WORD_COST,
                        surface: Surface::Literal(surface),
                    },
                )?;
            }
        }
        self.add_numeric_forms(dictionary, reading, 0, synthetic_id)?;
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
                connection_cost(
                    dictionary,
                    RightContextId::new(self.initial_right_id),
                    LeftContextId::new(spec.left_id),
                )
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
                    .saturating_add(connection_cost(
                        dictionary,
                        RightContextId::new(prior.right_id),
                        LeftContextId::new(spec.left_id),
                    ))
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
                connection_cost(
                    dictionary,
                    RightContextId::new(node.right_id),
                    LeftContextId::new(0),
                )
            } else {
                let mut next = self.starts_at[node.end];
                let mut best = i64::MAX;
                while next != NONE {
                    let following = self.nodes[next];
                    if following.suffix_cost != i64::MAX {
                        let cost = connection_cost(
                            dictionary,
                            RightContextId::new(node.right_id),
                            LeftContextId::new(following.left_id),
                        )
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
            let cost = node.best_cost.saturating_add(connection_cost(
                dictionary,
                RightContextId::new(node.right_id),
                LeftContextId::new(0),
            ));
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
                RightContextId::new(self.nodes[final_node].right_id),
                LeftContextId::new(0),
            ));
        let candidate = make_candidate(
            CandidateMaterialization {
                dictionary,
                user_dictionary,
                reading,
                initial_right_id: self.initial_right_id,
                bridge_reading_boundary: self.cross_commit_reading_boundary,
                nodes: &self.nodes,
                path: &self.path,
                generated: &self.generated,
            },
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
                let cost = connection_cost(
                    dictionary,
                    RightContextId::new(self.initial_right_id),
                    LeftContextId::new(lattice_node.left_id),
                )
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
                    RightContextId::new(lattice_node.right_id),
                    LeftContextId::new(0),
                ));
                // A later (costlier) path stitching together to more than
                // `MAX_PREEDIT_BYTES` is an unremarkable, expected outcome
                // once enough alternatives exist. Skipping it and continuing
                // the search for a smaller alternative keeps this in the same
                // "degrade gracefully" family as the lattice-node and
                // search-state budgets below.
                let candidate = match make_candidate(
                    CandidateMaterialization {
                        dictionary,
                        user_dictionary,
                        reading,
                        initial_right_id: self.initial_right_id,
                        bridge_reading_boundary: self.cross_commit_reading_boundary,
                        nodes: &self.nodes,
                        path: &self.path,
                        generated: &self.generated,
                    },
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
                            RightContextId::new(lattice_node.right_id),
                            LeftContextId::new(following.left_id),
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
            Surface::Literal(_) | Surface::Generated(_) => {}
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
            Surface::Literal(_) | Surface::Generated(_) => Self::Neutral,
        };
        match (self, next) {
            (current, Self::Neutral) => Some(current),
            (Self::Neutral, next) => Some(next),
            (current, next) if current == next => Some(current),
            _ => None,
        }
    }
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

/// Renders one appended character's annotation: 異体字（高） for a character
/// the pinned rules relate to another, and a plain marker otherwise.
///
/// A note is built whole or not at all, so a bounded buffer that cannot hold
/// the full relation never leaves a half-written label on a candidate.
fn single_kanji_annotation(
    dictionary: &Dictionary<'_>,
    character: char,
) -> Option<FixedStr<MAX_PREEDIT_BYTES>> {
    let mut annotation = FixedStr::new();
    let Some(SingleKanjiVariant { original, kind }) = dictionary.single_kanji_variant(character)
    else {
        annotation.push_str(SINGLE_KANJI_ANNOTATION).ok()?;
        return Some(annotation);
    };
    annotation.push_str(kind.label()).ok()?;
    annotation.push('（').ok()?;
    annotation.push(original).ok()?;
    annotation.push('）').ok()?;
    Some(annotation)
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

fn is_ascii_alpha_digit_identifier(value: &str) -> bool {
    let mut has_alpha = false;
    let mut has_digit = false;
    for byte in value.bytes() {
        if byte.is_ascii_alphabetic() {
            has_alpha = true;
        } else if byte.is_ascii_digit() {
            has_digit = true;
        } else {
            return false;
        }
    }
    has_alpha && has_digit
}

fn is_mixed_unresolved_latin(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_ascii_alphabetic())
}

fn synthetic_run_cost(base: i64, per_character: i64, characters: usize) -> i64 {
    base.saturating_add(per_character.saturating_mul(characters as i64))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};

    use super::{
        candidate_budget, CandidateAuthority, CandidateOrigin, ConversionCandidate,
        ConversionInput, ConversionInputClass, ConversionOptions, Converter, CorrectionMap,
        CorrectionMapError, CorrectionRun, CrossCommitBridge, DictionaryEdgeBudget, LiteralPolicy,
        RawRepairBudget, RawRepairPlan, RepairTier, RightContextId,
        BASE_DICTIONARY_EDGES_PER_READING, MAX_CONVERSION_CANDIDATES,
        MAX_DICTIONARY_SURFACES_PER_READING, SINGLE_KANJI_ANNOTATION,
    };
    use crate::dictionary::{image_format, Dictionary, EntryFlags};
    use crate::preferences::ConversionMethod;
    use crate::user_dictionary::UserDictionary;
    use crate::width::{CommaMark, PeriodMark, PunctuationStyle};
    use crate::RepairKind;

    #[derive(Clone)]
    struct FixtureEntry {
        reading: String,
        surface: String,
        cost: i32,
        flags: EntryFlags,
    }

    #[derive(Default)]
    struct FixtureTrieNode {
        label: char,
        children: BTreeMap<char, usize>,
        entries: Vec<usize>,
    }

    fn fixture_entry(reading: &str, surface: &str, cost: i32, flags: EntryFlags) -> FixtureEntry {
        FixtureEntry {
            reading: reading.to_owned(),
            surface: surface.to_owned(),
            cost,
            flags,
        }
    }

    #[test]
    fn dictionary_edge_budget_preserves_baseline_rows_then_adds_surface_diversity() {
        let surface_bound = MAX_DICTIONARY_SURFACES_PER_READING as u32;
        let mut budget = DictionaryEdgeBudget::new();
        for _ in 0..BASE_DICTIONARY_EDGES_PER_READING {
            assert!(budget.admit(1), "the historical baseline rows must survive");
        }
        assert!(
            !budget.admit(1),
            "later POS rows must not consume diversity slots"
        );
        for surface_id in 2..=surface_bound {
            assert!(budget.admit(surface_id), "surface {surface_id}");
        }
        assert!(
            !budget.admit(surface_bound + 1),
            "the distinct-surface bound must remain finite"
        );

        budget.reset();
        assert!(
            budget.admit(surface_bound + 1),
            "a new reading gets a fresh budget"
        );
    }

    /// Issue #94: a path that opens with a bare one-character hiragana fragment
    /// and then spends a whole kanji word is a splice of the reading, not a
    /// parse of it, and those splices were sitting on the first candidate page.
    /// The honorific prefixes stay, and so does a splice cheap enough to be a
    /// real reading of the input.
    #[test]
    fn kana_fragment_prefix_splits_leave_the_candidate_page() {
        let rows = [
            fixture_entry("たいあん", "対案", 1000, EntryFlags::NONE),
            fixture_entry("た", "た", 100, EntryFlags::NONE),
            fixture_entry("いあん", "慰安", 2600, EntryFlags::NONE),
            fixture_entry("ごいけん", "御意見", 1000, EntryFlags::NONE),
            fixture_entry("ご", "ご", 100, EntryFlags::NONE),
            fixture_entry("いけん", "意見", 2600, EntryFlags::NONE),
            fixture_entry("とじょう", "途上", 1000, EntryFlags::NONE),
            fixture_entry("と", "と", 100, EntryFlags::NONE),
            fixture_entry("じょう", "場", 900, EntryFlags::NONE),
        ];
        let bytes = synthetic_dictionary(&rows);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let surfaces = |reading: &str| {
            let mut converter = Converter::new();
            converter
                .convert(&dictionary, reading, ConversionOptions::default())
                .expect("conversion")
                .iter()
                .map(|candidate| candidate.text().to_owned())
                .collect::<Vec<_>>()
        };

        let taian = surfaces("たいあん");
        assert!(taian.contains(&"対案".to_owned()), "{taian:?}");
        assert!(!taian.contains(&"た慰安".to_owned()), "{taian:?}");

        let goiken = surfaces("ごいけん");
        assert!(
            goiken.contains(&"ご意見".to_owned()),
            "an honorific prefix is a parse, not a splice: {goiken:?}"
        );

        let tojou = surfaces("とじょう");
        assert!(
            tojou.contains(&"と場".to_owned()),
            "a splice inside the window still spells a real word: {tojou:?}"
        );
    }

    /// Issue #94: the surface bound used to sit at the twelve baseline edges, so
    /// the thirteenth distinct surface of a reading never entered the lattice at
    /// all. Shipped きゅう spent its last slot on the rare name kanji 邱 and lost
    /// the digit spelling 9 even though 9 had the cheaper whole-path cost. A
    /// reading must expose as many distinct surfaces as the output frame carries.
    #[test]
    fn distinct_surfaces_beyond_the_baseline_edges_still_reach_conversion() {
        const BASELINE_SURFACES: [&str; 12] = [
            "旧", "級", "急", "給", "球", "究", "求", "九", "久", "休", "吸", "宮",
        ];
        assert_eq!(BASELINE_SURFACES.len(), BASE_DICTIONARY_EDGES_PER_READING);

        let mut rows = BASELINE_SURFACES
            .iter()
            .enumerate()
            .map(|(index, surface)| {
                fixture_entry("きゅう", surface, 100 + index as i32, EntryFlags::NONE)
            })
            .collect::<Vec<_>>();
        rows.push(fixture_entry("きゅう", "9", 200, EntryFlags::NONE));
        let bytes = synthetic_dictionary(&rows);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");

        for method in ConversionMethod::ALL {
            let mut converter = Converter::new();
            let candidates = converter
                .convert(
                    &dictionary,
                    "きゅう",
                    ConversionOptions {
                        method,
                        ..ConversionOptions::default()
                    },
                )
                .expect("conversion");
            assert!(
                candidates.iter().any(|candidate| candidate.text() == "9"),
                "{method:?}: the thirteenth surface must survive the edge budget: {candidates:?}"
            );
        }
    }

    #[test]
    fn repeated_pos_rows_cannot_hide_a_distinct_surface_from_conversion() {
        let mut rows = (0..12)
            .map(|cost| fixture_entry("たて", "同じ", cost, EntryFlags::NONE))
            .collect::<Vec<_>>();
        rows.push(fixture_entry("たて", "縦", 100, EntryFlags::NONE));
        let bytes = synthetic_dictionary(&rows);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");

        for method in ConversionMethod::ALL {
            let mut converter = Converter::new();
            let candidates = converter
                .convert(
                    &dictionary,
                    "たて",
                    ConversionOptions {
                        method,
                        ..ConversionOptions::default()
                    },
                )
                .expect("conversion");
            assert!(
                candidates.iter().any(|candidate| candidate.text() == "縦"),
                "{method:?}: {candidates:?}"
            );
        }
    }

    /// A one-mora reading is where the gap against a commercial IME is widest
    /// and where the ranked list runs out first. The fixture therefore uses
    /// one ranked entry and a character list that overlaps it.
    fn single_kanji_fixture() -> Vec<u8> {
        synthetic_dictionary_with_single_kanji(
            &[
                fixture_entry("ひ", "日", 100, EntryFlags::NONE),
                fixture_entry("ひかり", "光", 100, EntryFlags::NONE),
            ],
            // Byte-ascending readings, each listing characters in the source's
            // own preference order. 日 is deliberately also a ranked entry.
            &[("ひ", "日火比髙"), ("ひかり", "灯")],
            &[('髙', '高', 1)],
        )
    }

    fn converted(bytes: &[u8], reading: &str, wanted: usize) -> Vec<(String, String)> {
        let dictionary = Dictionary::parse(bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        converter
            .convert(
                &dictionary,
                reading,
                ConversionOptions {
                    max_candidates: wanted,
                    ..ConversionOptions::default()
                },
            )
            .expect("conversion")
            .iter()
            .map(|candidate| {
                (
                    candidate.text().to_string(),
                    candidate.annotation().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn single_kanji_fill_the_slots_the_ranked_list_left_empty() {
        let listed = converted(&single_kanji_fixture(), "ひ", 8);
        let surfaces = listed
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>();
        // 日 is ranked, so it keeps its ranked position and is not repeated in
        // the tail; the rest follow in the source's preference order.
        assert_eq!(surfaces.first(), Some(&"日"));
        assert_eq!(&surfaces[surfaces.len() - 3..], &["火", "比", "髙"]);
        assert_eq!(surfaces.iter().filter(|text| **text == "日").count(), 1);
    }

    #[test]
    fn the_appended_tail_never_changes_the_ranked_list() {
        let rows = [
            fixture_entry("ひ", "日", 100, EntryFlags::NONE),
            fixture_entry("ひかり", "光", 100, EntryFlags::NONE),
        ];
        let without = converted(&synthetic_dictionary(&rows), "ひ", 8);
        let with = converted(&single_kanji_fixture(), "ひ", 8);
        assert!(with.len() > without.len(), "the tail must add rows");
        assert_eq!(
            &with[..without.len()],
            &without[..],
            "every ranked row must keep its text, annotation, and position"
        );
    }

    #[test]
    fn the_tail_stops_at_the_candidate_limit() {
        for wanted in 1..=8 {
            let listed = converted(&single_kanji_fixture(), "ひ", wanted);
            assert!(
                listed.len() <= wanted,
                "limit {wanted} produced {} candidates",
                listed.len()
            );
        }
        // The limit, not the character list, is what stops the tail: one slot
        // leaves room for the ranked entry alone.
        assert_eq!(converted(&single_kanji_fixture(), "ひ", 1).len(), 1);
    }

    /// Issue #95: far more single-kanji characters than the pre-#95 ceiling
    /// of 18, so a test can tell whether a wide request actually reached the
    /// tail or was silently narrowed by `candidate_budget`. The ranked row's
    /// surface is deliberately also the first listed character, the same
    /// overlap `single_kanji_fixture` exercises above, so the count reflects
    /// distinct surfaces rather than a duplicate.
    fn dictionary_with_many_single_kanji(reading: &str) -> Vec<u8> {
        const LISTED: &str = "日月火水木金土人子女男大小上下中左右前後内外一二三四五六七八九十";
        debug_assert_eq!(LISTED.chars().count(), 32);
        synthetic_dictionary_with_single_kanji(
            &[fixture_entry(reading, "日", 100, EntryFlags::NONE)],
            &[(reading, LISTED)],
            &[],
        )
    }

    /// Issue #95: readings are kana, so a byte count must not stand in for a
    /// character count. Every reading below is multi-byte UTF-8, so a byte
    /// count would already have crossed a boundary a character count has
    /// not, and this would catch that mistake as a wrong tier rather than a
    /// panic.
    #[test]
    fn candidate_budget_switches_tiers_at_four_and_eight_characters() {
        assert_eq!(candidate_budget(&"あ".repeat(4)), 256);
        assert_eq!(candidate_budget(&"あ".repeat(5)), 108);
        assert_eq!(candidate_budget(&"あ".repeat(8)), 108);
        assert_eq!(candidate_budget(&"あ".repeat(9)), 18);
    }

    /// Issue #95 raised `MAX_CONVERSION_CANDIDATES` from 18 to 256
    /// specifically so a short reading could reach single-kanji surfaces the
    /// old ceiling trimmed away; `candidate_budget` must actually let them
    /// through instead of silently keeping the old limit.
    #[test]
    fn a_short_reading_can_receive_more_than_eighteen_candidates_now() {
        let bytes = dictionary_with_many_single_kanji("ひ");
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let candidates = converter
            .convert(&dictionary, "ひ", ConversionOptions::default())
            .expect("conversion");
        assert!(
            candidates.len() > 18,
            "a one-character reading's own budget is the full ceiling: {} candidates",
            candidates.len()
        );
    }

    /// A long reading's candidate list is whole-sentence parses nobody pages
    /// through -- Issue #95 measured no single-kanji or homophone benefit
    /// past eight characters -- so it keeps the pre-#95 ceiling even when
    /// the caller explicitly asks for the full 256.
    #[test]
    fn a_long_reading_still_holds_at_eighteen_even_at_the_full_ceiling() {
        let long_reading = "ひ".repeat(9);
        let bytes = dictionary_with_many_single_kanji(&long_reading);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let candidates = converter
            .convert(
                &dictionary,
                &long_reading,
                ConversionOptions {
                    max_candidates: MAX_CONVERSION_CANDIDATES,
                    ..ConversionOptions::default()
                },
            )
            .expect("conversion");
        assert_eq!(
            candidates.len(),
            18,
            "a long reading must not see past the pre-#95 ceiling: {candidates:?}"
        );
    }

    /// The clamp only ever narrows a request; it must never raise a
    /// caller's own smaller number back up to the short-reading ceiling.
    #[test]
    fn a_smaller_request_than_the_budget_keeps_its_own_number() {
        let bytes = dictionary_with_many_single_kanji("ひ");
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let candidates = converter
            .convert(
                &dictionary,
                "ひ",
                ConversionOptions {
                    max_candidates: 5,
                    ..ConversionOptions::default()
                },
            )
            .expect("conversion");
        assert_eq!(
            candidates.len(),
            5,
            "the short-reading budget of 256 must not override a smaller request: {candidates:?}"
        );
    }

    #[test]
    fn an_appended_character_carries_its_relation_or_a_plain_marker() {
        let listed = converted(&single_kanji_fixture(), "ひ", 8);
        let note = |surface: &str| {
            listed
                .iter()
                .find(|(text, _)| text == surface)
                .map(|(_, annotation)| annotation.clone())
                .unwrap_or_else(|| panic!("{surface} is missing"))
        };
        assert_eq!(note("髙"), "異体字（高）");
        assert_eq!(note("火"), "単漢字");
        assert_eq!(note("比"), "単漢字");
    }

    #[test]
    fn a_reading_the_table_does_not_list_gets_no_tail() {
        // ひか sits between ひ and ひかり, so a lookup that stopped at the
        // shorter length would hand it one of their character lists.
        for reading in ["ひか", "ひかりの"] {
            let listed = converted(&single_kanji_fixture(), reading, 8);
            assert!(
                listed.iter().all(|(_, annotation)| annotation != "単漢字"),
                "{reading} borrowed a character list: {listed:?}"
            );
        }
    }

    #[test]
    fn appended_costs_stay_above_every_ranked_cost() {
        let bytes = single_kanji_fixture();
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let result = converter
            .convert_detailed(
                &dictionary,
                "ひ",
                ConversionOptions {
                    max_candidates: 8,
                    ..ConversionOptions::default()
                },
            )
            .expect("conversion");
        let candidates = result.candidates();
        let appended = |candidate: &ConversionCandidate| {
            candidate.annotation() == SINGLE_KANJI_ANNOTATION
                || candidate.annotation().starts_with("異体字")
        };
        let ranked_ceiling = candidates
            .iter()
            .filter(|candidate| !appended(candidate))
            .map(|candidate| candidate.cost)
            .max()
            .expect("a ranked row exists");
        let tail = candidates
            .iter()
            .filter(|candidate| appended(candidate))
            .collect::<Vec<_>>();
        assert!(!tail.is_empty(), "the fixture must produce a tail");
        // A later re-sort by cost must not be able to lift the tail into the
        // ranked list.
        for candidate in tail {
            assert!(
                candidate.cost > ranked_ceiling,
                "{} costs {} at or below the ranked ceiling {ranked_ceiling}",
                candidate.text(),
                candidate.cost
            );
        }
    }

    /// Issue #99.  The setting picks which mark comes first; it never picks
    /// which marks exist.  Every one of the nine combinations has to reach all
    /// four members of the family it is asked for, in an order it decides.
    #[test]
    fn every_punctuation_style_offers_its_whole_family_configured_glyph_first() {
        let bytes = synthetic_dictionary(&[fixture_entry("ひ", "日", 100, EntryFlags::NONE)]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        for comma in CommaMark::ALL {
            for period in PeriodMark::ALL {
                let style = PunctuationStyle::new(comma, period);
                let roles = [('\u{3001}', comma.glyph()), ('\u{3002}', period.glyph())];
                for (reading, configured) in roles {
                    let mut source = String::new();
                    source.push(reading);
                    let expected = style
                        .family_for(reading)
                        .expect("a punctuation reading belongs to a family");
                    let result = converter
                        .convert_detailed(
                            &dictionary,
                            &source,
                            ConversionOptions {
                                punctuation: style,
                                ..ConversionOptions::default()
                            },
                        )
                        .expect("conversion");
                    let candidates = result.candidates();
                    assert!(
                        candidates.len() >= expected.len(),
                        "{style:?} on {reading:?} produced only {} candidates",
                        candidates.len()
                    );
                    for (offset, variant) in expected.into_iter().enumerate() {
                        let candidate = &candidates[offset];
                        assert_eq!(
                            candidate.text(),
                            variant.glyph.to_string(),
                            "{style:?} on {reading:?} at slot {offset}"
                        );
                        assert_eq!(candidate.annotation(), variant.annotation);
                        // Without this bit the choke point rewrites all four
                        // rows to the configured glyph and the page shows one
                        // character four times.
                        assert!(
                            candidate.is_synthetic_exact(),
                            "{style:?} on {reading:?}: slot {offset} would be re-styled"
                        );
                    }
                    assert_eq!(
                        candidates[0].text(),
                        configured.to_string(),
                        "{style:?} must still default to the mark the reader set"
                    );
                    // A family member appearing twice -- once appended, once
                    // left over from the search -- is the failure this guards.
                    let members = candidates
                        .iter()
                        .filter(|candidate| {
                            expected
                                .iter()
                                .any(|variant| candidate.text() == variant.glyph.to_string())
                        })
                        .count();
                    assert_eq!(
                        members,
                        expected.len(),
                        "{style:?} on {reading:?} listed a family member more than once"
                    );
                }
            }
        }
    }

    /// The family rewrites one reading list, not the ranking.  A reading that
    /// is not a punctuation mark has to convert identically under every
    /// setting, or the feature has moved ordinary candidates around.
    #[test]
    fn an_ordinary_reading_converts_identically_under_every_punctuation_style() {
        let bytes = single_kanji_fixture();
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let listed = |converter: &mut Converter, style: PunctuationStyle| {
            converter
                .convert_detailed(
                    &dictionary,
                    "ひ",
                    ConversionOptions {
                        punctuation: style,
                        max_candidates: 8,
                        ..ConversionOptions::default()
                    },
                )
                .expect("conversion")
                .candidates()
                .iter()
                .map(|candidate| (candidate.text().to_owned(), candidate.cost))
                .collect::<Vec<_>>()
        };
        let baseline = listed(&mut converter, PunctuationStyle::default());
        assert!(!baseline.is_empty(), "the fixture must convert");
        for comma in CommaMark::ALL {
            for period in PeriodMark::ALL {
                let style = PunctuationStyle::new(comma, period);
                assert_eq!(listed(&mut converter, style), baseline, "{style:?}");
            }
        }
    }

    /// A reading that merely contains a punctuation mark is an ordinary
    /// sentence, not a request for the family.
    #[test]
    fn only_a_reading_that_is_itself_one_mark_opens_the_family() {
        let bytes = synthetic_dictionary(&[fixture_entry("ひ", "日", 100, EntryFlags::NONE)]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let style = PunctuationStyle::new(CommaMark::HalfWidth, PeriodMark::HalfWidth);
        for reading in ["\u{3001}ひ", "ひ\u{3001}", "\u{3001}\u{3001}", "ひ"] {
            let candidates = converter
                .convert_detailed(
                    &dictionary,
                    reading,
                    ConversionOptions {
                        punctuation: style,
                        ..ConversionOptions::default()
                    },
                )
                .expect("conversion")
                .candidates()
                .iter()
                .map(|candidate| candidate.text().to_owned())
                .collect::<Vec<_>>();
            assert!(
                !candidates.iter().any(|text| text == ","),
                "{reading:?} must not be offered the comma family: {candidates:?}"
            );
        }
    }

    #[test]
    fn an_image_without_the_tables_converts_exactly_as_before() {
        let rows = [fixture_entry("ひ", "日", 100, EntryFlags::NONE)];
        let listed = converted(&synthetic_dictionary(&rows), "ひ", 8);
        assert!(
            listed.iter().all(|(_, annotation)| annotation != "単漢字"),
            "{listed:?}"
        );
    }

    /// Small in-test image writer.  Keeping it here avoids adding the
    /// allocating `dictc` crate as a sakura-core dependency while still
    /// exercising the borrowed production dictionary parser and lattice.
    fn synthetic_dictionary(rows: &[FixtureEntry]) -> Vec<u8> {
        synthetic_image(rows, Vec::new())
    }

    /// Same image writer, plus the optional single-kanji tables.
    ///
    /// `single_kanji` pairs a reading with the characters it lists, and
    /// `variants` relates a character to another with a rule code. Both must
    /// already be in the ascending order the reader binary-searches, exactly
    /// as `dictc` emits them.
    fn synthetic_dictionary_with_single_kanji(
        rows: &[FixtureEntry],
        single_kanji: &[(&str, &str)],
        variants: &[(char, char, u8)],
    ) -> Vec<u8> {
        let mut index = Vec::new();
        let mut reading_data: Vec<u8> = Vec::new();
        let mut characters: Vec<u8> = Vec::new();
        let mut character_total = 0usize;
        for (reading, listed) in single_kanji {
            put_u32(&mut index, reading_data.len() as u32);
            put_u32(&mut index, character_total as u32);
            put_u16(&mut index, reading.len() as u16);
            put_u16(&mut index, listed.chars().count() as u16);
            reading_data.extend_from_slice(reading.as_bytes());
            for character in listed.chars() {
                put_u32(&mut characters, character as u32);
                character_total += 1;
            }
        }
        let mut variant_data = Vec::new();
        for (variant, original, kind) in variants {
            put_u32(&mut variant_data, *variant as u32);
            put_u32(&mut variant_data, *original as u32);
            variant_data.push(*kind);
            variant_data.extend_from_slice(&[0, 0, 0]);
        }
        let reading_bytes = reading_data.len();
        synthetic_image(
            rows,
            vec![
                (
                    image_format::TAG_SINGLE_KANJI_INDEX,
                    index,
                    single_kanji.len(),
                ),
                (
                    image_format::TAG_SINGLE_KANJI_READINGS,
                    reading_data,
                    reading_bytes,
                ),
                (
                    image_format::TAG_SINGLE_KANJI_CHARS,
                    characters,
                    character_total,
                ),
                (
                    image_format::TAG_SINGLE_KANJI_VARIANTS,
                    variant_data,
                    variants.len(),
                ),
            ],
        )
    }

    /// `extra` holds already-encoded optional tables, appended to the
    /// directory in the order the caller lists them.
    fn synthetic_image(rows: &[FixtureEntry], extra: Vec<([u8; 4], Vec<u8>, usize)>) -> Vec<u8> {
        let mut rows = rows.to_vec();
        rows.sort_by(|left, right| {
            (&left.reading, &left.surface, left.cost).cmp(&(
                &right.reading,
                &right.surface,
                right.cost,
            ))
        });

        let mut trie = vec![FixtureTrieNode {
            label: '\0',
            ..FixtureTrieNode::default()
        }];
        for (entry_index, row) in rows.iter().enumerate() {
            let mut node = 0usize;
            for character in row.reading.chars() {
                let child = if let Some(child) = trie[node].children.get(&character).copied() {
                    child
                } else {
                    let child = trie.len();
                    trie.push(FixtureTrieNode {
                        label: character,
                        ..FixtureTrieNode::default()
                    });
                    trie[node].children.insert(character, child);
                    child
                };
                node = child;
            }
            trie[node].entries.push(entry_index);
        }

        let mut order = Vec::with_capacity(trie.len());
        let mut queue = VecDeque::from([0usize]);
        while let Some(old) = queue.pop_front() {
            order.push(old);
            queue.extend(trie[old].children.values().copied());
        }
        let mut old_to_new = vec![0usize; trie.len()];
        for (new, old) in order.iter().copied().enumerate() {
            old_to_new[old] = new;
        }

        let node_count = order.len();
        let louds_bits = node_count * 2 - 1;
        let mut louds = vec![0u8; 4 + louds_bits.div_ceil(8)];
        put_u32_at(&mut louds, 0, louds_bits as u32);
        let mut bit = 0usize;
        let mut nodes = Vec::with_capacity(node_count * image_format::NODE_LEN);
        let mut labels = Vec::with_capacity(node_count * 4);
        for old in order.iter().copied() {
            let node = &trie[old];
            let first_child = node
                .children
                .values()
                .next()
                .map(|child| old_to_new[*child])
                .unwrap_or(0);
            put_u32(&mut nodes, first_child as u32);
            put_u16(&mut nodes, node.children.len() as u16);
            put_u16(&mut nodes, node.entries.len() as u16);
            put_u32(
                &mut nodes,
                node.entries.first().copied().unwrap_or(0) as u32,
            );
            put_u32(&mut nodes, 0);
            put_u32(&mut labels, node.label as u32);
            for _ in &node.children {
                louds[4 + bit / 8] |= 1 << (bit % 8);
                bit += 1;
            }
            bit += 1;
        }

        let mut surfaces = rows
            .iter()
            .map(|row| row.surface.clone())
            .collect::<Vec<_>>();
        surfaces.sort();
        surfaces.dedup();
        let mut surface_offsets = Vec::with_capacity(surfaces.len() * 4);
        let mut surface_data = Vec::new();
        for surface in &surfaces {
            put_u32(&mut surface_offsets, surface_data.len() as u32);
            put_u16(&mut surface_data, 0);
            put_u16(&mut surface_data, surface.len() as u16);
            surface_data.extend_from_slice(surface.as_bytes());
        }

        let mut entries = Vec::with_capacity(rows.len() * image_format::ENTRY_LEN);
        for row in &rows {
            let surface_id = surfaces
                .binary_search(&row.surface)
                .expect("fixture surface") as u32;
            put_u32(&mut entries, surface_id);
            put_u16(&mut entries, 0);
            put_u16(&mut entries, 0);
            put_i32(&mut entries, row.cost);
            put_i32(&mut entries, i32::MAX);
            put_u16(&mut entries, row.flags.bits());
            put_u16(&mut entries, 0);
            put_u32(&mut entries, image_format::NO_ANNOTATION);
        }

        let mut matrix = Vec::new();
        matrix.extend_from_slice(&image_format::MATRIX_MAGIC);
        put_u16(&mut matrix, 1);
        put_u16(&mut matrix, 0);
        put_u32(&mut matrix, 0);
        put_u32(&mut matrix, 0);
        put_u16(&mut matrix, 0);
        matrix.resize(20, 0);
        put_u32(&mut matrix, 0);
        put_u32(&mut matrix, 0);

        let mut tables = vec![
            (image_format::TAG_LOUDS, louds, louds_bits),
            (image_format::TAG_NODES, nodes, node_count),
            (image_format::TAG_LABELS, labels, node_count),
            (image_format::TAG_ENTRIES, entries, rows.len()),
            (
                image_format::TAG_SURFACE_OFFSETS,
                surface_offsets,
                surfaces.len(),
            ),
            (image_format::TAG_SURFACES, surface_data, surfaces.len()),
            (image_format::TAG_ANNOTATION_OFFSETS, Vec::new(), 0),
            (image_format::TAG_ANNOTATIONS, Vec::new(), 0),
            (image_format::TAG_MATRIX, matrix, 1),
        ];
        tables.extend(extra);
        let prefix = image_format::HEADER_LEN + tables.len() * image_format::DIRECTORY_ENTRY_LEN;
        let mut image = vec![0u8; prefix];
        let mut directory = Vec::with_capacity(tables.len());
        for (tag, bytes, count) in tables {
            while !image.len().is_multiple_of(8) {
                image.push(0);
            }
            let offset = image.len();
            image.extend_from_slice(&bytes);
            directory.push((tag, offset, bytes.len(), count));
        }
        image[0..8].copy_from_slice(&image_format::MAGIC);
        put_u16_at(&mut image, 8, image_format::VERSION);
        put_u16_at(&mut image, 10, image_format::HEADER_LEN as u16);
        put_u16_at(&mut image, 12, directory.len() as u16);
        put_u16_at(&mut image, 14, 1);
        put_u32_at(&mut image, 16, rows.len() as u32);
        put_u32_at(&mut image, 20, node_count as u32);
        let image_len = image.len() as u32;
        put_u32_at(&mut image, 24, image_len);
        put_u32_at(&mut image, 28, 0);
        for (index, (tag, offset, len, count)) in directory.into_iter().enumerate() {
            let at = image_format::HEADER_LEN + index * image_format::DIRECTORY_ENTRY_LEN;
            image[at..at + 4].copy_from_slice(&tag);
            put_u32_at(&mut image, at + 4, offset as u32);
            put_u32_at(&mut image, at + 8, len as u32);
            put_u32_at(&mut image, at + 12, count as u32);
        }
        image
    }

    fn put_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn put_i32(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u16_at(out: &mut [u8], at: usize, value: u16) {
        out[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32_at(out: &mut [u8], at: usize, value: u32) {
        out[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn local_plan(plan_id: u8, original: &str, corrected: &str, tier: RepairTier) -> RawRepairPlan {
        let runs = [CorrectionRun::replace(
            0,
            u16::try_from(corrected.len()).expect("corrected length"),
            0,
            u16::try_from(original.len()).expect("original length"),
        )];
        let map = CorrectionMap::new(original, corrected, &runs).expect("map");
        RawRepairPlan::new(plan_id, corrected, map, tier).expect("plan")
    }

    #[test]
    fn correction_map_projects_only_forward_complete_boundaries() {
        let original = "ないkにいく";
        let corrected = "ないかにいく";
        let runs = [
            CorrectionRun::equal(0, 6, 0, 6),
            CorrectionRun::replace(6, 9, 6, 7),
            CorrectionRun::equal(9, 18, 7, 16),
        ];
        let map = CorrectionMap::new(original, corrected, &runs).expect("valid map");
        assert_eq!(map.project_corrected_range(0, 12), Some((0, 10)));
        assert_eq!(map.project_corrected_range(12, 18), Some((10, 16)));
        assert_eq!(map.project_corrected_range(7, 12), None);
        assert_eq!(map.project_corrected_range(0, 18), Some((0, 16)));
        assert_eq!(map.project_corrected_range(1, 3), None);
    }

    #[test]
    fn correction_map_rejects_mismatched_snapshot_and_invalid_runs() {
        let original = "ないkにいく";
        let corrected = "ないかにいく";
        let runs = [
            CorrectionRun::equal(0, 6, 0, 6),
            CorrectionRun::replace(6, 9, 6, 7),
            CorrectionRun::equal(9, 18, 7, 16),
        ];
        let map = CorrectionMap::new(original, corrected, &runs).expect("valid map");
        assert_eq!(
            map.validate_for_readings("ないkにいる", corrected),
            Err(CorrectionMapError::EqualRunMismatch)
        );
        assert_eq!(
            CorrectionMap::new(original, corrected, &[CorrectionRun::replace(0, 6, 0, 6)]),
            Err(CorrectionMapError::ReplaceRunUnchanged)
        );
        let replacement_only = CorrectionMap::new(
            "あき",
            "あか",
            &[CorrectionRun::replace(
                0,
                "あか".len() as u16,
                0,
                "あき".len() as u16,
            )],
        )
        .expect("replacement-only map");
        assert_eq!(
            replacement_only.validate_for_readings("いき", "あか"),
            Err(CorrectionMapError::SnapshotMismatch)
        );
    }

    #[test]
    fn candidate_authority_is_strictly_direct_then_local_then_general() {
        assert!(CandidateAuthority::Direct.rank() > CandidateAuthority::LocalRawCompletion.rank());
        assert!(
            CandidateAuthority::LocalRawCompletion.rank()
                > CandidateAuthority::GeneralSingleInsertion.rank()
        );
        assert_eq!(
            CandidateOrigin::RawRepair {
                plan_id: 1,
                tier: RepairTier::LocalCompletion,
            }
            .authority(),
            CandidateAuthority::LocalRawCompletion
        );
    }

    fn cross_commit_fixture() -> Vec<u8> {
        synthetic_dictionary(&[
            fixture_entry("もれ", "漏れ", 0, EntryFlags::NONE),
            fixture_entry("ないか", "内科", 50, EntryFlags::NONE),
            fixture_entry("ないか", "内か", 70, EntryFlags::NONE),
            fixture_entry("ないか", "ないか", 100, EntryFlags::NONE),
            fixture_entry("ないか", "無いか", 110, EntryFlags::NONE),
            // These whole-reading entries are the evidence that cannot exist
            // in a lattice beginning at the current reading's byte zero.
            fixture_entry("もれないか", "漏れないか", 10, EntryFlags::NONE),
        ])
    }

    fn issue_83_bridge<'a>() -> CrossCommitBridge<'a> {
        CrossCommitBridge {
            tail_reading: "もれ",
            tail_surface: "漏れ",
            prefix_right_id: RightContextId::new(0),
            prefix_cost: 0,
        }
    }

    #[test]
    fn cross_commit_bridge_rescores_only_reachable_current_candidates() {
        let bytes = cross_commit_fixture();
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();

        let baseline = converter
            .convert(&dictionary, "ないか", ConversionOptions::default())
            .expect("baseline conversion");
        assert_eq!(baseline[0].text(), "内科");

        let result = converter
            .convert_with_user_dictionary_input_bridge_detailed(
                &dictionary,
                None,
                ConversionInput::ordinary("ないか"),
                ConversionOptions::default(),
                Some(issue_83_bridge()),
            )
            .expect("bridged conversion");
        let surfaces: Vec<&str> = result
            .candidates()
            .iter()
            .map(|candidate| candidate.text())
            .collect();
        let plain = surfaces
            .iter()
            .position(|surface| *surface == "ないか")
            .unwrap();
        let negative = surfaces
            .iter()
            .position(|surface| *surface == "無いか")
            .unwrap();
        let clinic = surfaces
            .iter()
            .position(|surface| *surface == "内科")
            .unwrap();
        let mixed = surfaces
            .iter()
            .position(|surface| *surface == "内か")
            .unwrap();
        assert!(plain < clinic && negative < clinic, "{surfaces:?}");
        assert!(
            clinic < mixed,
            "one-kana overlap must not inherit the bridge: {surfaces:?}"
        );
        assert_eq!(result.candidates()[plain].segments()[0].reading_start, 0);
        assert_eq!(
            usize::from(
                result.candidates()[plain]
                    .segments()
                    .last()
                    .unwrap()
                    .reading_end
            ),
            "ないか".len()
        );
        let diagnostics = result.diagnostics();
        assert!(diagnostics.cross_commit_bridge_attempted);
        assert_eq!(diagnostics.cross_commit_bridge_candidates_rescored, 2);
    }

    #[test]
    fn cross_commit_bridge_is_lexeme_agnostic() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("けんとう", "検討", 0, EntryFlags::NONE),
            fixture_entry("しますか", "シマスカ", 50, EntryFlags::NONE),
            fixture_entry("しますか", "しますか", 100, EntryFlags::NONE),
            fixture_entry("けんとうしますか", "検討しますか", 10, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();

        let baseline = converter
            .convert(&dictionary, "しますか", ConversionOptions::default())
            .expect("baseline conversion");
        assert_eq!(baseline[0].text(), "シマスカ");

        let result = converter
            .convert_with_user_dictionary_input_bridge_detailed(
                &dictionary,
                None,
                ConversionInput::ordinary("しますか"),
                ConversionOptions::default(),
                Some(CrossCommitBridge {
                    tail_reading: "けんとう",
                    tail_surface: "検討",
                    prefix_right_id: RightContextId::new(0),
                    prefix_cost: 0,
                }),
            )
            .expect("bridged conversion");
        assert_eq!(result.candidates()[0].text(), "しますか");
        assert!(result.diagnostics().cross_commit_bridge_spanning_paths > 0);
        assert!(result.diagnostics().cross_commit_bridge_candidates_rescored > 0);
    }

    #[test]
    fn commit_bridge_tail_preserves_the_exact_final_raw_edge() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("まえ", "前", 3, EntryFlags::NONE),
            fixture_entry("あと", "後", 7, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let candidates = converter
            .convert(&dictionary, "まえあと", ConversionOptions::default())
            .expect("conversion");
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.text() == "前後")
            .expect("composite system candidate");
        assert_eq!(candidate.system_entry_index(), None, "path has two edges");
        let tail = candidate
            .commit_bridge_tail(&dictionary)
            .expect("exact final edge evidence");
        assert_eq!(usize::from(tail.reading_start), "まえ".len());
        assert_eq!(usize::from(tail.text_start), "前".len());
        assert_eq!(tail.prefix_right_id, RightContextId::new(0));
        assert_eq!(tail.prefix_cost, 7);
    }

    #[test]
    fn cross_commit_bridge_fits_128_kib_thread_stack() {
        let bytes = cross_commit_fixture();
        // Production keeps converter arenas in ConversionService's boxed slot
        // pool; the worker stack only borrows a slot for the conversion call.
        // Mirror that ownership so this boundary measures the hot path rather
        // than constructing the reusable arena on the constrained stack.
        let converter = Box::new(Converter::new());
        let handle = std::thread::Builder::new()
            .name("conversion-bridge-128k".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || {
                let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
                let mut converter = converter;
                let result = converter
                    .convert_with_user_dictionary_input_bridge_detailed(
                        &dictionary,
                        None,
                        ConversionInput::ordinary("ないか"),
                        ConversionOptions::default(),
                        Some(issue_83_bridge()),
                    )
                    .expect("128 KiB bridge conversion");
                assert!(result.diagnostics().cross_commit_bridge_attempted);
            })
            .expect("128 KiB bridge thread");
        handle.join().expect("128 KiB bridge conversion thread");
    }

    #[test]
    fn cross_commit_bridge_mismatch_and_budget_exhaustion_fail_closed() {
        let bytes = cross_commit_fixture();
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let baseline = converter
            .convert(&dictionary, "ないか", ConversionOptions::default())
            .expect("baseline conversion")
            .to_vec();

        let mismatched = CrossCommitBridge {
            tail_surface: "洩れ",
            ..issue_83_bridge()
        };
        let result = converter
            .convert_with_user_dictionary_input_bridge_detailed(
                &dictionary,
                None,
                ConversionInput::ordinary("ないか"),
                ConversionOptions::default(),
                Some(mismatched),
            )
            .expect("mismatched bridge conversion");
        assert_eq!(result.candidates(), baseline.as_slice());
        assert_eq!(
            result.diagnostics().cross_commit_bridge_candidates_rescored,
            0
        );

        converter.set_cross_commit_budgets_for_test(0, 0);
        let exhausted = converter
            .convert_with_user_dictionary_input_bridge_detailed(
                &dictionary,
                None,
                ConversionInput::ordinary("ないか"),
                ConversionOptions::default(),
                Some(issue_83_bridge()),
            )
            .expect("budget-exhausted bridge conversion");
        assert_eq!(exhausted.candidates(), baseline.as_slice());
        assert!(exhausted.diagnostics().cross_commit_bridge_attempted);
        assert_eq!(
            exhausted.diagnostics().cross_commit_bridge_terminal,
            Some(super::ConversionSearchTerminal::LatticeBudgetReached)
        );
        assert_eq!(
            exhausted
                .diagnostics()
                .cross_commit_bridge_candidates_rescored,
            0
        );

        converter.set_cross_commit_budgets_for_test(super::MAX_CROSS_COMMIT_LATTICE_NODES, 0);
        let state_exhausted = converter
            .convert_with_user_dictionary_input_bridge_detailed(
                &dictionary,
                None,
                ConversionInput::ordinary("ないか"),
                ConversionOptions::default(),
                Some(issue_83_bridge()),
            )
            .expect("state-budget-exhausted bridge conversion");
        assert_eq!(state_exhausted.candidates(), baseline.as_slice());
        assert_eq!(
            state_exhausted.diagnostics().cross_commit_bridge_terminal,
            Some(super::ConversionSearchTerminal::StateBudgetReached)
        );
        assert_eq!(
            state_exhausted
                .diagnostics()
                .cross_commit_bridge_candidates_rescored,
            0
        );
    }

    #[test]
    fn invalid_or_oversized_cross_commit_evidence_is_not_replayed() {
        let bytes = cross_commit_fixture();
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let tail = "あ".repeat(super::MAX_CROSS_COMMIT_TAIL_BYTES / 3 + 1);
        let surface = "あ".repeat(super::MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES / 3 + 1);
        let current = "あ".repeat(super::MAX_CROSS_COMMIT_CURRENT_BYTES / 3 + 1);
        let mut converter = Converter::new();
        let baseline = converter
            .convert(&dictionary, "ないか", ConversionOptions::default())
            .expect("baseline conversion")
            .to_vec();
        {
            let mut assert_rejected = |bridge: CrossCommitBridge<'_>| {
                let result = converter
                    .convert_with_user_dictionary_input_bridge_detailed(
                        &dictionary,
                        None,
                        ConversionInput::ordinary("ないか"),
                        ConversionOptions::default(),
                        Some(bridge),
                    )
                    .expect("invalid bridge conversion");
                assert_eq!(result.candidates(), baseline.as_slice());
                assert!(!result.diagnostics().cross_commit_bridge_attempted);
            };
            assert_rejected(CrossCommitBridge {
                tail_reading: &tail,
                ..issue_83_bridge()
            });
            assert_rejected(CrossCommitBridge {
                tail_surface: &surface,
                ..issue_83_bridge()
            });
            assert_rejected(CrossCommitBridge {
                tail_reading: "の",
                tail_surface: "の",
                ..issue_83_bridge()
            });
            assert_rejected(CrossCommitBridge {
                prefix_right_id: RightContextId::new(1),
                ..issue_83_bridge()
            });
            assert_rejected(CrossCommitBridge {
                prefix_cost: -1,
                ..issue_83_bridge()
            });
            assert_rejected(CrossCommitBridge {
                prefix_cost: i64::MAX,
                ..issue_83_bridge()
            });
        }

        let oversized_baseline = converter
            .convert(&dictionary, &current, ConversionOptions::default())
            .expect("oversized-current baseline")
            .to_vec();
        let result = converter
            .convert_with_user_dictionary_input_bridge_detailed(
                &dictionary,
                None,
                ConversionInput::ordinary(&current),
                ConversionOptions::default(),
                Some(issue_83_bridge()),
            )
            .expect("oversized-current bridge conversion");
        assert_eq!(result.candidates(), oversized_baseline.as_slice());
        assert!(!result.diagnostics().cross_commit_bridge_attempted);
    }

    #[test]
    fn absent_bridge_and_user_or_exact_candidates_keep_their_authority() {
        let bytes = cross_commit_fixture();
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let user_dictionary = UserDictionary::parse_tsv(
            "# format-version: 1\nreading\tsurface\tpos\tcomment\nないか\t利用者語\tnoun\t\n",
        )
        .expect("user dictionary");
        let mut converter = Converter::new();
        let baseline = converter
            .convert_with_user_dictionary(
                &dictionary,
                Some(&user_dictionary),
                "ないか",
                ConversionOptions::default(),
            )
            .expect("baseline user conversion")
            .to_vec();
        let absent = converter
            .convert_with_user_dictionary_input_bridge_detailed(
                &dictionary,
                Some(&user_dictionary),
                ConversionInput::ordinary("ないか"),
                ConversionOptions::default(),
                None,
            )
            .expect("absent bridge conversion");
        assert_eq!(absent.candidates(), baseline.as_slice());

        let bridged = converter
            .convert_with_user_dictionary_input_bridge_detailed(
                &dictionary,
                Some(&user_dictionary),
                ConversionInput::ordinary("ないか"),
                ConversionOptions::default(),
                Some(issue_83_bridge()),
            )
            .expect("bridged user conversion");
        assert_eq!(bridged.candidates(), baseline.as_slice());
        assert!(!bridged.diagnostics().cross_commit_bridge_attempted);
        let user = bridged
            .candidates()
            .iter()
            .find(|candidate| candidate.text() == "利用者語")
            .expect("user candidate remains reachable");
        assert_eq!(user.path_evidence().user_edges, 1);
        assert!(!user.was_cross_commit_rescored());

        let exact = converter
            .convert_with_user_dictionary_input_bridge_detailed(
                &dictionary,
                None,
                ConversionInput::new(
                    "esp32",
                    "ESP32",
                    ConversionInputClass::OpaqueAsciiIdentifier,
                    LiteralPolicy::ExactTop1,
                ),
                ConversionOptions::default(),
                Some(issue_83_bridge()),
            )
            .expect("exact policy conversion");
        assert_eq!(exact.candidates()[0].text(), "ESP32");
        assert!(exact.candidates()[0].is_synthetic_exact());
        assert!(!exact.diagnostics().cross_commit_bridge_attempted);
    }

    #[test]
    fn classified_literal_inputs_validate_only_their_checked_policy_pair() {
        assert_eq!(ConversionInput::ordinary("かな").validate(), Ok(()));
        assert_eq!(
            ConversionInput::new(
                "esp32",
                "ESP32",
                ConversionInputClass::OpaqueAsciiIdentifier,
                LiteralPolicy::ExactTop1,
            )
            .validate(),
            Ok(())
        );
        assert_eq!(
            ConversionInput::new(
                "rおぐ",
                "rおぐ",
                ConversionInputClass::MixedUnresolvedLatin,
                LiteralPolicy::ExactOnly,
            )
            .validate(),
            Ok(())
        );

        let invalid = [
            ConversionInput::new(
                "esp32",
                "ESP32",
                ConversionInputClass::OpaqueAsciiIdentifier,
                LiteralPolicy::ExactOnly,
            ),
            ConversionInput::new(
                "esp32",
                "ESP-32",
                ConversionInputClass::OpaqueAsciiIdentifier,
                LiteralPolicy::ExactTop1,
            ),
            ConversionInput::new(
                "esp",
                "ESP",
                ConversionInputClass::OpaqueAsciiIdentifier,
                LiteralPolicy::ExactTop1,
            ),
            ConversionInput::new(
                "123",
                "123",
                ConversionInputClass::OpaqueAsciiIdentifier,
                LiteralPolicy::ExactTop1,
            ),
            ConversionInput::new(
                "rおぐ",
                "xおぐ",
                ConversionInputClass::MixedUnresolvedLatin,
                LiteralPolicy::ExactOnly,
            ),
            ConversionInput::new(
                "かな",
                "かな",
                ConversionInputClass::MixedUnresolvedLatin,
                LiteralPolicy::ExactOnly,
            ),
        ];
        assert!(invalid
            .into_iter()
            .all(|input| input.validate() == Err(super::ConversionError::InvalidOptions)));
    }

    #[test]
    fn exact_only_returns_one_literal_and_consumes_one_shot_state() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("こんにちは", "こんにちは", 1, EntryFlags::NONE),
            fixture_entry("きょう", "今日", 1, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        converter.set_civil_date(crate::calendar::CivilDate::from_ymd(2026, 8, 19));
        converter.set_commit_repair_readings(&["こんにちは"]);
        let exact = converter
            .convert_input_detailed(
                &dictionary,
                ConversionInput::new(
                    "rおぐ",
                    "rおぐ",
                    ConversionInputClass::MixedUnresolvedLatin,
                    LiteralPolicy::ExactOnly,
                ),
                ConversionOptions::default(),
            )
            .expect("exact-only conversion");
        assert_eq!(exact.candidates().len(), 1);
        assert_eq!(exact.candidates()[0].text(), "rおぐ");
        assert!(exact.candidates()[0].is_synthetic_exact());

        let later = converter
            .convert(&dictionary, "あいう", ConversionOptions::default())
            .expect("ordinary conversion after exact-only");
        assert!(!later
            .iter()
            .any(|candidate| candidate.text() == "こんにちは"));
        let calendar = converter
            .convert(&dictionary, "きょう", ConversionOptions::default())
            .expect("ordinary conversion after one-shot date");
        assert!(calendar
            .iter()
            .all(|candidate| !candidate.path_evidence().generated_edges.gt(&0)));
    }

    #[test]
    fn whole_reading_lexical_number_form_stays_ahead_of_generated_duplicates() {
        let bytes =
            synthetic_dictionary(&[fixture_entry("いちにち", "一日", 2_000, EntryFlags::NONE)]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let candidates = converter
            .convert(&dictionary, "いちにち", ConversionOptions::default())
            .expect("numeric lexical conversion");
        let surfaces: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.text())
            .collect();
        let ranking: Vec<(&str, i64, Option<u32>, super::PathEvidence)> = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.text(),
                    candidate.cost,
                    candidate.system_entry_index(),
                    candidate.path_evidence(),
                )
            })
            .collect();

        assert_eq!(
            surfaces.first().copied(),
            Some("一日"),
            "unexpected numeric order: {ranking:?}"
        );
        assert_eq!(
            surfaces
                .iter()
                .filter(|surface| **surface == "一日")
                .count(),
            1
        );
        assert_eq!(
            surfaces.iter().filter(|surface| **surface == "1日").count(),
            1
        );
    }

    #[test]
    fn bos_filters_non_initial_fragments_but_compound_paths_can_use_them() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("ずかい", "図解", 1_000, EntryFlags::NONE),
            fixture_entry("ずかい", "使い", 100, EntryFlags::NON_INITIAL),
            fixture_entry("つかい", "使い", 100, EntryFlags::NONE),
            fixture_entry("き", "気", 100, EntryFlags::NONE),
            fixture_entry("づかい", "遣い", 100, EntryFlags::NON_INITIAL),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();

        let independent = converter
            .convert(&dictionary, "ずかい", ConversionOptions::default())
            .expect("independent conversion");
        assert!(independent
            .iter()
            .any(|candidate| candidate.text() == "図解"));
        assert!(independent
            .iter()
            .all(|candidate| candidate.text() != "使い"));

        let ordinary = converter
            .convert(&dictionary, "つかい", ConversionOptions::default())
            .expect("ordinary unvoiced conversion");
        assert!(ordinary.iter().any(|candidate| candidate.text() == "使い"));

        let compound = converter
            .convert(&dictionary, "きづかい", ConversionOptions::default())
            .expect("compound conversion");
        assert!(
            compound
                .iter()
                .any(|candidate| candidate.text() == "気遣い"),
            "non-initial fragment was lost inside a compound: {:?}",
            compound
                .iter()
                .map(|candidate| candidate.text())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn trustworthy_exact_word_suppresses_unconfirmed_repairs_and_costly_mosaics() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("ずかい", "図解", 1_000, EntryFlags::NONE),
            fixture_entry("ずがい", "頭蓋", 100, EntryFlags::NONE),
            fixture_entry("ず", "図", 3_000, EntryFlags::NONE),
            fixture_entry("か", "書", 3_000, EntryFlags::NONE),
            fixture_entry("い", "い", 3_000, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let candidates = converter
            .convert(&dictionary, "ずかい", ConversionOptions::default())
            .expect("exact conversion");
        let ranking = candidates
            .iter()
            .map(|candidate| (candidate.text(), candidate.cost, candidate.path_evidence()))
            .collect::<Vec<_>>();

        assert!(candidates
            .iter()
            .any(|candidate| candidate.text() == "図解"));
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.text() != "頭蓋"),
            "dakuten repair polluted an exact query: {ranking:?}"
        );
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.text() != "図書い"),
            "costly split mosaic survived an exact word: {ranking:?}"
        );
    }

    #[test]
    fn repair_candidate_remains_available_when_no_trustworthy_exact_word_exists() {
        let bytes = synthetic_dictionary(&[fixture_entry("ずがい", "頭蓋", 100, EntryFlags::NONE)]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        let candidates = converter
            .convert(&dictionary, "ずかい", ConversionOptions::default())
            .expect("repair-only conversion");
        let repaired = candidates
            .iter()
            .find(|candidate| candidate.text() == "頭蓋")
            .expect("dakuten repair remains available");
        assert!(repaired.path_evidence().has_repair_kind(RepairKind::Rule));
        assert!(repaired.path_evidence().has_unconfirmed_repair());
    }

    #[test]
    fn exact_top1_is_fixed_zero_and_admits_only_full_span_non_spelling_edges() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("esp32", "SystemExact", 1, EntryFlags::NONE),
            fixture_entry("esp32", "SpellingExact", 0, EntryFlags::SPELLING_CORRECTION),
            fixture_entry("esp", "Partial", 0, EntryFlags::NONE),
            fixture_entry("2", "GeneratedLike", 0, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let user_dictionary = UserDictionary::parse_tsv(
            "reading\tsurface\tpos\tcomment\nesp32\tUserExact\talphabet\t\n",
        )
        .expect("alphabet user dictionary");
        let input = ConversionInput::new(
            "esp32",
            "ESP32",
            ConversionInputClass::OpaqueAsciiIdentifier,
            LiteralPolicy::ExactTop1,
        );
        let mut converter = Converter::new();
        let result = converter
            .convert_with_user_dictionary_input_detailed(
                &dictionary,
                Some(&user_dictionary),
                input,
                ConversionOptions {
                    max_candidates: 4,
                    ..ConversionOptions::default()
                },
            )
            .expect("exact-top1 conversion");
        let candidates = result.candidates();
        assert!(!candidates.is_empty());
        assert_eq!(candidates[0].text(), "ESP32");
        assert!(candidates[0].is_synthetic_exact());
        assert!(candidates.len() <= 4);
        assert!(candidates
            .iter()
            .skip(1)
            .any(|candidate| candidate.text() == "SystemExact"));
        assert!(candidates
            .iter()
            .skip(1)
            .any(|candidate| candidate.text() == "UserExact"));
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.text() == "SpellingExact"));
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.text() == "Partial"));
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.text() == "GeneratedLike"));
        assert!(candidates.iter().skip(1).all(|candidate| {
            candidate.segments().len() == 1
                && candidate.segments()[0].reading_start == 0
                && candidate.segments()[0].reading_end == "esp32".len() as u16
        }));
    }

    #[test]
    fn raw_multi_pass_preserves_direct_order_and_admits_only_system_paths() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("あき", "SAME", 100, EntryFlags::NONE),
            fixture_entry("あき", "DIRECT", 200, EntryFlags::NONE),
            fixture_entry("あか", "SAME", 1, EntryFlags::NONE),
            fixture_entry("あか", "CHEAP", 2, EntryFlags::NONE),
            fixture_entry("あ", "A", 10, EntryFlags::NONE),
            fixture_entry("か", "C", 10, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let options = ConversionOptions {
            max_candidates: 9,
            raw_repair_budget: RawRepairBudget {
                max_corrected_passes: 2,
                max_repair_candidates: 4,
                max_lattice_nodes: 64,
                max_search_states: 128,
            },
            ..ConversionOptions::default()
        };
        let mut converter = Converter::new();
        let direct = converter
            .convert(&dictionary, "あき", options)
            .expect("direct conversion")
            .iter()
            .map(|candidate| candidate.text().to_owned())
            .collect::<Vec<_>>();
        let direct_count = direct.len();
        assert!(direct_count < options.max_candidates);

        let plan = local_plan(7, "あき", "あか", RepairTier::LocalCompletion);
        let result = converter
            .convert_with_raw_repair_plans(&dictionary, None, "あき", &[plan], options)
            .expect("one-slot conversion");
        let candidates = result.candidates();
        let diagnostics = result.diagnostics();
        assert_eq!(diagnostics.raw_repair_passes, 1);
        assert!(diagnostics.raw_repair_lattice_nodes > 0);
        assert!(diagnostics.raw_repair_search_states > 0);
        assert_eq!(
            candidates[..direct_count]
                .iter()
                .map(|candidate| candidate.text().to_owned())
                .collect::<Vec<_>>(),
            direct
        );
        assert!(candidates[..direct_count]
            .iter()
            .all(|candidate| candidate.origin() == CandidateOrigin::Direct));
        let same = candidates
            .iter()
            .find(|candidate| candidate.text() == "SAME")
            .expect("direct same-surface candidate");
        assert_eq!(same.origin(), CandidateOrigin::Direct);
        let cheap_index = candidates
            .iter()
            .position(|candidate| candidate.text() == "CHEAP")
            .expect("cheaper repaired candidate");
        assert!(cheap_index >= direct_count);
        assert_eq!(
            candidates[cheap_index].origin(),
            CandidateOrigin::RawRepair {
                plan_id: 7,
                tier: RepairTier::LocalCompletion,
            }
        );
        let compound = candidates
            .iter()
            .find(|candidate| candidate.text() == "AC")
            .expect("corrected multi-segment system candidate");
        assert_eq!(compound.segments().len(), 2);
        assert_eq!(compound.path_evidence().system_edges, 2);
        assert!(compound.has_full_system_coverage("あか".len()));
        assert!(candidates.iter().all(|candidate| {
            candidate.origin() == CandidateOrigin::Direct
                || candidate.path_evidence().is_system_only()
        }));
    }

    #[test]
    fn raw_multi_pass_preserves_mixed_exact_direct_before_admitted_repair() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("ないk", "HOSTILE", 0, EntryFlags::NONE),
            fixture_entry("ないか", "内科", 1, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let input = ConversionInput::new(
            "ないk",
            "ないk",
            ConversionInputClass::MixedUnresolvedLatin,
            LiteralPolicy::ExactOnly,
        );
        let plan = local_plan(8, "ないk", "ないか", RepairTier::LocalCompletion);
        let mut converter = Converter::new();
        let result = converter
            .convert_input_with_raw_repair_plans(
                &dictionary,
                None,
                input,
                &[plan],
                ConversionOptions {
                    max_candidates: 9,
                    ..ConversionOptions::default()
                },
            )
            .expect("classified one-slot conversion");
        let candidates = result.candidates();
        let direct_count = candidates
            .iter()
            .take_while(|candidate| candidate.origin() == CandidateOrigin::Direct)
            .count();
        assert_eq!(direct_count, 1);
        assert_eq!(candidates[0].text(), "ないk");
        assert!(candidates[0].is_synthetic_exact());
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.text() == "HOSTILE"));
        let repaired = candidates
            .iter()
            .find(|candidate| candidate.text() == "内科")
            .expect("system-only corrected candidate");
        assert_eq!(
            repaired.origin(),
            CandidateOrigin::RawRepair {
                plan_id: 8,
                tier: RepairTier::LocalCompletion,
            }
        );
        assert_eq!(result.diagnostics().raw_repair_passes, 1);
        assert_eq!(result.diagnostics().raw_repair_candidates_added, 1);

        let mut exact_only_converter = Converter::new();
        let exact_only = exact_only_converter
            .convert_input_with_raw_repair_plans(
                &dictionary,
                None,
                input,
                &[local_plan(
                    9,
                    "ないk",
                    "ないか",
                    RepairTier::LocalCompletion,
                )],
                ConversionOptions {
                    max_candidates: 1,
                    ..ConversionOptions::default()
                },
            )
            .expect("full exact-only result");
        assert_eq!(exact_only.candidates().len(), 1);
        assert_eq!(exact_only.candidates()[0].text(), "ないk");
        assert!(exact_only.candidates()[0].is_synthetic_exact());
        assert_eq!(exact_only.diagnostics().raw_repair_passes, 0);
        assert_eq!(exact_only.diagnostics().raw_repair_candidates_added, 0);
    }

    #[test]
    fn raw_multi_pass_aggregate_budgets_bound_sequential_passes() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("あき", "DIRECT", 1, EntryFlags::NONE),
            fixture_entry("あか", "FIRST", 1, EntryFlags::NONE),
            fixture_entry("あく", "SECOND", 1, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let plans = [
            local_plan(10, "あき", "あか", RepairTier::LocalCompletion),
            local_plan(11, "あき", "あく", RepairTier::LocalCompletion),
        ];
        let options = ConversionOptions {
            max_candidates: 9,
            raw_repair_budget: RawRepairBudget {
                max_corrected_passes: 1,
                max_repair_candidates: 1,
                max_lattice_nodes: 64,
                max_search_states: 128,
            },
            ..ConversionOptions::default()
        };
        let mut converter = Converter::new();
        let result = converter
            .convert_with_raw_repair_plans(&dictionary, None, "あき", &plans, options)
            .expect("bounded multi-pass conversion");
        let diagnostics = result.diagnostics();
        assert_eq!(diagnostics.raw_repair_passes, 1);
        assert!(diagnostics.raw_repair_candidates_added <= 1);
        assert!(diagnostics.raw_repair_candidates_examined <= 1);
        assert!(diagnostics.raw_repair_lattice_nodes <= 64);
        assert!(diagnostics.raw_repair_search_states <= 128);
        assert!(!result
            .candidates()
            .iter()
            .any(|candidate| candidate.text() == "SECOND"));

        let lattice_limited = {
            let mut converter = Converter::new();
            let result = converter
                .convert_with_raw_repair_plans(
                    &dictionary,
                    None,
                    "あき",
                    &plans,
                    ConversionOptions {
                        max_candidates: 9,
                        raw_repair_budget: RawRepairBudget {
                            max_corrected_passes: 4,
                            max_repair_candidates: 9,
                            max_lattice_nodes: 1,
                            max_search_states: 128,
                        },
                        ..ConversionOptions::default()
                    },
                )
                .expect("lattice aggregate limit");
            result.diagnostics()
        };
        assert_eq!(lattice_limited.raw_repair_passes, 1);
        assert_eq!(lattice_limited.raw_repair_lattice_nodes, 1);

        let search_limited = {
            let mut converter = Converter::new();
            let result = converter
                .convert_with_raw_repair_plans(
                    &dictionary,
                    None,
                    "あき",
                    &plans,
                    ConversionOptions {
                        max_candidates: 9,
                        raw_repair_budget: RawRepairBudget {
                            max_corrected_passes: 4,
                            max_repair_candidates: 9,
                            max_lattice_nodes: 64,
                            max_search_states: 1,
                        },
                        ..ConversionOptions::default()
                    },
                )
                .expect("search aggregate limit");
            result.diagnostics()
        };
        assert_eq!(search_limited.raw_repair_passes, 1);
        assert_eq!(search_limited.raw_repair_search_states, 1);
    }

    #[test]
    fn raw_multi_pass_core_path_fits_128_kib_thread_stack() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("あき", "DIRECT", 1, EntryFlags::NONE),
            fixture_entry("あか", "CORRECTED", 1, EntryFlags::NONE),
        ]);
        // ConversionService owns each Converter in a boxed reusable slot. Do
        // not charge construction of that process-lifetime arena to the worker
        // stack whose conversion call this test is intended to bound.
        let converter = Box::new(Converter::new());
        let handle = std::thread::Builder::new()
            .name("conversion-raw-128k".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || {
                let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
                let plan = local_plan(12, "あき", "あか", RepairTier::LocalCompletion);
                let mut converter = converter;
                let result = converter
                    .convert_with_raw_repair_plans(
                        &dictionary,
                        None,
                        "あき",
                        &[plan],
                        ConversionOptions::default(),
                    )
                    .expect("128 KiB conversion");
                assert!(result.diagnostics().raw_repair_passes <= 1);
            })
            .expect("128 KiB thread");
        handle.join().expect("128 KiB conversion thread");
    }

    #[test]
    fn raw_multi_pass_skips_general_tier_and_keeps_full_direct_without_repair() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("あき", "ONE", 1, EntryFlags::NONE),
            fixture_entry("あき", "TWO", 2, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let plan = local_plan(1, "あき", "あか", RepairTier::GeneralSingleInsertion);
        let mut converter = Converter::new();
        let options = ConversionOptions {
            max_candidates: 4,
            ..ConversionOptions::default()
        };
        let general = converter
            .convert_with_raw_repair_plans(&dictionary, None, "あき", &[plan], options)
            .expect("general tier is a safe rejection");
        assert_eq!(general.diagnostics().raw_repair_passes, 0);
        assert_eq!(general.diagnostics().raw_repair_candidates_added, 0);
        assert!(general
            .candidates()
            .iter()
            .all(|candidate| candidate.origin() == CandidateOrigin::Direct));

        let full_options = ConversionOptions {
            max_candidates: 3,
            ..ConversionOptions::default()
        };
        let full = converter
            .convert_with_raw_repair_plans(
                &dictionary,
                None,
                "あき",
                &[local_plan(2, "あき", "あか", RepairTier::LocalCompletion)],
                full_options,
            )
            .expect("direct full result");
        assert_eq!(full.diagnostics().raw_repair_passes, 1);
        assert_eq!(full.diagnostics().raw_repair_candidates_added, 0);
        assert_eq!(
            full.candidates()
                .iter()
                .map(|candidate| candidate.text())
                .collect::<Vec<_>>(),
            vec!["ONE", "TWO", "あき"]
        );
    }

    #[test]
    fn raw_multi_pass_full_direct_reserves_one_tail_slot_for_best_local_repair() {
        let mut rows = (0..18)
            .map(|index| {
                fixture_entry(
                    "あき",
                    &format!("DIRECT-{index:02}"),
                    index,
                    EntryFlags::NONE,
                )
            })
            .collect::<Vec<_>>();
        for index in 0..4 {
            rows.push(fixture_entry(
                "あ",
                &format!("PREFIX-{index}"),
                100 + index,
                EntryFlags::NONE,
            ));
            rows.push(fixture_entry(
                "き",
                &format!("SUFFIX-{index}"),
                100 + index,
                EntryFlags::NONE,
            ));
        }
        rows.push(fixture_entry(
            "なずか",
            "OTHER-REPAIR",
            100,
            EntryFlags::NONE,
        ));
        rows.push(fixture_entry("なぜか", "内科", -100, EntryFlags::NONE));
        let bytes = synthetic_dictionary(&rows);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let plans = [
            local_plan(20, "あき", "なずか", RepairTier::LocalCompletion),
            local_plan(21, "あき", "なぜか", RepairTier::LocalCompletion),
        ];
        let options = ConversionOptions {
            max_candidates: 18,
            ..ConversionOptions::default()
        };
        let mut converter = Converter::new();
        let direct = converter
            .convert(&dictionary, "あき", options)
            .expect("full direct baseline")
            .iter()
            .map(|candidate| candidate.text().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(direct.len(), 18);
        let no_repair = converter
            .convert_with_raw_repair_plans(
                &dictionary,
                None,
                "あき",
                &[local_plan(
                    19,
                    "あき",
                    "なぬか",
                    RepairTier::LocalCompletion,
                )],
                options,
            )
            .expect("full direct result without an admissible repair");
        assert_eq!(
            no_repair
                .candidates()
                .iter()
                .map(|candidate| candidate.text().to_owned())
                .collect::<Vec<_>>(),
            direct
        );
        assert_eq!(no_repair.diagnostics().raw_repair_candidates_added, 0);
        let result = converter
            .convert_with_raw_repair_plans(&dictionary, None, "あき", &plans, options)
            .expect("full direct result with one repair reservation");
        let candidates = result.candidates();
        assert_eq!(candidates.len(), 18);
        assert_eq!(candidates[0].text(), direct[0]);
        assert_eq!(candidates[0].origin(), CandidateOrigin::Direct);
        assert_eq!(
            candidates[..17]
                .iter()
                .map(|candidate| candidate.text().to_owned())
                .collect::<Vec<_>>(),
            direct[..17]
        );
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.text() == direct[17]));
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.text() == "OTHER-REPAIR"));
        assert_eq!(candidates[17].text(), "内科");
        assert_eq!(
            candidates[17].origin(),
            CandidateOrigin::RawRepair {
                plan_id: 21,
                tier: RepairTier::LocalCompletion,
            }
        );
        assert_eq!(result.diagnostics().raw_repair_passes, 2);
        assert_eq!(result.diagnostics().raw_repair_candidates_added, 1);
    }

    #[test]
    fn raw_multi_pass_rejects_user_fallback_generated_and_spelling_paths() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("あき", "DIRECT", 1, EntryFlags::NONE),
            fixture_entry("あか", "SPELL", 1, EntryFlags::SPELLING_CORRECTION),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let user_dictionary = UserDictionary::parse_tsv(
            "# format-version: 1\nreading\tsurface\tpos\tcomment\nあか\tUSER\tnoun\t\n",
        )
        .expect("user dictionary");
        let options = ConversionOptions {
            max_candidates: 5,
            ..ConversionOptions::default()
        };
        let mut converter = Converter::new();
        let result = converter
            .convert_with_raw_repair_plans(
                &dictionary,
                Some(&user_dictionary),
                "あき",
                &[local_plan(3, "あき", "あか", RepairTier::LocalCompletion)],
                options,
            )
            .expect("repair rejection keeps direct result");
        assert_eq!(result.diagnostics().raw_repair_candidates_added, 0);
        assert!(result
            .candidates()
            .iter()
            .all(|candidate| candidate.origin() == CandidateOrigin::Direct));
        assert!(!result
            .candidates()
            .iter()
            .any(|candidate| candidate.text() == "USER"));
        assert!(!result
            .candidates()
            .iter()
            .any(|candidate| candidate.text() == "SPELL"));

        let fallback = converter
            .convert_with_raw_repair_plans(
                &dictionary,
                None,
                "あき",
                &[local_plan(4, "あき", "みす", RepairTier::LocalCompletion)],
                options,
            )
            .expect("fallback rejection keeps direct result");
        assert_eq!(fallback.diagnostics().raw_repair_candidates_added, 0);

        let generated = converter
            .convert_with_raw_repair_plans(
                &dictionary,
                None,
                "あき",
                &[local_plan(5, "あき", "2", RepairTier::LocalCompletion)],
                options,
            )
            .expect("generated rejection keeps direct result");
        assert_eq!(generated.diagnostics().raw_repair_candidates_added, 0);
    }

    #[test]
    fn raw_multi_pass_corrected_pass_skips_input_repair_and_consumes_one_shot_state() {
        let bytes = synthetic_dictionary(&[
            fixture_entry("x", "ORIGINAL", 1, EntryFlags::NONE),
            fixture_entry("こんにちは", "RULE_TARGET", 1, EntryFlags::NONE),
            fixture_entry("きょう", "今日", 1, EntryFlags::NONE),
        ]);
        let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
        let mut converter = Converter::new();
        converter.set_civil_date(crate::calendar::CivilDate::from_ymd(2026, 8, 19));
        let options = ConversionOptions {
            max_candidates: 5,
            ..ConversionOptions::default()
        };
        let result = converter
            .convert_with_raw_repair_plans(
                &dictionary,
                None,
                "x",
                &[local_plan(6, "x", "こにちは", RepairTier::LocalCompletion)],
                options,
            )
            .expect("skip-input-repair corrected pass");
        assert_eq!(result.diagnostics().raw_repair_passes, 1);
        assert_eq!(result.diagnostics().raw_repair_candidates_added, 0);
        assert!(!result
            .candidates()
            .iter()
            .any(|candidate| candidate.text() == "RULE_TARGET"));

        // The one-shot date was consumed by the direct pass; a subsequent
        // direct request must not inherit date surfaces from this request.
        let later = converter
            .convert(&dictionary, "きょう", options)
            .expect("later conversion");
        assert!(later
            .iter()
            .all(|candidate| !candidate.text().contains("2026年")));

        let mut invalid_request = Converter::new();
        invalid_request.set_civil_date(crate::calendar::CivilDate::from_ymd(2026, 8, 19));
        assert!(invalid_request
            .convert(
                &dictionary,
                "きょう",
                ConversionOptions {
                    max_candidates: 0,
                    ..options
                },
            )
            .is_err());
        let after_invalid = invalid_request
            .convert(&dictionary, "きょう", options)
            .expect("conversion after rejected request");
        assert!(after_invalid
            .iter()
            .all(|candidate| !candidate.text().contains("2026年")));
    }
}
