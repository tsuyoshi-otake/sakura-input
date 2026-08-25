use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::aggregate::Aggregate;
use crate::backend::{AlwaysA, BackendKind, PreferLiteral};
use crate::capture::load_capture;
use crate::capture_engine::{capture_candidates, capture_kana_candidates};
use crate::corpus::load_case_map;
use crate::gate;
use crate::hash::sha256_file;
use crate::identity;
use crate::isolation::IsolationDir;
use crate::judge::judge_pair;
use crate::oracle;
use crate::paths::{find_eval_root, semantic_corpus_dir};
use crate::prompt;
use crate::report;
use crate::types::{
    default_eval_search_roots, err, ArtifactIdentity, CaptureControlPair, CaptureFile, CapturePair,
    Error, GateProfile, SemanticCase, SystemOutput,
};

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<u8, Error> {
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|_| err("argument is not valid Unicode"))
        })
        .collect::<Result<_, _>>()?;
    if args.is_empty() || args[0] == "help" || args[0] == "--help" {
        print_help();
        return Ok(0);
    }
    let command = args[0].as_str();
    let flags = parse_flags(&args[1..])?;
    let eval_root = resolve_eval_root(&flags)?;
    match command {
        "identity" => cmd_identity(&eval_root, &flags),
        "oracle" => cmd_oracle(&eval_root, &flags),
        "prompt" => cmd_prompt(&eval_root, &flags),
        "capture" => cmd_capture(&eval_root, &flags),
        "quality-capture" => cmd_quality_capture(&flags),
        "quality-core-capture" => cmd_quality_core_capture(&flags),
        "quality-score" => cmd_quality_score(&flags),
        "quality-compare" => cmd_quality_compare(&flags),
        "quality-rank-score" => cmd_quality_rank_score(&flags),
        "ranking-compare" => cmd_ranking_compare(&flags),
        "approve-history" => cmd_approve_history(&eval_root, &flags),
        "judge" => cmd_judge(&eval_root, &flags),
        "calibrate" => cmd_calibrate(&flags),
        "gate" => cmd_gate(&eval_root, &flags),
        "report" => cmd_report(&eval_root, &flags),
        other => Err(err(format!("unknown command {other}"))),
    }
}

fn print_help() {
    eprintln!(
        "\
ime-eval — Sakura Input quality measurement

Commands:
  identity   Hash Judge v1 artifacts and the semantic corpus
  oracle     Run deterministic oracles against a capture file
  prompt     Write a blinded isolation directory for one pair
  capture    Capture real-engine candidates into a capture file
  quality-capture  Diagnostic active-segment real-engine replay (not scoreable)
  quality-core-capture  Capture whole-reading candidates through sakura-core
  quality-score  Score only a whole-reading core quality capture (never a Judge input)
  quality-compare  Compare two independent quality-score reports by stable case ID
                   (pass --fixture for the Issue #93 corpus adapter)
  quality-rank-score  Score one Issue #93 candidate snapshot against its fixture
  ranking-compare  Compare two Issue #93 candidate snapshots by stable case ID
  approve-history  Publish an explicit opaque-ID history approval list
  judge      Blind A/B (+ swap) using prefer-literal, always-a, or codex
  calibrate  Calculate Judge-vs-human calibration metrics
  gate       Evaluate GATE-01..10 against a judge result directory
  report     Render the human-readable quality gate report
"
    );
}

struct Flags {
    values: std::collections::BTreeMap<String, String>,
}

impl Flags {
    fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    fn require(&self, name: &str) -> Result<&str, Error> {
        self.get(name)
            .ok_or_else(|| err(format!("missing --{name}")))
    }
}

fn parse_flags(args: &[String]) -> Result<Flags, Error> {
    let mut values = std::collections::BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let Some(name) = arg.strip_prefix("--") else {
            return Err(err(format!("unexpected argument {arg}")));
        };
        index += 1;
        if index >= args.len() {
            return Err(err(format!("missing value for --{name}")));
        }
        values.insert(name.to_owned(), args[index].clone());
        index += 1;
    }
    Ok(Flags { values })
}

