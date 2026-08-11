//! Deterministic, offline-only context prediction baseline for Issue #34.
//!
//! This module evaluates signals the engine already owns. It does not retain
//! raw candidate text, call the production prediction path, or change visible
//! order. Replay/evaluator tooling can use it before a neural model is admitted.

use sakura_neural_proto::{CandidateAuthority, MAX_PREDICTION_CANDIDATES, MAX_RESIDUAL};

const RIGHT_CONTEXT_BONUS: i16 = 192;
const RECENT_EXACT_BONUS: i16 = 512;
const MAX_DOMAIN_BONUS: i16 = 200;

/// Hash-only recent exact-choice signal used by the offline local baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceFingerprint {
    pub hash: u64,
    pub byte_len: u16,
}

/// Existing engine-owned signals evaluated before introducing a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocalBaselineSignals {
    pub previous_right_id: u16,
    pub domain_it_per_mille: u16,
    pub recent_exact: Option<SurfaceFingerprint>,
}

/// Hash-only candidate features for deterministic offline replay.
///
/// This intentionally contains no raw surface. The candidate generator retains
/// ownership of text and supplies only its existing volatile fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalBaselineCandidate {
    pub candidate_id: u64,
    pub base_cost: i32,
    pub authority: CandidateAuthority,
    pub right_id: u16,
    pub is_it: bool,
    pub surface: SurfaceFingerprint,
}

/// One scored row in the deterministic offline ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocalBaselineScore {
    pub candidate_id: u64,
    pub residual: i16,
    pub adjusted_cost: i64,
    pub structural_tier: u8,
    pub original_index: u8,
}

impl LocalBaselineScore {
    fn key(self) -> (u8, i64, u8) {
        let sortable_cost = if self.structural_tier == 0 {
            // Preserve the candidate generator's exact order within the
            // protected tier. The offline baseline has no authority to
            // reinterpret learning versus user-dictionary priority.
            0
        } else {
            self.adjusted_cost
        };
        (self.structural_tier, sortable_cost, self.original_index)
    }
}

/// Fixed-capacity result of the offline local baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBaselineRanking {
    scores: [LocalBaselineScore; MAX_PREDICTION_CANDIDATES],
    len: usize,
}

impl LocalBaselineRanking {
    pub fn as_slice(&self) -> &[LocalBaselineScore] {
        &self.scores[..self.len]
    }
}

/// Fail-closed validation errors for replay inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalBaselineError {
    TooManyCandidates,
    DuplicateCandidate,
}

/// Scores at most 32 existing candidates without allocating or changing
/// production prediction order.
///
/// Protected exact-learning and user-dictionary candidates stay in structural
/// tier zero and receive no residual. Ordinary candidates receive bounded
/// bonus-only residuals from the existing grammatical, domain, and recent exact
/// signals. Original index is the deterministic final tie-breaker.
pub fn rank_local_baseline(
    signals: LocalBaselineSignals,
    candidates: &[LocalBaselineCandidate],
) -> Result<LocalBaselineRanking, LocalBaselineError> {
    if candidates.len() > MAX_PREDICTION_CANDIDATES {
        return Err(LocalBaselineError::TooManyCandidates);
    }
    for (index, candidate) in candidates.iter().enumerate() {
        if candidates[..index]
            .iter()
            .any(|earlier| earlier.candidate_id == candidate.candidate_id)
        {
            return Err(LocalBaselineError::DuplicateCandidate);
        }
    }

    let mut ranking = LocalBaselineRanking {
        scores: [LocalBaselineScore::default(); MAX_PREDICTION_CANDIDATES],
        len: 0,
    };
    for (index, candidate) in candidates.iter().enumerate() {
        let residual = local_residual(signals, *candidate);
        let score = LocalBaselineScore {
            candidate_id: candidate.candidate_id,
            residual,
            adjusted_cost: i64::from(candidate.base_cost).saturating_add(i64::from(residual)),
            structural_tier: u8::from(!candidate.authority.protected()),
            original_index: u8::try_from(index).unwrap_or(u8::MAX),
        };
        let mut at = ranking.len;
        ranking.scores[at] = score;
        ranking.len += 1;
        while at > 0 && ranking.scores[at].key() < ranking.scores[at - 1].key() {
            ranking.scores.swap(at, at - 1);
            at -= 1;
        }
    }
    Ok(ranking)
}

