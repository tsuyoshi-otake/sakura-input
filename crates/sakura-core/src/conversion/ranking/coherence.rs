use super::super::{char_class, CharClass, ConversionCandidate, Converter};

/// A one-character hiragana lead segment carries no lexical evidence of its
/// own: nothing in it says the user meant a phrase to start there. A path that
/// opens with one and then needs a whole kanji word to finish the reading is a
/// splice, not a parse, and those splices were burying real homophones on the
/// first candidate page -- たいあん offered た慰安 above 対案, and きかん put
/// き澗 ahead of 気管 and 旗艦 (Issue #94). This is the same rule as the
/// exact-lexical composite cost window, held much tighter for the one path
/// shape that is almost never a real segmentation. It stays a window rather
/// than a ban so a cheap splice that happens to spell a real word survives:
/// とじょう keeps と場 at +1190 over 途上.
const KANA_FRAGMENT_SPLIT_COST_WINDOW: i64 = 1_500;
/// Prefixes that genuinely attach to a following noun, so a path opening with
/// one is a parse after all: ご意見, お名前, み仏.
const KANA_PREFIX_MORPHEMES: [char; 3] = ['お', 'ご', 'み'];

impl Converter {
    /// Drops the kana-fragment splices described on
    /// [`KANA_FRAGMENT_SPLIT_COST_WINDOW`]. The window is measured from the
    /// cheapest whole-reading path, so a reading that produced no whole-reading
    /// candidate at all keeps everything it found.
    pub(in crate::conversion) fn drop_kana_fragment_prefix_splits(&mut self) {
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