fn resolve_eval_root(flags: &Flags) -> Result<PathBuf, Error> {
    if let Some(root) = flags.get("eval-root") {
        return Ok(PathBuf::from(root));
    }
    if let Some(repo) = flags.get("repo") {
        return find_eval_root(Path::new(repo));
    }
    for start in default_eval_search_roots() {
        if let Ok(root) = find_eval_root(&start) {
            return Ok(root);
        }
    }
    Err(err("could not locate eval/; pass --eval-root"))
}

fn cmd_identity(eval_root: &Path, flags: &Flags) -> Result<u8, Error> {
    let capture = load_optional_capture(flags)?;
    let version = flags.get("codex-version").unwrap_or("unspecified");
    let identity = identity::collect(eval_root, &capture, version)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&identity).map_err(|error| err(error.to_string()))?
    );
    Ok(0)
}

fn cmd_oracle(eval_root: &Path, flags: &Flags) -> Result<u8, Error> {
    let (cases, capture) = load_job(eval_root, flags)?;
    let mut failures = 0usize;
    for pair in &capture.pairs {
        let case = cases
            .get(&pair.case_id)
            .ok_or_else(|| err(format!("capture references unknown case {}", pair.case_id)))?;
        let candidate = oracle::evaluate(case, &pair.candidate);
        let baseline = oracle::evaluate(case, &pair.baseline);
        println!(
            "{} candidate={} baseline={}",
            pair.case_id,
            oracle_name(candidate),
            oracle_name(baseline)
        );
        if candidate.is_hard_failure() {
            failures += 1;
        }
    }
    Ok(if failures == 0 { 0 } else { 1 })
}

fn cmd_prompt(eval_root: &Path, flags: &Flags) -> Result<u8, Error> {
    let (cases, capture) = load_job(eval_root, flags)?;
    let seed = parse_seed(flags)?;
    let pair = capture
        .pairs
        .first()
        .ok_or_else(|| err("capture has no pairs"))?;
    let case = cases
        .get(&pair.case_id)
        .ok_or_else(|| err(format!("unknown case {}", pair.case_id)))?;
    let prepared = prompt::prepare(case, &pair.baseline, &pair.candidate, seed, false)?;
    let out = PathBuf::from(flags.require("out")?);
    fs::create_dir_all(&out).map_err(|error| err(format!("create {}: {error}", out.display())))?;
    let schema = fs::read(
        eval_root
            .join("judge")
            .join("v1")
            .join("result.schema.json"),
    )
    .map_err(|error| err(format!("read schema: {error}")))?;
    let isolation = IsolationDir::create(&out, &prepared.case_json, &schema)?;
    println!("{}", isolation.path.display());
    Ok(0)
}

fn cmd_capture(eval_root: &Path, flags: &Flags) -> Result<u8, Error> {
    let baseline_engine = PathBuf::from(flags.require("baseline-engine")?);
    let candidate_engine = PathBuf::from(flags.require("candidate-engine")?);
    let baseline_dictionary = PathBuf::from(flags.require("baseline-dictionary")?);
    let candidate_dictionary = PathBuf::from(flags.require("candidate-dictionary")?);
    let baseline_git = flags.require("baseline-git")?.to_owned();
    let candidate_git = flags.require("candidate-git")?.to_owned();
    let out = PathBuf::from(flags.require("out")?);
    validate_git_sha("baseline-git", &baseline_git)?;
    validate_git_sha("candidate-git", &candidate_git)?;
    let baseline_identity =
        artifact_identity(&baseline_git, &baseline_engine, &baseline_dictionary)?;
    let candidate_identity =
        artifact_identity(&candidate_git, &candidate_engine, &candidate_dictionary)?;

    let loaded = load_case_map(&semantic_corpus_dir(eval_root))?;
    let cases: Vec<SemanticCase> = if let Some(case_id) = flags.get("case-id") {
        vec![loaded
            .get(case_id)
            .ok_or_else(|| err(format!("unknown semantic case {case_id}")))?
            .clone()]
    } else {
        loaded.into_values().collect()
    };
    let timeout = parse_timeout(flags)?;
    let temp_root = flags
        .get("temp-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("sakura-ime-eval-capture"));

    let baseline_started = Instant::now();
    let baseline_candidates = capture_candidates(
        &baseline_engine,
        &baseline_dictionary,
        &cases,
        &temp_root,
        timeout,
    )?;
    let baseline_capture = crate::types::CaptureRuntime {
        terminal: "completed".to_owned(),
        truncated: false,
        elapsed_us: Some(
            baseline_started
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64,
        ),
    };
    let candidate_started = Instant::now();
    let candidate_candidates = capture_candidates(
        &candidate_engine,
        &candidate_dictionary,
        &cases,
        &temp_root,
        timeout,
    )?;
    let candidate_capture = crate::types::CaptureRuntime {
        terminal: "completed".to_owned(),
        truncated: false,
        elapsed_us: Some(
            candidate_started
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64,
        ),
    };
    if baseline_candidates.len() != cases.len() || candidate_candidates.len() != cases.len() {
        return Err(err("candidate capture returned an unexpected case count"));
    }

    let capture = CaptureFile {
        schema_version: 1,
        baseline: baseline_identity,
        candidate: candidate_identity,
        pairs: cases
            .into_iter()
            .zip(baseline_candidates.into_iter().zip(candidate_candidates))
            .map(|(case, (baseline, candidate))| crate::types::CapturePair {
                case_id: case.case_id,
                baseline,
                candidate,
            })
            .collect(),
        control_pairs: Vec::new(),
        baseline_capture: Some(baseline_capture),
        candidate_capture: Some(candidate_capture),
    };
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
    }
    let bytes = serde_json::to_vec_pretty(&capture)
        .map_err(|error| err(format!("serialize capture: {error}")))?;
    fs::write(&out, bytes).map_err(|error| err(format!("write {}: {error}", out.display())))?;
    println!("{}", out.display());
    Ok(0)
}

