//! Independent, deterministic whole-reading candidate capture for Issue #93.
//!
//! The tool deliberately lives outside every shipping crate. Copy this nested
//! workspace unchanged into each release worktree so that it links to that
//! worktree's own `sakura-core` implementation.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use sakura_core::conversion::MAX_CONVERSION_CANDIDATES;
use sakura_core::{
    ConversionCandidate, ConversionDiagnostics, ConversionMethod, ConversionOptions,
    ConversionSearchTerminal, Converter, Dictionary,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const SNAPSHOT_LANE: &str = "engine_candidate_snapshot_v1";
const EXPECTED_CORPUS_ID: &str = "issue93-ranking-comparison-v1";
const EXPECTED_STAGE: &str = "phase0";
const EXPECTED_CANDIDATE_LIMIT: usize = 18;
const MAX_CASE_ID_BYTES: usize = 128;
const MAX_FIXTURE_CASES: usize = 4_096;
const MAX_GIT_SHA_BYTES: usize = 40;
const MAX_VARIANT_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliOptions {
    dictionary: PathBuf,
    fixture: PathBuf,
    output: Option<PathBuf>,
    git_sha: String,
    variant: String,
    source_diff_sha256: String,
    it_bias: ItBias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItBias {
    On,
    Off,
}

impl ItBias {
    const fn name(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }

    const fn per_mille(self) -> u16 {
        match self {
            Self::On => 100,
            Self::Off => 0,
        }
    }

    const fn max_boost(self) -> i32 {
        match self {
            Self::On => 800,
            Self::Off => 0,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Fixture {
    schema_version: u32,
    corpus_id: String,
    stage: String,
    candidate_limit: usize,
    options: FixtureOptions,
    cases: Vec<FixtureCase>,
}

#[derive(Debug, Deserialize)]
struct FixtureOptions {
    profile: String,
    candidate_limit: usize,
    recall_k: usize,
    learning: String,
    user_dictionary: String,
    reranker: String,
    input_repair: String,
    context: String,
    locale: String,
}

#[derive(Debug, Deserialize)]
struct FixtureCase {
    case_id: String,
    reading: String,
}

#[derive(Debug, Serialize)]
struct SnapshotDocument {
    schema_version: u32,
    lane: &'static str,
    corpus_id: String,
    stage: String,
    artifact: ArtifactIdentity,
    evaluator: EvaluatorIdentity,
    engine: EngineIdentity,
    options: OptionIdentity,
    cases: Vec<SnapshotRecord>,
}

#[derive(Debug, Serialize)]
struct ArtifactIdentity {
    git_sha: String,
    evaluator_sha256: String,
    dictionary_sha256: String,
    fixture_sha256: String,
    source_diff_sha256: String,
    variant: String,
}

#[derive(Debug, Serialize)]
struct EvaluatorIdentity {
    name: &'static str,
    version: &'static str,
    executable_sha256: String,
    build_feature: &'static str,
}

#[derive(Debug, Serialize)]
struct EngineIdentity {
    package: &'static str,
    api: &'static str,
    origin_metadata: &'static str,
    path_evidence_metadata: &'static str,
    input_support_metadata: &'static str,
}

#[derive(Debug, Serialize)]
struct OptionIdentity {
    profile: &'static str,
    candidate_limit: usize,
    method: &'static str,
    it_bias: &'static str,
    it_bias_per_mille: u16,
    max_it_boost: i32,
    initial_right_id: u16,
    input_repair: &'static str,
    learning: &'static str,
    user_dictionary: &'static str,
    reranker: &'static str,
    material: String,
    options_sha256: String,
}

#[derive(Debug, Serialize)]
struct SnapshotRecord {
    case_id: String,
    reading: String,
    candidate_limit: usize,
    candidate_surfaces: Vec<String>,
    candidates: Vec<SnapshotCandidate>,
    terminal: &'static str,
    truncated: bool,
    diagnostics: SnapshotDiagnostics,
}

#[derive(Debug, Serialize)]
struct SnapshotDiagnostics {
    terminal: &'static str,
    lattice_nodes: Option<usize>,
    states_pushed: usize,
    incoherent_prefixes_pruned: usize,
    lossless_fallback_inserted: bool,
}

#[derive(Debug, Serialize)]
struct SnapshotCandidate {
    rank: usize,
    surface: String,
    annotation: String,
    /// Final path cost exposed by `sakura-core`; it is not a base/local cost.
    cost: i64,
    cost_kind: &'static str,
    segments: Vec<SnapshotSegment>,
    /// Exact common evidence. Composite/generated candidates deliberately do
    /// not claim a system-entry ordinal.
    origin: &'static str,
    system_entry_index: Option<u32>,
    /// Metadata introduced after v1.0.5 stays explicitly nullable for legacy
    /// captures rather than being guessed from surface text or cost.
    origin_detail: Option<String>,
    authority: Option<&'static str>,
    synthetic_exact: Option<bool>,
    cross_commit_rescored: Option<bool>,
    path_evidence: Option<SnapshotPathEvidence>,
    base_cost: Option<i64>,
    ranking_pass: Option<String>,
    unsupported_metadata: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct SnapshotSegment {
    text: String,
    reading_start: u16,
    reading_end: u16,
    text_start: u16,
    text_end: u16,
    left_id: u16,
    right_id: u16,
    flags: u16,
    word_count: u8,
    it_word_count: u8,
}

#[derive(Debug, Serialize)]
struct SnapshotPathEvidence {
    system_edges: u8,
    user_edges: u8,
    fallback_edges: u8,
    generated_edges: u8,
    spelling_edges: u8,
    system_only: bool,
}

fn main() {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return;
    }
    if let Err(error) = run(args) {
        eprintln!("candidate-snapshot: {error}");
        std::process::exit(2);
    }
}

fn run(args: Vec<OsString>) -> Result<(), String> {
    let cli = parse_options(args.into_iter())?;
    let fixture_bytes = fs::read(&cli.fixture)
        .map_err(|error| format!("read fixture {}: {error}", cli.fixture.display()))?;
    let fixture: Fixture = serde_json::from_slice(&fixture_bytes)
        .map_err(|error| format!("parse fixture {}: {error}", cli.fixture.display()))?;
    validate_fixture(&fixture)?;

    let dictionary_bytes = fs::read(&cli.dictionary)
        .map_err(|error| format!("read dictionary {}: {error}", cli.dictionary.display()))?;
    let dictionary = Dictionary::parse(&dictionary_bytes)
        .map_err(|error| format!("parse dictionary {}: {error}", cli.dictionary.display()))?;
    let records = capture_cases(&dictionary, &fixture.cases, cli.it_bias)?;

    let executable = env::current_exe().map_err(|error| format!("resolve evaluator: {error}"))?;
    let evaluator_sha256 = sha256_file(&executable)?;
    let document = SnapshotDocument {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        lane: SNAPSHOT_LANE,
        corpus_id: fixture.corpus_id,
        stage: fixture.stage,
        artifact: ArtifactIdentity {
            git_sha: cli.git_sha,
            evaluator_sha256: evaluator_sha256.clone(),
            dictionary_sha256: sha256_bytes(&dictionary_bytes),
            fixture_sha256: sha256_bytes(&fixture_bytes),
            source_diff_sha256: cli.source_diff_sha256,
            variant: cli.variant,
        },
        evaluator: EvaluatorIdentity {
            name: "sakura-candidate-snapshot",
            version: env!("CARGO_PKG_VERSION"),
            executable_sha256: evaluator_sha256,
            build_feature: build_feature(),
        },
        engine: engine_identity(),
        options: option_identity(cli.it_bias),
        cases: records,
    };
    let mut json = serde_json::to_string_pretty(&document)
        .map_err(|error| format!("encode snapshot: {error}"))?;
    json.push('\n');
    if let Some(output) = cli.output {
        if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        fs::write(&output, json).map_err(|error| format!("write {}: {error}", output.display()))?;
    } else {
        print!("{json}");
    }
    Ok(())
}

fn print_usage() {
    println!(
        "Usage: sakura-candidate-snapshot \\\n+  --dictionary <system.dic> --fixture <fixture.json> \\\n+  --git <40-hex> --variant <label> \\\n+  --source-diff-sha256 <clean|64-hex> --it-bias <on|off> \\\n+  [--output <snapshot.json>]"
    );
}

fn parse_options(args: impl Iterator<Item = OsString>) -> Result<CliOptions, String> {
    let mut dictionary = None;
    let mut fixture = None;
    let mut output = None;
    let mut git_sha = None;
    let mut variant = None;
    let mut source_diff_sha256 = None;
    let mut it_bias = None;
    let mut args = args;
    while let Some(flag) = args.next() {
        let flag = flag
            .into_string()
            .map_err(|_| "argument names must be valid Unicode".to_owned())?;
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--dictionary" => set_once(&mut dictionary, PathBuf::from(value), &flag)?,
            "--fixture" => set_once(&mut fixture, PathBuf::from(value), &flag)?,
            "--output" => set_once(&mut output, PathBuf::from(value), &flag)?,
            "--git" => set_once(&mut git_sha, unicode_value(value, &flag)?, &flag)?,
            "--variant" => set_once(&mut variant, unicode_value(value, &flag)?, &flag)?,
            "--source-diff-sha256" => {
                set_once(&mut source_diff_sha256, unicode_value(value, &flag)?, &flag)?
            }
            "--it-bias" => {
                let value = unicode_value(value, &flag)?;
                let parsed = match value.as_str() {
                    "on" => ItBias::On,
                    "off" => ItBias::Off,
                    _ => return Err("--it-bias must be on or off".to_owned()),
                };
                set_once(&mut it_bias, parsed, &flag)?;
            }
            _ => return Err(format!("unknown argument {flag}")),
        }
    }

    let git_sha = git_sha.ok_or("missing --git")?.to_ascii_lowercase();
    if git_sha.len() != MAX_GIT_SHA_BYTES || !is_hex(&git_sha) {
        return Err("--git must be a 40-character hexadecimal SHA".to_owned());
    }
    let variant = variant.ok_or("missing --variant")?;
    if variant.is_empty() || variant.len() > MAX_VARIANT_BYTES {
        return Err(format!(
            "--variant must contain 1..={MAX_VARIANT_BYTES} bytes"
        ));
    }
    let source_diff_sha256 = source_diff_sha256
        .ok_or("missing --source-diff-sha256")?
        .to_ascii_lowercase();
    if source_diff_sha256 != "clean"
        && (source_diff_sha256.len() != 64 || !is_hex(&source_diff_sha256))
    {
        return Err("--source-diff-sha256 must be clean or a 64-hex SHA-256".to_owned());
    }
    Ok(CliOptions {
        dictionary: dictionary.ok_or("missing --dictionary")?,
        fixture: fixture.ok_or("missing --fixture")?,
        output,
        git_sha,
        variant,
        source_diff_sha256,
        it_bias: it_bias.ok_or("missing --it-bias")?,
    })
}

fn unicode_value(value: OsString, flag: &str) -> Result<String, String> {
    value
        .into_string()
        .map_err(|_| format!("{flag} must be valid Unicode"))
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("duplicate argument {flag}"));
    }
    Ok(())
}

fn is_hex(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_fixture(fixture: &Fixture) -> Result<(), String> {
    if fixture.schema_version != 1
        || fixture.corpus_id != EXPECTED_CORPUS_ID
        || fixture.stage != EXPECTED_STAGE
    {
        return Err("unsupported Issue #93 fixture schema, corpus, or stage".to_owned());
    }
    if MAX_CONVERSION_CANDIDATES != EXPECTED_CANDIDATE_LIMIT
        || fixture.candidate_limit != EXPECTED_CANDIDATE_LIMIT
        || fixture.options.candidate_limit != EXPECTED_CANDIDATE_LIMIT
    {
        return Err(format!(
            "fixture and core candidate limits must all be {EXPECTED_CANDIDATE_LIMIT}"
        ));
    }
    let options = &fixture.options;
    if options.profile != EXPECTED_CORPUS_ID
        || options.recall_k != 5
        || options.learning != "disabled"
        || options.user_dictionary != "disabled"
        || options.reranker != "off"
        || options.input_repair != "disabled"
        || options.context != "empty"
        || options.locale != "ja-JP"
    {
        return Err("fixture options differ from the fixed Issue #93 profile".to_owned());
    }
    if fixture.cases.is_empty() || fixture.cases.len() > MAX_FIXTURE_CASES {
        return Err(format!(
            "fixture must contain 1..={MAX_FIXTURE_CASES} cases"
        ));
    }
    let mut ids = BTreeSet::new();
    for case_ in &fixture.cases {
        if case_.case_id.is_empty()
            || case_.case_id.len() > MAX_CASE_ID_BYTES
            || !ids.insert(case_.case_id.as_str())
        {
            return Err("fixture contains an empty, long, or duplicate case_id".to_owned());
        }
        if case_.reading.is_empty() {
            return Err(format!(
                "fixture case {} has an empty reading",
                case_.case_id
            ));
        }
    }
    Ok(())
}

fn capture_cases(
    dictionary: &Dictionary<'_>,
    cases: &[FixtureCase],
    it_bias: ItBias,
) -> Result<Vec<SnapshotRecord>, String> {
    let mut converter = Converter::new();
    let options = conversion_options(it_bias);
    let mut records = Vec::with_capacity(cases.len());
    for case_ in cases {
        let result = converter
            .convert_detailed(dictionary, &case_.reading, options)
            .map_err(|error| format!("convert {} ({}): {error}", case_.case_id, case_.reading))?;
        let diagnostics = result.diagnostics();
        let terminal = terminal_name(diagnostics.terminal);
        let mut candidates = Vec::with_capacity(result.candidates().len());
        for (index, candidate) in result.candidates().iter().enumerate() {
            candidates.push(snapshot_candidate(index + 1, candidate)?);
        }
        if candidates.is_empty() || candidates.len() > EXPECTED_CANDIDATE_LIMIT {
            return Err(format!(
                "case {} returned an invalid candidate count {}",
                case_.case_id,
                candidates.len()
            ));
        }
        let candidate_surfaces = candidates
            .iter()
            .map(|candidate| candidate.surface.clone())
            .collect();
        records.push(SnapshotRecord {
            case_id: case_.case_id.clone(),
            reading: case_.reading.clone(),
            candidate_limit: EXPECTED_CANDIDATE_LIMIT,
            candidate_surfaces,
            candidates,
            terminal,
            truncated: diagnostics.terminal != ConversionSearchTerminal::SearchExhausted,
            diagnostics: snapshot_diagnostics(diagnostics),
        });
    }
    Ok(records)
}

fn conversion_options(it_bias: ItBias) -> ConversionOptions {
    let mut options = ConversionOptions::default();
    options.max_candidates = EXPECTED_CANDIDATE_LIMIT;
    options.method = ConversionMethod::MultiSegment;
    options.it_bias_per_mille = it_bias.per_mille();
    options.max_it_boost = it_bias.max_boost();
    options.initial_right_id = 0;
    #[cfg(feature = "modern")]
    {
        options.input_support = disabled_input_support();
        options.skip_input_repair = true;
    }
    options
}

#[cfg(feature = "modern")]
fn disabled_input_support() -> sakura_core::InputSupport {
    sakura_core::InputSupport {
        enabled: false,
        commit_based: false,
        advanced: false,
        vowel_count: false,
        consonant_extra: false,
        n_count: false,
        dakuten_swap: false,
        tsu_sokuon: false,
        wa_wo: false,
        small_u: false,
        fuzzy_proper_nouns: false,
        english_to_katakana: false,
        period_after_digit: false,
        comma_after_digit: false,
        middle_dot_after_digit: false,
        long_vowel_after_alnum: false,
    }
}

fn snapshot_candidate(
    rank: usize,
    candidate: &ConversionCandidate,
) -> Result<SnapshotCandidate, String> {
    let surface = candidate.text().to_owned();
    let mut segments = Vec::with_capacity(candidate.segments().len());
    for segment in candidate.segments() {
        let start = usize::from(segment.text_start);
        let end = usize::from(segment.text_end);
        let text = surface.get(start..end).ok_or_else(|| {
            format!("candidate rank {rank} exposes a non-UTF-8 segment range {start}..{end}")
        })?;
        segments.push(SnapshotSegment {
            text: text.to_owned(),
            reading_start: segment.reading_start,
            reading_end: segment.reading_end,
            text_start: segment.text_start,
            text_end: segment.text_end,
            left_id: segment.left_id,
            right_id: segment.right_id,
            flags: segment.flags.bits(),
            word_count: segment.word_count,
            it_word_count: segment.it_word_count,
        });
    }
    let system_entry_index = candidate.system_entry_index();
    let common_origin = if system_entry_index.is_some() {
        "exact_system_entry"
    } else {
        "not_exactly_attributable"
    };
    let metadata = modern_candidate_metadata(candidate);
    Ok(SnapshotCandidate {
        rank,
        surface,
        annotation: candidate.annotation().to_owned(),
        cost: candidate.cost,
        cost_kind: "final_path_cost",
        segments,
        origin: common_origin,
        system_entry_index,
        origin_detail: metadata.origin_detail,
        authority: metadata.authority,
        synthetic_exact: metadata.synthetic_exact,
        cross_commit_rescored: metadata.cross_commit_rescored,
        path_evidence: metadata.path_evidence,
        base_cost: None,
        ranking_pass: None,
        unsupported_metadata: metadata.unsupported_metadata,
    })
}

struct ModernCandidateMetadata {
    origin_detail: Option<String>,
    authority: Option<&'static str>,
    synthetic_exact: Option<bool>,
    cross_commit_rescored: Option<bool>,
    path_evidence: Option<SnapshotPathEvidence>,
    unsupported_metadata: Vec<&'static str>,
}

#[cfg(not(feature = "modern"))]
fn modern_candidate_metadata(_candidate: &ConversionCandidate) -> ModernCandidateMetadata {
    ModernCandidateMetadata {
        origin_detail: None,
        authority: None,
        synthetic_exact: None,
        cross_commit_rescored: None,
        path_evidence: None,
        unsupported_metadata: vec![
            "candidate_origin",
            "candidate_authority",
            "synthetic_exact",
            "cross_commit_rescored",
            "path_evidence",
            "base_cost",
            "ranking_pass",
        ],
    }
}

#[cfg(feature = "modern")]
fn modern_candidate_metadata(candidate: &ConversionCandidate) -> ModernCandidateMetadata {
    use sakura_core::conversion::{CandidateAuthority, CandidateOrigin, RepairTier};

    let origin_detail = match candidate.origin() {
        CandidateOrigin::Direct => "direct".to_owned(),
        CandidateOrigin::RawRepair { plan_id, tier } => {
            let tier = match tier {
                RepairTier::LocalCompletion => "local_completion",
                RepairTier::GeneralSingleInsertion => "general_single_insertion",
            };
            format!("raw_repair:{plan_id}:{tier}")
        }
    };
    let authority = match candidate.authority() {
        CandidateAuthority::Direct => "direct",
        CandidateAuthority::LocalRawCompletion => "local_raw_completion",
        CandidateAuthority::GeneralSingleInsertion => "general_single_insertion",
    };
    let evidence = candidate.path_evidence();
    ModernCandidateMetadata {
        origin_detail: Some(origin_detail),
        authority: Some(authority),
        synthetic_exact: Some(candidate.is_synthetic_exact()),
        cross_commit_rescored: Some(candidate.was_cross_commit_rescored()),
        path_evidence: Some(SnapshotPathEvidence {
            system_edges: evidence.system_edges,
            user_edges: evidence.user_edges,
            fallback_edges: evidence.fallback_edges,
            generated_edges: evidence.generated_edges,
            spelling_edges: evidence.spelling_edges,
            system_only: evidence.is_system_only(),
        }),
        unsupported_metadata: vec!["base_cost", "ranking_pass"],
    }
}

fn snapshot_diagnostics(diagnostics: ConversionDiagnostics) -> SnapshotDiagnostics {
    SnapshotDiagnostics {
        terminal: terminal_name(diagnostics.terminal),
        lattice_nodes: diagnostics_lattice_nodes(diagnostics),
        states_pushed: diagnostics.states_pushed,
        incoherent_prefixes_pruned: diagnostics.incoherent_prefixes_pruned,
        lossless_fallback_inserted: diagnostics.lossless_fallback_inserted,
    }
}

#[cfg(feature = "modern")]
fn diagnostics_lattice_nodes(diagnostics: ConversionDiagnostics) -> Option<usize> {
    Some(diagnostics.lattice_nodes)
}

#[cfg(not(feature = "modern"))]
fn diagnostics_lattice_nodes(_diagnostics: ConversionDiagnostics) -> Option<usize> {
    None
}

fn terminal_name(terminal: ConversionSearchTerminal) -> &'static str {
    match terminal {
        ConversionSearchTerminal::CandidateLimitReached => "candidate_limit_reached",
        ConversionSearchTerminal::SearchExhausted => "search_exhausted",
        ConversionSearchTerminal::StateBudgetReached => "state_budget_reached",
        ConversionSearchTerminal::LatticeBudgetReached => "lattice_budget_reached",
    }
}

fn option_identity(it_bias: ItBias) -> OptionIdentity {
    let material = format!(
        "profile={EXPECTED_CORPUS_ID};candidate_limit={EXPECTED_CANDIDATE_LIMIT};method=multi-segment;it_bias={};it_bias_per_mille={};max_it_boost={};initial_right_id=0;input_repair=disabled;learning=disabled;user_dictionary=disabled;reranker=off",
        it_bias.name(),
        it_bias.per_mille(),
        it_bias.max_boost()
    );
    let options_sha256 = sha256_bytes(material.as_bytes());
    OptionIdentity {
        profile: EXPECTED_CORPUS_ID,
        candidate_limit: EXPECTED_CANDIDATE_LIMIT,
        method: "multi-segment",
        it_bias: it_bias.name(),
        it_bias_per_mille: it_bias.per_mille(),
        max_it_boost: it_bias.max_boost(),
        initial_right_id: 0,
        input_repair: "disabled",
        learning: "disabled",
        user_dictionary: "disabled",
        reranker: "off",
        material,
        options_sha256,
    }
}

fn engine_identity() -> EngineIdentity {
    if cfg!(feature = "modern") {
        EngineIdentity {
            package: "sakura-core",
            api: "convert_detailed",
            origin_metadata: "observed",
            path_evidence_metadata: "observed",
            input_support_metadata: "explicitly_disabled",
        }
    } else {
        EngineIdentity {
            package: "sakura-core",
            api: "convert_detailed",
            origin_metadata: "unsupported",
            path_evidence_metadata: "unsupported",
            input_support_metadata: "not_present_in_core",
        }
    }
}

fn build_feature() -> &'static str {
    if cfg!(feature = "modern") {
        "modern"
    } else {
        "legacy"
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("read {} for SHA-256: {error}", path.display()))?;
    Ok(sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_args() -> Vec<OsString> {
        [
            "--dictionary",
            "system.dic",
            "--fixture",
            "fixture.json",
            "--git",
            "0123456789abcdef0123456789abcdef01234567",
            "--variant",
            "v1.0.23",
            "--source-diff-sha256",
            "clean",
            "--it-bias",
            "on",
            "--output",
            "snapshot.json",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    fn sha256_known_vector_is_stable() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn cli_requires_all_identity_fields() {
        let parsed = parse_options(valid_args().into_iter()).expect("valid CLI");
        assert_eq!(parsed.it_bias, ItBias::On);
        assert_eq!(parsed.source_diff_sha256, "clean");
        assert_eq!(parsed.variant, "v1.0.23");

        let mut missing_git = valid_args();
        missing_git.drain(4..6);
        assert_eq!(
            parse_options(missing_git.into_iter()).unwrap_err(),
            "missing --git"
        );
    }

    #[test]
    fn cli_rejects_ambiguous_ablation_identity() {
        let mut args = valid_args();
        let index = args
            .iter()
            .position(|value| value == "clean")
            .expect("clean source identity");
        args[index] = OsString::from("dirty");
        assert!(parse_options(args.into_iter())
            .unwrap_err()
            .contains("clean or a 64-hex"));
    }

    #[test]
    fn option_material_is_deterministic_and_distinguishes_bias() {
        let on = option_identity(ItBias::On);
        let same = option_identity(ItBias::On);
        let off = option_identity(ItBias::Off);
        assert_eq!(on.options_sha256, same.options_sha256);
        assert_eq!(on.material, same.material);
        assert_ne!(on.options_sha256, off.options_sha256);
        assert_eq!(on.options_sha256.len(), 64);
    }

    #[test]
    fn production_limit_matches_the_fixture_contract() {
        assert_eq!(MAX_CONVERSION_CANDIDATES, EXPECTED_CANDIDATE_LIMIT);
    }
}
