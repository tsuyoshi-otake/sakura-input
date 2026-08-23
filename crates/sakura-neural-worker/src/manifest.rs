use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufReader, Read},
    path::Path,
};

pub const MODEL_ID: &str = "Sakura-Rerank-Tiny-v1-research-prototype";
pub const SOURCE_MANIFEST_SHA256: &str =
    "07f1c54cbe361e117b547f47511de960977f1d0f754f051f44b9447a591d96b9";
pub const RUNTIME_STATUS: &str = "release_experimental_gate_a_failed";
pub const MODEL_LICENSE: &str = "MIT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedManifest {
    pub model_hash: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    manifest_kind: String,
    status: String,
    model: Model,
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

fn hex(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|_| "artifact missing".to_owned())?;
    let mut source = BufReader::new(file);
    let mut digest = Sha256::new();
    // The Windows worker main thread has a deliberately small default stack.
    // Keep the bounded streaming buffer on the heap so startup validation
    // cannot overflow before ORT is initialized.
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|_| "artifact read failed".to_owned())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn validate(dir: &Path) -> Result<ValidatedManifest, String> {
    let source = fs::read(dir.join("manifest.json")).map_err(|_| "manifest missing".to_owned())?;
    let manifest: Manifest =
        serde_json::from_slice(&source).map_err(|_| "malformed manifest".to_owned())?;
    if manifest.schema_version != 1
        || manifest.manifest_kind != "sakura_rerank_runtime_model"
        || manifest.status != RUNTIME_STATUS
        || manifest.model.id != MODEL_ID
        || manifest.model.contract_version != 1
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
    if manifest.files.len() != 1 || manifest.files[0].path != "model.onnx" {
        return Err("manifest file set mismatch".into());
    }
    let record = &manifest.files[0];
    let path = dir.join("model.onnx");
    if fs::metadata(&path)
        .map_err(|_| "artifact missing".to_owned())?
        .len()
        != record.bytes
    {
        return Err("artifact size mismatch".into());
    }
    let model_hash = hex(&path)?;
    if record.sha256.len() != 64
        || !record.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !record.sha256.eq_ignore_ascii_case(&model_hash)
    {
        return Err("artifact hash mismatch".into());
    }
    Ok(ValidatedManifest { model_hash })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn directory() -> std::path::PathBuf {
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
        fs::write(path.join("model.onnx"), b"a").unwrap();
        let manifest = format!(
            "{{\"files\":[{{\"bytes\":1,\"path\":\"model.onnx\",\"sha256\":\"{}\"}}],\"manifest_kind\":\"sakura_rerank_runtime_model\",\"model\":{{\"contract_version\":1,\"format\":\"onnx-fp32\",\"id\":\"{}\",\"opset\":18}},\"raw_stable_ids_in_manifest\":false,\"raw_text_in_manifest\":false,\"research\":{{\"artifact_distribution_authorized\":true,\"final_holdout_used\":false,\"gate_a_status\":\"gate_a_failed\",\"license\":\"{}\",\"source_manifest_sha256\":\"{}\"}},\"runtime\":{{\"name\":\"onnxruntime\",\"version\":\"1.28.0\"}},\"schema_version\":1,\"status\":\"{}\"}}",
            hex(&path.join("model.onnx")).unwrap(),
            MODEL_ID,
            MODEL_LICENSE,
            SOURCE_MANIFEST_SHA256,
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
}
