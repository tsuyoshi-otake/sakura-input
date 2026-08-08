//! Transactional user-dictionary CRUD, import, and export.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use sakura_core::{UserDictionary, UserDictionaryEntry, UserDictionaryError};

use crate::formats::{
    decode_file_text, detect_format, encode_file_text, parse_dictionary, serialize_dictionary,
    DictionaryFormat, DictionaryFormatError,
};
use crate::storage::atomic_write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    Merge,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    pub total: usize,
    pub format: DictionaryFormat,
}

#[derive(Debug)]
pub enum DictionaryError {
    Io(io::Error),
    Format(DictionaryFormatError),
    Invalid(UserDictionaryError),
}

impl fmt::Display for DictionaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "dictionary file: {error}"),
            Self::Format(error) => write!(formatter, "dictionary format: {error}"),
            Self::Invalid(error) => write!(formatter, "dictionary entry: {error}"),
        }
    }
}

impl std::error::Error for DictionaryError {}

impl From<io::Error> for DictionaryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DictionaryFormatError> for DictionaryError {
    fn from(error: DictionaryFormatError) -> Self {
        Self::Format(error)
    }
}

impl From<UserDictionaryError> for DictionaryError {
    fn from(error: UserDictionaryError) -> Self {
        Self::Invalid(error)
    }
}

pub fn load(path: &Path) -> Result<UserDictionary, DictionaryError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(UserDictionary::from_entries(Vec::new())?)
        }
        Err(error) => return Err(error.into()),
    };
    Ok(UserDictionary::parse_tsv(&source)?)
}

pub fn save(path: &Path, dictionary: &UserDictionary) -> Result<(), DictionaryError> {
    atomic_write(path, dictionary.to_tsv().as_bytes())?;
    Ok(())
}

pub fn add(path: &Path, entry: UserDictionaryEntry) -> Result<UserDictionary, DictionaryError> {
    let dictionary = load(path)?;
    let mut entries = dictionary.entries().to_vec();
    entries.push(entry);
    let updated = UserDictionary::from_entries(entries)?;
    save(path, &updated)?;
    Ok(updated)
}

pub fn update(
    path: &Path,
    original_reading: &str,
    original_surface: &str,
    replacement: UserDictionaryEntry,
) -> Result<UserDictionary, DictionaryError> {
    let dictionary = load(path)?;
    let mut entries = dictionary.entries().to_vec();
    let target = entries
        .iter_mut()
        .find(|entry| entry.reading == original_reading && entry.surface == original_surface)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "dictionary entry {original_reading:?}/{original_surface:?} does not exist"
                ),
            )
        })?;
    *target = replacement;
    let updated = UserDictionary::from_entries(entries)?;
    save(path, &updated)?;
    Ok(updated)
}

pub fn delete(
    path: &Path,
    reading: &str,
    surface: &str,
) -> Result<UserDictionary, DictionaryError> {
    let dictionary = load(path)?;
    let mut entries = dictionary.entries().to_vec();
    let before = entries.len();
    entries.retain(|entry| entry.reading != reading || entry.surface != surface);
    if entries.len() == before {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("dictionary entry {reading:?}/{surface:?} does not exist"),
        )
        .into());
    }
    let updated = UserDictionary::from_entries(entries)?;
    save(path, &updated)?;
    Ok(updated)
}

/// Imports the entire source before touching the destination. In merge mode,
/// an imported reading/surface pair replaces the old pair's POS and comment.
pub fn import(
    destination: &Path,
    bytes: &[u8],
    requested_format: Option<DictionaryFormat>,
    mode: ImportMode,
) -> Result<ImportReport, DictionaryError> {
    let source = decode_file_text(bytes)?;
    let format = requested_format.map_or_else(|| detect_format(&source), Ok)?;
    let imported = parse_dictionary(&source, format)?;
    let imported_count = imported.len();

    let result = match mode {
        ImportMode::Replace => imported,
        ImportMode::Merge => {
            let existing = load(destination)?;
            let mut entries = BTreeMap::<(String, String), UserDictionaryEntry>::new();
            for entry in existing.entries().iter().chain(imported.entries().iter()) {
                entries.insert(
                    (entry.reading.clone(), entry.surface.clone()),
                    entry.clone(),
                );
            }
            UserDictionary::from_entries(entries.into_values().collect())?
        }
    };
    save(destination, &result)?;
    Ok(ImportReport {
        imported: imported_count,
        total: result.len(),
        format,
    })
}

pub fn export(
    source: &Path,
    destination: &Path,
    format: DictionaryFormat,
) -> Result<usize, DictionaryError> {
    let dictionary = load(source)?;
    let text = serialize_dictionary(&dictionary, format);
    let bytes = encode_file_text(&text, format);
    atomic_write(destination, &bytes)?;
    Ok(dictionary.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_core::UserPartOfSpeech;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    fn temporary_directory(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "sakura-settings-userdict-{}-{name}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn entry(reading: &str, surface: &str, comment: &str) -> UserDictionaryEntry {
        UserDictionaryEntry {
            reading: reading.to_owned(),
            surface: surface.to_owned(),
            part_of_speech: UserPartOfSpeech::Noun,
            comment: comment.to_owned(),
        }
    }

    #[test]
    fn crud_is_validated_and_published_as_complete_documents() {
        let directory = temporary_directory("crud");
        let path = directory.join("user.tsv");
        add(&path, entry("さくら", "桜", "first")).expect("add");
        update(&path, "さくら", "桜", entry("さくら", "櫻", "updated")).expect("update");
        assert_eq!(load(&path).expect("load").entries()[0].surface, "櫻");
        delete(&path, "さくら", "櫻").expect("delete");
        assert!(load(&path).expect("empty").is_empty());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn invalid_import_leaves_the_original_byte_for_byte_unchanged() {
        let directory = temporary_directory("atomic-import");
        let path = directory.join("user.tsv");
        add(&path, entry("さくら", "桜", "keep")).expect("fixture");
        let before = fs::read(&path).expect("before");
        let invalid = "# mozc\nさくら\tSakura\t未知\tbad\n";
        assert!(import(
            &path,
            invalid.as_bytes(),
            Some(DictionaryFormat::Mozc),
            ImportMode::Replace,
        )
        .is_err());
        assert_eq!(fs::read(&path).expect("after"), before);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn merge_replaces_matching_metadata_and_external_export_roundtrips() {
        let directory = temporary_directory("roundtrip");
        let path = directory.join("user.tsv");
        add(&path, entry("さくら", "桜", "old")).expect("fixture");
        let source = "# mozc\nさくら\t桜\t名詞\tnew\nつき\t月\t名詞\tadded\n";
        let report = import(
            &path,
            source.as_bytes(),
            Some(DictionaryFormat::Mozc),
            ImportMode::Merge,
        )
        .expect("merge");
        assert_eq!(report.imported, 2);
        assert_eq!(report.total, 2);
        assert_eq!(load(&path).expect("merged").entries()[0].comment, "new");

        let exported = directory.join("atok.txt");
        assert_eq!(
            export(&path, &exported, DictionaryFormat::Atok).expect("export"),
            2
        );
        let decoded = decode_file_text(&fs::read(&exported).expect("bytes")).expect("decode");
        let roundtrip = parse_dictionary(&decoded, DictionaryFormat::Atok).expect("parse");
        assert_eq!(roundtrip.entries(), load(&path).expect("source").entries());
        let _ = fs::remove_dir_all(directory);
    }
}