fn cmd_quality_capture(flags: &Flags) -> Result<u8, Error> {
    let fixture_path = Path::new(flags.require("fixture")?);
    let fixture = crate::quality::load_fixture(fixture_path)?;
    let baseline_engine = PathBuf::from(flags.require("baseline-engine")?);
    let candidate_engine = PathBuf::from(flags.require("candidate-engine")?);
    let baseline_dictionary = PathBuf::from(flags.require("baseline-dictionary")?);
    let candidate_dictionary = PathBuf::from(flags.require("candidate-dictionary")?);
    let baseline_git = flags.require("baseline-git")?.to_owned();
    let candidate_git = flags.require("candidate-git")?.to_owned();
    let out = PathBuf::from(flags.require("out")?);
    validate_git_sha("baseline-git", &baseline_git)?;
    validate_git_sha("candidate-git", &candidate_git)?;
    let baseline_identity =
        artifact_identity(&baseline_git, &baseline_engine, &baseline_dictionary)?;
    let candidate_identity =
        artifact_identity(&candidate_git, &candidate_engine, &candidate_dictionary)?;
    let cases = crate::quality::engine_cases(&fixture);
    let quality_case_count = fixture.cases.len();
    let expected_capture_count = quality_case_count + fixture.negative_controls.len();
    if cases.len() != expected_capture_count {
        return Err(err(
            "quality fixture capture case expansion is inconsistent",
        ));
    }
    let timeout = parse_timeout(flags)?;
    let temp_root = flags
        .get("temp-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("sakura-ime-eval-quality-capture"));

    let baseline_started = Instant::now();
    let baseline_candidates = capture_kana_candidates(
        &baseline_engine,
        &baseline_dictionary,
        &cases,
        &temp_root,
        timeout,
    )?;
    let baseline_capture = crate::types::CaptureRuntime {
        terminal: "completed".to_owned(),
        truncated: false,
        elapsed_us: Some(
            baseline_started
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64,
        ),
    };
    let candidate_started = Instant::now();
    let candidate_candidates = capture_kana_candidates(
        &candidate_engine,
        &candidate_dictionary,
        &cases,
        &temp_root,
        timeout,
    )?;
    let candidate_capture = crate::types::CaptureRuntime {
        terminal: "completed".to_owned(),
        truncated: false,
        elapsed_us: Some(
            candidate_started
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64,
        ),
    };
    if baseline_candidates.len() != expected_capture_count
        || candidate_candidates.len() != expected_capture_count
    {
        return Err(err("quality capture returned an unexpected case count"));
    }
    let pairs = fixture
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| CapturePair {
            case_id: case.case_id.clone(),
            baseline: baseline_candidates[index].clone(),
            candidate: candidate_candidates[index].clone(),
        })
        .collect();
    let control_pairs = fixture
        .negative_controls
        .iter()
        .enumerate()
        .map(|(index, control)| {
            let capture_index = quality_case_count + index;
            CaptureControlPair {
                control_id: control.control_id.clone(),
                reading: control.reading.clone(),
                baseline: baseline_candidates[capture_index].clone(),
                candidate: candidate_candidates[capture_index].clone(),
            }
        })
        .collect();
    let capture = crate::quality::QualityActiveSegmentCapture {
        schema_version: crate::quality::QUALITY_CAPTURE_SCHEMA_VERSION,
        lane: crate::quality::ACTIVE_SEGMENT_CAPTURE_LANE.to_owned(),
        baseline: baseline_identity,
        candidate: candidate_identity,
        pairs,
        control_pairs,
        baseline_capture,
        candidate_capture,
    };
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
    }
    let bytes = serde_json::to_vec_pretty(&capture)
        .map_err(|error| err(format!("serialize active-segment quality capture: {error}")))?;
    fs::write(&out, bytes).map_err(|error| err(format!("write {}: {error}", out.display())))?;
    println!("{}", out.display());
    Ok(0)
}

