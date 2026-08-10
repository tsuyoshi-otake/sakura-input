//! Command-line front end for Sakura's deterministic dictionary compiler.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use dictc::{
    compile_with_details, merge_entries, parse_category_entries, parse_connection, parse_entries,
    parse_mozc_connection, parse_mozc_entries, wordnet::import_lmf_gzip, SourceDetail, SourceEntry,
    MAX_DICTIONARY_IMAGE_BYTES,
};
use sakura_core::dictionary::DetailRelationKind;

#[derive(Clone, Copy, Default)]
struct RelationCounts {
    aliases: usize,
    related: usize,
    similar: usize,
    antonyms: usize,
}

impl RelationCounts {
    fn from_details(details: &[SourceDetail]) -> Self {
        let mut counts = Self::default();
        for detail in details {
            for relation in &detail.relations {
                match relation.kind {
                    DetailRelationKind::Alias => counts.aliases += 1,
                    DetailRelationKind::Related => counts.related += 1,
                    // Same-synset WordNet lemmas are compiled as Synonym and
                    // rendered as Similar by the UI.
                    DetailRelationKind::Synonym => counts.similar += 1,
                    DetailRelationKind::Antonym => counts.antonyms += 1,
                }
            }
        }
        counts
    }
}

#[derive(Clone, Copy)]
enum EntryFormat {
    Sakura,
    Category,
    Mozc,
}

#[derive(Clone, Copy)]
enum EntryLayer {
    /// Low-priority, additive source.  A matching system or overlay edge wins.
    Supplement,
    System,
    Overlay,
}

