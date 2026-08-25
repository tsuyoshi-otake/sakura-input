use sakura_ime_eval::comparison::{
    compare, ChangeDirection, ComparisonSide, QualityComparison, QUALITY_COMPARISON_SCHEMA_VERSION,
};
use sakura_ime_eval::hash::sha256_hex;
use sakura_ime_eval::quality::{
    QualityArtifactIdentity, QualityCandidate, QualityObservation, QualityOptionsIdentity,
    QualityRuntime, QualityScoreboard, QualitySummary, QualitySystemScore, QUALITY_CANDIDATE_LIMIT,
    QUALITY_SCHEMA_VERSION, WHOLE_READING_CAPTURE_LANE,
};
use sakura_ime_eval::ranking_comparison::{
    compare_ranking_files, compare_ranking_snapshots, load_ranking_fixture, load_ranking_input,
    score_ranking_file, score_ranking_snapshot, RankingArtifactIdentity, RankingComparisonFixture,
    RankingObservationSnapshot, RankingRuntime, RankingSnapshotObservation,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn options(tag: &str) -> QualityOptionsIdentity {
    QualityOptionsIdentity {
        profile: "quality-stage1-default".into(),
        candidate_limit: QUALITY_CANDIDATE_LIMIT,
        options_sha256: format!("options-{tag}"),
        config_sha256: format!("config-{tag}"),
        learning: "disabled".into(),
        user_dictionary: "disabled".into(),
        reranker: "off".into(),
    }
}

fn artifact(tag: &str) -> QualityArtifactIdentity {
    QualityArtifactIdentity {
        git_sha: format!("git-{tag}"),
        evaluator_sha256: format!("evaluator-{tag}"),
        dictionary_sha256: format!("dictionary-{tag}"),
    }
}

fn observation(case_id: &str, surfaces: &[&str], expected: &str) -> QualityObservation {
    let surface_rank = surfaces
        .iter()
        .position(|surface| *surface == expected)
        .map(|rank| rank + 1);
    QualityObservation {
        case_id: case_id.into(),
        category: "test".into(),
        reading: format!("reading-{case_id}"),
        expected_surface: expected.into(),
        expected_segments: vec![expected.into()],
        segment_assertion: sakura_ime_eval::quality::SegmentAssertion::Explicit,
        assertion_scope: sakura_ime_eval::quality::AssertionScope::CandidateObservation,
        candidate_limit: QUALITY_CANDIDATE_LIMIT,
        candidate_surfaces: surfaces.iter().map(|surface| (*surface).into()).collect(),
        candidates: surfaces
            .iter()
            .map(|surface| QualityCandidate {
                surface: (*surface).into(),
                segments: Some(vec![(*surface).into()]),
                origin: Some("test".into()),
                cost: Some(1),
            })
            .collect(),
        surface_top1: surface_rank == Some(1),
        surface_rank,
        surface_in_top18: surface_rank.is_some(),
        segment_status: "observed".into(),
        segment_exact: surface_rank.map(|_| true),
        candidate_metadata_status: "core_origin_and_cost_observed".into(),
        competitors: Vec::new(),
        elapsed_us: None,
        terminal: None,
        truncated: None,
    }
}

fn summary(observations: &[QualityObservation]) -> QualitySummary {
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
    QualitySummary {
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
        negative_control_top1: 0,
        negative_control_in_top18: 0,
    }
}

fn system(tag: &str, observations: Vec<QualityObservation>) -> QualitySystemScore {
    let summary = summary(&observations);
    QualitySystemScore {
        artifact: artifact(tag),
        capture: QualityRuntime {
            metadata_status: "core_whole_reading".into(),
            terminal: "completed".into(),
            truncated: Some(false),
            elapsed_us: Some(1),
        },
        summary,
        observations,
        negative_controls: Vec::new(),
    }
}

fn scoreboard_with_options(
    tag: &str,
    options_tag: &str,
    observations: Vec<QualityObservation>,
) -> QualityScoreboard {
    QualityScoreboard {
        schema_version: QUALITY_SCHEMA_VERSION,
        corpus_id: "comparison-test".into(),
        capture_lane: WHOLE_READING_CAPTURE_LANE.into(),
        candidate_limit: QUALITY_CANDIDATE_LIMIT,
        options: options(options_tag),
        baseline: system(&format!("{tag}-baseline"), observations.clone()),
        candidate: system(&format!("{tag}-candidate"), observations),
        score_elapsed_us: 1,
        determinism_fingerprint: format!("report-{tag}"),
    }
}

fn scoreboard(tag: &str, observations: Vec<QualityObservation>) -> QualityScoreboard {
    scoreboard_with_options(tag, "shared", observations)
}

fn sample_reports() -> (QualityScoreboard, QualityScoreboard) {
    let before = scoreboard(
        "before",
        vec![
            observation("case-3", &["other"], "expected"),
            observation("case-1", &["old", "expected"], "expected"),
            observation("case-2", &["expected"], "expected"),
        ],
    );
    let after = scoreboard(
        "after",
        vec![
            observation("case-2", &["other"], "expected"),
            observation("case-3", &["other", "expected"], "expected"),
            observation("case-1", &["expected"], "expected"),
        ],
    );
    (before, after)
}

#[test]
fn comparison_matches_stable_case_ids_and_separates_metric_changes() {
    let (before, after) = sample_reports();
    let comparison = compare(&before, &after, ComparisonSide::Candidate).expect("comparison");

    assert_eq!(comparison.schema_version, QUALITY_COMPARISON_SCHEMA_VERSION);
    assert_eq!(
        comparison
            .cases
            .iter()
            .map(|case_| case_.case_id.as_str())
            .collect::<Vec<_>>(),
        ["case-1", "case-2", "case-3"]
    );

    let case1 = &comparison.cases[0];
    assert_eq!(case1.top1.direction, ChangeDirection::Improved);
    assert_eq!(case1.recall_at18.direction, ChangeDirection::Unchanged);
    assert_eq!(case1.rank.direction, ChangeDirection::Improved);
    assert_eq!(case1.rank.delta, Some(-1));

    let case2 = &comparison.cases[1];
    assert_eq!(case2.top1.direction, ChangeDirection::Regressed);
    assert_eq!(case2.recall_at18.direction, ChangeDirection::Regressed);
    assert!(case2.rank.exited_top18);

    let case3 = &comparison.cases[2];
    assert_eq!(case3.top1.direction, ChangeDirection::Unchanged);
    assert_eq!(case3.recall_at18.direction, ChangeDirection::Improved);
    assert!(case3.rank.entered_top18);

    assert_eq!(comparison.summary.total, 3);
    assert_eq!(comparison.summary.changed_cases, 3);
    assert_eq!(comparison.summary.top1_before, 1);
    assert_eq!(comparison.summary.top1_after, 1);
    assert_eq!(comparison.summary.top1_delta, 0);
    assert_eq!(comparison.summary.top1_improved, 1);
    assert_eq!(comparison.summary.top1_regressed, 1);
    assert_eq!(comparison.summary.recall_at18_before, 2);
    assert_eq!(comparison.summary.recall_at18_after, 2);
    assert_eq!(comparison.summary.recall_at18_delta, 0);
    assert_eq!(comparison.summary.recall_at18_improved, 1);
    assert_eq!(comparison.summary.recall_at18_regressed, 1);
    assert_eq!(comparison.summary.rank_improved, 2);
    assert_eq!(comparison.summary.rank_regressed, 1);
    assert_eq!(comparison.summary.rank_entered_top18, 1);
    assert_eq!(comparison.summary.rank_exited_top18, 1);
}

#[test]
fn comparison_preserves_independent_artifact_and_option_identities() {
    let (before, after) = sample_reports();
    let comparison = compare(&before, &after, ComparisonSide::Candidate).expect("comparison");

    assert_eq!(comparison.before.artifact, before.candidate.artifact);
    assert_eq!(comparison.after.artifact, after.candidate.artifact);
    assert_eq!(comparison.before.options, before.options);
    assert_eq!(comparison.after.options, after.options);
    assert_eq!(
        comparison.before.report_determinism_fingerprint,
        before.determinism_fingerprint
    );
    assert_eq!(
        comparison.after.report_determinism_fingerprint,
        after.determinism_fingerprint
    );
}

#[test]
fn comparison_fingerprint_is_stable_when_observation_order_changes() {
    let (before, after) = sample_reports();
    let mut reordered_before = before.clone();
    reordered_before.candidate.observations.reverse();
    reordered_before.baseline.observations.reverse();
    let mut reordered_after = after.clone();
    reordered_after.candidate.observations.reverse();
    reordered_after.baseline.observations.reverse();

    let first = compare(&before, &after, ComparisonSide::Candidate).expect("comparison");
    let second = compare(
        &reordered_before,
        &reordered_after,
        ComparisonSide::Candidate,
    )
    .expect("reordered comparison");
    assert_eq!(first, second);
}

#[test]
fn duplicate_or_missing_case_ids_fail_closed() {
    let (mut before, after) = sample_reports();
    before
        .candidate
        .observations
        .push(before.candidate.observations[0].clone());
    before.candidate.summary.total += 1;
    before
        .baseline
        .observations
        .push(before.baseline.observations[0].clone());
    before.baseline.summary.total += 1;
    let error = compare(&before, &after, ComparisonSide::Candidate).expect_err("duplicate case");
    assert!(error.to_string().contains("duplicate case_id"));

    let (before, mut after) = sample_reports();
    after.candidate.observations.pop();
    after.candidate.summary.total -= 1;
    after.baseline.observations.pop();
    after.baseline.summary.total -= 1;
    let error = compare(&before, &after, ComparisonSide::Candidate).expect_err("missing case");
    assert!(error.to_string().contains("case count differs"));
}

#[test]
fn quality_comparison_rejects_different_option_identity() {
    let (before, mut after) = sample_reports();
    after.options.options_sha256 = "different-options".into();
    let error =
        compare(&before, &after, ComparisonSide::Candidate).expect_err("different option identity");
    assert!(error.to_string().contains("options or config identity"));
}

#[test]
fn baseline_side_can_be_selected_explicitly() {
    let (before, after) = sample_reports();
    let comparison: QualityComparison =
        compare(&before, &after, ComparisonSide::Baseline).expect("baseline comparison");
    assert_eq!(comparison.side, ComparisonSide::Baseline);
    assert_eq!(comparison.before.artifact, before.baseline.artifact);
    assert_eq!(comparison.after.artifact, after.baseline.artifact);
}

fn issue93_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../eval/corpus/behavioral/ranking-comparison-issue93/fixture.json")
}

