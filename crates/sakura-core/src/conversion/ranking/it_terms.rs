use sakura_values::{FixedStr, MAX_PREEDIT_BYTES};

use crate::dictionary::{Dictionary, EntryFlags};

use super::super::{ConversionError, ConversionOptions, Converter};

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

impl Converter {
    /// A longer technical dictionary term is bounded evidence for the spelling
    /// of its already-converted prefix. This resolves compound homophones
    /// without manufacturing prefix entries or globally weakening Mozc costs.
    pub(in crate::conversion) fn apply_it_completion_coherence(
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
    pub(in crate::conversion) fn apply_it_compound_coherence(
        &mut self,
        reading: &str,
        options: ConversionOptions,
    ) {
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
}
