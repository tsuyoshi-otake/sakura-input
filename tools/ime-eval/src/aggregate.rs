use crate::types::{CaseRecord, SemanticOutcome};

#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub pair_count: usize,
    pub hard_failures: usize,
    pub literal_corruption: usize,
    pub gradable: usize,
    pub candidate_better: usize,
    pub baseline_better: usize,
    pub tie: usize,
    pub ungradable: usize,
    pub unstable: usize,
    pub severity4_regression: usize,
    pub severity3_regression: usize,
    pub severity2_regression: usize,
    pub severity1_regression: usize,
}

impl Aggregate {
    pub fn from_records(records: &[CaseRecord]) -> Self {
        let mut agg = Self {
            pair_count: records.len(),
            hard_failures: 0,
            literal_corruption: 0,
            gradable: 0,
            candidate_better: 0,
            baseline_better: 0,
            tie: 0,
            ungradable: 0,
            unstable: 0,
            severity4_regression: 0,
            severity3_regression: 0,
            severity2_regression: 0,
            severity1_regression: 0,
        };
        for record in records {
            if record.hard_failure {
                agg.hard_failures += 1;
            }
            if record.oracle_candidate == "literal_corruption" {
                agg.literal_corruption += 1;
            }
            match record.semantic {
                Some(SemanticOutcome::CandidateBetter) => {
                    agg.candidate_better += 1;
                    agg.gradable += 1;
                }
                Some(SemanticOutcome::BaselineBetter) => {
                    agg.baseline_better += 1;
                    agg.gradable += 1;
                    match record.severity {
                        4 => agg.severity4_regression += 1,
                        3 => agg.severity3_regression += 1,
                        2 => agg.severity2_regression += 1,
                        1 => agg.severity1_regression += 1,
                        _ => {}
                    }
                }
                Some(SemanticOutcome::Tie) => {
                    agg.tie += 1;
                    agg.gradable += 1;
                }
                Some(SemanticOutcome::Ungradable) => agg.ungradable += 1,
                Some(SemanticOutcome::Unstable) => agg.unstable += 1,
                None => {}
            }
        }
        agg
    }

    pub fn material_regression_rate(&self) -> Option<f64> {
        if self.gradable == 0 {
            None
        } else {
            Some(self.baseline_better as f64 / self.gradable as f64)
        }
    }

    pub fn decisive_win_rate(&self) -> Option<f64> {
        let decisive = self.candidate_better + self.baseline_better;
        if decisive == 0 {
            None
        } else {
            Some(self.candidate_better as f64 / decisive as f64)
        }
    }

    pub fn quality_delta(&self) -> Option<f64> {
        if self.gradable == 0 {
            None
        } else {
            Some(
                (self.candidate_better as f64 - self.baseline_better as f64) / self.gradable as f64,
            )
        }
    }

    pub fn unstable_rate(&self) -> Option<f64> {
        let judged = self.gradable + self.ungradable + self.unstable;
        if judged == 0 {
            None
        } else {
            Some(self.unstable as f64 / judged as f64)
        }
    }
}

pub fn wilson_interval(successes: u64, n: u64, z: f64) -> Option<(f64, f64)> {
    if n == 0 {
        return None;
    }
    let p = successes as f64 / n as f64;
    let z2 = z * z;
    let n = n as f64;
    let denom = 1.0 + z2 / n;
    let center = p + z2 / (2.0 * n);
    let margin = z * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt();
    Some(((center - margin) / denom, (center + margin) / denom))
}
