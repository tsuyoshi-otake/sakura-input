//! Offline, deterministic selection and batching for LLM-authored details.
//!
//! This module never writes a definition.  It only produces hash-pinned target
//! records for a separately reviewed generation pass.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::category::DictionaryCategory;
use crate::{SourceDetail, SourceEntry};
use sakura_core::dictionary::EntryFlags;

pub const TARGET_SCHEMA_VERSION: &str = "sakura.llm-detail-target.v1";
pub const MANIFEST_SCHEMA_VERSION: &str = "sakura.llm-detail-target-manifest.v2";

/// Exact entry fields used by the importer to reject stale or cross-dictionary work.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryIdentity {
    pub left_id: u16,
    pub right_id: u16,
    pub word_cost: i32,
    pub prediction_cost: i32,
    pub flags: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DictionaryIdentity {
    pub entries: Vec<EntryIdentity>,
}

/// One definition-free JSONL request. `input_hash` covers every other field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub schema_version: String,
    pub surface: String,
    pub reading: String,
    pub category_ids: Vec<u8>,
    pub dictionary_identity: DictionaryIdentity,
    pub target_hash: String,
    pub input_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CoveredIdentity {
    pub reading: String,
    pub surface: String,
    pub left_id: u16,
    pub right_id: u16,
}

/// Turns an annotated Sakura TSV layer into exact existing-detail coverage.
/// Blank annotations are lexical candidates, not descriptions, and must stay
/// eligible. Every non-blank annotation is a source-owned description for the
/// exact ordinal identity it accompanies.
pub fn coverage_from_annotated_entries(
    entries: impl IntoIterator<Item = SourceEntry>,
) -> BTreeSet<CoveredIdentity> {
    entries
        .into_iter()
        .filter(|entry| !entry.annotation.trim().is_empty())
        .map(|entry| CoveredIdentity {
            reading: normalize(&entry.reading),
            surface: normalize(&entry.surface),
            left_id: entry.left_id,
            right_id: entry.right_id,
        })
        .collect()
}

/// Serializes the exact identities that already have source-backed details.
/// The output is deliberately small and stable so it can be fed straight back
/// to `--coverage` for a later target-selection run.
pub fn details_coverage_tsv(details: &[SourceDetail]) -> Result<Vec<u8>, String> {
    let mut identities = BTreeSet::new();
    for detail in details {
        let reading = normalize(&detail.reading);
        let surface = normalize(&detail.surface);
        if reading.is_empty()
            || surface.is_empty()
            || reading != detail.reading
            || surface != detail.surface
            || reading.contains(['\t', '\r', '\n'])
            || surface.contains(['\t', '\r', '\n'])
        {
            return Err("detail coverage contains a non-canonical TSV identity".into());
        }
        identities.insert((reading, surface, detail.left_id, detail.right_id));
    }
    let mut output = b"reading\tsurface\tleft_id\tright_id\n".to_vec();
    for (reading, surface, left_id, right_id) in identities {
        writeln!(output, "{reading}\t{surface}\t{left_id}\t{right_id}").expect("Vec write");
    }
    Ok(output)
}

