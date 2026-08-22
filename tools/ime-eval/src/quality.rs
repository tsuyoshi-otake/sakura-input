//! Deterministic conversion-quality observations for the Stage 1 challenge set.
//!
//! This module is deliberately separate from `SemanticCase` and the Judge
//! prompt path. Expected surfaces, segment contracts, ranks, and negative
//! controls stay in this deterministic lane and are never serialized into a
//! blinded semantic prompt.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::Instant;

use sakura_core::{
    ConversionCandidate, ConversionMethod, ConversionOptions, Converter, Dictionary, InputSupport,
};
use serde::{Deserialize, Serialize};

use crate::hash::{sha256_file, sha256_hex};
use crate::types::{
    err, ArtifactIdentity, CaptureControlPair, CapturePair, CaptureRuntime, Constraints, Context,
    Error, Input, SemanticCase,
};

pub const QUALITY_SCHEMA_VERSION: u32 = 1;
pub const QUALITY_CAPTURE_SCHEMA_VERSION: u32 = 2;
pub const QUALITY_CANDIDATE_LIMIT: usize = 18;
pub const STAGE1_CASE_COUNT: usize = 50;
pub const WHOLE_READING_CAPTURE_LANE: &str = "whole_reading_core";
pub const ACTIVE_SEGMENT_CAPTURE_LANE: &str = "active_segment_replay";
macro_rules! quality_profile_literal {
    () => {
        "quality-stage1-default"
    };
}
pub const QUALITY_PROFILE: &str = quality_profile_literal!();
pub const QUALITY_OPTIONS_MATERIAL: &str = concat!(
    "profile=",
    quality_profile_literal!(),
    "\ncandidate_limit=18\nlearning=disabled\nuser_dictionary=disabled\nreranker=off\ninput_repair=disabled\n"
);
pub const QUALITY_CAPTURE_CONFIG: &str = "[meta]\nformat-version = \"4\"\n\n[input]\ninput-method = \"kana\"\nprediction-enabled = \"false\"\nneural-reranker-scope = \"off\"\ninput-repair = \"disabled\"\ndeveloper-mode = \"false\"\n";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityFixture {
    pub schema_version: u32,
    pub corpus_id: String,
    pub stage: String,
    pub candidate_limit: usize,
    pub options: QualityOptionsIdentity,
    pub cases: Vec<QualityCase>,
    #[serde(default)]
    pub negative_controls: Vec<QualityNegativeControl>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityOptionsIdentity {
    pub profile: String,
    pub candidate_limit: usize,
    pub options_sha256: String,
    pub config_sha256: String,
    pub learning: String,
    pub user_dictionary: String,
    pub reranker: String,
}

/// Identity for the deterministic core evaluator.  A core quality report must
/// name the evaluator and dictionary actually used; an engine executable is
/// intentionally not part of this identity because the core lane never starts
/// or consults the real-engine candidate UI.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityArtifactIdentity {
    pub git_sha: String,
    pub evaluator_sha256: String,
    pub dictionary_sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityCase {
    pub case_id: String,
    pub category: String,
    pub reading: String,
    pub before_surface: String,
    pub before_segments: Vec<String>,
    pub expected_surface: String,
    pub expected_segments: Vec<String>,
    pub segment_assertion: SegmentAssertion,
    pub assertion_scope: AssertionScope,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SegmentAssertion {
    Explicit,
    SurfaceOnly,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssertionScope {
    CandidateObservation,
    ContextRequired,
    Hold,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityNegativeControl {
    pub control_id: String,
    pub reading: String,
    pub surface: String,
    pub policy: NegativeControlPolicy,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegativeControlPolicy {
    ReportIfTop1,
    RetainAsCompetitor,
    PreserveLiteral,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityCandidate {
    pub surface: String,
    /// Whole-reading core captures provide the exact path segment sequence.
    /// Active-segment replay captures never use this type and remain
    /// diagnostic-only.
    #[serde(default)]
    pub segments: Option<Vec<String>>,
    /// Candidate provenance is optional until a capture path provides a
    /// stable entry ordinal/source. `None` means unsupported, not system.
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub cost: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityCoreSystemOutput {
    pub candidates: Vec<QualityCandidate>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityCoreCapturePair {
    pub case_id: String,
    pub baseline: QualityCoreSystemOutput,
    pub candidate: QualityCoreSystemOutput,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityCoreControlPair {
    pub control_id: String,
    pub reading: String,
    pub baseline: QualityCoreSystemOutput,
    pub candidate: QualityCoreSystemOutput,
}

/// Versioned capture produced by the whole-reading core evaluator.  This is
/// the only capture shape accepted by `quality-score`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityWholeReadingCapture {
    pub schema_version: u32,
    pub lane: String,
    pub baseline: QualityArtifactIdentity,
    pub candidate: QualityArtifactIdentity,
    pub pairs: Vec<QualityCoreCapturePair>,
    #[serde(default)]
    pub control_pairs: Vec<QualityCoreControlPair>,
    pub baseline_capture: CaptureRuntime,
    pub candidate_capture: CaptureRuntime,
}

/// Diagnostic-only replay of the real engine's active-segment candidate UI.
/// It deliberately has a different lane and candidate shape, so it cannot be
/// mistaken for a whole-reading quality input.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityActiveSegmentCapture {
    pub schema_version: u32,
    pub lane: String,
    pub baseline: ArtifactIdentity,
    pub candidate: ArtifactIdentity,
    pub pairs: Vec<CapturePair>,
    #[serde(default)]
    pub control_pairs: Vec<CaptureControlPair>,
    pub baseline_capture: CaptureRuntime,
    pub candidate_capture: CaptureRuntime,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityRuntime {
    pub metadata_status: String,
    pub terminal: String,
    #[serde(default)]
    pub truncated: Option<bool>,
    #[serde(default)]
    pub elapsed_us: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct QualitySummary {
    pub total: usize,
    pub surface_top1: usize,
    pub surface_in_top18: usize,
    pub mrr_at18: f64,
    pub segment_observed: usize,
    pub segment_exact: usize,
    pub negative_control_top1: usize,
    pub negative_control_in_top18: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityNegativeObservation {
    pub control_id: String,
    pub surface: String,
    pub top1: bool,
    pub in_top18: bool,
    pub policy: NegativeControlPolicy,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QualityObservation {
    pub case_id: String,
    pub category: String,
    pub reading: String,
    pub expected_surface: String,
    pub expected_segments: Vec<String>,
    pub segment_assertion: SegmentAssertion,
    pub assertion_scope: AssertionScope,
    pub candidate_limit: usize,
    pub candidate_surfaces: Vec<String>,
    pub candidates: Vec<QualityCandidate>,
    pub surface_top1: bool,
    pub surface_rank: Option<usize>,
    pub surface_in_top18: bool,
    pub segment_status: String,
    pub segment_exact: Option<bool>,
    pub candidate_metadata_status: String,
    /// The supplied `before_surface` is a competitor observation, not a
    /// declared negative-control fixture. Explicit controls are reported at
    /// the system level below so the two meanings cannot be conflated.
    pub competitors: Vec<QualityNegativeObservation>,
    #[serde(default)]
    pub elapsed_us: Option<u64>,
    #[serde(default)]
    pub terminal: Option<String>,
    #[serde(default)]
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct QualitySystemScore {
    pub artifact: QualityArtifactIdentity,
    pub capture: QualityRuntime,
    pub summary: QualitySummary,
    pub observations: Vec<QualityObservation>,
    pub negative_controls: Vec<QualityNegativeObservation>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct QualityScoreboard {
    pub schema_version: u32,
    pub corpus_id: String,
    pub capture_lane: String,
    pub candidate_limit: usize,
    pub options: QualityOptionsIdentity,
    pub baseline: QualitySystemScore,
    pub candidate: QualitySystemScore,
    pub score_elapsed_us: u64,
    pub determinism_fingerprint: String,
}

pub fn load_fixture(path: &Path) -> Result<QualityFixture, Error> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| err(format!("read {}: {error}", path.display())))?;
    let fixture: QualityFixture = serde_json::from_str(&text)
        .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
    validate_fixture(&fixture)?;
    Ok(fixture)
}

pub fn validate_fixture(fixture: &QualityFixture) -> Result<(), Error> {
    if fixture.schema_version != QUALITY_SCHEMA_VERSION {
        return Err(err(format!(
            "unsupported quality fixture schema_version {}",
            fixture.schema_version
        )));
    }
    if fixture.corpus_id.is_empty() || fixture.corpus_id.len() > 128 {
        return Err(err(
            "quality fixture corpus_id must be non-empty and bounded",
        ));
    }
    if fixture.stage != "stage1" {
        return Err(err(format!(
            "quality fixture stage must be stage1, got {}",
            fixture.stage
        )));
    }
    if fixture.candidate_limit != QUALITY_CANDIDATE_LIMIT {
        return Err(err(format!(
            "quality fixture candidate_limit must be {QUALITY_CANDIDATE_LIMIT}"
        )));
    }
    validate_options(&fixture.options)?;
    if fixture.cases.len() != STAGE1_CASE_COUNT {
        return Err(err(format!(
            "Stage 1 fixture must contain {STAGE1_CASE_COUNT} cases, found {}",
            fixture.cases.len()
        )));
    }
    let mut ids = BTreeSet::new();
    for case in &fixture.cases {
        validate_case(case)?;
        if !ids.insert(case.case_id.as_str()) {
            return Err(err(format!("duplicate quality case_id {}", case.case_id)));
        }
    }
    let mut controls = BTreeSet::new();
    for control in &fixture.negative_controls {
        if control.control_id.is_empty() || !controls.insert(control.control_id.as_str()) {
            return Err(err(
                "quality negative-control IDs must be non-empty and unique",
            ));
        }
        validate_text(&control.reading, "negative-control reading")?;
        validate_text(&control.surface, "negative-control surface")?;
    }
    Ok(())
}

fn validate_options(options: &QualityOptionsIdentity) -> Result<(), Error> {
    if options.profile != QUALITY_PROFILE
        || options.candidate_limit != QUALITY_CANDIDATE_LIMIT
        || options.learning != "disabled"
        || options.user_dictionary != "disabled"
        || options.reranker != "off"
        || !is_hex(&options.options_sha256, 64)
        || !is_hex(&options.config_sha256, 64)
        || options.options_sha256 != sha256_hex(QUALITY_OPTIONS_MATERIAL.as_bytes())
        || options.config_sha256 != sha256_hex(QUALITY_CAPTURE_CONFIG.as_bytes())
    {
        return Err(err(
            "quality options identity is incomplete or outside Stage 1 bounds",
        ));
    }
    Ok(())
}

fn validate_case(case: &QualityCase) -> Result<(), Error> {
    if case.case_id.is_empty()
        || !case.case_id.starts_with("cq-")
        || case.case_id.contains('/')
        || case.category.is_empty()
    {
        return Err(err(format!(
            "invalid quality case identity {}",
            case.case_id
        )));
    }
    validate_text(&case.reading, "reading")?;
    validate_text(&case.before_surface, "before surface")?;
    validate_text(&case.expected_surface, "expected surface")?;
    if case.before_segments.is_empty()
        || case.expected_segments.is_empty()
        || case.before_segments.iter().any(String::is_empty)
        || case.expected_segments.iter().any(String::is_empty)
    {
        return Err(err(format!("{} has empty segment data", case.case_id)));
    }
    if case.before_segments.concat() != case.before_surface
        || case.expected_segments.concat() != case.expected_surface
    {
        return Err(err(format!(
            "{} surface and segment sequence disagree",
            case.case_id
        )));
    }
    if case.before_surface.contains('/') || case.expected_surface.contains('/') {
        return Err(err(format!(
            "{} stores slash-free surfaces; use segment arrays for boundaries",
            case.case_id
        )));
    }
    Ok(())
}

fn validate_text(value: &str, label: &str) -> Result<(), Error> {
    if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        return Err(err(format!(
            "quality {label} is empty, too long, or contains controls"
        )));
    }
    Ok(())
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn load_whole_reading_capture(path: &Path) -> Result<QualityWholeReadingCapture, Error> {
    let bytes = fs::read(path).map_err(|error| err(format!("read {}: {error}", path.display())))?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(err("quality capture exceeds 16 MiB"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
    let lane = value
        .get("lane")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("missing");
    if lane != WHOLE_READING_CAPTURE_LANE {
        return Err(err(format!(
            "quality-score accepts only {WHOLE_READING_CAPTURE_LANE} captures; got {lane}"
        )));
    }
    let capture: QualityWholeReadingCapture = serde_json::from_value(value)
        .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
    validate_whole_reading_capture(&capture)?;
    Ok(capture)
}

fn validate_whole_reading_capture(capture: &QualityWholeReadingCapture) -> Result<(), Error> {
    if capture.schema_version != QUALITY_CAPTURE_SCHEMA_VERSION {
        return Err(err(format!(
            "unsupported quality capture schema_version {}",
            capture.schema_version
        )));
    }
    if capture.lane != WHOLE_READING_CAPTURE_LANE {
        return Err(err(format!(
            "unsupported quality capture lane {}",
            capture.lane
        )));
    }
    for (name, artifact) in [
        ("baseline", &capture.baseline),
        ("candidate", &capture.candidate),
    ] {
        if !is_hex(&artifact.git_sha, 40)
            || !is_hex(&artifact.evaluator_sha256, 64)
            || !is_hex(&artifact.dictionary_sha256, 64)
        {
            return Err(err(format!("{name} core artifact identity is malformed")));
        }
    }
    validate_core_runtime("baseline", &capture.baseline_capture)?;
    validate_core_runtime("candidate", &capture.candidate_capture)?;
    for pair in &capture.pairs {
        if pair.case_id.is_empty() {
            return Err(err("quality core capture contains an empty case_id"));
        }
        validate_core_candidates(&pair.baseline.candidates)?;
        validate_core_candidates(&pair.candidate.candidates)?;
    }
    let mut control_ids = BTreeSet::new();
    for control in &capture.control_pairs {
        if control.control_id.is_empty() || !control_ids.insert(control.control_id.as_str()) {
            return Err(err(
                "quality core capture contains a duplicate or empty control_id",
            ));
        }
        validate_text(&control.reading, "core capture control reading")?;
        validate_core_candidates(&control.baseline.candidates)?;
        validate_core_candidates(&control.candidate.candidates)?;
    }
    Ok(())
}

fn validate_core_runtime(name: &str, runtime: &CaptureRuntime) -> Result<(), Error> {
    if runtime.terminal.is_empty() || runtime.terminal.len() > 64 {
        return Err(err(format!("{name} core capture terminal is malformed")));
    }
    Ok(())
}

fn validate_core_candidates(candidates: &[QualityCandidate]) -> Result<(), Error> {
    if candidates.is_empty() || candidates.len() > QUALITY_CANDIDATE_LIMIT {
        return Err(err(format!(
            "core capture candidate count must be 1..={QUALITY_CANDIDATE_LIMIT}"
        )));
    }
    for candidate in candidates {
        validate_text(&candidate.surface, "core capture candidate surface")?;
        let Some(segments) = &candidate.segments else {
            return Err(err(
                "whole-reading core capture must provide candidate segments",
            ));
        };
        if segments.is_empty() || segments.iter().any(String::is_empty) {
            return Err(err("core capture candidate has empty segment data"));
        }
        if segments.concat() != candidate.surface {
            return Err(err(
                "core capture candidate segments do not concatenate to its surface",
            ));
        }
    }
    Ok(())
}

pub fn score_whole_reading_capture_file(
    fixture_path: &Path,
    capture_path: &Path,
) -> Result<QualityScoreboard, Error> {
    let fixture = load_fixture(fixture_path)?;
    let capture = load_whole_reading_capture(capture_path)?;
    score_whole_reading_capture(&fixture, &capture)
}

pub fn score_whole_reading_capture(
    fixture: &QualityFixture,
    capture: &QualityWholeReadingCapture,
) -> Result<QualityScoreboard, Error> {
    validate_fixture(fixture)?;
    validate_whole_reading_capture(capture)?;
    let started = Instant::now();
    let baseline = score_core_system(
        fixture,
        &capture.baseline,
        &capture.pairs,
        &capture.control_pairs,
        &capture.baseline_capture,
        false,
    )?;
    let candidate = score_core_system(
        fixture,
        &capture.candidate,
        &capture.pairs,
        &capture.control_pairs,
        &capture.candidate_capture,
        true,
    )?;
    let mut scoreboard = QualityScoreboard {
        schema_version: QUALITY_SCHEMA_VERSION,
        corpus_id: fixture.corpus_id.clone(),
        capture_lane: WHOLE_READING_CAPTURE_LANE.to_owned(),
        candidate_limit: fixture.candidate_limit,
        options: fixture.options.clone(),
        baseline,
        candidate,
        score_elapsed_us: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
        determinism_fingerprint: String::new(),
    };
    scoreboard.determinism_fingerprint = fingerprint(&scoreboard)?;
    Ok(scoreboard)
}

/// Capture the fixture through the platform-free `sakura-core::Converter`.
/// The dictionary is parsed once and each reading is passed as one complete
/// query, so the resulting N-best list is a whole-reading list rather than an
/// active-segment candidate window from the engine protocol.
pub fn capture_whole_reading_fixture(
    fixture: &QualityFixture,
    baseline_dictionary: &Path,
    candidate_dictionary: &Path,
    baseline_git: &str,
    candidate_git: &str,
    evaluator: &Path,
) -> Result<QualityWholeReadingCapture, Error> {
    validate_fixture(fixture)?;
    if !is_hex(baseline_git, 40) || !is_hex(candidate_git, 40) {
        return Err(err("core capture git identity must be a 40-digit SHA-1"));
    }
    let evaluator_sha256 = sha256_file(evaluator)?;
    let baseline = QualityArtifactIdentity {
        git_sha: baseline_git.to_owned(),
        evaluator_sha256: evaluator_sha256.clone(),
        dictionary_sha256: sha256_file(baseline_dictionary)?,
    };
    let candidate = QualityArtifactIdentity {
        git_sha: candidate_git.to_owned(),
        evaluator_sha256,
        dictionary_sha256: sha256_file(candidate_dictionary)?,
    };
    let (baseline_outputs, baseline_capture) = capture_core_system(fixture, baseline_dictionary)?;
    let (candidate_outputs, candidate_capture) =
        capture_core_system(fixture, candidate_dictionary)?;
    let expected_count = fixture.cases.len() + fixture.negative_controls.len();
    if baseline_outputs.len() != expected_count || candidate_outputs.len() != expected_count {
        return Err(err("core capture returned an unexpected case count"));
    }
    let pairs = fixture
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| QualityCoreCapturePair {
            case_id: case.case_id.clone(),
            baseline: baseline_outputs[index].clone(),
            candidate: candidate_outputs[index].clone(),
        })
        .collect();
    let control_pairs = fixture
        .negative_controls
        .iter()
        .enumerate()
        .map(|(index, control)| {
            let capture_index = fixture.cases.len() + index;
            QualityCoreControlPair {
                control_id: control.control_id.clone(),
                reading: control.reading.clone(),
                baseline: baseline_outputs[capture_index].clone(),
                candidate: candidate_outputs[capture_index].clone(),
            }
        })
        .collect();
    Ok(QualityWholeReadingCapture {
        schema_version: QUALITY_CAPTURE_SCHEMA_VERSION,
        lane: WHOLE_READING_CAPTURE_LANE.to_owned(),
        baseline,
        candidate,
        pairs,
        control_pairs,
        baseline_capture,
        candidate_capture,
    })
}

fn capture_core_system(
    fixture: &QualityFixture,
    dictionary_path: &Path,
) -> Result<(Vec<QualityCoreSystemOutput>, CaptureRuntime), Error> {
    let started = Instant::now();
    let bytes = fs::read(dictionary_path).map_err(|error| {
        err(format!(
            "read core dictionary {}: {error}",
            dictionary_path.display()
        ))
    })?;
    let dictionary = Dictionary::parse(&bytes).map_err(|error| {
        err(format!(
            "parse core dictionary {}: {error}",
            dictionary_path.display()
        ))
    })?;
    let mut converter = Converter::new();
    let mut outputs = Vec::with_capacity(fixture.cases.len() + fixture.negative_controls.len());
    for case in &fixture.cases {
        outputs.push(convert_core_reading(
            &mut converter,
            &dictionary,
            &case.reading,
            &case.case_id,
        )?);
    }
    for control in &fixture.negative_controls {
        outputs.push(convert_core_reading(
            &mut converter,
            &dictionary,
            &control.reading,
            &control.control_id,
        )?);
    }
    Ok((
        outputs,
        CaptureRuntime {
            terminal: "completed".to_owned(),
            truncated: false,
            elapsed_us: Some(started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64),
        },
    ))
}

fn quality_conversion_options() -> ConversionOptions {
    ConversionOptions {
        max_candidates: QUALITY_CANDIDATE_LIMIT,
        method: ConversionMethod::MultiSegment,
        it_bias_per_mille: 100,
        max_it_boost: 800,
        initial_right_id: 0,
        input_support: InputSupport {
            enabled: false,
            commit_based: false,
            advanced: false,
            vowel_count: false,
            consonant_extra: false,
            n_count: false,
            dakuten_swap: false,
            tsu_sokuon: false,
            wa_wo: false,
            small_u: false,
            fuzzy_proper_nouns: false,
            english_to_katakana: false,
            period_after_digit: false,
            comma_after_digit: false,
            middle_dot_after_digit: false,
            long_vowel_after_alnum: false,
        },
        skip_input_repair: true,
        // `..Default` keeps this initializer compatible with both the
        // stable core API and worktrees that add private repair-budget fields;
        // `skip_input_repair=true` is the authoritative gate for this lane.
        ..ConversionOptions::default()
    }
}

fn convert_core_reading(
    converter: &mut Converter,
    dictionary: &Dictionary<'_>,
    reading: &str,
    case_id: &str,
) -> Result<QualityCoreSystemOutput, Error> {
    let result = converter
        .convert_detailed(dictionary, reading, quality_conversion_options())
        .map_err(|error| err(format!("core conversion for {case_id} failed: {error}")))?;
    let candidates = result
        .candidates()
        .iter()
        .map(core_candidate)
        .collect::<Result<Vec<_>, _>>()?;
    if candidates.is_empty() || candidates.len() > QUALITY_CANDIDATE_LIMIT {
        return Err(err(format!(
            "core conversion for {case_id} returned {} candidates outside 1..={QUALITY_CANDIDATE_LIMIT}",
            candidates.len()
        )));
    }
    Ok(QualityCoreSystemOutput { candidates })
}

fn core_candidate(candidate: &ConversionCandidate) -> Result<QualityCandidate, Error> {
    let text = candidate.text();
    let segments = candidate
        .segments()
        .iter()
        .map(|segment| {
            text.get(usize::from(segment.text_start)..usize::from(segment.text_end))
                .map(str::to_owned)
                .ok_or_else(|| err("core candidate segment range is not a UTF-8 boundary"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if segments.is_empty() || segments.concat() != text {
        return Err(err(
            "core candidate segment ranges do not cover its surface",
        ));
    }
    // The stable core API exposes an exact system-entry ordinal, but not a
    // general candidate-origin enum. Preserve only that evidence and keep
    // generated/composite candidates explicitly unsupported instead of
    // inferring provenance from their surface or cost.
    let origin = candidate
        .system_entry_index()
        .map(|index| format!("direct:system_entry:{index}"));
    Ok(QualityCandidate {
        surface: text.to_owned(),
        segments: Some(segments),
        origin,
        cost: Some(candidate.cost),
    })
}

/// Build the private direct-kana input cases used by `quality-capture`.
/// Expected surfaces and segment contracts are intentionally not copied into
/// these engine requests; they remain only in the deterministic fixture and
/// are therefore unavailable to the engine process and semantic Judge path.
pub fn engine_cases(fixture: &QualityFixture) -> Vec<SemanticCase> {
    fixture
        .cases
        .iter()
        .map(|case| SemanticCase {
            schema_version: 1,
            case_id: case.case_id.clone(),
            task: "conversion".to_owned(),
            family: Some(case.category.clone()),
            role: Some("quality_observation".to_owned()),
            context: Context {
                left: String::new(),
                right: String::new(),
            },
            input: Input {
                input_mode: Some("kana".to_owned()),
                reading: case.reading.clone(),
                typing: Some(case.reading.clone()),
            },
            constraints: Constraints::default(),
            privacy_provenance: None,
        })
        .chain(
            fixture
                .negative_controls
                .iter()
                .map(|control| SemanticCase {
                    schema_version: 1,
                    case_id: format!("control-{}", control.control_id),
                    task: "conversion".to_owned(),
                    family: Some("negative_control".to_owned()),
                    role: Some("quality_negative_control".to_owned()),
                    context: Context {
                        left: String::new(),
                        right: String::new(),
                    },
                    input: Input {
                        input_mode: Some("kana".to_owned()),
                        reading: control.reading.clone(),
                        typing: Some(control.reading.clone()),
                    },
                    constraints: Constraints::default(),
                    privacy_provenance: None,
                }),
        )
        .collect()
}

fn score_core_system(
    fixture: &QualityFixture,
    artifact: &QualityArtifactIdentity,
    pairs: &[QualityCoreCapturePair],
    control_pairs: &[QualityCoreControlPair],
    runtime: &CaptureRuntime,
    candidate_side: bool,
) -> Result<QualitySystemScore, Error> {
    let mut pair_map = BTreeMap::new();
    for pair in pairs {
        if pair_map.insert(pair.case_id.as_str(), pair).is_some() {
            return Err(err(format!("duplicate capture case_id {}", pair.case_id)));
        }
        if pair.baseline.candidates.len() > QUALITY_CANDIDATE_LIMIT
            || pair.candidate.candidates.len() > QUALITY_CANDIDATE_LIMIT
        {
            return Err(err(format!(
                "capture case {} exceeds candidate limit {QUALITY_CANDIDATE_LIMIT}",
                pair.case_id
            )));
        }
    }
    if pair_map.len() != fixture.cases.len()
        || fixture
            .cases
            .iter()
            .any(|case| !pair_map.contains_key(case.case_id.as_str()))
    {
        return Err(err(
            "capture cases do not exactly match the quality fixture",
        ));
    }
    let mut control_map = BTreeMap::new();
    for control in control_pairs {
        if control_map
            .insert(control.control_id.as_str(), control)
            .is_some()
        {
            return Err(err(format!(
                "duplicate capture control_id {}",
                control.control_id
            )));
        }
        if control.baseline.candidates.len() > QUALITY_CANDIDATE_LIMIT
            || control.candidate.candidates.len() > QUALITY_CANDIDATE_LIMIT
        {
            return Err(err(format!(
                "capture control {} exceeds candidate limit {QUALITY_CANDIDATE_LIMIT}",
                control.control_id
            )));
        }
    }
    if control_map.len() != fixture.negative_controls.len()
        || fixture
            .negative_controls
            .iter()
            .any(|control| !control_map.contains_key(control.control_id.as_str()))
    {
        return Err(err(
            "capture controls do not exactly match the quality fixture",
        ));
    }

    let capture_runtime = QualityRuntime {
        metadata_status: "core_whole_reading".to_owned(),
        terminal: runtime.terminal.clone(),
        truncated: Some(runtime.truncated),
        elapsed_us: runtime.elapsed_us,
    };

    let mut observations = Vec::with_capacity(fixture.cases.len());
    for case in &fixture.cases {
        let pair = pair_map[case.case_id.as_str()];
        let candidates = if candidate_side {
            &pair.candidate.candidates
        } else {
            &pair.baseline.candidates
        };
        let surfaces = candidates
            .iter()
            .map(|candidate| candidate.surface.clone())
            .collect::<Vec<_>>();
        let rank = surfaces
            .iter()
            .position(|surface| surface == &case.expected_surface)
            .map(|index| index + 1);
        let competitors = std::iter::once(QualityNegativeObservation {
            control_id: format!("{}:before", case.case_id),
            surface: case.before_surface.clone(),
            top1: surfaces.first() == Some(&case.before_surface),
            in_top18: surfaces
                .iter()
                .any(|surface| surface == &case.before_surface),
            policy: NegativeControlPolicy::RetainAsCompetitor,
        })
        .collect::<Vec<_>>();
        let segment_status = if candidates
            .iter()
            .all(|candidate| candidate.segments.is_some())
        {
            "observed"
        } else {
            "unsupported"
        };
        let segment_exact = (case.segment_assertion == SegmentAssertion::Explicit
            && segment_status == "observed")
            .then(|| {
                rank.and_then(|rank| candidates.get(rank - 1))
                    .and_then(|candidate| candidate.segments.as_ref())
                    .is_some_and(|segments| segments == &case.expected_segments)
            });
        let candidate_metadata_status = if candidates
            .iter()
            .all(|candidate| candidate.origin.is_some() && candidate.cost.is_some())
        {
            "core_origin_and_cost_observed"
        } else {
            "core_origin_or_cost_unsupported"
        };
        observations.push(QualityObservation {
            case_id: case.case_id.clone(),
            category: case.category.clone(),
            reading: case.reading.clone(),
            expected_surface: case.expected_surface.clone(),
            expected_segments: case.expected_segments.clone(),
            segment_assertion: case.segment_assertion,
            assertion_scope: case.assertion_scope,
            candidate_limit: QUALITY_CANDIDATE_LIMIT,
            candidate_surfaces: surfaces.clone(),
            candidates: candidates.clone(),
            surface_top1: rank == Some(1),
            surface_rank: rank,
            surface_in_top18: rank.is_some(),
            segment_status: segment_status.to_owned(),
            segment_exact,
            candidate_metadata_status: candidate_metadata_status.to_owned(),
            competitors,
            elapsed_us: None,
            terminal: None,
            truncated: None,
        });
    }

    let total = observations.len();
    let surface_top1 = observations
        .iter()
        .filter(|observation| observation.surface_top1)
        .count();
    let surface_in_top18 = observations
        .iter()
        .filter(|observation| observation.surface_in_top18)
        .count();
    let mrr_at18 = observations
        .iter()
        .filter_map(|observation| observation.surface_rank)
        .map(|rank| 1.0 / rank as f64)
        .sum::<f64>()
        / total as f64;
    let negative_controls = fixture
        .negative_controls
        .iter()
        .map(|control| {
            let pair = control_map[control.control_id.as_str()];
            let candidates = if candidate_side {
                &pair.candidate.candidates
            } else {
                &pair.baseline.candidates
            };
            let surfaces = candidates
                .iter()
                .map(|candidate| candidate.surface.as_str())
                .collect::<Vec<_>>();
            QualityNegativeObservation {
                control_id: control.control_id.clone(),
                surface: control.surface.clone(),
                top1: surfaces.first() == Some(&control.surface.as_str()),
                in_top18: surfaces.contains(&control.surface.as_str()),
                policy: control.policy,
            }
        })
        .collect::<Vec<_>>();
    let negative_control_top1 = negative_controls
        .iter()
        .filter(|control| control.top1)
        .count();
    let negative_control_in_top18 = negative_controls
        .iter()
        .filter(|control| control.in_top18)
        .count();
    Ok(QualitySystemScore {
        artifact: artifact.clone(),
        capture: capture_runtime,
        summary: QualitySummary {
            total,
            surface_top1,
            surface_in_top18,
            mrr_at18,
            segment_observed: observations
                .iter()
                .filter(|observation| observation.segment_exact.is_some())
                .count(),
            segment_exact: observations
                .iter()
                .filter(|observation| observation.segment_exact == Some(true))
                .count(),
            negative_control_top1,
            negative_control_in_top18,
        },
        observations,
        negative_controls,
    })
}

/// The fingerprint contains only reproducible quality content. Capture and
/// scoring timings plus terminal/truncated metadata remain in the report, but
/// are intentionally excluded because they describe the run rather than its
/// candidate result. Candidate metadata (including future segments, origin,
/// and cost when supported) remains part of the stable material.
#[derive(Serialize)]
struct StableObservation<'a> {
    case_id: &'a str,
    category: &'a str,
    reading: &'a str,
    expected_surface: &'a str,
    expected_segments: &'a [String],
    segment_assertion: SegmentAssertion,
    assertion_scope: AssertionScope,
    candidate_limit: usize,
    candidate_surfaces: &'a [String],
    candidates: &'a [QualityCandidate],
    surface_top1: bool,
    surface_rank: Option<usize>,
    surface_in_top18: bool,
    segment_status: &'a str,
    segment_exact: Option<bool>,
    candidate_metadata_status: &'a str,
    competitors: &'a [QualityNegativeObservation],
}

fn stable_observation(observation: &QualityObservation) -> StableObservation<'_> {
    StableObservation {
        case_id: &observation.case_id,
        category: &observation.category,
        reading: &observation.reading,
        expected_surface: &observation.expected_surface,
        expected_segments: &observation.expected_segments,
        segment_assertion: observation.segment_assertion,
        assertion_scope: observation.assertion_scope,
        candidate_limit: observation.candidate_limit,
        candidate_surfaces: &observation.candidate_surfaces,
        candidates: &observation.candidates,
        surface_top1: observation.surface_top1,
        surface_rank: observation.surface_rank,
        surface_in_top18: observation.surface_in_top18,
        segment_status: &observation.segment_status,
        segment_exact: observation.segment_exact,
        candidate_metadata_status: &observation.candidate_metadata_status,
        competitors: &observation.competitors,
    }
}

#[derive(Serialize)]
struct StableFingerprint<'a> {
    schema_version: u32,
    corpus_id: &'a str,
    capture_lane: &'a str,
    candidate_limit: usize,
    options: &'a QualityOptionsIdentity,
    baseline_artifact: &'a QualityArtifactIdentity,
    candidate_artifact: &'a QualityArtifactIdentity,
    baseline_observations: Vec<StableObservation<'a>>,
    candidate_observations: Vec<StableObservation<'a>>,
    baseline_negative_controls: &'a [QualityNegativeObservation],
    candidate_negative_controls: &'a [QualityNegativeObservation],
}

fn fingerprint(scoreboard: &QualityScoreboard) -> Result<String, Error> {
    let baseline_observations = scoreboard
        .baseline
        .observations
        .iter()
        .map(stable_observation)
        .collect();
    let candidate_observations = scoreboard
        .candidate
        .observations
        .iter()
        .map(stable_observation)
        .collect();
    let material = serde_json::to_vec(&StableFingerprint {
        schema_version: scoreboard.schema_version,
        corpus_id: &scoreboard.corpus_id,
        capture_lane: &scoreboard.capture_lane,
        candidate_limit: scoreboard.candidate_limit,
        options: &scoreboard.options,
        baseline_artifact: &scoreboard.baseline.artifact,
        candidate_artifact: &scoreboard.candidate.artifact,
        baseline_observations,
        candidate_observations,
        baseline_negative_controls: &scoreboard.baseline.negative_controls,
        candidate_negative_controls: &scoreboard.candidate.negative_controls,
    })
    .map_err(|error| err(format!("serialize quality fingerprint: {error}")))?;
    Ok(sha256_hex(&material))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> QualityOptionsIdentity {
        QualityOptionsIdentity {
            profile: QUALITY_PROFILE.into(),
            candidate_limit: QUALITY_CANDIDATE_LIMIT,
            options_sha256: sha256_hex(QUALITY_OPTIONS_MATERIAL.as_bytes()),
            config_sha256: sha256_hex(QUALITY_CAPTURE_CONFIG.as_bytes()),
            learning: "disabled".into(),
            user_dictionary: "disabled".into(),
            reranker: "off".into(),
        }
    }

    fn fixture() -> QualityFixture {
        QualityFixture {
            schema_version: QUALITY_SCHEMA_VERSION,
            corpus_id: "quality-stage1-test".into(),
            stage: "stage1".into(),
            candidate_limit: QUALITY_CANDIDATE_LIMIT,
            options: options(),
            cases: (0..STAGE1_CASE_COUNT)
                .map(|index| QualityCase {
                    case_id: format!("cq-{index:03}"),
                    category: "test".into(),
                    reading: format!("よみ{index}"),
                    before_surface: format!("前{index}"),
                    before_segments: vec![format!("前{index}")],
                    expected_surface: format!("正{index}"),
                    expected_segments: vec![format!("正{index}")],
                    segment_assertion: SegmentAssertion::SurfaceOnly,
                    assertion_scope: AssertionScope::CandidateObservation,
                    notes: None,
                })
                .collect(),
            negative_controls: Vec::new(),
        }
    }

    fn identity(seed: char) -> QualityArtifactIdentity {
        QualityArtifactIdentity {
            git_sha: seed.to_string().repeat(40),
            evaluator_sha256: seed.to_string().repeat(64),
            dictionary_sha256: seed.to_string().repeat(64),
        }
    }

    fn capture(fixture: &QualityFixture) -> QualityWholeReadingCapture {
        QualityWholeReadingCapture {
            schema_version: QUALITY_CAPTURE_SCHEMA_VERSION,
            lane: WHOLE_READING_CAPTURE_LANE.into(),
            baseline: identity('a'),
            candidate: identity('b'),
            pairs: fixture
                .cases
                .iter()
                .map(|case| QualityCoreCapturePair {
                    case_id: case.case_id.clone(),
                    baseline: QualityCoreSystemOutput {
                        candidates: vec![
                            QualityCandidate {
                                surface: case.before_surface.clone(),
                                segments: Some(case.before_segments.clone()),
                                origin: Some("synthetic".into()),
                                cost: Some(2),
                            },
                            QualityCandidate {
                                surface: case.expected_surface.clone(),
                                segments: Some(case.expected_segments.clone()),
                                origin: Some("synthetic".into()),
                                cost: Some(1),
                            },
                        ],
                    },
                    candidate: QualityCoreSystemOutput {
                        candidates: vec![
                            QualityCandidate {
                                surface: case.expected_surface.clone(),
                                segments: Some(case.expected_segments.clone()),
                                origin: Some("synthetic".into()),
                                cost: Some(1),
                            },
                            QualityCandidate {
                                surface: case.before_surface.clone(),
                                segments: Some(case.before_segments.clone()),
                                origin: Some("synthetic".into()),
                                cost: Some(2),
                            },
                        ],
                    },
                })
                .collect(),
            control_pairs: Vec::new(),
            baseline_capture: CaptureRuntime {
                terminal: "completed".into(),
                truncated: false,
                elapsed_us: Some(10),
            },
            candidate_capture: CaptureRuntime {
                terminal: "completed".into(),
                truncated: false,
                elapsed_us: Some(11),
            },
        }
    }

    #[test]
    fn fixture_validation_requires_stage1_shape() {
        let fixture = fixture();
        validate_fixture(&fixture).expect("valid fixture");
        let mut invalid = fixture.clone();
        invalid.cases.pop();
        assert!(validate_fixture(&invalid).is_err());
        let mut invalid_profile = fixture;
        invalid_profile.options.profile = "another-profile".into();
        assert!(validate_fixture(&invalid_profile).is_err());
    }

    #[test]
    fn score_keeps_surface_and_segment_contracts_separate() {
        let fixture = fixture();
        let report = score_whole_reading_capture(&fixture, &capture(&fixture)).expect("score");
        assert_eq!(report.baseline.summary.surface_top1, 0);
        assert_eq!(report.candidate.summary.surface_top1, STAGE1_CASE_COUNT);
        assert_eq!(report.baseline.summary.mrr_at18, 0.5);
        assert_eq!(report.candidate.summary.mrr_at18, 1.0);
        assert_eq!(report.candidate.summary.segment_observed, 0);
        assert!(report
            .candidate
            .observations
            .iter()
            .all(|observation| observation.surface_top1
                && observation.assertion_scope == AssertionScope::CandidateObservation
                && observation.segment_status == "observed"
                && observation.segment_exact.is_none()));
        assert!(report
            .candidate
            .observations
            .iter()
            .all(|observation| observation
                .candidates
                .iter()
                .all(|candidate| { candidate.origin.is_some() && candidate.cost.is_some() })));
    }

    #[test]
    fn unsupported_fields_are_serialized_as_explicit_nulls() {
        let fixture = fixture();
        let report = score_whole_reading_capture(&fixture, &capture(&fixture)).expect("score");
        let value = serde_json::to_value(report).expect("serialize report");
        let candidate = &value["candidate"]["observations"][0]["candidates"][0];
        assert!(candidate
            .get("segments")
            .is_some_and(serde_json::Value::is_array));
        assert!(candidate
            .get("origin")
            .is_some_and(serde_json::Value::is_string));
        assert!(candidate
            .get("cost")
            .is_some_and(serde_json::Value::is_number));
        let observation = &value["candidate"]["observations"][0];
        assert!(observation
            .get("elapsed_us")
            .is_some_and(serde_json::Value::is_null));
        assert!(observation
            .get("terminal")
            .is_some_and(serde_json::Value::is_null));
        assert!(observation
            .get("truncated")
            .is_some_and(serde_json::Value::is_null));
        let runtime = &value["candidate"]["capture"];
        assert!(runtime.get("truncated").is_some());
        assert!(runtime.get("elapsed_us").is_some());
    }

    #[test]
    fn identical_inputs_have_identical_fingerprint() {
        let fixture = fixture();
        let capture = capture(&fixture);
        let first = score_whole_reading_capture(&fixture, &capture).expect("first score");
        let second = score_whole_reading_capture(&fixture, &capture).expect("second score");
        assert_eq!(
            first.determinism_fingerprint,
            second.determinism_fingerprint
        );
    }

    #[test]
    fn fingerprint_excludes_volatile_runtime_metadata_but_tracks_content() {
        let fixture = fixture();
        let capture = capture(&fixture);
        let original = score_whole_reading_capture(&fixture, &capture).expect("score");
        let mut volatile = original.clone();
        volatile.score_elapsed_us = volatile.score_elapsed_us.saturating_add(1);
        for system in [&mut volatile.baseline, &mut volatile.candidate] {
            system.capture.elapsed_us = Some(999);
            for observation in &mut system.observations {
                observation.elapsed_us = Some(999);
                observation.terminal = Some("timed_out".into());
                observation.truncated = Some(true);
            }
        }
        assert_eq!(
            fingerprint(&original).expect("original fingerprint"),
            fingerprint(&volatile).expect("volatile fingerprint")
        );

        let mut changed = original.clone();
        let observation = &mut changed.candidate.observations[0];
        observation.surface_rank = Some(2);
        observation.segment_status = "observed".into();
        observation.candidates[0].segments = Some(vec!["正0".into()]);
        observation.candidates[0].origin = Some("dictionary".into());
        observation.candidates[0].cost = Some(1);
        assert_ne!(
            fingerprint(&original).expect("original fingerprint"),
            fingerprint(&changed).expect("changed fingerprint")
        );
    }

    #[test]
    fn assertion_scope_is_carried_without_restricting_future_values() {
        let mut fixture = fixture();
        fixture.cases[0].assertion_scope = AssertionScope::ContextRequired;
        fixture.cases[1].assertion_scope = AssertionScope::Hold;
        let report = score_whole_reading_capture(&fixture, &capture(&fixture)).expect("score");
        assert_eq!(
            report.candidate.observations[0].assertion_scope,
            AssertionScope::ContextRequired
        );
        assert_eq!(
            report.candidate.observations[1].assertion_scope,
            AssertionScope::Hold
        );
        let json = serde_json::to_value(report).expect("report JSON");
        assert_eq!(
            json["candidate"]["observations"][0]["assertion_scope"],
            "context_required"
        );
        assert_eq!(
            json["candidate"]["observations"][1]["assertion_scope"],
            "hold"
        );
    }

    #[test]
    fn candidate_limit_is_hard_and_negative_controls_are_reported() {
        let fixture = fixture();
        let mut capture = capture(&fixture);
        capture.pairs[0].candidate.candidates = (0..QUALITY_CANDIDATE_LIMIT + 1)
            .map(|index| QualityCandidate {
                surface: format!("候補{index}"),
                segments: Some(vec![format!("候補{index}")]),
                origin: Some("synthetic".into()),
                cost: Some(index as i64),
            })
            .collect();
        assert!(score_whole_reading_capture(&fixture, &capture).is_err());
    }

    #[test]
    fn declared_negative_controls_use_separate_captured_pairs() {
        let mut fixture = fixture();
        fixture.negative_controls = vec![QualityNegativeControl {
            control_id: "control-1".into(),
            reading: "よみ0".into(),
            surface: "抑制候補".into(),
            policy: NegativeControlPolicy::ReportIfTop1,
            note: None,
        }];
        let mut capture = capture(&fixture);
        capture.control_pairs = vec![QualityCoreControlPair {
            control_id: "control-1".into(),
            reading: "よみ0".into(),
            baseline: QualityCoreSystemOutput {
                candidates: vec![QualityCandidate {
                    surface: "抑制候補".into(),
                    segments: Some(vec!["抑制候補".into()]),
                    origin: Some("synthetic".into()),
                    cost: Some(1),
                }],
            },
            candidate: QualityCoreSystemOutput {
                candidates: vec![QualityCandidate {
                    surface: "別候補".into(),
                    segments: Some(vec!["別候補".into()]),
                    origin: Some("synthetic".into()),
                    cost: Some(1),
                }],
            },
        }];
        let report = score_whole_reading_capture(&fixture, &capture).expect("score controls");
        assert_eq!(report.baseline.summary.negative_control_top1, 1);
        assert_eq!(report.candidate.summary.negative_control_top1, 0);
        assert_eq!(report.baseline.negative_controls.len(), 1);
        assert_eq!(report.candidate.negative_controls.len(), 1);
        capture.control_pairs = Vec::new();
        assert!(score_whole_reading_capture(&fixture, &capture).is_err());
    }

    #[test]
    fn engine_cases_do_not_copy_expected_targets() {
        let fixture = fixture();
        let cases = engine_cases(&fixture);
        assert_eq!(cases.len(), STAGE1_CASE_COUNT);
        assert!(cases.iter().all(|case| case.task == "conversion"));
        assert!(cases
            .iter()
            .all(|case| case.input.input_mode.as_deref() == Some("kana")));
        assert!(cases
            .iter()
            .all(|case| case.input.typing == Some(case.input.reading.clone())));
    }
}
