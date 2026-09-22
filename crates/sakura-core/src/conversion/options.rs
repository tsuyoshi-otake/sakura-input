use crate::preferences::ConversionMethod;
use crate::width::PunctuationStyle;

use super::{RawRepairBudget, MAX_CONVERSION_CANDIDATES};

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

/// How many ranked candidates N-best search may produce for `reading`.
/// Dictionary single-kanji tails use the caller's separate display allowance;
/// they do not spend search states or widen this limit.
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
