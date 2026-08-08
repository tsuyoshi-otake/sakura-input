//! Loss-aware editing of the per-user configuration document.

use std::fs;
use std::io;
use std::path::Path;

use sakura_core::{
    default_app_profiles, is_valid_profile_process_name, parse_preferences,
    serialize_preferences_with_profiles, AppProfile, Preferences, CONFIG_FORMAT_VERSION,
};

use crate::storage::atomic_write;

/// The settings values a frontend may edit.
///
/// `source_version` is retained so opening a file from a newer Sakura release
/// cannot accidentally rewrite it in an older format. Callers must explicitly
/// upgrade the settings executable instead of accepting data loss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationDocument {
    pub preferences: Preferences,
    pub profiles: Vec<AppProfile>,
    pub source_version: u16,
}

impl Default for ConfigurationDocument {
    fn default() -> Self {
        let preferences = Preferences::default();
        Self {
            preferences,
            profiles: default_app_profiles(preferences),
            source_version: CONFIG_FORMAT_VERSION,
        }
    }
}

impl ConfigurationDocument {
    pub fn load(path: &Path) -> io::Result<Self> {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let parsed = parse_preferences(&source)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(Self {
            preferences: parsed.preferences,
            profiles: parsed.profiles,
            source_version: parsed.source_version,
        })
    }

    /// Publishes a complete canonical document in one replacement operation.
    pub fn save(&mut self, path: &Path) -> io::Result<()> {
        if self.source_version > CONFIG_FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "configuration format {} is newer than supported format {}; refusing a destructive downgrade",
                    self.source_version, CONFIG_FORMAT_VERSION
                ),
            ));
        }
        validate_profiles(&self.profiles)?;
        let source = serialize_preferences_with_profiles(self.preferences, &self.profiles);
        atomic_write(path, source.as_bytes())?;
        self.source_version = CONFIG_FORMAT_VERSION;
        Ok(())
    }

    /// Inserts or replaces a profile, matching executable names without case.
    pub fn upsert_profile(&mut self, profile: AppProfile) -> io::Result<()> {
        validate_process_name(&profile.process_name)?;
        if let Some(existing) = self
            .profiles
            .iter_mut()
            .find(|existing| existing.matches(&profile.process_name))
        {
            *existing = profile;
        } else {
            self.profiles.push(profile);
        }
        self.profiles.sort_by(|left, right| {
            left.process_name
                .to_ascii_lowercase()
                .cmp(&right.process_name.to_ascii_lowercase())
        });
        Ok(())
    }

    pub fn remove_profile(&mut self, process_name: &str) -> io::Result<AppProfile> {
        validate_process_name(process_name)?;
        let index = self
            .profiles
            .iter()
            .position(|profile| profile.matches(process_name))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("application profile {process_name:?} does not exist"),
                )
            })?;
        Ok(self.profiles.remove(index))
    }
}

fn validate_profiles(profiles: &[AppProfile]) -> io::Result<()> {
    for (index, profile) in profiles.iter().enumerate() {
        validate_process_name(&profile.process_name).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("application profile {}: {error}", index + 1),
            )
        })?;
        if profiles[..index]
            .iter()
            .any(|earlier| earlier.matches(&profile.process_name))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "duplicate application profile for {:?}",
                    profile.process_name
                ),
            ));
        }
    }
    Ok(())
}

fn validate_process_name(process_name: &str) -> io::Result<()> {
    if is_valid_profile_process_name(process_name) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process name must be a plain executable name using ASCII letters, digits, '.', '-', or '_'",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_core::{Preset, SuggestAccept};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

    fn temporary_file(name: &str) -> std::path::PathBuf {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "sakura-settings-config-{}-{name}-{id}",
                std::process::id()
            ))
            .join("config.toml")
    }

    #[test]
    fn current_document_roundtrips_global_values_and_profiles() {
        let path = temporary_file("roundtrip");
        let mut document = ConfigurationDocument::default();
        document.preferences.keymap_preset = Preset::Atok;
        document.preferences.prediction_enabled = false;
        document.preferences.suggest_accept = SuggestAccept::ShiftEnter;
        let mut profile = document.profiles[0].clone();
        profile.process_name = "notes.exe".to_owned();
        profile.prediction_enabled = true;
        document.upsert_profile(profile.clone()).expect("profile");
        document.save(&path).expect("save");

        let loaded = ConfigurationDocument::load(&path).expect("load");
        assert_eq!(loaded.preferences, document.preferences);
        assert!(loaded.profiles.contains(&profile));
        assert_eq!(loaded.source_version, CONFIG_FORMAT_VERSION);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn future_document_is_readable_but_never_rewritten() {
        let path = temporary_file("future");
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        let source = "[meta]\nformat-version = \"99\"\n\n[input]\nkeymap-preset = \"atok\"\nfuture-value = \"retain\"\n";
        fs::write(&path, source).expect("fixture");
        let mut document = ConfigurationDocument::load(&path).expect("load future");
        assert_eq!(document.source_version, 99);
        assert_eq!(
            document.save(&path).expect_err("must refuse").kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(fs::read_to_string(&path).expect("unchanged"), source);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn profile_names_are_unique_without_case() {
        let mut document = ConfigurationDocument::default();
        let profile = document.profiles[0].clone();
        let mut replacement = profile.clone();
        replacement.process_name = profile.process_name.to_ascii_uppercase();
        replacement.prediction_enabled = !profile.prediction_enabled;
        let before = document.profiles.len();
        document
            .upsert_profile(replacement.clone())
            .expect("replace");
        assert_eq!(document.profiles.len(), before);
        assert!(document.profiles.contains(&replacement));
        assert!(document
            .upsert_profile(AppProfile {
                process_name: "..\\escape.exe".to_owned(),
                ..replacement
            })
            .is_err());
    }
}
