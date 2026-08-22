//! Converts an explicit, opaque-ID approval list into semantic cases.
//!
//! The review TSV is deliberately kept outside the repository. Only the
//! bounded fields selected by the reviewer are copied into the corpus, and
//! the manifest receives a source hash rather than the source contents.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::hash::sha256_hex;
use crate::types::{err, Constraints, Context, Error, Input, SemanticCase};

const REVIEW_HEADER: &str = "case-id\tfamily\tinput-mode\treading\ttyping\tleft-context\tright-context\tfrequency-bucket\tprivacy-provenance";
const REVIEW_PROVENANCE: &str = "local-opt-in-normal-commit-v1";
const MAX_REVIEW_TEXT_CHARS: usize = 96;
const MAX_REVIEW_TYPING_CHARS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewRow {
    case_id: String,
    family: String,
    input_mode: String,
    reading: String,
    typing: String,
    left_context: String,
    right_context: String,
    frequency_bucket: String,
    privacy_provenance: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalReport {
    pub approved_count: usize,
    pub corpus_case_count: usize,
    pub source_sha256: [u8; 32],
}

#[derive(Debug, Deserialize, Serialize)]
struct SemanticManifest {
    schema_version: u32,
    corpus_id: String,
    case_count: usize,
    notes: String,
    #[serde(default)]
    history_derived_count: usize,
    #[serde(default)]
    history_derived_source_sha256: String,
    #[serde(default)]
    history_derived_generation: String,
}

/// Approves only IDs explicitly listed in `approved_ids`.
///
/// The approval list contains opaque IDs, not source text. This prevents an
/// accidental "approve everything in whatever file happens to be present"
/// operation and leaves a deterministic, reviewable boundary before corpus
/// publication.
pub fn approve(
    review_path: &Path,
    approved_ids_path: &Path,
    out_dir: &Path,
    manifest_path: &Path,
) -> Result<ApprovalReport, Error> {
    let review_bytes = fs::read(review_path)
        .map_err(|error| err(format!("read {}: {error}", review_path.display())))?;
    let approval_bytes = fs::read(approved_ids_path)
        .map_err(|error| err(format!("read {}: {error}", approved_ids_path.display())))?;
    let rows = parse_review(std::str::from_utf8(&review_bytes).map_err(|_| {
        err(format!(
            "review file {} is not UTF-8",
            review_path.display()
        ))
    })?)?;
    let approved_ids = parse_approved_ids(std::str::from_utf8(&approval_bytes).map_err(|_| {
        err(format!(
            "approval file {} is not UTF-8",
            approved_ids_path.display()
        ))
    })?)?;
    let by_id = rows
        .into_iter()
        .map(|row| (row.case_id.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let mut selected = Vec::with_capacity(approved_ids.len());
    for case_id in approved_ids {
        let row = by_id
            .get(&case_id)
            .ok_or_else(|| err(format!("approval references unknown case {case_id}")))?;
        selected.push(to_semantic_case(row));
    }
    selected.sort_by(|left, right| left.case_id.cmp(&right.case_id));

    let manifest_text = fs::read_to_string(manifest_path)
        .map_err(|error| err(format!("read {}: {error}", manifest_path.display())))?;
    let mut manifest: SemanticManifest = serde_json::from_str(&manifest_text)
        .map_err(|error| err(format!("parse {}: {error}", manifest_path.display())))?;
    if manifest.schema_version != 1 {
        return Err(err("unsupported semantic corpus manifest schema_version"));
    }

    for case in &selected {
        let family = case
            .family
            .as_deref()
            .expect("history-derived case has a family");
        let category = family
            .strip_prefix("history-")
            .expect("history-derived family has a stable prefix");
        let path = out_dir
            .join(category)
            .join(format!("{}.json", case.case_id));
        write_case(&path, case)?;
    }

    let corpus_case_count = count_case_files(
        manifest_path
            .parent()
            .ok_or_else(|| err("semantic manifest has no parent directory"))?,
    )?;
    let source_sha256 = source_hash(&review_bytes, &approval_bytes);
    manifest.case_count = corpus_case_count;
    manifest.history_derived_count = selected.len();
    manifest.history_derived_source_sha256 = hex_bytes(&source_sha256);
    manifest.history_derived_generation = "history-approval-v1".to_owned();
    let serialized = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| err(format!("serialize {}: {error}", manifest_path.display())))?;
    fs::write(manifest_path, serialized)
        .map_err(|error| err(format!("write {}: {error}", manifest_path.display())))?;

    Ok(ApprovalReport {
        approved_count: selected.len(),
        corpus_case_count,
        source_sha256,
    })
}

fn parse_review(text: &str) -> Result<Vec<ReviewRow>, Error> {
    let mut header_seen = false;
    let mut rows = Vec::new();
    let mut ids = BTreeSet::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !header_seen {
            if line != REVIEW_HEADER {
                return Err(err(format!(
                    "review line {} has an unexpected header",
                    line_number + 1
                )));
            }
            header_seen = true;
            continue;
        }
        let fields = line
            .split('\t')
            .map(unescape_field)
            .collect::<Result<Vec<_>, _>>()?;
        if fields.len() != 9 {
            return Err(err(format!(
                "review line {} has {} fields, expected 9",
                line_number + 1,
                fields.len()
            )));
        }
        let row = ReviewRow {
            case_id: fields[0].clone(),
            family: fields[1].clone(),
            input_mode: fields[2].clone(),
            reading: fields[3].clone(),
            typing: fields[4].clone(),
            left_context: fields[5].clone(),
            right_context: fields[6].clone(),
            frequency_bucket: fields[7].clone(),
            privacy_provenance: fields[8].clone(),
        };
        validate_row(&row)?;
        if !ids.insert(row.case_id.clone()) {
            return Err(err(format!(
                "review contains duplicate case {}",
                row.case_id
            )));
        }
        rows.push(row);
    }
    if !header_seen {
        return Err(err("review is missing its field header"));
    }
    Ok(rows)
}

fn parse_approved_ids(text: &str) -> Result<Vec<String>, Error> {
    let mut ids = BTreeSet::new();
    for (line_number, line) in text.lines().enumerate() {
        let id = line.trim();
        if id.is_empty() || id.starts_with('#') {
            continue;
        }
        validate_case_id(id)?;
        if !ids.insert(id.to_owned()) {
            return Err(err(format!(
                "approval line {} repeats case {}",
                line_number + 1,
                id
            )));
        }
    }
    if ids.is_empty() {
        return Err(err("approval list is empty"));
    }
    Ok(ids.into_iter().collect())
}

fn validate_row(row: &ReviewRow) -> Result<(), Error> {
    validate_case_id(&row.case_id)?;
    if !matches!(
        row.family.as_str(),
        "katakana" | "mixed-romaji" | "normal-conversion" | "technical-terms"
    ) {
        return Err(err(format!("unsupported history family {}", row.family)));
    }
    if !matches!(row.input_mode.as_str(), "romaji" | "kana") {
        return Err(err(format!(
            "unsupported history input mode {}",
            row.input_mode
        )));
    }
    if row.privacy_provenance != REVIEW_PROVENANCE {
        return Err(err(
            "review provenance is not the approved local-history policy",
        ));
    }
    if !matches!(
        row.frequency_bucket.as_str(),
        "rare" | "occasional" | "frequent" | "very-frequent"
    ) {
        return Err(err("review frequency bucket is invalid"));
    }
    validate_text(&row.reading, MAX_REVIEW_TEXT_CHARS, "reading")?;
    validate_text(&row.typing, MAX_REVIEW_TYPING_CHARS, "typing")?;
    validate_optional_context(&row.left_context)?;
    validate_optional_context(&row.right_context)?;
    Ok(())
}

fn validate_case_id(case_id: &str) -> Result<(), Error> {
    let Some(digest) = case_id.strip_prefix("hist-") else {
        return Err(err(format!("history case id is not opaque: {case_id}")));
    };
    if digest.len() != 32 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(err(format!(
            "history case id has an invalid digest: {case_id}"
        )));
    }
    Ok(())
}

