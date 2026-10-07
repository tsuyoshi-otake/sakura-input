//! Reproducible smile-chat glossary to Sakura overlay TSV importer.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use dictc_core::glossary::{parse_part, ImportResult, Importer, OverlayDefaults};
use dictc_core::segmenter::parse_mozc_pos_features;
use dictc_core::{
    entries_to_tsv, parse_entries, parse_mozc_connection, parse_mozc_entries, MOZC_UPSTREAM_COMMIT,
};

const GLOSSARY_REPOSITORY: &str =
    "https://github.com/systemexe-research-and-development/smile-chat";
const MOZC_REPOSITORY: &str = "https://github.com/google/mozc";
// The imported JSON lives below `frontend/public/LICENSE` in the pinned
// smile-chat tree. That nearest license boundary is MIT, even though the
// repository root carries a different project-wide notice.
const GLOSSARY_LICENSE_PATH: &str = "frontend/public/LICENSE";
const OUTPUT_LICENSE: &str = "MIT";
const DEFAULT_NOUN_ID: u16 = 1_851;
const DEFAULT_WORD_COST: i32 = 4_800;

struct Config {
    glossary_parts: Vec<PathBuf>,
    glossary_directory: Option<PathBuf>,
    mozc_shards: Vec<PathBuf>,
    upstream_systems: Vec<PathBuf>,
    mozc_connection: PathBuf,
    mozc_id_def: PathBuf,
    output: PathBuf,
    report: PathBuf,
    glossary_revision: String,
    defaults: OverlayDefaults,
}

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("glossary-import: {error}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let started = Instant::now();
    let mut config = parse_args(args)?;
    if let Some(directory) = config.glossary_directory.take() {
        config.glossary_parts.extend(find_parts(&directory)?);
    }
    config.glossary_parts.sort();
    config.glossary_parts.dedup();
    config.mozc_shards.sort();
    config.mozc_shards.dedup();
    config.upstream_systems.sort();
    config.upstream_systems.dedup();
    if config.glossary_parts.is_empty() {
        return Err("at least one --glossary or a non-empty --glossary-dir is required".into());
    }
    if config.mozc_shards.is_empty() {
        return Err("at least one --mozc-system file is required".into());
    }
    if config.upstream_systems.is_empty() {
        return Err("at least one --upstream-system file is required".into());
    }

    let mut terms = Vec::new();
    for path in &config.glossary_parts {
        let source = path.display().to_string();
        let text = read_utf8(path)?;
        terms.extend(parse_part(&source, &text).map_err(|error| error.to_string())?);
    }

    let mut importer = Importer::new(&terms, config.defaults).map_err(|error| error.to_string())?;
    drop(terms);
    for path in &config.mozc_shards {
        let source = path.display().to_string();
        let text = read_utf8(path)?;
        let shard = parse_mozc_entries(&source, &text).map_err(|error| error.to_string())?;
        importer.match_mozc(&shard);
    }
    // Matched glossary edges are ranked against the pre-overlay dictionary
    // the image actually ships (trimmed Mozc plus generated inflections),
    // not against the raw shards they were matched in.
    for path in &config.upstream_systems {
        let source = path.display().to_string();
        let text = read_utf8(path)?;
        let entries = parse_entries(&source, &text).map_err(|error| error.to_string())?;
        importer.observe_upstream(&entries);
    }
    let connection = parse_mozc_connection(
        &config.mozc_connection.display().to_string(),
        &read_utf8(&config.mozc_connection)?,
        true,
    )
    .map_err(|error| error.to_string())?;
    let pos_features = parse_mozc_pos_features(
        &config.mozc_id_def.display().to_string(),
        &read_utf8(&config.mozc_id_def)?,
    )
    .map_err(|error| error.to_string())?;
    if pos_features.len() != usize::from(connection.class_count()) {
        return Err(format!(
            "{} names {} classes but {} has {}; they must come from one Mozc revision",
            config.mozc_id_def.display(),
            pos_features.len(),
            config.mozc_connection.display(),
            connection.class_count()
        ));
    }
    let imported = importer.finish(&connection, &pos_features);
    let tsv =
        entries_to_tsv(&imported.entries, OUTPUT_LICENSE).map_err(|error| error.to_string())?;
    let report = report_json(&config, &imported)?;

    replacing_write(&config.output, tsv.as_bytes())?;
    replacing_write(&config.report, report.as_bytes())?;
    println!(
        "wrote {} overlay entries ({} ASCII aliases, {} ASCII-only terms, {} Mozc matches, {} yielded to upstream leaders, {} defaults, {} gaps) in {:.2}s",
        imported.report.surfaces,
        imported.report.ascii_aliases,
        imported.report.ascii_only_terms,
        imported.report.matched_to_mozc,
        imported.report.upstream_yields.len(),
        imported.report.defaulted,
        imported.report.gaps.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Config, String> {
    let mut glossary_parts = Vec::new();
    let mut glossary_directory = None;
    let mut mozc_shards = Vec::new();
    let mut upstream_systems = Vec::new();
    let mut mozc_connection = None;
    let mut mozc_id_def = None;
    let mut output = None;
    let mut report = None;
    let mut glossary_revision = None;
    let mut defaults = OverlayDefaults {
        katakana_left_id: DEFAULT_NOUN_ID,
        katakana_right_id: DEFAULT_NOUN_ID,
        ascii_left_id: DEFAULT_NOUN_ID,
        ascii_right_id: DEFAULT_NOUN_ID,
        base_word_cost: DEFAULT_WORD_COST,
    };
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--glossary") => glossary_parts.push(next_path(&mut args, &argument)?),
            Some("--glossary-dir") => set_once(
                &mut glossary_directory,
                next_path(&mut args, &argument)?,
                "glossary directory",
            )?,
            Some("--mozc-system") => mozc_shards.push(next_path(&mut args, &argument)?),
            Some("--upstream-system") => {
                upstream_systems.push(next_path(&mut args, &argument)?);
            }
            Some("--mozc-connection") => set_once(
                &mut mozc_connection,
                next_path(&mut args, &argument)?,
                "Mozc connection",
            )?,
            Some("--mozc-id-def") => set_once(
                &mut mozc_id_def,
                next_path(&mut args, &argument)?,
                "Mozc id.def taxonomy",
            )?,
            Some("--output") => set_once(&mut output, next_path(&mut args, &argument)?, "output")?,
            Some("--report") => set_once(&mut report, next_path(&mut args, &argument)?, "report")?,
            Some("--glossary-revision") => {
                let value = next_string(&mut args, &argument)?;
                validate_revision(&value)?;
                set_once(&mut glossary_revision, value, "glossary revision")?;
            }
            Some("--katakana-left-id") => {
                defaults.katakana_left_id = next_number(&mut args, &argument)?;
            }
            Some("--katakana-right-id") => {
                defaults.katakana_right_id = next_number(&mut args, &argument)?;
            }
            Some("--ascii-left-id") => {
                defaults.ascii_left_id = next_number(&mut args, &argument)?;
            }
            Some("--ascii-right-id") => {
                defaults.ascii_right_id = next_number(&mut args, &argument)?;
            }
            Some("--base-word-cost") => {
                defaults.base_word_cost = next_number(&mut args, &argument)?;
                if defaults.base_word_cost < 0 {
                    return Err("--base-word-cost must be non-negative".into());
                }
            }
            Some("--help" | "-h") => {
                println!(
                    "Usage: glossary-import (--glossary FILE | --glossary-dir DIR) \\\n+                     --mozc-system FILE... --upstream-system FILE... \\\n+                     --mozc-connection FILE --mozc-id-def FILE \\\n+                     --glossary-revision SHA \\\n+                     --output FILE --report FILE [default-id/cost options]"
                );
                std::process::exit(0);
            }
            Some(other) => return Err(format!("unknown argument '{other}'; use --help")),
            None => return Err("arguments must be valid Unicode".into()),
        }
    }
    Ok(Config {
        glossary_parts,
        glossary_directory,
        mozc_shards,
        upstream_systems,
        mozc_connection: mozc_connection.ok_or("--mozc-connection is required")?,
        mozc_id_def: mozc_id_def.ok_or("--mozc-id-def is required")?,
        output: output.ok_or("--output is required")?,
        report: report.ok_or("--report is required")?,
        glossary_revision: glossary_revision.ok_or("--glossary-revision is required")?,
        defaults,
    })
}