fn cmd_quality_core_capture(flags: &Flags) -> Result<u8, Error> {
    let fixture_path = Path::new(flags.require("fixture")?);
    let fixture = crate::quality::load_fixture(fixture_path)?;
    let baseline_dictionary = PathBuf::from(flags.require("baseline-dictionary")?);
    let candidate_dictionary = PathBuf::from(flags.require("candidate-dictionary")?);
    let baseline_git = flags.require("baseline-git")?.to_owned();
    let candidate_git = flags.require("candidate-git")?.to_owned();
    let evaluator = flags
        .get("evaluator")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .ok_or_else(|| err("cannot resolve the core evaluator executable"))?;
    let out = PathBuf::from(flags.require("out")?);
    validate_git_sha("baseline-git", &baseline_git)?;
    validate_git_sha("candidate-git", &candidate_git)?;
    let capture = crate::quality::capture_whole_reading_fixture(
        &fixture,
        &baseline_dictionary,
        &candidate_dictionary,
        &baseline_git,
        &candidate_git,
        &evaluator,
    )?;
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
    }
    let bytes = serde_json::to_vec_pretty(&capture)
        .map_err(|error| err(format!("serialize core quality capture: {error}")))?;
    fs::write(&out, bytes).map_err(|error| err(format!("write {}: {error}", out.display())))?;
    println!(
        "quality-stage1-core\tcases={}\tcontrols={}\tevaluator={}\tdictionary_baseline={}\tdictionary_candidate={}",
        capture.pairs.len(),
        capture.control_pairs.len(),
        evaluator.display(),
        capture.baseline.dictionary_sha256,
        capture.candidate.dictionary_sha256,
    );
    println!("{}", out.display());
    Ok(0)
}

fn cmd_quality_score(flags: &Flags) -> Result<u8, Error> {
    let fixture = Path::new(flags.require("fixture")?);
    let capture = Path::new(flags.require("capture")?);
    let out = PathBuf::from(flags.require("out")?);
    let scoreboard = crate::quality::score_whole_reading_capture_file(fixture, capture)?;
    let bytes = serde_json::to_vec_pretty(&scoreboard)
        .map_err(|error| err(format!("serialize quality scoreboard: {error}")))?;
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
    }
    fs::write(&out, bytes).map_err(|error| err(format!("write {}: {error}", out.display())))?;
    println!(
        "quality-stage1\tbaseline_top1={}\tcandidate_top1={}\tcandidate_recall_at18={}\tfingerprint={}",
        scoreboard.baseline.summary.surface_top1,
        scoreboard.candidate.summary.surface_top1,
        scoreboard.candidate.summary.surface_in_top18,
        scoreboard.determinism_fingerprint,
    );
    println!("{}", out.display());
    Ok(0)
}

