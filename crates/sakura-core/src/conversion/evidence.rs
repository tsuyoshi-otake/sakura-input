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

/// The shipped selection rule of the optional neural reranker (Issue #32).
///
/// The model may not cross a candidate evidence boundary.  Trustworthy
/// one-edge whole-reading exact candidates form the strongest group.
/// Composite lexical candidates may be reordered only when no exact candidate
/// is present.  Generated, repaired, fallback, and raw-repair candidates never
/// enter the choice set, so their local ordering remains authoritative.
/// Inside the chosen group the highest model log probability wins; local cost
/// is not added (`docs/decisions/issue-32-reranker.md`).
///
/// Returns `None` (keep local order) for fewer than two candidates, when no
/// candidate has a neural group, or when a score in the chosen group is not
/// finite.  The engine and the offline `neural-eval` evaluator both call this
/// function, so the evaluator measures exactly what ships (Issue #370).
pub fn neural_listwise_choice(
    candidate_count: usize,
    mut log_probability_at: impl FnMut(usize) -> f32,
    mut neural_group_at: impl FnMut(usize) -> Option<u8>,
) -> Option<usize> {
    if candidate_count < 2 {
        return None;
    }
    let target_group = (0..candidate_count)
        .filter_map(&mut neural_group_at)
        .max()?;
    let mut selected = None;
    let mut selected_score = f32::NEG_INFINITY;
    for index in 0..candidate_count {
        if neural_group_at(index) != Some(target_group) {
            continue;
        }
        let score = log_probability_at(index);
        if !score.is_finite() {
            return None;
        }
        if selected.is_none() || score > selected_score {
            selected = Some(index);
            selected_score = score;
        }
    }
    selected
}

/// The outcome of the margin-gated model selection (Sakura Input Issue #440,
/// distilled from the sakura-rerank Issue #29 research gate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeuralGateOutcome {
    /// The model-selected candidate clears the margin over the local choice.
    Override(usize),
    /// Scores are valid but the local choice stays (agreement, insufficient
    /// margin, or the local choice is outside the scored prefix).
    KeepLocal,
    /// `neural_listwise_choice` yields nothing or a needed score is
    /// non-finite.
    Unavailable,
}

