//! Emits deterministic, definition-free JSONL batches for the LLM detail lane.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use dictc::category::DictionaryCategory;
use dictc::llm_detail_targets::{
    coverage_from_annotated_entries, file_sha256, parse_allowlist_tsv, parse_coverage_tsv,
    select_targets, write_batches, CategorizedSourceEntry, CoveredIdentity,
};
use dictc::{parse_category_entries, parse_entries};

#[derive(Clone, Copy)]
enum InputFormat {
    Category,
    Sakura,
}

struct Input {
    path: PathBuf,
    category: DictionaryCategory,
    format: InputFormat,
}

struct CoverageInput {
    path: PathBuf,
    /// Stable, manifest-safe logical identity.  An external coverage artifact
    /// must name the pinned source/revision that produced it.
    source_id: String,
}

struct Config {
    inputs: Vec<Input>,
    coverage: Vec<CoverageInput>,
    coverage_entries: Vec<PathBuf>,
    output_directory: PathBuf,
    batch_size: usize,
    max_targets: Option<usize>,
    allowlist: Option<PathBuf>,
    prompt_version: String,
}

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("llm-detail-targets: {error}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let Some(config) = parse_args(args)? else {
        return Ok(());
    };
    let mut source_sha256 = BTreeMap::new();
    let mut entries = Vec::new();
    for input in config.inputs {
        let source = input.path.display().to_string();
        let text = read_utf8(&input.path)?;
        let parsed = match input.format {
            InputFormat::Category => parse_category_entries(&source, &text),
            InputFormat::Sakura => parse_entries(&source, &text),
        }
        .map_err(|error| error.to_string())?;
        insert_source_hash(
            &mut source_sha256,
            logical_source_id(&input.path, "category"),
            file_sha256(&input.path)?,
        )?;
        entries.extend(parsed.into_iter().map(|entry| CategorizedSourceEntry {
            entry,
            category: input.category,
        }));
    }
    let mut covered = BTreeSet::<CoveredIdentity>::new();
    for input in &config.coverage {
        let source = input.path.display().to_string();
        let coverage = parse_coverage_tsv(&source, &read_utf8(&input.path)?)?;
        insert_source_hash(
            &mut source_sha256,
            input.source_id.clone(),
            file_sha256(&input.path)?,
        )?;
        covered.extend(coverage);
    }
    for path in &config.coverage_entries {
        let source = path.display().to_string();
        let entries =
            parse_entries(&source, &read_utf8(path)?).map_err(|error| error.to_string())?;
        insert_source_hash(
            &mut source_sha256,
            logical_source_id(path, "coverage-entry"),
            file_sha256(path)?,
        )?;
        covered.extend(coverage_from_annotated_entries(entries));
    }
    let mut selection = select_targets(entries, &covered);
    if let Some(path) = &config.allowlist {
        let text = read_utf8(path)?;
        let allowed = parse_allowlist_tsv(&path.display().to_string(), &text)?;
        let selected = selection
            .targets
            .iter()
            .map(|target| (target.surface.clone(), target.reading.clone()))
            .collect::<BTreeSet<_>>();
        if let Some((surface, reading)) = allowed.iter().find(|pair| !selected.contains(*pair)) {
            return Err(format!(
                "allowlist pair is not a safe selected target: {surface}/{reading}"
            ));
        }
        selection
            .targets
            .retain(|target| allowed.contains(&(target.surface.clone(), target.reading.clone())));
        selection.counts.selected_targets = selection.targets.len();
        selection.allowlist_sha256 = Some(file_sha256(path)?);
    }
    if let Some(maximum) = config.max_targets {
        selection.targets.truncate(maximum);
        selection.counts.selected_targets = selection.targets.len();
        selection.target_limit = Some(maximum);
    }
    write_batches(
        &config.output_directory,
        &selection,
        config.batch_size,
        &config.prompt_version,
        &source_sha256,
    )?;
    println!(
        "wrote {} targets in {} batch(es); held {} terms",
        selection.targets.len(),
        selection.targets.len().div_ceil(config.batch_size),
        selection.held.len(),
    );
    Ok(())
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Option<Config>, String> {
    let mut inputs = Vec::new();
    let mut coverage = Vec::<CoverageInput>::new();
    let mut coverage_entries = Vec::new();
    let mut output_directory = None;
    let mut batch_size = 200usize;
    let mut max_targets = None;
    let mut allowlist = None;
    let mut prompt_version = "sakura.llm-detail-prompt.v1".to_owned();
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--category") => inputs.push(Input {
                category: category_id(&mut args, &argument)?,
                path: next_path(&mut args, &argument)?,
                format: InputFormat::Category,
            }),
            Some("--entries") => inputs.push(Input {
                category: category_id(&mut args, &argument)?,
                path: next_path(&mut args, &argument)?,
                format: InputFormat::Sakura,
            }),
            Some("--coverage") => coverage.push(CoverageInput {
                path: next_path(&mut args, &argument)?,
                source_id: String::new(),
            }),
            Some("--coverage-source-id") => {
                let source_id = next_string(&mut args, &argument)?;
                let coverage = coverage
                    .last_mut()
                    .ok_or("--coverage-source-id must immediately follow a --coverage input")?;
                if !coverage.source_id.is_empty() {
                    return Err("each --coverage input accepts one --coverage-source-id".into());
                }
                if !is_logical_source_id(&source_id) {
                    return Err(
                        "--coverage-source-id must be a stable logical ID without paths".into(),
                    );
                }
                coverage.source_id = source_id;
            }
            Some("--coverage-entries") => coverage_entries.push(next_path(&mut args, &argument)?),
            Some("--output-dir") => set_once(
                &mut output_directory,
                next_path(&mut args, &argument)?,
                "output directory",
            )?,
            Some("--batch-size") => {
                batch_size = next_usize(&mut args, &argument)?;
                if batch_size == 0 {
                    return Err("--batch-size must be greater than zero".into());
                }
            }
            Some("--max-targets") => {
                max_targets = Some(next_usize(&mut args, &argument)?);
            }
            Some("--allowlist") => set_once(
                &mut allowlist,
                next_path(&mut args, &argument)?,
                "allowlist",
            )?,
            Some("--prompt-version") => {
                prompt_version = next_string(&mut args, &argument)?;
                if prompt_version.trim().is_empty() {
                    return Err("--prompt-version must not be empty".into());
                }
            }
            Some("--help" | "-h") => {
                println!("Usage: llm-detail-targets --output-dir DIR [--batch-size N] [--max-targets N] [--allowlist FILE] [--prompt-version VERSION]");
                println!("       (--category CATEGORY_ID CATEGORY_TSV | --entries CATEGORY_ID SAKURA_TSV)... [--coverage DETAIL_COVERAGE_TSV --coverage-source-id PINNED_LOGICAL_ID]... [--coverage-entries ANNOTATED_SAKURA_TSV]...");
                println!("Coverage header: reading<TAB>surface<TAB>left_id<TAB>right_id");
                return Ok(None);
            }
            Some(other) => return Err(format!("unknown argument '{other}'; use --help")),
            None => return Err("arguments must be Unicode".into()),
        }
    }
    if inputs.is_empty() {
        return Err("at least one --category or --entries input is required".into());
    }
    if coverage
        .iter()
        .any(|coverage| coverage.source_id.is_empty())
    {
        return Err("every --coverage input requires a following --coverage-source-id".into());
    }
    Ok(Some(Config {
        inputs,
        coverage,
        coverage_entries,
        output_directory: output_directory.ok_or("--output-dir is required")?,
        batch_size,
        max_targets,
        allowlist,
        prompt_version,
    }))
}

