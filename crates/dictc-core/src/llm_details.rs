//! Fail-closed importer for independently reviewed, LLM-authored detail text.
//!
//! This is deliberately a release *gate*, not a best-effort enrichment path.
//! A malformed record makes the entire import fail so a partially reviewed batch
//! can never become a dictionary artifact by accident.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::llm_detail_targets::{
    input_hash, target_hash, validate_target, DictionaryIdentity, Target, TARGET_SCHEMA_VERSION,
};
use crate::{SourceDetail, SourceDetailRelation, SourceEntry};
use sakura_core::dictionary::DetailRelationKind;

pub const SCHEMA_VERSION: &str = "sakura.llm-detail.v1";
pub const DRAFT_SCHEMA_VERSION: &str = "sakura.llm-detail-draft.v1";
pub const RELEASE_MANIFEST_SCHEMA_VERSION: &str = "sakura.llm-detail-release-manifest.v1";
pub const HIGH_CONFIDENCE_MINIMUM: f64 = 0.98;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RejectionReport {
    pub malformed_records: usize,
    pub unknown_targets: usize,
    pub stale_targets: usize,
    pub duplicate_records: usize,
    pub unsafe_text: usize,
    pub insufficient_confidence: usize,
    pub verification_disagreed: usize,
    pub dictionary_mismatch: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Eligible target groups from the independently hash-validated batch.
    pub available_targets: usize,
    pub input_records: usize,
    pub accepted_records: usize,
    /// Validated unique `(surface, reading)` pairs; never raw generated count.
    pub validated_unique_terms: usize,
    /// Target groups for which this JSONL contains no validated record.
    pub uncovered_targets: usize,
    pub emitted_details: usize,
    pub suppressed_by_curated: usize,
    /// A trusted detail already owns this normalized `(surface, reading)` pair,
    /// even if the candidate names a different exact dictionary entry identity.
    pub suppressed_by_existing_pair: usize,
    /// Pair-colliding exact identities that may only be considered by a future
    /// curated exact-identity-fill lane; never emitted by the LLM new-term lane.
    pub exact_identity_fill_candidates: usize,
    pub rejections: RejectionReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub details: Vec<SourceDetail>,
    pub report: ImportReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportError {
    pub report: Box<ImportReport>,
    message: String,
}

type EntryLookup<'a> = BTreeMap<(&'a str, &'a str, u16, u16, i32, i32, u16), &'a SourceEntry>;

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for ImportError {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: String,
    target_hash: String,
    input_hash: String,
    surface: String,
    reading: String,
    definition: String,
    relations: Relations,
    confidence: f64,
    review_state: String,
    generator: Provenance,
    generation_fingerprint: String,
    verification: Verification,
    dictionary_identity: DictionaryIdentity,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseManifest {
    schema_version: String,
    target_manifest_schema_version: String,
    target_manifest_sha256: String,
    target_batch_count: usize,
    batches: Vec<ReleaseBatch>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseBatch {
    batch_index: usize,
    file: String,
    record_count: usize,
    sha256: String,
    target_hashes: Vec<String>,
    generation_fingerprints: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relations {
    pub aliases: Vec<String>,
    pub related: Vec<String>,
    pub similar: Vec<String>,
    pub antonyms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    model: String,
    prompt_version: String,
    schema_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    model: String,
    prompt_version: String,
    schema_version: String,
    fingerprint: String,
    agrees: bool,
}

/// A deliberately non-importable generation artifact. It cannot name a
/// `SourceDetail`; only [`promote_drafts`] can create release JSONL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftRecord {
    schema_version: String,
    target_hash: String,
    input_hash: String,
    surface: String,
    reading: String,
    definition: String,
    relations: Relations,
    confidence: f64,
    review_state: String,
    generator: Provenance,
    generation_fingerprint: String,
    dictionary_identity: DictionaryIdentity,
}

#[derive(Serialize)]
struct DraftFingerprintPayload<'a> {
    schema_version: &'a str,
    target_hash: &'a str,
    input_hash: &'a str,
    surface: &'a str,
    reading: &'a str,
    definition: &'a str,
    relations: &'a Relations,
    confidence: f64,
    review_state: &'a str,
    generator: &'a Provenance,
    dictionary_identity: &'a DictionaryIdentity,
}

/// Canonical fingerprint for a draft JSONL line. Generation tooling must set
/// `generation_fingerprint` to this value after all semantic fields are final.
pub fn draft_fingerprint_from_json(line: &str) -> Result<String, String> {
    reject_duplicate_object_keys(line)?;
    let draft: DraftRecord =
        serde_json::from_str(line).map_err(|_| "malformed draft schema".to_owned())?;
    Ok(draft_fingerprint(&draft))
}

/// A reviewed semantic candidate that can be bound to one committed target.
/// Relations remain draft-only until the independent promotion gate accepts the
/// complete payload and its canonical fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftDefinition {
    pub surface: String,
    pub reading: String,
    pub definition: String,
    pub relations: Relations,
}

/// Builds a complete, non-release JSONL draft batch in committed target order.
/// Every target must have exactly one definition and every emitted fingerprint
/// is calculated from the final semantic payload.
pub fn build_definition_drafts_jsonl(
    targets: &[Target],
    definitions: &[DraftDefinition],
    generator_model: &str,
    prompt_version: &str,
) -> Result<Vec<u8>, String> {
    if targets.is_empty() {
        return Err("draft batch has no committed targets".to_owned());
    }
    if !safe_single_line(generator_model) || !safe_single_line(prompt_version) {
        return Err("unsafe draft generator provenance".to_owned());
    }

    let mut definitions_by_pair = BTreeMap::new();
    for definition in definitions {
        let pair = (
            definition.surface.nfc().collect::<String>(),
            definition.reading.nfc().collect::<String>(),
        );
        if definitions_by_pair.insert(pair, definition).is_some() {
            return Err("duplicate definition pair".to_owned());
        }
    }
    if definitions_by_pair.len() != targets.len() {
        return Err("definition count does not match committed targets".to_owned());
    }

    let mut seen_targets = BTreeSet::new();
    let mut output = Vec::new();
    for (zero_based, target) in targets.iter().enumerate() {
        validate_target(target)?;
        if !seen_targets.insert(target.target_hash.as_str()) {
            return Err("duplicate committed target hash".to_owned());
        }
        let pair = (
            target.surface.nfc().collect::<String>(),
            target.reading.nfc().collect::<String>(),
        );
        let definition = definitions_by_pair
            .remove(&pair)
            .ok_or_else(|| "committed target has no exact definition".to_owned())?;
        let mut draft = DraftRecord {
            schema_version: DRAFT_SCHEMA_VERSION.to_owned(),
            target_hash: target.target_hash.clone(),
            input_hash: target.input_hash.clone(),
            surface: target.surface.clone(),
            reading: target.reading.clone(),
            definition: definition.definition.clone(),
            relations: definition.relations.clone(),
            confidence: HIGH_CONFIDENCE_MINIMUM,
            review_state: "draft_unverified".to_owned(),
            generator: Provenance {
                model: generator_model.to_owned(),
                prompt_version: prompt_version.to_owned(),
                schema_version: DRAFT_SCHEMA_VERSION.to_owned(),
            },
            generation_fingerprint: String::new(),
            dictionary_identity: target.dictionary_identity.clone(),
        };
        draft.generation_fingerprint = draft_fingerprint(&draft);
        validate_draft(
            "generated-definition-draft",
            zero_based + 1,
            &draft,
            &mut ImportReport::default(),
        )
        .map_err(|error| error.to_string())?;
        serde_json::to_writer(&mut output, &draft).map_err(|error| error.to_string())?;
        output.push(b'\n');
    }
    if !definitions_by_pair.is_empty() {
        return Err("definition is outside committed targets".to_owned());
    }
    Ok(output)
}

fn draft_fingerprint(draft: &DraftRecord) -> String {
    let payload = DraftFingerprintPayload {
        schema_version: &draft.schema_version,
        target_hash: &draft.target_hash,
        input_hash: &draft.input_hash,
        surface: &draft.surface,
        reading: &draft.reading,
        definition: &draft.definition,
        relations: &draft.relations,
        confidence: draft.confidence,
        review_state: &draft.review_state,
        generator: &draft.generator,
        dictionary_identity: &draft.dictionary_identity,
    };
    canonical_sha256(&payload)
}

#[derive(Serialize)]
struct ReviewFingerprintPayload<'a> {
    target_hash: &'a str,
    draft_generation_fingerprint: &'a str,
    status: ReviewStatus,
    reason: Option<&'a str>,
    model: Option<&'a str>,
    prompt_version: Option<&'a str>,
    schema_version: Option<&'a str>,
}

