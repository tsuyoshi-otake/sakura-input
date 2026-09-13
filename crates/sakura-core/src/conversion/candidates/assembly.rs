use sakura_values::{FixedStr, FixedVec, MAX_PREEDIT_BYTES, MAX_SEGMENTS};

use crate::dictionary::{Dictionary, EntryFlags};
use crate::input_repair::RepairKind;

use super::super::{
    connection_cost, BridgeBoundaryKind, CandidateAuthority, CandidateEvidence, CandidateOrigin,
    CommitBridgeTail, CommitBridgeTailStorage, LeftContextId, PathEvidence, RightContextId,
    MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES, NO_COMMIT_BRIDGE_ENTRY, NO_SYSTEM_ENTRY_INDEX,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionCandidate {
    pub(in crate::conversion) text: FixedStr<MAX_PREEDIT_BYTES>,
    pub(in crate::conversion) annotation: FixedStr<MAX_PREEDIT_BYTES>,
    pub(in crate::conversion) segments: FixedVec<ConversionSegment, MAX_SEGMENTS>,
    /// Exact system-dictionary provenance, present only for a one-edge system
    /// candidate. Composite and generated candidates deliberately have none.
    pub(in crate::conversion) system_entry_index: u32,
    /// Set only for a literal candidate injected by an exact literal policy.
    /// It is distinct from the ordinary lossless fallback so callers can
    /// prevent an emergency/raw-preservation result from entering learning.
    pub(in crate::conversion) synthetic_exact: bool,
    pub(in crate::conversion) origin: CandidateOrigin,
    pub(in crate::conversion) path_evidence: PathEvidence,
    /// True when the materialized path contains a generated calendar day
    /// edge after its reading start. This metadata keeps the admission rule
    /// independent of surface text and survives candidate cloning.
    pub(in crate::conversion) generated_day_suffix: bool,
    /// Folded from raw path edges while the optional combined pass knows its
    /// exact reading boundary. Display bunsetsu fusion cannot forge it.
    pub(in crate::conversion) bridge_boundary_kind: Option<BridgeBoundaryKind>,
    /// Exact identity of the final raw system edge, kept separately from the
    /// fused display segments and materialized only if this candidate commits.
    pub(in crate::conversion) commit_bridge_tail: CommitBridgeTailStorage,
    /// A contextual result is never reused as cost evidence for a later
    /// bridge. This one-bit terminal marker avoids retaining another i64 cost
    /// in every candidate.
    pub(in crate::conversion) cross_commit_rescored: bool,
    pub cost: i64,
}

/// One Viterbi path edge materialized as byte ranges in a candidate.
///
/// Reading and surface ranges let the engine focus and resize segments without
/// copying strings or losing every other boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConversionSegment {
    pub reading_start: u16,
    pub reading_end: u16,
    pub text_start: u16,
    pub text_end: u16,
    pub left_id: u16,
    pub right_id: u16,
    pub flags: EntryFlags,
    /// How many dictionary path words this segment covers. Bunsetsu fusion
    /// OR-merges `flags`, which would otherwise let one flagged word count
    /// as the whole fused segment in per-word statistics.
    pub word_count: u8,
    /// How many of those words carried [`EntryFlags::IT`] on their own.
    pub it_word_count: u8,
}

impl ConversionCandidate {
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    pub fn annotation(&self) -> &str {
        self.annotation.as_str()
    }

    pub fn segments(&self) -> &[ConversionSegment] {
        self.segments.as_slice()
    }

    pub const fn system_entry_index(&self) -> Option<u32> {
        if self.system_entry_index == NO_SYSTEM_ENTRY_INDEX {
            None
        } else {
            Some(self.system_entry_index)
        }
    }

    /// Whether this candidate was injected from
    /// [`crate::conversion::ConversionInput::exact_surface`] by an exact
    /// literal policy.
    pub const fn is_synthetic_exact(&self) -> bool {
        self.synthetic_exact
    }

    pub const fn origin(&self) -> CandidateOrigin {
        self.origin
    }

    pub const fn authority(&self) -> CandidateAuthority {
        self.origin.authority()
    }

    pub const fn path_evidence(&self) -> PathEvidence {
        self.path_evidence
    }