fn cmd_quality_compare(flags: &Flags) -> Result<u8, Error> {
    if flags.get("fixture").is_some() {
        return cmd_ranking_compare(flags);
    }
    let before = Path::new(flags.require("before")?);
    let after = Path::new(flags.require("after")?);
    let side = crate::comparison::ComparisonSide::parse(flags.get("side").unwrap_or("candidate"))?;
    let out = PathBuf::from(flags.require("out")?);
    let comparison = crate::comparison::compare_files(before, after, side)?;
    let bytes = serde_json::to_vec_pretty(&comparison)
        .map_err(|error| err(format!("serialize quality comparison: {error}")))?;
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
    }
    fs::write(&out, bytes).map_err(|error| err(format!("write {}: {error}", out.display())))?;
    println!("{}", comparison.human_summary());
    println!("{}", out.display());
    Ok(0)
}

fn cmd_quality_rank_score(flags: &Flags) -> Result<u8, Error> {
    let fixture = Path::new(flags.require("fixture")?);
    let snapshot = Path::new(
        flags
            .get("snapshot")
            .or_else(|| flags.get("input"))
            .ok_or_else(|| err("missing --snapshot"))?,
    );
    let out = PathBuf::from(flags.require("out")?);
    let report = crate::ranking_comparison::score_ranking_file(fixture, snapshot)?;
    let bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| err(format!("serialize Issue #93 ranking score: {error}")))?;
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
    }
    fs::write(&out, bytes).map_err(|error| err(format!("write {}: {error}", out.display())))?;
    println!("{}", report.human_summary());
    println!("{}", out.display());
    Ok(0)
}

fn cmd_ranking_compare(flags: &Flags) -> Result<u8, Error> {
    let fixture = Path::new(flags.require("fixture")?);
    let before = Path::new(flags.require("before")?);
    let after = Path::new(flags.require("after")?);
    let out = PathBuf::from(flags.require("out")?);
    let report = crate::ranking_comparison::compare_ranking_files(fixture, before, after)?;
    let bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| err(format!("serialize Issue #93 ranking comparison: {error}")))?;
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
    }
    fs::write(&out, bytes).map_err(|error| err(format!("write {}: {error}", out.display())))?;
    println!("{}", report.human_summary());
    println!("{}", out.display());
    Ok(0)
}

fn cmd_approve_history(eval_root: &Path, flags: &Flags) -> Result<u8, Error> {
    let review = Path::new(flags.require("review")?);
    let approved_ids = Path::new(flags.require("approved-ids")?);
    let out_dir = Path::new(flags.require("out-dir")?);
    let manifest = flags
        .get("manifest")
        .map(PathBuf::from)
        .unwrap_or_else(|| semantic_corpus_dir(eval_root).join("manifest.json"));
    let report = crate::history_approval::approve(review, approved_ids, out_dir, &manifest)?;
    println!("approved-history-cases\t{}", report.approved_count);
    println!("semantic-corpus-case-count\t{}", report.corpus_case_count);
    println!(
        "approval-source-sha256\t{}",
        crate::hash::hex(&report.source_sha256)
    );
    println!("history-approval-output\t{}", out_dir.display());
    Ok(0)
}

fn artifact_identity(
    git_sha: &str,
    engine: &Path,
    dictionary: &Path,
) -> Result<ArtifactIdentity, Error> {
    let identity = ArtifactIdentity {
        git_sha: git_sha.to_owned(),
        engine_sha256: sha256_file(engine)?,
        dictionary_sha256: sha256_file(dictionary)?,
    };
    if !identity::artifact_identity_known(&identity) {
        return Err(err("capture artifact identity is malformed"));
    }
    Ok(identity)
}

fn validate_git_sha(name: &str, value: &str) -> Result<(), Error> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(err(format!(
            "--{name} must be a 40-character hexadecimal git SHA"
        )));
    }
    Ok(())
}

