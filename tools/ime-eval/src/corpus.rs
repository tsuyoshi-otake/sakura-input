use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::types::{err, Error, SemanticCase};

#[derive(Debug, Deserialize)]
struct CorpusManifest {
    schema_version: u32,
    case_count: usize,
}

pub fn load_semantic_corpus(root: &Path) -> Result<Vec<SemanticCase>, Error> {
    let mut files = Vec::new();
    collect_json_files(root, &mut files)?;
    files.sort();
    let mut cases = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path)
            .map_err(|error| err(format!("read {}: {error}", path.display())))?;
        let parsed: SemanticCase = serde_json::from_str(&text)
            .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
        if parsed.schema_version != 1 {
            return Err(err(format!(
                "{}: unsupported schema_version {}",
                path.display(),
                parsed.schema_version
            )));
        }
        if parsed.case_id.is_empty() {
            return Err(err(format!("{}: empty case_id", path.display())));
        }
        if parsed.case_id.starts_with("hist-") {
            if !parsed
                .family
                .as_deref()
                .is_some_and(|family| family.starts_with("history-"))
            {
                return Err(err(format!(
                    "{}: history-derived case has no history family",
                    path.display()
                )));
            }
            if parsed.privacy_provenance.as_deref() != Some("local-opt-in-normal-commit-v1") {
                return Err(err(format!(
                    "{}: history-derived case has invalid privacy provenance",
                    path.display()
                )));
            }
            if parsed.input.typing.as_deref().is_none_or(str::is_empty) {
                return Err(err(format!(
                    "{}: history-derived case has no typing sequence",
                    path.display()
                )));
            }
        }
        cases.push(parsed);
    }
    let manifest_path = root.join("manifest.json");
    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|error| err(format!("read {}: {error}", manifest_path.display())))?;
    let manifest: CorpusManifest = serde_json::from_str(&manifest_text)
        .map_err(|error| err(format!("parse {}: {error}", manifest_path.display())))?;
    if manifest.schema_version != 1 {
        return Err(err(format!(
            "unsupported semantic corpus manifest schema_version {}",
            manifest.schema_version
        )));
    }
    if manifest.case_count != cases.len() {
        return Err(err(format!(
            "semantic corpus manifest expects {} cases, found {}",
            manifest.case_count,
            cases.len()
        )));
    }
    Ok(cases)
}

pub fn load_case_map(
    root: &Path,
) -> Result<std::collections::BTreeMap<String, SemanticCase>, Error> {
    let mut map = std::collections::BTreeMap::new();
    for case in load_semantic_corpus(root)? {
        if map.insert(case.case_id.clone(), case).is_some() {
            return Err(err("duplicate semantic case_id"));
        }
    }
    Ok(map)
}

fn collect_json_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let entries =
        fs::read_dir(dir).map_err(|error| err(format!("read {}: {error}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|error| err(format!("walk {}: {error}", dir.display())))?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, out)?;
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("manifest.json") {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}
