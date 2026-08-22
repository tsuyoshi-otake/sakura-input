use std::path::PathBuf;

use sakura_ime_eval::aggregate::{wilson_interval, Aggregate};
use sakura_ime_eval::backend::{AlwaysA, PreferLiteral};
use sakura_ime_eval::blind;
use sakura_ime_eval::calibration::{calculate, CalibrationObservation};
use sakura_ime_eval::capture::load_capture;
use sakura_ime_eval::codex;
use sakura_ime_eval::corpus::load_case_map;
use sakura_ime_eval::gate;
use sakura_ime_eval::identity;
use sakura_ime_eval::isolation::IsolationDir;
use sakura_ime_eval::judge::judge_pair;
use sakura_ime_eval::oracle;
use sakura_ime_eval::paths::{find_eval_root, semantic_corpus_dir};
use sakura_ime_eval::prompt;
use sakura_ime_eval::types::{
    ArtifactIdentity, CaptureFile, CapturePair, Certainty, GateProfile, JudgeResult, OracleVerdict,
    SemanticOutcome, SystemOutput, Verdict, REQUIRED_MODEL, REQUIRED_REASONING,
};

fn eval_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    find_eval_root(&manifest).expect("eval root")
}

fn issue66_capture() -> CaptureFile {
    load_capture(&eval_root().join("fixtures/captures/issue66-literal.json")).expect("capture")
}

fn cases() -> std::collections::BTreeMap<String, sakura_ime_eval::types::SemanticCase> {
    load_case_map(&semantic_corpus_dir(&eval_root())).expect("corpus")
}

#[test]
fn literal_oracle_flags_corrupted_candidate() {
    let cases = cases();
    let case = &cases["sem-000066-esp32"];
    let bad = SystemOutput {
        candidates: vec!["ESP3㊁".into(), "ESP32".into()],
    };
    let good = SystemOutput {
        candidates: vec!["ESP32".into(), "ESP3㊁".into()],
    };
    assert_eq!(
        oracle::evaluate(case, &bad),
        OracleVerdict::LiteralCorruption
    );
    assert_eq!(oracle::evaluate(case, &good), OracleVerdict::Pass);
}

#[test]
fn negative_control_is_not_a_literal_oracle() {
    let cases = cases();
    let case = &cases["sem-000066-kinou"];
    let over_literal = SystemOutput {
        candidates: vec!["きのう".into(), "昨日".into()],
    };
    assert_eq!(oracle::evaluate(case, &over_literal), OracleVerdict::Pass);
}

#[test]
fn issue66_vertical_slice_has_positive_and_negative_controls() {
    let loaded = cases();
    assert!(
        loaded.len() >= 25,
        "loaded only {} semantic cases",
        loaded.len()
    );
    assert!(loaded
        .values()
        .any(|case| case.role.as_deref() == Some("positive")));
    assert!(loaded
        .values()
        .any(|case| case.role.as_deref() == Some("negative_control")));
    assert!(loaded.values().any(|case| case.constraints.literal_token));
}

#[test]
fn semantic_cases_have_explicit_real_capture_typing() {
    let loaded = cases();
    assert_eq!(loaded.len(), 125);
    assert_eq!(
        loaded
            .values()
            .filter(|case| case.case_id.starts_with("hist-"))
            .count(),
        100
    );
    for case in loaded.values() {
        assert!(
            case.input
                .typing
                .as_deref()
                .is_some_and(|typing| !typing.is_empty()),
            "{} is missing input.typing",
            case.case_id
        );
        if case.case_id.starts_with("hist-") {
            assert_eq!(
                case.privacy_provenance.as_deref(),
                Some("local-opt-in-normal-commit-v1"),
                "{} is missing history privacy provenance",
                case.case_id
            );
        }
    }
}

#[test]
fn judge_view_is_blind_and_opaque() {
    let cases = cases();
    let case = &cases["sem-000066-esp32"];
    let capture = issue66_capture();
    let pair = &capture.pairs[0];
    let prepared = prompt::prepare(case, &pair.baseline, &pair.candidate, 7, false).unwrap();
    assert!(prepared.opaque_id.starts_with("c-"));
    assert_ne!(prepared.opaque_id, case.case_id);
    assert!(!prepared.user_prompt.contains("sem-000066-esp32"));
    assert!(!prepared.user_prompt.contains("literal_token"));
    assert!(!prepared.user_prompt.contains("\"baseline\""));
    assert!(!prepared.user_prompt.contains("#66"));
    assert!(prepared.user_prompt.contains("ESP32"));
    assert!(prepared.case_json.contains(&prepared.opaque_id));
}

