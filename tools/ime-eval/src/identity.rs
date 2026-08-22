use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::hash::{sha256_file, sha256_hex};
use crate::paths::{judge_v1_dir, semantic_corpus_dir};
use crate::types::{
    err, ArtifactIdentity, CaptureFile, Error, JudgeManifest, REQUIRED_MODEL, REQUIRED_REASONING,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunIdentity {
    pub baseline: ArtifactIdentity,
    pub candidate: ArtifactIdentity,
    pub model: String,
    pub reasoning: String,
    pub codex_cli_version: String,
    pub prompt_sha256: String,
    pub rubric_sha256: String,
    pub schema_sha256: String,
    pub calibration_schema_sha256: String,
    pub manifest_sha256: String,
    pub corpus_sha256: String,
    pub aggregation_version: u32,
}

impl RunIdentity {
    pub fn judge_known(&self) -> bool {
        !self.model.is_empty()
            && self.model == REQUIRED_MODEL
            && self.reasoning == REQUIRED_REASONING
            && (self.codex_cli_version == "test-double" || is_version(&self.codex_cli_version))
            && !self.prompt_sha256.is_empty()
            && !self.rubric_sha256.is_empty()
            && !self.schema_sha256.is_empty()
            && !self.calibration_schema_sha256.is_empty()
    }

    pub fn artifacts_known(&self) -> bool {
        artifact_identity_known(&self.baseline) && artifact_identity_known(&self.candidate)
    }
}

pub fn artifact_identity_known(id: &ArtifactIdentity) -> bool {
    is_hex(&id.git_sha, 40) && is_hex(&id.engine_sha256, 64) && is_hex(&id.dictionary_sha256, 64)
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let valid = parts.clone().count() == 3
        && parts.all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    valid
}

pub fn load_manifest(eval_root: &Path) -> Result<JudgeManifest, Error> {
    let path = judge_v1_dir(eval_root).join("manifest.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| err(format!("read {}: {error}", path.display())))?;
    let manifest: JudgeManifest = serde_json::from_str(&text)
        .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
    if manifest.model != REQUIRED_MODEL {
        return Err(err(format!(
            "judge manifest model must be {REQUIRED_MODEL}, got {}",
            manifest.model
        )));
    }
    if manifest.reasoning != REQUIRED_REASONING {
        return Err(err(format!(
            "judge manifest reasoning must be {REQUIRED_REASONING}, got {}",
            manifest.reasoning
        )));
    }
    if !manifest.fail_closed_on_effort_mismatch || !manifest.no_effort_downgrade {
        return Err(err("judge manifest must fail closed on effort mismatch"));
    }
    let calibration_path = Path::new(&manifest.calibration_schema_file);
    if calibration_path.components().count() != 1
        || !matches!(
            calibration_path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(err("calibration schema path must be a single file name"));
    }
    Ok(manifest)
}

pub fn collect(
    eval_root: &Path,
    capture: &CaptureFile,
    codex_cli_version: &str,
) -> Result<RunIdentity, Error> {
    let judge = judge_v1_dir(eval_root);
    let manifest = load_manifest(eval_root)?;
    let calibration_schema = judge.join(&manifest.calibration_schema_file);
    Ok(RunIdentity {
        baseline: capture.baseline.clone(),
        candidate: capture.candidate.clone(),
        model: REQUIRED_MODEL.to_owned(),
        reasoning: REQUIRED_REASONING.to_owned(),
        codex_cli_version: codex_cli_version.to_owned(),
        prompt_sha256: sha256_file(&judge.join("developer-instructions.md"))?,
        rubric_sha256: sha256_file(&judge.join("rubric.md"))?,
        schema_sha256: sha256_file(&judge.join("result.schema.json"))?,
        calibration_schema_sha256: sha256_file(&calibration_schema)?,
        manifest_sha256: sha256_file(&judge.join("manifest.json"))?,
        corpus_sha256: corpus_fingerprint(&semantic_corpus_dir(eval_root))?,
        aggregation_version: manifest.aggregation_version,
    })
}

fn corpus_fingerprint(root: &Path) -> Result<String, Error> {
    let mut files = Vec::new();
    collect_files(root, &mut files)?;
    files.sort();
    let mut material = String::new();
    for path in files {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        material.push_str(&rel);
        material.push('=');
        material.push_str(&sha256_file(&path)?);
        material.push('\n');
    }
    Ok(sha256_hex(material.as_bytes()))
}

fn collect_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), Error> {
    for entry in
        std::fs::read_dir(dir).map_err(|error| err(format!("read {}: {error}", dir.display())))?
    {
        let entry = entry.map_err(|error| err(format!("walk {}: {error}", dir.display())))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

pub fn refuse_effort_downgrade(requested: &str, actual: &str) -> Result<(), Error> {
    if requested != REQUIRED_REASONING {
        return Err(err(format!(
            "required reasoning is {REQUIRED_REASONING}, requested {requested}"
        )));
    }
    if actual != REQUIRED_REASONING {
        return Err(err(format!(
            "Judge environment invalid: required reasoning {REQUIRED_REASONING}, actual {actual}"
        )));
    }
    Ok(())
}
