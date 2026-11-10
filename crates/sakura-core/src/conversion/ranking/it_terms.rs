use sakura_values::{FixedStr, MAX_PREEDIT_BYTES};

use crate::dictionary::{Dictionary, EntryFlags};

use super::super::{ConversionCandidate, ConversionError, ConversionOptions, Converter};

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
/// 名詞-接尾 classes in the pinned Mozc `id.def` (1936..=2039): サ変接続,
/// 一般, 人名, 副詞可能, 助動詞語幹, 助数詞, 地域, 形容動詞語幹 and 特殊,
/// ending just before 名詞-接尾可能 at 2040. The IT glossary carries a few
/// edges in these classes (そう→層, そう→レイヤー, ち→値 and counter
/// units). A suffix is not a standalone technical word: it is compound
/// evidence only when it sits next to another IT word.
const IT_SUFFIX_CLASS_RANGE: std::ops::RangeInclusive<u16> = 1936..=2039;

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
                            || is_whole_query_yield(candidate)
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
    /// technical words receives two. An IT-flagged noun suffix counts only
    /// when it touches another IT word, so an ordinary stem chased by a
    /// suffix such as 層 (貧弱層, 脆弱層, なくなり層 -- Issue #454) does not
    /// pose as an IT compound. Short ordinary phrases are left to the
    /// dictionary and connection matrix. This is a candidate-shape rule,
    /// not a list of registered compounds.
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
            if is_whole_query_yield(candidate) {
                continue;
            }
            let evidence = candidate.path_evidence();
            if evidence.fallback_edges != 0
                || evidence.generated_edges != 0
                || evidence.spelling_edges != 0
            {
                continue;
            }
            let segments = candidate.segments();
            let it_words = segments
                .iter()
                .enumerate()
                .fold(0u16, |count, (index, segment)| {
                    if segment.it_word_count == 0 {
                        return count;
                    }
                    // A lone IT word led by a suffix class is evidence only
                    // inside a cluster of IT words; attached to a plain stem
                    // it is the stem's own suffix slot, not a technical
                    // compound. The fused 層に/層な/層だ entries lead with the
                    // same class, so they follow the same rule. A fused entry
                    // carrying more than one IT word counts fully, since the
                    // lexicon already priced the phrase (若年層).
                    let bare_suffix = segment.it_word_count == 1
                        && IT_SUFFIX_CLASS_RANGE.contains(&segment.left_id);
                    let touches_it = (index > 0 && segments[index - 1].it_word_count != 0)
                        || segments
                            .get(index + 1)
                            .is_some_and(|next| next.it_word_count != 0);
                    if bare_suffix && !touches_it {
                        return count;
                    }
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

/// A candidate that is one [`EntryFlags::READING_YIELD`] word covering the
/// whole query. The dictionary priced that word behind the word that owns
/// this reading, so IT evidence from the coherence passes must not lift it
/// back over that word (Issue #291). Inside a longer query the yielded word
/// is still ordinary IT evidence.
fn is_whole_query_yield(candidate: &ConversionCandidate) -> bool {
    matches!(
        candidate.segments(),
        [segment] if segment.word_count == 1
            && segment.flags.contains(EntryFlags::READING_YIELD)
    )
}
