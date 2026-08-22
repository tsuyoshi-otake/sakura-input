use crate::aggregate::{wilson_interval, Aggregate};
use crate::calibration::CalibrationMetrics;
use crate::identity::RunIdentity;
use crate::types::{GateCheck, GateProfile};

pub fn evaluate(
    profile: GateProfile,
    aggregate: &Aggregate,
    identity: &RunIdentity,
    calibration: Option<&CalibrationMetrics>,
) -> Vec<GateCheck> {
    let mut checks = Vec::new();
    checks.push(check(
        "GATE-01",
        aggregate.hard_failures == 0,
        format!("deterministic failures={}", aggregate.hard_failures),
    ));
    checks.push(check(
        "GATE-02",
        aggregate.literal_corruption == 0,
        format!("literal corruption={}", aggregate.literal_corruption),
    ));
    checks.push(check(
        "GATE-03",
        aggregate.severity4_regression == 0,
        format!("severity-4 regressions={}", aggregate.severity4_regression),
    ));
    checks.push(check(
        "GATE-04",
        aggregate.severity3_regression == 0,
        format!("severity-3 regressions={}", aggregate.severity3_regression),
    ));
    let regression_upper = aggregate
        .material_regression_rate()
        .and_then(|_| {
            wilson_interval(
                aggregate.baseline_better as u64,
                aggregate.gradable as u64,
                1.96,
            )
        })
        .map(|(_, upper)| upper);
    if matches!(profile, GateProfile::Release) {
        checks.push(check(
            "GATE-05",
            match regression_upper {
                Some(upper) => upper <= 0.01,
                None => true,
            },
            format!("material regression Wilson upper={regression_upper:?}"),
        ));
    }
    let unstable = aggregate.unstable_rate().unwrap_or(0.0);
    checks.push(check(
        "GATE-06",
        unstable <= 0.03,
        format!("unstable rate={unstable:.4}"),
    ));
    checks.push(check(
        "GATE-09",
        identity.judge_known(),
        "judge identity".to_owned(),
    ));
    checks.push(check(
        "GATE-10",
        identity.artifacts_known(),
        "artifact identity".to_owned(),
    ));
    if matches!(profile, GateProfile::Release) {
        checks.push(check(
            "GATE-07",
            calibration.is_some_and(CalibrationMetrics::meets_acceptance),
            calibration_detail(calibration),
        ));
        checks.push(check(
            "GATE-08",
            calibration.is_some_and(CalibrationMetrics::major_recall_passes),
            calibration_major_detail(calibration),
        ));
    }
    checks
}

fn calibration_detail(calibration: Option<&CalibrationMetrics>) -> String {
    let Some(metrics) = calibration else {
        return "human calibration metrics are missing".to_owned();
    };
    format!(
        "agreement={:.4}, material={:?}, kappa={:?}, ungradable_disagreement={:.4}",
        metrics.overall_agreement,
        metrics.material_agreement_rate,
        metrics.weighted_severity_kappa,
        metrics.ungradable_disagreement_rate
    )
}

fn calibration_major_detail(calibration: Option<&CalibrationMetrics>) -> String {
    let Some(metrics) = calibration else {
        return "human calibration metrics are missing".to_owned();
    };
    format!(
        "major_recall={:?}, literal_false_negatives={}",
        metrics.major_regression_recall, metrics.literal_corruption_false_negatives
    )
}

fn check(id: &str, passed: bool, detail: String) -> GateCheck {
    GateCheck {
        id: id.to_owned(),
        passed,
        detail,
    }
}

pub fn all_passed(checks: &[GateCheck]) -> bool {
    checks.iter().all(|check| check.passed)
}