/// Canonical audit fingerprint for the exact independent-review decision.
pub fn canonical_review_fingerprint(review: &IndependentReview) -> String {
    let (status, reason, model, prompt_version, schema_version) = match &review.decision {
        ReviewDecision::Approved {
            model,
            prompt_version,
            schema_version,
        } => (
            ReviewStatus::Approved,
            None,
            Some(model.as_str()),
            Some(prompt_version.as_str()),
            Some(schema_version.as_str()),
        ),
        ReviewDecision::Held { reason } => {
            (ReviewStatus::Held, Some(reason.as_str()), None, None, None)
        }
        ReviewDecision::Rejected { reason } => (
            ReviewStatus::Rejected,
            Some(reason.as_str()),
            None,
            None,
            None,
        ),
    };
    canonical_sha256(&ReviewFingerprintPayload {
        target_hash: &review.target_hash,
        draft_generation_fingerprint: &review.draft_generation_fingerprint,
        status,
        reason,
        model,
        prompt_version,
        schema_version,
    })
}

fn canonical_sha256(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("fixed canonical schema serializes");
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(output, "{byte:02x}").expect("String write");
    }
    output
}

#[derive(Debug, Clone)]
pub struct Draft {
    record: DraftRecord,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftReport {
    pub input_records: usize,
    pub validated_unique_terms: usize,
}

#[derive(Debug, Clone)]
pub struct DraftBatch {
    pub drafts: Vec<Draft>,
    pub report: DraftReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved {
        model: String,
        prompt_version: String,
        schema_version: String,
    },
    Held {
        reason: String,
    },
    Rejected {
        reason: String,
    },
}

/// The reviewer has to bind its decision to the exact generated artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndependentReview {
    pub target_hash: String,
    pub draft_generation_fingerprint: String,
    pub review_fingerprint: String,
    pub decision: ReviewDecision,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PromotionReport {
    pub reviewed: usize,
    pub approved: usize,
    pub held: usize,
    pub rejected: usize,
    /// Exact trusted details found during the promotion-time coverage recheck.
    pub suppressed_by_curated: usize,
    pub suppressed_by_existing_pair: usize,
    pub exact_identity_fill_candidates: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    /// Only release-schema lines in this string are eligible for
    /// `import_release_jsonl`.
    pub release_jsonl: String,
    pub report: PromotionReport,
    pub review_outcomes: Vec<ReviewOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Approved,
    Held,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewOutcome {
    pub target_hash: String,
    pub draft_generation_fingerprint: String,
    pub status: ReviewStatus,
    pub reason: Option<String>,
}

/// Parses a non-release draft batch against the same pinned target and current
/// dictionary identity used by release import. Drafts are never returned as
/// source details and never enter `compile_with_details`.
pub fn parse_drafts_jsonl(
    source_name: &str,
    text: &str,
    targets: &[Target],
    entries: &[SourceEntry],
) -> Result<DraftBatch, ImportError> {
    let mut report = ImportReport {
        available_targets: targets.len(),
        ..ImportReport::default()
    };
    validate_targets(targets, &mut report)?;
    let targets_by_hash = targets
        .iter()
        .map(|target| (target.target_hash.as_str(), target))
        .collect::<BTreeMap<_, _>>();
    if targets_by_hash.len() != targets.len() {
        return Err(fail(
            report,
            "target batch contains duplicate target hashes",
        ));
    }
    let entry_identities = entry_lookup(entries);
    let mut seen_targets = BTreeSet::new();
    let mut seen_terms = BTreeSet::new();
    let mut drafts = Vec::new();
    for (zero_based, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        report.input_records = report.input_records.saturating_add(1);
        let line_number = zero_based + 1;
        reject_duplicate_object_keys(line).map_err(|reason| {
            report.rejections.malformed_records =
                report.rejections.malformed_records.saturating_add(1);
            fail(
                report,
                format!("{source_name}:{line_number}: malformed draft: {reason}"),
            )
        })?;
        let draft: DraftRecord = serde_json::from_str(line).map_err(|_| {
            report.rejections.malformed_records =
                report.rejections.malformed_records.saturating_add(1);
            fail(
                report,
                format!("{source_name}:{line_number}: malformed draft schema"),
            )
        })?;
        validate_draft(source_name, line_number, &draft, &mut report)?;
        let target = targets_by_hash
            .get(draft.target_hash.as_str())
            .ok_or_else(|| {
                report.rejections.unknown_targets =
                    report.rejections.unknown_targets.saturating_add(1);
                fail(
                    report,
                    format!("{source_name}:{line_number}: unknown draft target"),
                )
            })?;
        if draft.input_hash != target.input_hash {
            report.rejections.stale_targets = report.rejections.stale_targets.saturating_add(1);
            return Err(fail(
                report,
                format!("{source_name}:{line_number}: stale draft input hash"),
            ));
        }
        if draft.surface != target.surface
            || draft.reading != target.reading
            || draft.dictionary_identity != target.dictionary_identity
            || !seen_targets.insert(draft.target_hash.clone())
            || !seen_terms.insert((draft.surface.clone(), draft.reading.clone()))
        {
            report.rejections.duplicate_records =
                report.rejections.duplicate_records.saturating_add(1);
            return Err(fail(
                report,
                format!("{source_name}:{line_number}: conflicting draft identity"),
            ));
        }
        if target_entries(target, &entry_identities).is_none() {
            report.rejections.dictionary_mismatch =
                report.rejections.dictionary_mismatch.saturating_add(1);
            return Err(fail(
                report,
                format!("{source_name}:{line_number}: draft dictionary mismatch"),
            ));
        }
        report.validated_unique_terms = report.validated_unique_terms.saturating_add(1);
        drafts.push(Draft { record: draft });
    }
    Ok(DraftBatch {
        drafts,
        report: DraftReport {
            input_records: report.input_records,
            validated_unique_terms: report.validated_unique_terms,
        },
    })
}

/// Produces release-only records from individually reviewed drafts. Held and
/// rejected decisions are intentionally absent from output, while their exact
/// outcomes remain in the deterministic evidence report. A missing or duplicate
/// review fails closed.
pub fn promote_drafts(
    drafts: &[Draft],
    reviews: &[IndependentReview],
    targets: &[Target],
    entries: &[SourceEntry],
    existing: &[SourceDetail],
) -> Result<Promotion, ImportError> {
    let mut report = ImportReport::default();
    validate_targets(targets, &mut report)?;
    let targets_by_hash = targets
        .iter()
        .map(|target| (target.target_hash.as_str(), target))
        .collect::<BTreeMap<_, _>>();
    let entry_identities = entry_lookup(entries);
    let existing_identities = existing
        .iter()
        .map(|detail| {
            (
                detail.reading.as_str(),
                detail.surface.as_str(),
                detail.left_id,
                detail.right_id,
            )
        })
        .collect::<BTreeSet<_>>();
    let existing_pairs = existing
        .iter()
        .map(|detail| normalized_pair(&detail.reading, &detail.surface))
        .collect::<BTreeSet<_>>();
    let reviews_by_target = reviews
        .iter()
        .map(|review| (review.target_hash.as_str(), review))
        .collect::<BTreeMap<_, _>>();
    if reviews_by_target.len() != reviews.len() {
        return Err(fail(report, "duplicate independent review target"));
    }
    let mut lines = Vec::new();
    let mut outcome = PromotionReport::default();
    let mut review_outcomes = Vec::new();
    let mut sorted_drafts = drafts.iter().collect::<Vec<_>>();
    sorted_drafts.sort_by(|left, right| left.record.target_hash.cmp(&right.record.target_hash));
    for draft in sorted_drafts {
        let target = targets_by_hash
            .get(draft.record.target_hash.as_str())
            .ok_or_else(|| fail(report, "draft target is absent from current batch"))?;
        if draft.record.input_hash != target.input_hash
            || draft.record.surface != target.surface
            || draft.record.reading != target.reading
            || draft.record.dictionary_identity != target.dictionary_identity
        {
            return Err(fail(report, "draft no longer matches the current target"));
        }
        let current_entries = target_entries(target, &entry_identities).ok_or_else(|| {
            fail(
                report,
                "draft target no longer matches the current dictionary",
            )
        })?;
        let review = reviews_by_target
            .get(draft.record.target_hash.as_str())
            .ok_or_else(|| {
                report.rejections.verification_disagreed =
                    report.rejections.verification_disagreed.saturating_add(1);
                fail(report, "draft is missing an independent review")
            })?;
        outcome.reviewed = outcome.reviewed.saturating_add(1);
        if review.draft_generation_fingerprint != draft.record.generation_fingerprint {
            report.rejections.stale_targets = report.rejections.stale_targets.saturating_add(1);
            return Err(fail(report, "review refers to a different generated draft"));
        }
        if !is_sha256(&review.review_fingerprint)
            || review.review_fingerprint != canonical_review_fingerprint(review)
        {
            report.rejections.verification_disagreed =
                report.rejections.verification_disagreed.saturating_add(1);
            return Err(fail(
                report,
                "review fingerprint does not bind its decision",
            ));
        }
        match &review.decision {
            ReviewDecision::Approved {
                model,
                prompt_version,
                schema_version,
            } => {
                if schema_version != SCHEMA_VERSION
                    || model.is_empty()
                    || prompt_version.is_empty()
                    || model == &draft.record.generator.model
                        && prompt_version == &draft.record.generator.prompt_version
                {
                    report.rejections.verification_disagreed =
                        report.rejections.verification_disagreed.saturating_add(1);
                    return Err(fail(
                        report,
                        "approved review is not independently attributable",
                    ));
                }
                if existing_pairs.contains(&normalized_pair(
                    &draft.record.reading,
                    &draft.record.surface,
                )) {
                    outcome.held = outcome.held.saturating_add(1);
                    outcome.suppressed_by_existing_pair =
                        outcome.suppressed_by_existing_pair.saturating_add(1);
                    outcome.exact_identity_fill_candidates = outcome
                        .exact_identity_fill_candidates
                        .saturating_add(current_entries.len());
                    review_outcomes.push(ReviewOutcome {
                        target_hash: review.target_hash.clone(),
                        draft_generation_fingerprint: review.draft_generation_fingerprint.clone(),
                        status: ReviewStatus::Held,
                        reason: Some(
                            "current curated detail owns this normalized surface/reading pair"
                                .into(),
                        ),
                    });
                    continue;
                }
                let covered = current_entries
                    .iter()
                    .filter(|entry| {
                        existing_identities.contains(&(
                            entry.reading.as_str(),
                            entry.surface.as_str(),
                            entry.left_id,
                            entry.right_id,
                        ))
                    })
                    .count();
                if covered != 0 {
                    outcome.held = outcome.held.saturating_add(1);
                    outcome.suppressed_by_curated =
                        outcome.suppressed_by_curated.saturating_add(covered);
                    review_outcomes.push(ReviewOutcome {
                        target_hash: review.target_hash.clone(),
                        draft_generation_fingerprint: review.draft_generation_fingerprint.clone(),
                        status: ReviewStatus::Held,
                        reason: Some(
                            "current curated detail coverage changed; regenerate target".into(),
                        ),
                    });
                    continue;
                }
                let record = Record {
                    schema_version: SCHEMA_VERSION.into(),
                    target_hash: draft.record.target_hash.clone(),
                    input_hash: draft.record.input_hash.clone(),
                    surface: draft.record.surface.clone(),
                    reading: draft.record.reading.clone(),
                    definition: draft.record.definition.clone(),
                    relations: draft.record.relations.clone(),
                    confidence: draft.record.confidence,
                    review_state: "independently_verified".into(),
                    generator: Provenance {
                        model: draft.record.generator.model.clone(),
                        prompt_version: draft.record.generator.prompt_version.clone(),
                        schema_version: DRAFT_SCHEMA_VERSION.into(),
                    },
                    generation_fingerprint: draft.record.generation_fingerprint.clone(),
                    verification: Verification {
                        model: model.clone(),
                        prompt_version: prompt_version.clone(),
                        schema_version: schema_version.clone(),
                        fingerprint: review.review_fingerprint.clone(),
                        agrees: true,
                    },
                    dictionary_identity: draft.record.dictionary_identity.clone(),
                };
                lines.push(serde_json::to_string(&record).expect("release record serializes"));
                outcome.approved = outcome.approved.saturating_add(1);
                review_outcomes.push(ReviewOutcome {
                    target_hash: review.target_hash.clone(),
                    draft_generation_fingerprint: review.draft_generation_fingerprint.clone(),
                    status: ReviewStatus::Approved,
                    reason: None,
                });
            }
            ReviewDecision::Held { reason } => {
                if !safe_single_line(reason) {
                    return Err(fail(report, "held review has no safe reason"));
                }
                outcome.held = outcome.held.saturating_add(1);
                review_outcomes.push(ReviewOutcome {
                    target_hash: review.target_hash.clone(),
                    draft_generation_fingerprint: review.draft_generation_fingerprint.clone(),
                    status: ReviewStatus::Held,
                    reason: Some(reason.clone()),
                });
            }
            ReviewDecision::Rejected { reason } => {
                if !safe_single_line(reason) {
                    return Err(fail(report, "rejected review has no safe reason"));
                }
                outcome.rejected = outcome.rejected.saturating_add(1);
                review_outcomes.push(ReviewOutcome {
                    target_hash: review.target_hash.clone(),
                    draft_generation_fingerprint: review.draft_generation_fingerprint.clone(),
                    status: ReviewStatus::Rejected,
                    reason: Some(reason.clone()),
                });
            }
        }
    }
    Ok(Promotion {
        release_jsonl: if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        },
        report: outcome,
        review_outcomes,
    })
}

/// Imports a complete release batch. `existing` is the already trusted
/// smile-chat/WordNet detail set and always wins exact-identity collisions.
pub fn import_release_jsonl(
    source_name: &str,
    text: &str,
    targets: &[Target],
    entries: &[SourceEntry],
    existing: &[SourceDetail],
) -> Result<Import, ImportError> {
    let mut report = ImportReport {
        available_targets: targets.len(),
        ..ImportReport::default()
    };
    validate_targets(targets, &mut report)?;
    let targets_by_hash = targets
        .iter()
        .map(|target| (target.target_hash.as_str(), target))
        .collect::<BTreeMap<_, _>>();
    if targets_by_hash.len() != targets.len() {
        return Err(fail(
            report,
            "target batch contains duplicate target hashes; regenerate it",
        ));
    }
    let entry_identities = entry_lookup(entries);
    let existing_identities = existing
        .iter()
        .map(|detail| {
            (
                detail.reading.as_str(),
                detail.surface.as_str(),
                detail.left_id,
                detail.right_id,
            )
        })
        .collect::<BTreeSet<_>>();
    let existing_pairs = existing
        .iter()
        .map(|detail| normalized_pair(&detail.reading, &detail.surface))
        .collect::<BTreeSet<_>>();
    let mut seen_targets = BTreeSet::new();
    let mut seen_terms = BTreeSet::new();
    let mut details = Vec::new();

    for (zero_based, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        report.input_records = report.input_records.saturating_add(1);
        let line_number = zero_based + 1;
        reject_duplicate_object_keys(line).map_err(|reason| {
            report.rejections.malformed_records =
                report.rejections.malformed_records.saturating_add(1);
            fail(
                report,
                format!("{source_name}:{line_number}: malformed JSONL record: {reason}"),
            )
        })?;
        let record: Record = serde_json::from_str(line).map_err(|_| {
            report.rejections.malformed_records =
                report.rejections.malformed_records.saturating_add(1);
            fail(
                report,
                format!("{source_name}:{line_number}: record does not match {SCHEMA_VERSION}"),
            )
        })?;
        validate_record(source_name, line_number, &record, &mut report)?;
        let target = targets_by_hash
            .get(record.target_hash.as_str())
            .ok_or_else(|| {
                report.rejections.unknown_targets =
                    report.rejections.unknown_targets.saturating_add(1);
                fail(
                    report,
                    format!("{source_name}:{line_number}: unknown target_hash"),
                )
            })?;
        if record.input_hash != target.input_hash {
            report.rejections.stale_targets = report.rejections.stale_targets.saturating_add(1);
            return Err(fail(
                report,
                format!("{source_name}:{line_number}: stale input_hash for target"),
            ));
        }
        if record.surface != target.surface
            || record.reading != target.reading
            || record.dictionary_identity != target.dictionary_identity
        {
            report.rejections.dictionary_mismatch =
                report.rejections.dictionary_mismatch.saturating_add(1);
            return Err(fail(
                report,
                format!("{source_name}:{line_number}: dictionary identity does not match target"),
            ));
        }
        if !seen_targets.insert(record.target_hash.clone()) {
            report.rejections.duplicate_records =
                report.rejections.duplicate_records.saturating_add(1);
            return Err(fail(
                report,
                format!("{source_name}:{line_number}: conflicting duplicate target record"),
            ));
        }
        if !seen_terms.insert((record.surface.clone(), record.reading.clone())) {
            report.rejections.duplicate_records =
                report.rejections.duplicate_records.saturating_add(1);
            return Err(fail(
                report,
                format!("{source_name}:{line_number}: duplicate surface/reading record"),
            ));
        }

        report.accepted_records = report.accepted_records.saturating_add(1);
        if existing_pairs.contains(&normalized_pair(&record.reading, &record.surface)) {
            report.suppressed_by_existing_pair =
                report.suppressed_by_existing_pair.saturating_add(1);
            report.exact_identity_fill_candidates = report
                .exact_identity_fill_candidates
                .saturating_add(target.dictionary_identity.entries.len());
            continue;
        }
        let target_entries = target_entries(target, &entry_identities).ok_or_else(|| {
            report.rejections.dictionary_mismatch =
                report.rejections.dictionary_mismatch.saturating_add(1);
            fail(
                report,
                format!("{source_name}:{line_number}: target does not match this dictionary"),
            )
        })?;
        let relations = source_relations(&record.relations);
        report.validated_unique_terms = report.validated_unique_terms.saturating_add(1);
        for entry in target_entries {
            let identity = (
                entry.reading.as_str(),
                entry.surface.as_str(),
                entry.left_id,
                entry.right_id,
            );
            if existing_identities.contains(&identity) {
                report.suppressed_by_curated = report.suppressed_by_curated.saturating_add(1);
                continue;
            }
            details.push(SourceDetail {
                reading: entry.reading.clone(),
                surface: entry.surface.clone(),
                left_id: entry.left_id,
                right_id: entry.right_id,
                description: record.definition.clone(),
                relations: relations.clone(),
            });
            report.emitted_details = report.emitted_details.saturating_add(1);
        }
    }
    report.uncovered_targets = targets.len().saturating_sub(report.validated_unique_terms);
    Ok(Import { details, report })
}

/// Loads a committed release directory without globbing. The manifest binds the
/// release bytes to one exact committed target manifest and enumerates every
/// target batch in order, including empty release batches.
pub fn load_committed_release_jsonl(
    release_directory: &Path,
    target_directory: &Path,
    targets: &[Target],
) -> Result<String, String> {
    let target_manifest = fs::read(target_directory.join("manifest.json"))
        .map_err(|error| format!("read target manifest: {error}"))?;
    let manifest_path = release_directory.join("manifest.json");
    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest: ReleaseManifest = serde_json::from_str(&manifest_text).map_err(|error| {
        format!(
            "{}: invalid release manifest: {error}",
            manifest_path.display()
        )
    })?;
    if manifest.schema_version != RELEASE_MANIFEST_SCHEMA_VERSION
        || manifest.target_manifest_schema_version != TARGET_SCHEMA_VERSION
        || !is_sha256(&manifest.target_manifest_sha256)
        || manifest.target_manifest_sha256 != sha256_hex(&target_manifest)
        || manifest.target_batch_count == 0
        || manifest.batches.len() != manifest.target_batch_count
    {
        return Err("release manifest is not pinned to the committed target manifest".into());
    }
    let known_targets = targets
        .iter()
        .map(|target| target.target_hash.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen_records = BTreeSet::new();
    let mut output = String::new();
    for (zero_based, batch) in manifest.batches.iter().enumerate() {
        let expected_index = zero_based + 1;
        let expected_file = format!("{expected_index:06}.release.jsonl");
        if batch.batch_index != expected_index
            || batch.file != expected_file
            || batch.target_hashes.len() != batch.record_count
            || batch.generation_fingerprints.len() != batch.record_count
            || !is_sha256(&batch.sha256)
            || batch
                .target_hashes
                .iter()
                .any(|hash| !known_targets.contains(hash.as_str()))
            || batch
                .target_hashes
                .iter()
                .any(|hash| !seen_records.insert(hash.as_str()))
            || batch
                .generation_fingerprints
                .iter()
                .any(|hash| !is_sha256(hash))
        {
            return Err(format!(
                "release manifest batch {expected_index} is invalid"
            ));
        }
        let path = release_directory.join(&batch.file);
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        if sha256_hex(&bytes) != batch.sha256 {
            return Err(format!(
                "{}: release batch SHA-256 mismatch",
                path.display()
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{}: release batch is not UTF-8", path.display()))?;
        let records = release_manifest_records(&path, &text)?;
        if records.len() != batch.record_count
            || records
                .iter()
                .map(|record| &record.target_hash)
                .collect::<Vec<_>>()
                != batch.target_hashes.iter().collect::<Vec<_>>()
            || records
                .iter()
                .map(|record| &record.generation_fingerprint)
                .collect::<Vec<_>>()
                != batch.generation_fingerprints.iter().collect::<Vec<_>>()
        {
            return Err(format!(
                "{}: manifest record binding mismatch",
                path.display()
            ));
        }
        output.push_str(&text);
        if !text.is_empty() && !text.ends_with('\n') {
            output.push('\n');
        }
    }
    Ok(output)
}

fn release_manifest_records(path: &Path, text: &str) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for (zero_based, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        reject_duplicate_object_keys(line)
            .map_err(|reason| format!("{}:{}: {reason}", path.display(), zero_based + 1))?;
        records.push(serde_json::from_str(line).map_err(|_| {
            format!(
                "{}:{}: malformed release record",
                path.display(),
                zero_based + 1
            )
        })?);
    }
    Ok(records)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(output, "{byte:02x}").expect("String write");
    }
    output
}

fn validate_targets(targets: &[Target], report: &mut ImportReport) -> Result<(), ImportError> {
    let mut seen = BTreeSet::new();
    for target in targets {
        if validate_target(target).is_err()
            || target.schema_version != TARGET_SCHEMA_VERSION
            || target.target_hash != target_hash(target)
            || target.input_hash != input_hash(target)
            || !is_sha256(&target.target_hash)
            || !is_sha256(&target.input_hash)
            || !seen.insert(target.target_hash.as_str())
        {
            report.rejections.stale_targets = report.rejections.stale_targets.saturating_add(1);
            return Err(fail(*report, "unvalidated or stale target input"));
        }
    }
    Ok(())
}

fn entry_lookup(entries: &[SourceEntry]) -> EntryLookup<'_> {
    entries
        .iter()
        .map(|entry| {
            (
                (
                    entry.reading.as_str(),
                    entry.surface.as_str(),
                    entry.left_id,
                    entry.right_id,
                    entry.word_cost,
                    entry.prediction_cost,
                    entry.flags.bits(),
                ),
                entry,
            )
        })
        .collect()
}

fn target_entries<'a>(
    target: &'a Target,
    entries: &EntryLookup<'a>,
) -> Option<Vec<&'a SourceEntry>> {
    let mut output = Vec::new();
    for identity in &target.dictionary_identity.entries {
        let entry = entries.get(&(
            target.reading.as_str(),
            target.surface.as_str(),
            identity.left_id,
            identity.right_id,
            identity.word_cost,
            identity.prediction_cost,
            identity.flags,
        ))?;
        output.push(*entry);
    }
    (!output.is_empty()).then_some(output)
}

