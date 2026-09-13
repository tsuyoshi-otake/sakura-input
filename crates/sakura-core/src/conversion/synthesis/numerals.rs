use sakura_values::{FixedStr, FixedVec, MAX_PREEDIT_BYTES};

use crate::dictionary::EntryFlags;
use crate::numerals::{
    is_decorative_numeral_char, parse_numeric_prefix, should_emit_numeric_span, NUMERIC_STYLES,
};

use super::super::{
    numeric_form_cost, CandidateOrigin, CommitBridgeTailStorage, ConversionCandidate,
    ConversionError, ConversionSegment, Converter, PathEvidence, NO_SYSTEM_ENTRY_INDEX,
};

impl Converter {
    pub(in crate::conversion) fn prefer_numeric_forms(
        &mut self,
        reading: &str,
    ) -> Result<(), ConversionError> {
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
                generated_day_suffix: false,
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
}
