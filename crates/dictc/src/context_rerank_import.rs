//! Fail-closed adapter from Sakura-Rerank research snapshots to Issue #34 replay records.
//!
//! The adapter is offline-only. It independently binds the aggregate manifests,
//! validates every converter candidate and joins source spans by stable id. Raw
//! text stays in caller-owned external artifacts and no production code calls
//! this module.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::context_dataset::{
    CandidateAuthority, CandidateRecord, CandidateSource, DictionaryKind, RawReplayRecord, Tier,
    RECORD_SCHEMA_VERSION,
};

const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MAX_CONTEXT_BYTES: usize = 512;

#[derive(Debug, Clone)]
pub struct RerankImportConfig {
    pub source_spans: PathBuf,
    pub source_span_manifest: PathBuf,
    pub expected_source_span_manifest_sha256: String,
    pub exporter_records: PathBuf,
    pub snapshot_manifest: PathBuf,
    pub expected_snapshot_manifest_sha256: String,
    pub source_id: String,
    pub output_directory: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RerankImportManifest {
    pub schema_version: u16,
    pub manifest_kind: String,
    pub source_id: String,
    pub source_span_manifest_sha256: String,
    pub source_span_sha256: String,
    pub snapshot_manifest_sha256: String,
    pub exporter_records_sha256: String,
    pub output_records_sha256: String,
    pub sakura_input_head: String,
    pub dictionary_sha256: String,
    pub exporter_git_sha: String,
    pub exporter_binary_sha256: String,
    pub records: usize,
    pub candidates: usize,
    pub tier_a_records: usize,
    pub tier_c_records: usize,
    pub raw_text_in_manifest: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceSpanManifest {
    schema_version: u16,
    manifest_kind: String,
    verification_status: String,
    snapshot_date: String,
    jawiki_local_sha256: String,
    dictionary_index_sha256: String,
    extractor_git_sha: String,
    cleaner_version: String,
    config: serde_json::Value,
    eligible_dictionary_surface_count: u64,
    record_count: usize,
    content_sha256: String,
    counts: serde_json::Value,
    raw_text_in_report: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotManifest {
    schema_version: u16,
    manifest_kind: String,
    verification_status: String,
    snapshot_date: String,
    source_span_extractor_git_sha: String,
    source_span_content_sha256: String,
    dictionary_indexer_git_sha: String,
    dictionary_index_content_sha256: String,
    request_builder_git_sha: String,
    request_record_count: usize,
    request_content_sha256: String,
    request_report_sha256: String,
    exporter_identity_manifest_sha256: String,
    exporter_git_sha: String,
    exporter_binary_sha256: String,
    sakura_input_head: String,
    dictionary_sha256: String,
    requested_limit: usize,
    effective_converter_bound: usize,
    user_dictionary_enabled: bool,
    record_count: usize,
    candidate_count: usize,
    search_exhausted_record_count: usize,
    truncated_record_count: usize,
    content_sha256: String,
    report_sha256: String,
    reproduction_run_count: usize,
    raw_text_in_manifest: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceSpan {
    schema_version: u16,
    record_type: String,
    stable_id: String,
    source: SpanSource,
    committed_prefix: String,
    gold_surface: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpanSource {
    corpus: String,
    snapshot_date: String,
    article_id: String,
    page_id: String,
    revision_id: String,
    paragraph_hash: String,
    sentence_hash: String,
    sentence_shingle_hashes: Vec<String>,
    template_cluster_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportRecord {
    schema_version: u16,
    record_type: String,
    stable_id: String,
    reading: String,
    converter_provenance: ConverterProvenance,
    candidate_snapshots: CandidateSnapshots,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConverterProvenance {
    kind: String,
    sakura_input_head: String,
    dictionary_sha256: String,
    feature_contract_version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateSnapshots {
    training_top32: CandidateSnapshot,
    production_top6: CandidateSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateSnapshot {
    limit: usize,
    source: String,
    feature_contract_version: u16,
    reading: String,
    candidates: Vec<ExportCandidate>,
    content_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exporter_run: Option<ExporterRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExporterRun {
    contract_version: u16,
    verification_status: String,
    exporter_git_sha: String,
    exporter_binary_sha256: String,
    requested_limit: usize,
    effective_converter_bound: usize,
    returned_count: usize,
    result_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportCandidate {
    rank: usize,
    surface: String,
    local_cost: i64,
    source_category: String,
    fingerprint: String,
    system_entry_index: Option<u32>,
    segments: Vec<ExportSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportSegment {
    reading_start: usize,
    reading_end: usize,
    text_start: usize,
    text_end: usize,
    left_id: u16,
    right_id: u16,
    flags: u16,
    source_category: String,
}

pub fn import_rerank_snapshot(config: &RerankImportConfig) -> Result<RerankImportManifest, String> {
    if config.source_id.is_empty() || config.output_directory.exists() {
        return Err("source id must be nonempty and output directory must not exist".into());
    }
    let source_manifest_hash = hash_file(&config.source_span_manifest)?;
    let snapshot_manifest_hash = hash_file(&config.snapshot_manifest)?;
    if !valid_sha256(&config.expected_source_span_manifest_sha256)
        || !valid_sha256(&config.expected_snapshot_manifest_sha256)
        || source_manifest_hash != config.expected_source_span_manifest_sha256
        || snapshot_manifest_hash != config.expected_snapshot_manifest_sha256
    {
        return Err("manifest hash does not match the caller-pinned identity".into());
    }
    let source_manifest: SourceSpanManifest = read_json(&config.source_span_manifest)?;
    let snapshot_manifest: SnapshotManifest = read_json(&config.snapshot_manifest)?;
    validate_manifests(&source_manifest, &snapshot_manifest)?;

    let source_hash = hash_file(&config.source_spans)?;
    let exporter_hash = hash_file(&config.exporter_records)?;
    if source_hash != source_manifest.content_sha256
        || source_hash != snapshot_manifest.source_span_content_sha256
        || exporter_hash != snapshot_manifest.content_sha256
    {
        return Err("input artifact hash does not match the verified manifests".into());
    }

    fs::create_dir(&config.output_directory)
        .map_err(|error| format!("create {}: {error}", config.output_directory.display()))?;
    let output_path = config.output_directory.join("records.jsonl");
    let output_file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&output_path)
        .map_err(|error| format!("create {}: {error}", output_path.display()))?;
    let mut output = BufWriter::new(output_file);
    let mut output_hash = Sha256::new();
    let mut ordinals = BTreeMap::<u64, u32>::new();
    let mut previous_id = None::<String>;
    let mut record_count = 0usize;
    let mut candidate_count = 0usize;
    let mut tier_a = 0usize;
    let mut tier_c = 0usize;

    let mut spans = jsonl_lines::<SourceSpan>(&config.source_spans)?;
    let mut exports = jsonl_lines::<ExportRecord>(&config.exporter_records)?;
    loop {
        let pair = (spans.next().transpose()?, exports.next().transpose()?);
        let (span, export) = match pair {
            (Some(span), Some(export)) => (span, export),
            (None, None) => break,
            _ => return Err("source spans and exporter records have different counts".into()),
        };
        record_count += 1;
        if record_count > MAX_RECORDS
            || previous_id.as_ref().is_some_and(|id| id >= &span.stable_id)
            || span.stable_id != export.stable_id
        {
            return Err("stable ids must be sorted, unique, and joined exactly".into());
        }
        previous_id = Some(span.stable_id.clone());
        validate_source_span(&span, &source_manifest.snapshot_date)?;
        validate_export(&export, &snapshot_manifest)?;

        // Sakura-Rerank's `article_id` is a namespaced stable string while the
        // paired MediaWiki `page_id` is the canonical nonzero numeric article
        // identity expected by the Phase 5B split contract.
        let article_id = parse_nonzero(&span.source.page_id, "page id")?;
        let revision_id = parse_nonzero(&span.source.revision_id, "revision id")?;
        let ordinal = ordinals.entry(article_id).or_insert(0);
        let sample_ordinal = *ordinal;
        *ordinal = ordinal
            .checked_add(1)
            .ok_or_else(|| "sample ordinal overflow".to_string())?;

        let top32 = &export.candidate_snapshots.training_top32;
        let expected = unique_surface_index(&top32.candidates, &span.gold_surface)?;
        let tier = if expected.is_some() { Tier::A } else { Tier::C };
        tier_a += usize::from(tier == Tier::A);
        tier_c += usize::from(tier == Tier::C);
        let candidates = top32
            .candidates
            .iter()
            .map(|candidate| import_candidate(candidate, &export.reading))
            .collect::<Result<Vec<_>, _>>()?;
        candidate_count += candidates.len();
        let record = RawReplayRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            source_id: config.source_id.clone(),
            article_id,
            revision_id,
            sample_ordinal,
            tier,
            snapshot_fingerprint: top32.content_sha256.clone(),
            context: utf8_tail(&span.committed_prefix, MAX_CONTEXT_BYTES).to_string(),
            reading: export.reading.clone(),
            candidates,
            expected_candidate_index: expected,
        };
        let mut bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        output
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        output_hash.update(&bytes);
    }
    if record_count == 0
        || record_count != source_manifest.record_count
        || record_count != snapshot_manifest.record_count
        || candidate_count != snapshot_manifest.candidate_count
    {
        return Err("record or candidate accounting differs from the verified manifests".into());
    }
    output.flush().map_err(|error| error.to_string())?;
    output
        .get_ref()
        .sync_all()
        .map_err(|error| error.to_string())?;
    let manifest = RerankImportManifest {
        schema_version: 1,
        manifest_kind: "sakura_context_rerank_import".into(),
        source_id: config.source_id.clone(),
        source_span_manifest_sha256: source_manifest_hash,
        source_span_sha256: source_hash,
        snapshot_manifest_sha256: snapshot_manifest_hash,
        exporter_records_sha256: exporter_hash,
        output_records_sha256: hex_digest(output_hash.finalize()),
        sakura_input_head: snapshot_manifest.sakura_input_head,
        dictionary_sha256: snapshot_manifest.dictionary_sha256,
        exporter_git_sha: snapshot_manifest.exporter_git_sha,
        exporter_binary_sha256: snapshot_manifest.exporter_binary_sha256,
        records: record_count,
        candidates: candidate_count,
        tier_a_records: tier_a,
        tier_c_records: tier_c,
        raw_text_in_manifest: false,
    };
    write_new_json(&config.output_directory.join("manifest.json"), &manifest)?;
    Ok(manifest)
}

pub fn verify_rerank_import(directory: &Path) -> Result<RerankImportManifest, String> {
    let manifest: RerankImportManifest = read_json(&directory.join("manifest.json"))?;
    if manifest.schema_version != 1
        || manifest.manifest_kind != "sakura_context_rerank_import"
        || manifest.raw_text_in_manifest
        || !valid_sha256(&manifest.source_span_manifest_sha256)
        || !valid_sha256(&manifest.snapshot_manifest_sha256)
        || !valid_sha256(&manifest.output_records_sha256)
    {
        return Err("rerank import manifest contract is invalid".into());
    }
    let records_path = directory.join("records.jsonl");
    if hash_file(&records_path)? != manifest.output_records_sha256 {
        return Err("rerank import records hash mismatch".into());
    }
    let mut records = 0usize;
    let mut candidates = 0usize;
    let mut tier_a = 0usize;
    let mut tier_c = 0usize;
    for record in jsonl_lines::<RawReplayRecord>(&records_path)? {
        let record = record?;
        if record.schema_version != RECORD_SCHEMA_VERSION
            || record.source_id != manifest.source_id
            || !valid_sha256(&record.snapshot_fingerprint)
        {
            return Err("rerank import record contract is invalid".into());
        }
        records += 1;
        candidates += record.candidates.len();
        tier_a += usize::from(record.tier == Tier::A);
        tier_c += usize::from(record.tier == Tier::C);
    }
    if (records, candidates, tier_a, tier_c)
        != (
            manifest.records,
            manifest.candidates,
            manifest.tier_a_records,
            manifest.tier_c_records,
        )
    {
        return Err("rerank import accounting mismatch".into());
    }
    Ok(manifest)
}

fn validate_manifests(
    source: &SourceSpanManifest,
    snapshot: &SnapshotManifest,
) -> Result<(), String> {
    if source.schema_version != 1
        || source.manifest_kind != "jawiki_tier_a_source_spans"
        || source.verification_status != "verified"
        || source.raw_text_in_report
        || snapshot.schema_version != 1
        || snapshot.manifest_kind != "jawiki_research_top32_snapshot"
        || snapshot.verification_status != "verified"
        || snapshot.raw_text_in_manifest
        || snapshot.user_dictionary_enabled
        || snapshot.requested_limit != 32
        || snapshot.effective_converter_bound != 32
        || snapshot.reproduction_run_count < 2
        || snapshot.snapshot_date != source.snapshot_date
        || snapshot.source_span_extractor_git_sha != source.extractor_git_sha
        || snapshot.source_span_content_sha256 != source.content_sha256
        || snapshot.dictionary_index_content_sha256 != source.dictionary_index_sha256
        || snapshot.record_count != source.record_count
        || snapshot.request_record_count != source.record_count
        || snapshot.search_exhausted_record_count + snapshot.truncated_record_count
            != snapshot.record_count
    {
        return Err("rerank aggregate manifests are inconsistent or unverified".into());
    }
    for hash in [
        &source.jawiki_local_sha256,
        &source.dictionary_index_sha256,
        &source.content_sha256,
        &snapshot.dictionary_index_content_sha256,
        &snapshot.request_content_sha256,
        &snapshot.request_report_sha256,
        &snapshot.exporter_identity_manifest_sha256,
        &snapshot.exporter_binary_sha256,
        &snapshot.dictionary_sha256,
        &snapshot.content_sha256,
        &snapshot.report_sha256,
    ] {
        if !valid_sha256(hash) {
            return Err("rerank manifest contains an invalid SHA-256".into());
        }
    }
    for git_sha in [
        &source.extractor_git_sha,
        &snapshot.dictionary_indexer_git_sha,
        &snapshot.request_builder_git_sha,
        &snapshot.exporter_git_sha,
        &snapshot.sakura_input_head,
    ] {
        if !valid_hex(git_sha, 40) {
            return Err("rerank manifest contains an invalid Git identity".into());
        }
    }
    // Read otherwise aggregate-only fields so adding them cannot silently turn
    // this strict manifest into a partially interpreted contract.
    if source.cleaner_version.is_empty()
        || source.eligible_dictionary_surface_count == 0
        || !source.config.is_object()
        || !source.counts.is_object()
    {
        return Err("source span manifest metadata is incomplete".into());
    }
    Ok(())
}

fn validate_source_span(span: &SourceSpan, snapshot_date: &str) -> Result<(), String> {
    if span.schema_version != 1
        || span.record_type != "jawiki_tier_a_source_span"
        || span.stable_id.is_empty()
        || span.stable_id.len() > 128
        || span.source.corpus != "jawiki"
        || span.source.snapshot_date != snapshot_date
        || span.committed_prefix.is_empty()
        || span.gold_surface.is_empty()
        || span.source.page_id.is_empty()
        || span.source.article_id.is_empty()
        || span.source.sentence_shingle_hashes.is_empty()
    {
        return Err("source span contract is invalid".into());
    }
    for hash in [&span.source.paragraph_hash, &span.source.sentence_hash]
        .into_iter()
        .chain(span.source.sentence_shingle_hashes.iter())
    {
        if !valid_sha256(hash) {
            return Err("source span hash is invalid".into());
        }
    }
    if span.source.template_cluster_id.as_deref() == Some("") {
        return Err("empty template cluster id".into());
    }
    Ok(())
}

fn validate_export(export: &ExportRecord, manifest: &SnapshotManifest) -> Result<(), String> {
    if export.schema_version != 3
        || export.record_type != "research_converter_snapshot"
        || export.reading.is_empty()
        || export.converter_provenance.kind != "sakura_input_converter_export"
        || export.converter_provenance.feature_contract_version != 1
        || export.converter_provenance.sakura_input_head != manifest.sakura_input_head
        || export.converter_provenance.dictionary_sha256 != manifest.dictionary_sha256
    {
        return Err("export record provenance is invalid".into());
    }
    validate_snapshot(
        &export.candidate_snapshots.training_top32,
        32,
        &export.reading,
        true,
        manifest,
    )?;
    validate_snapshot(
        &export.candidate_snapshots.production_top6,
        6,
        &export.reading,
        false,
        manifest,
    )?;
    if export.candidate_snapshots.production_top6.candidates
        != export.candidate_snapshots.training_top32.candidates[..export
            .candidate_snapshots
            .training_top32
            .candidates
            .len()
            .min(6)]
    {
        return Err("production top-6 is not the top-32 prefix".into());
    }
    Ok(())
}

fn validate_snapshot(
    snapshot: &CandidateSnapshot,
    limit: usize,
    reading: &str,
    require_run: bool,
    manifest: &SnapshotManifest,
) -> Result<(), String> {
    if snapshot.limit != limit
        || snapshot.feature_contract_version != 1
        || snapshot.reading != reading
        || snapshot.candidates.is_empty()
        || snapshot.candidates.len() > limit
        || !valid_sha256(&snapshot.content_sha256)
        || snapshot.source != "sakura_converter_full_reading_nbest"
    {
        return Err("candidate snapshot contract is invalid".into());
    }
    match (&snapshot.exporter_run, require_run) {
        (Some(run), true)
            if run.contract_version == 1
                && run.verification_status == "verified"
                && run.exporter_git_sha == manifest.exporter_git_sha
                && run.exporter_binary_sha256 == manifest.exporter_binary_sha256
                && run.requested_limit == 32
                && run.effective_converter_bound == 32
                && run.returned_count == snapshot.candidates.len()
                && matches!(run.result_status.as_str(), "search_exhausted" | "truncated") => {}
        (None, false) => {}
        _ => return Err("candidate snapshot exporter evidence is invalid".into()),
    }
    let mut surfaces = BTreeSet::new();
    for (rank, candidate) in snapshot.candidates.iter().enumerate() {
        validate_export_candidate(candidate, rank, reading)?;
        if !surfaces.insert(candidate.surface.as_str()) {
            return Err("candidate surfaces must be unique".into());
        }
    }
    Ok(())
}

fn validate_export_candidate(
    candidate: &ExportCandidate,
    rank: usize,
    reading: &str,
) -> Result<(), String> {
    if candidate.rank != rank
        || candidate.surface.is_empty()
        || candidate.fingerprint != candidate_fingerprint(&candidate.surface, candidate.local_cost)
        || candidate.segments.is_empty()
        || candidate.segments.len() > 18
    {
        return Err("converter candidate identity is invalid".into());
    }
    let mut reading_at = 0usize;
    let mut text_at = 0usize;
    let reading_boundaries = utf8_boundaries(reading);
    let text_boundaries = utf8_boundaries(&candidate.surface);
    let mut categories = BTreeSet::new();
    for segment in &candidate.segments {
        if segment.reading_start != reading_at
            || segment.text_start != text_at
            || segment.reading_end <= reading_at
            || segment.text_end <= text_at
            || !reading_boundaries.contains(&segment.reading_end)
            || !text_boundaries.contains(&segment.text_end)
            || !matches!(
                segment.source_category.as_str(),
                "system_dictionary"
                    | "reading_fallback"
                    | "katakana_fallback"
                    | "generated_literal"
            )
        {
            return Err("converter segment contract is invalid".into());
        }
        reading_at = segment.reading_end;
        text_at = segment.text_end;
        categories.insert(segment.source_category.as_str());
    }
    if reading_at != reading.len() || text_at != candidate.surface.len() {
        return Err("converter segments do not cover reading and surface".into());
    }
    let category = if categories.len() == 1 {
        *categories.first().expect("one category")
    } else {
        "mixed"
    };
    if category != candidate.source_category {
        return Err("candidate source does not match its segments".into());
    }
    let exact_system = candidate.segments.len() == 1 && category == "system_dictionary";
    if exact_system != candidate.system_entry_index.is_some() {
        return Err("candidate dictionary identity is inconsistent".into());
    }
    Ok(())
}

fn import_candidate(candidate: &ExportCandidate, reading: &str) -> Result<CandidateRecord, String> {
    let all_system = candidate
        .segments
        .iter()
        .all(|segment| segment.source_category == "system_dictionary");
    let (dictionary_kind, dictionary_ordinal) = match candidate.system_entry_index {
        Some(index) => (DictionaryKind::System, Some(index)),
        None => (DictionaryKind::None, None),
    };
    let base_cost = i32::try_from(candidate.local_cost)
        .map_err(|_| "converter local cost is outside the runtime replay range".to_string())?;
    Ok(CandidateRecord {
        runtime_candidate_id: candidate.fingerprint.clone(),
        reading: reading.to_string(),
        surface: candidate.surface.clone(),
        dictionary_kind,
        dictionary_ordinal,
        base_cost,
        authority: CandidateAuthority::Ordinary,
        source: if all_system {
            CandidateSource::SystemDictionary
        } else {
            CandidateSource::GeneratedFallback
        },
        right_id: candidate
            .segments
            .last()
            .ok_or_else(|| "candidate has no segments".to_string())?
            .right_id,
        is_it: candidate
            .segments
            .iter()
            .any(|segment| segment.flags & 1 != 0),
    })
}

fn unique_surface_index(
    candidates: &[ExportCandidate],
    surface: &str,
) -> Result<Option<usize>, String> {
    let mut matches = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.surface == surface)
        .map(|(index, _)| index);
    let first = matches.next();
    if matches.next().is_some() {
        return Err("gold surface matches more than one candidate".into());
    }
    Ok(first)
}

fn candidate_fingerprint(surface: &str, local_cost: i64) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in surface
        .as_bytes()
        .iter()
        .copied()
        .chain(local_cost.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn utf8_boundaries(text: &str) -> BTreeSet<usize> {
    text.char_indices()
        .map(|(index, _)| index)
        .chain([text.len()])
        .collect()
}

fn utf8_tail(text: &str, maximum: usize) -> &str {
    if text.len() <= maximum {
        return text;
    }
    let mut start = text.len() - maximum;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn parse_nonzero(value: &str, label: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{label} is not an unsigned integer"))?;
    if parsed == 0 {
        return Err(format!("{label} must be nonzero"));
    }
    Ok(parsed)
}

fn jsonl_lines<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<impl Iterator<Item = Result<T, String>>, String> {
    bounded_file(path).map(|file| {
        BufReader::new(file)
            .lines()
            .enumerate()
            .map(|(index, line)| {
                let line = line.map_err(|error| error.to_string())?;
                if line.is_empty() || line.ends_with('\r') {
                    return Err(format!("line {} is empty or not LF-canonical", index + 1));
                }
                serde_json::from_str(&line).map_err(|error| format!("line {}: {error}", index + 1))
            })
    })
}

fn bounded_file(path: &Path) -> Result<fs::File, String> {
    let file = fs::File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let bytes = file
        .metadata()
        .map_err(|error| format!("metadata {}: {error}", path.display()))?
        .len();
    if bytes == 0 || bytes > MAX_INPUT_BYTES {
        return Err(format!(
            "{} is outside the input size bound",
            path.display()
        ));
    }
    Ok(file)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    serde_json::from_reader(BufReader::new(bounded_file(path)?))
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = bounded_file(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hex_digest(hash.finalize()))
}

fn write_new_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn valid_sha256(value: &str) -> bool {
    valid_hex(value, 64)
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("write to String");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "sakura-context-rerank-import-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
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

    #[test]
    fn fingerprint_matches_the_cross_repository_contract() {
        assert_eq!(candidate_fingerprint("変換", 123), "f7174729a1ab1bbe");
    }

    #[test]
    fn utf8_tail_keeps_complete_newest_scalars() {
        assert_eq!(utf8_tail("abc日本語", 7), "本語");
    }

    #[test]
    fn compound_system_and_generated_paths_map_without_personal_sources() {
        let system = ExportCandidate {
            rank: 0,
            surface: "日本語".into(),
            local_cost: 7,
            source_category: "system_dictionary".into(),
            fingerprint: candidate_fingerprint("日本語", 7),
            system_entry_index: None,
            segments: vec![
                segment("system_dictionary", 0, 3, 0, 3),
                segment("system_dictionary", 3, 6, 3, 9),
            ],
        };
        let generated = ExportCandidate {
            rank: 1,
            surface: "にほんご".into(),
            local_cost: 9,
            source_category: "reading_fallback".into(),
            fingerprint: candidate_fingerprint("にほんご", 9),
            system_entry_index: None,
            segments: vec![segment("reading_fallback", 0, 12, 0, 12)],
        };
        assert_eq!(
            import_candidate(&system, "にほんご").unwrap().source,
            CandidateSource::SystemDictionary
        );
        assert_eq!(
            import_candidate(&generated, "にほんご").unwrap().source,
            CandidateSource::GeneratedFallback
        );
    }

    #[test]
    fn verified_join_is_deterministic_and_hash_tampering_fails_closed() {
        let root = TestDirectory::new();
        let spans_path = root.0.join("spans.jsonl");
        let exports_path = root.0.join("exports.jsonl");
        let source_manifest_path = root.0.join("source-manifest.json");
        let snapshot_manifest_path = root.0.join("snapshot-manifest.json");
        let source = SourceSpan {
            schema_version: 1,
            record_type: "jawiki_tier_a_source_span".into(),
            stable_id: "case-001".into(),
            source: SpanSource {
                corpus: "jawiki".into(),
                snapshot_date: "2026-08-01".into(),
                article_id: "10".into(),
                page_id: "10".into(),
                revision_id: "11".into(),
                paragraph_hash: "1".repeat(64),
                sentence_hash: "2".repeat(64),
                sentence_shingle_hashes: vec!["3".repeat(64)],
                template_cluster_id: None,
            },
            committed_prefix: "これは確定済みの左文脈です。".into(),
            gold_surface: "日本語".into(),
        };
        write_jsonl(&spans_path, &source);
        let candidate = ExportCandidate {
            rank: 0,
            surface: "日本語".into(),
            local_cost: 7,
            source_category: "system_dictionary".into(),
            fingerprint: candidate_fingerprint("日本語", 7),
            system_entry_index: Some(42),
            segments: vec![segment("system_dictionary", 0, 12, 0, 9)],
        };
        let provenance = ConverterProvenance {
            kind: "sakura_input_converter_export".into(),
            sakura_input_head: "a".repeat(40),
            dictionary_sha256: "b".repeat(64),
            feature_contract_version: 1,
        };
        let top32 = CandidateSnapshot {
            limit: 32,
            source: "sakura_converter_full_reading_nbest".into(),
            feature_contract_version: 1,
            reading: "にほんご".into(),
            candidates: vec![candidate.clone()],
            content_sha256: "c".repeat(64),
            exporter_run: Some(ExporterRun {
                contract_version: 1,
                verification_status: "verified".into(),
                exporter_git_sha: "d".repeat(40),
                exporter_binary_sha256: "e".repeat(64),
                requested_limit: 32,
                effective_converter_bound: 32,
                returned_count: 1,
                result_status: "search_exhausted".into(),
            }),
        };
        let top6 = CandidateSnapshot {
            limit: 6,
            exporter_run: None,
            ..top32.clone()
        };
        let export = ExportRecord {
            schema_version: 3,
            record_type: "research_converter_snapshot".into(),
            stable_id: "case-001".into(),
            reading: "にほんご".into(),
            converter_provenance: provenance,
            candidate_snapshots: CandidateSnapshots {
                training_top32: top32,
                production_top6: top6,
            },
        };
        write_jsonl(&exports_path, &export);
        let source_hash = hash_file(&spans_path).unwrap();
        let export_hash = hash_file(&exports_path).unwrap();
        write_new_json(
            &source_manifest_path,
            &SourceSpanManifest {
                schema_version: 1,
                manifest_kind: "jawiki_tier_a_source_spans".into(),
                verification_status: "verified".into(),
                snapshot_date: "2026-08-01".into(),
                jawiki_local_sha256: "f".repeat(64),
                dictionary_index_sha256: "4".repeat(64),
                extractor_git_sha: "5".repeat(40),
                cleaner_version: "fixture-cleaner".into(),
                config: serde_json::json!({"fixture": true}),
                eligible_dictionary_surface_count: 1,
                record_count: 1,
                content_sha256: source_hash.clone(),
                counts: serde_json::json!({"records": 1}),
                raw_text_in_report: false,
            },
        )
        .unwrap();
        write_new_json(
            &snapshot_manifest_path,
            &SnapshotManifest {
                schema_version: 1,
                manifest_kind: "jawiki_research_top32_snapshot".into(),
                verification_status: "verified".into(),
                snapshot_date: "2026-08-01".into(),
                source_span_extractor_git_sha: "5".repeat(40),
                source_span_content_sha256: source_hash,
                dictionary_indexer_git_sha: "6".repeat(40),
                dictionary_index_content_sha256: "4".repeat(64),
                request_builder_git_sha: "7".repeat(40),
                request_record_count: 1,
                request_content_sha256: "8".repeat(64),
                request_report_sha256: "9".repeat(64),
                exporter_identity_manifest_sha256: "0".repeat(64),
                exporter_git_sha: "d".repeat(40),
                exporter_binary_sha256: "e".repeat(64),
                sakura_input_head: "a".repeat(40),
                dictionary_sha256: "b".repeat(64),
                requested_limit: 32,
                effective_converter_bound: 32,
                user_dictionary_enabled: false,
                record_count: 1,
                candidate_count: 1,
                search_exhausted_record_count: 1,
                truncated_record_count: 0,
                content_sha256: export_hash,
                report_sha256: "1".repeat(64),
                reproduction_run_count: 2,
                raw_text_in_manifest: false,
            },
        )
        .unwrap();
        let source_manifest_hash = hash_file(&source_manifest_path).unwrap();
        let snapshot_manifest_hash = hash_file(&snapshot_manifest_path).unwrap();

        let run = |name: &str| {
            import_rerank_snapshot(&RerankImportConfig {
                source_spans: spans_path.clone(),
                source_span_manifest: source_manifest_path.clone(),
                expected_source_span_manifest_sha256: source_manifest_hash.clone(),
                exporter_records: exports_path.clone(),
                snapshot_manifest: snapshot_manifest_path.clone(),
                expected_snapshot_manifest_sha256: snapshot_manifest_hash.clone(),
                source_id: "wikimedia-jawiki-20260801".into(),
                output_directory: root.0.join(name),
            })
        };
        let first = run("run-1").unwrap();
        let second = run("run-2").unwrap();
        assert_eq!(first.output_records_sha256, second.output_records_sha256);
        assert_eq!(verify_rerank_import(&root.0.join("run-1")).unwrap(), first);

        fs::write(&exports_path, b"{}\n").unwrap();
        assert!(run("tampered").is_err());
        assert!(!root.0.join("tampered").exists());
    }

    fn write_jsonl<T: Serialize>(path: &Path, value: &T) {
        let mut bytes = serde_json::to_vec(value).unwrap();
        bytes.push(b'\n');
        fs::write(path, bytes).unwrap();
    }

    fn segment(category: &str, rs: usize, re: usize, ts: usize, te: usize) -> ExportSegment {
        ExportSegment {
            reading_start: rs,
            reading_end: re,
            text_start: ts,
            text_end: te,
            left_id: 1,
            right_id: 2,
            flags: 0,
            source_category: category.into(),
        }
    }
}