fn validate_draft(
    source: &str,
    line: usize,
    draft: &DraftRecord,
    report: &mut ImportReport,
) -> Result<(), ImportError> {
    if draft.schema_version != DRAFT_SCHEMA_VERSION
        || draft.generator.schema_version != DRAFT_SCHEMA_VERSION
        || draft.review_state != "draft_unverified"
        || !is_sha256(&draft.target_hash)
        || !is_sha256(&draft.input_hash)
        || !is_sha256(&draft.generation_fingerprint)
        || draft.generation_fingerprint != draft_fingerprint(draft)
    {
        report.rejections.malformed_records = report.rejections.malformed_records.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: invalid draft provenance"),
        ));
    }
    if !draft.confidence.is_finite()
        || draft.confidence < HIGH_CONFIDENCE_MINIMUM
        || draft.confidence > 1.0
    {
        report.rejections.insufficient_confidence =
            report.rejections.insufficient_confidence.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: draft confidence below release threshold"),
        ));
    }
    for value in [
        &draft.surface,
        &draft.reading,
        &draft.generator.model,
        &draft.generator.prompt_version,
    ] {
        if !safe_single_line(value) {
            report.rejections.unsafe_text = report.rejections.unsafe_text.saturating_add(1);
            return Err(fail(*report, format!("{source}:{line}: unsafe draft text")));
        }
    }
    if !valid_definition(&draft.definition)
        || validate_relations(&draft.surface, &draft.relations).is_err()
    {
        report.rejections.unsafe_text = report.rejections.unsafe_text.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: unsafe draft detail content"),
        ));
    }
    Ok(())
}

