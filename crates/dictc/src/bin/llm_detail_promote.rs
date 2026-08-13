use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use dictc::llm_detail_targets::{load_committed_targets, TARGET_SCHEMA_VERSION};
use dictc::llm_details::{
    canonical_review_fingerprint, import_release_jsonl, load_committed_release_jsonl,
    parse_drafts_jsonl, promote_drafts, promotion_report_json, IndependentReview, ReviewDecision,
    RELEASE_MANIFEST_SCHEMA_VERSION, SCHEMA_VERSION,
};
use dictc::{parse_category_entries, SourceDetail, SourceEntry};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug)]
struct Options {
    target_directory: PathBuf,
    draft_file: PathBuf,
    review_file: PathBuf,
    category_files: Vec<PathBuf>,
    coverage_files: Vec<PathBuf>,
    output_directory: PathBuf,
    reviewer_model: String,
    review_prompt_version: String,
}

#[derive(Debug)]
struct Decision {
    status: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct ReleaseManifest {
    schema_version: &'static str,
    target_manifest_schema_version: &'static str,
    target_manifest_sha256: String,
    target_batch_count: usize,
    batches: Vec<ReleaseBatch>,
}

#[derive(Debug, Serialize)]
struct ReleaseBatch {
    batch_index: usize,
    file: String,
    record_count: usize,
    sha256: String,
    target_hashes: Vec<String>,
    generation_fingerprints: Vec<String>,
}

fn main() {
    if let Err(error) = run(env::args_os().skip(1)) {
        eprintln!("llm-detail-promote: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: impl Iterator<Item = OsString>) -> Result<(), String> {
    let Some(options) = parse_options(arguments)? else {
        return Ok(());
    };
    let targets = load_committed_targets(&options.target_directory)?;
    let entries = load_entries(&options.category_files)?;
    let existing = load_coverage(&options.coverage_files)?;
    let draft_text = fs::read_to_string(&options.draft_file)
        .map_err(|error| format!("read {}: {error}", options.draft_file.display()))?;
    let drafts = parse_drafts_jsonl(
        &options.draft_file.display().to_string(),
        &draft_text,
        &targets,
        &entries,
    )
    .map_err(|error| error.to_string())?;
    let fingerprints = draft_fingerprints(&draft_text)?;
    let decisions = load_decisions(&options.review_file)?;
    if decisions.len() != targets.len() {
        return Err("review decision count does not match committed targets".to_owned());
    }

    let mut reviews = Vec::with_capacity(targets.len());
    for target in &targets {
        let pair = (target.surface.clone(), target.reading.clone());
        let decision = decisions
            .get(&pair)
            .ok_or_else(|| "committed target is missing a review decision".to_owned())?;
        let draft_generation_fingerprint = fingerprints
            .get(&target.target_hash)
            .ok_or_else(|| "committed target is missing from the draft artifact".to_owned())?
            .clone();
        let decision = match decision.status.as_str() {
            "approved" if decision.reason.is_empty() => ReviewDecision::Approved {
                model: options.reviewer_model.clone(),
                prompt_version: options.review_prompt_version.clone(),
                schema_version: SCHEMA_VERSION.to_owned(),
            },
            "held" if !decision.reason.is_empty() => ReviewDecision::Held {
                reason: decision.reason.clone(),
            },
            "rejected" if !decision.reason.is_empty() => ReviewDecision::Rejected {
                reason: decision.reason.clone(),
            },
            _ => return Err("review status and reason are inconsistent".to_owned()),
        };
        let mut review = IndependentReview {
            target_hash: target.target_hash.clone(),
            draft_generation_fingerprint,
            review_fingerprint: String::new(),
            decision,
        };
        review.review_fingerprint = canonical_review_fingerprint(&review);
        reviews.push(review);
    }
    let promotion = promote_drafts(&drafts.drafts, &reviews, &targets, &entries, &existing)
        .map_err(|error| error.to_string())?;
    let release_bytes = promotion.release_jsonl.as_bytes();
    let (target_hashes, generation_fingerprints) = release_bindings(&promotion.release_jsonl)?;
    let release_file = "000001.release.jsonl";
    let target_manifest = fs::read(options.target_directory.join("manifest.json"))
        .map_err(|error| format!("read target manifest: {error}"))?;
    let manifest = ReleaseManifest {
        schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
        target_manifest_schema_version: TARGET_SCHEMA_VERSION,
        target_manifest_sha256: sha256(&target_manifest),
        target_batch_count: 1,
        batches: vec![ReleaseBatch {
            batch_index: 1,
            file: release_file.to_owned(),
            record_count: target_hashes.len(),
            sha256: sha256(release_bytes),
            target_hashes,
            generation_fingerprints,
        }],
    };
    let mut manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    manifest_bytes.push(b'\n');
    let audit_bytes = promotion_report_json(&promotion).into_bytes();

    fs::create_dir_all(&options.output_directory)
        .map_err(|error| format!("create {}: {error}", options.output_directory.display()))?;
    write_or_verify(
        &options.output_directory.join(release_file),
        release_bytes,
        "release batch",
    )?;
    write_or_verify(
        &options.output_directory.join("promotion-audit.json"),
        &audit_bytes,
        "promotion audit",
    )?;
    write_or_verify(
        &options.output_directory.join("manifest.json"),
        &manifest_bytes,
        "release manifest",
    )?;

    let committed = load_committed_release_jsonl(
        &options.output_directory,
        &options.target_directory,
        &targets,
    )?;
    let imported = import_release_jsonl(
        &options.output_directory.display().to_string(),
        &committed,
        &targets,
        &entries,
        &existing,
    )
    .map_err(|error| error.to_string())?;
    if imported.report.validated_unique_terms != promotion.report.approved
        || imported.report.suppressed_by_existing_pair != 0
        || imported.report.suppressed_by_curated != 0
    {
        return Err("committed release does not reproduce the promotion result".to_owned());
    }
    println!(
        "reviewed {}, approved {}, held {}, rejected {}; emitted {} exact details",
        promotion.report.reviewed,
        promotion.report.approved,
        promotion.report.held,
        promotion.report.rejected,
        imported.report.emitted_details
    );
    Ok(())
}

fn load_entries(paths: &[PathBuf]) -> Result<Vec<SourceEntry>, String> {
    let mut entries = Vec::new();
    for path in paths {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        entries.extend(
            parse_category_entries(&path.display().to_string(), &text)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(entries)
}

fn load_coverage(paths: &[PathBuf]) -> Result<Vec<SourceDetail>, String> {
    let mut details = Vec::new();
    for path in paths {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        for (zero_based, raw) in text.lines().enumerate() {
            let line = raw.trim_end_matches('\r');
            if zero_based == 0 && line == "reading\tsurface\tleft_id\tright_id" {
                continue;
            }
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() != 4 || fields[0].is_empty() || fields[1].is_empty() {
                return Err(format!(
                    "{}:{}: malformed detail coverage",
                    path.display(),
                    zero_based + 1
                ));
            }
            details.push(SourceDetail {
                reading: fields[0].to_owned(),
                surface: fields[1].to_owned(),
                left_id: fields[2].parse().map_err(|_| {
                    format!("{}:{}: invalid left_id", path.display(), zero_based + 1)
                })?,
                right_id: fields[3].parse().map_err(|_| {
                    format!("{}:{}: invalid right_id", path.display(), zero_based + 1)
                })?,
                description: "coverage-only promotion recheck".to_owned(),
                relations: Vec::new(),
            });
        }
    }
    Ok(details)
}

fn load_decisions(path: &Path) -> Result<BTreeMap<(String, String), Decision>, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut decisions = BTreeMap::new();
    for (zero_based, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if zero_based == 0 && line == "surface\treading\tstatus\treason" {
            continue;
        }
        // A complete split, so a fifth column fails the count check below
        // instead of disappearing into the free-text reason.
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 || fields[0].is_empty() || fields[1].is_empty() {
            return Err(format!(
                "{}:{}: malformed review decision",
                path.display(),
                zero_based + 1
            ));
        }
        if decisions
            .insert(
                (fields[0].to_owned(), fields[1].to_owned()),
                Decision {
                    status: fields[2].to_owned(),
                    reason: fields[3].to_owned(),
                },
            )
            .is_some()
        {
            return Err(format!(
                "{}:{}: duplicate review pair",
                path.display(),
                zero_based + 1
            ));
        }
    }
    Ok(decisions)
}

fn draft_fingerprints(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut output = BTreeMap::new();
    for (zero_based, line) in text.lines().enumerate() {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("draft line {}: {error}", zero_based + 1))?;
        let target_hash = json_string(&value, "target_hash", zero_based + 1)?;
        let fingerprint = json_string(&value, "generation_fingerprint", zero_based + 1)?;
        if output.insert(target_hash, fingerprint).is_some() {
            return Err("draft contains duplicate target hashes".to_owned());
        }
    }
    Ok(output)
}

fn release_bindings(text: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let mut targets = Vec::new();
    let mut fingerprints = Vec::new();
    for (zero_based, line) in text.lines().enumerate() {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("release line {}: {error}", zero_based + 1))?;
        targets.push(json_string(&value, "target_hash", zero_based + 1)?);
        fingerprints.push(json_string(
            &value,
            "generation_fingerprint",
            zero_based + 1,
        )?);
    }
    Ok((targets, fingerprints))
}

fn json_string(value: &serde_json::Value, field: &str, line: usize) -> Result<String, String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("line {line}: missing {field}"))
}