fn validate_text(value: &str, max_chars: usize, label: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(|character| character.is_control())
    {
        return Err(err(format!(
            "{label} is empty, too long, or contains controls"
        )));
    }
    if value.contains("://")
        || value.contains('@')
        || value.contains(":\\")
        || value.starts_with("\\\\")
    {
        return Err(err(format!("{label} failed the privacy filter")));
    }
    Ok(())
}

fn validate_optional_context(value: &str) -> Result<(), Error> {
    if value.chars().count() > MAX_REVIEW_TEXT_CHARS
        || value.chars().any(|character| character.is_control())
    {
        return Err(err("history context failed the privacy filter"));
    }
    Ok(())
}

fn unescape_field(value: &str) -> Result<String, Error> {
    let mut output = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            output.push(match character {
                '\\' => '\\',
                't' => '\t',
                'r' => '\r',
                'n' => '\n',
                other => return Err(err(format!("unknown review escape \\{other}"))),
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            output.push(character);
        }
    }
    if escaped {
        return Err(err("review field ends with an escape"));
    }
    Ok(output)
}

fn to_semantic_case(row: &ReviewRow) -> SemanticCase {
    SemanticCase {
        schema_version: 1,
        case_id: row.case_id.clone(),
        task: "conversion".to_owned(),
        family: Some(format!("history-{}", row.family)),
        role: Some("history-review-approved".to_owned()),
        context: Context {
            left: row.left_context.clone(),
            right: row.right_context.clone(),
        },
        input: Input {
            input_mode: Some(row.input_mode.clone()),
            reading: row.reading.clone(),
            typing: Some(row.typing.clone()),
        },
        constraints: Constraints::default(),
        privacy_provenance: Some(row.privacy_provenance.clone()),
    }
}