/// Applies [`neural_listwise_choice`] and then the shipped margin gate.
///
/// The protected evidence-group rule still applies first: the gated candidate
/// is always the `neural_listwise_choice` winner, never a cross-group pick.
/// The local choice is replaced only when the winner's score minus the local
/// choice's score, computed in `f64`, is strictly greater than `margin` (the
/// shipped value is `sakura_rerank_proto::Selection::LOCAL_OVERRIDE_MARGIN`).
///
/// The result is [`NeuralGateOutcome::Unavailable`] when there is nothing
/// defensible to compare: no listwise choice, a non-finite score for the
/// local choice, or a non-finite `margin`. An identical or out-of-prefix
/// local index is [`NeuralGateOutcome::KeepLocal`]: an unscored local choice
/// is never overridden.
pub fn neural_gated_choice(
    candidate_count: usize,
    local_index: usize,
    margin: f64,
    mut log_probability_at: impl FnMut(usize) -> f32,
    neural_group_at: impl FnMut(usize) -> Option<u8>,
) -> NeuralGateOutcome {
    if !margin.is_finite() {
        return NeuralGateOutcome::Unavailable;
    }
    let Some(selected) =
        neural_listwise_choice(candidate_count, &mut log_probability_at, neural_group_at)
    else {
        return NeuralGateOutcome::Unavailable;
    };
    if selected == local_index || local_index >= candidate_count {
        return NeuralGateOutcome::KeepLocal;
    }
    let local_score = log_probability_at(local_index);
    if !local_score.is_finite() {
        return NeuralGateOutcome::Unavailable;
    }
    if f64::from(log_probability_at(selected)) - f64::from(local_score) > margin {
        NeuralGateOutcome::Override(selected)
    } else {
        NeuralGateOutcome::KeepLocal
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
            Surface::Reading | Surface::Katakana | Surface::Passthrough => {
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

#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn neural_gated_choice_overrides_only_past_the_f64_margin() {
        let group = |_| CandidateEvidence::ExactSystem.neural_group();
        // The shipped margin in f64: a difference strictly above it overrides.
        let scores = [0.0f32, 1.7, -1.0];
        assert_eq!(
            neural_gated_choice(3, 0, 1.7, |index| scores[index], group),
            NeuralGateOutcome::Override(1),
            "1.7f32 is 1.7000000476837158 in f64, strictly above 1.7"
        );
        // A difference exactly equal to the margin is kept (strict `>`).
        let scores = [0.0f32, 1.5, -1.0];
        assert_eq!(
            neural_gated_choice(3, 0, 1.5, |index| scores[index], group),
            NeuralGateOutcome::KeepLocal
        );
        // Just under the shipped margin is kept.
        assert_eq!(
            neural_gated_choice(3, 0, 1.7, |index| scores[index], group),
            NeuralGateOutcome::KeepLocal
        );
    }

    #[test]
    fn neural_gated_choice_keeps_agreement_and_unscored_local_choices() {
        let group = |_| CandidateEvidence::ExactSystem.neural_group();
        // The model agrees with the local choice: kept.
        let scores = [3.0f32, 2.9, -1.0];
        assert_eq!(
            neural_gated_choice(3, 0, 1.7, |index| scores[index], group),
            NeuralGateOutcome::KeepLocal
        );
        // A local index outside the scored prefix is never overridden, even
        // when the model's choice would clear the margin against any listed
        // score.
        assert_eq!(
            neural_gated_choice(3, 3, 1.7, |index| scores[index], group),
            NeuralGateOutcome::KeepLocal
        );
        assert_eq!(
            neural_gated_choice(3, usize::MAX, 1.7, |index| scores[index], group),
            NeuralGateOutcome::KeepLocal
        );
    }

    #[test]
    fn neural_gated_choice_is_unavailable_without_a_choice_or_finite_scores() {
        // No neural group at all: nothing to compare.
        let no_group = |_| CandidateEvidence::Fallback.neural_group();
        assert_eq!(
            neural_gated_choice(2, 0, 1.7, |_| 0.0, no_group),
            NeuralGateOutcome::Unavailable
        );
        // A non-finite score inside the chosen group leaves no selection.
        let group = |_| CandidateEvidence::ExactSystem.neural_group();
        let scores = [0.0f32, f32::NAN];
        assert_eq!(
            neural_gated_choice(2, 0, 1.7, |index| scores[index], group),
            NeuralGateOutcome::Unavailable
        );
        // A non-finite local score outside every neural group is still
        // unavailable: the margin cannot be computed.
        let local = [CandidateEvidence::Fallback, CandidateEvidence::ExactSystem];
        let scores = [f32::NAN, 4.0f32];
        assert_eq!(
            neural_gated_choice(
                2,
                0,
                1.7,
                |index| scores[index],
                |index| local[index].neural_group()
            ),
            NeuralGateOutcome::Unavailable
        );
        // A non-finite margin is a contract violation, not a keep.
        let scores = [0.0f32, 4.0];
        for margin in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                neural_gated_choice(2, 0, margin, |index| scores[index], group),
                NeuralGateOutcome::Unavailable
            );
        }
    }

    #[test]
    fn neural_gated_choice_keeps_group_protection_then_compares_with_local() {
        // The composite local choice outscores every exact candidate, yet the
        // strongest group still owns the selection; the gate then compares
        // the chosen exact candidate against the local choice's score.
        let groups = [
            CandidateEvidence::CompositeLexical,
            CandidateEvidence::ExactSystem,
            CandidateEvidence::ExactSystem,
        ];
        let group = |index: usize| groups[index].neural_group();
        let scores = [200.0f32, -1.0, 202.0];
        assert_eq!(
            neural_gated_choice(3, 0, 1.7, |index| scores[index], group),
            NeuralGateOutcome::Override(2),
            "group protection selects the exact 202.0, and 202-200 clears 1.7"
        );
        // Same protection, but the chosen exact score stays within the margin
        // of the local composite choice: kept.
        let scores = [200.0f32, -1.0, 200.5];
        assert_eq!(
            neural_gated_choice(3, 0, 1.7, |index| scores[index], group),
            NeuralGateOutcome::KeepLocal
        );
    }
}
