//! Deterministic, bounded-memory trim of pinned Mozc dictionary shards.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use dictc::{
    category::{
        mark_non_initial_allomorphs_with_boundary_policy, parse_mozc_pos_catalog,
        parse_non_initial_boundary_policy, NonInitialBoundaryReport,
    },
    entries_to_tsv, parse_mozc_entries, MozcTrimmer, TrimPolicy, TrimReport, MOZC_UPSTREAM_COMMIT,
};
use sha2::{Digest, Sha256};

const MOZC_REPOSITORY: &str = "https://github.com/google/mozc";
const OUTPUT_LICENSE: &str = "LicenseRef-Mozc-Dictionary";
const DEFAULT_MAX_WORD_COST: i32 = 6_900;
/// Frozen provenance boundary for allomorph classification, not an admission
/// rule. It is deliberately absent from the command line: moving it would
/// retroactively reclassify rows that already shipped under the former
/// row-based cap.
const LEGACY_ROW_EVIDENCE_CAP: usize = 12;
/// No per-reading surface cap by default, so `--max-word-cost` stays the single
/// admission rule and a reading keeps every affordable homophone.
const DEFAULT_MAX_SURFACES_PER_READING: Option<usize> = None;

struct Config {
    shards: Vec<PathBuf>,
    pos_catalog: PathBuf,
    non_initial_policy: PathBuf,
    output: PathBuf,
    report: PathBuf,
    policy: TrimPolicy,
}

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("mozc-trim: {error}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let started = Instant::now();
    let mut config = parse_args(args)?;
    config.shards.sort();
    config.shards.dedup();
    if config.shards.is_empty() {
        return Err("at least one --mozc-system file is required".into());
    }

    let mut trimmer = MozcTrimmer::new(config.policy).map_err(|error| error.to_string())?;
    for path in &config.shards {
        let source = path.display().to_string();
        let text = read_utf8(path)?;
        let shard = parse_mozc_entries(&source, &text).map_err(|error| error.to_string())?;
        trimmer.push_shard(shard);
    }
    let (mut entries, legacy_evidence, mut trim_report) = trimmer.finish_with_legacy_evidence();
    let pos_source = config.pos_catalog.display().to_string();
    let pos_catalog = parse_mozc_pos_catalog(&pos_source, &read_utf8(&config.pos_catalog)?)?;
    let boundary_policy_source = config.non_initial_policy.display().to_string();
    let boundary_policy_bytes = std::fs::read(&config.non_initial_policy)
        .map_err(|error| format!("read {}: {error}", config.non_initial_policy.display()))?;
    let boundary_policy_text = std::str::from_utf8(&boundary_policy_bytes).map_err(|error| {
        format!(
            "{} is not valid UTF-8: {error}",
            config.non_initial_policy.display()
        )
    })?;
    let boundary_policy = parse_non_initial_boundary_policy(
        &boundary_policy_source,
        boundary_policy_text,
        MOZC_UPSTREAM_COMMIT,
    )?;
    let boundary_report = mark_non_initial_allomorphs_with_boundary_policy(
        &mut entries,
        &legacy_evidence,
        &pos_catalog,
        &boundary_policy,
    )?;
    trim_report.non_initial_entries = boundary_report.remaining_non_initial_entries;
    trim_report.output_entries = entries.len();
    let boundary_policy_sha256 = sha256_hex(&boundary_policy_bytes);
    let tsv = entries_to_tsv(&entries, OUTPUT_LICENSE).map_err(|error| error.to_string())?;
    let report = report_json(
        &config,
        trim_report,
        boundary_report,
        &boundary_policy_sha256,
    )?;
    replacing_write(&config.output, tsv.as_bytes())?;
    replacing_write(&config.report, report.as_bytes())?;
    println!(
        "trimmed {} input rows to {} entries ({} cost-filtered, {} deduplicated, {} rows / {} surfaces capped, {} rows / {} surfaces rescued from the legacy row cap, {} initial pairs released after a {}-pair boundary audit) in {:.2}s",
        trim_report.input_entries,
        trim_report.output_entries,
        trim_report.input_entries.saturating_sub(trim_report.cost_eligible),
        trim_report.duplicate_entries,
        trim_report.capped_entries,
        trim_report.capped_surfaces,
        trim_report.surface_cap_rescued_entries,
        trim_report.surface_cap_rescued_surfaces,
        boundary_report.allowed_initial_pairs,
        boundary_report.audited_blocked_pairs,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Config, String> {
    let mut shards = Vec::new();
    let mut output = None;
    let mut report = None;
    let mut pos_catalog = None;
    let mut non_initial_policy = None;
    let mut max_word_cost = DEFAULT_MAX_WORD_COST;
    let mut max_surfaces_per_reading = DEFAULT_MAX_SURFACES_PER_READING;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--mozc-system") => shards.push(next_path(&mut args, &argument)?),
            Some("--mozc-id-def") => set_once(
                &mut pos_catalog,
                next_path(&mut args, &argument)?,
                "Mozc POS taxonomy",
            )?,
            Some("--non-initial-policy") => set_once(
                &mut non_initial_policy,
                next_path(&mut args, &argument)?,
                "non-initial boundary policy",
            )?,
            Some("--output") => set_once(&mut output, next_path(&mut args, &argument)?, "output")?,
            Some("--report") => set_once(&mut report, next_path(&mut args, &argument)?, "report")?,
            Some("--max-word-cost") => {
                max_word_cost = next_number(&mut args, &argument)?;
            }
            Some("--max-surfaces-per-reading") => {
                max_surfaces_per_reading = next_optional_number(&mut args, &argument)?;
            }
            Some("--help" | "-h") => {
                println!(
                    "Usage: mozc-trim --mozc-system FILE... --mozc-id-def FILE \
                     --non-initial-policy FILE \
                     --output FILE --report FILE \
                     [--max-word-cost N] [--max-surfaces-per-reading N|none]"
                );
                std::process::exit(0);
            }
            Some(other) => return Err(format!("unknown argument '{other}'; use --help")),
            None => return Err("arguments must be valid Unicode".into()),
        }
    }
    Ok(Config {
        shards,
        pos_catalog: pos_catalog.ok_or("--mozc-id-def is required")?,
        non_initial_policy: non_initial_policy.ok_or("--non-initial-policy is required")?,
        output: output.ok_or("--output is required")?,
        report: report.ok_or("--report is required")?,
        policy: TrimPolicy {
            max_word_cost,
            legacy_row_evidence_cap: LEGACY_ROW_EVIDENCE_CAP,
            max_surfaces_per_reading,
        },
    })
}

