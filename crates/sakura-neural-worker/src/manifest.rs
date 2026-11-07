use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{self, File},
    io::Read,
    path::Path,
};

pub const MODEL_ID: &str = "Sakura-Rerank-Tiny-v1-issue29-distilled";
pub const SOURCE_MANIFEST_SHA256: &str =
    "42fbc5af761194b01375dd71e6e34a71ea028a9d9f5337e4c861ee25cbaebed3";
pub const RUNTIME_STATUS: &str = "release_experimental_gate_a_failed";
pub const MODEL_LICENSE: &str = "MIT";
// Reviewed model pins. These must stay equal to the values in
// scripts/stage-sakura-rerank.ps1 and
// scripts/import-sakura-rerank-release-model.ps1 and to the tracked
// artifact models/sakura-rerank-tiny-v1/model.onnx.
pub const MODEL_SHA256: &str = "a58ebea1257cdc2507b02acb8b7e958181f05577fb629d7928eb1f02fe078686";
pub const MODEL_BYTES: u64 = 7_467_219;

pub struct ValidatedManifest {
    pub model_hash: String,
    pub model_bytes: Vec<u8>,
}

impl fmt::Debug for ValidatedManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedManifest")
            .field("model_hash", &self.model_hash)
            .field("model_bytes_len", &self.model_bytes.len())
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    manifest_kind: String,
    status: String,
    model: Model,
    selection: Selection,
    runtime: Runtime,
    research: Research,
    files: Vec<Artifact>,
    raw_text_in_manifest: bool,
    raw_stable_ids_in_manifest: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    id: String,
    contract_version: u32,
    format: String,
    opset: u32,
}