fn write_case(path: &Path, case: &SemanticCase) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
    }
    let bytes = serde_json::to_vec_pretty(case)
        .map_err(|error| err(format!("serialize {}: {error}", path.display())))?;
    if path.exists() {
        let existing =
            fs::read(path).map_err(|error| err(format!("read {}: {error}", path.display())))?;
        if existing != bytes {
            return Err(err(format!(
                "refusing to overwrite an existing history case {}",
                path.display()
            )));
        }
        return Ok(());
    }
    fs::write(path, bytes).map_err(|error| err(format!("write {}: {error}", path.display())))
}

fn count_case_files(root: &Path) -> Result<usize, Error> {
    let mut count = 0usize;
    count_case_files_recursive(root, &mut count)?;
    Ok(count)
}

fn count_case_files_recursive(root: &Path, count: &mut usize) -> Result<(), Error> {
    let entries =
        fs::read_dir(root).map_err(|error| err(format!("read {}: {error}", root.display())))?;
    for entry in entries {
        let path = entry
            .map_err(|error| err(format!("walk {}: {error}", root.display())))?
            .path();
        if path.is_dir() {
            count_case_files_recursive(&path, count)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("json")
            && path.file_name().and_then(|name| name.to_str()) != Some("manifest.json")
        {
            *count += 1;
        }
    }
    Ok(())
}

fn source_hash(review: &[u8], approval: &[u8]) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(review.len() + approval.len() + 1);
    bytes.extend_from_slice(review);
    bytes.push(0);
    bytes.extend_from_slice(approval);
    let digest = sha256_hex(&bytes);
    let mut output = [0u8; 32];
    for (index, pair) in digest.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_value(pair[0]) << 4) | hex_value(pair[1]);
    }
    output
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("sha256_hex only emits lowercase hexadecimal"),
    }
}

fn hex_bytes(bytes: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review_line(case_id: &str, reading: &str) -> String {
        format!(
            "{case_id}\tnormal-conversion\tromaji\t{reading}\tkyou\t\t\tfrequent\t{REVIEW_PROVENANCE}\n"
        )
    }

    #[test]
    fn approval_parser_requires_opaque_ids_and_provenance() {
        let text = format!(
            "# header\n{REVIEW_HEADER}\n{}",
            review_line("hist-00000000000000000000000000000001", "きょう")
        );
        let rows = parse_review(&text).expect("review");
        assert_eq!(rows.len(), 1);
        assert!(parse_approved_ids("hist-not-an-id\n").is_err());
    }

    #[test]
    fn source_hash_is_stable_and_separated_from_raw_fields() {
        let first = source_hash(b"reading-a", b"hist-00000000000000000000000000000001\n");
        let second = source_hash(b"reading-a", b"hist-00000000000000000000000000000001\n");
        assert_eq!(first, second);
        assert_ne!(
            first,
            source_hash(b"reading-b", b"hist-00000000000000000000000000000001\n")
        );
    }

    #[test]
    fn approval_publishes_minimal_case_and_manifest_metadata() {
        let root = std::env::temp_dir().join(format!(
            "sakura-ime-history-approval-{}",
            std::process::id()
        ));
        let semantic = root.join("semantic");
        let out_dir = semantic.join("history-derived");
        let review = root.join("review.tsv");
        let approved = root.join("approved.ids");
        let manifest = semantic.join("manifest.json");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&semantic).expect("create semantic root");
        fs::write(
            &review,
            format!(
                "# format\n{REVIEW_HEADER}\n{}",
                review_line("hist-00000000000000000000000000000001", "きょう")
            ),
        )
        .expect("write review");
        fs::write(&approved, "hist-00000000000000000000000000000001\n").expect("write approval");
        fs::write(
            &manifest,
            r#"{
  "schema_version": 1,
  "corpus_id": "test",
  "case_count": 0,
  "notes": "test"
}"#,
        )
        .expect("write manifest");

        let report = approve(&review, &approved, &out_dir, &manifest).expect("approve");
        assert_eq!(report.approved_count, 1);
        assert_eq!(report.corpus_case_count, 1);
        let case = fs::read_to_string(
            out_dir
                .join("normal-conversion")
                .join("hist-00000000000000000000000000000001.json"),
        )
        .expect("read case");
        assert!(case.contains("\"privacy_provenance\""));
        assert!(!case.contains("\"surface\""));
        assert!(!case.contains("\"session\""));
        let manifest_text = fs::read_to_string(&manifest).expect("read manifest");
        assert!(manifest_text.contains("\"history_derived_count\": 1"));
        let _ = fs::remove_dir_all(root);
    }
}