fn local_residual(signals: LocalBaselineSignals, candidate: LocalBaselineCandidate) -> i16 {
    if candidate.authority.protected() {
        return 0;
    }

    let mut bonus = 0i16;
    if signals.previous_right_id != 0 && candidate.right_id == signals.previous_right_id {
        bonus = bonus.saturating_add(RIGHT_CONTEXT_BONUS);
    }
    if signals.recent_exact == Some(candidate.surface) {
        bonus = bonus.saturating_add(RECENT_EXACT_BONUS);
    }
    if candidate.is_it {
        let domain = signals.domain_it_per_mille.min(1_000);
        let domain_bonus =
            i16::try_from(u32::from(domain) * u32::from(MAX_DOMAIN_BONUS as u16) / 1_000)
                .unwrap_or(MAX_DOMAIN_BONUS);
        bonus = bonus.saturating_add(domain_bonus);
    }
    -bonus.min(MAX_RESIDUAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        candidate_id: u64,
        base_cost: i32,
        authority: CandidateAuthority,
        right_id: u16,
        is_it: bool,
        surface: SurfaceFingerprint,
    ) -> LocalBaselineCandidate {
        LocalBaselineCandidate {
            candidate_id,
            base_cost,
            authority,
            right_id,
            is_it,
            surface,
        }
    }

    #[test]
    fn existing_signals_rank_ordinary_candidates_but_preserve_protected_tiers() {
        let recent = SurfaceFingerprint {
            hash: 10,
            byte_len: 6,
        };
        let candidates = [
            candidate(
                1,
                100,
                CandidateAuthority::UserDictionary,
                0,
                false,
                SurfaceFingerprint {
                    hash: 1,
                    byte_len: 3,
                },
            ),
            candidate(2, 900, CandidateAuthority::Ordinary, 42, true, recent),
            candidate(
                3,
                300,
                CandidateAuthority::Ordinary,
                0,
                false,
                SurfaceFingerprint {
                    hash: 3,
                    byte_len: 3,
                },
            ),
        ];
        let ranking = rank_local_baseline(
            LocalBaselineSignals {
                previous_right_id: 42,
                domain_it_per_mille: 1_000,
                recent_exact: Some(recent),
            },
            &candidates,
        )
        .expect("valid baseline input");

        let scores = ranking.as_slice();
        assert_eq!(scores[0].candidate_id, 1, "user tier remains first");
        assert_eq!(scores[0].residual, 0);
        assert_eq!(scores[1].candidate_id, 2, "all three local signals apply");
        assert_eq!(scores[1].residual, -904);
        assert_eq!(scores[2].candidate_id, 3);
    }

    #[test]
    fn protected_candidates_keep_original_order_and_receive_no_residual() {
        let shared = SurfaceFingerprint {
            hash: 7,
            byte_len: 3,
        };
        let candidates = [
            candidate(10, 20, CandidateAuthority::ExactLearning, 8, true, shared),
            candidate(11, 10, CandidateAuthority::UserDictionary, 8, true, shared),
        ];
        let ranking = rank_local_baseline(
            LocalBaselineSignals {
                previous_right_id: 8,
                domain_it_per_mille: 1_000,
                recent_exact: Some(shared),
            },
            &candidates,
        )
        .expect("valid baseline input");
        assert_eq!(ranking.as_slice()[0].candidate_id, 10);
        assert_eq!(ranking.as_slice()[1].candidate_id, 11);
        assert!(ranking.as_slice().iter().all(|score| score.residual == 0));
    }

    #[test]
    fn duplicate_and_oversized_pools_fail_before_partial_output() {
        let item = candidate(
            1,
            0,
            CandidateAuthority::Ordinary,
            0,
            false,
            SurfaceFingerprint {
                hash: 0,
                byte_len: 0,
            },
        );
        assert_eq!(
            rank_local_baseline(LocalBaselineSignals::default(), &[item, item]),
            Err(LocalBaselineError::DuplicateCandidate)
        );
        let oversized = [item; MAX_PREDICTION_CANDIDATES + 1];
        assert_eq!(
            rank_local_baseline(LocalBaselineSignals::default(), &oversized),
            Err(LocalBaselineError::TooManyCandidates)
        );
    }
}
