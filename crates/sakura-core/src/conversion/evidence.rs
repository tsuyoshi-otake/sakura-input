use crate::input_repair::RepairKind;

use super::Surface;

/// The authority of a candidate source.  The numeric rank is intentionally
/// explicit: a lower-cost repair must never displace a direct candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateAuthority {
    Direct,
    LocalRawCompletion,
    GeneralSingleInsertion,
}

impl CandidateAuthority {
    pub const fn rank(self) -> u8 {
        match self {
            Self::Direct => 3,
            Self::LocalRawCompletion => 2,
            Self::GeneralSingleInsertion => 1,
        }
    }
}

/// Text-free evidence carried by a materialized conversion candidate.
///
/// Candidate cost is a useful local ordering signal, but it is not authority:
/// a generated calendar/numeric surface, a repaired dictionary edge, or a
/// composite path must not become equivalent to an exact whole-reading edge
/// merely because a later ranker assigns it a lower score.  Keep this enum
/// derived from path metadata rather than from the candidate's rendered text.
///
/// `ExactSystem` and `ExactUser` are the only trustworthy whole-reading
/// classes.  `CompositeLexical` remains ordinary lexical evidence, but is a
/// weaker class because it is made from more than one edge.  `Repair` keeps
/// the original repair kind so commit-history and advanced repairs cannot be
/// silently treated as ordinary dictionary entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateEvidence {
    ExactSystem,
    ExactUser,
    CompositeLexical,
    Generated,
    Repair(RepairKind),
    /// A path containing more than one repair kind cannot be reduced to one
    /// provenance label without losing information.  It is still protected
    /// from neural reordering just like a single repair kind.
    MixedRepair,
    Fallback,
    RawRepair {
        tier: RepairTier,
    },
}

/// Descriptive alias for callers that prefer the longer contract name.
pub type CandidateEvidenceClass = CandidateEvidence;

impl CandidateEvidence {
    /// Whether this evidence is a trustworthy one-edge whole-reading answer.
    pub const fn is_trustworthy_whole_reading_exact(self) -> bool {
        matches!(self, Self::ExactSystem | Self::ExactUser)
    }

    /// Whether this class may participate in the optional ordinary lexical
    /// neural reranker.  Exact system/user candidates share one group, while
    /// composite lexical candidates form a weaker group.  Everything derived,
    /// repaired, generated, or fallback remains local-order-only.
    pub const fn neural_group(self) -> Option<u8> {
        match self {
            // User-dictionary authority is already an explicit product
            // contract. Keep it above system exact entries so the optional
            // model cannot silently undo a user's deliberate override.
            Self::ExactUser => Some(3),
            Self::ExactSystem => Some(2),
            Self::CompositeLexical => Some(1),
            Self::Generated
            | Self::Repair(_)
            | Self::MixedRepair
            | Self::Fallback
            | Self::RawRepair { .. } => None,
        }
    }

    /// A stable, text-free tag for candidate-set fingerprints.  This is an
    /// implementation detail of the engine/worker handshake; it deliberately
    /// does not expose any surface text or dictionary ordinal.
    pub const fn fingerprint_tag(self) -> u8 {
        match self {
            Self::ExactSystem => 1,
            Self::ExactUser => 2,
            Self::CompositeLexical => 3,
            Self::Generated => 4,
            Self::Repair(RepairKind::Rule) => 10,
            Self::Repair(RepairKind::Advanced) => 11,
            Self::Repair(RepairKind::EnglishSpelling) => 12,
            Self::Repair(RepairKind::CommitHistory) => 13,
            Self::MixedRepair => 14,
            Self::Fallback => 20,
            Self::RawRepair {
                tier: RepairTier::LocalCompletion,
            } => 30,
            Self::RawRepair {
                tier: RepairTier::GeneralSingleInsertion,
            } => 31,
        }
    }
}

/// The repair tier attached to a raw-repair plan and its accepted candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairTier {
    LocalCompletion,
    GeneralSingleInsertion,
}

impl RepairTier {
    pub const fn authority(self) -> CandidateAuthority {
        match self {
            Self::LocalCompletion => CandidateAuthority::LocalRawCompletion,
            Self::GeneralSingleInsertion => CandidateAuthority::GeneralSingleInsertion,
        }
    }
}