#[derive(Clone, Copy)]
enum ConnectionFormat {
    Sakura,
    Mozc,
}

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("dictc: {error}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let mut entry_paths = Vec::new();
    let mut connection_path = None;
    let mut output_path = None;
    let mut wordnet_lmf_path = None;
    let mut wordnet_report_path = None;
    let mut glossary_directory = None;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--supplement") => {
                entry_paths.push((
                    next_path(&mut args, &argument)?,
                    EntryFormat::Sakura,
                    EntryLayer::Supplement,
                ));
            }
            Some("--entries" | "--system") => {
                entry_paths.push((
                    next_path(&mut args, &argument)?,
                    EntryFormat::Sakura,
                    EntryLayer::System,
                ));
            }
            Some("--overlay") => {
                entry_paths.push((
                    next_path(&mut args, &argument)?,
                    EntryFormat::Sakura,
                    EntryLayer::Overlay,
                ));
            }
            Some("--category") => {
                entry_paths.push((
                    next_path(&mut args, &argument)?,
                    EntryFormat::Category,
                    EntryLayer::System,
                ));
            }
            Some("--mozc-system") => {
                entry_paths.push((
                    next_path(&mut args, &argument)?,
                    EntryFormat::Mozc,
                    EntryLayer::System,
                ));
            }
            Some("--connection") => {
                set_once(
                    &mut connection_path,
                    (next_path(&mut args, &argument)?, ConnectionFormat::Sakura),
                    "connection source",
                )?;
            }
            Some("--mozc-connection") => {
                set_once(
                    &mut connection_path,
                    (next_path(&mut args, &argument)?, ConnectionFormat::Mozc),
                    "connection source",
                )?;
            }
            Some("--output") => output_path = Some(next_path(&mut args, &argument)?),
            Some("--wordnet-lmf") => set_once(
                &mut wordnet_lmf_path,
                next_path(&mut args, &argument)?,
                "WordNet LMF source",
            )?,
            Some("--wordnet-report") => set_once(
                &mut wordnet_report_path,
                next_path(&mut args, &argument)?,
                "WordNet import report",
            )?,
            Some("--glossary-dir") => set_once(
                &mut glossary_directory,
                next_path(&mut args, &argument)?,
                "glossary directory",
            )?,
            Some("--help" | "-h") => {
                println!(
                    "Usage: dictc (--category FILE | --system FILE | --supplement FILE | --mozc-system FILE)... \
                       (--connection FILE | --mozc-connection FILE) --output FILE \
                       [--wordnet-lmf FILE --wordnet-report FILE] [--glossary-dir DIR]"
                );
                return Ok(());
            }
            Some(other) => return Err(format!("unknown argument '{other}'; use --help")),
            None => return Err("arguments must be valid Unicode paths".to_string()),
        }
    }
    if entry_paths.is_empty() {
        return Err(
            "at least one --category/--system/--supplement/--overlay/--entries file is required"
                .to_string(),
        );
    }
    let connection_path = connection_path.ok_or("--connection is required")?;
    let output_path = output_path.ok_or("--output is required")?;
    if wordnet_lmf_path.is_some() != wordnet_report_path.is_some() {
        return Err("--wordnet-lmf and --wordnet-report must be specified together".to_owned());
    }

    let mut supplement_entries = Vec::<SourceEntry>::new();
    let mut system_entries = Vec::<SourceEntry>::new();
    let mut overlay_entries = Vec::<SourceEntry>::new();
    for (path, entry_format, layer) in entry_paths {
        let text = read_utf8(&path)?;
        let parsed = match entry_format {
            EntryFormat::Sakura => parse_entries(&path.display().to_string(), &text),
            EntryFormat::Category => parse_category_entries(&path.display().to_string(), &text),
            EntryFormat::Mozc => parse_mozc_entries(&path.display().to_string(), &text),
        }
        .map_err(|error| error.to_string())?;
        match layer {
            EntryLayer::Supplement => supplement_entries.extend(parsed),
            EntryLayer::System => system_entries.extend(parsed),
            EntryLayer::Overlay => overlay_entries.extend(parsed),
        }
    }
    // Preserve the current Mozc system layer on duplicate edges, then permit
    // curated overlays to keep their existing precedence over both layers.
    // This lets a large supplemental lexicon improve recall without replacing
    // tuned core costs or requiring its source files to pre-filter every base
    // dictionary identity.
    let entries = merge_entries(supplement_entries, system_entries)
        .and_then(|merged| merge_entries(merged, overlay_entries))
        .map_err(|error| error.to_string())?;
    let (connection_path, connection_format) = connection_path;
    let connection_text = read_utf8(&connection_path)?;
    let connection = match connection_format {
        ConnectionFormat::Sakura => parse_connection(
            &connection_path.display().to_string(),
            &connection_text,
            true,
        ),
        ConnectionFormat::Mozc => parse_mozc_connection(
            &connection_path.display().to_string(),
            &connection_text,
            true,
        ),
    }
    .map_err(|error| error.to_string())?;
    let mut details = glossary_details(glossary_directory.as_deref(), &entries)?;
    let glossary_detail_count = details.len();
    let glossary_relations = RelationCounts::from_details(&details);
    let mut wordnet_report = None;
    let wordnet_details = match wordnet_lmf_path {
        Some(path) => {
            let file = std::fs::File::open(&path)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
            let import = import_lmf_gzip(file, &entries)?;
            wordnet_report = Some(import.report);
            import.details
        }
        None => Vec::new(),
    };
    let wordnet_detail_count = wordnet_details.len();
    let wordnet_relations = RelationCounts::from_details(&wordnet_details);
    let wordnet_suppressed_by_glossary = merge_details(&mut details, wordnet_details);
    if let Some(report) = wordnet_report.as_ref() {
        let report_path = wordnet_report_path.expect("paired argument checked above");
        atomic_write(
            &report_path,
            wordnet_report_json(
                report,
                glossary_detail_count,
                glossary_relations,
                wordnet_detail_count,
                wordnet_relations,
                wordnet_suppressed_by_glossary,
                details.len(),
            )
            .as_bytes(),
        )?;
    }
    let image =
        compile_with_details(&entries, &connection, &details).map_err(|error| error.to_string())?;
    if image.len() > MAX_DICTIONARY_IMAGE_BYTES {
        return Err(format!(
            "compiled image is {} bytes, exceeding the {}-byte release gate",
            image.len(),
            MAX_DICTIONARY_IMAGE_BYTES
        ));
    }
    atomic_write(&output_path, &image)?;
    println!(
        "wrote {} entries, {} detail records, {} classes, {} bytes to {}",
        entries.len(),
        details.len(),
        connection.class_count(),
        image.len(),
        output_path.display()
    );
    Ok(())
}

