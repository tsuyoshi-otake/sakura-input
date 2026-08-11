//! Offline replay metrics for dormant context prediction work.
//!
//! Evaluation consumes hash-only [`PredictionSnapshot`] values. A stale score
//! response is counted and evaluated as the unchanged generator order, matching
//! the future fail-closed runtime contract. No production prediction caller,
//! display list, settings surface, or worker is activated here.

use sakura_neural_proto::{CandidateAuthority, MAX_PREDICTION_CANDIDATES};

use crate::prediction_snapshot::{PredictionSnapshot, SnapshotCorrelation, SnapshotSource};

pub const DISPLAY_CANDIDATES: usize = 9;

/// One ordered replay observation. Candidate ids contain no candidate text.
#[derive(Debug, Clone, Copy)]
pub struct ReplayObservation<'a> {
    pub snapshot: &'a PredictionSnapshot,
    /// Proposed full internal order, never a post-display reorder.
    pub ranked_candidate_ids: &'a [u64],
    /// Exact committed candidate when replay ground truth has one.
    pub expected_candidate_id: Option<u64>,
    /// Correlation returned with the proposed ranking.
    pub response_correlation: SnapshotCorrelation,
    /// Characters/keystrokes required without and with prediction acceptance.
    pub keystrokes_without_prediction: u16,
    pub keystrokes_with_prediction: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fraction {
    pub numerator: u64,
    pub denominator: u64,
}