fn validate_record(
    source: &str,
    line: usize,
    record: &Record,
    report: &mut ImportReport,
) -> Result<(), ImportError> {
    let fail_text = |report: &mut ImportReport, message: &str| {
        report.rejections.unsafe_text = report.rejections.unsafe_text.saturating_add(1);
        fail(*report, format!("{source}:{line}: {message}"))
    };
    if record.schema_version != SCHEMA_VERSION
        || record.generator.schema_version != DRAFT_SCHEMA_VERSION
        || record.verification.schema_version != SCHEMA_VERSION
    {
        report.rejections.malformed_records = report.rejections.malformed_records.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: unknown schema version"),
        ));
    }
    let generated_draft = DraftRecord {
        schema_version: DRAFT_SCHEMA_VERSION.into(),
        target_hash: record.target_hash.clone(),
        input_hash: record.input_hash.clone(),
        surface: record.surface.clone(),
        reading: record.reading.clone(),
        definition: record.definition.clone(),
        relations: record.relations.clone(),
        confidence: record.confidence,
        review_state: "draft_unverified".into(),
        generator: record.generator.clone(),
        generation_fingerprint: record.generation_fingerprint.clone(),
        dictionary_identity: record.dictionary_identity.clone(),
    };
    if record.generation_fingerprint != draft_fingerprint(&generated_draft) {
        report.rejections.malformed_records = report.rejections.malformed_records.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: generation fingerprint mismatch"),
        ));
    }
    if !is_sha256(&record.target_hash)
        || !is_sha256(&record.input_hash)
        || !is_sha256(&record.generation_fingerprint)
        || !is_sha256(&record.verification.fingerprint)
    {
        report.rejections.malformed_records = report.rejections.malformed_records.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: invalid fingerprint"),
        ));
    }
    if record.generation_fingerprint == record.verification.fingerprint
        || record.generator.model == record.verification.model
            && record.generator.prompt_version == record.verification.prompt_version
    {
        report.rejections.verification_disagreed =
            report.rejections.verification_disagreed.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: generation and verification are not independent"),
        ));
    }
    let review = IndependentReview {
        target_hash: record.target_hash.clone(),
        draft_generation_fingerprint: record.generation_fingerprint.clone(),
        review_fingerprint: record.verification.fingerprint.clone(),
        decision: ReviewDecision::Approved {
            model: record.verification.model.clone(),
            prompt_version: record.verification.prompt_version.clone(),
            schema_version: record.verification.schema_version.clone(),
        },
    };
    if record.verification.fingerprint != canonical_review_fingerprint(&review) {
        report.rejections.verification_disagreed =
            report.rejections.verification_disagreed.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: verification fingerprint mismatch"),
        ));
    }
    if !record.confidence.is_finite()
        || record.confidence < HIGH_CONFIDENCE_MINIMUM
        || record.confidence > 1.0
    {
        report.rejections.insufficient_confidence =
            report.rejections.insufficient_confidence.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: confidence is below the release threshold"),
        ));
    }
    if record.review_state != "independently_verified" || !record.verification.agrees {
        report.rejections.verification_disagreed =
            report.rejections.verification_disagreed.saturating_add(1);
        return Err(fail(
            *report,
            format!("{source}:{line}: independent verification did not agree"),
        ));
    }
    for (label, value) in [
        ("surface", &record.surface),
        ("reading", &record.reading),
        ("generator model", &record.generator.model),
        ("generator prompt version", &record.generator.prompt_version),
        ("verification model", &record.verification.model),
        (
            "verification prompt version",
            &record.verification.prompt_version,
        ),
    ] {
        if !safe_single_line(value) {
            return Err(fail_text(report, &format!("unsafe {label}")));
        }
    }
    if !valid_definition(&record.definition) {
        return Err(fail_text(
            report,
            "definition is not concise Japanese prose",
        ));
    }
    validate_relations(&record.surface, &record.relations)
        .map_err(|message| fail_text(report, &message))?;
    Ok(())
}