fn glossary_details(
    directory: Option<&Path>,
    entries: &[SourceEntry],
) -> Result<Vec<SourceDetail>, String> {
    let Some(directory) = directory else {
        return Ok(Vec::new());
    };
    let mut paths = std::fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("ja_part") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        return Err(format!("no ja_part*.json files in {}", directory.display()));
    }
    let mut terms = Vec::new();
    for path in paths {
        let text = read_utf8(&path)?;
        terms.extend(
            dictc::glossary::parse_part(&path.display().to_string(), &text)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(dictc::glossary::detail_sources(&terms, entries))
}

fn merge_details(base: &mut Vec<SourceDetail>, overlay: Vec<SourceDetail>) -> usize {
    let mut by_identity = std::collections::BTreeMap::new();
    let mut suppressed = 0;
    for detail in std::mem::take(base) {
        by_identity
            .entry((
                detail.reading.clone(),
                detail.surface.clone(),
                detail.left_id,
                detail.right_id,
            ))
            .or_insert(detail);
    }
    for detail in overlay {
        let key = (
            detail.reading.clone(),
            detail.surface.clone(),
            detail.left_id,
            detail.right_id,
        );
        if let std::collections::btree_map::Entry::Vacant(slot) = by_identity.entry(key) {
            slot.insert(detail);
        } else {
            suppressed += 1;
        }
    }
    *base = by_identity.into_values().collect();
    suppressed
}

fn wordnet_report_json(
    report: &dictc::wordnet::ImportReport,
    glossary_detail_count: usize,
    glossary_relations: RelationCounts,
    wordnet_detail_count: usize,
    wordnet_relations: RelationCounts,
    wordnet_suppressed_by_glossary: usize,
    merged_detail_count: usize,
) -> String {
    let unresolved = report.unresolved;
    format!(
        concat!(
            "{{\n  \"schema_version\": ",
            "{},\n  \"detail_count\": {},\n  \"unresolved\": {{\n",
            "    \"surface_ambiguous\": {},\n    \"sense_ambiguous\": {},\n",
            "    \"missing_definition\": {},\n    \"relation_ambiguous\": {},\n",
            "    \"relation_unsupported\": {},\n    \"relation_truncated\": {}\n  }},\n",
            "  \"details\": {{\n    \"merged_count\": {},\n    \"sources\": {{\n",
            "      \"smile-chat\": {{\"detail_count\": {}, \"aliases\": {}, \"related\": {}}},\n",
            "      \"japanese-wordnet\": {{\"detail_count\": {}, \"similar\": {}, \"antonyms\": {}, \"suppressed_by_smile_chat\": {}}}\n",
            "    }}\n  }}\n}}\n"
        ),
        report.schema_version,
        report.detail_count,
        unresolved.surface_ambiguous,
        unresolved.sense_ambiguous,
        unresolved.missing_definition,
        unresolved.relation_ambiguous,
        unresolved.relation_unsupported,
        unresolved.relation_truncated,
        merged_detail_count,
        glossary_detail_count,
        glossary_relations.aliases,
        glossary_relations.related,
        wordnet_detail_count,
        wordnet_relations.similar,
        wordnet_relations.antonyms,
        wordnet_suppressed_by_glossary,
    )
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

fn read_utf8(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
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
