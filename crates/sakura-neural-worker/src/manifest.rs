use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufReader, Read},
    path::Path,
};

pub const MODEL: &str = "ku-nlp/deberta-v2-tiny-japanese-char-wwm";
pub const REV: &str = "41bcb8a393383a039c7ee18ded6893ca82e668b7";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    model: Model,
    tokenizer: Tokenizer,
    runtime: Runtime,
    files: Vec<Artifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    id: String,
    revision: String,
    format: String,
    opset: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tokenizer {
    class: String,
    word_tokenizer_type: String,
    subword_tokenizer_type: String,
    do_lower_case: bool,
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

pub fn validate(dir: &Path) -> Result<String, String> {
    let source = fs::read(dir.join("manifest.json")).map_err(|_| "manifest missing".to_owned())?;
    let manifest: Manifest =
        serde_json::from_slice(&source).map_err(|_| "malformed manifest".to_owned())?;
    if manifest.schema_version != 1
        || manifest.model.id != MODEL
        || manifest.model.revision != REV
        || manifest.model.format != "onnx-fp32-o2"
        || manifest.model.opset != 18
        || manifest.tokenizer.class != "BertJapaneseTokenizer"
        || manifest.tokenizer.word_tokenizer_type != "basic"
        || manifest.tokenizer.subword_tokenizer_type != "character"
        || manifest.tokenizer.do_lower_case
        || manifest.runtime.name != "onnxruntime"
        || manifest.runtime.version != "1.28.0"
    {
        return Err("manifest identity mismatch".into());
    }
    if manifest.files.len() != 2 {
        return Err("manifest file set mismatch".into());
    }
    let mut model_hash = None;
    for name in ["model.onnx", "vocab.txt"] {
        let records: Vec<_> = manifest
            .files
            .iter()
            .filter(|artifact| artifact.path == name)
            .collect();
        if records.len() != 1 {
            return Err("manifest file set mismatch".into());
        }
        let record = records[0];
        let path = dir.join(name);
        if fs::metadata(&path)
            .map_err(|_| "artifact missing".to_owned())?
            .len()
            != record.bytes
        {
            return Err("artifact size mismatch".into());
        }
        let hash = hex(&path)?;
        if record.sha256.len() != 64
            || !record.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !record.sha256.eq_ignore_ascii_case(&hash)
        {
            return Err("artifact hash mismatch".into());
        }
        if name == "model.onnx" {
            model_hash = Some(hash);
        }
    }
    model_hash.ok_or_else(|| "manifest file set mismatch".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn directory() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "sakura-worker-manifest-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("model.onnx"), b"a").unwrap();
        fs::write(path.join("vocab.txt"), b"b").unwrap();
        let manifest = format!(
            "{{\"files\":[{{\"bytes\":1,\"path\":\"model.onnx\",\"sha256\":\"{}\"}},{{\"bytes\":1,\"path\":\"vocab.txt\",\"sha256\":\"{}\"}}],\"model\":{{\"format\":\"onnx-fp32-o2\",\"id\":\"{}\",\"opset\":18,\"revision\":\"{}\"}},\"runtime\":{{\"name\":\"onnxruntime\",\"version\":\"1.28.0\"}},\"schema_version\":1,\"tokenizer\":{{\"class\":\"BertJapaneseTokenizer\",\"do_lower_case\":false,\"subword_tokenizer_type\":\"character\",\"word_tokenizer_type\":\"basic\"}}}}",
            hex(&path.join("model.onnx")).unwrap(),
            hex(&path.join("vocab.txt")).unwrap(),
            MODEL,
            REV
        );
        fs::write(path.join("manifest.json"), manifest).unwrap();
        path
    }

    #[test]
    fn validates_exact_manifest() {
        let path = directory();
        assert!(validate(&path).is_ok());
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
