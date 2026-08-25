//! Deterministic before/after comparison for versioned quality observations.
//!
//! A `QualityScoreboard` contains a baseline and a candidate observation from
//! one run.  This module compares the selected side of two *independently
//! produced* scoreboards.  In particular, it does not treat the baseline in
//! one scoreboard as the before value: callers can select the side explicitly
//! and the selected artifact identity is retained in the comparison output.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::hash::sha256_hex;
use crate::quality::{
    QualityArtifactIdentity, QualityNegativeObservation, QualityObservation,
    QualityOptionsIdentity, QualityScoreboard, QualitySystemScore, QUALITY_CANDIDATE_LIMIT,
    QUALITY_SCHEMA_VERSION, WHOLE_READING_CAPTURE_LANE,
};
use crate::types::{err, Error};

pub const QUALITY_COMPARISON_SCHEMA_VERSION: u32 = 1;
const MAX_SCOREBOARD_BYTES: usize = 64 * 1024 * 1024;

/// Which side of each input scoreboard is compared.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonSide {
    Baseline,
    Candidate,
}

impl ComparisonSide {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "baseline" => Ok(Self::Baseline),
            "candidate" => Ok(Self::Candidate),
            _ => Err(err(format!(
                "--side must be baseline or candidate, got {value}"
            ))),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        }
    }
}