fn cmd_judge(eval_root: &Path, flags: &Flags) -> Result<u8, Error> {
    let (cases, capture) = load_job(eval_root, flags)?;
    let seed = parse_seed(flags)?;
    let backend_kind = BackendKind::parse(flags.get("backend").unwrap_or("prefer-literal"))?;
    let (codex_version, records) = if matches!(backend_kind, BackendKind::Codex) {
        let manifest = identity::load_manifest(eval_root)?;
        let actual = crate::codex::detect_version()?;
        crate::codex::require_pinned_version(&actual, &manifest.codex_cli_version)?;
        let temp_root = flags
            .get("temp-root")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("sakura-ime-judge"));
        let timeout = parse_timeout(flags)?;
        let mut backend = crate::backend::CodexBackend::new(eval_root, temp_root, timeout)?;
        let mut records = Vec::new();
        for pair in &capture.pairs {
            let case = cases
                .get(&pair.case_id)
                .ok_or_else(|| err(format!("unknown case {}", pair.case_id)))?;
            records.push(judge_pair(
                case,
                &pair.baseline,
                &pair.candidate,
                seed,
                &mut backend,
            )?);
        }
        (actual, records)
    } else {
        let mut records = Vec::new();
        for pair in &capture.pairs {
            let case = cases
                .get(&pair.case_id)
                .ok_or_else(|| err(format!("unknown case {}", pair.case_id)))?;
            let record = match backend_kind {
                BackendKind::PreferLiteral => judge_pair(
                    case,
                    &pair.baseline,
                    &pair.candidate,
                    seed,
                    &mut PreferLiteral,
                )?,
                BackendKind::AlwaysA => {
                    judge_pair(case, &pair.baseline, &pair.candidate, seed, &mut AlwaysA)?
                }
                BackendKind::Codex => unreachable!(),
            };
            records.push(record);
        }
        (
            flags
                .get("codex-version")
                .unwrap_or("test-double")
                .to_owned(),
            records,
        )
    };
    let identity = identity::collect(eval_root, &capture, &codex_version)?;
    let aggregate = Aggregate::from_records(&records);
    let profile = GateProfile::parse(flags.get("profile").unwrap_or("phase1"))?;
    let calibration = flags
        .get("calibration")
        .map(|path| crate::calibration::load(Path::new(path)))
        .transpose()?
        .map(|file| crate::calibration::calculate(&file.observations))
        .transpose()?;
    let checks = gate::evaluate(profile, &aggregate, &identity, calibration.as_ref());
    let passed = gate::all_passed(&checks);
    if let Some(out) = flags.get("out") {
        write_results(
            Path::new(out),
            &records,
            &identity,
            &aggregate,
            &checks,
            passed,
        )?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&records).map_err(|error| err(error.to_string()))?
    );
    Ok(if passed { 0 } else { 1 })
}

fn cmd_calibrate(flags: &Flags) -> Result<u8, Error> {
    let path = Path::new(flags.require("labels")?);
    let file = crate::calibration::load(path)?;
    let metrics = crate::calibration::calculate(&file.observations)?;
    let json = serde_json::to_string_pretty(&metrics)
        .map_err(|error| err(format!("serialize calibration metrics: {error}")))?;
    if let Some(out) = flags.get("out") {
        fs::write(out, &json).map_err(|error| err(format!("write {out}: {error}")))?;
    }
    println!("{json}");
    Ok(if metrics.meets_acceptance() { 0 } else { 1 })
}

fn cmd_gate(eval_root: &Path, flags: &Flags) -> Result<u8, Error> {
    let results_dir = Path::new(flags.require("results")?);
    let (records, identity) = read_results(results_dir)?;
    let _ = eval_root;
    let aggregate = Aggregate::from_records(&records);
    let profile = GateProfile::parse(flags.get("profile").unwrap_or("phase1"))?;
    let calibration = flags
        .get("calibration")
        .map(|path| crate::calibration::load(Path::new(path)))
        .transpose()?
        .map(|file| crate::calibration::calculate(&file.observations))
        .transpose()?;
    let checks = gate::evaluate(profile, &aggregate, &identity, calibration.as_ref());
    for check in &checks {
        println!(
            "{} {} {}",
            check.id,
            if check.passed { "PASS" } else { "FAIL" },
            check.detail
        );
    }
    Ok(if gate::all_passed(&checks) { 0 } else { 1 })
}

fn cmd_report(eval_root: &Path, flags: &Flags) -> Result<u8, Error> {
    let results_dir = Path::new(flags.require("results")?);
    let (records, identity) = read_results(results_dir)?;
    let _ = eval_root;
    let aggregate = Aggregate::from_records(&records);
    let profile = GateProfile::parse(flags.get("profile").unwrap_or("phase1"))?;
    let calibration = flags
        .get("calibration")
        .map(|path| crate::calibration::load(Path::new(path)))
        .transpose()?
        .map(|file| crate::calibration::calculate(&file.observations))
        .transpose()?;
    let checks = gate::evaluate(profile, &aggregate, &identity, calibration.as_ref());
    let passed = gate::all_passed(&checks);
    print!(
        "{}",
        report::render(&identity, &aggregate, &checks, passed)?
    );
    Ok(if passed { 0 } else { 1 })
}

