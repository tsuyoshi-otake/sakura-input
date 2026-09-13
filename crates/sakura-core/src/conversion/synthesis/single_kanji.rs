use sakura_values::{FixedStr, FixedVec, MAX_PREEDIT_BYTES};

use crate::dictionary::{Dictionary, EntryFlags, SingleKanjiVariant};

use super::super::{
    CandidateOrigin, CommitBridgeTailStorage, ConversionCandidate, ConversionError,
    ConversionSegment, Converter, PathEvidence, DEFAULT_NOUN_ID, NO_SYSTEM_ENTRY_INDEX,
    SINGLE_KANJI_ANNOTATION,
};

impl Converter {
    /// Appends the pinned single-kanji table to a finished candidate list.
    ///
    /// Single kanji are deliberately not lattice edges. こう alone names 315
    /// characters in the pinned source, so admitting them as edges would spend
    /// a one-mora reading's whole `MAX_LATTICE_NODES` budget on them and would
    /// change the cost of every path that crosses them. Mozc reaches the same
    /// conclusion and runs its single-kanji rewriter after conversion; this is
    /// the same position in the pipeline. The tail therefore cannot move TOP-1
    /// or reorder anything the search produced. It only fills slots the ranked
    /// list left empty, and every appended cost sits above the whole ranked
    /// list so a later re-sort keeps it at the end.
    pub(in crate::conversion) fn append_single_kanji(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        wanted: usize,
    ) -> Result<(), ConversionError> {
        if self.candidates.len() >= wanted || !dictionary.has_single_kanji() {
            return Ok(());
        }
        let reading_end =
            u16::try_from(reading.len()).map_err(|_| ConversionError::ReadingTooLong)?;
        let synthetic_id = if dictionary.class_count() > usize::from(DEFAULT_NOUN_ID) {
            DEFAULT_NOUN_ID
        } else {
            0
        };
        // Measured against the ranked ceiling rather than the previous tail
        // row: a cheap ranked list must not let its tail overtake anything.
        let ranked_ceiling = self
            .candidates
            .iter()
            .map(|candidate| candidate.cost)
            .max()
            .unwrap_or(0);
        for (index, character) in dictionary.single_kanji(reading).enumerate() {
            if self.candidates.len() >= wanted {
                break;
            }
            let mut text = FixedStr::new();
            if text.push(character).is_err() {
                continue;
            }
            // A character the search already ranked keeps its ranked position
            // and its own annotation.
            if self
                .candidates
                .iter()
                .any(|candidate| candidate.text() == text.as_str())
            {
                continue;
            }
            let Some(annotation) = single_kanji_annotation(dictionary, character) else {
                continue;
            };
            let mut segments = FixedVec::new();
            segments
                .push(ConversionSegment {
                    reading_start: 0,
                    reading_end,
                    text_start: 0,
                    text_end: u16::try_from(text.len())
                        .map_err(|_| ConversionError::OutputTooLong)?,
                    left_id: synthetic_id,
                    right_id: synthetic_id,
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
                // Not system-only, so the cross-commit bridge can neither
                // anchor on an appended character nor transfer a contextual
                // gain to one.
                path_evidence: PathEvidence {
                    generated_edges: 1,
                    ..PathEvidence::default()
                },
                generated_day_suffix: false,
                bridge_boundary_kind: None,
                commit_bridge_tail: CommitBridgeTailStorage::default(),
                cross_commit_rescored: false,
                cost: ranked_ceiling.saturating_add(1 + i64::try_from(index).unwrap_or(0)),
            });
        }
        Ok(())
    }
}

/// Renders one appended character's annotation: 異体字（高） for a character
/// the pinned rules relate to another, and a plain marker otherwise.
///
/// A note is built whole or not at all, so a bounded buffer that cannot hold
/// the full relation never leaves a half-written label on a candidate.
fn single_kanji_annotation(
    dictionary: &Dictionary<'_>,
    character: char,
) -> Option<FixedStr<MAX_PREEDIT_BYTES>> {
    let mut annotation = FixedStr::new();
    let Some(SingleKanjiVariant { original, kind }) = dictionary.single_kanji_variant(character)
    else {
        annotation.push_str(SINGLE_KANJI_ANNOTATION).ok()?;
        return Some(annotation);
    };
    annotation.push_str(kind.label()).ok()?;
    annotation.push('（').ok()?;
    annotation.push(original).ok()?;
    annotation.push('）').ok()?;
    Some(annotation)
}