impl Fraction {
    pub fn value(self) -> f64 {
        if self.denominator == 0 {
            0.0
        } else {
            self.numerator as f64 / self.denominator as f64
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceHit {
    pub source: SnapshotSource,
    pub top9_hits: u64,
    pub oracle_opportunities: u64,
}

/// Aggregate Phase 4 metrics. Sums and denominators remain available so report
/// writers do not lose the sample size behind a floating-point rate.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationMetrics {
    pub observations: u64,
    pub labelled_observations: u64,
    pub oracle_recall_at_9: Fraction,
    pub oracle_recall_at_16: Fraction,
    pub oracle_recall_at_32: Fraction,
    pub top_1: Fraction,
    pub top_3: Fraction,
    pub top_9: Fraction,
    pub reciprocal_rank_sum: f64,
    pub ndcg_sum: f64,
    pub keystrokes_saved: u64,
    pub keystrokes_without_prediction: u64,
    pub persistence: Fraction,
    pub churn: Fraction,
    pub stale_responses: Fraction,
    pub duplicates_removed: Fraction,
    pub source_hits: [SourceHit; 3],
}

impl Default for EvaluationMetrics {
    fn default() -> Self {
        Self {
            observations: 0,
            labelled_observations: 0,
            oracle_recall_at_9: Fraction::default(),
            oracle_recall_at_16: Fraction::default(),
            oracle_recall_at_32: Fraction::default(),
            top_1: Fraction::default(),
            top_3: Fraction::default(),
            top_9: Fraction::default(),
            reciprocal_rank_sum: 0.0,
            ndcg_sum: 0.0,
            keystrokes_saved: 0,
            keystrokes_without_prediction: 0,
            persistence: Fraction::default(),
            churn: Fraction::default(),
            stale_responses: Fraction::default(),
            duplicates_removed: Fraction::default(),
            source_hits: [
                SourceHit {
                    source: SnapshotSource::History,
                    ..SourceHit::default()
                },
                SourceHit {
                    source: SnapshotSource::SystemDictionary,
                    ..SourceHit::default()
                },
                SourceHit {
                    source: SnapshotSource::UserDictionary,
                    ..SourceHit::default()
                },
            ],
        }
    }
}

impl EvaluationMetrics {
    pub fn mrr(&self) -> f64 {
        mean(self.reciprocal_rank_sum, self.labelled_observations)
    }

    pub fn ndcg(&self) -> f64 {
        mean(self.ndcg_sum, self.labelled_observations)
    }

    pub fn keystroke_saving_rate(&self) -> f64 {
        Fraction {
            numerator: self.keystrokes_saved,
            denominator: self.keystrokes_without_prediction,
        }
        .value()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationError {
    TooManyRankedCandidates,
    DuplicateRankedCandidate,
    UnexpectedRankedCandidate,
    MissingProtectedCandidate,
    ProtectedOrderChanged,
    OrdinaryBeforeProtected,
    InvalidKeystrokeCounts,
}

/// Evaluates an ordered replay without allocating candidate text or mutating
/// any prediction state.
pub fn evaluate_replay(
    observations: &[ReplayObservation<'_>],
) -> Result<EvaluationMetrics, EvaluationError> {
    let mut metrics = EvaluationMetrics::default();
    let mut previous: Option<PreviousOrder> = None;

    for observation in observations {
        validate_observation(observation)?;
        metrics.observations += 1;
        metrics.stale_responses.denominator += 1;
        let stale = !observation
            .snapshot
            .accepts(observation.response_correlation);
        if stale {
            metrics.stale_responses.numerator += 1;
        }

        metrics.duplicates_removed.numerator +=
            u64::try_from(observation.snapshot.duplicate_count()).unwrap_or(u64::MAX);
        metrics.duplicates_removed.denominator +=
            u64::try_from(observation.snapshot.offered_count()).unwrap_or(u64::MAX);

        let original_ids = OriginalIds::new(observation.snapshot);
        let effective = if stale {
            original_ids.as_slice()
        } else {
            observation.ranked_candidate_ids
        };

        if let Some(expected) = observation.expected_candidate_id {
            metrics.labelled_observations += 1;
            update_oracle(
                &mut metrics.oracle_recall_at_9,
                original_ids.as_slice(),
                expected,
                9,
            );
            update_oracle(
                &mut metrics.oracle_recall_at_16,
                original_ids.as_slice(),
                expected,
                16,
            );
            update_oracle(
                &mut metrics.oracle_recall_at_32,
                original_ids.as_slice(),
                expected,
                32,
            );

            metrics.top_1.denominator += 1;
            metrics.top_3.denominator += 1;
            metrics.top_9.denominator += 1;
            if let Some(rank) = effective.iter().position(|id| *id == expected) {
                let one_based = rank + 1;
                metrics.top_1.numerator += u64::from(one_based <= 1);
                metrics.top_3.numerator += u64::from(one_based <= 3);
                metrics.top_9.numerator += u64::from(one_based <= DISPLAY_CANDIDATES);
                metrics.reciprocal_rank_sum += 1.0 / one_based as f64;
                metrics.ndcg_sum += 1.0 / ((one_based + 1) as f64).log2();
            }

            if let Some(candidate) = observation.snapshot.candidate(expected) {
                let source = &mut metrics.source_hits[source_index(candidate.source)];
                source.oracle_opportunities += 1;
                if effective
                    .iter()
                    .take(DISPLAY_CANDIDATES)
                    .any(|id| *id == expected)
                {
                    source.top9_hits += 1;
                }
            }
        }

        let without = u64::from(observation.keystrokes_without_prediction);
        let with = u64::from(observation.keystrokes_with_prediction);
        metrics.keystrokes_without_prediction += without;
        metrics.keystrokes_saved += without - with;

        if let Some(previous) = &previous {
            if !stale
                && !previous.stale
                && previous.session_id == observation.snapshot.correlation().session_id
            {
                update_stability(&mut metrics, previous.as_slice(), effective);
            }
        }
        previous = Some(PreviousOrder::new(
            observation.snapshot.correlation().session_id,
            effective,
            stale,
        ));
    }
    Ok(metrics)
}

fn validate_observation(observation: &ReplayObservation<'_>) -> Result<(), EvaluationError> {
    if observation.ranked_candidate_ids.len() > MAX_PREDICTION_CANDIDATES {
        return Err(EvaluationError::TooManyRankedCandidates);
    }
    if observation.keystrokes_with_prediction > observation.keystrokes_without_prediction {
        return Err(EvaluationError::InvalidKeystrokeCounts);
    }
    for (index, candidate_id) in observation.ranked_candidate_ids.iter().enumerate() {
        if observation.ranked_candidate_ids[..index].contains(candidate_id) {
            return Err(EvaluationError::DuplicateRankedCandidate);
        }
        if observation.snapshot.candidate(*candidate_id).is_none() {
            return Err(EvaluationError::UnexpectedRankedCandidate);
        }
    }

    let protected: [u64; MAX_PREDICTION_CANDIDATES] = {
        let mut ids = [0; MAX_PREDICTION_CANDIDATES];
        let mut len = 0;
        for candidate in observation.snapshot.candidates() {
            if candidate.authority != CandidateAuthority::Ordinary {
                ids[len] = candidate.candidate_id;
                len += 1;
            }
        }
        if observation.ranked_candidate_ids.len() < len {
            return Err(EvaluationError::MissingProtectedCandidate);
        }
        for id in &ids[..len] {
            if !observation.ranked_candidate_ids.contains(id) {
                return Err(EvaluationError::MissingProtectedCandidate);
            }
        }
        let ranked_protected = observation
            .ranked_candidate_ids
            .iter()
            .filter(|id| ids[..len].contains(id));
        if !ranked_protected.eq(ids[..len].iter()) {
            return Err(EvaluationError::ProtectedOrderChanged);
        }
        if observation.ranked_candidate_ids[..len]
            .iter()
            .any(|id| !ids[..len].contains(id))
        {
            return Err(EvaluationError::OrdinaryBeforeProtected);
        }
        ids
    };
    let _ = protected;
    Ok(())
}

fn update_oracle(metric: &mut Fraction, order: &[u64], expected: u64, limit: usize) {
    metric.denominator += 1;
    metric.numerator += u64::from(order.iter().take(limit).any(|id| *id == expected));
}

fn update_stability(metrics: &mut EvaluationMetrics, previous: &[u64], current: &[u64]) {
    let previous = &previous[..previous.len().min(DISPLAY_CANDIDATES)];
    let current = &current[..current.len().min(DISPLAY_CANDIDATES)];
    metrics.persistence.denominator += u64::from(!previous.is_empty());
    if let Some(previous_top) = previous.first() {
        metrics.persistence.numerator += u64::from(current.contains(previous_top));
    }

    metrics.churn.denominator += u64::try_from(previous.len()).unwrap_or(u64::MAX);
    metrics.churn.numerator += u64::try_from(
        previous
            .iter()
            .filter(|candidate_id| !current.contains(candidate_id))
            .count(),
    )
    .unwrap_or(u64::MAX);
}

fn source_index(source: SnapshotSource) -> usize {
    match source {
        SnapshotSource::History => 0,
        SnapshotSource::SystemDictionary => 1,
        SnapshotSource::UserDictionary => 2,
    }
}

fn mean(sum: f64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

struct OriginalIds {
    ids: [u64; MAX_PREDICTION_CANDIDATES],
    len: usize,
}

struct PreviousOrder {
    session_id: u64,
    ids: [u64; MAX_PREDICTION_CANDIDATES],
    len: usize,
    stale: bool,
}

impl PreviousOrder {
    fn new(session_id: u64, order: &[u64], stale: bool) -> Self {
        let mut ids = [0; MAX_PREDICTION_CANDIDATES];
        ids[..order.len()].copy_from_slice(order);
        Self {
            session_id,
            ids,
            len: order.len(),
            stale,
        }
    }

    fn as_slice(&self) -> &[u64] {
        &self.ids[..self.len]
    }
}

impl OriginalIds {
    fn new(snapshot: &PredictionSnapshot) -> Self {
        let mut ids = [0; MAX_PREDICTION_CANDIDATES];
        let len = snapshot.candidates().len();
        for (destination, candidate) in ids.iter_mut().zip(snapshot.candidates()) {
            *destination = candidate.candidate_id;
        }
        Self { ids, len }
    }

    fn as_slice(&self) -> &[u64] {
        &self.ids[..self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prediction_snapshot::{DictionaryIdentity, SnapshotCandidateInput};
    use sakura_proto::InputScope;

    fn input<'a>(
        reading: &'a str,
        surface: &'a str,
        authority: CandidateAuthority,
        source: SnapshotSource,
    ) -> SnapshotCandidateInput<'a> {
        SnapshotCandidateInput {
            reading,
            surface,
            dictionary_identity: match source {
                SnapshotSource::SystemDictionary => Some(DictionaryIdentity::SystemEntry(
                    surface.as_bytes()[0].into(),
                )),
                SnapshotSource::UserDictionary => {
                    Some(DictionaryIdentity::UserEntry(surface.as_bytes()[0].into()))
                }
                SnapshotSource::History => None,
            },
            base_cost: 100,
            authority,
            source,
            right_id: 1,
            is_it: false,
        }
    }

    fn snapshot(generation: u64, candidates: &[SnapshotCandidateInput<'_>]) -> PredictionSnapshot {
        PredictionSnapshot::build(
            10,
            20,
            generation,
            InputScope::Normal,
            true,
            false,
            candidates,
        )
        .expect("valid snapshot")
    }

    #[test]
    fn computes_quality_efficiency_stability_and_source_metrics() {
        let candidates = [
            input(
                "a",
                "A",
                CandidateAuthority::ExactLearning,
                SnapshotSource::History,
            ),
            input(
                "b",
                "B",
                CandidateAuthority::Ordinary,
                SnapshotSource::SystemDictionary,
            ),
            input(
                "c",
                "C",
                CandidateAuthority::Ordinary,
                SnapshotSource::SystemDictionary,
            ),
        ];
        let first = snapshot(1, &candidates);
        let second = snapshot(2, &candidates);
        let first_ids = first
            .candidates()
            .iter()
            .map(|candidate| candidate.candidate_id)
            .collect::<Vec<_>>();
        let second_ids = second
            .candidates()
            .iter()
            .map(|candidate| candidate.candidate_id)
            .collect::<Vec<_>>();
        let ranked_first = [first_ids[0], first_ids[2], first_ids[1]];
        let ranked_second = [second_ids[0], second_ids[1], second_ids[2]];
        let observations = [
            ReplayObservation {
                snapshot: &first,
                ranked_candidate_ids: &ranked_first,
                expected_candidate_id: Some(first_ids[2]),
                response_correlation: first.correlation(),
                keystrokes_without_prediction: 8,
                keystrokes_with_prediction: 3,
            },
            ReplayObservation {
                snapshot: &second,
                ranked_candidate_ids: &ranked_second,
                expected_candidate_id: Some(second_ids[1]),
                response_correlation: second.correlation(),
                keystrokes_without_prediction: 4,
                keystrokes_with_prediction: 2,
            },
        ];

        let metrics = evaluate_replay(&observations).expect("valid replay");
        assert_eq!(
            metrics.oracle_recall_at_9,
            Fraction {
                numerator: 2,
                denominator: 2
            }
        );
        assert_eq!(metrics.top_1.numerator, 0);
        assert_eq!(metrics.top_3.numerator, 2);
        assert_eq!(metrics.top_9.numerator, 2);
        assert!((metrics.mrr() - 0.5).abs() < f64::EPSILON);
        assert!(metrics.ndcg() > 0.6);
        assert_eq!(metrics.keystrokes_saved, 7);
        assert_eq!(metrics.keystrokes_without_prediction, 12);
        assert_eq!(
            metrics.persistence,
            Fraction {
                numerator: 1,
                denominator: 1
            }
        );
        assert_eq!(
            metrics.churn,
            Fraction {
                numerator: 0,
                denominator: 3
            }
        );
        assert_eq!(metrics.stale_responses.numerator, 0);
        assert_eq!(metrics.source_hits[1].top9_hits, 2);
    }

    #[test]
    fn stale_response_keeps_original_order_and_is_counted() {
        let candidates = [
            input(
                "a",
                "A",
                CandidateAuthority::Ordinary,
                SnapshotSource::SystemDictionary,
            ),
            input(
                "b",
                "B",
                CandidateAuthority::Ordinary,
                SnapshotSource::SystemDictionary,
            ),
        ];
        let snapshot = snapshot(1, &candidates);
        let ids = snapshot
            .candidates()
            .iter()
            .map(|candidate| candidate.candidate_id)
            .collect::<Vec<_>>();
        let ranked = [ids[1], ids[0]];
        let stale = SnapshotCorrelation {
            composition_generation: 99,
            ..snapshot.correlation()
        };
        let metrics = evaluate_replay(&[ReplayObservation {
            snapshot: &snapshot,
            ranked_candidate_ids: &ranked,
            expected_candidate_id: Some(ids[0]),
            response_correlation: stale,
            keystrokes_without_prediction: 2,
            keystrokes_with_prediction: 2,
        }])
        .expect("stale is an observable terminal outcome");
        assert_eq!(
            metrics.stale_responses,
            Fraction {
                numerator: 1,
                denominator: 1
            }
        );
        assert_eq!(metrics.top_1.numerator, 1, "original order remains visible");
    }

    #[test]
    fn protected_candidate_loss_or_reorder_fails_closed() {
        let candidates = [
            input(
                "a",
                "A",
                CandidateAuthority::ExactLearning,
                SnapshotSource::History,
            ),
            input(
                "b",
                "B",
                CandidateAuthority::UserDictionary,
                SnapshotSource::UserDictionary,
            ),
            input(
                "c",
                "C",
                CandidateAuthority::Ordinary,
                SnapshotSource::SystemDictionary,
            ),
        ];
        let snapshot = snapshot(1, &candidates);
        let ids = snapshot
            .candidates()
            .iter()
            .map(|candidate| candidate.candidate_id)
            .collect::<Vec<_>>();
        let observation = |ranked| ReplayObservation {
            snapshot: &snapshot,
            ranked_candidate_ids: ranked,
            expected_candidate_id: None,
            response_correlation: snapshot.correlation(),
            keystrokes_without_prediction: 0,
            keystrokes_with_prediction: 0,
        };
        let missing = [ids[0], ids[2]];
        let reordered = [ids[1], ids[0], ids[2]];
        let ordinary_first = [ids[0], ids[2], ids[1]];
        assert_eq!(
            evaluate_replay(&[observation(&missing)]),
            Err(EvaluationError::MissingProtectedCandidate)
        );
        assert_eq!(
            evaluate_replay(&[observation(&reordered)]),
            Err(EvaluationError::ProtectedOrderChanged)
        );
        assert_eq!(
            evaluate_replay(&[observation(&ordinary_first)]),
            Err(EvaluationError::OrdinaryBeforeProtected)
        );
    }
}
