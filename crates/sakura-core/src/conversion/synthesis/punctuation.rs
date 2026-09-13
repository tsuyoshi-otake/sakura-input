use sakura_values::{FixedStr, FixedVec};

use crate::dictionary::EntryFlags;
use crate::width::PunctuationStyle;

use super::super::{
    CandidateOrigin, CommitBridgeTailStorage, ConversionCandidate, ConversionError,
    ConversionSegment, Converter, PathEvidence, NO_SYSTEM_ENTRY_INDEX,
};

impl Converter {
    /// Offers the whole punctuation family for a reading that is a single
    /// punctuation mark, configured glyph first.
    ///
    /// The setting picks the default, not the vocabulary. Before this, a
    /// reader who had chosen the full-width comma could not reach the touten
    /// for one quoted sentence without opening the settings window: the width
    /// choke point re-emits the configured glyph for whichever family member a
    /// candidate carries, so four distinct candidates would all have rendered
    /// as the same character. That collapse happens at display time, which is
    /// why the fix is a candidate bit rather than a normalizer change -- each
    /// appended row carries `synthetic_exact`, which `append_candidate_surface`
    /// and both commit-only surface paths honour ahead of `normalize_into`.
    ///
    /// Rule 4's owned set stays four code points wide. The two half-width kana
    /// marks are offerable without being claimed, and the ASCII pair keeps its
    /// emit-but-never-reclaim direction: nothing here rewrites a character the
    /// reader typed.
    ///
    /// Carrying `synthetic_exact` also suppresses learning and the exact cache
    /// for these rows, which is what this feature wants: one quoted sentence's
    /// touten must not train the ranker to override the reader's configured
    /// mark on every later comma.
    pub(in crate::conversion) fn append_punctuation_family(
        &mut self,
        reading: &str,
        style: PunctuationStyle,
        wanted: usize,
    ) -> Result<(), ConversionError> {
        let Some(family) = style.family_reading(reading) else {
            return Ok(());
        };
        let reading_end =
            u16::try_from(reading.len()).map_err(|_| ConversionError::ReadingTooLong)?;
        // Every family member the search already produced has to go: it would
        // otherwise sit in the list without `synthetic_exact` and render as the
        // configured glyph, putting a second copy of one row on the page. The
        // appended set is a superset of what is dropped, so nothing the reader
        // could reach before becomes unreachable.
        self.candidates.retain(|candidate| {
            let mut text = candidate.text().chars();
            !matches!(
                (text.next(), text.next()),
                (Some(existing), None) if family.iter().any(|variant| variant.glyph == existing)
            )
        });
        // Below every surviving candidate, so the configured glyph is TOP-1 and
        // a later re-sort cannot interleave the family with anything else. This
        // is the one appender allowed to take TOP-1, and only for a reading that
        // is itself a single punctuation mark: the character it puts there is
        // the one the page already showed.
        let base = self
            .candidates
            .iter()
            .map(|candidate| candidate.cost)
            .min()
            .unwrap_or(0)
            .saturating_sub(i64::try_from(family.len()).unwrap_or(0));
        for (index, variant) in family.into_iter().enumerate() {
            let mut text = FixedStr::new();
            text.push(variant.glyph)
                .map_err(|_| ConversionError::OutputTooLong)?;
            let mut annotation = FixedStr::new();
            annotation
                .push_str(variant.annotation)
                .map_err(|_| ConversionError::OutputTooLong)?;
            let mut segments = FixedVec::new();
            segments
                .push(ConversionSegment {
                    reading_start: 0,
                    reading_end,
                    text_start: 0,
                    text_end: u16::try_from(text.len())
                        .map_err(|_| ConversionError::OutputTooLong)?,
                    // Neutral connection class in both directions: a
                    // punctuation mark must not hand the next conversion a
                    // noun's right ID.
                    left_id: 0,
                    right_id: 0,
                    flags: EntryFlags::NONE,
                    word_count: 1,
                    it_word_count: 0,
                })
                .map_err(|_| ConversionError::TooManySegments)?;
            self.candidates.insert(
                index,
                ConversionCandidate {
                    text,
                    annotation,
                    segments,
                    system_entry_index: NO_SYSTEM_ENTRY_INDEX,
                    synthetic_exact: true,
                    origin: CandidateOrigin::Direct,
                    path_evidence: PathEvidence {
                        generated_edges: 1,
                        ..PathEvidence::default()
                    },
                    generated_day_suffix: false,
                    bridge_boundary_kind: None,
                    commit_bridge_tail: CommitBridgeTailStorage::default(),
                    cross_commit_rescored: false,
                    cost: base.saturating_add(i64::try_from(index).unwrap_or(0)),
                },
            );
        }
        self.candidates.truncate(wanted.max(family.len()));
        Ok(())
    }
}
