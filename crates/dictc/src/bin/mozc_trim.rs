//! Deterministic, bounded-memory trim of pinned Mozc dictionary shards.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use dictc::{
    entries_to_tsv, parse_mozc_entries, MozcTrimmer, TrimPolicy, TrimReport, MOZC_UPSTREAM_COMMIT,
};

const MOZC_REPOSITORY: &str = "https://github.com/google/mozc";
const OUTPUT_LICENSE: &str = "LicenseRef-Mozc-Dictionary";
const DEFAULT_MAX_WORD_COST: i32 = 6_900;
const DEFAULT_MAX_CANDIDATES: usize = 12;

struct Config {
    shards: Vec<PathBuf>,
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
    let (entries, trim_report) = trimmer.finish();
    let tsv = entries_to_tsv(&entries, OUTPUT_LICENSE).map_err(|error| error.to_string())?;
    let report = report_json(&config, trim_report)?;
    replacing_write(&config.output, tsv.as_bytes())?;
    replacing_write(&config.report, report.as_bytes())?;
    println!(
        "trimmed {} input rows to {} entries ({} cost-filtered, {} deduplicated, {} capped) in {:.2}s",
        trim_report.input_entries,
        trim_report.output_entries,
        trim_report.input_entries.saturating_sub(trim_report.cost_eligible),
        trim_report.duplicate_entries,
        trim_report.capped_entries,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Config, String> {
    let mut shards = Vec::new();
    let mut output = None;
    let mut report = None;
    let mut max_word_cost = DEFAULT_MAX_WORD_COST;
    let mut max_candidates_per_reading = DEFAULT_MAX_CANDIDATES;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--mozc-system") => shards.push(next_path(&mut args, &argument)?),
            Some("--output") => set_once(&mut output, next_path(&mut args, &argument)?, "output")?,
            Some("--report") => set_once(&mut report, next_path(&mut args, &argument)?, "report")?,
            Some("--max-word-cost") => {
                max_word_cost = next_number(&mut args, &argument)?;
            }
            Some("--max-candidates-per-reading") => {
                max_candidates_per_reading = next_number(&mut args, &argument)?;
            }
            Some("--help" | "-h") => {
                println!(
                    "Usage: mozc-trim --mozc-system FILE... --output FILE --report FILE \\\n+                     [--max-word-cost N] [--max-candidates-per-reading N]"
                );
                std::process::exit(0);
            }
            Some(other) => return Err(format!("unknown argument '{other}'; use --help")),
            None => return Err("arguments must be valid Unicode".into()),
        }
    }
    Ok(Config {
        shards,
        output: output.ok_or("--output is required")?,
        report: report.ok_or("--report is required")?,
        policy: TrimPolicy {
            max_word_cost,
            max_candidates_per_reading,
        },
    })
}

fn report_json(config: &Config, report: TrimReport) -> Result<String, String> {
    let mut output = String::new();
    writeln!(
        &mut output,
        "{{\n  \"schema_version\": 1,\n  \"mozc_repository\": {},\n  \"mozc_revision\": {},\n  \"output_license\": {},",
        json_string(MOZC_REPOSITORY),
        json_string(MOZC_UPSTREAM_COMMIT),
        json_string(OUTPUT_LICENSE)
    )
    .map_err(|error| error.to_string())?;
    writeln!(
        &mut output,
        "  \"source_shards\": {},\n  \"max_word_cost\": {},\n  \"max_candidates_per_reading\": {},\n  \"input_entries\": {},\n  \"cost_eligible\": {},\n  \"duplicate_entries\": {},\n  \"capped_entries\": {},\n  \"output_entries\": {}\n}}",
        config.shards.len(),
        config.policy.max_word_cost,
        config.policy.max_candidates_per_reading,
        report.input_entries,
        report.cost_eligible,
        report.duplicate_entries,
        report.capped_entries,
        report.output_entries
    )
    .map_err(|error| error.to_string())?;
    Ok(output)
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