    /// Computes the candidate's authority class from its immutable path
    /// evidence.  No rendered surface comparison is involved, so homophones
    /// such as `労力` and `ロウ力` retain different provenance even when a
    /// normalizer would make them look alike.
    pub fn evidence_class(&self) -> CandidateEvidence {
        if let CandidateOrigin::RawRepair { tier, .. } = self.origin {
            return CandidateEvidence::RawRepair { tier };
        }

        let evidence = self.path_evidence;
        if evidence.has_repair_evidence() {
            // A spelling-correction flag is the same unconfirmed boundary as
            // the explicit English-spelling repair kind.  If both it and
            // another repair are present, retain the ambiguity instead of
            // selecting one label by accident.
            if evidence.spelling_edges != 0 {
                return if evidence.repair_kinds == 0 {
                    CandidateEvidence::Repair(RepairKind::EnglishSpelling)
                } else {
                    CandidateEvidence::MixedRepair
                };
            }
            return evidence
                .sole_repair_kind()
                .map_or(CandidateEvidence::MixedRepair, CandidateEvidence::Repair);
        }
        if evidence.generated_edges != 0 {
            return CandidateEvidence::Generated;
        }
        if evidence.fallback_edges != 0 {
            return CandidateEvidence::Fallback;
        }

        // `system_entry_index` is only materialized for a one-edge system
        // path.  Requiring the same one-segment/full-prefix shape here keeps
        // the exact class honest even if a future materializer adds another
        // source of entry ordinals.
        let one_whole_segment = self.segments.len() == 1
            && self.segments()[0].reading_start == 0
            && self.segments()[0].reading_end > 0;
        if one_whole_segment && evidence.system_edges == 1 && self.system_entry_index().is_some() {
            return CandidateEvidence::ExactSystem;
        }
        if one_whole_segment && evidence.user_edges == 1 && evidence.system_edges == 0 {
            return CandidateEvidence::ExactUser;
        }
        if evidence.system_edges != 0 || evidence.user_edges != 0 {
            return CandidateEvidence::CompositeLexical;
        }

        // Defensive default for a future edge kind that forgot to update the
        // evidence counters.  It must remain outside neural lexical groups.
        CandidateEvidence::Fallback
    }

    /// Convenience predicate for callers enforcing a whole-reading authority
    /// boundary without matching enum variants themselves.
    pub fn is_trustworthy_whole_reading_exact(&self) -> bool {
        self.evidence_class().is_trustworthy_whole_reading_exact()
    }

    pub const fn was_cross_commit_rescored(&self) -> bool {
        self.cross_commit_rescored
    }

    /// Materializes bounded bridge evidence from the exact final raw edge.
    /// Surface text is verified against the selected candidate; no later
    /// surface lookup is used to guess dictionary provenance.
    pub fn commit_bridge_tail(&self, dictionary: &Dictionary<'_>) -> Option<CommitBridgeTail> {
        if self.origin != CandidateOrigin::Direct
            || self.synthetic_exact
            || !self.path_evidence.is_system_only()
            || self.commit_bridge_tail.entry_index == NO_COMMIT_BRIDGE_ENTRY
        {
            return None;
        }
        let entry = dictionary
            .entry_at(self.commit_bridge_tail.entry_index as usize)
            .ok()?;
        if self.segments.last()?.right_id != entry.right_id {
            return None;
        }
        let mut final_surface = FixedStr::<MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES>::new();
        dictionary.write_surface(entry, &mut final_surface).ok()?;
        let prefix = self.text().strip_suffix(final_surface.as_str())?;
        let text_start = u16::try_from(prefix.len()).ok()?;
        let connection = connection_cost(
            dictionary,
            RightContextId::new(self.commit_bridge_tail.prefix_right_id),
            LeftContextId::new(entry.left_id),
        );
        let prefix_cost = connection + i64::from(entry.word_cost);
        Some(CommitBridgeTail {
            reading_start: self.commit_bridge_tail.reading_start,
            text_start,
            prefix_right_id: RightContextId::new(self.commit_bridge_tail.prefix_right_id),
            prefix_cost,
        })
    }

    pub(in crate::conversion) fn bridge_boundary_kind(&self) -> Option<BridgeBoundaryKind> {
        if self.path_evidence.is_system_only() {
            self.bridge_boundary_kind
        } else {
            None
        }
    }

    /// A corrected pass is admitted only when its segments cover the complete
    /// corrected reading (in UTF-8 bytes) and its path is system-dictionary-only.
    pub fn has_full_system_coverage(&self, corrected_reading_len: usize) -> bool {
        if !self.path_evidence.is_system_only() || self.segments.is_empty() {
            return false;
        }
        let mut next = 0u16;
        for segment in self.segments() {
            if segment.reading_start != next || segment.reading_end < segment.reading_start {
                return false;
            }
            next = segment.reading_end;
        }
        usize::from(next) == corrected_reading_len
    }
}
