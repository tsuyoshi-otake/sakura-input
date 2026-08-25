//! Issue #93 fixture/snapshot scoring and before/after comparison.
//!
//! This module is intentionally separate from the fixed Stage 1
//! QualityScoreboard comparison in crate::comparison.  The Issue #93
//! fixture has its own role/assertion contract and accepts independent
//! candidate-snapshot JSON artifacts.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::comparison::{
    bool_change, normalize_zero, rank_change, BoolChange, ChangeDirection, RankChange,
    QUALITY_COMPARISON_SCHEMA_VERSION,
};
use crate::hash::sha256_hex;
use crate::quality::QUALITY_CANDIDATE_LIMIT;
use crate::types::{err, Error};

const MAX_SCOREBOARD_BYTES: usize = 64 * 1024 * 1024;

/// The Issue #93 corpus intentionally has a different contract from the
/// fixed 50-case Stage 1 fixture.  These types are kept in this module so the
/// new corpus can use the same deterministic comparison machinery without
/// weakening `quality::validate_fixture` or changing its schema.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingComparisonOptions {
    pub profile: String,
    pub candidate_limit: usize,
    pub recall_k: usize,
    pub learning: String,
    pub user_dictionary: String,
    pub reranker: String,
    pub input_repair: String,
    pub context: String,
    pub locale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub it_bias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub it_bias_per_mille: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_it_boost: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_right_id: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingReferenceComparison {
    pub baseline_label: String,
    pub after_label: String,
    pub ablation_label: String,
    pub source: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RankingAssertionKind {
    Top1,
    RecallAtK,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingReferenceObservation {
    pub baseline: String,
    pub after: String,
    pub ablation: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingProvenance {
    pub kind: String,
    pub references: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingComparisonCase {
    pub case_id: String,
    pub role: String,
    pub contrast_group: String,
    pub category: String,
    pub reading: String,
    pub assertion_kind: RankingAssertionKind,
    pub assertion_k: usize,
    pub expected_surface: Option<String>,
    pub semantic_scope: String,
    pub ambiguity: String,
    pub rationale: String,
    #[serde(default)]
    pub reference_observation: Option<RankingReferenceObservation>,
    #[serde(default)]
    pub provenance: Option<RankingProvenance>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingComparisonFixture {
    pub schema_version: u32,
    pub corpus_id: String,
    pub stage: String,
    pub candidate_limit: usize,
    pub options: RankingComparisonOptions,
    pub reference_comparison: RankingReferenceComparison,
    pub cases: Vec<RankingComparisonCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingArtifactIdentity {
    pub git_sha: String,
    /// The candidate snapshot is an evaluator artifact, not a shipped engine.
    /// The input adapter accepts the legacy `engine_sha256` spelling but emits
    /// the provenance-safe evaluator name.
    #[serde(alias = "engine_sha256")]
    pub evaluator_sha256: String,
    pub dictionary_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_diff_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_executable_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_build_feature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_metadata: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_evidence_metadata: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_support_metadata: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingRuntime {
    pub terminal: String,
    pub truncated: bool,
    #[serde(default)]
    pub elapsed_us: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingSnapshotObservation {
    pub case_id: String,
    #[serde(alias = "input")]
    pub reading: String,
    /// Preferred field for a compact independent candidate snapshot.
    #[serde(default, alias = "surfaces")]
    pub candidate_surfaces: Vec<String>,
    /// Adapter input accepted from a capture that retained candidate detail
    /// objects.  Only each object's `surface` is used for ranking comparison.
    #[serde(default)]
    pub candidates: Vec<serde_json::Value>,
    #[serde(default)]
    pub candidate_metadata_status: Option<String>,
    #[serde(default)]
    pub terminal: Option<String>,
    #[serde(default)]
    pub truncated: Option<bool>,
}

impl RankingSnapshotObservation {
    fn surfaces(&self) -> Result<Vec<String>, Error> {
        let detailed = self.detailed_surfaces()?;
        if !self.candidate_surfaces.is_empty() {
            if !detailed.is_empty() && detailed != self.candidate_surfaces {
                return Err(err(format!(
                    "ranking snapshot case {} candidate surface/order mismatch",
                    self.case_id
                )));
            }
            return Ok(self.candidate_surfaces.clone());
        }
        Ok(detailed)
    }

    fn detailed_surfaces(&self) -> Result<Vec<String>, Error> {
        let mut surfaces = Vec::with_capacity(self.candidates.len());
        for (index, candidate) in self.candidates.iter().enumerate() {
            if let Some(rank) = candidate.get("rank") {
                let expected_rank = u64::try_from(index + 1).unwrap_or(u64::MAX);
                if rank.as_u64() != Some(expected_rank) {
                    return Err(err(format!(
                        "ranking snapshot case {} candidate rank/order mismatch",
                        self.case_id
                    )));
                }
            }
            if let Some(surface) = candidate.as_str() {
                surfaces.push(surface.to_owned());
                continue;
            }
            let surface = candidate
                .get("surface")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| err("ranking snapshot candidate lacks a surface"))?;
            surfaces.push(surface.to_owned());
        }
        Ok(surfaces)
    }

    fn metadata_status(&self) -> Result<String, Error> {
        if let Some(status) = &self.candidate_metadata_status {
            if status != "observed" && status != "unsupported" {
                return Err(err(format!(
                    "ranking snapshot case {} has invalid candidate_metadata_status",
                    self.case_id
                )));
            }
            return Ok(status.clone());
        }
        if self.candidates.is_empty() {
            return Ok("unsupported".into());
        }
        let observed = self.candidates.iter().all(|candidate| {
            candidate.as_object().is_some_and(|object| {
                let unsupported = object
                    .get("unsupported_metadata")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|items| !items.is_empty());
                !unsupported && object.contains_key("origin") && object.contains_key("cost")
            })
        });
        Ok(if observed {
            "observed".into()
        } else {
            "unsupported".into()
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingObservationSnapshot {
    pub schema_version: u32,
    pub corpus_id: String,
    #[serde(alias = "fixture_sha256")]
    pub corpus_sha256: String,
    pub options_sha256: String,
    #[serde(default)]
    pub config_sha256: Option<String>,
    pub options: RankingComparisonOptions,
    pub candidate_limit: usize,
    pub artifact: RankingArtifactIdentity,
    #[serde(alias = "capture")]
    pub runtime: RankingRuntime,
    #[serde(alias = "cases")]
    pub observations: Vec<RankingSnapshotObservation>,
    #[serde(default, alias = "determinism_fingerprint")]
    pub report_determinism_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingObservationView {
    pub candidate_surfaces: Vec<String>,
    pub candidates: Vec<serde_json::Value>,
    pub candidate_metadata_status: String,
    pub surface_top1: bool,
    pub surface_rank: Option<usize>,
    pub surface_in_candidate_limit: bool,
    pub surface_in_recall_k: bool,
    pub terminal: Option<String>,
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RankingCaseComparison {
    pub case_id: String,
    pub reading: String,
    pub role: String,
    pub contrast_group: String,
    pub assertion_kind: RankingAssertionKind,
    pub assertion_k: usize,
    pub expected_surface: String,
    pub before: RankingObservationView,
    pub after: RankingObservationView,
    pub top1: BoolChange,
    pub recall: BoolChange,
    pub rank: RankChange,
    pub changed: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingComparisonSummary {
    pub total: usize,
    pub changed_cases: usize,
    pub top1_before: usize,
    pub top1_after: usize,
    pub top1_delta: i64,
    pub top1_improved: usize,
    pub top1_regressed: usize,
    pub top1_unchanged: usize,
    pub recall_before: usize,
    pub recall_after: usize,
    pub recall_delta: i64,
    pub recall_improved: usize,
    pub recall_regressed: usize,
    pub recall_unchanged: usize,
    pub mrr_at_candidate_limit_before: f64,
    pub mrr_at_candidate_limit_after: f64,
    pub mrr_at_candidate_limit_delta: f64,
    pub rank_improved: usize,
    pub rank_regressed: usize,
    pub rank_unchanged: usize,
    pub rank_entered_candidate_limit: usize,
    pub rank_exited_candidate_limit: usize,
    pub roles: Vec<RankingRoleSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingRoleSummary {
    pub role: String,
    pub total: usize,
    pub top1_before: usize,
    pub top1_after: usize,
    pub top1_delta: i64,
    pub recall_before: usize,
    pub recall_after: usize,
    pub recall_delta: i64,
    pub rank_improved: usize,
    pub rank_regressed: usize,
    pub rank_unchanged: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingScoreObservation {
    pub case_id: String,
    pub reading: String,
    pub role: String,
    pub contrast_group: String,
    pub assertion_kind: RankingAssertionKind,
    pub assertion_k: usize,
    pub expected_surface: String,
    pub observation: RankingObservationView,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingScoreSummary {
    pub total: usize,
    pub top1: usize,
    pub recall: usize,
    pub mrr_at_candidate_limit: f64,
    pub roles: Vec<RankingScoreRoleSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingScoreRoleSummary {
    pub role: String,
    pub total: usize,
    pub top1: usize,
    pub recall: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingScoreReport {
    pub schema_version: u32,
    pub report_type: String,
    pub corpus_id: String,
    pub capture_lane: String,
    pub candidate_limit: usize,
    pub identity: RankingComparisonIdentity,
    pub summary: RankingScoreSummary,
    pub observations: Vec<RankingScoreObservation>,
    pub determinism_fingerprint: String,
}

impl RankingScoreReport {
    pub fn human_summary(&self) -> String {
        format!(
            "ranking-score\tcorpus={}\tcases={}\ttop1={}\trecall={}\tfingerprint={}",
            self.corpus_id,
            self.summary.total,
            self.summary.top1,
            self.summary.recall,
            self.determinism_fingerprint,
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingComparisonIdentity {
    pub artifact: RankingArtifactIdentity,
    pub corpus_sha256: String,
    pub options_sha256: String,
    pub config_sha256: Option<String>,
    pub options: RankingComparisonOptions,
    pub runtime: RankingRuntime,
    /// None means the input snapshot did not publish its own report
    /// fingerprint; the corpus hash is never presented as a report hash.
    pub report_determinism_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RankingComparisonReport {
    pub schema_version: u32,
    pub report_type: String,
    pub corpus_id: String,
    pub capture_lane: String,
    pub candidate_limit: usize,
    pub before: RankingComparisonIdentity,
    pub after: RankingComparisonIdentity,
    pub summary: RankingComparisonSummary,
    pub cases: Vec<RankingCaseComparison>,
    pub determinism_fingerprint: String,
}

impl RankingComparisonReport {
    pub fn human_summary(&self) -> String {
        let summary = &self.summary;
        format!(
            "ranking-comparison\tcorpus={}\tcases={}\ttop1_before={}\ttop1_after={}\ttop1_delta={:+}\trecall_before={}\trecall_after={}\trecall_delta={:+}\trank_improved={}\trank_regressed={}\tfingerprint={}",
            self.corpus_id,
            summary.total,
            summary.top1_before,
            summary.top1_after,
            summary.top1_delta,
            summary.recall_before,
            summary.recall_after,
            summary.recall_delta,
            summary.rank_improved,
            summary.rank_regressed,
            self.determinism_fingerprint,
        )
    }
}

/// Load the versioned Issue #93 corpus and compute both raw and canonical
/// hashes.  Reports may use either hash representation, but the pair must
/// agree and the bytes must be from this committed fixture.
pub fn load_ranking_fixture(path: &Path) -> Result<(RankingComparisonFixture, Vec<String>), Error> {
    let bytes = fs::read(path).map_err(|error| err(format!("read {}: {error}", path.display())))?;
    if bytes.len() > MAX_SCOREBOARD_BYTES {
        return Err(err("ranking comparison fixture exceeds 64 MiB"));
    }
    let fixture: RankingComparisonFixture = serde_json::from_slice(&bytes)
        .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
    validate_ranking_fixture(&fixture)?;
    let canonical = serde_json::to_vec(&fixture)
        .map_err(|error| err(format!("serialize ranking fixture: {error}")))?;
    let hashes = vec![sha256_hex(&bytes), sha256_hex(&canonical)];
    Ok((fixture, hashes))
}

pub fn load_ranking_snapshot(path: &Path) -> Result<RankingObservationSnapshot, Error> {
    let bytes = fs::read(path).map_err(|error| err(format!("read {}: {error}", path.display())))?;
    if bytes.len() > MAX_SCOREBOARD_BYTES {
        return Err(err(format!(
            "ranking snapshot exceeds {MAX_SCOREBOARD_BYTES} bytes"
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
    if value.get("lane").and_then(serde_json::Value::as_str) != Some("engine_candidate_snapshot_v1")
    {
        return Err(err(
            "unsupported ranking snapshot input; expected engine_candidate_snapshot_v1 JSON",
        ));
    }
    let document: CandidateSnapshotDocument = serde_json::from_value(value).map_err(|error| {
        err(format!(
            "parse candidate snapshot {}: {error}",
            path.display()
        ))
    })?;
    adapt_candidate_snapshot(document)
}

pub fn compare_ranking_files(
    fixture_path: &Path,
    before_path: &Path,
    after_path: &Path,
) -> Result<RankingComparisonReport, Error> {
    let (fixture, fixture_hashes) = load_ranking_fixture(fixture_path)?;
    let before = load_ranking_input(before_path)?;
    let after = load_ranking_input(after_path)?;
    compare_ranking_snapshots(&fixture, &fixture_hashes, &before, &after)
}

/// Score one standalone candidate snapshot against the Issue #93 fixture.
/// This is the adapter boundary used when a runner can produce only one side
/// at a time; the resulting report is then consumable by
/// `compare_ranking_files` through the same case/assertion model.
pub fn score_ranking_file(
    fixture_path: &Path,
    snapshot_path: &Path,
) -> Result<RankingScoreReport, Error> {
    let (fixture, fixture_hashes) = load_ranking_fixture(fixture_path)?;
    let snapshot = load_ranking_input(snapshot_path)?;
    score_ranking_snapshot(&fixture, &fixture_hashes, &snapshot)
}

pub fn score_ranking_snapshot(
    fixture: &RankingComparisonFixture,
    fixture_hashes: &[String],
    snapshot: &RankingObservationSnapshot,
) -> Result<RankingScoreReport, Error> {
    validate_ranking_fixture(fixture)?;
    validate_ranking_snapshot(snapshot)?;
    validate_ranking_snapshot_against_fixture(snapshot, fixture, "snapshot")?;
    if !fixture_hashes
        .iter()
        .any(|hash| hash == &snapshot.corpus_sha256)
    {
        return Err(err("ranking snapshot corpus_sha256 does not match fixture"));
    }
    if snapshot.corpus_id != fixture.corpus_id
        || snapshot.candidate_limit != fixture.candidate_limit
        || !ranking_options_match_fixture(&snapshot.options, &fixture.options)
    {
        return Err(err("ranking snapshot identity does not match fixture"));
    }
    let observations_by_id = ranking_observation_map(snapshot)?;
    let mut observations = Vec::with_capacity(fixture.cases.len());
    for case_ in &fixture.cases {
        let expected_surface = case_.expected_surface.as_deref().ok_or_else(|| {
            err(format!(
                "ranking case {} lacks expected_surface",
                case_.case_id
            ))
        })?;
        let input = observations_by_id
            .get(case_.case_id.as_str())
            .ok_or_else(|| err(format!("snapshot is missing case_id {}", case_.case_id)))?;
        let surfaces = input.surfaces()?;
        let observation = ranking_view(
            input,
            &surfaces,
            expected_surface,
            case_.assertion_k,
            fixture.candidate_limit,
        )?;
        observations.push(RankingScoreObservation {
            case_id: case_.case_id.clone(),
            reading: case_.reading.clone(),
            role: case_.role.clone(),
            contrast_group: case_.contrast_group.clone(),
            assertion_kind: case_.assertion_kind,
            assertion_k: case_.assertion_k,
            expected_surface: expected_surface.to_owned(),
            observation,
        });
    }
    let summary = ranking_score_summary(&observations);
    let mut report = RankingScoreReport {
        schema_version: QUALITY_COMPARISON_SCHEMA_VERSION,
        report_type: "issue93_ranking_observation".into(),
        corpus_id: fixture.corpus_id.clone(),
        capture_lane: "issue93_ranking_comparison".into(),
        candidate_limit: fixture.candidate_limit,
        identity: ranking_identity(snapshot, &snapshot.options),
        summary,
        observations,
        determinism_fingerprint: String::new(),
    };
    report.determinism_fingerprint = ranking_score_fingerprint(&report)?;
    Ok(report)
}

/// Load one independent Issue #93 snapshot from structured JSON.
/// Candidate-snapshot JSON is adapted at this boundary.  The normalized
/// `RankingObservationSnapshot` type is an internal report model, not a
/// second file input format.
pub fn load_ranking_input(path: &Path) -> Result<RankingObservationSnapshot, Error> {
    load_ranking_snapshot(path)
}

#[derive(Debug, Clone, Deserialize)]
struct CandidateSnapshotDocument {
    schema_version: u32,
    lane: String,
    corpus_id: String,
    stage: String,
    artifact: CandidateSnapshotArtifact,
    #[serde(default)]
    evaluator: Option<CandidateSnapshotEvaluator>,
    #[serde(default)]
    engine: Option<CandidateSnapshotEngine>,
    options: CandidateSnapshotOptions,
    #[serde(alias = "observations")]
    cases: Vec<CandidateSnapshotCase>,
}

#[derive(Debug, Clone, Deserialize)]
struct CandidateSnapshotArtifact {
    git_sha: String,
    #[serde(alias = "engine_sha256")]
    evaluator_sha256: String,
    dictionary_sha256: String,
    #[serde(alias = "corpus_sha256", alias = "fixture_hash")]
    fixture_sha256: String,
    source_diff_sha256: String,
    variant: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CandidateSnapshotEvaluator {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    executable_sha256: Option<String>,
    #[serde(default)]
    build_feature: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CandidateSnapshotEngine {
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    origin_metadata: Option<String>,
    #[serde(default)]
    path_evidence_metadata: Option<String>,
    #[serde(default)]
    input_support_metadata: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CandidateSnapshotOptions {
    profile: String,
    candidate_limit: usize,
    method: String,
    it_bias: String,
    it_bias_per_mille: u16,
    max_it_boost: i32,
    initial_right_id: u16,
    input_repair: String,
    learning: String,
    user_dictionary: String,
    reranker: String,
    material: String,
    options_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CandidateSnapshotCase {
    case_id: String,
    reading: String,
    candidate_limit: usize,
    #[serde(alias = "surfaces")]
    candidate_surfaces: Vec<String>,
    #[serde(default)]
    candidates: Vec<serde_json::Value>,
    #[serde(alias = "terminal_status")]
    terminal: String,
    #[serde(alias = "is_truncated")]
    truncated: bool,
}

fn adapt_candidate_snapshot(
    document: CandidateSnapshotDocument,
) -> Result<RankingObservationSnapshot, Error> {
    if document.schema_version != 1
        || document.lane != "engine_candidate_snapshot_v1"
        || document.stage != "phase0"
    {
        return Err(err("unsupported candidate snapshot schema or lane"));
    }
    validate_candidate_snapshot_identity(&document.artifact, document.evaluator.as_ref())?;
    let evaluator = document.evaluator.as_ref();
    let engine = document.engine.as_ref();
    let corpus_sha256 = document.artifact.fixture_sha256.clone();
    let supplied_options_sha256 = document.options.options_sha256.clone();
    let artifact = RankingArtifactIdentity {
        git_sha: document.artifact.git_sha,
        evaluator_sha256: document.artifact.evaluator_sha256,
        dictionary_sha256: document.artifact.dictionary_sha256,
        fixture_sha256: Some(document.artifact.fixture_sha256.clone()),
        source_diff_sha256: Some(document.artifact.source_diff_sha256),
        variant: Some(document.artifact.variant),
        evaluator_name: evaluator.and_then(|value| value.name.clone()),
        evaluator_version: evaluator.and_then(|value| value.version.clone()),
        evaluator_executable_sha256: evaluator.and_then(|value| value.executable_sha256.clone()),
        evaluator_build_feature: evaluator.and_then(|value| value.build_feature.clone()),
        engine_package: engine.and_then(|value| value.package.clone()),
        engine_api: engine.and_then(|value| value.api.clone()),
        origin_metadata: engine.and_then(|value| value.origin_metadata.clone()),
        path_evidence_metadata: engine.and_then(|value| value.path_evidence_metadata.clone()),
        input_support_metadata: engine.and_then(|value| value.input_support_metadata.clone()),
    };
    let options = RankingComparisonOptions {
        profile: document.options.profile,
        candidate_limit: document.options.candidate_limit,
        recall_k: 5,
        learning: document.options.learning,
        user_dictionary: document.options.user_dictionary,
        reranker: document.options.reranker,
        input_repair: document.options.input_repair,
        context: "empty".into(),
        locale: "ja-JP".into(),
        method: Some(document.options.method),
        it_bias: Some(document.options.it_bias),
        it_bias_per_mille: Some(document.options.it_bias_per_mille),
        max_it_boost: Some(document.options.max_it_boost),
        initial_right_id: Some(document.options.initial_right_id),
        material: Some(document.options.material),
    };
    if document.cases.is_empty() {
        return Err(err("candidate snapshot has no cases"));
    }
    let mut observations = Vec::with_capacity(document.cases.len());
    let mut terminals = BTreeMap::<String, usize>::new();
    let mut truncated = false;
    for case_ in document.cases {
        if case_.candidate_limit != options.candidate_limit {
            return Err(err(format!(
                "candidate snapshot case {} candidate_limit differs from options",
                case_.case_id
            )));
        }
        validate_candidate_terminal(&case_.terminal, case_.truncated, &case_.case_id)?;
        *terminals.entry(case_.terminal.clone()).or_default() += 1;
        truncated |= case_.truncated;
        observations.push(RankingSnapshotObservation {
            case_id: case_.case_id,
            reading: case_.reading,
            candidate_surfaces: case_.candidate_surfaces,
            candidates: case_.candidates,
            candidate_metadata_status: None,
            terminal: Some(case_.terminal),
            truncated: Some(case_.truncated),
        });
    }
    let terminal = terminals
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(terminal, _)| terminal)
        .unwrap_or_else(|| "unknown".into());
    Ok(RankingObservationSnapshot {
        schema_version: 1,
        corpus_id: document.corpus_id,
        corpus_sha256,
        options_sha256: options_hash_from_snapshot(&options, &supplied_options_sha256)?,
        config_sha256: None,
        options,
        candidate_limit: document.options.candidate_limit,
        artifact,
        runtime: RankingRuntime {
            terminal,
            truncated,
            elapsed_us: None,
        },
        observations,
        report_determinism_fingerprint: None,
    })
}

fn options_hash_from_snapshot(
    options: &RankingComparisonOptions,
    supplied: &str,
) -> Result<String, Error> {
    if !is_sha256(supplied) {
        return Err(err("candidate snapshot options_sha256 is malformed"));
    }
    let material = options
        .material
        .as_deref()
        .ok_or_else(|| err("candidate snapshot options material is missing"))?;
    let expected = sha256_hex(material.as_bytes());
    if !supplied.eq_ignore_ascii_case(&expected) {
        return Err(err(
            "candidate snapshot options_sha256 does not match options material",
        ));
    }
    Ok(expected)
}

fn validate_candidate_snapshot_identity(
    artifact: &CandidateSnapshotArtifact,
    evaluator: Option<&CandidateSnapshotEvaluator>,
) -> Result<(), Error> {
    let source_diff_valid =
        artifact.source_diff_sha256 == "clean" || is_sha256(&artifact.source_diff_sha256);
    if artifact.git_sha.len() != 40
        || !artifact
            .git_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !is_sha256(&artifact.evaluator_sha256)
        || !is_sha256(&artifact.dictionary_sha256)
        || !is_sha256(&artifact.fixture_sha256)
        || !source_diff_valid
        || artifact.variant.is_empty()
    {
        return Err(err("candidate snapshot artifact identity is malformed"));
    }
    if evaluator
        .and_then(|identity| identity.executable_sha256.as_deref())
        .is_some_and(|hash| {
            !is_sha256(hash) || !hash.eq_ignore_ascii_case(&artifact.evaluator_sha256)
        })
    {
        return Err(err(
            "candidate snapshot evaluator hash differs from artifact identity",
        ));
    }
    Ok(())
}

fn validate_candidate_terminal(
    terminal: &str,
    truncated: bool,
    case_id: &str,
) -> Result<(), Error> {
    let expected_truncated = match terminal {
        "search_exhausted" => false,
        "candidate_limit_reached" | "state_budget_reached" | "lattice_budget_reached" => true,
        _ => {
            return Err(err(format!(
                "candidate snapshot case {case_id} has an unknown terminal"
            )))
        }
    };
    if truncated != expected_truncated {
        return Err(err(format!(
            "candidate snapshot case {case_id} terminal/truncation differs"
        )));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn compare_ranking_snapshots(
    fixture: &RankingComparisonFixture,
    fixture_hashes: &[String],
    before: &RankingObservationSnapshot,
    after: &RankingObservationSnapshot,
) -> Result<RankingComparisonReport, Error> {
    validate_ranking_fixture(fixture)?;
    validate_ranking_snapshot(before)?;
    validate_ranking_snapshot(after)?;
    validate_ranking_snapshot_against_fixture(before, fixture, "before")?;
    validate_ranking_snapshot_against_fixture(after, fixture, "after")?;
    if !fixture_hashes
        .iter()
        .any(|hash| hash == &before.corpus_sha256)
        || !fixture_hashes
            .iter()
            .any(|hash| hash == &after.corpus_sha256)
    {
        return Err(err("ranking snapshot corpus_sha256 does not match fixture"));
    }
    if before.corpus_sha256 != after.corpus_sha256
        || before.options_sha256 != after.options_sha256
        || before.config_sha256 != after.config_sha256
        || before.options != after.options
        || before.corpus_id != after.corpus_id
        || before.candidate_limit != after.candidate_limit
    {
        return Err(err(
            "ranking before/after fixture, options, or candidate-limit identity differs",
        ));
    }
    if before.corpus_id != fixture.corpus_id
        || before.candidate_limit != fixture.candidate_limit
        || !ranking_options_match_fixture(&before.options, &fixture.options)
    {
        return Err(err("ranking snapshot identity does not match fixture"));
    }

    let before_map = ranking_observation_map(before)?;
    let after_map = ranking_observation_map(after)?;
    let fixture_map = fixture
        .cases
        .iter()
        .map(|case_| (case_.case_id.as_str(), case_))
        .collect::<BTreeMap<_, _>>();
    if before_map.len() != fixture_map.len() || after_map.len() != fixture_map.len() {
        return Err(err("ranking snapshot case count does not match fixture"));
    }
    for case_id in fixture_map.keys() {
        if !before_map.contains_key(case_id) {
            return Err(err(format!(
                "before ranking snapshot is missing case_id {case_id}"
            )));
        }
        if !after_map.contains_key(case_id) {
            return Err(err(format!(
                "after ranking snapshot is missing case_id {case_id}"
            )));
        }
    }

    let mut cases = Vec::with_capacity(fixture.cases.len());
    for fixture_case in &fixture.cases {
        let case_id = fixture_case.case_id.as_str();
        let expected_surface = fixture_case
            .expected_surface
            .as_deref()
            .ok_or_else(|| err(format!("ranking case {case_id} lacks expected_surface")))?;
        let before_surfaces = before_map[case_id].surfaces()?;
        let after_surfaces = after_map[case_id].surfaces()?;
        let before_view = ranking_view(
            before_map[case_id],
            &before_surfaces,
            expected_surface,
            fixture_case.assertion_k,
            fixture.candidate_limit,
        )?;
        let after_view = ranking_view(
            after_map[case_id],
            &after_surfaces,
            expected_surface,
            fixture_case.assertion_k,
            fixture.candidate_limit,
        )?;
        let top1 = bool_change(before_view.surface_top1, after_view.surface_top1);
        let recall = bool_change(
            before_view.surface_in_recall_k,
            after_view.surface_in_recall_k,
        );
        let rank = rank_change(before_view.surface_rank, after_view.surface_rank);
        cases.push(RankingCaseComparison {
            case_id: case_id.to_owned(),
            reading: fixture_case.reading.clone(),
            role: fixture_case.role.clone(),
            contrast_group: fixture_case.contrast_group.clone(),
            assertion_kind: fixture_case.assertion_kind,
            assertion_k: fixture_case.assertion_k,
            expected_surface: expected_surface.to_owned(),
            before: before_view,
            after: after_view,
            changed: top1.direction != ChangeDirection::Unchanged
                || recall.direction != ChangeDirection::Unchanged
                || rank.direction != ChangeDirection::Unchanged,
            top1,
            recall,
            rank,
        });
    }

    let summary = ranking_summary(&cases, fixture.candidate_limit);
    let mut report = RankingComparisonReport {
        schema_version: QUALITY_COMPARISON_SCHEMA_VERSION,
        report_type: "issue93_ranking_comparison".into(),
        corpus_id: fixture.corpus_id.clone(),
        capture_lane: "issue93_ranking_comparison".into(),
        candidate_limit: fixture.candidate_limit,
        before: ranking_identity(before, &before.options),
        after: ranking_identity(after, &after.options),
        summary,
        cases,
        determinism_fingerprint: String::new(),
    };
    report.determinism_fingerprint = ranking_comparison_fingerprint(&report)?;
    Ok(report)
}

fn validate_ranking_fixture(fixture: &RankingComparisonFixture) -> Result<(), Error> {
    if fixture.schema_version != 1
        || fixture.corpus_id != "issue93-ranking-comparison-v1"
        || fixture.stage != "phase0"
        || fixture.candidate_limit != QUALITY_CANDIDATE_LIMIT
        || fixture.options.candidate_limit != fixture.candidate_limit
        || fixture.options.recall_k != 5
        || fixture.cases.len() != 22
    {
        return Err(err("unsupported Issue #93 ranking comparison fixture"));
    }
    if fixture.options.profile != fixture.corpus_id
        || fixture.options.learning != "disabled"
        || fixture.options.user_dictionary != "disabled"
        || fixture.options.reranker != "off"
        || fixture.options.input_repair != "disabled"
        || fixture.options.context != "empty"
        || fixture.options.locale != "ja-JP"
    {
        return Err(err(
            "Issue #93 ranking fixture options are outside the fixed contract",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for case_ in &fixture.cases {
        if case_.case_id.is_empty()
            || !ids.insert(case_.case_id.as_str())
            || case_.reading.is_empty()
            || case_.expected_surface.as_deref().is_none_or(str::is_empty)
            || case_.assertion_k == 0
            || case_.assertion_k > fixture.candidate_limit
        {
            return Err(err(format!(
                "invalid or duplicate Issue #93 ranking case {}",
                case_.case_id
            )));
        }
        match case_.assertion_kind {
            RankingAssertionKind::Top1
                if case_.assertion_k != 1
                    || case_.semantic_scope != "bounded_issue93_regression" =>
            {
                return Err(err(format!(
                    "top1 ranking case {} has an invalid k or semantic scope",
                    case_.case_id
                )));
            }
            RankingAssertionKind::RecallAtK
                if case_.assertion_k != fixture.options.recall_k
                    || case_.semantic_scope != "candidate_presence_only" =>
            {
                return Err(err(format!(
                    "recall ranking case {} has an invalid k or semantic scope",
                    case_.case_id
                )));
            }
            _ => {}
        }
        match case_.role.as_str() {
            "general_negative_control" if case_.assertion_kind != RankingAssertionKind::Top1 => {
                return Err(err(format!(
                    "general control {} must be a top1 assertion",
                    case_.case_id
                )));
            }
            "coverage_sentinel" if case_.assertion_kind != RankingAssertionKind::RecallAtK => {
                return Err(err(format!(
                    "coverage sentinel {} must be a recall assertion",
                    case_.case_id
                )));
            }
            "general_negative_control" | "coverage_sentinel" | "it_positive" => {}
            _ => {
                return Err(err(format!(
                    "ranking case {} has an unknown role",
                    case_.case_id
                )));
            }
        }
    }
    Ok(())
}

/// The committed fixture fixes the evaluator-independent option contract.
/// Standalone candidate snapshots may additionally report evaluator knobs
/// that were not present in the fixture's v1 options object; those optional
/// fields are retained in the report and are compared exactly between sides.
fn ranking_options_match_fixture(
    observed: &RankingComparisonOptions,
    fixture: &RankingComparisonOptions,
) -> bool {
    observed.profile == fixture.profile
        && observed.candidate_limit == fixture.candidate_limit
        && observed.recall_k == fixture.recall_k
        && observed.learning == fixture.learning
        && observed.user_dictionary == fixture.user_dictionary
        && observed.reranker == fixture.reranker
        && observed.input_repair == fixture.input_repair
        && observed.context == fixture.context
        && observed.locale == fixture.locale
        && (fixture.method.is_none() || observed.method == fixture.method)
        && (fixture.it_bias.is_none() || observed.it_bias == fixture.it_bias)
        && (fixture.it_bias_per_mille.is_none()
            || observed.it_bias_per_mille == fixture.it_bias_per_mille)
        && (fixture.max_it_boost.is_none() || observed.max_it_boost == fixture.max_it_boost)
        && (fixture.initial_right_id.is_none()
            || observed.initial_right_id == fixture.initial_right_id)
        && (fixture.material.is_none() || observed.material == fixture.material)
}

fn validate_ranking_snapshot(snapshot: &RankingObservationSnapshot) -> Result<(), Error> {
    if snapshot.schema_version != 1
        || snapshot.corpus_id.is_empty()
        || snapshot.corpus_sha256.len() != 64
        || !snapshot
            .corpus_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || snapshot.options_sha256.len() != 64
        || !snapshot
            .options_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || snapshot.candidate_limit != QUALITY_CANDIDATE_LIMIT
        || snapshot.options.candidate_limit != snapshot.candidate_limit
        || snapshot.runtime.terminal.is_empty()
        || snapshot.artifact.git_sha.is_empty()
        || snapshot.artifact.evaluator_sha256.is_empty()
        || snapshot.artifact.dictionary_sha256.is_empty()
    {
        return Err(err("malformed Issue #93 ranking snapshot identity"));
    }
    if snapshot
        .config_sha256
        .as_ref()
        .is_some_and(|hash| hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(err("malformed Issue #93 ranking snapshot config_sha256"));
    }
    if snapshot
        .artifact
        .fixture_sha256
        .as_ref()
        .is_some_and(|hash| hash != &snapshot.corpus_sha256)
    {
        return Err(err(
            "Issue #93 ranking snapshot artifact fixture_sha256 differs from corpus_sha256",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for observation in &snapshot.observations {
        if observation.case_id.is_empty()
            || observation.reading.is_empty()
            || !ids.insert(observation.case_id.as_str())
        {
            return Err(err(
                "Issue #93 ranking snapshot has duplicate or empty case_id",
            ));
        }
        let surfaces = observation.surfaces()?;
        if surfaces.is_empty()
            || surfaces.len() > snapshot.candidate_limit
            || surfaces.iter().any(String::is_empty)
        {
            return Err(err(format!(
                "Issue #93 ranking snapshot case {} has malformed candidates",
                observation.case_id
            )));
        }
    }
    Ok(())
}

fn validate_ranking_snapshot_against_fixture(
    snapshot: &RankingObservationSnapshot,
    fixture: &RankingComparisonFixture,
    side: &str,
) -> Result<(), Error> {
    if snapshot.observations.len() != fixture.cases.len() {
        return Err(err(format!(
            "{side} ranking snapshot case count does not match fixture"
        )));
    }
    for (index, (observation, fixture_case)) in
        snapshot.observations.iter().zip(&fixture.cases).enumerate()
    {
        if observation.case_id != fixture_case.case_id {
            return Err(err(format!(
                "{side} ranking snapshot case order differs at index {index}: {} vs {}",
                observation.case_id, fixture_case.case_id
            )));
        }
        if observation.reading != fixture_case.reading {
            return Err(err(format!(
                "{side} ranking snapshot reading differs for case_id {}",
                fixture_case.case_id
            )));
        }
    }
    Ok(())
}

fn ranking_observation_map(
    snapshot: &RankingObservationSnapshot,
) -> Result<BTreeMap<&str, &RankingSnapshotObservation>, Error> {
    let mut map = BTreeMap::new();
    for observation in &snapshot.observations {
        if map
            .insert(observation.case_id.as_str(), observation)
            .is_some()
        {
            return Err(err(format!(
                "Issue #93 ranking snapshot has duplicate case_id {}",
                observation.case_id
            )));
        }
    }
    Ok(map)
}

fn ranking_view(
    observation: &RankingSnapshotObservation,
    surfaces: &[String],
    expected_surface: &str,
    assertion_k: usize,
    candidate_limit: usize,
) -> Result<RankingObservationView, Error> {
    let surface_rank = surfaces
        .iter()
        .position(|surface| surface == expected_surface)
        .map(|index| index + 1);
    Ok(RankingObservationView {
        candidate_surfaces: surfaces.to_vec(),
        candidates: observation.candidates.clone(),
        candidate_metadata_status: observation.metadata_status()?,
        surface_top1: surface_rank == Some(1),
        surface_rank,
        surface_in_candidate_limit: surface_rank.is_some_and(|rank| rank <= candidate_limit),
        surface_in_recall_k: surface_rank.is_some_and(|rank| rank <= assertion_k),
        terminal: observation.terminal.clone(),
        truncated: observation.truncated,
    })
}

fn ranking_summary(
    cases: &[RankingCaseComparison],
    _candidate_limit: usize,
) -> RankingComparisonSummary {
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
        .filter(|case_| case_.before.surface_in_recall_k)
        .count();
    let recall_after = cases
        .iter()
        .filter(|case_| case_.after.surface_in_recall_k)
        .count();
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
        .filter(|case_| case_.recall.direction == ChangeDirection::Improved)
        .count();
    let recall_regressed = cases
        .iter()
        .filter(|case_| case_.recall.direction == ChangeDirection::Regressed)
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
    let mrr_before = ranking_mrr(cases, true);
    let mrr_after = ranking_mrr(cases, false);
    RankingComparisonSummary {
        total: cases.len(),
        changed_cases: cases.iter().filter(|case_| case_.changed).count(),
        top1_before,
        top1_after,
        top1_delta: top1_after as i64 - top1_before as i64,
        top1_improved,
        top1_regressed,
        top1_unchanged: cases.len() - top1_improved - top1_regressed,
        recall_before,
        recall_after,
        recall_delta: recall_after as i64 - recall_before as i64,
        recall_improved,
        recall_regressed,
        recall_unchanged: cases.len() - recall_improved - recall_regressed,
        mrr_at_candidate_limit_before: mrr_before,
        mrr_at_candidate_limit_after: mrr_after,
        mrr_at_candidate_limit_delta: normalize_zero(mrr_after - mrr_before),
        rank_improved,
        rank_regressed,
        rank_unchanged,
        rank_entered_candidate_limit: cases
            .iter()
            .filter(|case_| case_.rank.entered_top18)
            .count(),
        rank_exited_candidate_limit: cases.iter().filter(|case_| case_.rank.exited_top18).count(),
        roles: ranking_role_summaries(cases),
    }
}

fn ranking_role_summaries(cases: &[RankingCaseComparison]) -> Vec<RankingRoleSummary> {
    let mut grouped: BTreeMap<&str, Vec<&RankingCaseComparison>> = BTreeMap::new();
    for case_ in cases {
        grouped.entry(case_.role.as_str()).or_default().push(case_);
    }
    grouped
        .into_iter()
        .map(|(role, cases)| {
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
                .filter(|case_| case_.before.surface_in_recall_k)
                .count();
            let recall_after = cases
                .iter()
                .filter(|case_| case_.after.surface_in_recall_k)
                .count();
            RankingRoleSummary {
                role: role.to_owned(),
                total: cases.len(),
                top1_before,
                top1_after,
                top1_delta: top1_after as i64 - top1_before as i64,
                recall_before,
                recall_after,
                recall_delta: recall_after as i64 - recall_before as i64,
                rank_improved: cases
                    .iter()
                    .filter(|case_| case_.rank.direction == ChangeDirection::Improved)
                    .count(),
                rank_regressed: cases
                    .iter()
                    .filter(|case_| case_.rank.direction == ChangeDirection::Regressed)
                    .count(),
                rank_unchanged: cases
                    .iter()
                    .filter(|case_| case_.rank.direction == ChangeDirection::Unchanged)
                    .count(),
            }
        })
        .collect()
}

fn ranking_score_summary(observations: &[RankingScoreObservation]) -> RankingScoreSummary {
    let top1 = observations
        .iter()
        .filter(|observation| observation.observation.surface_top1)
        .count();
    let recall = observations
        .iter()
        .filter(|observation| observation.observation.surface_in_recall_k)
        .count();
    let mrr = if observations.is_empty() {
        0.0
    } else {
        observations
            .iter()
            .filter_map(|observation| observation.observation.surface_rank)
            .map(|rank| 1.0 / rank as f64)
            .sum::<f64>()
            / observations.len() as f64
    };
    let mut grouped: BTreeMap<&str, Vec<&RankingScoreObservation>> = BTreeMap::new();
    for observation in observations {
        grouped
            .entry(observation.role.as_str())
            .or_default()
            .push(observation);
    }
    let roles = grouped
        .into_iter()
        .map(|(role, observations)| RankingScoreRoleSummary {
            role: role.to_owned(),
            total: observations.len(),
            top1: observations
                .iter()
                .filter(|observation| observation.observation.surface_top1)
                .count(),
            recall: observations
                .iter()
                .filter(|observation| observation.observation.surface_in_recall_k)
                .count(),
        })
        .collect();
    RankingScoreSummary {
        total: observations.len(),
        top1,
        recall,
        mrr_at_candidate_limit: mrr,
        roles,
    }
}

fn ranking_score_fingerprint(report: &RankingScoreReport) -> Result<String, Error> {
    let mut value = serde_json::to_value(report)
        .map_err(|error| err(format!("serialize ranking score fingerprint: {error}")))?;
    value
        .as_object_mut()
        .ok_or_else(|| err("ranking score fingerprint is not an object"))?
        .remove("determinism_fingerprint");
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| err(format!("serialize ranking score fingerprint: {error}")))?;
    Ok(sha256_hex(&bytes))
}

fn ranking_mrr(cases: &[RankingCaseComparison], before: bool) -> f64 {
    if cases.is_empty() {
        return 0.0;
    }
    cases
        .iter()
        .filter_map(|case_| {
            let rank = if before {
                case_.before.surface_rank
            } else {
                case_.after.surface_rank
            }?;
            Some(1.0 / rank as f64)
        })
        .sum::<f64>()
        / cases.len() as f64
}

fn ranking_identity(
    snapshot: &RankingObservationSnapshot,
    options: &RankingComparisonOptions,
) -> RankingComparisonIdentity {
    RankingComparisonIdentity {
        artifact: snapshot.artifact.clone(),
        corpus_sha256: snapshot.corpus_sha256.clone(),
        options_sha256: snapshot.options_sha256.clone(),
        config_sha256: snapshot.config_sha256.clone(),
        options: options.clone(),
        runtime: snapshot.runtime.clone(),
        report_determinism_fingerprint: snapshot.report_determinism_fingerprint.clone(),
    }
}

fn ranking_comparison_fingerprint(report: &RankingComparisonReport) -> Result<String, Error> {
    let mut value = serde_json::to_value(report)
        .map_err(|error| err(format!("serialize ranking comparison fingerprint: {error}")))?;
    value
        .as_object_mut()
        .ok_or_else(|| err("ranking comparison fingerprint is not an object"))?
        .remove("determinism_fingerprint");
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| err(format!("serialize ranking comparison fingerprint: {error}")))?;
    Ok(sha256_hex(&bytes))
}
