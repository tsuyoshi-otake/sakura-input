//! Bounded lattice conversion with Viterbi top-1 and A* N-best search.
//!
//! The dictionary stays borrowed and mapped. All arenas are allocated once by
//! [`Converter::new`] and cleared between queries, so steady-state conversion
//! does not grow the heap. Every search is finite: lattice nodes, A* states,
//! candidates, and text are independently bounded.

use core::cmp::Ordering;
use std::collections::BinaryHeap;

#[cfg(not(feature = "research-top32"))]
use sakura_proto::MAX_CANDIDATES;
use sakura_proto::{FixedStr, FixedVec, MAX_PREEDIT_BYTES, MAX_SEGMENTS};

use crate::calendar::{date_offset_for_reading, date_surface_specs, CivilDate};
use crate::dictionary::{Dictionary, Entry, EntryFlags};
use crate::input_repair::{
    allows_system_entry, collect_repair_variants, english_spelling_katakana_reading,
    COMMIT_HISTORY_PENALTY, ENGLISH_KATAKANA_PENALTY, MAX_REPAIR_VARIANTS,
};
use crate::numerals::{
    is_decorative_numeral_char, is_numeric_day_surface, parse_numeric_prefix,
    should_emit_numeric_span, NumericSpan, NUMERIC_STYLES,
};
use crate::preferences::ConversionMethod;
use crate::user_dictionary::UserDictionary;
use crate::TextSink;

const NONE: usize = usize::MAX;
const NONE_STATE: u32 = u32::MAX;
const MAX_LATTICE_NODES: usize = 32_768;
const MAX_SEARCH_STATES: usize = 65_536;
const MAX_DICTIONARY_EDGES_PER_READING: usize = 12;
#[cfg(not(feature = "research-top32"))]
pub const MAX_CONVERSION_CANDIDATES: usize = MAX_CANDIDATES;
#[cfg(feature = "research-top32")]
pub const MAX_CONVERSION_CANDIDATES: usize = 32;
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
/// Once an atomic whole-reading entry exists, a much more expensive all-system
/// segmentation is usually a mosaic of individually valid short words. The
/// atomic gate is deliberately narrower than "any exact phrase": long Japanese
/// compounds still need legitimate split alternatives. Whole-reading, user,
/// generated, and lossless candidates are protected.
const EXACT_LEXICAL_COMPOSITE_COST_WINDOW: i64 = 4_000;
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
}

impl PathEvidence {
    pub const fn is_system_only(self) -> bool {
        self.system_edges > 0
            && self.user_edges == 0
            && self.fallback_edges == 0
            && self.generated_edges == 0
            && self.spelling_edges == 0
    }

    fn add_system(&mut self, spelling: bool) {
        if spelling {
            self.spelling_edges = self.spelling_edges.saturating_add(1);
        } else {
            self.system_edges = self.system_edges.saturating_add(1);
        }
    }