fn source_relations(relations: &Relations) -> Vec<SourceDetailRelation> {
    let mut output = Vec::new();
    for (kind, values) in [
        (DetailRelationKind::Alias, &relations.aliases),
        (DetailRelationKind::Related, &relations.related),
        (DetailRelationKind::Synonym, &relations.similar),
        (DetailRelationKind::Antonym, &relations.antonyms),
    ] {
        output.extend(
            values
                .iter()
                .cloned()
                .map(|target| SourceDetailRelation { kind, target }),
        );
    }
    output
}

fn validate_relations(surface: &str, relations: &Relations) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for values in [
        &relations.aliases,
        &relations.related,
        &relations.similar,
        &relations.antonyms,
    ] {
        for target in values {
            if !safe_single_line(target) || target == surface {
                return Err("unsafe or self-referential relation".to_owned());
            }
            if !seen.insert(target.as_str()) {
                return Err("duplicate relation".to_owned());
            }
        }
    }
    Ok(())
}

fn safe_single_line(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && value.chars().all(|character| !character.is_control())
}

fn normalized_pair(reading: &str, surface: &str) -> (String, String) {
    (reading.nfc().collect(), surface.nfc().collect())
}

fn valid_definition(value: &str) -> bool {
    safe_single_line(value)
        && value.chars().count() >= 5
        && value.ends_with('\u{3002}')
        && value
            .chars()
            .any(|character| matches!(character, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}'))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn fail(report: ImportReport, message: impl Into<String>) -> ImportError {
    ImportError {
        report: Box::new(report),
        message: message.into(),
    }
}

/// Stable JSON for release evidence. This intentionally omits source text.
pub fn report_json(report: ImportReport) -> String {
    let serialized = serde_json::to_string_pretty(&report).expect("integer-only report serializes");
    let mut output = String::from(
        "{\n  \"schema_version\": \"sakura.llm-detail-import-report.v1\",\n  \"report\": ",
    );
    output.push_str(&serialized);
    output.push_str("\n}\n");
    output
}

/// Stable audit JSON for promotion decisions. Rejected and held decisions are
/// intentionally retained here, never in release JSONL.
pub fn promotion_report_json(promotion: &Promotion) -> String {
    #[derive(Serialize)]
    struct Evidence<'a> {
        schema_version: &'static str,
        report: PromotionReport,
        review_outcomes: &'a [ReviewOutcome],
    }
    let evidence = Evidence {
        schema_version: "sakura.llm-detail-promotion-report.v1",
        report: promotion.report,
        review_outcomes: &promotion.review_outcomes,
    };
    let mut output = serde_json::to_string_pretty(&evidence).expect("promotion report serializes");
    output.push('\n');
    output
}

