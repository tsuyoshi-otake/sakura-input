//! Real-engine integration coverage for the ime-eval capture boundary.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sakura_ime_eval::capture::MAX_CANDIDATES_PER_SYSTEM;
use sakura_ime_eval::capture_engine::capture_candidates;
use sakura_ime_eval::types::{Constraints, Context, Input, SemanticCase};

const PATIENT: Duration = Duration::from_secs(5);

// Exact fixture rows moved from sakura-engine/tests/common/mod.rs at 1ac33a1.
// Keeping them local makes ime-eval own its integration fixture without
// importing the engine's test harness or touching a user dictionary.
fn test_dictionary(local_app_data: &Path, extra: Vec<dictc_core::SourceEntry>) -> PathBuf {
    let directory = local_app_data.join("engine-fixture");
    std::fs::create_dir(&directory).expect("create owned fixture directory");
    let path = directory.join("system.dic");
    let mut entries = dictc_core::parse_entries(
        "engine-fixture.tsv",
        concat!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
            "かな\t仮名\t0\t0\t100\t100\tit\tIT用語\n",
            "きょう\t今日\t0\t0\t100\t100\t\tfixture\n",
            "は\tは\t0\t0\t100\t100\t\tfixture\n",
            "きょうは\t今日は\t0\t0\t500\t500\t\tfixture\n",
        ),
    )
    .expect("parse engine fixture entries");
    entries.extend(
        dictc_core::parse_entries(
            "engine-shifted-english.tsv",
            concat!(
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
                "claude\tClaude\t0\t0\t100\t100\tit\tfixture\n",
                "claude\tClaude Code\t0\t0\t150\t150\tit\tfixture\n",
                "openai\tOpenAI\t0\t0\t100\t100\tit\tfixture\n",
                "gitlab\tGitLab\t0\t0\t100\t100\tit\tfixture\n",
                "pytorch\tPyTorch\t0\t0\t100\t100\tit\tfixture\n",
            ),
        )
        .expect("parse shifted English fixture entries"),
    );
    entries.extend(extra);
    let matrix = dictc_core::parse_connection(
        "engine-fixture-matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("parse engine fixture matrix");
    let image = dictc_core::compile(&entries, &matrix).expect("compile engine fixture dictionary");
    std::fs::write(&path, image).expect("write owned fixture dictionary");
    path
}

/// One owned fixture profile and runner temp root under the test target
/// directory, so no test touches the user's profile or ambient pipe.
fn owned_roots() -> (PathBuf, PathBuf) {
    // Tests run in parallel threads that can read the same clock value, so
    // the per-process sequence keeps their roots distinct.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sakura-ime-eval-capture");
    fs::create_dir_all(&root).expect("create capture fixture root");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let owner = format!("{}-{sequence}-{nonce:x}", std::process::id());
    let profile = root.join(format!("fixture-{owner}"));
    fs::create_dir(&profile).expect("create capture fixture profile");
    let temp_root = root.join(format!("runner-{owner}"));
    (profile, temp_root)
}

fn romaji_case(case_id: &str, left: &str, reading: &str, typing: &str) -> SemanticCase {
    SemanticCase {
        schema_version: 1,
        case_id: case_id.to_owned(),
        task: "conversion".to_owned(),
        family: Some("normal-conversion".to_owned()),
        role: Some("positive".to_owned()),
        context: Context {
            left: left.to_owned(),
            right: "晴れ".to_owned(),
        },
        input: Input {
            input_mode: Some("romaji".to_owned()),
            reading: reading.to_owned(),
            typing: Some(typing.to_owned()),
        },
        constraints: Constraints::default(),
        privacy_provenance: None,
    }
}

fn literal_case(case_id: &str, typed: &str) -> SemanticCase {
    let mut case = romaji_case(case_id, "開発には", typed, typed);
    case.family = Some("ascii-literal".to_owned());
    case.context.right = "を使う".to_owned();
    case.constraints.literal_token = true;
    case
}