fn parse_options(arguments: impl Iterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut target_directory = None;
    let mut draft_file = None;
    let mut review_file = None;
    let mut category_files = Vec::new();
    let mut coverage_files = Vec::new();
    let mut output_directory = None;
    let mut reviewer_model = None;
    let mut review_prompt_version = None;
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        let argument = argument
            .into_string()
            .map_err(|_| "arguments must be Unicode".to_owned())?;
        if matches!(argument.as_str(), "-h" | "--help") {
            println!(
                "Usage: llm-detail-promote --target-dir DIR --draft-file FILE --review-file FILE \\\n+                 --category FILE... --coverage FILE... --output-dir DIR \\\n+                 --reviewer-model MODEL --review-prompt-version VERSION"
            );
            return Ok(None);
        }
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {argument}"))?;
        match argument.as_str() {
            "--target-dir" => set_once(&mut target_directory, value, &argument)?,
            "--draft-file" => set_once(&mut draft_file, value, &argument)?,
            "--review-file" => set_once(&mut review_file, value, &argument)?,
            "--category" => category_files.push(PathBuf::from(value)),
            "--coverage" => coverage_files.push(PathBuf::from(value)),
            "--output-dir" => set_once(&mut output_directory, value, &argument)?,
            "--reviewer-model" => set_once(&mut reviewer_model, value, &argument)?,
            "--review-prompt-version" => set_once(&mut review_prompt_version, value, &argument)?,
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    if category_files.is_empty() || coverage_files.is_empty() {
        return Err("at least one --category and --coverage are required".to_owned());
    }
    Ok(Some(Options {
        target_directory: PathBuf::from(required(target_directory, "--target-dir")?),
        draft_file: PathBuf::from(required(draft_file, "--draft-file")?),
        review_file: PathBuf::from(required(review_file, "--review-file")?),
        category_files,
        coverage_files,
        output_directory: PathBuf::from(required(output_directory, "--output-dir")?),
        reviewer_model: unicode_value(reviewer_model, "--reviewer-model")?,
        review_prompt_version: unicode_value(review_prompt_version, "--review-prompt-version")?,
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

fn sha256(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("String write");
    }
    output
}

fn write_or_verify(path: &Path, expected: &[u8], label: &str) -> Result<(), String> {
    if path.exists() {
        let actual = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        if actual == expected {
            return Ok(());
        }
        return Err(format!(
            "refusing to overwrite mismatched committed {label}: {}",
            path.display()
        ));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    file.write_all(expected)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("write {}: {error}", path.display()))
}