// serde rejects unknown fields but intentionally accepts duplicate JSON keys.
// Release inputs must reject them at every nesting level, so scan the one JSON
// value first and hand actual string decoding/number grammar to serde afterwards.
fn reject_duplicate_object_keys(text: &str) -> Result<(), &'static str> {
    let bytes = text.as_bytes();
    let mut index = 0;
    scan_value(bytes, &mut index)?;
    skip_ws(bytes, &mut index);
    if index == bytes.len() {
        Ok(())
    } else {
        Err("trailing data")
    }
}

fn scan_value(bytes: &[u8], index: &mut usize) -> Result<(), &'static str> {
    skip_ws(bytes, index);
    match bytes.get(*index) {
        Some(b'{') => scan_object(bytes, index),
        Some(b'[') => scan_array(bytes, index),
        Some(b'\"') => scan_string(bytes, index).map(|_| ()),
        Some(_) => {
            let start = *index;
            while let Some(byte) = bytes.get(*index) {
                if matches!(byte, b',' | b']' | b'}' | b' ' | b'\t' | b'\r' | b'\n') {
                    break;
                }
                *index += 1;
            }
            (start != *index).then_some(()).ok_or("missing JSON value")
        }
        None => Err("missing JSON value"),
    }
}

fn scan_object(bytes: &[u8], index: &mut usize) -> Result<(), &'static str> {
    *index += 1;
    let mut keys = BTreeSet::new();
    loop {
        skip_ws(bytes, index);
        if bytes.get(*index) == Some(&b'}') {
            *index += 1;
            return Ok(());
        }
        let key = scan_string(bytes, index)?;
        if !keys.insert(key) {
            return Err("duplicate object key");
        }
        skip_ws(bytes, index);
        if bytes.get(*index) != Some(&b':') {
            return Err("object key without colon");
        }
        *index += 1;
        scan_value(bytes, index)?;
        skip_ws(bytes, index);
        match bytes.get(*index) {
            Some(b',') => *index += 1,
            Some(b'}') => {
                *index += 1;
                return Ok(());
            }
            _ => return Err("unterminated object"),
        }
    }
}

fn scan_array(bytes: &[u8], index: &mut usize) -> Result<(), &'static str> {
    *index += 1;
    loop {
        skip_ws(bytes, index);
        if bytes.get(*index) == Some(&b']') {
            *index += 1;
            return Ok(());
        }
        scan_value(bytes, index)?;
        skip_ws(bytes, index);
        match bytes.get(*index) {
            Some(b',') => *index += 1,
            Some(b']') => {
                *index += 1;
                return Ok(());
            }
            _ => return Err("unterminated array"),
        }
    }
}

fn scan_string(bytes: &[u8], index: &mut usize) -> Result<String, &'static str> {
    let start = *index;
    if bytes.get(*index) != Some(&b'\"') {
        return Err("object key is not a string");
    }
    *index += 1;
    loop {
        match bytes.get(*index) {
            Some(b'\"') => {
                *index += 1;
                break;
            }
            Some(b'\\') => {
                *index += 2;
            }
            Some(byte) if *byte >= 0x20 => *index += 1,
            _ => return Err("unterminated string"),
        }
    }
    serde_json::from_slice(&bytes[start..*index]).map_err(|_| "invalid string escape")
}

