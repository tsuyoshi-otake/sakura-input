//! Deterministic, offline-only Context Prediction dataset gate.
//!
//! Raw records must come from an actual Sakura prediction snapshot producer.
//! This module validates that bounded schema, creates stable SHA-256 identities,
//! assigns whole articles to frozen splits, removes exact and narrowly defined
//! layout-only near duplicates, and commits hash-bound JSONL artifacts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use sakura_neural_proto::{
    MAX_CANDIDATE_SURFACE_BYTES, MAX_CONTEXT_BYTES, MAX_PREDICTION_CANDIDATES, MAX_READING_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

pub const RECORD_SCHEMA_VERSION: u16 = 1;
pub const DATASET_SCHEMA_VERSION: u16 = 1;
pub const SPLIT_ALGORITHM: &str = "sha256-article-80-10-10-v1";
pub const IDENTITY_ALGORITHM: &str = "sha256-nfc-length-framed-v1";
pub const NEAR_DUPLICATE_ALGORITHM: &str = "nfc-alphanumeric-layout-insensitive-v1";
pub const MINIMUM_TIER_A_AUDIT: usize = 1_000;

#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub records: PathBuf,
    pub source_manifest: PathBuf,
    pub output_directory: PathBuf,
    pub generator_sha256: String,
    pub dictionary_sha256: String,
    pub audit_tier_a: usize,
    pub audit_tier_b: usize,
    pub audit_tier_c: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedSource {
    pub source_id: String,
    pub snapshot: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    A,
    B,
    C,
}