fn find_parts(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {}: {error}", directory.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_file() && name.starts_with("ja_part") && name.ends_with(".json") {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn report_json(config: &Config, imported: &ImportResult) -> Result<String, String> {
    let mut output = String::new();
    writeln!(&mut output, "{{").map_err(|error| error.to_string())?;
    writeln!(
        &mut output,
        "  \"schema_version\": 1,\n  \"glossary_repository\": {},\n  \"glossary_revision\": {},\n  \"glossary_license_path\": {},",
        json_string(GLOSSARY_REPOSITORY),
        json_string(&config.glossary_revision),
        json_string(GLOSSARY_LICENSE_PATH)
    )
    .map_err(|error| error.to_string())?;
    writeln!(
        &mut output,
        "  \"mozc_repository\": {},\n  \"mozc_revision\": {},\n  \"output_license\": {},",
        json_string(MOZC_REPOSITORY),
        json_string(MOZC_UPSTREAM_COMMIT),
        json_string(OUTPUT_LICENSE)
    )
    .map_err(|error| error.to_string())?;
    writeln!(
        &mut output,
        "  \"glossary_parts\": {},\n  \"mozc_shards\": {},\n  \"upstream_systems\": {},\n  \"terms\": {},\n  \"surfaces\": {},\n  \"ascii_aliases\": {},\n  \"ascii_only_terms\": {},\n  \"matched_to_mozc\": {},\n  \"defaulted\": {},\n  \"duplicate_surfaces\": {},",
        config.glossary_parts.len(),
        config.mozc_shards.len(),
        config.upstream_systems.len(),
        imported.report.terms,
        imported.report.surfaces,
        imported.report.ascii_aliases,
        imported.report.ascii_only_terms,
        imported.report.matched_to_mozc,
        imported.report.defaulted,
        imported.report.duplicate_surfaces
    )
    .map_err(|error| error.to_string())?;
    write_string_array(
        &mut output,
        "upstream_yields",
        &imported.report.upstream_yields,
        ",",
    )?;
    write_string_array(&mut output, "gaps", &imported.report.gaps, "")?;
    output.push_str("}\n");
    Ok(output)
}