/// The selection contract the manifest declares for the gated model choice.
/// Anything but the shipped `sakura_rerank_proto::Selection` rule and margin
/// is refused outright: a runtime built for one gate must not load a model
/// exported for another.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    rule: String,
    margin: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Research {
    source_manifest_sha256: String,
    gate_a_status: String,
    final_holdout_used: bool,
    artifact_distribution_authorized: bool,
    license: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Runtime {
    name: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: String,
    bytes: u64,
    sha256: String,
}

pub fn validate(dir: &Path) -> Result<ValidatedManifest, String> {
    let source = fs::read(dir.join("manifest.json")).map_err(|_| "manifest missing".to_owned())?;
    let manifest: Manifest =
        serde_json::from_slice(&source).map_err(|_| "malformed manifest".to_owned())?;
    if manifest.schema_version != 1
        || manifest.manifest_kind != "sakura_rerank_runtime_model"
        || manifest.status != RUNTIME_STATUS
        || manifest.model.id != MODEL_ID
        || manifest.model.contract_version != 2
        || manifest.model.format != "onnx-fp32"
        || manifest.model.opset != 18
        || manifest.runtime.name != "onnxruntime"
        || manifest.runtime.version != "1.28.0"
        || manifest.research.source_manifest_sha256 != SOURCE_MANIFEST_SHA256
        || manifest.research.gate_a_status != "gate_a_failed"
        || manifest.research.final_holdout_used
        || !manifest.research.artifact_distribution_authorized
        || manifest.research.license != MODEL_LICENSE
        || manifest.raw_text_in_manifest
        || manifest.raw_stable_ids_in_manifest
    {
        return Err("manifest identity mismatch".into());
    }
    if manifest.selection.rule != sakura_rerank_proto::Selection::RULE
        || manifest.selection.margin != sakura_rerank_proto::Selection::LOCAL_OVERRIDE_MARGIN
    {
        return Err("manifest selection mismatch".into());
    }
    if manifest.files.len() != 1 || manifest.files[0].path != "model.onnx" {
        return Err("manifest file set mismatch".into());
    }
    let record = &manifest.files[0];
    if record.bytes != MODEL_BYTES || !record.sha256.eq_ignore_ascii_case(MODEL_SHA256) {
        return Err("manifest pin mismatch".into());
    }
    let path = dir.join("model.onnx");
    // Open the artifact exactly once so the bytes hashed here are the bytes
    // the runtime later parses from ValidatedManifest::model_bytes (no TOCTOU
    // window and no second open).
    let file = File::open(&path).map_err(|_| "artifact missing".to_owned())?;
    if file
        .metadata()
        .map_err(|_| "artifact read failed".to_owned())?
        .len()
        != MODEL_BYTES
    {
        return Err("artifact size mismatch".into());
    }
    let mut model_bytes = Vec::new();
    file.take(MODEL_BYTES + 1)
        .read_to_end(&mut model_bytes)
        .map_err(|_| "artifact read failed".to_owned())?;
    if model_bytes.len() as u64 != MODEL_BYTES {
        return Err("artifact size mismatch".into());
    }
    let model_hash = format!("{:x}", Sha256::digest(&model_bytes));
    if model_hash != MODEL_SHA256 {
        return Err("artifact hash mismatch".into());
    }
    Ok(ValidatedManifest {
        model_hash,
        model_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn hex(path: &Path) -> Result<String, String> {
        let bytes = fs::read(path).map_err(|_| "artifact missing".to_owned())?;
        Ok(format!("{:x}", Sha256::digest(&bytes)))
    }

    fn tracked_model() -> Vec<u8> {
        fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../models/sakura-rerank-tiny-v1/model.onnx"),
        )
        .unwrap()
    }

    fn directory() -> std::path::PathBuf {
        directory_with(&tracked_model())
    }

    fn directory_with(model: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "sakura-worker-manifest-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("model.onnx"), model).unwrap();
        let manifest = format!(
            "{{\"files\":[{{\"bytes\":{},\"path\":\"model.onnx\",\"sha256\":\"{}\"}}],\"manifest_kind\":\"sakura_rerank_runtime_model\",\"model\":{{\"contract_version\":2,\"format\":\"onnx-fp32\",\"id\":\"{}\",\"opset\":18}},\"raw_stable_ids_in_manifest\":false,\"raw_text_in_manifest\":false,\"research\":{{\"artifact_distribution_authorized\":true,\"final_holdout_used\":false,\"gate_a_status\":\"gate_a_failed\",\"license\":\"{}\",\"source_manifest_sha256\":\"{}\"}},\"runtime\":{{\"name\":\"onnxruntime\",\"version\":\"1.28.0\"}},\"schema_version\":1,\"selection\":{{\"margin\":1.7,\"rule\":\"{}\"}},\"status\":\"{}\"}}",
            model.len(),
            hex(&path.join("model.onnx")).unwrap(),
            MODEL_ID,
            MODEL_LICENSE,
            SOURCE_MANIFEST_SHA256,
            sakura_rerank_proto::Selection::RULE,
            RUNTIME_STATUS,
        );
        fs::write(path.join("manifest.json"), manifest).unwrap();
        path
    }

    #[test]
    fn validates_exact_sakura_manifest() {
        let path = directory();
        assert_eq!(
            validate(&path).unwrap().model_hash,
            hex(&path.join("model.onnx")).unwrap()
        );
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_changed_artifact() {
        let path = directory();
        fs::write(path.join("model.onnx"), b"x").unwrap();
        assert!(validate(&path).is_err());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_unapproved_or_mislicensed_distribution_metadata() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replace(
                "\"artifact_distribution_authorized\":true",
                "\"artifact_distribution_authorized\":false",
            ),
        )
        .unwrap();
        assert!(validate(&path).is_err());
        fs::write(
            path.join("manifest.json"),
            manifest.replace("\"license\":\"MIT\"", "\"license\":\"unknown\""),
        )
        .unwrap();
        assert!(validate(&path).is_err());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_legacy_deberta_identity() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replace(MODEL_ID, "ku-nlp/deberta-v2-tiny-japanese-char-wwm"),
        )
        .unwrap();
        assert!(validate(&path).is_err());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_unknown_duplicate_or_missing_schema() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replacen(
                "\"schema_version\":1",
                "\"schema_version\":1,\"extra\":true",
                1,
            ),
        )
        .unwrap();
        assert!(validate(&path).is_err());
        fs::write(
            path.join("manifest.json"),
            manifest.replacen(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1",
                1,
            ),
        )
        .unwrap();
        assert!(validate(&path).is_err());
        fs::remove_dir_all(path).unwrap();
        assert!(validate(Path::new("missing-model-directory")).is_err());
    }

    /// M1: a consistently rewritten model + manifest is rejected by the pin.
    #[test]
    fn rejects_consistent_rewrite_of_model_and_manifest() {
        let path = directory_with(b"not the reviewed model");
        assert_eq!(validate(&path).unwrap_err(), "manifest pin mismatch");
        fs::remove_dir_all(path).unwrap();
    }

    /// M1: a same-length one-byte change under the pinned manifest is rejected.
    #[test]
    fn rejects_same_length_artifact_with_one_byte_changed() {
        let path = directory();
        let mut model = tracked_model();
        *model.last_mut().unwrap() ^= 0xff;
        fs::write(path.join("model.onnx"), model).unwrap();
        assert_eq!(validate(&path).unwrap_err(), "artifact hash mismatch");
        fs::remove_dir_all(path).unwrap();
    }

    /// M2: the returned bytes are exactly the bytes that were hashed.
    #[test]
    fn returns_the_hashed_bytes() {
        let path = directory();
        let validated = validate(&path).unwrap();
        fs::write(path.join("model.onnx"), vec![0u8; MODEL_BYTES as usize]).unwrap();
        assert_eq!(validated.model_bytes, tracked_model());
        assert_eq!(
            format!("{:x}", Sha256::digest(&validated.model_bytes)),
            MODEL_SHA256
        );
        fs::remove_dir_all(path).unwrap();
    }

    /// M1: a manifest with the reviewed hash but a wrong `bytes` field is
    /// rejected by the size half of the pin check.
    #[test]
    fn rejects_manifest_with_wrong_bytes_field_and_reviewed_hash() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replacen(
                &format!("\"bytes\":{}", MODEL_BYTES),
                &format!("\"bytes\":{}", MODEL_BYTES - 1),
                1,
            ),
        )
        .unwrap();
        assert_eq!(validate(&path).unwrap_err(), "manifest pin mismatch");
        fs::remove_dir_all(path).unwrap();
    }

    /// M1: a manifest with the reviewed size but a wrong `sha256` field is
    /// rejected by the hash half of the pin check.
    #[test]
    fn rejects_manifest_with_wrong_sha256_field_and_reviewed_size() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replacen(
                &format!("\"sha256\":\"{}\"", MODEL_SHA256),
                "\"sha256\":\"0000000000000000000000000000000000000000000000000000000000000000\"",
                1,
            ),
        )
        .unwrap();
        assert_eq!(validate(&path).unwrap_err(), "manifest pin mismatch");
        fs::remove_dir_all(path).unwrap();
    }

    /// M1/M5: a manifest that records the pinned hash in uppercase is
    /// accepted, matching the pre-pin case-insensitive comparison.
    #[test]
    fn accepts_uppercase_pinned_hash_in_manifest() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replacen(
                &format!("\"sha256\":\"{}\"", MODEL_SHA256),
                &format!("\"sha256\":\"{}\"", MODEL_SHA256.to_ascii_uppercase()),
                1,
            ),
        )
        .unwrap();
        let validated = validate(&path).unwrap();
        assert_eq!(validated.model_hash, MODEL_SHA256);
        fs::remove_dir_all(path).unwrap();
    }

    /// #440: a manifest that declares a different gate margin is refused.
    #[test]
    fn rejects_manifest_with_different_selection_margin() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replacen("\"margin\":1.7", "\"margin\":1.6", 1),
        )
        .unwrap();
        assert_eq!(validate(&path).unwrap_err(), "manifest selection mismatch");
        fs::remove_dir_all(path).unwrap();
    }

    /// #440: a manifest that declares an unknown selection rule is refused.
    #[test]
    fn rejects_manifest_with_unknown_selection_rule() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replacen(
                "\"rule\":\"logit_margin_over_local_choice\"",
                "\"rule\":\"max_score\"",
                1,
            ),
        )
        .unwrap();
        assert_eq!(validate(&path).unwrap_err(), "manifest selection mismatch");
        fs::remove_dir_all(path).unwrap();
    }

    /// #440: the six-feature contract is refused; contract version 2 is the
    /// only accepted model contract.
    #[test]
    fn rejects_contract_version_1() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replacen("\"contract_version\":2", "\"contract_version\":1", 1),
        )
        .unwrap();
        assert_eq!(validate(&path).unwrap_err(), "manifest identity mismatch");
        fs::remove_dir_all(path).unwrap();
    }

    /// #440: the previously shipped research-prototype id is refused.
    #[test]
    fn rejects_previous_research_prototype_identity() {
        let path = directory();
        let manifest = fs::read_to_string(path.join("manifest.json")).unwrap();
        fs::write(
            path.join("manifest.json"),
            manifest.replacen(MODEL_ID, "Sakura-Rerank-Tiny-v1-research-prototype", 1),
        )
        .unwrap();
        assert_eq!(validate(&path).unwrap_err(), "manifest identity mismatch");
        fs::remove_dir_all(path).unwrap();
    }

    /// M3: the compiled pins equal both scripts' pins and the tracked model.
    #[test]
    fn pins_match_scripts_and_tracked_model() {
        for source in [
            include_str!("../../../scripts/stage-sakura-rerank.ps1"),
            include_str!("../../../scripts/import-sakura-rerank-release-model.ps1"),
        ] {
            let sha256 = source
                .split("$expectedModelSha256 = '")
                .nth(1)
                .unwrap()
                .split('\'')
                .next()
                .unwrap();
            let bytes: u64 = source
                .split("$expectedModelBytes = ")
                .nth(1)
                .unwrap()
                .split('L')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(sha256, MODEL_SHA256);
            assert_eq!(bytes, MODEL_BYTES);
        }
        let model = tracked_model();
        assert_eq!(model.len() as u64, MODEL_BYTES);
        assert_eq!(format!("{:x}", Sha256::digest(&model)), MODEL_SHA256);
    }
}