/// Provenance of a materialized candidate.  This is kept in the core object,
/// rather than inferred later from its surface or cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateOrigin {
    Direct,
    RawRepair { plan_id: u8, tier: RepairTier },
}

impl CandidateOrigin {
    pub const fn authority(self) -> CandidateAuthority {
        match self {
            Self::Direct => CandidateAuthority::Direct,
            Self::RawRepair { tier, .. } => tier.authority(),
        }
    }
}

/// Text-free path evidence used by the raw-repair admission gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PathEvidence {
    pub system_edges: u8,
    pub user_edges: u8,
    pub fallback_edges: u8,
    pub generated_edges: u8,
    pub spelling_edges: u8,
    pub(super) repair_kinds: u8,
}

impl PathEvidence {
    pub const fn is_system_only(self) -> bool {
        self.system_edges > 0
            && self.user_edges == 0
            && self.fallback_edges == 0
            && self.generated_edges == 0
            && self.spelling_edges == 0
            && self.repair_kinds == 0
    }

    fn add_system(&mut self, spelling: bool) {
        if spelling {
            self.spelling_edges = self.spelling_edges.saturating_add(1);
        } else {
            self.system_edges = self.system_edges.saturating_add(1);
        }
    }

    pub const fn has_unconfirmed_repair(self) -> bool {
        self.repair_kinds
            & (repair_kind_bit(RepairKind::Rule)
                | repair_kind_bit(RepairKind::Advanced)
                | repair_kind_bit(RepairKind::EnglishSpelling))
            != 0
    }

    pub const fn has_repair_kind(self, kind: RepairKind) -> bool {
        self.repair_kinds & repair_kind_bit(kind) != 0
    }

    /// Whether this path contains a repair edge or a spelling-correction
    /// dictionary edge.  Spelling-correction is kept as a separate counter in
    /// the coarse contract, but it has the same authority boundary as the
    /// explicit English-spelling repair kind.
    pub const fn has_repair_evidence(self) -> bool {
        self.spelling_edges != 0 || self.repair_kinds != 0
    }

    /// Returns the sole repair kind when the path has exactly one.  Multiple
    /// kinds are intentionally reported as `None` so callers can fail closed
    /// with [`CandidateEvidence::MixedRepair`].
    pub const fn sole_repair_kind(self) -> Option<RepairKind> {
        let known = repair_kind_bit(RepairKind::Rule)
            | repair_kind_bit(RepairKind::Advanced)
            | repair_kind_bit(RepairKind::EnglishSpelling)
            | repair_kind_bit(RepairKind::CommitHistory);
        if self.repair_kinds & !known != 0 || self.repair_kinds.count_ones() != 1 {
            return None;
        }
        if self.has_repair_kind(RepairKind::Rule) {
            Some(RepairKind::Rule)
        } else if self.has_repair_kind(RepairKind::Advanced) {
            Some(RepairKind::Advanced)
        } else if self.has_repair_kind(RepairKind::EnglishSpelling) {
            Some(RepairKind::EnglishSpelling)
        } else {
            Some(RepairKind::CommitHistory)
        }
    }

    pub(super) fn add_surface(&mut self, surface: Surface, spelling: bool) {
        match surface {
            Surface::Dictionary {
                repair: Some(kind), ..
            } => self.repair_kinds |= repair_kind_bit(kind),
            Surface::Dictionary { repair: None, .. } => self.add_system(spelling),
            Surface::User(_) => self.user_edges = self.user_edges.saturating_add(1),
            Surface::Reading | Surface::Katakana => {
                self.fallback_edges = self.fallback_edges.saturating_add(1)
            }
            Surface::Literal(_) | Surface::Generated(_) => {
                self.generated_edges = self.generated_edges.saturating_add(1)
            }
        }
    }
}

const fn repair_kind_bit(kind: RepairKind) -> u8 {
    match kind {
        RepairKind::Rule => 1 << 0,
        RepairKind::Advanced => 1 << 1,
        RepairKind::EnglishSpelling => 1 << 2,
        RepairKind::CommitHistory => 1 << 3,
    }
}
