//! Domain decision for Space while a Shift-started ASCII composition is live.
//!
//! The dictionary lookup is an input fact, not part of this decision. Keeping
//! the rule pure makes the key semantics testable independently from the
//! conversion implementation and its glossary fixtures.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShiftAsciiConvertFacts {
    pub(crate) shifted_ascii: bool,
    pub(crate) trigger_is_space: bool,
    pub(crate) shift_modifier: bool,
    pub(crate) dictionary_hit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShiftAsciiConvertDecision {
    Convert,
    InsertLiteralSpace,
    RejectUnknown,
}

pub(crate) fn decide_shift_ascii_convert(
    facts: ShiftAsciiConvertFacts,
) -> ShiftAsciiConvertDecision {
    if !facts.shifted_ascii {
        return ShiftAsciiConvertDecision::Convert;
    }
    if facts.trigger_is_space && (facts.shift_modifier || !facts.dictionary_hit) {
        return ShiftAsciiConvertDecision::InsertLiteralSpace;
    }
    if facts.dictionary_hit {
        ShiftAsciiConvertDecision::Convert
    } else {
        ShiftAsciiConvertDecision::RejectUnknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ConversionTrigger {
    pub(crate) is_space: bool,
    pub(crate) shifted: bool,
}