/// The quality runner must exercise the same real engine binary as the
/// ordinary pipe tests, not a dispatcher double or the user's ambient pipe.
#[test]
fn real_engine_candidate_capture_round_trip() {
    let (profile, temp_root) = owned_roots();
    let dictionary = test_dictionary(&profile, Vec::new());
    let case = romaji_case("real-capture-kyou", "今日は", "きょう", "kyou");
    let engine = PathBuf::from(env!("CARGO_BIN_EXE_ime_eval_sakura_engine"));
    let result = capture_candidates(&engine, &dictionary, &[case], &temp_root, PATIENT);
    let _ = fs::remove_dir_all(&profile);
    let _ = fs::remove_dir_all(&temp_root);
    let capture = result.expect("real capture must complete");
    assert!(capture.truncated_case_ids.is_empty());
    let outputs = capture.outputs;
    assert_eq!(outputs.len(), 1);
    assert!(
        outputs[0]
            .candidates
            .iter()
            .any(|candidate| candidate == "今日"),
        "fixture candidate missing from real capture: {:?}",
        outputs[0].candidates
    );
}

/// Issue #297: a reading whose real candidate list is longer than the
/// capture-file bound (`MAX_CANDIDATES_PER_SYSTEM`) must still produce a
/// loadable capture. The romaji capture keeps the leading candidates in
/// engine order instead of failing the whole run.
#[test]
fn real_engine_capture_keeps_leading_candidates_of_a_long_list() {
    let (profile, temp_root) = owned_roots();
    // 70 distinct surfaces with rising cost, so the engine order is
    // 候補00, 候補01, ... and the list exceeds the 64-candidate file bound.
    let rows: String = (0..70)
        .map(|index| {
            let cost = 100 + index;
            format!("こうほ\t候補{index:02}\t0\t0\t{cost}\t{cost}\t\tfixture\n")
        })
        .collect();
    let extra = dictc_core::parse_entries(
        "engine-long-list.tsv",
        &format!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n{rows}"
        ),
    )
    .expect("parse long-list fixture entries");
    let dictionary = test_dictionary(&profile, extra);
    let case = romaji_case("real-capture-long-list", "", "こうほ", "kouho");
    let engine = PathBuf::from(env!("CARGO_BIN_EXE_ime_eval_sakura_engine"));
    let result = capture_candidates(&engine, &dictionary, &[case], &temp_root, PATIENT);
    let _ = fs::remove_dir_all(&profile);
    let _ = fs::remove_dir_all(&temp_root);
    let capture = result.expect("a long real list must still be captured");
    assert_eq!(capture.truncated_case_ids, ["real-capture-long-list"]);
    let outputs = capture.outputs;
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].candidates.len(), MAX_CANDIDATES_PER_SYSTEM);
    let fixture_order: Vec<&str> = outputs[0]
        .candidates
        .iter()
        .map(String::as_str)
        .filter(|candidate| candidate.starts_with("候補"))
        .collect();
    let expected: Vec<String> = (0..fixture_order.len())
        .map(|index| format!("候補{index:02}"))
        .collect();
    assert!(
        fixture_order.len() > 32,
        "long-list fixture surfaces missing from capture: {:?}",
        outputs[0].candidates
    );
    assert_eq!(fixture_order, expected, "capture must keep engine order");
}

/// Issue #303: a `literal_token` case types an ASCII token that the engine
/// keeps as a literal composition, so Space inserts a space instead of
/// opening a candidate list. The capture records the typed composition as
/// the case's single candidate and the ordinary case in the same run still
/// converts.
#[test]
fn real_engine_capture_records_the_composition_of_literal_cases() {
    let (profile, temp_root) = owned_roots();
    let dictionary = test_dictionary(&profile, Vec::new());
    let cases = [
        literal_case("real-capture-literal-avx", "AVX-512"),
        literal_case("real-capture-literal-spaced", "Claude Code"),
        romaji_case("real-capture-kyou", "今日は", "きょう", "kyou"),
    ];
    let engine = PathBuf::from(env!("CARGO_BIN_EXE_ime_eval_sakura_engine"));
    let result = capture_candidates(&engine, &dictionary, &cases, &temp_root, PATIENT);
    let _ = fs::remove_dir_all(&profile);
    let _ = fs::remove_dir_all(&temp_root);
    let capture = result.expect("literal cases must not fail the capture");
    assert!(capture.truncated_case_ids.is_empty());
    let outputs = capture.outputs;
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0].candidates, ["AVX-512"]);
    assert_eq!(outputs[1].candidates, ["Claude Code"]);
    assert!(
        outputs[2]
            .candidates
            .iter()
            .any(|candidate| candidate == "今日"),
        "ordinary case after literal cases must still convert: {:?}",
        outputs[2].candidates
    );
}