fn category_id(
    args: &mut impl Iterator<Item = OsString>,
    option: &OsString,
) -> Result<DictionaryCategory, String> {
    let id = next_usize(args, option)?;
    u8::try_from(id)
        .ok()
        .and_then(DictionaryCategory::from_id)
        .ok_or_else(|| format!("{} category id must be 1..=14", option.to_string_lossy()))
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
        .ok_or_else(|| format!("{} requires Unicode text", option.to_string_lossy()))
}

fn next_usize(
    args: &mut impl Iterator<Item = OsString>,
    option: &OsString,
) -> Result<usize, String> {
    next_string(args, option)?.parse().map_err(|_| {
        format!(
            "{} requires a non-negative integer",
            option.to_string_lossy()
        )
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, label: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        Err(format!("{label} was specified more than once"))
    } else {
        Ok(())
    }
}

fn read_utf8(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn insert_source_hash(
    sources: &mut BTreeMap<String, String>,
    source_id: String,
    sha256: String,
) -> Result<(), String> {
    if let Some(previous) = sources.insert(source_id.clone(), sha256.clone()) {
        if previous != sha256 {
            return Err(format!("logical source ID collision: {source_id}"));
        }
    }
    Ok(())
}

fn logical_source_id(path: &Path, namespace: &str) -> String {
    let repository = std::env::current_dir()
        .ok()
        .and_then(|path| path.canonicalize().ok());
    let canonical = path.canonicalize().ok();
    if let Some(relative) = repository
        .as_deref()
        .zip(canonical.as_deref())
        .and_then(|(repository, path)| path.strip_prefix(repository).ok())
    {
        return format!("repo/{}", relative.to_string_lossy().replace('\\', "/"));
    }
    format!(
        "{namespace}/{}",
        path.file_name().unwrap_or_default().to_string_lossy()
    )
}

fn is_logical_source_id(value: &str) -> bool {
    !value.is_empty()
        && !Path::new(value).is_absolute()
        && !value.contains(['\\', ':'])
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::run;

    #[test]
    fn resume_is_idempotent_and_malformed_source_is_rejected() {
        let root = std::env::temp_dir().join(format!("sakura-llm-targets-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let input = root.join("general.tsv");
        let output = root.join("out");
        fs::write(&input, "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nてすと\tテスト\t1\t1\t1\t-\t\t\n").unwrap();
        let args = || {
            vec![
                "--category".into(),
                "3".into(),
                input.clone().into_os_string(),
                "--output-dir".into(),
                output.clone().into_os_string(),
            ]
        };
        run(args().into_iter()).unwrap();
        let one = fs::read(output.join("000001.targets.jsonl")).unwrap();
        run(args().into_iter()).unwrap();
        assert_eq!(one, fs::read(output.join("000001.targets.jsonl")).unwrap());
        fs::write(&input, "bad\n").unwrap();
        assert!(run(args().into_iter()).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn help_is_a_successful_terminal_state_without_writes() {
        let root =
            std::env::temp_dir().join(format!("sakura-llm-targets-help-{}", std::process::id()));
        let args = vec![
            "--help".into(),
            "--output-dir".into(),
            root.clone().into_os_string(),
        ];
        assert!(run(args.into_iter()).is_ok());
        assert!(!root.exists(), "--help must not create an output directory");
    }

    #[test]
    fn annotated_coverage_entries_exclude_only_the_exact_ordinal() {
        let root = std::env::temp_dir().join(format!(
            "sakura-llm-targets-coverage-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let input = root.join("general.tsv");
        let coverage = root.join("curated.tsv");
        let output = root.join("out");
        let header =
            "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n";
        fs::write(&input, format!("{header}あ\t語\t1\t1\t1\t-\t\t\n")).unwrap();
        fs::write(
            &coverage,
            format!("# license: MIT\n{header}あ\t語\t1\t1\t1\t-\t\t既存の説明。\n"),
        )
        .unwrap();
        run(vec![
            "--category".into(),
            "3".into(),
            input.into_os_string(),
            "--coverage-entries".into(),
            coverage.into_os_string(),
            "--output-dir".into(),
            output.clone().into_os_string(),
        ]
        .into_iter())
        .unwrap();
        assert!(
            !output.join("000001.targets.jsonl").exists(),
            "annotated exact coverage must remove the only candidate"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn external_coverage_requires_a_logical_pinned_identity() {
        let root = std::env::temp_dir().join(format!(
            "sakura-llm-targets-portable-manifest-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let input = root.join("general.tsv");
        let coverage = root.join("coverage.tsv");
        let output = root.join("out");
        let header =
            "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n";
        fs::write(&input, format!("{header}てすと\tテスト\t1\t1\t1\t-\t\t\n")).unwrap();
        fs::write(
            &coverage,
            "reading\tsurface\tleft_id\tright_id\n別\t別\t1\t1\n",
        )
        .unwrap();
        let args = || {
            vec![
                "--category".into(),
                "3".into(),
                input.clone().into_os_string(),
                "--coverage".into(),
                coverage.clone().into_os_string(),
                "--coverage-source-id".into(),
                "pinned/wordnet-1.1+smile-chat".into(),
                "--output-dir".into(),
                output.clone().into_os_string(),
            ]
        };
        run(args().into_iter()).unwrap();
        let manifest = fs::read_to_string(output.join("manifest.json")).unwrap();
        assert!(manifest.contains("pinned/wordnet-1.1+smile-chat"));
        assert!(!manifest.contains(&coverage.display().to_string()));
        assert!(!output.join("held.jsonl").exists());
        let missing_id = vec![
            "--category".into(),
            "3".into(),
            input.into_os_string(),
            "--coverage".into(),
            coverage.into_os_string(),
            "--output-dir".into(),
            root.join("missing-id").into_os_string(),
        ];
        assert!(run(missing_id.into_iter()).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
