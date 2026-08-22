use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::types::{err, Error, JudgeResult, Verdict, REASON_CODES};

pub const MIN_OVERALL_AGREEMENT: f64 = 0.90;
pub const MIN_MATERIAL_AGREEMENT: f64 = 0.92;
pub const MIN_MAJOR_RECALL: f64 = 0.95;
pub const MIN_WEIGHTED_KAPPA: f64 = 0.80;
pub const MAX_UNGRADABLE_DISAGREEMENT: f64 = 0.10;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CalibrationObservation {
    pub case_id: String,
    pub human: JudgeResult,
    pub judge: JudgeResult,
    #[serde(default)]
    pub literal_corruption: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CalibrationFile {
    pub schema_version: u32,
    pub split: String,
    pub observations: Vec<CalibrationObservation>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CalibrationMetrics {
    pub total: usize,
    pub overall_agreement: f64,
    pub material_total: usize,
    pub material_agreement: usize,
    pub material_agreement_rate: Option<f64>,
    pub major_regressions: usize,
    pub major_regressions_detected: usize,
    pub major_regression_recall: Option<f64>,
    pub literal_corruption_cases: usize,
    pub literal_corruption_false_negatives: usize,
    pub weighted_severity_kappa: Option<f64>,
    pub ungradable_disagreements: usize,
    pub ungradable_disagreement_rate: f64,
}

impl CalibrationMetrics {
    pub fn meets_acceptance(&self) -> bool {
        self.total > 0
            && self.overall_agreement >= MIN_OVERALL_AGREEMENT
            && self
                .material_agreement_rate
                .is_some_and(|rate| rate >= MIN_MATERIAL_AGREEMENT)
            && self
                .major_regression_recall
                .is_some_and(|recall| recall >= MIN_MAJOR_RECALL)
            && self.literal_corruption_false_negatives == 0
            && self
                .weighted_severity_kappa
                .is_some_and(|kappa| kappa >= MIN_WEIGHTED_KAPPA)
            && self.ungradable_disagreement_rate <= MAX_UNGRADABLE_DISAGREEMENT
    }

    pub fn major_recall_passes(&self) -> bool {
        self.major_regression_recall
            .is_some_and(|recall| recall >= MIN_MAJOR_RECALL)
            && self.literal_corruption_false_negatives == 0
    }
}

pub fn load(path: &Path) -> Result<CalibrationFile, Error> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| err(format!("read {}: {error}", path.display())))?;
    let file: CalibrationFile = serde_json::from_str(&text)
        .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
    validate_file(&file)?;
    Ok(file)
}

pub fn calculate(observations: &[CalibrationObservation]) -> Result<CalibrationMetrics, Error> {
    validate_observations(observations)?;
    let total = observations.len();
    if total == 0 {
        return Err(err("calibration set is empty"));
    }

    let overall_agreement = observations
        .iter()
        .filter(|observation| observation.human.verdict == observation.judge.verdict)
        .count() as f64
        / total as f64;
    let material: Vec<&CalibrationObservation> = observations
        .iter()
        .filter(|observation| observation.human.verdict.is_decisive())
        .collect();
    let material_total = material.len();
    let material_agreement = material
        .iter()
        .filter(|observation| observation.human.verdict == observation.judge.verdict)
        .count();
    let material_agreement_rate = ratio(material_agreement, material_total);

    let major_regressions = observations
        .iter()
        .filter(|observation| observation.human.severity >= 3)
        .count();
    let major_regressions_detected = observations
        .iter()
        .filter(|observation| observation.human.severity >= 3 && observation.judge.severity >= 3)
        .count();
    let major_regression_recall = ratio(major_regressions_detected, major_regressions);

    let literal_corruption_cases = observations
        .iter()
        .filter(|observation| observation.literal_corruption)
        .count();
    let literal_corruption_false_negatives = observations
        .iter()
        .filter(|observation| {
            observation.literal_corruption
                && !observation
                    .judge
                    .reason_codes
                    .iter()
                    .any(|code| code == "literal_corruption")
        })
        .count();

    let ungradable_disagreements = observations
        .iter()
        .filter(|observation| {
            (observation.human.verdict == Verdict::Ungradable)
                != (observation.judge.verdict == Verdict::Ungradable)
        })
        .count();
    let ungradable_disagreement_rate = ungradable_disagreements as f64 / total as f64;

    Ok(CalibrationMetrics {
        total,
        overall_agreement,
        material_total,
        material_agreement,
        material_agreement_rate,
        major_regressions,
        major_regressions_detected,
        major_regression_recall,
        literal_corruption_cases,
        literal_corruption_false_negatives,
        weighted_severity_kappa: weighted_kappa(
            &observations
                .iter()
                .map(|observation| (observation.human.severity, observation.judge.severity))
                .collect::<Vec<_>>(),
        ),
        ungradable_disagreements,
        ungradable_disagreement_rate,
    })
}

