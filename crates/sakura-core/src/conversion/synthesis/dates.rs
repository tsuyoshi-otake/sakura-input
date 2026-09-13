use sakura_values::{FixedStr, FixedVec};

use crate::calendar::{date_offset_for_reading, date_surface_specs, CivilDate};
use crate::dictionary::EntryFlags;
use crate::numerals::is_numeric_day_surface;

use super::super::{
    CandidateOrigin, CommitBridgeTailStorage, ConversionCandidate, ConversionError,
    ConversionSegment, Converter, PathEvidence, GENERATED_VARIANT_SLACK, MAX_CONVERSION_CANDIDATES,
    NO_SYSTEM_ENTRY_INDEX,
};

impl Converter {
    /// `じつ` is a word ending (先日, 本日, 全日), not the day counter `にち`.
    /// Numeric rewriter output and a 千+日 splice both look like "1000日" and
    /// bury the word the user is typing.
    pub(in crate::conversion) fn drop_jitsu_day_counts(&mut self, reading: &str) {
        if !reading.ends_with("じつ") {
            return;
        }
        self.candidates
            .retain(|candidate| !is_numeric_day_surface(candidate.text()));
    }

    /// A fully covered, system-only path is stronger evidence than a
    /// generated calendar suffix. This pass only moves the generated edge
    /// behind that evidence; it does not remove it, so legitimate numeric
    /// compounds remain available when no lexical interpretation exists.
    pub(in crate::conversion) fn demote_generated_day_suffixes(&mut self, reading_len: usize) {
        let Some(best_lexical_cost) = self
            .candidates
            .iter()
            .filter(|candidate| candidate.has_full_system_coverage(reading_len))
            .map(|candidate| candidate.cost)
            .min()
        else {
            return;
        };
        let floor = best_lexical_cost.saturating_add(1);
        for candidate in &mut self.candidates {
            if candidate.generated_day_suffix {
                candidate.cost = candidate.cost.max(floor);
            }
        }
    }

    pub(in crate::conversion) fn add_date_candidates(
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
                cost: base
                    .cost
                    .saturating_add(10 + i64::try_from(index).unwrap_or(0)),
            });
        }
        Ok(())
    }
}