fn report_json(
    config: &Config,
    report: TrimReport,
    boundary: NonInitialBoundaryReport,
    boundary_policy_sha256: &str,
) -> Result<String, String> {
    let mut output = String::new();
    writeln!(
        &mut output,
        "{{\n  \"schema_version\": 4,\n  \"mozc_repository\": {},\n  \"mozc_revision\": {},\n  \"output_license\": {},",
        json_string(MOZC_REPOSITORY),
        json_string(MOZC_UPSTREAM_COMMIT),
        json_string(OUTPUT_LICENSE)
    )
    .map_err(|error| error.to_string())?;
    writeln!(
        &mut output,
        "  \"source_shards\": {},\n  \"max_word_cost\": {},\n  \"max_surfaces_per_reading\": {},\n  \"candidate_cap_unit\": \"surface\",\n  \"legacy_row_evidence_cap\": {},\n  \"non_initial_policy_sha256\": {},\n  \"input_entries\": {},\n  \"cost_eligible\": {},\n  \"duplicate_entries\": {},\n  \"legacy_row_capped_entries\": {},\n  \"capped_entries\": {},\n  \"capped_surfaces\": {},\n  \"surface_cap_rescued_entries\": {},\n  \"surface_cap_rescued_surfaces\": {},\n  \"raw_non_initial_entries\": {},\n  \"raw_non_initial_pairs\": {},\n  \"already_initial_pairs\": {},\n  \"audited_blocked_pairs\": {},\n  \"allowed_initial_pairs\": {},\n  \"retained_non_initial_pairs\": {},\n  \"released_non_initial_entries\": {},\n  \"generated_initial_alias_entries\": {},\n  \"non_initial_entries\": {},\n  \"output_entries\": {}\n}}",
        config.shards.len(),
        config.policy.max_word_cost,
        config
            .policy
            .max_surfaces_per_reading
            .map_or_else(|| "null".to_string(), |cap| cap.to_string()),
        config.policy.legacy_row_evidence_cap,
        json_string(boundary_policy_sha256),
        report.input_entries,
        report.cost_eligible,
        report.duplicate_entries,
        report.legacy_row_capped_entries,
        report.capped_entries,
        report.capped_surfaces,
        report.surface_cap_rescued_entries,
        report.surface_cap_rescued_surfaces,
        boundary.raw_non_initial_entries,
        boundary.raw_non_initial_pairs,
        boundary.already_initial_pairs,
        boundary.audited_blocked_pairs,
        boundary.allowed_initial_pairs,
        boundary.retained_non_initial_pairs,
        boundary.released_non_initial_entries,
        boundary.generated_initial_alias_entries,
        report.non_initial_entries,
        report.output_entries
    )
    .map_err(|error| error.to_string())?;
    Ok(output)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("write to String");
    }
    output
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            control if control <= '\u{1f}' => {
                write!(&mut output, "\\u{:04x}", u32::from(control)).expect("write to String");
            }
            other => output.push(other),
        }
    }
    output.push('"');
    output
}

fn read_utf8(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn replacing_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("output path {} has no file name", path.display()))?;
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(format!(".{}.tmp", std::process::id()));
    let temporary = parent.join(temporary_name);
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| format!("replace {}: {error}", path.display()))?;
    }
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!(
            "rename {} to {}: {error}",
            temporary.display(),
            path.display()
        )
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, label: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{label} was specified more than once"));
    }
    *slot = Some(value);
    Ok(())
}

fn next_path(
    args: &mut impl Iterator<Item = OsString>,
    option: &OsString,
) -> Result<PathBuf, String> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{} requires a path", option.to_string_lossy()))
}

fn next_optional_number(
    args: &mut impl Iterator<Item = OsString>,
    option: &OsString,
) -> Result<Option<usize>, String> {
    let value = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| {
            format!(
                "{} requires a Unicode number or 'none'",
                option.to_string_lossy()
            )
        })?;
    if value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    value.parse::<usize>().map(Some).map_err(|_| {
        format!(
            "{} requires a valid number or 'none', found '{value}'",
            option.to_string_lossy()
        )
    })
}

fn next_number<T>(args: &mut impl Iterator<Item = OsString>, option: &OsString) -> Result<T, String>
where
    T: std::str::FromStr,
{
    let value = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| format!("{} requires a Unicode number", option.to_string_lossy()))?;
    value.parse::<T>().map_err(|_| {
        format!(
            "{} requires a valid number, found '{value}'",
            option.to_string_lossy()
        )
    })
}