fn load_job(
    eval_root: &Path,
    flags: &Flags,
) -> Result<
    (
        std::collections::BTreeMap<String, SemanticCase>,
        CaptureFile,
    ),
    Error,
> {
    let capture = load_capture(Path::new(flags.require("capture")?))?;
    let cases = load_case_map(&semantic_corpus_dir(eval_root))?;
    Ok((cases, capture))
}

fn load_optional_capture(flags: &Flags) -> Result<CaptureFile, Error> {
    if let Some(path) = flags.get("capture") {
        load_capture(Path::new(path))
    } else {
        Ok(CaptureFile {
            schema_version: 1,
            baseline: crate::types::ArtifactIdentity {
                git_sha: "unspecified".into(),
                engine_sha256: "unspecified".into(),
                dictionary_sha256: "unspecified".into(),
            },
            candidate: crate::types::ArtifactIdentity {
                git_sha: "unspecified".into(),
                engine_sha256: "unspecified".into(),
                dictionary_sha256: "unspecified".into(),
            },
            pairs: vec![crate::types::CapturePair {
                case_id: "none".into(),
                baseline: SystemOutput {
                    candidates: vec!["_".into()],
                },
                candidate: SystemOutput {
                    candidates: vec!["_".into()],
                },
            }],
            control_pairs: Vec::new(),
            baseline_capture: None,
            candidate_capture: None,
        })
    }
}

fn parse_seed(flags: &Flags) -> Result<u64, Error> {
    flags
        .get("seed")
        .unwrap_or("1")
        .parse()
        .map_err(|_| err("--seed must be u64"))
}

fn parse_timeout(flags: &Flags) -> Result<Duration, Error> {
    let milliseconds = flags
        .get("timeout-ms")
        .unwrap_or("120000")
        .parse::<u64>()
        .map_err(|_| err("--timeout-ms must be an unsigned integer"))?;
    if milliseconds == 0 {
        return Err(err("--timeout-ms must be greater than zero"));
    }
    Ok(Duration::from_millis(milliseconds))
}

fn oracle_name(verdict: crate::types::OracleVerdict) -> &'static str {
    match verdict {
        crate::types::OracleVerdict::Pass => "pass",
        crate::types::OracleVerdict::LiteralCorruption => "literal_corruption",
    }
}

fn write_results(
    dir: &Path,
    records: &[crate::types::CaseRecord],
    identity: &crate::identity::RunIdentity,
    aggregate: &Aggregate,
    checks: &[crate::types::GateCheck],
    passed: bool,
) -> Result<(), Error> {
    fs::create_dir_all(dir).map_err(|error| err(format!("create {}: {error}", dir.display())))?;
    fs::write(
        dir.join("records.json"),
        serde_json::to_vec_pretty(records).map_err(|error| err(error.to_string()))?,
    )
    .map_err(|error| err(format!("write records.json: {error}")))?;
    fs::write(
        dir.join("identity.json"),
        serde_json::to_vec_pretty(identity).map_err(|error| err(error.to_string()))?,
    )
    .map_err(|error| err(format!("write identity.json: {error}")))?;
    fs::write(
        dir.join("report.txt"),
        report::render(identity, aggregate, checks, passed)?,
    )
    .map_err(|error| err(format!("write report.txt: {error}")))?;
    Ok(())
}

fn read_results(
    dir: &Path,
) -> Result<(Vec<crate::types::CaseRecord>, crate::identity::RunIdentity), Error> {
    let records: Vec<crate::types::CaseRecord> = serde_json::from_slice(
        &fs::read(dir.join("records.json"))
            .map_err(|error| err(format!("read records.json: {error}")))?,
    )
    .map_err(|error| err(format!("parse records.json: {error}")))?;
    let identity: crate::identity::RunIdentity = serde_json::from_slice(
        &fs::read(dir.join("identity.json"))
            .map_err(|error| err(format!("read identity.json: {error}")))?,
    )
    .map_err(|error| err(format!("parse identity.json: {error}")))?;
    Ok((records, identity))
}