fn write_string_array(
    output: &mut String,
    name: &str,
    values: &[String],
    trailer: &str,
) -> Result<(), String> {
    writeln!(output, "  {}: [", json_string(name)).map_err(|error| error.to_string())?;
    for (index, value) in values.iter().enumerate() {
        let comma = if index + 1 == values.len() { "" } else { "," };
        writeln!(output, "    {}{comma}", json_string(value)).map_err(|error| error.to_string())?;
    }
    writeln!(output, "  ]{trailer}").map_err(|error| error.to_string())
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
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

fn next_string(
    args: &mut impl Iterator<Item = OsString>,
    option: &OsString,
) -> Result<String, String> {
    args.next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| format!("{} requires a Unicode value", option.to_string_lossy()))
}

fn next_number<T>(args: &mut impl Iterator<Item = OsString>, option: &OsString) -> Result<T, String>
where
    T: std::str::FromStr,
{
    let value = next_string(args, option)?;
    value.parse::<T>().map_err(|_| {
        format!(
            "{} requires a valid number, found '{value}'",
            option.to_string_lossy()
        )
    })
}

fn validate_revision(value: &str) -> Result<(), String> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("--glossary-revision must be a full lowercase 40-character Git SHA".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{json_string, validate_revision};

    #[test]
    fn json_escaping_covers_controls_and_quotes() {
        assert_eq!(json_string("a\n\"\\\u{01}"), "\"a\\n\\\"\\\\\\u0001\"");
    }

    #[test]
    fn revisions_must_be_pinned_full_hashes() {
        assert!(validate_revision("b5cada441b41c207ab49bf2cd5f1d9c5614c5b92").is_ok());
        assert!(validate_revision("main").is_err());
        assert!(validate_revision("B5CADA441B41C207AB49BF2CD5F1D9C5614C5B92").is_err());
    }
}