fn issue93_baseline_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../eval/baselines/ranking-comparison-issue93")
}

fn issue93_snapshot(
    fixture: &RankingComparisonFixture,
    fixture_hash: &str,
    tag: &str,
    expected_first: bool,
) -> RankingObservationSnapshot {
    let options_sha256 = sha256_hex(&serde_json::to_vec(&fixture.options).expect("options JSON"));
    RankingObservationSnapshot {
        schema_version: 1,
        corpus_id: fixture.corpus_id.clone(),
        corpus_sha256: fixture_hash.into(),
        options_sha256,
        config_sha256: Some("c".repeat(64)),
        options: fixture.options.clone(),
        candidate_limit: fixture.candidate_limit,
        artifact: sakura_imeval_artifact(tag),
        runtime: RankingRuntime {
            terminal: "completed".into(),
            truncated: false,
            elapsed_us: Some(10),
        },
        observations: fixture
            .cases
            .iter()
            .map(|case_| {
                let expected = case_.expected_surface.clone().expect("fixture target");
                let candidate_surfaces = if expected_first {
                    vec![expected]
                } else {
                    vec!["別候補".into(), expected]
                };
                RankingSnapshotObservation {
                    case_id: case_.case_id.clone(),
                    reading: case_.reading.clone(),
                    candidate_surfaces,
                    candidates: Vec::new(),
                    candidate_metadata_status: Some("unsupported".into()),
                    terminal: Some("completed".into()),
                    truncated: Some(false),
                }
            })
            .collect(),
        report_determinism_fingerprint: Some(format!("snapshot-{tag}")),
    }
}