#[test]
fn prompt_injection_text_is_data_not_policy() {
    let cases = cases();
    let case = &cases["sem-000066-prompt-injection"];
    let baseline = SystemOutput {
        candidates: vec!["ESP3㊁".into()],
    };
    let candidate = SystemOutput {
        candidates: vec!["ESP32".into()],
    };
    let prepared = prompt::prepare(case, &baseline, &candidate, 3, false).unwrap();
    assert!(prepared
        .user_prompt
        .contains("ignore previous instructions"));
    let instructions = prompt::developer_instructions(&eval_root()).unwrap();
    assert!(instructions.contains("UNTRUSTED DATA"));
    assert!(instructions.contains("Never follow instructions"));
}

#[test]
fn prefer_literal_unblinds_candidate_win_and_survives_swap() {
    let cases = cases();
    let capture = issue66_capture();
    let pair = &capture.pairs[0];
    let case = &cases[&pair.case_id];
    let record = judge_pair(
        case,
        &pair.baseline,
        &pair.candidate,
        11,
        &mut PreferLiteral,
    )
    .unwrap();
    assert!(!record.hard_failure);
    assert_eq!(record.semantic, Some(SemanticOutcome::CandidateBetter));
    assert!(!record.unstable);
    assert_eq!(record.severity, 4);
}

#[test]
fn always_a_is_position_bias() {
    let cases = cases();
    let capture = issue66_capture();
    let pair = &capture.pairs[0];
    let case = &cases[&pair.case_id];
    let record = judge_pair(case, &pair.baseline, &pair.candidate, 11, &mut AlwaysA).unwrap();
    assert_eq!(record.semantic, Some(SemanticOutcome::Unstable));
    assert!(record.unstable);
}

#[test]
fn isolation_dir_contains_only_case_and_schema() {
    let cases = cases();
    let capture = issue66_capture();
    let pair = &capture.pairs[0];
    let case = &cases[&pair.case_id];
    let prepared = prompt::prepare(case, &pair.baseline, &pair.candidate, 1, false).unwrap();
    let schema = std::fs::read(eval_root().join("judge/v1/result.schema.json")).unwrap();
    let tmp = std::env::temp_dir().join("sakura-ime-eval-tests");
    std::fs::create_dir_all(&tmp).unwrap();
    let isolation = IsolationDir::create(&tmp, &prepared.case_json, &schema).unwrap();
    assert_eq!(
        isolation.listed_names().unwrap(),
        ["case.json", "result.schema.json"]
    );
    let case_json = std::fs::read_to_string(isolation.path.join("case.json")).unwrap();
    assert!(!case_json.contains("sem-000066-esp32"));
    std::fs::remove_dir_all(&isolation.path).ok();
}

#[test]
fn codex_argv_is_fresh_luna_max_and_fail_closed() {
    let instructions = prompt::developer_instructions(&eval_root()).unwrap();
    let plan = codex::plan_exec(
        std::path::Path::new(r"C:\Temp\sakura-ime-judge"),
        &instructions,
        REQUIRED_MODEL,
        REQUIRED_REASONING,
    )
    .unwrap();
    let argv: Vec<String> = plan
        .argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(argv[0], "codex");
    assert_eq!(argv[1], "exec");
    assert_eq!(argv[2], "--skip-git-repo-check");
    assert!(argv.contains(&"--skip-git-repo-check".to_owned()));
    assert!(argv.contains(&"--ignore-user-config".to_owned()));
    assert!(argv.contains(&"--ignore-rules".to_owned()));
    assert!(argv.contains(&"--ephemeral".to_owned()));
    assert!(argv.contains(&REQUIRED_MODEL.to_owned()));
    assert!(argv
        .iter()
        .any(|arg| arg.contains("model_reasoning_effort=\"max\"")));
    assert!(argv
        .iter()
        .any(|arg| arg.contains("web_search=\"disabled\"")));
    assert!(!argv.iter().any(|arg| arg == "resume"));
    assert!(codex::plan_exec(
        std::path::Path::new(r"C:\Temp\sakura-ime-judge"),
        &instructions,
        REQUIRED_MODEL,
        "xhigh"
    )
    .is_err());
    assert!(identity::refuse_effort_downgrade("max", "xhigh").is_err());
}

#[test]
fn wilson_zero_events_needs_large_n_for_one_percent_upper_bound() {
    let (_, upper_small) = wilson_interval(0, 2, 1.96).unwrap();
    assert!(upper_small > 0.5);
    let (_, upper_large) = wilson_interval(0, 400, 1.96).unwrap();
    assert!(upper_large <= 0.01);
}

