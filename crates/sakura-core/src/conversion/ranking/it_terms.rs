use sakura_values::{FixedStr, MAX_PREEDIT_BYTES};

use crate::dictionary::{Dictionary, EntryFlags};

use super::super::{
    ConversionCandidate, ConversionError, ConversionOptions, ConversionSegment, Converter,
};

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
/// evidence only when a run of suffix-led segments connects it to an IT
/// word or to a plain noun whose span the lexicon has not already claimed.
/// `dictc-core`'s `mozc` tests pin this range against the pinned `id.def`
/// checkout the dictionary build uses.
pub const IT_SUFFIX_CLASS_RANGE: std::ops::RangeInclusive<u16> = 1936..=2039;

/// Plain noun classes in the pinned Mozc `id.def`: 名詞-サ変接続, 一般,
/// 代名詞, 副詞可能, 固有名詞 and 引用文字列 (1841..=1930 minus the
/// inflecting stems ナイ形容詞語幹 1850 and 動詞非自立的 1919), plus
/// 接尾可能, 接続詞的 and 数 (2040..=2055). These heads take noun suffixes
/// as compounding; the excluded noun classes (形容動詞語幹 1931..=1935,
/// the 接尾 band itself, 特殊 and 非自立) and every non-noun class either
/// inflect with そう or bind grammatically. `dictc-core`'s `mozc` tests
/// pin this set against the same `id.def`.
pub const PLAIN_NOUN_STEM_RANGES: &[std::ops::RangeInclusive<u16>] =
    &[1841..=1849, 1851..=1918, 1920..=1930, 2040..=2055];

fn is_plain_noun_class(left_id: u16) -> bool {
    PLAIN_NOUN_STEM_RANGES
        .iter()
        .any(|range| range.contains(&left_id))
}

/// Noun-band stem classes whose words inflect with the aux-そう reading:
/// ナイ形容詞語幹 (1850), 動詞非自立的 (1919) and 形容動詞語幹
/// (1931..=1935) in the pinned Mozc `id.def`. A head reading that the
/// lexicon also stores under one of these classes is ambiguous: the
/// trailing suffix may be a spelling impostor in the そう slot (脆弱層
/// where 脆弱+そう means "seems fragile"), so the suffix loses its
/// compound evidence. Verb and adjective homophones do not block a noun
/// compound — a candidate headed by a real verb stem is already stopped
/// by its own class, and a homophonous sense does not delegitimize the
/// noun (独身 stays a 層 head next to the 篤信 reading, Issue #454).
/// `dictc-core`'s `mozc` tests pin this set against the same `id.def`.
pub const SOU_STEM_CLAIM_RANGES: &[std::ops::RangeInclusive<u16>] =
    &[1850..=1850, 1919..=1919, 1931..=1935];

fn is_stem_claim_class(left_id: u16) -> bool {
    SOU_STEM_CLAIM_RANGES
        .iter()
        .any(|range| range.contains(&left_id))
}

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
    /// when `it_suffix_counts` accepts it, so an ordinary stem chased by a
    /// suffix such as 層 (貧弱層, 脆弱層, なくなり層) does not pose as an IT
    /// compound while real suffix compounds keep their support (開発者層,
    /// ネットワーク層 -- Issue #454). An IT word elsewhere in the sentence
    /// does not rescue a dangling suffix (サーバーが貧弱そう keeps its そう).
    /// Short ordinary phrases are left to the dictionary and connection
    /// matrix. This is a candidate-shape rule, not a list of compounds.
    pub(in crate::conversion) fn apply_it_compound_coherence(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        options: ConversionOptions,
    ) -> Result<(), ConversionError> {
        if reading.chars().count() < MIN_IT_COMPOUND_READING_CHARS || options.it_bias_per_mille == 0
        {
            return Ok(());
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
            let mut it_words = 0u16;
            for (index, segment) in segments.iter().enumerate() {
                if segment.it_word_count == 0 {
                    continue;
                }
                // A lone IT word led by a suffix class is evidence only when
                // `it_suffix_counts` accepts it; a fused entry carrying more
                // than one IT word counts fully since the lexicon already
                // priced the phrase (若年層).
                if segment.it_word_count == 1
                    && IT_SUFFIX_CLASS_RANGE.contains(&segment.left_id)
                    && !it_suffix_counts(dictionary, reading, segments, index)?
                {
                    continue;
                }
                it_words = it_words.saturating_add(u16::from(segment.it_word_count));
            }
            if it_words == 0 {
                continue;
            }
            let boost = i64::from(it_words)
                .saturating_mul(IT_COMPOUND_WORD_BONUS)
                .min(MAX_IT_COMPOUND_BOOST);
            candidate.cost = candidate.cost.saturating_sub(boost);
        }
        Ok(())
    }
}

/// Whether one suffix-class IT word counts as compound evidence. A
/// contiguous run of suffix-led segments may pass through non-IT suffixes
/// (開発 + 者 + 層, fused 層に/層な/層だ) and then either reach an IT word
/// (ネットワーク + 層) or end at a head the lexicon leaves to plain
/// nouns. A head led by a non-noun class (貧弱 at 名詞-形容動詞語幹 1931,
/// なくなり at a verb class) cannot head a noun compound, and even a
/// plain-noun head is refused when the dictionary also stores its reading
/// under a そう-inflecting stem class: ぜいじゃく covers both 一般名詞
/// (1851) and 形容動詞語幹 (1931) edges, so 脆弱+層 is a spelling impostor
/// for 脆弱+そう while 開発者+層, 独身+者+層 and the other reviewed
/// compounds are real (Issue #454). A multi-edge head has no single
/// covering edge, so its trailing class decides the same way. An IT word
/// elsewhere in the sentence never reaches across a non-suffix boundary
/// (サーバーが貧弱そう).
fn it_suffix_counts(
    dictionary: &Dictionary<'_>,
    reading: &str,
    segments: &[ConversionSegment],
    index: usize,
) -> Result<bool, ConversionError> {
    let suffix_led = |segment: &ConversionSegment| IT_SUFFIX_CLASS_RANGE.contains(&segment.left_id);
    for head in [
        segments[..index]
            .iter()
            .rev()
            .find(|segment| !suffix_led(segment)),
        segments[index + 1..]
            .iter()
            .find(|segment| !suffix_led(segment)),
    ]
    .into_iter()
    .flatten()
    {
        if head.it_word_count != 0 {
            return Ok(true);
        }
        if !is_plain_noun_class(head.left_id) {
            continue;
        }
        let span = &reading[usize::from(head.reading_start)..usize::from(head.reading_end)];
        let mut covered = false;
        let mut claimed = false;
        dictionary
            .common_prefix_search(span, |found| {
                if found.matched_bytes != span.len() {
                    return true;
                }
                covered = true;
                if is_stem_claim_class(found.entry.left_id) {
                    claimed = true;
                    return false;
                }
                true
            })
            .map_err(ConversionError::Dictionary)?;
        if claimed {
            continue;
        }
        // A single-edge head always meets a covering edge; a multi-edge
        // head ends at a word the dictionary prices inside the segment,
        // so its trailing class stands in for the same plain-noun test.
        if covered || is_plain_noun_class(head.right_id) {
            return Ok(true);
        }
    }
    Ok(false)
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
