//! Build the fourteen source-neutral Sakura category dictionaries.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use dictc::category::{classify_existing_entry, parse_mozc_pos_catalog, DictionaryCategory};
use dictc::{entries_to_category_tsv, merge_entries, parse_entries, SourceEntry};

enum InputLayer {
    SystemCategory(DictionaryCategory),
    System,
    Overlay,
}

struct Input {
    path: PathBuf,
    layer: InputLayer,
}

struct Config {
    inputs: Vec<Input>,
    mozc_pos: PathBuf,
    output_directory: PathBuf,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EntryKey {
    reading: String,
    surface: String,
    left_id: u16,
    right_id: u16,
    word_cost: i32,
    prediction_cost: i32,
    flags: u16,
    annotation: String,
}

impl From<&SourceEntry> for EntryKey {
    fn from(entry: &SourceEntry) -> Self {
        Self {
            reading: entry.reading.clone(),
            surface: entry.surface.clone(),
            left_id: entry.left_id,
            right_id: entry.right_id,
            word_cost: entry.word_cost,
            prediction_cost: entry.prediction_cost,
            flags: entry.flags.bits(),
            annotation: entry.annotation.clone(),
        }
    }
}

struct CategorizedEntry {
    entry: SourceEntry,
    category: DictionaryCategory,
}

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("category-split: {error}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let started = Instant::now();
    let config = parse_args(args)?;
    let pos_source = config.mozc_pos.display().to_string();
    let pos_catalog = parse_mozc_pos_catalog(&pos_source, &read_utf8(&config.mozc_pos)?)?;

    let mut system_categories = Vec::new();
    let mut system = Vec::new();
    // Keep each overlay as a separate precedence layer.  The final
    // conversion-priority layer is allowed to replace an edge from the IT or
    // curated layer; concatenating all overlays before `merge_entries` would
    // incorrectly report that intentional replacement as a duplicate.
    let mut overlays: Vec<Vec<CategorizedEntry>> = Vec::new();
    for input in config.inputs {
        let source = input.path.display().to_string();
        let entries =
            parse_entries(&source, &read_utf8(&input.path)?).map_err(|error| error.to_string())?;
        match input.layer {
            InputLayer::SystemCategory(category) => system_categories.extend(
                entries
                    .into_iter()
                    .map(|entry| CategorizedEntry { entry, category }),
            ),
            InputLayer::System => {
                system.extend(entries.into_iter().map(|entry| CategorizedEntry {
                    category: classify_existing_entry(&entry, &pos_catalog),
                    entry,
                }))
            }
            InputLayer::Overlay => overlays.push(
                entries
                    .into_iter()
                    .map(|entry| CategorizedEntry {
                        category: classify_existing_entry(&entry, &pos_catalog),
                        entry,
                    })
                    .collect(),
            ),
        }
    }

    // Apply normal dictionary precedence before assigning each final entry to a
    // category.  That prevents a lower-priority copy from surviving in a
    // different category file when an overlay replaces the same lattice edge.
    let mut category_by_entry = BTreeMap::new();
    record_categories(&mut category_by_entry, &system_categories);
    record_categories(&mut category_by_entry, &system);
    let system_category_entries = take_entries(system_categories);
    let system_entries = take_entries(system);
    let mut merged = merge_entries(system_category_entries, system_entries)
        .map_err(|error| error.to_string())?;
    for overlay in overlays {
        record_categories(&mut category_by_entry, &overlay);
        merged = merge_entries(merged, take_entries(overlay)).map_err(|error| error.to_string())?;
    }

    let mut categorized = DictionaryCategory::ALL.map(|_| Vec::new());
    for entry in merged {
        let key = EntryKey::from(&entry);
        let category = category_by_entry.get(&key).copied().ok_or_else(|| {
            format!(
                "internal category lookup failed for '{}' -> '{}'",
                entry.reading, entry.surface
            )
        })?;
        categorized[usize::from(category.id() - 1)].push(entry);
    }

    for (index, category) in DictionaryCategory::ALL.into_iter().enumerate() {
        let path = config.output_directory.join(category.file_name());
        let tsv =
            entries_to_category_tsv(&categorized[index]).map_err(|error| error.to_string())?;
        replacing_write(&path, tsv.as_bytes())?;
        println!(
            "{}: {} entries",
            category.file_name(),
            categorized[index].len()
        );
    }
    println!(
        "wrote {} entries into {} category dictionaries in {:.2}s",
        categorized.iter().map(Vec::len).sum::<usize>(),
        DictionaryCategory::ALL.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn record_categories(
    categories: &mut BTreeMap<EntryKey, DictionaryCategory>,
    entries: &[CategorizedEntry],
) {
    for entry in entries {
        categories.insert(EntryKey::from(&entry.entry), entry.category);
    }
}

fn take_entries(entries: Vec<CategorizedEntry>) -> Vec<SourceEntry> {
    entries.into_iter().map(|entry| entry.entry).collect()
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Config, String> {
    let mut inputs = Vec::new();
    let mut mozc_pos = None;
    let mut output_directory = None;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--system-category" | "--supplement") => {
                let id = next_number::<u8>(&mut args, &argument)?;
                let category = DictionaryCategory::from_id(id)
                    .ok_or_else(|| format!("--system-category id must be 1..=14, found {id}"))?;
                inputs.push(Input {
                    path: next_path(&mut args, &argument)?,
                    layer: InputLayer::SystemCategory(category),
                });
            }
            Some("--system") => inputs.push(Input {
                path: next_path(&mut args, &argument)?,
                layer: InputLayer::System,
            }),
            Some("--overlay") => inputs.push(Input {
                path: next_path(&mut args, &argument)?,
                layer: InputLayer::Overlay,
            }),
            Some("--mozc-pos") => set_once(
                &mut mozc_pos,
                next_path(&mut args, &argument)?,
                "Mozc POS file",
            )?,
            Some("--output-dir") => set_once(
                &mut output_directory,
                next_path(&mut args, &argument)?,
                "output directory",
            )?,
            Some("--help" | "-h") => {
                println!("Usage: category-split --mozc-pos FILE --output-dir DIR");
                println!("       [--system FILE]... [--overlay FILE]... [--system-category CATEGORY_ID FILE]...");
                println!("       (--supplement remains accepted as a compatibility alias)");
                std::process::exit(0);
            }
            Some(other) => return Err(format!("unknown argument '{other}'; use --help")),
            None => return Err("arguments must be valid Unicode".into()),
        }
    }
    if inputs.is_empty() {
        return Err(
            "at least one --system, --overlay, or --system-category input is required".into(),
        );
    }
    Ok(Config {
        inputs,
        mozc_pos: mozc_pos.ok_or("--mozc-pos is required")?,
        output_directory: output_directory.ok_or("--output-dir is required")?,
    })
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

fn set_once<T>(slot: &mut Option<T>, value: T, label: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{label} was specified more than once"));
    }
    *slot = Some(value);
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use dictc::category::DictionaryCategory;

    use super::run;

    // The immediately invoked closure keeps fixture setup and assertions in a
    // single scope so cleanup remains an explicit terminal step below.
    #[allow(clippy::let_unit_value, clippy::redundant_closure_call)]
    #[test]
    fn emits_one_header_only_file_per_category_after_precedence() {
        let root = std::env::temp_dir().join(format!(
            "sakura-category-split-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
        ));
        let result = (|| {
            fs::create_dir_all(&root).expect("create fixture directory");
            let pos = root.join("id.def");
            let system = root.join("system.tsv");
            let overlay = root.join("overlay.tsv");
            let output = root.join("out");
            fs::write(&pos, "100 名詞,一般,*,*\n").expect("write POS fixture");
            fs::write(
                &system,
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nてすと\tテスト\t100\t100\t100\t-\t\t\n",
            )
            .expect("write system fixture");
            fs::write(
                &overlay,
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nてすと\tテスト\t100\t100\t50\t-\tit\t\n",
            )
            .expect("write overlay fixture");
            let args = vec![
                OsString::from("--mozc-pos"),
                pos.into_os_string(),
                OsString::from("--output-dir"),
                output.clone().into_os_string(),
                OsString::from("--system"),
                system.into_os_string(),
                OsString::from("--overlay"),
                overlay.into_os_string(),
            ];
            run(args.into_iter()).expect("category split succeeds");
            let it = fs::read_to_string(output.join(DictionaryCategory::ItEngineering.file_name()))
                .expect("IT category output");
            assert!(it.starts_with("reading\tsurface\t"));
            assert!(!it.contains("# license:"));
            assert!(it.contains("\t50\t-\tit\t"));
            let general =
                fs::read_to_string(output.join(DictionaryCategory::GeneralLexicon.file_name()))
                    .expect("general category output");
            assert_eq!(general, "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n");
        })();
        let _ = fs::remove_dir_all(&root);
        result
    }
}