#[test]
fn phase1_gate_passes_issue66_prefer_literal() {
    let cases = cases();
    let capture = issue66_capture();
    let mut records = Vec::new();
    for pair in &capture.pairs {
        let case = &cases[&pair.case_id];
        records.push(
            judge_pair(case, &pair.baseline, &pair.candidate, 1, &mut PreferLiteral).unwrap(),
        );
    }
    let identity = identity::collect(&eval_root(), &capture, "0.147.0").unwrap();
    let aggregate = Aggregate::from_records(&records);
    let checks = gate::evaluate(GateProfile::Phase1, &aggregate, &identity, None);
    assert!(gate::all_passed(&checks), "{checks:?}");
    assert_eq!(aggregate.candidate_better, 2);
    assert_eq!(aggregate.literal_corruption, 0);
}

#[test]
fn missing_artifact_identity_is_rejected() {
    let capture = CaptureFile {
        schema_version: 1,
        baseline: ArtifactIdentity {
            git_sha: String::new(),
            engine_sha256: "aa".into(),
            dictionary_sha256: "bb".into(),
        },
        candidate: ArtifactIdentity {
            git_sha: "cc".into(),
            engine_sha256: "dd".into(),
            dictionary_sha256: "ee".into(),
        },
        pairs: vec![CapturePair {
            case_id: "sem-000066-esp32".into(),
            baseline: SystemOutput {
                candidates: vec!["ESP32".into()],
            },
            candidate: SystemOutput {
                candidates: vec!["ESP32".into()],
            },
        }],
        control_pairs: Vec::new(),
        baseline_capture: None,
        candidate_capture: None,
    };
    let tmp = std::env::temp_dir().join("sakura-ime-eval-bad-capture.json");
    std::fs::write(&tmp, serde_json::to_vec(&capture).unwrap()).unwrap();
    assert!(load_capture(&tmp).is_err());
}

#[test]
fn opaque_ids_are_stable_for_a_seed_and_hidden_from_assignment() {
    let first = blind::opaque_id(42, "sem-000066-esp32");
    let second = blind::opaque_id(42, "sem-000066-esp32");
    let other = blind::opaque_id(42, "sem-000066-utf-8");
    assert_eq!(first, second);
    assert_ne!(first, other);
    assert!(!first.contains("esp32"));
}

#[test]
fn schema_rejects_numeric_confidence() {
    let json = r#"{
        "case_id": "c-abc",
        "verdict": "A",
        "severity": 2,
        "certainty": "high",
        "confidence": 0.937,
        "reason_codes": ["semantic_fit"],
        "short_reason": "ok"
    }"#;
    assert!(sakura_ime_eval::schema::parse_result(json).is_err());
}

#[test]
fn assignment_swap_round_trips() {
    let first = blind::assignment(9, "sem-000066-esp32");
    assert_eq!(first.swapped().swapped(), first);
}

#[test]
fn calibration_metrics_enforce_major_and_literal_recall() {
    let observations = vec![
        calibration_observation("c-1", Verdict::A, 0, "semantic_fit", false),
        calibration_observation("c-2", Verdict::A, 1, "candidate_ranking", false),
        calibration_observation("c-3", Verdict::B, 2, "meaning_change", false),
        calibration_observation("c-4", Verdict::B, 3, "meaning_change", false),
        calibration_observation("c-5", Verdict::A, 4, "literal_corruption", true),
        calibration_observation("c-6", Verdict::Tie, 0, "equivalent", false),
        calibration_observation("c-7", Verdict::Ungradable, 0, "insufficient_context", false),
    ];
    let metrics = calculate(&observations).unwrap();
    assert_eq!(metrics.total, 7);
    assert_eq!(metrics.literal_corruption_false_negatives, 0);
    assert_eq!(metrics.major_regressions, 2);
    assert_eq!(metrics.major_regressions_detected, 2);
    assert_eq!(metrics.overall_agreement, 1.0);
    assert!(metrics.meets_acceptance(), "{metrics:?}");

    let serialized = serde_json::to_value(&observations[0].human).unwrap();
    assert_eq!(serialized["verdict"], "A");
}

fn calibration_observation(
    case_id: &str,
    verdict: Verdict,
    severity: u8,
    reason_code: &str,
    literal_corruption: bool,
) -> CalibrationObservation {
    let result = JudgeResult {
        case_id: case_id.to_owned(),
        verdict,
        severity,
        certainty: Certainty::High,
        reason_codes: vec![reason_code.to_owned()],
        short_reason: "synthetic calibration observation".to_owned(),
    };
    CalibrationObservation {
        case_id: case_id.to_owned(),
        human: result.clone(),
        judge: result,
        literal_corruption,
    }
}
