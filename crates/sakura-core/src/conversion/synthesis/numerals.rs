use sakura_values::{FixedStr, FixedVec, MAX_PREEDIT_BYTES};

use crate::dictionary::{Dictionary, EntryFlags};
use crate::numerals::{
    is_decorative_numeral_char, parse_numeric_prefix, should_emit_numeric_span, NumericSpan,
    NumericStyle, NUMERIC_STYLES,
};

use super::super::{
    dictionary_exact_surface_entry, numeric_form_cost, sort_by_cost, CandidateEvidence,
    CandidateOrigin, CommitBridgeTailStorage, ConversionCandidate, ConversionError, ConversionSegment, Converter,
    PathEvidence, NO_SYSTEM_ENTRY_INDEX,
};

impl Converter {
    /// New temporal forms follow the dictionary's number spelling when it
    /// exists (十 + 週間, 十三 + 時間). Rewriting the unit must not itself
    /// impose a new Arabic-versus-kanji preference. Existing calendar forms
    /// and numbers without lexical evidence keep their established order.
    pub(in crate::conversion) fn numeric_styles(
        dictionary: &Dictionary<'_>,
        reading: &str,
        span: NumericSpan,
    ) -> Result<[NumericStyle; 3], ConversionError> {
        let mut styles = NUMERIC_STYLES;
        let Some(counter) = span.counter.filter(|counter| !counter.is_calendar()) else {
            return Ok(styles);
        };
        let number_bytes = span.bytes - counter.reading().len();
        let number_span = NumericSpan {
            counter: None,
            bytes: number_bytes,
            ..span
        };
        let mut preferred = 0;
        let mut cost = i32::MAX;
        for (index, style) in styles.iter().enumerate() {
            let mut text = FixedStr::<MAX_PREEDIT_BYTES>::new();
            if style.write(number_span, &mut text).is_err() {
                continue;
            }
            if let Some(entry) = dictionary_exact_surface_entry(
                dictionary,
                &reading[..number_bytes],
                text.as_str(),
                None,
            )? {
                if entry.word_cost < cost {
                    cost = entry.word_cost;
                    preferred = index;
                }
            }
        }
        styles[..=preferred].rotate_right(1);
        Ok(styles)
    }

    pub(in crate::conversion) fn prefer_numeric_forms(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
    ) -> Result<(), ConversionError> {
        if crate::calendar::date_offset_for_reading(reading).is_some() {
            // Keep generated relative-date alternatives as well as the word
            // itself; せんしゅう also parses as a numeric week reading.
            return Ok(());
        }
        let Some(span) = parse_numeric_prefix(reading) else {
            return Ok(());
        };
        if span.bytes != reading.len() || !should_emit_numeric_span(span) {
            return Ok(());
        }
        let form_cost = numeric_form_cost(reading, span);
        let mut forms = Vec::new();
        for style in Self::numeric_styles(dictionary, reading, span)? {
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
            // A word the user registered for exactly this reading is the
            // user's explicit answer (#439). It is lexical evidence the
            // rewriter neither generates nor replaces, so it stays with its
            // own cost, whatever its spelling.
            if candidate.evidence_class() == CandidateEvidence::ExactUser {
                return true;
            }
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
        sort_by_cost(&mut self.candidates);
        Ok(())
    }
}
