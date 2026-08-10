use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use dictc::llm_detail_targets::load_committed_targets;
use dictc::llm_details::{
    build_definition_drafts_jsonl, DraftDefinition, Relations, DRAFT_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

const DRAFT_MANIFEST_SCHEMA_VERSION: &str = "sakura.llm-detail-draft-manifest.v1";

#[derive(Debug)]
struct Options {
    target_directory: PathBuf,
    source_jsonl: Vec<PathBuf>,
    relations_jsonl: Vec<PathBuf>,
    output_directory: PathBuf,
    batch_id: String,
    generator_model: String,
    prompt_version: String,
}

#[derive(Debug, Deserialize)]
struct SourceRecord {
    surface: String,
    reading: String,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    definition: Option<String>,
    #[serde(default)]
    semantic_status: Option<String>,
    #[serde(default)]
    review_state: Option<String>,
    #[serde(default)]
    duplicate_with: Option<serde_json::Value>,
    #[serde(default)]
    relations: Relations,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationRecord {
    surface: String,
    reading: String,
    relations: Relations,
}

#[derive(Debug, Serialize)]
struct DraftManifest<'a> {
    schema_version: &'static str,
    coverage_status: &'static str,
    target_directory: &'a str,
    target_manifest_sha256: String,
    draft_file: &'a str,
    draft_schema_version: &'static str,
    record_count: usize,
    draft_sha256: String,
}

fn main() {
    if let Err(error) = run(env::args_os().skip(1)) {
        eprintln!("llm-detail-drafts: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: impl Iterator<Item = OsString>) -> Result<(), String> {
    let Some(options) = parse_options(arguments)? else {
        return Ok(());
    };
    let targets = load_committed_targets(&options.target_directory)?;
    let target_pairs = targets
        .iter()
        .map(|target| (normalize(&target.surface), normalize(&target.reading)))
        .collect::<BTreeSet<_>>();
    if target_pairs.len() != targets.len() {
        return Err("committed targets contain duplicate normalized pairs".to_owned());
    }

    let mut definitions = BTreeMap::new();
    for source_path in &options.source_jsonl {
        let source = fs::read_to_string(source_path)
            .map_err(|error| format!("read {}: {error}", source_path.display()))?;
        for (zero_based, raw) in source.lines().enumerate() {
            let line = raw.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            let record: SourceRecord = serde_json::from_str(line).map_err(|error| {
                format!(
                    "{}:{}: invalid source JSON: {error}",
                    source_path.display(),
                    zero_based + 1
                )
            })?;
            let pair = (normalize(&record.surface), normalize(&record.reading));
            if !target_pairs.contains(&pair) {
                continue;
            }
            let candidate_ready = record.semantic_status.as_deref() == Some("ready");
            let release_verified = record.review_state.as_deref() == Some("independently_verified");
            if !(candidate_ready || release_verified) || record.duplicate_with.is_some() {
                return Err(format!(
                    "{}:{}: target definition is not an attributable ready record",
                    source_path.display(),
                    zero_based + 1
                ));
            }
            let detail = match (record.detail, record.definition) {
                (Some(detail), None) | (None, Some(detail)) => detail,
                _ => {
                    return Err(format!(
                        "{}:{}: target must have exactly one definition field",
                        source_path.display(),
                        zero_based + 1
                    ));
                }
            };
            let definition = DraftDefinition {
                surface: record.surface,
                reading: record.reading,
                definition: detail,
                relations: record.relations,
            };
            if definitions.insert(pair, definition).is_some() {
                return Err(format!(
                    "{}:{}: duplicate source definition for a target pair",
                    source_path.display(),
                    zero_based + 1
                ));
            }
        }
    }
    let require_relations = !options.relations_jsonl.is_empty();
    let mut overlaid_pairs = BTreeSet::new();
    for source_path in &options.relations_jsonl {
        let source = fs::read_to_string(source_path)
            .map_err(|error| format!("read {}: {error}", source_path.display()))?;
        for (zero_based, raw) in source.lines().enumerate() {
            let line = raw.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            let record: RelationRecord = serde_json::from_str(line).map_err(|error| {
                format!(
                    "{}:{}: invalid relation JSON: {error}",
                    source_path.display(),
                    zero_based + 1
                )
            })?;
            let pair = (normalize(&record.surface), normalize(&record.reading));
            let definition = definitions.get_mut(&pair).ok_or_else(|| {
                format!(
                    "{}:{}: relation pair is not a committed target",
                    source_path.display(),
                    zero_based + 1
                )
            })?;
            if !overlaid_pairs.insert(pair) || !relations_empty(&definition.relations) {
                return Err(format!(
                    "{}:{}: duplicate or conflicting relations for a target pair",
                    source_path.display(),
                    zero_based + 1
                ));
            }
            if relations_empty(&record.relations) {
                return Err(format!(
                    "{}:{}: relation overlay must contain at least one typed relation",
                    source_path.display(),
                    zero_based + 1
                ));
            }
            definition.relations = record.relations;
        }
    }
    if require_relations {
        let missing = definitions
            .values()
            .filter(|definition| relations_empty(&definition.relations))
            .count();
        if missing != 0 {
            return Err(format!(
                "{missing} committed targets still have no reviewed relation"
            ));
        }
    }
    let definitions = definitions.into_values().collect::<Vec<_>>();
    let draft = build_definition_drafts_jsonl(
        &targets,
        &definitions,
        &options.generator_model,
        &options.prompt_version,
    )?;

    fs::create_dir_all(&options.output_directory)
        .map_err(|error| format!("create {}: {error}", options.output_directory.display()))?;
    let draft_name = format!("{}.draft.jsonl", options.batch_id);
    let manifest_name = format!("{}.manifest.json", options.batch_id);
    let draft_path = options.output_directory.join(&draft_name);
    let manifest_path = options.output_directory.join(&manifest_name);
    let target_manifest_path = options.target_directory.join("manifest.json");
    let target_manifest = fs::read(&target_manifest_path)
        .map_err(|error| format!("read {}: {error}", target_manifest_path.display()))?;
    let target_directory_label = options
        .target_directory
        .to_string_lossy()
        .replace('\\', "/");
    let manifest = DraftManifest {
        schema_version: DRAFT_MANIFEST_SCHEMA_VERSION,
        coverage_status: "current_pair_coverage_used_draft_nonrelease",
        target_directory: &target_directory_label,
        target_manifest_sha256: sha256(&target_manifest),
        draft_file: &draft_name,
        draft_schema_version: DRAFT_SCHEMA_VERSION,
        record_count: targets.len(),
        draft_sha256: sha256(&draft),
    };
    let mut manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    manifest_bytes.push(b'\n');

    write_or_verify_uncommitted(&draft_path, &draft)?;
    write_or_verify_manifest(&manifest_path, &manifest_bytes)?;
    println!(
        "wrote {} semantic drafts to {}",
        targets.len(),
        draft_path.display()
    );
    Ok(())
}

fn parse_options(arguments: impl Iterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut target_directory = None;
    let mut source_jsonl = Vec::new();
    let mut relations_jsonl = Vec::new();
    let mut output_directory = None;
    let mut batch_id = None;
    let mut generator_model = None;
    let mut prompt_version = None;
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        let argument = argument
            .into_string()
            .map_err(|_| "arguments must be Unicode".to_owned())?;
        if matches!(argument.as_str(), "-h" | "--help") {
            println!(
                "Usage: llm-detail-drafts --target-dir DIR --source-jsonl FILE [--relations-jsonl FILE] --output-dir DIR --batch-id ID --generator-model MODEL --prompt-version VERSION"
            );
            return Ok(None);
        }
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {argument}"))?;
        match argument.as_str() {
            "--target-dir" => set_once(&mut target_directory, value, &argument)?,
            "--source-jsonl" => source_jsonl.push(PathBuf::from(value)),
            "--relations-jsonl" => relations_jsonl.push(PathBuf::from(value)),
            "--output-dir" => set_once(&mut output_directory, value, &argument)?,
            "--batch-id" => set_once(&mut batch_id, value, &argument)?,
            "--generator-model" => set_once(&mut generator_model, value, &argument)?,
            "--prompt-version" => set_once(&mut prompt_version, value, &argument)?,
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    let batch_id = unicode_value(batch_id, "--batch-id")?;
    if batch_id.is_empty()
        || !batch_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'_')
    {
        return Err("--batch-id must contain only ASCII digits, '-' or '_'".to_owned());
    }
    Ok(Some(Options {
        target_directory: PathBuf::from(required(target_directory, "--target-dir")?),
        source_jsonl: if source_jsonl.is_empty() {
            return Err("missing required argument: --source-jsonl".to_owned());
        } else {
            source_jsonl
        },
        relations_jsonl,
        output_directory: PathBuf::from(required(output_directory, "--output-dir")?),
        batch_id,
        generator_model: unicode_value(generator_model, "--generator-model")?,
        prompt_version: unicode_value(prompt_version, "--prompt-version")?,
    }))
}

fn set_once(slot: &mut Option<OsString>, value: OsString, name: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("duplicate argument: {name}"));
    }
    Ok(())
}

fn required(value: Option<OsString>, name: &str) -> Result<OsString, String> {
    value.ok_or_else(|| format!("missing required argument: {name}"))
}

fn unicode_value(value: Option<OsString>, name: &str) -> Result<String, String> {
    required(value, name)?
        .into_string()
        .map_err(|_| format!("{name} must be Unicode"))
}

fn normalize(value: &str) -> String {
    value.nfc().collect()
}

fn relations_empty(relations: &Relations) -> bool {
    relations.aliases.is_empty()
        && relations.related.is_empty()
        && relations.similar.is_empty()
        && relations.antonyms.is_empty()
}

fn sha256(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("String write");
    }
    output
}

fn write_or_verify_uncommitted(path: &Path, expected: &[u8]) -> Result<(), String> {
    if path.exists() {
        let actual = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        if actual == expected {
            return Ok(());
        }
        return Err(format!(
            "refusing to overwrite mismatched uncommitted draft: {}",
            path.display()
        ));
    }
    write_new(path, expected)
}

fn write_or_verify_manifest(path: &Path, expected: &[u8]) -> Result<(), String> {
    if path.exists() {
        let actual = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        if actual == expected {
            return Ok(());
        }
        return Err(format!(
            "refusing to overwrite mismatched committed draft manifest: {}",
            path.display()
        ));
    }
    write_new(path, expected)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("write {}: {error}", path.display()))
}