fn sakura_imeval_artifact(tag: &str) -> RankingArtifactIdentity {
    RankingArtifactIdentity {
        git_sha: format!("git-{tag}"),
        evaluator_sha256: format!("evaluator-{tag}"),
        dictionary_sha256: format!("dictionary-{tag}"),
        fixture_sha256: None,
        source_diff_sha256: None,
        variant: None,
        evaluator_name: None,
        evaluator_version: None,
        evaluator_executable_sha256: None,
        evaluator_build_feature: None,
        engine_package: None,
        engine_api: None,
        origin_metadata: None,
        path_evidence_metadata: None,
        input_support_metadata: None,
    }
}

#[test]
fn issue93_fixture_adapter_scores_roles_assertions_and_unsupported_metadata() {
    let (fixture, hashes) =
        load_ranking_fixture(&issue93_fixture_path()).expect("Issue #93 fixture");
    assert_eq!(fixture.cases.len(), 22);
    let snapshot = issue93_snapshot(&fixture, &hashes[1], "before", true);
    let report = score_ranking_snapshot(&fixture, &hashes, &snapshot).expect("ranking score");
    assert_eq!(report.report_type, "issue93_ranking_observation");
    assert_eq!(report.summary.total, fixture.cases.len());
    assert_eq!(report.summary.top1, fixture.cases.len());
    assert!(report
        .summary
        .roles
        .iter()
        .any(|role| role.role == "general_negative_control"));
    assert!(report
        .summary
        .roles
        .iter()
        .any(|role| role.role == "it_positive"));
    assert!(report
        .summary
        .roles
        .iter()
        .any(|role| role.role == "coverage_sentinel"));
    assert!(report
        .observations
        .iter()
        .all(|observation| observation.observation.candidate_metadata_status == "unsupported"));
    assert_eq!(report.identity.artifact, snapshot.artifact);
    let json = serde_json::to_value(&report).expect("score JSON");
    assert_eq!(json["identity"]["options_sha256"], snapshot.options_sha256);
    assert_eq!(json["identity"]["config_sha256"], "c".repeat(64));
}

