//! Real-engine integration coverage for the ime-eval capture boundary.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sakura_ime_eval::capture_engine::capture_candidates;
use sakura_ime_eval::types::{Constraints, Context, Input, SemanticCase};

const PATIENT: Duration = Duration::from_secs(5);

// Exact fixture rows moved from sakura-engine/tests/common/mod.rs at 1ac33a1.
// Keeping them local makes ime-eval own its integration fixture without
// importing the engine's test harness or touching a user dictionary.
fn test_dictionary(local_app_data: &Path) -> PathBuf {
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

/// The quality runner must exercise the same real engine binary as the
/// ordinary pipe tests, not a dispatcher double or the user's ambient pipe.
#[test]
fn real_engine_candidate_capture_round_trip() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sakura-ime-eval-capture");
    fs::create_dir_all(&root).expect("create capture fixture root");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let profile = root.join(format!("fixture-{}-{nonce:x}", std::process::id()));
    fs::create_dir(&profile).expect("create capture fixture profile");
    let dictionary = test_dictionary(&profile);
    let temp_root = root.join(format!("runner-{}-{nonce:x}", std::process::id()));
    let case = SemanticCase {
        schema_version: 1,
        case_id: "real-capture-kyou".to_owned(),
        task: "conversion".to_owned(),
        family: Some("normal-conversion".to_owned()),
        role: Some("positive".to_owned()),
        context: Context {
            left: "今日は".to_owned(),
            right: "晴れ".to_owned(),
        },
        input: Input {
            input_mode: Some("romaji".to_owned()),
            reading: "きょう".to_owned(),
            typing: Some("kyou".to_owned()),
        },
        constraints: Constraints::default(),
        privacy_provenance: None,
    };
    let engine = PathBuf::from(env!("CARGO_BIN_EXE_ime_eval_sakura_engine"));
    let result = capture_candidates(&engine, &dictionary, &[case], &temp_root, PATIENT);
    let _ = fs::remove_dir_all(&profile);
    let _ = fs::remove_dir_all(&temp_root);
    let outputs = result.expect("real capture must complete");
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
