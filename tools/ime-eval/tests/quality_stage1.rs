use std::path::PathBuf;

use sakura_ime_eval::capture::MAX_CANDIDATES_PER_SYSTEM;
use sakura_ime_eval::hash::sha256_hex;
use sakura_ime_eval::quality::{
    engine_cases, load_fixture, load_whole_reading_capture, score_whole_reading_capture,
    AssertionScope, QualityArtifactIdentity, QualityCandidate, QualityCoreCapturePair,
    QualityCoreControlPair, QualityCoreSystemOutput, QualityWholeReadingCapture, SegmentAssertion,
    ACTIVE_SEGMENT_CAPTURE_LANE, QUALITY_CANDIDATE_LIMIT, QUALITY_CAPTURE_CONFIG,
    QUALITY_CAPTURE_SCHEMA_VERSION, QUALITY_OPTIONS_MATERIAL, QUALITY_PROFILE, STAGE1_CASE_COUNT,
    WHOLE_READING_CAPTURE_LANE,
};
use sakura_ime_eval::types::CaptureRuntime;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../eval/corpus/behavioral/conversion-quality-stage1/fixture.json")
}

fn fixture() -> sakura_ime_eval::quality::QualityFixture {
    load_fixture(&fixture_path()).expect("Stage 1 fixture")
}

#[test]
fn stage1_fixture_is_versioned_and_has_fifty_boundary_separated_cases() {
    let fixture = fixture();
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.candidate_limit, 18);
    assert_eq!(fixture.cases.len(), STAGE1_CASE_COUNT);
    assert!(fixture.cases.iter().all(|case| {
        case.expected_segments.concat() == case.expected_surface
            && case.before_segments.concat() == case.before_surface
    }));
    assert!(fixture
        .cases
        .iter()
        .any(|case| case.segment_assertion == SegmentAssertion::Explicit));
    assert!(fixture
        .cases
        .iter()
        .any(|case| case.segment_assertion == SegmentAssertion::SurfaceOnly));
}

#[test]
fn stage1_options_identity_matches_capture_profile() {
    let raw = std::fs::read_to_string(fixture_path()).expect("fixture JSON");
    let fixture: sakura_ime_eval::quality::QualityFixture =
        serde_json::from_str(&raw).expect("fixture shape");
    assert_eq!(fixture.options.profile, QUALITY_PROFILE);
    assert_eq!(
        fixture.options.options_sha256,
        sha256_hex(QUALITY_OPTIONS_MATERIAL.as_bytes())
    );
    assert_eq!(
        fixture.options.config_sha256,
        sha256_hex(QUALITY_CAPTURE_CONFIG.as_bytes())
    );
}

#[test]
fn quality_limit_stays_within_production_protocol_without_narrowing_generic_capture_loading() {
    // Issue #95 split the wire ceiling into three reading-length tiers
    // (256/108/18; see `sakura_core::conversion::candidate_budget`) instead
    // of one flat number, so the quality harness's own fixed contract can no
    // longer equal `MAX_CANDIDATES` outright. Stage 1 exercises ordinary
    // multi-character readings, which stay on the long-reading tier the
    // pre-#95 ceiling already represented, so `QUALITY_CANDIDATE_LIMIT` keeps
    // its historical value. What still must hold is that the harness never
    // asks the wire protocol for more candidates than it can carry.
    // Both sides are constants, so this holds when the test crate is
    // compiled rather than when the test is run.
    const _: () = assert!(
        QUALITY_CANDIDATE_LIMIT <= sakura_proto::MAX_CANDIDATES,
        "the quality harness must not ask the wire protocol for more \
         candidates than one frame can carry"
    );
    assert_eq!(MAX_CANDIDATES_PER_SYSTEM, 64);
}

#[test]
fn quality_engine_cases_are_separate_and_include_controls() {
    let fixture = fixture();
    let cases = engine_cases(&fixture);
    assert_eq!(
        cases.len(),
        fixture.cases.len() + fixture.negative_controls.len()
    );
    assert!(cases[..fixture.cases.len()].iter().zip(&fixture.cases).all(
        |(engine_case, quality_case)| {
            engine_case.case_id == quality_case.case_id
                && engine_case.input.reading == quality_case.reading
                && engine_case.input.typing.as_deref() == Some(quality_case.reading.as_str())
        }
    ));
    for (engine_case, control) in cases[fixture.cases.len()..]
        .iter()
        .zip(&fixture.negative_controls)
    {
        assert_eq!(
            engine_case.case_id,
            format!("control-{}", control.control_id)
        );
        assert_eq!(engine_case.input.reading, control.reading);
    }
}

#[test]
fn stage1_fixture_is_outside_the_semantic_case_tree() {
    let semantic_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../eval/corpus/semantic");
    let entries = std::fs::read_dir(semantic_root).expect("semantic corpus");
    let names = entries
        .map(|entry| entry.expect("semantic entry").file_name())
        .filter_map(|name| name.into_string().ok())
        .collect::<Vec<_>>();
    assert!(!names
        .iter()
        .any(|name| name.contains("conversion-quality-stage1")));
}

fn synthetic_candidate(surface: &str, segments: &[String], cost: i64) -> QualityCandidate {
    QualityCandidate {
        surface: surface.to_owned(),
        segments: Some(segments.to_vec()),
        origin: Some("synthetic".into()),
        cost: Some(cost),
    }
}