fn candidate_snapshot_document(
    fixture: &RankingComparisonFixture,
    fixture_hash: &str,
    expected_first: bool,
) -> serde_json::Value {
    let cases = fixture
        .cases
        .iter()
        .map(|case_| {
            let expected = case_.expected_surface.clone().expect("fixture target");
            let candidate_surfaces = if expected_first {
                vec![expected]
            } else {
                vec!["別候補".to_owned(), expected]
            };
            let candidates = candidate_surfaces
                .iter()
                .enumerate()
                .map(|(index, surface)| {
                    json!({
                        "rank": index + 1,
                        "surface": surface,
                        "annotation": "",
                        "cost": index as i64,
                        "cost_kind": "final_path_cost",
                        "segments": [{
                            "text": surface,
                            "reading_start": 0,
                            "reading_end": case_.reading.chars().count(),
                            "text_start": 0,
                            "text_end": surface.chars().count(),
                            "left_id": 0,
                            "right_id": 0,
                            "flags": 0,
                            "word_count": 1,
                            "it_word_count": 0
                        }],
                        "origin": null,
                        "system_entry_index": null,
                        "origin_detail": null,
                        "path_evidence": null,
                        "base_cost": null,
                        "ranking_pass": null,
                        "unsupported_metadata": ["origin_detail", "path_evidence"]
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "case_id": case_.case_id,
                "reading": case_.reading,
                "candidate_limit": fixture.candidate_limit,
                "candidate_surfaces": candidate_surfaces,
                "candidates": candidates,
                "terminal": "search_exhausted",
                "truncated": false
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": 1,
        "lane": "engine_candidate_snapshot_v1",
        "corpus_id": fixture.corpus_id,
        "stage": "phase0",
        "artifact": {
            "git_sha": "1".repeat(40),
            "evaluator_sha256": "a".repeat(64),
            "dictionary_sha256": "b".repeat(64),
            "fixture_sha256": fixture_hash,
            "source_diff_sha256": "c".repeat(64),
            "variant": "e2e"
        },
        "evaluator": {
            "name": "candidate-snapshot-e2e",
            "version": "test",
            "executable_sha256": "a".repeat(64),
            "build_feature": "legacy"
        },
        "engine": {
            "package": "sakura-core",
            "api": "test-api",
            "origin_metadata": "unsupported",
            "path_evidence_metadata": "unsupported",
            "input_support_metadata": "unsupported"
        },
        "options": {
            "profile": fixture.options.profile,
            "candidate_limit": fixture.candidate_limit,
            "method": "multi-segment",
            "it_bias": "on",
            "it_bias_per_mille": 100,
            "max_it_boost": 800,
            "initial_right_id": 0,
            "input_repair": fixture.options.input_repair,
            "learning": fixture.options.learning,
            "user_dictionary": fixture.options.user_dictionary,
            "reranker": fixture.options.reranker,
            "material": "test",
            "options_sha256": sha256_hex(b"test")
        },
        "cases": cases
    })
}

#[test]
fn candidate_snapshot_json_scores_end_to_end_and_preserves_evidence() {
    let (fixture, hashes) =
        load_ranking_fixture(&issue93_fixture_path()).expect("Issue #93 fixture");
    let path = std::env::temp_dir().join(format!(
        "sakura-candidate-snapshot-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let document = candidate_snapshot_document(&fixture, &hashes[0], true);
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&document).expect("snapshot JSON"),
    )
    .expect("write snapshot");
    let report = score_ranking_file(&issue93_fixture_path(), &path).expect("score snapshot");
    std::fs::remove_file(&path).expect("remove snapshot");

    assert_eq!(report.summary.total, fixture.cases.len());
    assert_eq!(report.summary.top1, fixture.cases.len());
    assert_eq!(report.identity.artifact.evaluator_sha256, "a".repeat(64));
    assert_eq!(report.identity.artifact.variant.as_deref(), Some("e2e"));
    assert_eq!(
        report.identity.artifact.source_diff_sha256.as_deref(),
        Some("c".repeat(64).as_str())
    );
    assert_eq!(report.identity.options_sha256, sha256_hex(b"test"));
    assert_eq!(
        report.identity.options.method.as_deref(),
        Some("multi-segment")
    );
    let first = &report.observations[0].observation;
    assert_eq!(first.candidate_metadata_status, "unsupported");
    assert_eq!(first.terminal.as_deref(), Some("search_exhausted"));
    assert_eq!(first.truncated, Some(false));
    assert_eq!(first.candidates[0]["cost_kind"], "final_path_cost");
    assert!(first.candidates[0]["segments"].is_array());
}

#[test]
fn candidate_snapshot_rejects_fixture_hash_and_detail_order_mismatch() {
    let (fixture, hashes) =
        load_ranking_fixture(&issue93_fixture_path()).expect("Issue #93 fixture");
    let path = std::env::temp_dir().join(format!(
        "sakura-candidate-snapshot-invalid-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let mut document = candidate_snapshot_document(&fixture, &hashes[0], true);
    document["artifact"]["fixture_sha256"] = json!("0".repeat(64));
    std::fs::write(&path, serde_json::to_vec(&document).expect("snapshot JSON"))
        .expect("write snapshot");
    let error = score_ranking_file(&issue93_fixture_path(), &path).expect_err("fixture hash");
    assert!(error
        .to_string()
        .contains("corpus_sha256 does not match fixture"));

    document = candidate_snapshot_document(&fixture, &hashes[0], true);
    document["cases"][0]["candidate_surfaces"][0] = json!("別の表記");
    std::fs::write(&path, serde_json::to_vec(&document).expect("snapshot JSON"))
        .expect("write snapshot");
    let error = score_ranking_file(&issue93_fixture_path(), &path).expect_err("surface order");
    assert!(error
        .to_string()
        .contains("candidate surface/order mismatch"));

    document = candidate_snapshot_document(&fixture, &hashes[0], true);
    document["options"]["options_sha256"] = json!("0".repeat(64));
    std::fs::write(&path, serde_json::to_vec(&document).expect("snapshot JSON"))
        .expect("write snapshot");
    let error = score_ranking_file(&issue93_fixture_path(), &path).expect_err("options hash");
    assert!(error
        .to_string()
        .contains("does not match options material"));

    document = candidate_snapshot_document(&fixture, &hashes[0], true);
    document["artifact"]["git_sha"] = json!("short");
    std::fs::write(&path, serde_json::to_vec(&document).expect("snapshot JSON"))
        .expect("write snapshot");
    let error = score_ranking_file(&issue93_fixture_path(), &path).expect_err("artifact identity");
    assert!(error.to_string().contains("artifact identity is malformed"));

    document = candidate_snapshot_document(&fixture, &hashes[0], true);
    document["cases"][0]["terminal"] = json!("candidate_limit_reached");
    std::fs::write(&path, serde_json::to_vec(&document).expect("snapshot JSON"))
        .expect("write snapshot");
    let error = score_ranking_file(&issue93_fixture_path(), &path).expect_err("terminal contract");
    assert!(error.to_string().contains("terminal/truncation differs"));
    std::fs::remove_file(&path).expect("remove snapshot");
}

#[test]
fn candidate_snapshot_json_compares_two_independent_artifacts() {
    let (fixture, hashes) =
        load_ranking_fixture(&issue93_fixture_path()).expect("Issue #93 fixture");
    let stem = format!(
        "sakura-candidate-snapshot-compare-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let before_path = std::env::temp_dir().join(format!("{stem}-before.json"));
    let after_path = std::env::temp_dir().join(format!("{stem}-after.json"));
    let before = candidate_snapshot_document(&fixture, &hashes[0], true);
    let mut after = candidate_snapshot_document(&fixture, &hashes[0], false);
    after["artifact"]["evaluator_sha256"] = json!("f".repeat(64));
    after["evaluator"]["executable_sha256"] = json!("f".repeat(64));
    std::fs::write(
        &before_path,
        serde_json::to_vec(&before).expect("before JSON"),
    )
    .expect("write before");
    std::fs::write(&after_path, serde_json::to_vec(&after).expect("after JSON"))
        .expect("write after");
    let report = compare_ranking_files(&issue93_fixture_path(), &before_path, &after_path)
        .expect("compare snapshots");
    std::fs::remove_file(&before_path).expect("remove before");
    std::fs::remove_file(&after_path).expect("remove after");

    assert_eq!(report.summary.top1_before, fixture.cases.len());
    assert_eq!(report.summary.top1_after, 0);
    assert_eq!(report.before.artifact.evaluator_sha256, "a".repeat(64));
    assert_eq!(report.after.artifact.evaluator_sha256, "f".repeat(64));
    assert_eq!(
        report.cases[0].before.candidates[0]["surface"],
        report.cases[0].before.candidate_surfaces[0]
    );
}

#[test]
fn issue93_adapter_compares_top1_recall_rank_and_rejects_order_or_reading_mismatch() {
    let (fixture, hashes) =
        load_ranking_fixture(&issue93_fixture_path()).expect("Issue #93 fixture");
    let before = issue93_snapshot(&fixture, &hashes[1], "before", true);
    let after = issue93_snapshot(&fixture, &hashes[1], "after", false);
    let report =
        compare_ranking_snapshots(&fixture, &hashes, &before, &after).expect("ranking comparison");
    assert_eq!(report.summary.total, fixture.cases.len());
    assert_eq!(report.summary.top1_before, fixture.cases.len());
    assert_eq!(report.summary.top1_after, 0);
    assert_eq!(report.summary.top1_regressed, fixture.cases.len());
    assert!(report.summary.recall_after > 0);
    assert!(report.summary.roles.len() >= 3);
    assert!(report
        .cases
        .iter()
        .all(|case_| case_.before.candidate_metadata_status == "unsupported"));

    let mut reordered = after.clone();
    reordered.observations.swap(0, 1);
    let error = compare_ranking_snapshots(&fixture, &hashes, &before, &reordered)
        .expect_err("reordered snapshot");
    assert!(error.to_string().contains("case order differs"));

    let mut changed_reading = after;
    changed_reading.observations[0].reading.push('x');
    let error = compare_ranking_snapshots(&fixture, &hashes, &before, &changed_reading)
        .expect_err("changed reading");
    assert!(error.to_string().contains("reading differs"));
}

#[test]
fn checked_in_issue93_snapshots_match_manifest_and_report_fingerprints() {
    let fixture_path = issue93_fixture_path();
    let baseline_dir = issue93_baseline_dir();
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(baseline_dir.join("manifest.json")).expect("baseline manifest"),
    )
    .expect("baseline manifest JSON");
    let fixture_bytes = std::fs::read(&fixture_path).expect("Issue #93 fixture bytes");
    assert_eq!(
        manifest["fixture_sha256"],
        sha256_hex(&fixture_bytes),
        "manifest must pin the exact fixture bytes"
    );

    let (fixture, fixture_hashes) = load_ranking_fixture(&fixture_path).expect("Issue #93 fixture");
    let mut snapshots = BTreeMap::<String, RankingObservationSnapshot>::new();
    for entry in manifest["snapshots"]
        .as_array()
        .expect("snapshot manifest entries")
    {
        let variant = entry["variant"].as_str().expect("variant");
        let path = baseline_dir.join(entry["file"].as_str().expect("snapshot file"));
        let bytes = std::fs::read(&path).expect("checked-in candidate snapshot");
        assert_eq!(entry["bytes"].as_u64(), Some(bytes.len() as u64));
        assert_eq!(entry["sha256"], sha256_hex(&bytes));

        let snapshot = load_ranking_input(&path).expect("load checked-in candidate snapshot");
        let score = score_ranking_snapshot(&fixture, &fixture_hashes, &snapshot)
            .expect("score checked-in candidate snapshot");
        assert_eq!(
            entry["score"]["top1"].as_u64(),
            Some(score.summary.top1 as u64)
        );
        assert_eq!(
            entry["score"]["declared_assertion_passes"].as_u64(),
            Some(score.summary.recall as u64)
        );
        assert_eq!(
            entry["score"]["determinism_fingerprint"],
            score.determinism_fingerprint
        );
        snapshots.insert(variant.to_owned(), snapshot);
    }

    for comparison in manifest["comparisons"]
        .as_array()
        .expect("comparison manifest entries")
    {
        let before = snapshots
            .get(comparison["before"].as_str().expect("before variant"))
            .expect("before snapshot");
        let after = snapshots
            .get(comparison["after"].as_str().expect("after variant"))
            .expect("after snapshot");
        let report = compare_ranking_snapshots(&fixture, &fixture_hashes, before, after)
            .expect("compare checked-in snapshots");
        assert_eq!(
            comparison["determinism_fingerprint"],
            report.determinism_fingerprint
        );
        assert_eq!(
            comparison["changed_cases"].as_u64(),
            Some(report.summary.changed_cases as u64)
        );
        assert_eq!(
            comparison["rank_regressed"].as_u64(),
            Some(report.summary.rank_regressed as u64)
        );
    }
}
