use crate::dictionary::Dictionary;
use crate::numerals::NumericSpan;

use super::super::{LeftContextId, RightContextId, BARE_KANA_NUMBER_FORM_COST, NUMBER_FORM_COST};

pub(in crate::conversion) fn connection_cost(
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

pub(in crate::conversion) fn numeric_form_cost(source: &str, span: NumericSpan) -> i64 {
    let has_explicit_digit = source
        .chars()
        .any(|character| character.is_ascii_digit() || ('０'..='９').contains(&character));
    if span.counter.is_some() || has_explicit_digit {
        NUMBER_FORM_COST
    } else {
        BARE_KANA_NUMBER_FORM_COST
    }
}

pub(in crate::conversion) fn synthetic_run_cost(
    base: i64,
    per_character: i64,
    characters: usize,
) -> i64 {
    base.saturating_add(per_character.saturating_mul(characters as i64))
}
