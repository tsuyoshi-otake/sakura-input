//! Offline Context Prediction dataset builder and verifier.

use std::collections::BTreeMap;
use std::env;
use std::path::{Component, Path, PathBuf};

use dictc::context_dataset::{build_dataset, verify_dataset, BuildConfig};

const DEFAULT_TIER_A_AUDIT: usize = 1_000;
const DEFAULT_TIER_B_AUDIT: usize = 100;
const DEFAULT_TIER_C_AUDIT: usize = 100;
const MAX_AUDIT_RECORDS: usize = 1_000_000;

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("context-dataset: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let Some((command, rest)) = arguments.split_first() else {
        return Err(usage());
    };
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        println!("{}", usage());
        return Ok(());
    }
    let flags = parse_flags(rest)?;
    match command.as_str() {
        "build" => build(&flags),
        "verify" => verify(&flags),
        _ => Err(format!("unknown command {command:?}\n{}", usage())),
    }
}

fn build(flags: &BTreeMap<String, String>) -> Result<(), String> {
    reject_unknown(
        flags,
        &[
            "--records",
            "--source-manifest",
            "--output-dir",
            "--generator-sha256",
            "--dictionary-sha256",
            "--audit-tier-a",
            "--audit-tier-b",
            "--audit-tier-c",
            "--repo-root",
        ],
    )?;
    let repo_root = canonical_existing(
        flags
            .get("--repo-root")
            .map(PathBuf::from)
            .unwrap_or(env::current_dir().map_err(|error| error.to_string())?),
        "repository root",
    )?;
    let records = canonical_existing(required(flags, "--records")?, "record input")?;
    let source_manifest =
        canonical_existing(required(flags, "--source-manifest")?, "source manifest")?;
    let output_directory = canonical_new_directory(required(flags, "--output-dir")?)?;
    require_external(&repo_root, &records, "record input")?;
    require_external(&repo_root, &output_directory, "output directory")?;

    let config = BuildConfig {
        records,
        source_manifest,
        output_directory,
        generator_sha256: required(flags, "--generator-sha256")?
            .to_string_lossy()
            .into_owned(),
        dictionary_sha256: required(flags, "--dictionary-sha256")?
            .to_string_lossy()
            .into_owned(),
        audit_tier_a: audit_count(flags, "--audit-tier-a", DEFAULT_TIER_A_AUDIT)?,
        audit_tier_b: audit_count(flags, "--audit-tier-b", DEFAULT_TIER_B_AUDIT)?,
        audit_tier_c: audit_count(flags, "--audit-tier-c", DEFAULT_TIER_C_AUDIT)?,
    };
    let manifest = build_dataset(&config)?;
    println!(
        "built {} accepted records from {}; Tier A held-out {}/{} ({})",
        manifest.deduplication.accepted_records,
        manifest.deduplication.input_records,
        manifest.audit_gate.tier_a_available,
        manifest.audit_gate.tier_a_required,
        if manifest.audit_gate.tier_a_requirement_met {
            "audit gate met"
        } else {
            "audit gate NOT met"
        }
    );
    Ok(())
}

fn verify(flags: &BTreeMap<String, String>) -> Result<(), String> {
    reject_unknown(flags, &["--dataset-dir"])?;
    let directory = canonical_existing(required(flags, "--dataset-dir")?, "dataset directory")?;
    let manifest = verify_dataset(&directory)?;
    println!(
        "verified {} accepted records; Tier A held-out {}/{} ({})",
        manifest.deduplication.accepted_records,
        manifest.audit_gate.tier_a_available,
        manifest.audit_gate.tier_a_required,
        if manifest.audit_gate.tier_a_requirement_met {
            "audit gate met"
        } else {
            "audit gate NOT met"
        }
    );
    Ok(())
}

fn parse_flags(arguments: &[String]) -> Result<BTreeMap<String, String>, String> {
    if !arguments.len().is_multiple_of(2) {
        return Err(format!("each flag requires one value\n{}", usage()));
    }
    let mut flags = BTreeMap::new();
    for pair in arguments.chunks_exact(2) {
        if !pair[0].starts_with("--") || flags.insert(pair[0].clone(), pair[1].clone()).is_some() {
            return Err(format!("invalid or duplicate flag {:?}", pair[0]));
        }
    }
    Ok(flags)
}

fn required(flags: &BTreeMap<String, String>, name: &str) -> Result<PathBuf, String> {
    flags
        .get(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing required flag {name}"))
}

fn reject_unknown(flags: &BTreeMap<String, String>, allowed: &[&str]) -> Result<(), String> {
    if let Some(name) = flags.keys().find(|name| !allowed.contains(&name.as_str())) {
        return Err(format!("unknown flag {name}"));
    }
    Ok(())
}

fn audit_count(
    flags: &BTreeMap<String, String>,
    name: &str,
    default: usize,
) -> Result<usize, String> {
    let value = flags.get(name).map_or(Ok(default), |raw| {
        raw.parse::<usize>()
            .map_err(|_| format!("{name} must be a nonnegative integer"))
    })?;
    if value > MAX_AUDIT_RECORDS {
        return Err(format!("{name} exceeds {MAX_AUDIT_RECORDS}"));
    }
    Ok(value)
}

fn canonical_existing(path: PathBuf, label: &str) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("resolve {label} {}: {error}", path.display()))
}

fn canonical_new_directory(path: PathBuf) -> Result<PathBuf, String> {
    if path.exists() {
        return Err(format!(
            "output directory already exists; use a new immutable directory: {}",
            path.display()
        ));
    }
    let absolute = if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let name = absolute
        .file_name()
        .ok_or_else(|| "output directory needs a final component".to_string())?;
    let parent = absolute
        .parent()
        .ok_or_else(|| "output directory needs an existing parent".to_string())?
        .canonicalize()
        .map_err(|error| format!("resolve output parent: {error}"))?;
    Ok(normalize_lexically(&parent.join(name)))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn require_external(repo_root: &Path, path: &Path, label: &str) -> Result<(), String> {
    if path.starts_with(repo_root) {
        return Err(format!(
            "{label} must stay outside the Git repository: {}",
            path.display()
        ));
    }
    Ok(())
}

fn usage() -> String {
    "usage:\n  context-dataset build --records <external.jsonl> --source-manifest <source-manifest.json> --output-dir <new-external-dir> --generator-sha256 <hex> --dictionary-sha256 <hex> [--audit-tier-a 1000] [--audit-tier-b 100] [--audit-tier-c 100] [--repo-root <dir>]\n  context-dataset verify --dataset-dir <dir>".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_strict_and_bounded() {
        assert!(parse_flags(&["--a".into(), "1".into(), "--a".into(), "2".into()]).is_err());
        let flags = parse_flags(&["--audit-tier-a".into(), "1000001".into()]).expect("flags");
        assert!(audit_count(&flags, "--audit-tier-a", 1_000).is_err());
    }

    #[test]
    fn lexical_normalization_collapses_parent_components() {
        let path = Path::new("C:/outside/one/../two");
        assert_eq!(normalize_lexically(path), Path::new("C:/outside/two"));
    }

    #[test]
    fn repository_paths_are_rejected() {
        let repository = Path::new("C:/work/sakura-input");
        assert!(require_external(
            repository,
            Path::new("C:/work/sakura-input/raw.jsonl"),
            "raw"
        )
        .is_err());
        assert!(
            require_external(repository, Path::new("C:/context-data/raw.jsonl"), "raw").is_ok()
        );
    }
}
