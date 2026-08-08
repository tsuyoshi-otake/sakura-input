//! Per-user configuration loading and loss-aware format upgrades.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sakura_core::{
    default_app_profiles, parse_preferences, serialize_preferences_with_profiles, AppProfile,
    ParseError, Preferences, CONFIG_FORMAT_VERSION,
};

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Parse(ParseError),
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "configuration file: {error}"),
            Self::Parse(error) => write!(f, "configuration syntax: {error}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<io::Error> for LoadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ParseError> for LoadError {
    fn from(error: ParseError) -> Self {
        Self::Parse(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfiguration {
    pub preferences: Preferences,
    pub profiles: Vec<AppProfile>,
    pub source_version: u16,
    /// Original previous-version file retained during an automatic upgrade.
    pub backup: Option<PathBuf>,
}

pub fn load(path: &Path) -> Result<LoadedConfiguration, LoadError> {
    if !path.exists() {
        return Ok(LoadedConfiguration {
            preferences: Preferences::default(),
            profiles: default_app_profiles(Preferences::default()),
            source_version: CONFIG_FORMAT_VERSION,
            backup: None,
        });
    }

    let source = fs::read_to_string(path)?;
    let parsed = parse_preferences(&source)?;
    let backup = if parsed.source_version < CONFIG_FORMAT_VERSION {
        Some(upgrade(
            path,
            parsed.preferences,
            &parsed.profiles,
            parsed.source_version,
        )?)
    } else {
        None
    };
    Ok(LoadedConfiguration {
        preferences: parsed.preferences,
        profiles: parsed.profiles,
        source_version: parsed.source_version,
        backup,
    })
}

pub fn default_path() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for the per-user configuration",
        )
    })?;
    Ok(PathBuf::from(local)
        .join("SakuraInput")
        .join("config")
        .join("config.toml"))
}

fn upgrade(
    path: &Path,
    preferences: Preferences,
    profiles: &[AppProfile],
    source_version: u16,
) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = unique_sibling(path, "upgrade.tmp");
    let backup = unique_sibling(path, &format!("v{source_version}.bak"));

    let write_result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(serialize_preferences_with_profiles(preferences, profiles).as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    fs::rename(path, &backup)?;
    if let Err(error) = fs::rename(&temporary, path) {
        // Best-effort rollback keeps the original active if publishing the
        // canonical replacement fails after the backup rename.
        let _ = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(backup)
}

fn unique_sibling(path: &Path, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let first = parent.join(format!("{file_name}.{suffix}"));
    if !first.exists() {
        return first;
    }
    for sequence in 1..=u16::MAX {
        let candidate = parent.join(format!("{file_name}.{suffix}.{sequence}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{file_name}.{suffix}.overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    fn temporary_config(name: &str) -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("sakura-config-{}-{name}-{id}", std::process::id()));
        fs::create_dir_all(&directory).expect("temporary directory");
        directory.join("config.toml")
    }

    #[test]
    fn missing_file_uses_current_defaults_without_creating_state() {
        let path = temporary_config("missing");
        let loaded = load(&path).expect("defaults");
        assert_eq!(loaded.preferences, Preferences::default());
        assert_eq!(
            loaded.profiles,
            default_app_profiles(Preferences::default())
        );
        assert_eq!(loaded.source_version, CONFIG_FORMAT_VERSION);
        assert_eq!(loaded.backup, None);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn previous_file_is_upgraded_and_the_exact_original_is_retained() {
        let path = temporary_config("upgrade");
        let previous = "[settings]\nformat-version = \"1\"\nkeymap = \"atok\"\nprediction = \"false\"\nunknown = \"keep in backup\"\n";
        fs::write(&path, previous).expect("previous config");

        let loaded = load(&path).expect("upgrade");

        assert_eq!(loaded.preferences.keymap_preset, sakura_core::Preset::Atok);
        assert!(!loaded.preferences.prediction_enabled);
        let backup = loaded.backup.expect("backup path");
        assert_eq!(fs::read_to_string(&backup).expect("backup"), previous);
        let current = fs::read_to_string(&path).expect("current");
        assert!(current.contains(&format!("format-version = \"{CONFIG_FORMAT_VERSION}\"")));
        assert_eq!(
            parse_preferences(&current)
                .expect("current parse")
                .preferences,
            loaded.preferences
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn future_version_loads_known_fields_without_destructive_downgrade() {
        let path = temporary_config("future");
        let future = "[meta]\nformat-version = \"99\"\n[input]\nkeymap-preset = \"atok\"\nfuture = \"value\"\n";
        fs::write(&path, future).expect("future config");

        let loaded = load(&path).expect("forward compatible");

        assert_eq!(loaded.source_version, 99);
        assert_eq!(loaded.preferences.keymap_preset, sakura_core::Preset::Atok);
        assert_eq!(loaded.backup, None);
        assert_eq!(fs::read_to_string(&path).expect("unchanged"), future);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}
