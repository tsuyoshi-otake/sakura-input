//! Command-line front end for Sakura's deterministic dictionary compiler.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use dictc::{
    compile, merge_entries, parse_connection, parse_entries, parse_mozc_connection,
    parse_mozc_entries, SourceEntry, MAX_DICTIONARY_IMAGE_BYTES,
};

#[derive(Clone, Copy)]
enum EntryFormat {
    Sakura,
    Mozc,
}

#[derive(Clone, Copy)]
enum EntryLayer {
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
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        match argument.to_str() {
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
            Some("--help" | "-h") => {
                println!(
                    "Usage: dictc (--system FILE | --mozc-system FILE)... \
                     (--connection FILE | --mozc-connection FILE) --output FILE"
                );
                return Ok(());
            }
            Some(other) => return Err(format!("unknown argument '{other}'; use --help")),
            None => return Err("arguments must be valid Unicode paths".to_string()),
        }
    }
    if entry_paths.is_empty() {
        return Err("at least one --system/--overlay/--entries file is required".to_string());
    }
    let connection_path = connection_path.ok_or("--connection is required")?;
    let output_path = output_path.ok_or("--output is required")?;

    let mut system_entries = Vec::<SourceEntry>::new();
    let mut overlay_entries = Vec::<SourceEntry>::new();
    for (path, entry_format, layer) in entry_paths {
        let text = read_utf8(&path)?;
        let parsed = match entry_format {
            EntryFormat::Sakura => parse_entries(&path.display().to_string(), &text),
            EntryFormat::Mozc => parse_mozc_entries(&path.display().to_string(), &text),
        }
        .map_err(|error| error.to_string())?;
        match layer {
            EntryLayer::System => system_entries.extend(parsed),
            EntryLayer::Overlay => overlay_entries.extend(parsed),
        }
    }
    let entries =
        merge_entries(system_entries, overlay_entries).map_err(|error| error.to_string())?;
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
    let image = compile(&entries, &connection).map_err(|error| error.to_string())?;
    if image.len() > MAX_DICTIONARY_IMAGE_BYTES {
        return Err(format!(
            "compiled image is {} bytes, exceeding the {}-byte release gate",
            image.len(),
            MAX_DICTIONARY_IMAGE_BYTES
        ));
    }
    atomic_write(&output_path, &image)?;
    println!(
        "wrote {} entries, {} classes, {} bytes to {}",
        entries.len(),
        connection.class_count(),
        image.len(),
        output_path.display()
    );
    Ok(())
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