/// Direction of a quality metric from before to after.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeDirection {
    Improved,
    Regressed,
    Unchanged,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct BoolChange {
    pub before: bool,
    pub after: bool,
    /// `after - before`: +1 is an improvement and -1 is a regression.
    pub delta: i8,
    pub direction: ChangeDirection,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct OptionalBoolChange {
    pub before: Option<bool>,
    pub after: Option<bool>,
    /// Set only when both reports observed the metric.  This avoids treating
    /// an unsupported metric as a false value.
    pub delta: Option<i8>,
    pub direction: ChangeDirection,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankChange {
    pub before: Option<usize>,
    pub after: Option<usize>,
    /// `after - before` when both ranks are present.  A negative value means
    /// the expected surface moved closer to the top of the list.
    pub delta: Option<i64>,
    pub entered_top18: bool,
    pub exited_top18: bool,
    pub direction: ChangeDirection,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityObservationSnapshot {
    pub candidate_surfaces: Vec<String>,
    pub surface_top1: bool,
    pub surface_rank: Option<usize>,
    pub surface_in_top18: bool,
    pub segment_exact: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityCaseComparison {
    pub case_id: String,
    pub before: QualityObservationSnapshot,
    pub after: QualityObservationSnapshot,
    pub top1: BoolChange,
    pub recall_at18: BoolChange,
    pub rank: RankChange,
    pub segment_exact: OptionalBoolChange,
    pub changed: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityControlComparison {
    pub control_id: String,
    pub surface: String,
    pub top1: BoolChange,
    pub recall_at18: BoolChange,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct QualityComparisonSummary {
    pub total: usize,
    pub changed_cases: usize,

    pub top1_before: usize,
    pub top1_after: usize,
    pub top1_delta: i64,
    pub top1_improved: usize,
    pub top1_regressed: usize,
    pub top1_unchanged: usize,

    pub recall_at18_before: usize,
    pub recall_at18_after: usize,
    pub recall_at18_delta: i64,
    pub recall_at18_improved: usize,
    pub recall_at18_regressed: usize,
    pub recall_at18_unchanged: usize,

    pub mrr_at18_before: f64,
    pub mrr_at18_after: f64,
    pub mrr_at18_delta: f64,

    pub rank_improved: usize,
    pub rank_regressed: usize,
    pub rank_unchanged: usize,
    pub rank_entered_top18: usize,
    pub rank_exited_top18: usize,

    pub segment_exact_before: usize,
    pub segment_exact_after: usize,
    pub segment_exact_delta: i64,

    pub negative_control_total: usize,
    pub negative_control_top1_before: usize,
    pub negative_control_top1_after: usize,
    pub negative_control_top1_delta: i64,
    pub negative_control_recall_at18_before: usize,
    pub negative_control_recall_at18_after: usize,
    pub negative_control_recall_at18_delta: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityComparisonIdentity {
    pub artifact: QualityArtifactIdentity,
    pub options: QualityOptionsIdentity,
    /// Fingerprint of the complete input scoreboard, including its selected
    /// and unselected system observations.
    pub report_determinism_fingerprint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct QualityComparison {
    pub schema_version: u32,
    pub side: ComparisonSide,
    pub corpus_id: String,
    pub capture_lane: String,
    pub candidate_limit: usize,
    pub before: QualityComparisonIdentity,
    pub after: QualityComparisonIdentity,
    pub summary: QualityComparisonSummary,
    pub cases: Vec<QualityCaseComparison>,
    pub negative_controls: Vec<QualityControlComparison>,
    pub determinism_fingerprint: String,
}

impl QualityComparison {
    /// Render one stable line suitable for the existing CLI's human output.
    pub fn human_summary(&self) -> String {
        let summary = &self.summary;
        format!(
            "quality-comparison\tside={}\tcases={}\ttop1_before={}\ttop1_after={}\ttop1_delta={:+}\trecall_at18_before={}\trecall_at18_after={}\trecall_at18_delta={:+}\trank_improved={}\trank_regressed={}\tmrr_delta={:+.6}\tfingerprint={}",
            self.side.as_str(),
            summary.total,
            summary.top1_before,
            summary.top1_after,
            summary.top1_delta,
            summary.recall_at18_before,
            summary.recall_at18_after,
            summary.recall_at18_delta,
            summary.rank_improved,
            summary.rank_regressed,
            summary.mrr_at18_delta,
            self.determinism_fingerprint,
        )
    }
}

/// Load and validate a quality scoreboard emitted by `quality-score`.
pub fn load_scoreboard(path: &Path) -> Result<QualityScoreboard, Error> {
    let bytes = fs::read(path).map_err(|error| err(format!("read {}: {error}", path.display())))?;
    if bytes.len() > MAX_SCOREBOARD_BYTES {
        return Err(err(format!(
            "quality scoreboard exceeds {MAX_SCOREBOARD_BYTES} bytes"
        )));
    }
    let scoreboard: QualityScoreboard = serde_json::from_slice(&bytes)
        .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
    validate_scoreboard(&scoreboard, &path.display().to_string())?;
    Ok(scoreboard)
}

/// Compare the selected side of two already-loaded quality scoreboards.
pub fn compare(
    before: &QualityScoreboard,
    after: &QualityScoreboard,
    side: ComparisonSide,
) -> Result<QualityComparison, Error> {
    validate_scoreboard(before, "before scoreboard")?;
    validate_scoreboard(after, "after scoreboard")?;
    validate_compatible_metadata(before, after)?;

    let before_system = selected_system(before, side);
    let after_system = selected_system(after, side);
    let before_observations = observation_map(before_system, "before")?;
    let after_observations = observation_map(after_system, "after")?;

    if before_observations.len() != after_observations.len() {
        return Err(err(format!(
            "before/after case count differs: {} vs {}",
            before_observations.len(),
            after_observations.len()
        )));
    }
    for case_id in before_observations.keys() {
        if !after_observations.contains_key(case_id) {
            return Err(err(format!(
                "after scoreboard is missing case_id {case_id}"
            )));
        }
    }
    for case_id in after_observations.keys() {
        if !before_observations.contains_key(case_id) {
            return Err(err(format!(
                "before scoreboard is missing case_id {case_id}"
            )));
        }
    }

    let mut cases = Vec::with_capacity(before_observations.len());
    for (case_id, before_observation) in &before_observations {
        let after_observation = after_observations[case_id];
        validate_case_contract(case_id, before_observation, after_observation)?;
        cases.push(compare_case(case_id, before_observation, after_observation));
    }

    let before_controls = control_map(&before_system.negative_controls, "before")?;
    let after_controls = control_map(&after_system.negative_controls, "after")?;
    if before_controls.len() != after_controls.len() {
        return Err(err(format!(
            "before/after negative-control count differs: {} vs {}",
            before_controls.len(),
            after_controls.len()
        )));
    }
    for control_id in before_controls.keys() {
        if !after_controls.contains_key(control_id) {
            return Err(err(format!(
                "after scoreboard is missing control_id {control_id}"
            )));
        }
    }
    let mut negative_controls = Vec::with_capacity(before_controls.len());
    for (control_id, before_control) in &before_controls {
        let after_control = after_controls[control_id];
        if before_control.surface != after_control.surface
            || before_control.policy != after_control.policy
        {
            return Err(err(format!(
                "negative control {control_id} changed surface or policy"
            )));
        }
        negative_controls.push(QualityControlComparison {
            control_id: control_id.clone(),
            surface: before_control.surface.clone(),
            top1: bool_change(before_control.top1, after_control.top1),
            recall_at18: bool_change(before_control.in_top18, after_control.in_top18),
        });
    }

    let summary = summarize(&cases, &negative_controls, before_system, after_system);
    let mut result = QualityComparison {
        schema_version: QUALITY_COMPARISON_SCHEMA_VERSION,
        side,
        corpus_id: before.corpus_id.clone(),
        capture_lane: before.capture_lane.clone(),
        candidate_limit: before.candidate_limit,
        before: comparison_identity(before, before_system),
        after: comparison_identity(after, after_system),
        summary,
        cases,
        negative_controls,
        determinism_fingerprint: String::new(),
    };
    result.determinism_fingerprint = comparison_fingerprint(&result)?;
    Ok(result)
}

/// Compare two scoreboard files.  This is the entry point used by the CLI.
pub fn compare_files(
    before_path: &Path,
    after_path: &Path,
    side: ComparisonSide,
) -> Result<QualityComparison, Error> {
    let before = load_scoreboard(before_path)?;
    let after = load_scoreboard(after_path)?;
    compare(&before, &after, side)
}

/// Alias kept explicit for callers that want to distinguish this operation
/// from other comparison utilities in the evaluation tool.
pub fn compare_scoreboards(
    before: &QualityScoreboard,
    after: &QualityScoreboard,
    side: ComparisonSide,
) -> Result<QualityComparison, Error> {
    compare(before, after, side)
}

fn validate_scoreboard(scoreboard: &QualityScoreboard, name: &str) -> Result<(), Error> {
    if scoreboard.schema_version != QUALITY_SCHEMA_VERSION {
        return Err(err(format!(
            "{name} has unsupported quality scoreboard schema_version {}",
            scoreboard.schema_version
        )));
    }
    if scoreboard.corpus_id.is_empty() {
        return Err(err(format!("{name} has an empty corpus_id")));
    }
    if scoreboard.capture_lane != WHOLE_READING_CAPTURE_LANE {
        return Err(err(format!(
            "{name} must use {WHOLE_READING_CAPTURE_LANE}; got {}",
            scoreboard.capture_lane
        )));
    }
    if scoreboard.candidate_limit != QUALITY_CANDIDATE_LIMIT
        || scoreboard.options.candidate_limit != scoreboard.candidate_limit
    {
        return Err(err(format!(
            "{name} has unsupported candidate_limit {}",
            scoreboard.candidate_limit
        )));
    }
    if scoreboard.determinism_fingerprint.is_empty() {
        return Err(err(format!("{name} has an empty determinism_fingerprint")));
    }
    validate_options(&scoreboard.options, name)?;
    validate_system(
        &scoreboard.baseline,
        "baseline",
        scoreboard.candidate_limit,
        name,
    )?;
    validate_system(
        &scoreboard.candidate,
        "candidate",
        scoreboard.candidate_limit,
        name,
    )?;
    Ok(())
}

fn validate_options(options: &QualityOptionsIdentity, name: &str) -> Result<(), Error> {
    if options.profile.is_empty()
        || options.options_sha256.is_empty()
        || options.config_sha256.is_empty()
        || options.learning.is_empty()
        || options.user_dictionary.is_empty()
        || options.reranker.is_empty()
    {
        return Err(err(format!("{name} has incomplete options identity")));
    }
    Ok(())
}

fn validate_system(
    system: &QualitySystemScore,
    side: &str,
    candidate_limit: usize,
    scoreboard_name: &str,
) -> Result<(), Error> {
    validate_artifact(&system.artifact, &format!("{scoreboard_name} {side}"))?;
    if system.summary.total != system.observations.len() {
        return Err(err(format!(
            "{scoreboard_name} {side} summary total does not match observations"
        )));
    }
    for observation in &system.observations {
        if observation.case_id.is_empty() {
            return Err(err(format!(
                "{scoreboard_name} {side} contains an empty case_id"
            )));
        }
        if observation.candidate_limit != candidate_limit
            || observation.candidate_surfaces.len() != observation.candidates.len()
            || observation.candidate_surfaces.is_empty()
            || observation.candidate_surfaces.len() > candidate_limit
        {
            return Err(err(format!(
                "{scoreboard_name} {side} case {} has malformed candidate observation",
                observation.case_id
            )));
        }
    }
    control_map(&system.negative_controls, side)?;
    Ok(())
}

fn validate_artifact(artifact: &QualityArtifactIdentity, name: &str) -> Result<(), Error> {
    if artifact.git_sha.is_empty()
        || artifact.evaluator_sha256.is_empty()
        || artifact.dictionary_sha256.is_empty()
    {
        return Err(err(format!("{name} has incomplete artifact identity")));
    }
    Ok(())
}

fn validate_compatible_metadata(
    before: &QualityScoreboard,
    after: &QualityScoreboard,
) -> Result<(), Error> {
    if before.corpus_id != after.corpus_id {
        return Err(err(format!(
            "before/after corpus_id differs: {} vs {}",
            before.corpus_id, after.corpus_id
        )));
    }
    if before.capture_lane != after.capture_lane {
        return Err(err(format!(
            "before/after capture_lane differs: {} vs {}",
            before.capture_lane, after.capture_lane
        )));
    }
    if before.candidate_limit != after.candidate_limit {
        return Err(err(format!(
            "before/after candidate_limit differs: {} vs {}",
            before.candidate_limit, after.candidate_limit
        )));
    }
    if before.options != after.options {
        return Err(err(
            "before/after options or config identity differs; compare requires one fixed capture configuration",
        ));
    }
    Ok(())
}

fn selected_system(scoreboard: &QualityScoreboard, side: ComparisonSide) -> &QualitySystemScore {
    match side {
        ComparisonSide::Baseline => &scoreboard.baseline,
        ComparisonSide::Candidate => &scoreboard.candidate,
    }
}

fn comparison_identity(
    scoreboard: &QualityScoreboard,
    system: &QualitySystemScore,
) -> QualityComparisonIdentity {
    QualityComparisonIdentity {
        artifact: system.artifact.clone(),
        options: scoreboard.options.clone(),
        report_determinism_fingerprint: scoreboard.determinism_fingerprint.clone(),
    }
}

fn observation_map<'a>(
    system: &'a QualitySystemScore,
    name: &str,
) -> Result<BTreeMap<String, &'a QualityObservation>, Error> {
    let mut map = BTreeMap::new();
    for observation in &system.observations {
        if map
            .insert(observation.case_id.clone(), observation)
            .is_some()
        {
            return Err(err(format!(
                "{name} scoreboard contains duplicate case_id {}",
                observation.case_id
            )));
        }
    }
    Ok(map)
}

fn control_map<'a>(
    controls: &'a [QualityNegativeObservation],
    name: &str,
) -> Result<BTreeMap<String, &'a QualityNegativeObservation>, Error> {
    let mut map = BTreeMap::new();
    for control in controls {
        if control.control_id.is_empty()
            || map.insert(control.control_id.clone(), control).is_some()
        {
            return Err(err(format!(
                "{name} scoreboard contains duplicate or empty control_id"
            )));
        }
    }
    Ok(map)
}

fn validate_case_contract(
    case_id: &str,
    before: &QualityObservation,
    after: &QualityObservation,
) -> Result<(), Error> {
    if before.category != after.category
        || before.reading != after.reading
        || before.expected_surface != after.expected_surface
        || before.expected_segments != after.expected_segments
        || before.segment_assertion != after.segment_assertion
        || before.assertion_scope != after.assertion_scope
    {
        return Err(err(format!(
            "case {case_id} changed fixture contract between before and after"
        )));
    }
    Ok(())
}

fn snapshot(observation: &QualityObservation) -> QualityObservationSnapshot {
    QualityObservationSnapshot {
        candidate_surfaces: observation.candidate_surfaces.clone(),
        surface_top1: observation.surface_top1,
        surface_rank: observation.surface_rank,
        surface_in_top18: observation.surface_in_top18,
        segment_exact: observation.segment_exact,
    }
}

fn compare_case(
    case_id: &str,
    before: &QualityObservation,
    after: &QualityObservation,
) -> QualityCaseComparison {
    let top1 = bool_change(before.surface_top1, after.surface_top1);
    let recall_at18 = bool_change(before.surface_in_top18, after.surface_in_top18);
    let rank = rank_change(before.surface_rank, after.surface_rank);
    let segment_exact = optional_bool_change(before.segment_exact, after.segment_exact);
    let changed = top1.direction != ChangeDirection::Unchanged
        || recall_at18.direction != ChangeDirection::Unchanged
        || rank.direction != ChangeDirection::Unchanged
        || segment_exact.direction != ChangeDirection::Unchanged;
    QualityCaseComparison {
        case_id: case_id.to_owned(),
        before: snapshot(before),
        after: snapshot(after),
        top1,
        recall_at18,
        rank,
        segment_exact,
        changed,
    }
}

pub(crate) fn bool_change(before: bool, after: bool) -> BoolChange {
    let delta = after as i8 - before as i8;
    BoolChange {
        before,
        after,
        delta,
        direction: direction_from_delta(delta),
    }
}

fn optional_bool_change(before: Option<bool>, after: Option<bool>) -> OptionalBoolChange {
    let delta = match (before, after) {
        (Some(before), Some(after)) => Some(after as i8 - before as i8),
        _ => None,
    };
    let direction = match (before, after) {
        (Some(false), Some(true)) | (None, Some(true)) => ChangeDirection::Improved,
        (Some(true), Some(false)) | (Some(true), None) => ChangeDirection::Regressed,
        _ => ChangeDirection::Unchanged,
    };
    OptionalBoolChange {
        before,
        after,
        delta,
        direction,
    }
}

pub(crate) fn rank_change(before: Option<usize>, after: Option<usize>) -> RankChange {
    let entered_top18 = before.is_none() && after.is_some();
    let exited_top18 = before.is_some() && after.is_none();
    let delta = before
        .zip(after)
        .map(|(before, after)| after as i64 - before as i64);
    let direction = match (before, after) {
        (Some(before), Some(after)) if after < before => ChangeDirection::Improved,
        (Some(before), Some(after)) if after > before => ChangeDirection::Regressed,
        (None, Some(_)) => ChangeDirection::Improved,
        (Some(_), None) => ChangeDirection::Regressed,
        _ => ChangeDirection::Unchanged,
    };
    RankChange {
        before,
        after,
        delta,
        entered_top18,
        exited_top18,
        direction,
    }
}

fn direction_from_delta(delta: i8) -> ChangeDirection {
    match delta.cmp(&0) {
        std::cmp::Ordering::Greater => ChangeDirection::Improved,
        std::cmp::Ordering::Less => ChangeDirection::Regressed,
        std::cmp::Ordering::Equal => ChangeDirection::Unchanged,
    }
}

fn summarize(
    cases: &[QualityCaseComparison],
    controls: &[QualityControlComparison],
    before_system: &QualitySystemScore,
    after_system: &QualitySystemScore,
) -> QualityComparisonSummary {
    let top1_before = cases
        .iter()
        .filter(|case_| case_.before.surface_top1)
        .count();
    let top1_after = cases
        .iter()
        .filter(|case_| case_.after.surface_top1)
        .count();
    let recall_before = cases
        .iter()
        .filter(|case_| case_.before.surface_in_top18)
        .count();
    let recall_after = cases
        .iter()
        .filter(|case_| case_.after.surface_in_top18)
        .count();
    let segment_before = cases
        .iter()
        .filter(|case_| case_.before.segment_exact == Some(true))
        .count();
    let segment_after = cases
        .iter()
        .filter(|case_| case_.after.segment_exact == Some(true))
        .count();
    let mrr_before = mean_reciprocal_rank(cases, true);
    let mrr_after = mean_reciprocal_rank(cases, false);
    let top1_improved = cases
        .iter()
        .filter(|case_| case_.top1.direction == ChangeDirection::Improved)
        .count();
    let top1_regressed = cases
        .iter()
        .filter(|case_| case_.top1.direction == ChangeDirection::Regressed)
        .count();
    let recall_improved = cases
        .iter()
        .filter(|case_| case_.recall_at18.direction == ChangeDirection::Improved)
        .count();
    let recall_regressed = cases
        .iter()
        .filter(|case_| case_.recall_at18.direction == ChangeDirection::Regressed)
        .count();
    let rank_improved = cases
        .iter()
        .filter(|case_| case_.rank.direction == ChangeDirection::Improved)
        .count();
    let rank_regressed = cases
        .iter()
        .filter(|case_| case_.rank.direction == ChangeDirection::Regressed)
        .count();
    let rank_unchanged = cases
        .iter()
        .filter(|case_| case_.rank.direction == ChangeDirection::Unchanged)
        .count();
    let rank_entered_top18 = cases
        .iter()
        .filter(|case_| case_.rank.entered_top18)
        .count();
    let rank_exited_top18 = cases.iter().filter(|case_| case_.rank.exited_top18).count();
    let control_top1_before = controls
        .iter()
        .filter(|control| control.top1.before)
        .count();
    let control_top1_after = controls.iter().filter(|control| control.top1.after).count();
    let control_recall_before = controls
        .iter()
        .filter(|control| control.recall_at18.before)
        .count();
    let control_recall_after = controls
        .iter()
        .filter(|control| control.recall_at18.after)
        .count();

    // The summaries in a valid scoreboard must agree with its observations;
    // use the observations for the comparison so the result is case-auditable.
    debug_assert_eq!(before_system.summary.total, cases.len());
    debug_assert_eq!(after_system.summary.total, cases.len());
    QualityComparisonSummary {
        total: cases.len(),
        changed_cases: cases.iter().filter(|case_| case_.changed).count(),
        top1_before,
        top1_after,
        top1_delta: top1_after as i64 - top1_before as i64,
        top1_improved,
        top1_regressed,
        top1_unchanged: cases.len() - top1_improved - top1_regressed,
        recall_at18_before: recall_before,
        recall_at18_after: recall_after,
        recall_at18_delta: recall_after as i64 - recall_before as i64,
        recall_at18_improved: recall_improved,
        recall_at18_regressed: recall_regressed,
        recall_at18_unchanged: cases.len() - recall_improved - recall_regressed,
        mrr_at18_before: mrr_before,
        mrr_at18_after: mrr_after,
        mrr_at18_delta: normalize_zero(mrr_after - mrr_before),
        rank_improved,
        rank_regressed,
        rank_unchanged,
        rank_entered_top18,
        rank_exited_top18,
        segment_exact_before: segment_before,
        segment_exact_after: segment_after,
        segment_exact_delta: segment_after as i64 - segment_before as i64,
        negative_control_total: controls.len(),
        negative_control_top1_before: control_top1_before,
        negative_control_top1_after: control_top1_after,
        negative_control_top1_delta: control_top1_after as i64 - control_top1_before as i64,
        negative_control_recall_at18_before: control_recall_before,
        negative_control_recall_at18_after: control_recall_after,
        negative_control_recall_at18_delta: control_recall_after as i64
            - control_recall_before as i64,
    }
}

fn mean_reciprocal_rank(cases: &[QualityCaseComparison], before: bool) -> f64 {
    if cases.is_empty() {
        return 0.0;
    }
    let sum = cases
        .iter()
        .filter_map(|case_| {
            let rank = if before {
                case_.before.surface_rank
            } else {
                case_.after.surface_rank
            }?;
            Some(1.0 / rank as f64)
        })
        .sum::<f64>();
    sum / cases.len() as f64
}

pub(crate) fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value
    }
}

fn comparison_fingerprint(comparison: &QualityComparison) -> Result<String, Error> {
    let mut value = serde_json::to_value(comparison)
        .map_err(|error| err(format!("serialize comparison fingerprint: {error}")))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| err("comparison fingerprint is not an object"))?;
    object.remove("determinism_fingerprint");
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| err(format!("serialize comparison fingerprint: {error}")))?;
    Ok(sha256_hex(&bytes))
}
