//! Expand trimmed Mozc 基本形 lemmas into fused Japanese conjugations.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use dictc::inflection::{expand_inflections, parse_inflection_pos_catalog, InflectionReport};
use dictc::{entries_to_tsv, parse_entries};

const OUTPUT_LICENSE: &str = "LicenseRef-Mozc-Dictionary";

struct Config {
    system: PathBuf,
    mozc_pos: PathBuf,
    output: PathBuf,
    report: PathBuf,
}

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("inflection-expand: {error}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let started = Instant::now();
    let config = parse_args(args)?;
    let lemmas = parse_entries(
        &config.system.display().to_string(),
        &read_utf8(&config.system)?,
    )
    .map_err(|error| error.to_string())?;
    let catalog = parse_inflection_pos_catalog(
        &config.mozc_pos.display().to_string(),
        &read_utf8(&config.mozc_pos)?,
    )?;
    let (entries, report) = expand_inflections(&lemmas, &catalog)?;
    let tsv = entries_to_tsv(&entries, OUTPUT_LICENSE).map_err(|error| error.to_string())?;
    replacing_write(&config.output, tsv.as_bytes())?;
    replacing_write(&config.report, report_json(&report)?.as_bytes())?;
    println!(
        "expanded {} 基本形 lemmas into {} missing conjugations ({} already present, {} unsupported) in {:.2}s",
        report.lemma_entries,
        report.emitted_entries,
        report.skipped_existing,
        report.skipped_unsupported,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Config, String> {
    let mut system = None;
    let mut mozc_pos = None;
    let mut output = None;
    let mut report = None;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--system") => set_once(&mut system, next_path(&mut args, &argument)?, "system")?,
            Some("--mozc-id-def") => {
                set_once(
                    &mut mozc_pos,
                    next_path(&mut args, &argument)?,
                    "mozc-id-def",
                )?;
            }
            Some("--output") => set_once(&mut output, next_path(&mut args, &argument)?, "output")?,
            Some("--report") => set_once(&mut report, next_path(&mut args, &argument)?, "report")?,
            Some("--help" | "-h") => {
                println!(
                    "Usage: inflection-expand --system FILE --mozc-id-def FILE --output FILE --report FILE"
                );
                std::process::exit(0);
            }
            Some(other) => return Err(format!("unknown argument '{other}'; use --help")),
            None => return Err("arguments must be valid Unicode".into()),
        }
    }
    Ok(Config {
        system: system.ok_or("--system is required")?,
        mozc_pos: mozc_pos.ok_or("--mozc-id-def is required")?,
        output: output.ok_or("--output is required")?,
        report: report.ok_or("--report is required")?,
    })
}

fn report_json(report: &InflectionReport) -> Result<String, String> {
    let mut output = String::new();
    writeln!(
        &mut output,
        "{{\n  \"schema_version\": 1,\n  \"lemma_entries\": {},\n  \"emitted_entries\": {},\n  \"skipped_existing\": {},\n  \"skipped_unsupported\": {}\n}}",
        report.lemma_entries,
        report.emitted_entries,
        report.skipped_existing,
        report.skipped_unsupported
    )
    .map_err(|error| error.to_string())?;
    Ok(output)
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