#[derive(Debug, Clone)]
pub struct CategorizedSourceEntry {
    pub entry: SourceEntry,
    pub category: DictionaryCategory,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterCounts {
    pub input_entries: usize,
    pub duplicate_identities: usize,
    pub already_detailed: usize,
    pub already_detailed_pairs: usize,
    pub non_lexical: usize,
    pub proper_name: usize,
    pub spelling_or_variant: usize,
    pub abnormal_length: usize,
    pub reading_mismatch: usize,
    pub ambiguous_surface: usize,
    pub selected_targets: usize,
    pub held_targets: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldTarget {
    pub surface: String,
    pub reading: String,
    pub reason: String,
    pub category_ids: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub targets: Vec<Target>,
    pub held: Vec<HeldTarget>,
    pub counts: FilterCounts,
    /// Safe candidates before a caller optionally creates a smaller review batch.
    pub eligible_targets_before_limit: usize,
    /// `Some(n)` proves a committed manifest intentionally contains only the
    /// first `n` deterministic candidates after all inputs were examined.
    pub target_limit: Option<usize>,
    pub allowlist_sha256: Option<String>,
}

/// Select unique normalized `(surface, reading)` pairs, but retain all exact
/// entry identities that still need a detail under that target.
pub fn select_targets(
    entries: impl IntoIterator<Item = CategorizedSourceEntry>,
    covered: &BTreeSet<CoveredIdentity>,
) -> Selection {
    let entries = entries.into_iter().collect::<Vec<_>>();
    let mut counts = FilterCounts {
        input_entries: entries.len(),
        ..FilterCounts::default()
    };
    let mut readings_by_surface = BTreeMap::<String, BTreeSet<String>>::new();
    let covered_pairs = covered
        .iter()
        .map(|item| (item.surface.clone(), item.reading.clone()))
        .collect::<BTreeSet<_>>();
    for item in &entries {
        readings_by_surface
            .entry(normalize(&item.entry.surface))
            .or_default()
            .insert(normalize(&item.entry.reading));
    }

    let mut groups = BTreeMap::<(String, String), BTreeMap<EntryIdentity, BTreeSet<u8>>>::new();
    let mut held = BTreeMap::<(String, String, String), BTreeSet<u8>>::new();
    for item in entries {
        let reading = normalize(&item.entry.reading);
        let surface = normalize(&item.entry.surface);
        let reason = reject_reason(&reading, &surface, item.category, item.entry.flags)
            .or_else(|| (readings_by_surface[&surface].len() > 1).then_some("ambiguous_surface"));
        if let Some(reason) = reason {
            increment_reason(&mut counts, reason);
            held.entry((surface, reading, reason.to_owned()))
                .or_default()
                .insert(item.category.id());
            continue;
        }
        if covered_pairs.contains(&(surface.clone(), reading.clone())) {
            counts.already_detailed_pairs += 1;
            continue;
        }
        if covered.contains(&CoveredIdentity {
            reading: reading.clone(),
            surface: surface.clone(),
            left_id: item.entry.left_id,
            right_id: item.entry.right_id,
        }) {
            counts.already_detailed += 1;
            continue;
        }
        let identity = EntryIdentity {
            left_id: item.entry.left_id,
            right_id: item.entry.right_id,
            word_cost: item.entry.word_cost,
            prediction_cost: item.entry.prediction_cost,
            flags: item.entry.flags.bits(),
        };
        let group = groups.entry((reading, surface)).or_default();
        if group.contains_key(&identity) {
            counts.duplicate_identities += 1;
        }
        group
            .entry(identity)
            .or_default()
            .insert(item.category.id());
    }

    let mut targets = groups
        .into_iter()
        .map(|((reading, surface), entries)| {
            make_target(reading, surface, entries.into_iter().collect())
        })
        .collect::<Vec<_>>();
    targets.sort_by_key(rank_key);
    counts.selected_targets = targets.len();
    let held = held
        .into_iter()
        .map(|((surface, reading, reason), categories)| HeldTarget {
            surface,
            reading,
            reason,
            category_ids: categories.into_iter().collect(),
        })
        .collect::<Vec<_>>();
    counts.held_targets = held.len();
    let eligible_targets_before_limit = targets.len();
    Selection {
        targets,
        held,
        counts,
        eligible_targets_before_limit,
        target_limit: None,
        allowlist_sha256: None,
    }
}

fn make_target(
    reading: String,
    surface: String,
    entries: Vec<(EntryIdentity, BTreeSet<u8>)>,
) -> Target {
    let category_ids = entries
        .iter()
        .flat_map(|(_, categories)| categories.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut target = Target {
        schema_version: TARGET_SCHEMA_VERSION.into(),
        surface,
        reading,
        category_ids,
        dictionary_identity: DictionaryIdentity {
            entries: entries.into_iter().map(|(entry, _)| entry).collect(),
        },
        target_hash: String::new(),
        input_hash: String::new(),
    };
    target.target_hash = target_hash(&target);
    target.input_hash = input_hash(&target);
    target
}

/// Canonical identity hash, independent of batch order and prompt version.
pub fn target_hash(target: &Target) -> String {
    let mut canonical = String::new();
    canonical_component(&mut canonical, TARGET_SCHEMA_VERSION);
    canonical_component(&mut canonical, &target.reading);
    canonical_component(&mut canonical, &target.surface);
    for entry in &target.dictionary_identity.entries {
        write!(
            canonical,
            "{}:{}:{}:{}:{};",
            entry.left_id, entry.right_id, entry.word_cost, entry.prediction_cost, entry.flags,
        )
        .expect("String write");
    }
    sha256_hex(canonical.as_bytes())
}

/// Hash of the canonical request JSON excluding the self-referential `input_hash`.
pub fn input_hash(target: &Target) -> String {
    sha256_hex(&canonical_target_json(target, false).expect("Target serialization is infallible"))
}

pub fn targets_jsonl(targets: &[Target]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for target in targets {
        validate_target(target)?;
        out.extend(canonical_target_json(target, true)?);
        out.push(b'\n');
    }
    Ok(out)
}

/// Strict parser shared with the importer; all hashes and normalized invariants
/// are recomputed before a target becomes usable.
pub fn parse_targets_jsonl(text: &str) -> Result<Vec<Target>, String> {
    let mut targets = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let target: Target = serde_json::from_str(line)
            .map_err(|error| format!("target line {}: invalid JSON: {error}", index + 1))?;
        validate_target(&target).map_err(|error| format!("target line {}: {error}", index + 1))?;
        targets.push(target);
    }
    Ok(targets)
}

pub fn parse_coverage_tsv(source: &str, text: &str) -> Result<BTreeSet<CoveredIdentity>, String> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header = lines
        .next()
        .ok_or_else(|| format!("{source}: missing coverage TSV header"))?
        .trim_end_matches('\r');
    if header != "reading\tsurface\tleft_id\tright_id" {
        return Err(format!("{source}: unexpected coverage TSV header"));
    }
    let mut output = BTreeSet::new();
    for (zero_based, raw) in lines.enumerate() {
        let fields = raw.trim_end_matches('\r').split('\t').collect::<Vec<_>>();
        if fields.len() != 4 || fields.iter().any(|field| field.is_empty()) {
            return Err(format!(
                "{source}: line {} must contain four non-empty columns",
                zero_based + 2
            ));
        }
        output.insert(CoveredIdentity {
            reading: normalize(fields[0]),
            surface: normalize(fields[1]),
            left_id: fields[2]
                .parse()
                .map_err(|_| format!("{source}: line {} has invalid left_id", zero_based + 2))?,
            right_id: fields[3]
                .parse()
                .map_err(|_| format!("{source}: line {} has invalid right_id", zero_based + 2))?,
        });
    }
    Ok(output)
}

/// Strict, review-authored subset keys.  Entries must already be in the safe
/// selection; this parser never turns an unknown or held pair into a target.
pub fn parse_allowlist_tsv(source: &str, text: &str) -> Result<BTreeSet<(String, String)>, String> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header = lines
        .next()
        .ok_or_else(|| format!("{source}: missing allowlist header"))?
        .trim_end_matches('\r');
    if header != "surface\treading" {
        return Err(format!("{source}: expected surface<TAB>reading header"));
    }
    let mut output = BTreeSet::new();
    for (index, line) in lines.enumerate() {
        let fields = line.trim_end_matches('\r').split('\t').collect::<Vec<_>>();
        if fields.len() != 2
            || fields
                .iter()
                .any(|field| field.is_empty() || normalize(field) != *field)
        {
            return Err(format!(
                "{source}: line {} must contain two non-empty NFC columns",
                index + 2
            ));
        }
        if !output.insert((fields[0].to_owned(), fields[1].to_owned())) {
            return Err(format!(
                "{source}: duplicate allowlist pair at line {}",
                index + 2
            ));
        }
    }
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchManifest {
    schema_version: String,
    target_schema_version: String,
    prompt_version: String,
    batch_size: usize,
    target_count: usize,
    eligible_targets_before_limit: usize,
    target_limit: Option<usize>,
    allowlist_sha256: Option<String>,
    source_sha256: BTreeMap<String, String>,
    filter_counts: FilterCounts,
    /// Audit-only evidence for held candidates.  The held corpus is deliberately
    /// not materialized in a committed subset directory: it may be enormous and
    /// is not an importer input.
    held_audit: HeldAudit,
    batches: Vec<BatchEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeldAudit {
    record_count: usize,
    sha256: String,
    materialization: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchEntry {
    batch_index: usize,
    file: String,
    record_count: usize,
    input_sha256: String,
    input_hashes: Vec<String>,
}

/// Writes immutable batch inputs, then commits the directory by creating its
/// manifest last. Existing committed directories are verified and never
/// overwritten. A partially written directory has no manifest and is resumed
/// only when every existing managed file exactly matches this invocation.
pub fn write_batches(
    output_directory: &Path,
    selection: &Selection,
    batch_size: usize,
    prompt_version: &str,
    source_sha256: &BTreeMap<String, String>,
) -> Result<(), String> {
    if batch_size == 0 {
        return Err("batch_size must be greater than zero".into());
    }
    if prompt_version.trim().is_empty() {
        return Err("prompt_version must not be empty".into());
    }
    fs::create_dir_all(output_directory)
        .map_err(|error| format!("create {}: {error}", output_directory.display()))?;
    let mut batches = Vec::new();
    let mut planned = BTreeMap::new();
    for (zero_based, group) in selection.targets.chunks(batch_size).enumerate() {
        let batch_index = zero_based + 1;
        let file = format!("{batch_index:06}.targets.jsonl");
        let bytes = targets_jsonl(group)?;
        planned.insert(file.clone(), bytes.clone());
        batches.push(BatchEntry {
            batch_index,
            file,
            record_count: group.len(),
            input_sha256: sha256_hex(&bytes),
            input_hashes: group
                .iter()
                .map(|target| target.input_hash.clone())
                .collect(),
        });
    }
    let held_audit = held_audit(&selection.held)?;
    let manifest = BatchManifest {
        schema_version: MANIFEST_SCHEMA_VERSION.into(),
        target_schema_version: TARGET_SCHEMA_VERSION.into(),
        prompt_version: prompt_version.into(),
        batch_size,
        target_count: selection.targets.len(),
        eligible_targets_before_limit: selection.eligible_targets_before_limit,
        target_limit: selection.target_limit,
        allowlist_sha256: selection.allowlist_sha256.clone(),
        source_sha256: source_sha256.clone(),
        filter_counts: selection.counts,
        held_audit,
        batches,
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let manifest_path = output_directory.join("manifest.json");
    if manifest_path.exists() {
        verify_committed_batches(output_directory)?;
        let existing = fs::read(&manifest_path)
            .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
        if existing == bytes {
            return Ok(());
        }
        return Err(format!(
            "{} is already committed for another batch plan; use a new output directory",
            output_directory.display()
        ));
    }
    validate_uncommitted_files(output_directory, &planned)?;
    for (file, content) in &planned {
        let path = output_directory.join(file);
        if !path.exists() {
            write_new(&path, content)?;
        }
    }
    // The manifest is the only commit marker consumers may trust. If creation
    // fails, the recoverable directory remains explicitly uncommitted.
    write_new(&manifest_path, &bytes)
}

/// Validates the committed manifest against every listed immutable JSONL batch.
/// Consumers must call this before globbing or importing any batch file.
pub fn verify_committed_batches(output_directory: &Path) -> Result<(), String> {
    let path = output_directory.join("manifest.json");
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let manifest: BatchManifest = serde_json::from_str(&text)
        .map_err(|error| format!("{}: invalid target manifest: {error}", path.display()))?;
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION
        || manifest.target_schema_version != TARGET_SCHEMA_VERSION
    {
        return Err(format!(
            "{}: unsupported target manifest schema",
            path.display()
        ));
    }
    let mut total = 0usize;
    let mut expected = BTreeSet::new();
    let mut target_hashes = BTreeSet::new();
    for (zero_based, batch) in manifest.batches.iter().enumerate() {
        let expected_name = format!("{:06}.targets.jsonl", zero_based + 1);
        if batch.batch_index != zero_based + 1
            || batch.file != expected_name
            || !expected.insert(batch.file.clone())
        {
            return Err(format!(
                "{}: invalid or duplicate batch index",
                path.display()
            ));
        }
        let bytes = fs::read(output_directory.join(&batch.file))
            .map_err(|error| format!("read batch {}: {error}", batch.file))?;
        if sha256_hex(&bytes) != batch.input_sha256 {
            return Err(format!("batch {} hash does not match manifest", batch.file));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| format!("batch {} is not UTF-8", batch.file))?;
        let targets = parse_targets_jsonl(text)?;
        if targets
            .iter()
            .any(|target| !target_hashes.insert(target.target_hash.clone()))
        {
            return Err(format!("batch {} duplicates a target_hash", batch.file));
        }
        if targets.len() != batch.record_count
            || targets
                .iter()
                .map(|target| &target.input_hash)
                .collect::<Vec<_>>()
                != batch.input_hashes.iter().collect::<Vec<_>>()
        {
            return Err(format!(
                "batch {} records do not match manifest",
                batch.file
            ));
        }
        total = total.saturating_add(targets.len());
    }
    if total != manifest.target_count {
        return Err(format!(
            "{}: target_count does not match batches",
            path.display()
        ));
    }
    if manifest.eligible_targets_before_limit < manifest.target_count
        || manifest
            .target_limit
            .is_some_and(|limit| limit < manifest.target_count)
    {
        return Err(format!(
            "{}: invalid target limit accounting",
            path.display()
        ));
    }
    if manifest.held_audit.materialization != "external_nonmaterialized"
        || !is_sha256(&manifest.held_audit.sha256)
        || manifest.held_audit.record_count != manifest.filter_counts.held_targets
    {
        return Err(format!(
            "{}: invalid nonmaterialized held audit",
            path.display()
        ));
    }
    if output_directory.join("held.jsonl").exists() {
        return Err(format!(
            "{}: held.jsonl is forbidden in a committed target subset",
            output_directory.display()
        ));
    }
    for entry in fs::read_dir(output_directory)
        .map_err(|error| format!("read {}: {error}", output_directory.display()))?
    {
        let entry =
            entry.map_err(|error| format!("read {} entry: {error}", output_directory.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_target_batch_name(&name) && !expected.contains(&name) {
            return Err(format!(
                "{}: stale batch is outside manifest",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

/// Loads exactly the target batches named by a committed, hash-validated manifest.
/// This is the only supported consumer entry point; callers must not glob JSONL
/// files, because uncommitted or stale batches are intentionally fail-closed.
pub fn load_committed_targets(output_directory: &Path) -> Result<Vec<Target>, String> {
    verify_committed_batches(output_directory)?;
    let manifest_path = output_directory.join("manifest.json");
    let manifest: BatchManifest = serde_json::from_slice(
        &fs::read(&manifest_path)
            .map_err(|error| format!("read {}: {error}", manifest_path.display()))?,
    )
    .map_err(|error| {
        format!(
            "{}: invalid target manifest: {error}",
            manifest_path.display()
        )
    })?;
    let mut targets = Vec::with_capacity(manifest.target_count);
    let mut target_hashes = BTreeSet::new();
    for batch in manifest.batches {
        let bytes = fs::read(output_directory.join(&batch.file))
            .map_err(|error| format!("read batch {}: {error}", batch.file))?;
        if sha256_hex(&bytes) != batch.input_sha256 {
            return Err(format!("batch {} changed while loading", batch.file));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| format!("batch {} is not UTF-8", batch.file))?;
        let parsed = parse_targets_jsonl(text)?;
        if parsed.len() != batch.record_count
            || parsed
                .iter()
                .map(|target| &target.input_hash)
                .collect::<Vec<_>>()
                != batch.input_hashes.iter().collect::<Vec<_>>()
        {
            return Err(format!("batch {} changed while loading", batch.file));
        }
        for target in parsed {
            if !target_hashes.insert(target.target_hash.clone()) {
                return Err(format!(
                    "duplicate target_hash across committed batches: {}",
                    target.target_hash
                ));
            }
            targets.push(target);
        }
    }
    if targets.len() != manifest.target_count {
        return Err("committed batch target count changed while loading".into());
    }
    Ok(targets)
}

fn validate_uncommitted_files(
    output_directory: &Path,
    planned: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    for entry in fs::read_dir(output_directory)
        .map_err(|error| format!("read {}: {error}", output_directory.display()))?
    {
        let entry =
            entry.map_err(|error| format!("read {} entry: {error}", output_directory.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_target_batch_name(&name) && name != "held.jsonl" {
            continue;
        }
        let expected = planned.get(&name).ok_or_else(|| {
            format!(
                "{}: stale managed batch is outside this plan",
                entry.path().display()
            )
        })?;
        let existing = fs::read(entry.path())
            .map_err(|error| format!("read {}: {error}", entry.path().display()))?;
        if existing != *expected {
            return Err(format!(
                "{}: uncommitted managed batch differs from this plan",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

fn held_audit(held: &[HeldTarget]) -> Result<HeldAudit, String> {
    struct DigestWriter(Sha256);

    impl std::io::Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = DigestWriter(Sha256::new());
    for item in held {
        serde_json::to_writer(&mut writer, item).map_err(|error| error.to_string())?;
        writer.write_all(b"\n").expect("hash writer is infallible");
    }
    let mut sha256 = String::with_capacity(64);
    for byte in writer.0.finalize() {
        write!(sha256, "{byte:02x}").expect("String write");
    }
    Ok(HeldAudit {
        record_count: held.len(),
        sha256,
        materialization: "external_nonmaterialized".into(),
    })
}

fn is_target_batch_name(name: &str) -> bool {
    name.len() == 20
        && name.as_bytes()[..6].iter().all(u8::is_ascii_digit)
        && name.ends_with(".targets.jsonl")
}

pub fn file_sha256(path: &Path) -> Result<String, String> {
    fs::read(path)
        .map_err(|error| format!("read {}: {error}", path.display()))
        .map(|bytes| sha256_hex(&bytes))
}

/// Strictly validates target schema, canonicalization, ordering, and both hashes.
/// Importers must call this before matching generated content to a target.
pub fn validate_target(target: &Target) -> Result<(), String> {
    if target.schema_version != TARGET_SCHEMA_VERSION {
        return Err("unsupported schema_version".into());
    }
    if target.surface.is_empty()
        || target.reading.is_empty()
        || normalize(&target.surface) != target.surface
        || normalize(&target.reading) != target.reading
    {
        return Err("surface and reading must be non-empty NFC".into());
    }
    if target.category_ids.is_empty()
        || target
            .category_ids
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || target
            .category_ids
            .iter()
            .any(|id| DictionaryCategory::from_id(*id).is_none())
    {
        return Err("category_ids must be sorted unique valid ids".into());
    }
    let entries = &target.dictionary_identity.entries;
    if entries.is_empty() || entries.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("dictionary identities must be sorted unique valid entries".into());
    }
    if !is_sha256(&target.target_hash) || !is_sha256(&target.input_hash) {
        return Err("hashes must be lowercase SHA-256 hex".into());
    }
    if target.target_hash != target_hash(target) {
        return Err("target_hash mismatch".into());
    }
    if target.input_hash != input_hash(target) {
        return Err("input_hash mismatch".into());
    }
    Ok(())
}

fn canonical_target_json(target: &Target, include_input_hash: bool) -> Result<Vec<u8>, String> {
    #[derive(Serialize)]
    struct Input<'a> {
        schema_version: &'a str,
        surface: &'a str,
        reading: &'a str,
        category_ids: &'a [u8],
        dictionary_identity: &'a DictionaryIdentity,
        target_hash: &'a str,
    }
    if include_input_hash {
        serde_json::to_vec(target).map_err(|error| error.to_string())
    } else {
        serde_json::to_vec(&Input {
            schema_version: &target.schema_version,
            surface: &target.surface,
            reading: &target.reading,
            category_ids: &target.category_ids,
            dictionary_identity: &target.dictionary_identity,
            target_hash: &target.target_hash,
        })
        .map_err(|error| error.to_string())
    }
}

fn reject_reason(
    reading: &str,
    surface: &str,
    category: DictionaryCategory,
    flags: EntryFlags,
) -> Option<&'static str> {
    if matches!(
        category,
        DictionaryCategory::PersonNames
            | DictionaryCategory::PlaceNames
            | DictionaryCategory::OrganizationsProducts
    ) {
        return Some("proper_name");
    }
    if matches!(
        category,
        DictionaryCategory::GrammarFunction
            | DictionaryCategory::Inflectional
            | DictionaryCategory::NumericTimeUnits
            | DictionaryCategory::SymbolsEmoji
    ) {
        return Some("non_lexical");
    }
    if matches!(category, DictionaryCategory::OrthographyVariants)
        || flags.contains(EntryFlags::SPELLING_CORRECTION)
    {
        return Some("spelling_or_variant");
    }
    if reading.chars().count() > 32 || surface.chars().count() > 48 {
        return Some("abnormal_length");
    }
    if !reading.chars().all(is_reading_char) {
        return Some("reading_mismatch");
    }
    if !surface.chars().any(char::is_alphabetic) || surface.chars().any(char::is_control) {
        return Some("non_lexical");
    }
    None
}

fn is_reading_char(character: char) -> bool {
    matches!(character, '\u{3041}'..='\u{3096}' | '\u{30a1}'..='\u{30fa}' | '\u{30fc}' | '\u{30fd}' | '\u{30fe}' | 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.')
}

fn increment_reason(counts: &mut FilterCounts, reason: &str) {
    match reason {
        "non_lexical" => counts.non_lexical += 1,
        "proper_name" => counts.proper_name += 1,
        "spelling_or_variant" => counts.spelling_or_variant += 1,
        "abnormal_length" => counts.abnormal_length += 1,
        "reading_mismatch" => counts.reading_mismatch += 1,
        "ambiguous_surface" => counts.ambiguous_surface += 1,
        _ => unreachable!("known reject reason"),
    }
}

fn rank_key(target: &Target) -> (u8, u8, i32, String, String) {
    let category = target
        .category_ids
        .iter()
        .copied()
        .map(category_rank)
        .min()
        .expect("target has category");
    let flags = target
        .dictionary_identity
        .entries
        .iter()
        .map(|entry| flag_rank(entry.flags))
        .min()
        .expect("target has identity");
    let cost = target
        .dictionary_identity
        .entries
        .iter()
        .map(|entry| entry.word_cost)
        .min()
        .expect("target has identity");
    (
        category,
        flags,
        cost,
        target.reading.clone(),
        target.surface.clone(),
    )
}

fn category_rank(id: u8) -> u8 {
    match DictionaryCategory::from_id(id) {
        Some(DictionaryCategory::ItEngineering) => 0,
        Some(DictionaryCategory::SpecialistDomains) => 1,
        Some(DictionaryCategory::GeneralLexicon) => 2,
        Some(DictionaryCategory::KatakanaLoanwords) => 3,
        Some(DictionaryCategory::AbbreviationsAscii) => 4,
        Some(DictionaryCategory::FixedExpressions) => 5,
        _ => u8::MAX,
    }
}
fn flag_rank(flags: u16) -> u8 {
    if flags & EntryFlags::IT.bits() != 0 {
        0
    } else if flags & EntryFlags::PREDICTION.bits() != 0 {
        1
    } else {
        2
    }
}
fn normalize(value: &str) -> String {
    value.nfc().collect()
}
fn canonical_component(out: &mut String, value: &str) {
    write!(out, "{}:{value}|", value.len()).expect("String write");
}
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(output, "{byte:02x}").expect("String write");
    }
    output
}
fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_category_entries;

    const HEADER: &str =
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n";
    fn entries(body: &str, category: DictionaryCategory) -> Vec<CategorizedSourceEntry> {
        parse_category_entries("fixture", &(HEADER.to_owned() + body))
            .unwrap()
            .into_iter()
            .map(|entry| CategorizedSourceEntry { entry, category })
            .collect()
    }

    #[test]
    fn duplicate_ordinals_are_grouped_and_existing_exact_detail_is_excluded() {
        let entries = entries(
            "てすと\tテスト\t1\t2\t200\t-\tit\t\nてすと\tテスト\t3\t4\t100\t-\t\t\n",
            DictionaryCategory::ItEngineering,
        );
        let covered = parse_coverage_tsv(
            "covered",
            "reading\tsurface\tleft_id\tright_id\nてすと\tテスト\t1\t2\n",
        )
        .unwrap();
        let selected = select_targets(entries, &covered);
        assert!(selected.targets.is_empty());
        assert_eq!(selected.counts.already_detailed_pairs, 2);
    }

    #[test]
    fn unicode_normalization_and_hash_round_trip_are_stable() {
        let entries = entries(
            "がくせい\t学\u{304b}\u{3099}\t1\t1\t1\t-\t\t\n",
            DictionaryCategory::GeneralLexicon,
        );
        let selection = select_targets(entries, &BTreeSet::new());
        assert_eq!(selection.targets[0].surface, "学が");
        let json = String::from_utf8(targets_jsonl(&selection.targets).unwrap()).unwrap();
        assert_eq!(parse_targets_jsonl(&json).unwrap(), selection.targets);
    }

    #[test]
    fn order_is_input_independent_and_risky_rows_are_held() {
        let mut input = entries(
            "いっぱん\t一般語\t1\t1\t900\t-\t\t\nあいてぃ\tIT用語\t1\t1\t9999\t-\tit\t\n",
            DictionaryCategory::GeneralLexicon,
        );
        input.extend(entries(
            "あいてぃ\tIT用語\t1\t1\t9999\t-\tit\t\n",
            DictionaryCategory::ItEngineering,
        ));
        input.extend(entries(
            "とうきょう\t東京\t1\t1\t1\t-\t\t\n",
            DictionaryCategory::PlaceNames,
        ));
        let one = select_targets(input.clone(), &BTreeSet::new());
        input.reverse();
        let two = select_targets(input, &BTreeSet::new());
        assert_eq!(one.targets, two.targets);
        assert_eq!(one.targets[0].surface, "IT用語");
        assert!(one.held.iter().any(|held| held.reason == "proper_name"));
    }

    #[test]
    fn malformed_coverage_and_tampered_json_fail_closed() {
        assert!(parse_coverage_tsv("bad", "wrong\n").is_err());
        let target = select_targets(
            entries(
                "てすと\tテスト\t1\t1\t1\t-\t\t\n",
                DictionaryCategory::GeneralLexicon,
            ),
            &BTreeSet::new(),
        )
        .targets
        .pop()
        .unwrap();
        let json = String::from_utf8(targets_jsonl(&[target]).unwrap())
            .unwrap()
            .replace("テスト", "改竄");
        assert!(parse_targets_jsonl(&json).is_err());
    }

    #[test]
    fn annotated_sakura_entries_become_exact_coverage_only() {
        let parsed = parse_category_entries(
            "coverage",
            &(HEADER.to_owned() + "あ\t語\t1\t2\t1\t-\t\t\nあ\t語\t3\t4\t1\t-\t\t既存の説明。\n"),
        )
        .unwrap();
        let coverage = coverage_from_annotated_entries(parsed);
        assert_eq!(coverage.len(), 1);
        assert!(coverage.contains(&CoveredIdentity {
            reading: "あ".into(),
            surface: "語".into(),
            left_id: 3,
            right_id: 4,
        }));
    }

    #[test]
    fn detail_coverage_is_sorted_unique_and_rejects_noncanonical_identities() {
        let details = vec![
            SourceDetail {
                reading: "い".into(),
                surface: "語B".into(),
                left_id: 2,
                right_id: 1,
                description: "説明。".into(),
                relations: Vec::new(),
            },
            SourceDetail {
                reading: "あ".into(),
                surface: "語A".into(),
                left_id: 1,
                right_id: 2,
                description: "説明。".into(),
                relations: Vec::new(),
            },
            SourceDetail {
                reading: "あ".into(),
                surface: "語A".into(),
                left_id: 1,
                right_id: 2,
                description: "別の説明。".into(),
                relations: Vec::new(),
            },
        ];
        assert_eq!(
            String::from_utf8(details_coverage_tsv(&details).unwrap()).unwrap(),
            "reading\tsurface\tleft_id\tright_id\nあ\t語A\t1\t2\nい\t語B\t2\t1\n"
        );
        let mut invalid = details;
        invalid[0].surface.push('\n');
        assert!(details_coverage_tsv(&invalid).is_err());
    }

    #[test]
    fn later_category_reading_collision_holds_the_earlier_ranked_candidate() {
        let mut early = entries(
            "あ\t表記\t1\t1\t1\t-\t\t\n",
            DictionaryCategory::GeneralLexicon,
        );
        // This simulates a later input category. Selection must compute the
        // full reading index before ranking/truncating the first category.
        early.extend(entries(
            "い\t表記\t2\t2\t1\t-\t\t\n",
            DictionaryCategory::KatakanaLoanwords,
        ));
        let selection = select_targets(early, &BTreeSet::new());
        assert!(selection.targets.is_empty());
        assert_eq!(selection.counts.ambiguous_surface, 2);
        assert_eq!(selection.held.len(), 2);
    }

    #[test]
    fn deterministic_jsonl_mutation_sweep_is_panic_free() {
        let target = select_targets(
            entries(
                "てすと\tテスト\t1\t1\t1\t-\t\t\n",
                DictionaryCategory::GeneralLexicon,
            ),
            &BTreeSet::new(),
        )
        .targets
        .pop()
        .unwrap();
        let json = targets_jsonl(&[target]).unwrap();
        // Fixed-seed byte mutations exercise JSON/string/Unicode boundaries
        // without adding a fuzzing dependency to the offline compiler.
        let mut state = 0x1a2b_3c4du32;
        for _ in 0..4096 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let index = (state as usize) % json.len();
            let mut corrupt = json.clone();
            corrupt[index] ^= (state >> 24) as u8;
            assert!(std::panic::catch_unwind(|| {
                let _ = parse_targets_jsonl(std::str::from_utf8(&corrupt).unwrap_or("\u{fffd}"));
            })
            .is_ok());
        }
    }

    #[test]
    fn manifest_is_the_commit_marker_and_rejects_stale_or_tampered_batches() {
        let root = std::env::temp_dir().join(format!(
            "sakura-llm-target-manifest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let selection = select_targets(
            entries(
                "てすと\tテスト\t1\t1\t1\t-\t\t\n",
                DictionaryCategory::GeneralLexicon,
            ),
            &BTreeSet::new(),
        );
        let sources = BTreeMap::new();
        write_batches(&root, &selection, 1, "prompt.v1", &sources).unwrap();
        verify_committed_batches(&root).unwrap();
        assert!(
            !root.join("held.jsonl").exists(),
            "committed review subsets must not materialize the held corpus"
        );
        let manifest = fs::read_to_string(root.join("manifest.json")).unwrap();
        assert!(manifest.contains("\"external_nonmaterialized\""));
        // A second exact invocation is a no-write resume success.
        write_batches(&root, &selection, 1, "prompt.v1", &sources).unwrap();
        fs::write(root.join("000002.targets.jsonl"), b"stale\n").unwrap();
        assert!(verify_committed_batches(&root).is_err());
        let _ = fs::remove_file(root.join("000002.targets.jsonl"));
        fs::write(root.join("000001.targets.jsonl"), b"tampered\n").unwrap();
        assert!(verify_committed_batches(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn uncommitted_matching_batch_resumes_and_materialized_held_corpus_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "sakura-llm-target-resume-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let selection = select_targets(
            entries(
                "てすと\tテスト\t1\t1\t1\t-\t\t\n",
                DictionaryCategory::GeneralLexicon,
            ),
            &BTreeSet::new(),
        );
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("000001.targets.jsonl"),
            targets_jsonl(&selection.targets).unwrap(),
        )
        .unwrap();
        write_batches(&root, &selection, 1, "prompt.v1", &BTreeMap::new()).unwrap();
        verify_committed_batches(&root).unwrap();
        assert_eq!(load_committed_targets(&root).unwrap(), selection.targets);
        fs::write(root.join("held.jsonl"), b"{\"surface\":\"unexpected\"}\n").unwrap();
        assert!(verify_committed_batches(&root).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