impl Tier {
    const fn label(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateAuthority {
    Ordinary,
    ExactLearning,
    UserDictionary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSource {
    History,
    SystemDictionary,
    UserDictionary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictionaryKind {
    None,
    System,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRecord {
    pub runtime_candidate_id: String,
    pub reading: String,
    pub surface: String,
    pub dictionary_kind: DictionaryKind,
    pub dictionary_ordinal: Option<u32>,
    pub base_cost: i32,
    pub authority: CandidateAuthority,
    pub source: CandidateSource,
    pub right_id: u16,
    pub is_it: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawReplayRecord {
    pub schema_version: u16,
    pub source_id: String,
    pub article_id: u64,
    pub revision_id: u64,
    pub sample_ordinal: u32,
    pub tier: Tier,
    pub snapshot_fingerprint: String,
    pub context: String,
    pub reading: String,
    pub candidates: Vec<CandidateRecord>,
    pub expected_candidate_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetCandidate {
    pub stable_id: String,
    pub runtime_candidate_id: String,
    pub reading: String,
    pub surface: String,
    pub dictionary_kind: DictionaryKind,
    pub dictionary_ordinal: Option<u32>,
    pub base_cost: i32,
    pub authority: CandidateAuthority,
    pub source: CandidateSource,
    pub right_id: u16,
    pub is_it: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRecord {
    pub schema_version: u16,
    pub sample_id: String,
    pub split: Split,
    pub source_id: String,
    pub article_id: u64,
    pub revision_id: u64,
    pub sample_ordinal: u32,
    pub tier: Tier,
    pub snapshot_fingerprint: String,
    pub context: String,
    pub reading: String,
    pub candidates: Vec<DatasetCandidate>,
    pub expected_candidate_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Split {
    Train,
    Tuning,
    HeldOut,
}

impl Split {
    const ALL: [Self; 3] = [Self::Train, Self::Tuning, Self::HeldOut];

    const fn file_name(self) -> &'static str {
        match self {
            Self::Train => "train.jsonl",
            Self::Tuning => "tuning.jsonl",
            Self::HeldOut => "held-out.jsonl",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    pub role: String,
    pub file: String,
    pub records: usize,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeduplicationReport {
    pub input_records: usize,
    pub accepted_records: usize,
    pub exact_duplicates_removed: usize,
    pub near_duplicates_removed: usize,
    pub cross_split_exact_duplicates_removed: usize,
    pub cross_split_near_duplicates_removed: usize,
    pub cross_split_exact_leakage: usize,
    pub cross_split_near_leakage: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditGate {
    pub tier_a_required: usize,
    pub tier_a_available: usize,
    pub tier_a_requirement_met: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetManifest {
    pub schema_version: u16,
    pub record_schema_version: u16,
    pub source_id: String,
    pub source_manifest_sha256: String,
    pub input_records_sha256: String,
    pub generator_sha256: String,
    pub dictionary_sha256: String,
    pub split_algorithm: String,
    pub identity_algorithm: String,
    pub near_duplicate_algorithm: String,
    pub deduplication: DeduplicationReport,
    pub splits: Vec<ArtifactRecord>,
    pub audits: Vec<ArtifactRecord>,
    pub audit_gate: AuditGate,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceManifest {
    schema_version: u16,
    source_id: String,
    database: String,
    language: String,
    snapshot: String,
    source_page: String,
    usage_boundary: String,
    license_review_status: String,
    license_reference: String,
    files: Vec<SourceFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    role: String,
    name: String,
    url: String,
    bytes: u64,
    hash_algorithm: String,
    hash: String,
}

/// Builds one immutable external dataset directory. The manifest is created
/// last and is the only commit marker consumers may trust.
pub fn build_dataset(config: &BuildConfig) -> Result<DatasetManifest, String> {
    validate_sha256(&config.generator_sha256, "generator SHA-256")?;
    validate_sha256(&config.dictionary_sha256, "dictionary SHA-256")?;
    if config.audit_tier_a < MINIMUM_TIER_A_AUDIT {
        return Err(format!(
            "Tier A audit request must be at least {MINIMUM_TIER_A_AUDIT}"
        ));
    }
    if config.output_directory.exists() {
        return Err(format!(
            "output directory already exists; use a new immutable directory: {}",
            config.output_directory.display()
        ));
    }

    let source = load_pinned_source(&config.source_manifest)?;

    let input_bytes = fs::read(&config.records)
        .map_err(|error| format!("read {}: {error}", config.records.display()))?;
    if input_bytes.contains(&b'\r') {
        return Err(format!("{} is not LF-canonical", config.records.display()));
    }
    let input_records_sha256 = sha256_hex(&input_bytes);
    let input_text = std::str::from_utf8(&input_bytes)
        .map_err(|_| format!("{} is not UTF-8", config.records.display()))?;
    let raw = parse_records_jsonl(input_text)?;
    let input_count = raw.len();
    let mut records = raw
        .into_iter()
        .map(|record| normalize_record(record, &source.source_id))
        .collect::<Result<Vec<_>, _>>()?;
    records.sort_by(|left, right| left.sample_id.cmp(&right.sample_id));

    let mut exact_seen = BTreeMap::<String, Split>::new();
    let mut near_seen = BTreeMap::<String, Split>::new();
    let mut accepted = Vec::with_capacity(records.len());
    let mut exact_removed = 0usize;
    let mut near_removed = 0usize;
    let mut cross_exact = 0usize;
    let mut cross_near = 0usize;
    for record in records {
        let exact = exact_duplicate_key(&record);
        if let Some(previous_split) = exact_seen.get(&exact) {
            exact_removed += 1;
            cross_exact += usize::from(*previous_split != record.split);
            continue;
        }
        exact_seen.insert(exact, record.split);

        if let Some(near) = near_duplicate_key(&record) {
            if let Some(previous_split) = near_seen.get(&near) {
                near_removed += 1;
                cross_near += usize::from(*previous_split != record.split);
                continue;
            }
            near_seen.insert(near, record.split);
        }
        accepted.push(record);
    }

    fs::create_dir(&config.output_directory)
        .map_err(|error| format!("create {}: {error}", config.output_directory.display()))?;
    let mut splits = Vec::new();
    for split in Split::ALL {
        let selected = accepted
            .iter()
            .filter(|record| record.split == split)
            .collect::<Vec<_>>();
        splits.push(write_jsonl_artifact(
            &config.output_directory,
            split.file_name(),
            &format!("split:{split:?}"),
            &selected,
        )?);
    }

    let audit_specs = [
        (Tier::A, config.audit_tier_a),
        (Tier::B, config.audit_tier_b),
        (Tier::C, config.audit_tier_c),
    ];
    let mut audits = Vec::new();
    for (tier, count) in audit_specs {
        let selected = select_audit(&accepted, tier, count);
        audits.push(write_jsonl_artifact(
            &config.output_directory,
            &format!("audit-tier-{}.jsonl", tier.label().to_ascii_lowercase()),
            &format!("audit:tier-{}", tier.label()),
            &selected,
        )?);
    }
    let tier_a_available = accepted
        .iter()
        .filter(|record| record.split == Split::HeldOut && record.tier == Tier::A)
        .count();
    let manifest = DatasetManifest {
        schema_version: DATASET_SCHEMA_VERSION,
        record_schema_version: RECORD_SCHEMA_VERSION,
        source_id: source.source_id,
        source_manifest_sha256: source.manifest_sha256,
        input_records_sha256,
        generator_sha256: config.generator_sha256.clone(),
        dictionary_sha256: config.dictionary_sha256.clone(),
        split_algorithm: SPLIT_ALGORITHM.into(),
        identity_algorithm: IDENTITY_ALGORITHM.into(),
        near_duplicate_algorithm: NEAR_DUPLICATE_ALGORITHM.into(),
        deduplication: DeduplicationReport {
            input_records: input_count,
            accepted_records: accepted.len(),
            exact_duplicates_removed: exact_removed,
            near_duplicates_removed: near_removed,
            cross_split_exact_duplicates_removed: cross_exact,
            cross_split_near_duplicates_removed: cross_near,
            cross_split_exact_leakage: 0,
            cross_split_near_leakage: 0,
        },
        splits,
        audits,
        audit_gate: AuditGate {
            tier_a_required: MINIMUM_TIER_A_AUDIT,
            tier_a_available,
            tier_a_requirement_met: tier_a_available >= MINIMUM_TIER_A_AUDIT,
        },
    };
    write_manifest(&config.output_directory, &manifest)?;
    if let Err(error) = verify_dataset(&config.output_directory) {
        let _ = fs::remove_file(config.output_directory.join("manifest.json"));
        return Err(format!(
            "generated dataset failed verification; commit marker removed: {error}"
        ));
    }
    Ok(manifest)
}

pub fn load_pinned_source(path: &Path) -> Result<PinnedSource, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let source: SourceManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid source manifest: {error}"))?;
    validate_source_manifest(&source)?;
    Ok(PinnedSource {
        source_id: source.source_id,
        snapshot: source.snapshot,
        manifest_sha256: sha256_hex(&bytes),
    })
}

pub fn verify_dataset(directory: &Path) -> Result<DatasetManifest, String> {
    let manifest_path = directory.join("manifest.json");
    let bytes = fs::read(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest: DatasetManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid dataset manifest: {error}"))?;
    if manifest.schema_version != DATASET_SCHEMA_VERSION
        || manifest.record_schema_version != RECORD_SCHEMA_VERSION
        || manifest.split_algorithm != SPLIT_ALGORITHM
        || manifest.identity_algorithm != IDENTITY_ALGORITHM
        || manifest.near_duplicate_algorithm != NEAR_DUPLICATE_ALGORITHM
        || manifest.deduplication.cross_split_exact_leakage != 0
        || manifest.deduplication.cross_split_near_leakage != 0
    {
        return Err("dataset manifest contract is invalid".into());
    }
    validate_sha256(&manifest.source_manifest_sha256, "source manifest SHA-256")?;
    validate_sha256(&manifest.input_records_sha256, "input records SHA-256")?;
    validate_sha256(&manifest.generator_sha256, "generator SHA-256")?;
    validate_sha256(&manifest.dictionary_sha256, "dictionary SHA-256")?;

    let expected_split_files = Split::ALL
        .into_iter()
        .map(|split| split.file_name())
        .collect::<BTreeSet<_>>();
    if manifest.splits.len() != expected_split_files.len()
        || manifest
            .splits
            .iter()
            .map(|artifact| artifact.file.as_str())
            .collect::<BTreeSet<_>>()
            != expected_split_files
    {
        return Err("dataset split artifact set is invalid".into());
    }
    let expected_audits = [
        "audit-tier-a.jsonl",
        "audit-tier-b.jsonl",
        "audit-tier-c.jsonl",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    if manifest.audits.len() != expected_audits.len()
        || manifest
            .audits
            .iter()
            .map(|artifact| artifact.file.as_str())
            .collect::<BTreeSet<_>>()
            != expected_audits
    {
        return Err("dataset audit artifact set is invalid".into());
    }

    let mut accepted = Vec::new();
    for split in Split::ALL {
        let artifact = find_artifact(&manifest.splits, split.file_name())?;
        if artifact.role != format!("split:{split:?}") {
            return Err(format!("invalid role for {}", artifact.file));
        }
        validate_artifact(directory, artifact)?;
        let records = read_dataset_records(directory, artifact)?;
        for record in &records {
            validate_dataset_record(record, &manifest.source_id, split)?;
        }
        accepted.extend(records);
    }
    if accepted.len() != manifest.deduplication.accepted_records
        || manifest.deduplication.input_records
            != manifest
                .deduplication
                .accepted_records
                .saturating_add(manifest.deduplication.exact_duplicates_removed)
                .saturating_add(manifest.deduplication.near_duplicates_removed)
    {
        return Err("dataset deduplication accounting is invalid".into());
    }
    if manifest.deduplication.cross_split_exact_duplicates_removed
        > manifest.deduplication.exact_duplicates_removed
        || manifest.deduplication.cross_split_near_duplicates_removed
            > manifest.deduplication.near_duplicates_removed
    {
        return Err("cross-split deduplication accounting is invalid".into());
    }

    let mut sample_ids = BTreeSet::new();
    let mut exact_keys = BTreeSet::new();
    let mut near_keys = BTreeSet::new();
    for record in &accepted {
        if !sample_ids.insert(record.sample_id.as_str())
            || !exact_keys.insert(exact_duplicate_key(record))
            || near_duplicate_key(record).is_some_and(|key| !near_keys.insert(key))
        {
            return Err("accepted dataset contains identity or duplicate leakage".into());
        }
    }

    let tier_a_available = accepted
        .iter()
        .filter(|record| record.split == Split::HeldOut && record.tier == Tier::A)
        .count();
    if manifest.audit_gate.tier_a_required != MINIMUM_TIER_A_AUDIT
        || manifest.audit_gate.tier_a_available != tier_a_available
        || manifest.audit_gate.tier_a_requirement_met != (tier_a_available >= MINIMUM_TIER_A_AUDIT)
    {
        return Err("Tier A audit gate accounting is invalid".into());
    }

    for tier in [Tier::A, Tier::B, Tier::C] {
        let file_name = format!("audit-tier-{}.jsonl", tier.label().to_ascii_lowercase());
        let artifact = find_artifact(&manifest.audits, &file_name)?;
        if artifact.role != format!("audit:tier-{}", tier.label()) {
            return Err(format!("invalid role for {}", artifact.file));
        }
        validate_artifact(directory, artifact)?;
        let audit = read_dataset_records(directory, artifact)?;
        if tier == Tier::A && audit.len() < tier_a_available.min(MINIMUM_TIER_A_AUDIT) {
            return Err("Tier A audit artifact is smaller than the fixed gate".into());
        }
        for record in &audit {
            validate_dataset_record(record, &manifest.source_id, Split::HeldOut)?;
            if record.tier != tier || !accepted.contains(record) {
                return Err(format!(
                    "{} is not a held-out Tier {} subset",
                    artifact.file,
                    tier.label()
                ));
            }
        }
        if select_audit(&accepted, tier, audit.len()) != audit.iter().collect::<Vec<_>>() {
            return Err(format!(
                "{} is not the deterministic audit selection",
                artifact.file
            ));
        }
    }
    Ok(manifest)
}

fn find_artifact<'a>(
    artifacts: &'a [ArtifactRecord],
    file_name: &str,
) -> Result<&'a ArtifactRecord, String> {
    artifacts
        .iter()
        .find(|artifact| artifact.file == file_name)
        .ok_or_else(|| format!("missing artifact {file_name}"))
}

fn read_dataset_records(
    directory: &Path,
    artifact: &ArtifactRecord,
) -> Result<Vec<DatasetRecord>, String> {
    let bytes = fs::read(directory.join(&artifact.file))
        .map_err(|error| format!("read {}: {error}", artifact.file))?;
    if bytes.contains(&b'\r') || (!bytes.is_empty() && !bytes.ends_with(b"\n")) {
        return Err(format!("{} is not LF-canonical JSONL", artifact.file));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| format!("{} is not UTF-8", artifact.file))?;
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<DatasetRecord>(line)
                .map_err(|error| format!("{} line {}: {error}", artifact.file, index + 1))
        })
        .collect()
}

fn validate_dataset_record(
    record: &DatasetRecord,
    source_id: &str,
    expected_split: Split,
) -> Result<(), String> {
    if record.schema_version != RECORD_SCHEMA_VERSION
        || record.source_id != source_id
        || record.article_id == 0
        || record.revision_id == 0
        || record.split != expected_split
        || split_for_article(source_id, record.article_id) != expected_split
        || stable_sample_id(
            source_id,
            record.article_id,
            record.revision_id,
            record.sample_ordinal,
            &record.snapshot_fingerprint,
        ) != record.sample_id
    {
        return Err(format!(
            "invalid dataset record identity: {}",
            record.sample_id
        ));
    }
    validate_sha256(&record.sample_id, "sample id")?;
    validate_hex(&record.snapshot_fingerprint, 64, "snapshot fingerprint")?;
    if normalize_nfc(&record.context) != record.context
        || normalize_nfc(&record.reading) != record.reading
    {
        return Err(format!("record {} is not NFC", record.sample_id));
    }
    validate_text(&record.context, MAX_CONTEXT_BYTES, "context")?;
    validate_text(&record.reading, MAX_READING_BYTES, "reading")?;
    if record.candidates.is_empty() || record.candidates.len() > MAX_PREDICTION_CANDIDATES {
        return Err(format!("invalid candidate count in {}", record.sample_id));
    }

    let mut runtime_ids = BTreeSet::new();
    let mut stable_ids = BTreeSet::new();
    for candidate in &record.candidates {
        validate_hex(&candidate.runtime_candidate_id, 16, "runtime candidate id")?;
        validate_sha256(&candidate.stable_id, "stable candidate id")?;
        if candidate.runtime_candidate_id == "0000000000000000"
            || !runtime_ids.insert(candidate.runtime_candidate_id.as_str())
            || !stable_ids.insert(candidate.stable_id.as_str())
            || normalize_nfc(&candidate.reading) != candidate.reading
            || normalize_nfc(&candidate.surface) != candidate.surface
        {
            return Err(format!(
                "invalid candidate identity in {}",
                record.sample_id
            ));
        }
        validate_text(&candidate.reading, MAX_READING_BYTES, "candidate reading")?;
        validate_text(
            &candidate.surface,
            MAX_CANDIDATE_SURFACE_BYTES,
            "candidate surface",
        )?;
        let raw = CandidateRecord {
            runtime_candidate_id: candidate.runtime_candidate_id.clone(),
            reading: candidate.reading.clone(),
            surface: candidate.surface.clone(),
            dictionary_kind: candidate.dictionary_kind,
            dictionary_ordinal: candidate.dictionary_ordinal,
            base_cost: candidate.base_cost,
            authority: candidate.authority,
            source: candidate.source,
            right_id: candidate.right_id,
            is_it: candidate.is_it,
        };
        validate_candidate_contract(&raw)?;
        let identity = canonical_candidate_identity(
            &candidate.reading,
            &candidate.surface,
            candidate.dictionary_kind,
            candidate.dictionary_ordinal,
        );
        if sha256_hex(identity.as_bytes()) != candidate.stable_id {
            return Err(format!("candidate hash mismatch in {}", record.sample_id));
        }
    }
    let expected_is_present = record
        .expected_candidate_id
        .as_ref()
        .is_some_and(|expected| stable_ids.contains(expected.as_str()));
    if (matches!(record.tier, Tier::A | Tier::B) && !expected_is_present)
        || (record.tier == Tier::C && record.expected_candidate_id.is_some())
    {
        return Err(format!("invalid label in {}", record.sample_id));
    }
    Ok(())
}

fn parse_records_jsonl(text: &str) -> Result<Vec<RawReplayRecord>, String> {
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            return Err(format!("record line {} is empty", index + 1));
        }
        if line.ends_with('\r') {
            return Err(format!("record line {} is not LF-canonical", index + 1));
        }
        let record = serde_json::from_str::<RawReplayRecord>(line)
            .map_err(|error| format!("record line {}: {error}", index + 1))?;
        records.push(record);
    }
    if records.is_empty() {
        return Err("record input is empty".into());
    }
    Ok(records)
}

fn normalize_record(raw: RawReplayRecord, source_id: &str) -> Result<DatasetRecord, String> {
    if raw.schema_version != RECORD_SCHEMA_VERSION || raw.source_id != source_id {
        return Err("record schema or source_id does not match the pinned source".into());
    }
    if raw.article_id == 0 || raw.revision_id == 0 {
        return Err("article_id and revision_id must be nonzero".into());
    }
    validate_hex(&raw.snapshot_fingerprint, 64, "snapshot fingerprint")?;
    let context = normalize_nfc(&raw.context);
    let reading = normalize_nfc(&raw.reading);
    validate_text(&context, MAX_CONTEXT_BYTES, "context")?;
    validate_text(&reading, MAX_READING_BYTES, "reading")?;
    if raw.candidates.is_empty() || raw.candidates.len() > MAX_PREDICTION_CANDIDATES {
        return Err("candidate count must be between 1 and 32".into());
    }
    if matches!(raw.tier, Tier::A | Tier::B) && raw.expected_candidate_index.is_none() {
        return Err("Tier A/B records require an expected candidate".into());
    }
    if raw.tier == Tier::C && raw.expected_candidate_index.is_some() {
        return Err("Tier C records must remain unlabeled".into());
    }
    if raw
        .expected_candidate_index
        .is_some_and(|index| index >= raw.candidates.len())
    {
        return Err("expected candidate index is outside the snapshot".into());
    }

    let mut runtime_ids = BTreeSet::new();
    let mut canonical_identities = BTreeSet::new();
    let mut candidates = Vec::with_capacity(raw.candidates.len());
    for candidate in raw.candidates {
        validate_hex(&candidate.runtime_candidate_id, 16, "runtime candidate id")?;
        if candidate.runtime_candidate_id == "0000000000000000"
            || !runtime_ids.insert(candidate.runtime_candidate_id.clone())
        {
            return Err("runtime candidate ids must be unique and nonzero".into());
        }
        validate_candidate_contract(&candidate)?;
        let candidate_reading = normalize_nfc(&candidate.reading);
        let surface = normalize_nfc(&candidate.surface);
        validate_text(&candidate_reading, MAX_READING_BYTES, "candidate reading")?;
        validate_text(&surface, MAX_CANDIDATE_SURFACE_BYTES, "candidate surface")?;
        let identity = canonical_candidate_identity(
            &candidate_reading,
            &surface,
            candidate.dictionary_kind,
            candidate.dictionary_ordinal,
        );
        if !canonical_identities.insert(identity.clone()) {
            return Err("snapshot contains a duplicate canonical candidate identity".into());
        }
        candidates.push(DatasetCandidate {
            stable_id: sha256_hex(identity.as_bytes()),
            runtime_candidate_id: candidate.runtime_candidate_id,
            reading: candidate_reading,
            surface,
            dictionary_kind: candidate.dictionary_kind,
            dictionary_ordinal: candidate.dictionary_ordinal,
            base_cost: candidate.base_cost,
            authority: candidate.authority,
            source: candidate.source,
            right_id: candidate.right_id,
            is_it: candidate.is_it,
        });
    }
    let expected_candidate_id = raw
        .expected_candidate_index
        .map(|index| candidates[index].stable_id.clone());
    let split = split_for_article(source_id, raw.article_id);
    let sample_id = stable_sample_id(
        source_id,
        raw.article_id,
        raw.revision_id,
        raw.sample_ordinal,
        &raw.snapshot_fingerprint,
    );
    Ok(DatasetRecord {
        schema_version: RECORD_SCHEMA_VERSION,
        sample_id,
        split,
        source_id: raw.source_id,
        article_id: raw.article_id,
        revision_id: raw.revision_id,
        sample_ordinal: raw.sample_ordinal,
        tier: raw.tier,
        snapshot_fingerprint: raw.snapshot_fingerprint,
        context,
        reading,
        candidates,
        expected_candidate_id,
    })
}

fn validate_candidate_contract(candidate: &CandidateRecord) -> Result<(), String> {
    let dictionary_valid = match candidate.dictionary_kind {
        DictionaryKind::None => candidate.dictionary_ordinal.is_none(),
        DictionaryKind::System | DictionaryKind::User => candidate.dictionary_ordinal.is_some(),
    };
    let structural_valid = matches!(
        (
            candidate.source,
            candidate.authority,
            candidate.dictionary_kind
        ),
        (
            CandidateSource::SystemDictionary,
            CandidateAuthority::Ordinary,
            DictionaryKind::System
        )
    );
    if !dictionary_valid || !structural_valid {
        return Err("offline corpus candidates must be ordinary system-dictionary entries".into());
    }
    Ok(())
}

fn validate_source_manifest(source: &SourceManifest) -> Result<(), String> {
    if source.schema_version != 1
        || source.database != "jawiki"
        || source.language != "ja"
        || source.usage_boundary != "offline-research-only-not-shipped"
        || source.license_review_status != "required-before-dataset-or-model-distribution"
        || source.snapshot.len() != 8
        || source.source_page.is_empty()
        || source.license_reference.is_empty()
        || source.files.len() != 3
    {
        return Err("source manifest contract is invalid".into());
    }
    let roles = source
        .files
        .iter()
        .map(|file| file.role.as_str())
        .collect::<BTreeSet<_>>();
    if roles
        != ["articles", "index", "official-checksums"]
            .into_iter()
            .collect()
    {
        return Err("source manifest role set is invalid".into());
    }
    for file in &source.files {
        if file.name.is_empty()
            || file.url.is_empty()
            || file.bytes == 0
            || !matches!(file.hash_algorithm.as_str(), "sha1" | "sha256")
        {
            return Err("source manifest file record is invalid".into());
        }
        let length = if file.hash_algorithm == "sha1" {
            40
        } else {
            64
        };
        validate_hex(&file.hash, length, "source file hash")?;
    }
    Ok(())
}

fn split_for_article(source_id: &str, article_id: u64) -> Split {
    let key = canonical_parts(&[SPLIT_ALGORITHM, source_id, &article_id.to_string()]);
    let digest = Sha256::digest(key.as_bytes());
    let bucket = u16::from_be_bytes([digest[0], digest[1]]) % 10_000;
    match bucket {
        0..=7_999 => Split::Train,
        8_000..=8_999 => Split::Tuning,
        _ => Split::HeldOut,
    }
}

fn stable_sample_id(
    source_id: &str,
    article_id: u64,
    revision_id: u64,
    sample_ordinal: u32,
    snapshot_fingerprint: &str,
) -> String {
    sha256_hex(
        canonical_parts(&[
            IDENTITY_ALGORITHM,
            source_id,
            &article_id.to_string(),
            &revision_id.to_string(),
            &sample_ordinal.to_string(),
            snapshot_fingerprint,
        ])
        .as_bytes(),
    )
}

fn canonical_candidate_identity(
    reading: &str,
    surface: &str,
    kind: DictionaryKind,
    ordinal: Option<u32>,
) -> String {
    canonical_parts(&[
        IDENTITY_ALGORITHM,
        reading,
        surface,
        match kind {
            DictionaryKind::None => "none",
            DictionaryKind::System => "system",
            DictionaryKind::User => "user",
        },
        &ordinal.map_or_else(|| "-".into(), |value| value.to_string()),
    ])
}

fn exact_duplicate_key(record: &DatasetRecord) -> String {
    let candidates = record
        .candidates
        .iter()
        .map(|candidate| candidate.stable_id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    sha256_hex(
        canonical_parts(&[
            &record.context,
            &record.reading,
            &candidates,
            record.expected_candidate_id.as_deref().unwrap_or("-"),
        ])
        .as_bytes(),
    )
}

fn near_duplicate_key(record: &DatasetRecord) -> Option<String> {
    let context = record
        .context
        .nfc()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    if context.chars().count() < 16 {
        return None;
    }
    Some(sha256_hex(
        canonical_parts(&[
            NEAR_DUPLICATE_ALGORITHM,
            &context,
            &record.reading,
            record.expected_candidate_id.as_deref().unwrap_or("-"),
        ])
        .as_bytes(),
    ))
}

fn select_audit(records: &[DatasetRecord], tier: Tier, count: usize) -> Vec<&DatasetRecord> {
    let mut candidates = records
        .iter()
        .filter(|record| record.split == Split::HeldOut && record.tier == tier)
        .map(|record| {
            let key = sha256_hex(
                canonical_parts(&["audit-v1", tier.label(), &record.sample_id]).as_bytes(),
            );
            (key, record)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates
        .into_iter()
        .take(count)
        .map(|(_, record)| record)
        .collect()
}

fn write_jsonl_artifact<T: Serialize>(
    directory: &Path,
    file_name: &str,
    role: &str,
    records: &[T],
) -> Result<ArtifactRecord, String> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
    }
    let path = directory.join(file_name);
    write_new(&path, &bytes)?;
    Ok(ArtifactRecord {
        role: role.into(),
        file: file_name.into(),
        records: records.len(),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        sha256: sha256_hex(&bytes),
    })
}

fn write_manifest(directory: &Path, manifest: &DatasetManifest) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    write_new(&directory.join("manifest.json"), &bytes)
}

fn validate_artifact(directory: &Path, artifact: &ArtifactRecord) -> Result<(), String> {
    if Path::new(&artifact.file)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(artifact.file.as_str())
    {
        return Err(format!("artifact name is unsafe: {}", artifact.file));
    }
    validate_sha256(&artifact.sha256, "artifact SHA-256")?;
    let bytes = fs::read(directory.join(&artifact.file))
        .map_err(|error| format!("read {}: {error}", artifact.file))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != artifact.bytes
        || sha256_hex(&bytes) != artifact.sha256
        || bytes.iter().filter(|byte| **byte == b'\n').count() != artifact.records
    {
        return Err(format!(
            "artifact does not match manifest: {}",
            artifact.file
        ));
    }
    Ok(())
}

fn validate_text(text: &str, maximum: usize, label: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > maximum {
        return Err(format!("{label} must contain 1..={maximum} UTF-8 bytes"));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    validate_hex(value, 64, label)
}

fn validate_hex(value: &str, length: usize, label: &str) -> Result<(), String> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!(
            "{label} must be {length} lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

fn normalize_nfc(value: &str) -> String {
    value.nfc().collect()
}

fn canonical_parts(parts: &[&str]) -> String {
    let mut output = String::new();
    for part in parts {
        write!(output, "{}:{part}|", part.len()).expect("String write");
    }
    output
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(output, "{byte:02x}").expect("String write");
    }
    output
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
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
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sakura-context-dataset-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn candidate(id: u64, surface: &str) -> CandidateRecord {
        CandidateRecord {
            runtime_candidate_id: format!("{id:016x}"),
            reading: "かな".into(),
            surface: surface.into(),
            dictionary_kind: DictionaryKind::System,
            dictionary_ordinal: Some(u32::try_from(id).unwrap_or(u32::MAX)),
            base_cost: 100,
            authority: CandidateAuthority::Ordinary,
            source: CandidateSource::SystemDictionary,
            right_id: 1,
            is_it: false,
        }
    }

    fn raw(article_id: u64, context: &str, tier: Tier) -> RawReplayRecord {
        RawReplayRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            source_id: "test-source".into(),
            article_id,
            revision_id: article_id + 100,
            sample_ordinal: 0,
            tier,
            snapshot_fingerprint: format!("{article_id:064x}"),
            context: context.into(),
            reading: "かな".into(),
            candidates: vec![candidate(article_id, "仮名")],
            expected_candidate_index: (tier != Tier::C).then_some(0),
        }
    }

    #[test]
    fn stable_ids_normalize_nfc_and_article_split_is_stable() {
        let composed = raw(1, "ガイドで説明します。", Tier::A);
        let mut decomposed = composed.clone();
        decomposed.context = "カ\u{3099}イドで説明します。".into();
        let first = normalize_record(composed, "test-source").expect("record");
        let second = normalize_record(decomposed, "test-source").expect("record");
        assert_eq!(first.context, second.context);
        assert_eq!(first.split, second.split);
        assert_eq!(first.sample_id, second.sample_id);
        assert_eq!(
            first.candidates[0].stable_id,
            second.candidates[0].stable_id
        );

        let another =
            normalize_record(raw(1, "別の文脈です。", Tier::B), "test-source").expect("record");
        assert_eq!(
            first.split, another.split,
            "one article never crosses splits"
        );
    }

    #[test]
    fn near_duplicate_v1_ignores_only_layout_and_punctuation() {
        let first = normalize_record(
            raw(1, "これは十分に長い、同じ文脈の文章です。", Tier::A),
            "test-source",
        )
        .expect("record");
        let second = normalize_record(
            {
                let mut record = raw(2, "これは十分に長い 同じ文脈の文章です", Tier::A);
                record.candidates = first
                    .candidates
                    .iter()
                    .map(|candidate| CandidateRecord {
                        runtime_candidate_id: candidate.runtime_candidate_id.clone(),
                        reading: candidate.reading.clone(),
                        surface: candidate.surface.clone(),
                        dictionary_kind: candidate.dictionary_kind,
                        dictionary_ordinal: candidate.dictionary_ordinal,
                        base_cost: candidate.base_cost,
                        authority: candidate.authority,
                        source: candidate.source,
                        right_id: candidate.right_id,
                        is_it: candidate.is_it,
                    })
                    .collect();
                record
            },
            "test-source",
        )
        .expect("record");
        assert_eq!(near_duplicate_key(&first), near_duplicate_key(&second));
        assert_ne!(exact_duplicate_key(&first), exact_duplicate_key(&second));
    }

    #[test]
    fn malformed_structural_tiers_and_duplicate_candidates_fail_closed() {
        let mut invalid = raw(1, "文脈", Tier::A);
        invalid.candidates[0].authority = CandidateAuthority::UserDictionary;
        assert!(normalize_record(invalid, "test-source").is_err());

        let mut learned = raw(1, "文脈", Tier::A);
        learned.candidates[0].source = CandidateSource::History;
        learned.candidates[0].authority = CandidateAuthority::ExactLearning;
        learned.candidates[0].dictionary_kind = DictionaryKind::None;
        learned.candidates[0].dictionary_ordinal = None;
        assert!(normalize_record(learned, "test-source").is_err());

        let mut duplicate = raw(1, "文脈", Tier::A);
        let mut second = duplicate.candidates[0].clone();
        second.runtime_candidate_id = "0000000000000002".into();
        duplicate.candidates.push(second);
        assert!(normalize_record(duplicate, "test-source").is_err());

        let mut unlabeled = raw(1, "文脈", Tier::C);
        unlabeled.expected_candidate_index = Some(0);
        assert!(normalize_record(unlabeled, "test-source").is_err());
    }

    #[test]
    fn build_is_deterministic_removes_cross_split_leakage_and_detects_tampering() {
        let temporary = TestDirectory::new();
        let source_manifest = temporary.0.join("source-manifest.json");
        let source = serde_json::json!({
            "schema_version": 1,
            "source_id": "test-source",
            "database": "jawiki",
            "language": "ja",
            "snapshot": "20260801",
            "source_page": "https://dumps.wikimedia.org/jawiki/20260801/",
            "usage_boundary": "offline-research-only-not-shipped",
            "license_review_status": "required-before-dataset-or-model-distribution",
            "license_reference": "https://dumps.wikimedia.org/legal.html",
            "files": [
                {"role":"articles","name":"articles.xml.bz2","url":"https://example.invalid/articles","bytes":1,"hash_algorithm":"sha1","hash":"1111111111111111111111111111111111111111"},
                {"role":"index","name":"index.txt.bz2","url":"https://example.invalid/index","bytes":1,"hash_algorithm":"sha1","hash":"2222222222222222222222222222222222222222"},
                {"role":"official-checksums","name":"sha1sums.txt","url":"https://example.invalid/checksums","bytes":1,"hash_algorithm":"sha256","hash":"3333333333333333333333333333333333333333333333333333333333333333"}
            ]
        });
        fs::write(
            &source_manifest,
            serde_json::to_vec_pretty(&source).expect("source JSON"),
        )
        .expect("write source manifest");

        let held_a = article_for_split(Split::HeldOut, 1);
        let held_b = article_for_split(Split::HeldOut, held_a + 1);
        let held_c = article_for_split(Split::HeldOut, held_b + 1);
        let train = article_for_split(Split::Train, 1);
        let tuning = article_for_split(Split::Tuning, 1);
        let mut records = vec![
            raw(held_a, "監査用の十分に長いTier A文脈です", Tier::A),
            raw(held_b, "監査用の十分に長いTier B文脈です", Tier::B),
            raw(held_c, "監査用の十分に長いTier C文脈です", Tier::C),
            raw(train, "記事をまたぐ完全重複の文脈です", Tier::A),
            raw(held_a + 10_000, "記事をまたぐ完全重複の文脈です", Tier::A),
            raw(
                tuning,
                "記事をまたぐ、レイアウトだけが違う長い文脈です。",
                Tier::B,
            ),
            raw(
                held_b + 10_000,
                "記事をまたぐ レイアウトだけが違う長い文脈です",
                Tier::B,
            ),
        ];
        // Freeze candidate identity independently from article identity so the
        // duplicate pairs represent the same actual Sakura snapshot candidates.
        for record in &mut records {
            record.candidates = vec![candidate(1, "仮名")];
        }
        // Ensure the manually offset article IDs really cross a split. Find a
        // replacement deterministically when they do not.
        records[4].article_id = article_for_split(Split::HeldOut, held_a + 10_000);
        records[4].revision_id = records[4].article_id + 100;
        records[4].snapshot_fingerprint = format!("{:064x}", records[4].article_id);
        records[6].article_id = article_for_split(Split::HeldOut, held_b + 10_000);
        records[6].revision_id = records[6].article_id + 100;
        records[6].snapshot_fingerprint = format!("{:064x}", records[6].article_id);

        let input = temporary.0.join("raw.jsonl");
        let mut input_bytes = Vec::new();
        for record in &records {
            serde_json::to_writer(&mut input_bytes, record).expect("record JSON");
            input_bytes.push(b'\n');
        }
        fs::write(&input, input_bytes).expect("write records");

        let first_output = temporary.0.join("dataset-one");
        let second_output = temporary.0.join("dataset-two");
        let base = BuildConfig {
            records: input,
            source_manifest,
            output_directory: first_output.clone(),
            generator_sha256: "44".repeat(32),
            dictionary_sha256: "55".repeat(32),
            audit_tier_a: MINIMUM_TIER_A_AUDIT,
            audit_tier_b: 1,
            audit_tier_c: 1,
        };
        let first_manifest = build_dataset(&base).expect("first build");
        let mut second = base.clone();
        second.output_directory = second_output.clone();
        let second_manifest = build_dataset(&second).expect("second build");
        assert_eq!(first_manifest, second_manifest);
        assert_eq!(first_manifest.deduplication.input_records, 7);
        assert_eq!(first_manifest.deduplication.accepted_records, 5);
        assert_eq!(first_manifest.deduplication.exact_duplicates_removed, 1);
        assert_eq!(first_manifest.deduplication.near_duplicates_removed, 1);
        assert_eq!(
            first_manifest
                .deduplication
                .cross_split_exact_duplicates_removed,
            1
        );
        assert_eq!(
            first_manifest
                .deduplication
                .cross_split_near_duplicates_removed,
            1
        );
        assert!(!first_manifest.audit_gate.tier_a_requirement_met);

        for file in [
            "train.jsonl",
            "tuning.jsonl",
            "held-out.jsonl",
            "audit-tier-a.jsonl",
            "audit-tier-b.jsonl",
            "audit-tier-c.jsonl",
            "manifest.json",
        ] {
            assert_eq!(
                fs::read(first_output.join(file)).expect("first artifact"),
                fs::read(second_output.join(file)).expect("second artifact")
            );
        }
        fs::OpenOptions::new()
            .append(true)
            .open(second_output.join("train.jsonl"))
            .and_then(|mut file| file.write_all(b"{}\n"))
            .expect("tamper artifact");
        assert!(verify_dataset(&second_output).is_err());
    }

    fn article_for_split(expected: Split, start: u64) -> u64 {
        (start..)
            .find(|article_id| split_for_article("test-source", *article_id) == expected)
            .expect("article for split")
    }
}