fn synthetic_capture(
    fixture: &sakura_ime_eval::quality::QualityFixture,
) -> QualityWholeReadingCapture {
    let identity = |seed: char| QualityArtifactIdentity {
        git_sha: seed.to_string().repeat(40),
        evaluator_sha256: seed.to_string().repeat(64),
        dictionary_sha256: seed.to_string().repeat(64),
    };
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
                        synthetic_candidate(&case.before_surface, &case.before_segments, 2),
                        synthetic_candidate(&case.expected_surface, &case.expected_segments, 1),
                    ],
                },
                candidate: QualityCoreSystemOutput {
                    candidates: vec![
                        synthetic_candidate(&case.expected_surface, &case.expected_segments, 1),
                        synthetic_candidate(&case.before_surface, &case.before_segments, 2),
                    ],
                },
            })
            .collect(),
        control_pairs: fixture
            .negative_controls
            .iter()
            .map(|control| QualityCoreControlPair {
                control_id: control.control_id.clone(),
                reading: control.reading.clone(),
                baseline: QualityCoreSystemOutput {
                    candidates: vec![synthetic_candidate(
                        &control.surface,
                        std::slice::from_ref(&control.surface),
                        1,
                    )],
                },
                candidate: QualityCoreSystemOutput {
                    candidates: vec![synthetic_candidate("別候補", &["別候補".to_owned()], 1)],
                },
            })
            .collect(),
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
fn quality_report_schema_fields_and_fingerprint_are_stable() {
    let fixture = fixture();
    let capture = synthetic_capture(&fixture);
    let first = score_whole_reading_capture(&fixture, &capture).expect("quality report");
    let second = score_whole_reading_capture(&fixture, &capture).expect("quality report repeat");
    assert_eq!(
        first.determinism_fingerprint,
        second.determinism_fingerprint
    );
    assert_eq!(first.candidate.summary.total, STAGE1_CASE_COUNT);
    assert_eq!(
        first.candidate.negative_controls.len(),
        fixture.negative_controls.len()
    );
    assert_eq!(first.capture_lane, WHOLE_READING_CAPTURE_LANE);
    let explicit_count = fixture
        .cases
        .iter()
        .filter(|case| case.segment_assertion == SegmentAssertion::Explicit)
        .count();
    assert_eq!(first.candidate.summary.segment_observed, explicit_count);
    assert!(first.candidate.observations.iter().all(|observation| {
        (observation.segment_assertion == SegmentAssertion::Explicit
            && observation.segment_exact == Some(true)
            || observation.segment_assertion == SegmentAssertion::SurfaceOnly
                && observation.segment_exact.is_none())
            && observation.candidates.iter().all(|candidate| {
                candidate.segments.is_some()
                    && candidate.origin.is_some()
                    && candidate.cost.is_some()
            })
    }));
    let json = serde_json::to_value(&first).expect("quality report JSON");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["candidate_limit"], 18);
    assert!(json["candidate"]["negative_controls"].is_array());
    assert!(json["candidate"]["capture"]["elapsed_us"].is_number());
    assert_eq!(
        json["candidate"]["observations"][0]["assertion_scope"],
        "candidate_observation"
    );
    assert_eq!(json["capture_lane"], WHOLE_READING_CAPTURE_LANE);
    assert!(json["candidate"]["observations"][0]["candidates"][0]["segments"].is_array());
    assert!(json["candidate"]["artifact"]["evaluator_sha256"].is_string());
}

#[test]
fn future_assertion_scopes_survive_into_quality_report() {
    let mut fixture = fixture();
    fixture.cases[0].assertion_scope = AssertionScope::ContextRequired;
    fixture.cases[1].assertion_scope = AssertionScope::Hold;
    let report = score_whole_reading_capture(&fixture, &synthetic_capture(&fixture))
        .expect("quality report");
    assert_eq!(
        report.candidate.observations[0].assertion_scope,
        AssertionScope::ContextRequired
    );
    assert_eq!(
        report.candidate.observations[1].assertion_scope,
        AssertionScope::Hold
    );
}

#[test]
fn active_segment_capture_is_rejected_by_whole_reading_loader() {
    let path = std::env::temp_dir().join(format!(
        "sakura-ime-eval-active-segment-rejection-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    let value = serde_json::json!({
        "schema_version": QUALITY_CAPTURE_SCHEMA_VERSION,
        "lane": ACTIVE_SEGMENT_CAPTURE_LANE,
    });
    std::fs::write(&path, serde_json::to_vec(&value).expect("active lane JSON"))
        .expect("write active lane capture");
    let error = load_whole_reading_capture(&path).expect_err("active lane must be rejected");
    let _ = std::fs::remove_file(&path);
    assert!(error
        .to_string()
        .contains("quality-score accepts only whole_reading_core"));
}

#[test]
fn checked_in_baseline_is_the_generated_quality_scoreboard_shape() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../eval/baselines/quality-stage1-v1.json");
    let raw = std::fs::read_to_string(path).expect("generated quality baseline");
    let report: sakura_ime_eval::quality::QualityScoreboard =
        serde_json::from_str(&raw).expect("quality scoreboard schema shape");
    assert_eq!(report.schema_version, 1);
    assert_eq!(report.candidate_limit, QUALITY_CANDIDATE_LIMIT);
    assert_eq!(report.baseline.observations.len(), STAGE1_CASE_COUNT);
    assert_eq!(report.candidate.observations.len(), STAGE1_CASE_COUNT);
    assert_eq!(report.baseline.negative_controls.len(), 4);
    assert_eq!(report.candidate.negative_controls.len(), 4);
    let value: serde_json::Value = serde_json::from_str(&raw).expect("baseline JSON");
    assert!(value.get("status").is_none());
    assert!(value.get("report_type").is_none());
    assert!(value.get("case_observations").is_none());
}