fn validate_file(file: &CalibrationFile) -> Result<(), Error> {
    if file.schema_version != 1 {
        return Err(err(format!(
            "unsupported calibration schema_version {}",
            file.schema_version
        )));
    }
    if file.split != "train-like" && file.split != "holdout" {
        return Err(err(format!("unknown calibration split {}", file.split)));
    }
    validate_observations(&file.observations)
}

fn validate_observations(observations: &[CalibrationObservation]) -> Result<(), Error> {
    let mut ids = BTreeSet::new();
    for observation in observations {
        if observation.case_id.is_empty() || !ids.insert(&observation.case_id) {
            return Err(err("calibration case_id must be non-empty and unique"));
        }
        if observation.human.case_id != observation.case_id
            || observation.judge.case_id != observation.case_id
        {
            return Err(err(format!(
                "calibration {} has inconsistent result case_id",
                observation.case_id
            )));
        }
        validate_result(&observation.human)?;
        validate_result(&observation.judge)?;
    }
    Ok(())
}

fn validate_result(result: &JudgeResult) -> Result<(), Error> {
    if result.severity > 4 || result.short_reason.chars().count() > 240 {
        return Err(err(format!(
            "invalid calibration result {}",
            result.case_id
        )));
    }
    if matches!(result.verdict, Verdict::Tie | Verdict::Ungradable) && result.severity != 0 {
        return Err(err(format!(
            "calibration {} has non-zero severity for tie/ungradable",
            result.case_id
        )));
    }
    for code in &result.reason_codes {
        if !REASON_CODES.contains(&code.as_str()) {
            return Err(err(format!(
                "calibration {} has unknown reason code {code}",
                result.case_id
            )));
        }
    }
    Ok(())
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then_some(numerator as f64 / denominator as f64)
}

fn weighted_kappa(pairs: &[(u8, u8)]) -> Option<f64> {
    if pairs.is_empty() {
        return None;
    }
    const CATEGORIES: usize = 5;
    const MAX_DISTANCE: f64 = 16.0;
    let n = pairs.len() as f64;
    let observed = pairs
        .iter()
        .map(|(human, judge)| {
            let distance = (*human as f64 - *judge as f64).powi(2);
            1.0 - distance / MAX_DISTANCE
        })
        .sum::<f64>()
        / n;
    let mut human_marginal = [0.0; CATEGORIES];
    let mut judge_marginal = [0.0; CATEGORIES];
    for (human, judge) in pairs {
        human_marginal[*human as usize] += 1.0 / n;
        judge_marginal[*judge as usize] += 1.0 / n;
    }
    let expected = human_marginal
        .iter()
        .enumerate()
        .flat_map(|(human, human_probability)| {
            judge_marginal
                .iter()
                .enumerate()
                .map(move |(judge, judge_probability)| {
                    let distance = (human as f64 - judge as f64).powi(2);
                    human_probability * judge_probability * (1.0 - distance / MAX_DISTANCE)
                })
        })
        .sum::<f64>();
    if (1.0 - expected).abs() < f64::EPSILON {
        return Some(if (observed - expected).abs() < f64::EPSILON {
            1.0
        } else {
            0.0
        });
    }
    Some((observed - expected) / (1.0 - expected))
}