    fn add_surface(&mut self, surface: Surface, spelling: bool) {
        match surface {
            Surface::Dictionary { .. } => self.add_system(spelling),
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
    /// Set only for a literal candidate injected by an exact literal policy.
    /// It is distinct from the ordinary lossless fallback so callers can
    /// prevent an emergency/raw-preservation result from entering learning.
    synthetic_exact: bool,
    origin: CandidateOrigin,
    path_evidence: PathEvidence,
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
        self.system_entry_index
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
    Dictionary { entry: Entry, entry_index: u32 },
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
    sequence: u64,
    initial_right_id: u16,
    lattice_node_budget: usize,
    search_state_budget: usize,
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
            sequence: 0,
            initial_right_id: 0,
            lattice_node_budget: MAX_LATTICE_NODES,
            search_state_budget: MAX_SEARCH_STATES,
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
                    self.apply_exact_lexical_quality_gate();
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

    /// Protects whole-reading lexical evidence from the low-information tail
    /// of a fully lexical N-best search. Without this gate an exact loanword
    /// can be followed by arbitrary one-character + unit-name mosaics, even
    /// though each edge is independently present in the dictionary.
    fn apply_exact_lexical_quality_gate(&mut self) {
        let Some(best_exact_cost) = self
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.system_entry_index().is_some()
                    && is_atomic_whole_reading_surface(candidate.text())
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
            candidate.system_entry_index().is_some()
                || evidence.user_edges != 0
                || evidence.system_edges < 2
                || evidence.fallback_edges != 0
                || evidence.generated_edges != 0
                || evidence.spelling_edges != 0
                || candidate.cost <= maximum_composite_cost
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
                system_entry_index: None,
                synthetic_exact: false,
                origin: CandidateOrigin::Direct,
                path_evidence: PathEvidence {
                    generated_edges: 1,
                    ..PathEvidence::default()
                },
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
                system_entry_index: None,
                synthetic_exact: false,
                origin: CandidateOrigin::Direct,
                path_evidence: PathEvidence {
                    generated_edges: 1,
                    ..PathEvidence::default()
                },
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
                    ) {
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
            MAX_DICTIONARY_EDGES_PER_READING.saturating_sub(exact_edges_for_length);
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
        let mut remaining_slots = MAX_DICTIONARY_EDGES_PER_READING;
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
            ) {
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
        dictionary
            .common_prefix_search(reading, |matched| {
                if matched.matched_bytes != reading_len {
                    return true;
                }
                if !allows_system_entry(
                    options.input_support,
                    options.skip_input_repair,
                    matched.entry.flags,
                ) {
                    return true;
                }
                if exact_edges_added >= MAX_DICTIONARY_EDGES_PER_READING {
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
            &self.generated,
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
                    &self.generated,
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
            word_count: 1,
            it_word_count: 0,
        })
        .map_err(|_| ConversionError::TooManySegments)?;
    Ok(ConversionCandidate {
        text,
        annotation: FixedStr::new(),
        segments,
        system_entry_index: None,
        synthetic_exact: false,
        origin: CandidateOrigin::Direct,
        path_evidence: PathEvidence {
            fallback_edges: 1,
            ..PathEvidence::default()
        },
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
    let cost = connection_cost(dictionary, initial_right_id, synthetic_id)
        .saturating_add(local_cost)
        .saturating_add(connection_cost(dictionary, synthetic_id, 0));
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
        system_entry_index: None,
        synthetic_exact: true,
        origin: CandidateOrigin::Direct,
        path_evidence: PathEvidence {
            fallback_edges: 1,
            ..PathEvidence::default()
        },
        cost,
    })
}

fn make_candidate(
    dictionary: &Dictionary<'_>,
    user_dictionary: Option<&UserDictionary>,
    reading: &str,
    nodes: &[Node],
    path: &[usize],
    generated: &[GeneratedSurface],
    cost: i64,
) -> Result<ConversionCandidate, ConversionError> {
    let mut text = FixedStr::new();
    let mut annotation = FixedStr::new();
    let mut segments = FixedVec::new();
    let mut path_evidence = PathEvidence::default();
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
                    reading_start: u16::try_from(node.start)
                        .map_err(|_| ConversionError::ReadingTooLong)?,
                    reading_end: u16::try_from(node.end)
                        .map_err(|_| ConversionError::ReadingTooLong)?,
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
        synthetic_exact: false,
        origin: CandidateOrigin::Direct,
        path_evidence,
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
        CandidateAuthority, CandidateOrigin, ConversionInput, ConversionInputClass,
        ConversionOptions, Converter, CorrectionMap, CorrectionMapError, CorrectionRun,
        LiteralPolicy, RawRepairBudget, RawRepairPlan, RepairTier,
    };
    use crate::dictionary::{image_format, Dictionary, EntryFlags};
    use crate::user_dictionary::UserDictionary;

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

    /// Small in-test image writer.  Keeping it here avoids adding the
    /// allocating `dictc` crate as a sakura-core dependency while still
    /// exercising the borrowed production dictionary parser and lattice.
    fn synthetic_dictionary(rows: &[FixtureEntry]) -> Vec<u8> {
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

        let tables = [
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
        let handle = std::thread::Builder::new()
            .name("conversion-raw-128k".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || {
                let dictionary = Dictionary::parse(&bytes).expect("synthetic dictionary");
                let plan = local_plan(12, "あき", "あか", RepairTier::LocalCompletion);
                let mut converter = Converter::new();
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