fn skip_ws(bytes: &[u8], index: &mut usize) {
    while matches!(bytes.get(*index), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        *index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_detail_targets::{input_hash, target_hash, EntryIdentity};
    use crate::parse_entries;

    fn target() -> Target {
        let mut target = Target {
            schema_version: "sakura.llm-detail-target.v1".into(),
            surface: "用語".into(),
            reading: "ようご".into(),
            category_ids: vec![3],
            dictionary_identity: DictionaryIdentity {
                entries: vec![EntryIdentity {
                    left_id: 1,
                    right_id: 1,
                    word_cost: 100,
                    prediction_cost: i32::MAX,
                    flags: 0,
                }],
            },
            target_hash: String::new(),
            input_hash: String::new(),
        };
        target.target_hash = target_hash(&target);
        target.input_hash = input_hash(&target);
        target
    }

    fn entries() -> Vec<SourceEntry> {
        parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nようご\t用語\t1\t1\t100\t-\t\t\n",
        )
        .unwrap()
    }

    #[test]
    fn semantic_draft_builder_binds_every_target_and_preserves_typed_relations() {
        let target = target();
        let definitions = vec![DraftDefinition {
            surface: target.surface.clone(),
            reading: target.reading.clone(),
            definition: "入力や変換で使う言葉の意味を簡潔に説明する文。".into(),
            relations: Relations {
                related: vec!["関連語".into()],
                ..Relations::default()
            },
        }];
        let bytes = build_definition_drafts_jsonl(
            std::slice::from_ref(&target),
            &definitions,
            "generator-a",
            "definition-only-v1",
        )
        .expect("complete exact definitions");
        let jsonl = String::from_utf8(bytes).unwrap();
        assert!(jsonl.contains("\"related\":[\"関連語\"]"));
        let parsed = parse_drafts_jsonl(
            "generated.jsonl",
            &jsonl,
            std::slice::from_ref(&target),
            &entries(),
        )
        .expect("generated draft passes the strict parser");
        assert_eq!(parsed.report.validated_unique_terms, 1);

        assert!(build_definition_drafts_jsonl(
            std::slice::from_ref(&target),
            &[],
            "generator-a",
            "definition-only-v1",
        )
        .is_err());
        assert!(build_definition_drafts_jsonl(
            std::slice::from_ref(&target),
            &[definitions[0].clone(), definitions[0].clone()],
            "generator-a",
            "definition-only-v1",
        )
        .is_err());
    }

    fn record(target: &Target) -> String {
        let mut value = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "target_hash": target.target_hash,
            "input_hash": target.input_hash,
            "surface": target.surface,
            "reading": target.reading,
            "definition": "入力や変換で使う語句。",
            "relations": {"aliases": ["別名"], "related": [], "similar": [], "antonyms": []},
            "confidence": 0.99,
            "review_state": "independently_verified",
            "generator": {"model": "generator-a", "prompt_version": "prompt-1", "schema_version": DRAFT_SCHEMA_VERSION},
            "generation_fingerprint": "",
            "verification": {"model": "verifier-b", "prompt_version": "prompt-2", "schema_version": SCHEMA_VERSION, "fingerprint": "", "agrees": true},
            "dictionary_identity": target.dictionary_identity,
        });
        let mut draft_value = value.clone();
        let draft_object = draft_value.as_object_mut().expect("object");
        draft_object.insert(
            "schema_version".into(),
            serde_json::json!(DRAFT_SCHEMA_VERSION),
        );
        draft_object.insert("review_state".into(), serde_json::json!("draft_unverified"));
        draft_object.remove("verification");
        let generation_fingerprint =
            draft_fingerprint_from_json(&draft_value.to_string()).expect("draft hash");
        value.as_object_mut().expect("object").insert(
            "generation_fingerprint".into(),
            serde_json::json!(generation_fingerprint.clone()),
        );
        let mut review = IndependentReview {
            target_hash: target.target_hash.clone(),
            draft_generation_fingerprint: generation_fingerprint,
            review_fingerprint: String::new(),
            decision: ReviewDecision::Approved {
                model: "verifier-b".into(),
                prompt_version: "prompt-2".into(),
                schema_version: SCHEMA_VERSION.into(),
            },
        };
        review.review_fingerprint = canonical_review_fingerprint(&review);
        value
            .as_object_mut()
            .expect("object")
            .get_mut("verification")
            .expect("verification")
            .as_object_mut()
            .expect("object")
            .insert(
                "fingerprint".into(),
                serde_json::json!(review.review_fingerprint),
            );
        value.to_string()
    }

    fn draft_record(target: &Target) -> String {
        let mut value: serde_json::Value = serde_json::from_str(&record(target)).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert(
            "schema_version".into(),
            serde_json::json!(DRAFT_SCHEMA_VERSION),
        );
        object.insert("review_state".into(), serde_json::json!("draft_unverified"));
        object.remove("verification");
        object
            .get_mut("generator")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "schema_version".into(),
                serde_json::json!(DRAFT_SCHEMA_VERSION),
            );
        let fingerprint = draft_fingerprint_from_json(&value.to_string()).unwrap();
        value.as_object_mut().unwrap().insert(
            "generation_fingerprint".into(),
            serde_json::json!(fingerprint),
        );
        value.to_string()
    }

    #[test]
    fn duplicate_keys_are_rejected_at_every_json_depth() {
        for source in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":{"b":1,"b":2}}"#,
            r#"{"a":[{"b":1,"b":2}]}"#,
            r#"{"a":1} trailing"#,
            r#"{"a":"unterminated}"#,
        ] {
            assert!(reject_duplicate_object_keys(source).is_err(), "{source}");
        }
        assert!(reject_duplicate_object_keys(r#"{"a":{"b":[true,false,null]}}"#).is_ok());
    }

    #[test]
    fn prose_and_relations_have_no_lossy_recovery_path() {
        assert!(valid_definition("入出力を扱うための仕組み。"));
        for value in [
            "English only.",
            "短い。",
            "末尾に句点がない説明",
            "説明。\n次の行。",
        ] {
            assert!(!valid_definition(value), "{value}");
        }
        let good = Relations {
            aliases: vec!["別名".into()],
            related: vec!["関連語".into()],
            similar: vec!["類似語".into()],
            antonyms: vec!["反対語".into()],
        };
        assert!(validate_relations("用語", &good).is_ok());
        let duplicate = Relations {
            aliases: vec!["重複".into()],
            related: vec!["重複".into()],
            similar: Vec::new(),
            antonyms: Vec::new(),
        };
        assert!(validate_relations("用語", &duplicate).is_err());
        let self_reference = Relations {
            aliases: vec!["用語".into()],
            related: Vec::new(),
            similar: Vec::new(),
            antonyms: Vec::new(),
        };
        assert!(validate_relations("用語", &self_reference).is_err());
    }

    #[test]
    fn report_counts_only_validated_unique_terms() {
        let report = ImportReport {
            input_records: 50_000,
            accepted_records: 3,
            validated_unique_terms: 3,
            emitted_details: 5,
            ..ImportReport::default()
        };
        let json = report_json(report);
        assert!(json.contains("\"validated_unique_terms\": 3"));
        assert!(!json.contains("generated_count"));
    }

    #[test]
    fn accepted_record_is_pinned_and_curated_detail_wins() {
        let target = target();
        let entries = entries();
        let imported = import_release_jsonl(
            "records.jsonl",
            &record(&target),
            std::slice::from_ref(&target),
            &entries,
            &[],
        )
        .expect("valid independently verified record");
        assert_eq!(imported.report.validated_unique_terms, 1);
        assert_eq!(imported.details.len(), 1);
        let existing = SourceDetail {
            reading: "ようご".into(),
            surface: "用語".into(),
            left_id: 1,
            right_id: 1,
            description: "trusted source detail".into(),
            relations: Vec::new(),
        };
        let suppressed = import_release_jsonl(
            "records.jsonl",
            &record(&target),
            &[target],
            &entries,
            &[existing],
        )
        .expect("trusted detail suppresses rather than replaces");
        assert!(suppressed.details.is_empty());
        assert_eq!(suppressed.report.suppressed_by_existing_pair, 1);
        assert_eq!(suppressed.report.exact_identity_fill_candidates, 1);
    }

    #[test]
    fn normalized_pair_coverage_blocks_other_exact_identity_from_new_term_lane() {
        let target = target();
        let entries = entries();
        let existing = SourceDetail {
            // Decomposed `ご` proves the pair check is NFC-normalized, while
            // IDs deliberately differ from the target's exact dictionary edge.
            reading: "ようこ\u{3099}".into(),
            surface: "用語".into(),
            left_id: 999,
            right_id: 999,
            description: "trusted pair owner".into(),
            relations: Vec::new(),
        };
        let imported = import_release_jsonl(
            "records.jsonl",
            &record(&target),
            std::slice::from_ref(&target),
            &entries,
            std::slice::from_ref(&existing),
        )
        .expect("trusted pair suppresses the new-term lane");
        assert_eq!(imported.report.accepted_records, 1);
        assert_eq!(imported.report.validated_unique_terms, 0);
        assert_eq!(imported.report.suppressed_by_existing_pair, 1);
        assert_eq!(imported.report.exact_identity_fill_candidates, 1);
        assert!(imported.details.is_empty());

        let draft_line = draft_record(&target);
        let batch = parse_drafts_jsonl(
            "draft.jsonl",
            &draft_line,
            std::slice::from_ref(&target),
            &entries,
        )
        .unwrap();
        let mut review = IndependentReview {
            target_hash: target.target_hash.clone(),
            draft_generation_fingerprint: draft_fingerprint_from_json(&draft_line).unwrap(),
            review_fingerprint: String::new(),
            decision: ReviewDecision::Approved {
                model: "reviewer-b".into(),
                prompt_version: "review-1".into(),
                schema_version: SCHEMA_VERSION.into(),
            },
        };
        review.review_fingerprint = canonical_review_fingerprint(&review);
        let promoted = promote_drafts(
            &batch.drafts,
            &[review],
            std::slice::from_ref(&target),
            &entries,
            std::slice::from_ref(&existing),
        )
        .expect("pair collision holds promotion");
        assert!(promoted.release_jsonl.is_empty());
        assert_eq!(promoted.report.approved, 0);
        assert_eq!(promoted.report.held, 1);
        assert_eq!(promoted.report.suppressed_by_existing_pair, 1);
        assert_eq!(promoted.report.exact_identity_fill_candidates, 1);
        assert_eq!(promoted.review_outcomes[0].status, ReviewStatus::Held);
    }

    #[test]
    fn existing_pair_suppresses_release_pinned_to_an_older_dictionary_identity() {
        let target = target();
        let changed_entries = parse_entries(
            "changed.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
             ようご\t用語\t1\t1\t101\t-\t\t\n",
        )
        .unwrap();
        let existing = SourceDetail {
            reading: "ようご".into(),
            surface: "用語".into(),
            left_id: 1,
            right_id: 1,
            description: "current curated detail".into(),
            relations: Vec::new(),
        };

        let suppressed = import_release_jsonl(
            "records.jsonl",
            &record(&target),
            std::slice::from_ref(&target),
            &changed_entries,
            std::slice::from_ref(&existing),
        )
        .expect("the trusted current pair owns the term before old identity matching");
        assert!(suppressed.details.is_empty());
        assert_eq!(suppressed.report.accepted_records, 1);
        assert_eq!(suppressed.report.validated_unique_terms, 0);
        assert_eq!(suppressed.report.suppressed_by_existing_pair, 1);
        assert_eq!(suppressed.report.exact_identity_fill_candidates, 1);
        assert_eq!(suppressed.report.rejections.dictionary_mismatch, 0);

        let error = import_release_jsonl(
            "records.jsonl",
            &record(&target),
            &[target],
            &changed_entries,
            &[],
        )
        .expect_err("an unowned stale target must still fail closed");
        assert_eq!(error.report.rejections.dictionary_mismatch, 1);
    }

    #[test]
    fn stale_hashes_duplicate_keys_and_unverified_records_fail_closed() {
        let target = target();
        let entries = entries();
        let original = record(&target);
        for bad in [
            original.replacen(&target.input_hash, &"c".repeat(64), 1),
            original.replacen("\"agrees\":true", "\"agrees\":false", 1),
            original.replacen("\"confidence\":0.99", "\"confidence\":0.1", 1),
            format!(
                "{{\"schema_version\":\"{SCHEMA_VERSION}\",{}}}",
                &original[1..]
            ),
        ] {
            assert!(
                import_release_jsonl(
                    "records.jsonl",
                    &bad,
                    std::slice::from_ref(&target),
                    &entries,
                    &[]
                )
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn canonical_fingerprints_reject_case_and_semantic_mutations() {
        let target = target();
        let entries = entries();
        let draft = draft_record(&target);
        let original: serde_json::Value = serde_json::from_str(&draft).unwrap();
        let fingerprint = original["generation_fingerprint"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(is_sha256(&fingerprint));
        assert!(!is_sha256(&fingerprint.to_uppercase()));
        for (name, replacement) in [
            ("surface", serde_json::json!("別の語")),
            ("reading", serde_json::json!("べつのご")),
            ("definition", serde_json::json!("別の意味を持つ語句。")),
            ("confidence", serde_json::json!(0.98)),
            ("review_state", serde_json::json!("held")),
            ("target_hash", serde_json::json!("0".repeat(64))),
            ("input_hash", serde_json::json!("1".repeat(64))),
        ] {
            let mut changed = original.clone();
            changed
                .as_object_mut()
                .unwrap()
                .insert(name.into(), replacement);
            let changed_line = changed.to_string();
            assert_ne!(
                draft_fingerprint_from_json(&changed_line).unwrap(),
                fingerprint,
                "{name}"
            );
            assert!(
                parse_drafts_jsonl(
                    "draft",
                    &changed_line,
                    std::slice::from_ref(&target),
                    &entries
                )
                .is_err(),
                "{name}"
            );
        }
        let mut review = IndependentReview {
            target_hash: target.target_hash.clone(),
            draft_generation_fingerprint: fingerprint,
            review_fingerprint: String::new(),
            decision: ReviewDecision::Held {
                reason: "要確認".into(),
            },
        };
        review.review_fingerprint = canonical_review_fingerprint(&review);
        let mut tampered_review = review.clone();
        tampered_review.decision = ReviewDecision::Held {
            reason: "別の理由".into(),
        };
        let batch =
            parse_drafts_jsonl("draft", &draft, std::slice::from_ref(&target), &entries).unwrap();
        assert!(promote_drafts(
            &batch.drafts,
            &[tampered_review],
            std::slice::from_ref(&target),
            &entries,
            &[]
        )
        .is_err());
    }

    #[test]
    fn directly_constructed_stale_target_is_rejected_at_public_boundaries() {
        let mut stale = target();
        stale.input_hash = "0".repeat(64);
        let entries = entries();
        assert!(parse_drafts_jsonl(
            "draft",
            &draft_record(&stale),
            std::slice::from_ref(&stale),
            &entries
        )
        .is_err());
        assert!(import_release_jsonl(
            "release",
            &record(&stale),
            std::slice::from_ref(&stale),
            &entries,
            &[]
        )
        .is_err());
    }

    #[test]
    fn committed_000004_release_is_bound_and_imports_six_new_unique_terms() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target_directory = root.join("data/llm-detail-targets/000004");
        let release_directory = root.join("data/llm-details/releases/000004");
        let targets = crate::llm_detail_targets::load_committed_targets(&target_directory).unwrap();
        let release =
            load_committed_release_jsonl(&release_directory, &target_directory, &targets).unwrap();
        let source = std::fs::read_to_string(root.join("data/it-terms.tsv")).unwrap();
        let entries = crate::parse_entries("data/it-terms.tsv", &source).unwrap();
        let imported = import_release_jsonl("000004", &release, &targets, &entries, &[]).unwrap();
        assert_eq!(imported.report.validated_unique_terms, 6);
        assert_eq!(imported.report.emitted_details, 6);
        assert_eq!(imported.details.len(), 6);
    }

    #[test]
    fn draft_is_not_a_detail_and_only_approved_second_pass_promotes_it() {
        let target = target();
        let entries = entries();
        let draft_line = draft_record(&target);
        assert!(import_release_jsonl(
            "draft.jsonl",
            &draft_line,
            std::slice::from_ref(&target),
            &entries,
            &[]
        )
        .is_err());
        let batch = parse_drafts_jsonl(
            "draft.jsonl",
            &draft_line,
            std::slice::from_ref(&target),
            &entries,
        )
        .expect("safe draft");
        assert_eq!(batch.report.validated_unique_terms, 1);
        let mut held_review = IndependentReview {
            target_hash: target.target_hash.clone(),
            draft_generation_fingerprint: draft_fingerprint_from_json(&draft_line).unwrap(),
            review_fingerprint: String::new(),
            decision: ReviewDecision::Held {
                reason: "用法を確認待ち".into(),
            },
        };
        held_review.review_fingerprint = canonical_review_fingerprint(&held_review);
        let held = promote_drafts(
            &batch.drafts,
            &[held_review],
            std::slice::from_ref(&target),
            &entries,
            &[],
        )
        .expect("held review is recorded but never released");
        assert!(held.release_jsonl.is_empty());
        assert_eq!(held.report.held, 1);
        assert_eq!(held.review_outcomes[0].status, ReviewStatus::Held);
        assert_eq!(
            held.review_outcomes[0].reason.as_deref(),
            Some("用法を確認待ち")
        );
        assert_eq!(promotion_report_json(&held), promotion_report_json(&held));
        assert!(promotion_report_json(&held).contains("draft_generation_fingerprint"));
        let mut approved_review = IndependentReview {
            target_hash: target.target_hash.clone(),
            draft_generation_fingerprint: draft_fingerprint_from_json(&draft_line).unwrap(),
            review_fingerprint: String::new(),
            decision: ReviewDecision::Approved {
                model: "reviewer-b".into(),
                prompt_version: "review-1".into(),
                schema_version: SCHEMA_VERSION.into(),
            },
        };
        approved_review.review_fingerprint = canonical_review_fingerprint(&approved_review);
        let promoted = promote_drafts(
            &batch.drafts,
            &[approved_review],
            std::slice::from_ref(&target),
            &entries,
            &[],
        )
        .expect("independent approval promotes");
        assert_eq!(promoted.report.approved, 1);
        import_release_jsonl(
            "release.jsonl",
            &promoted.release_jsonl,
            std::slice::from_ref(&target),
            &entries,
            &[],
        )
        .expect("approved release record is importable");
    }
}
