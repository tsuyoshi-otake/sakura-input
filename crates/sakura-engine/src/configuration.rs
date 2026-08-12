//! Per-user configuration loading and loss-aware format upgrades.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use sakura_core::{
    default_app_profiles, parse_preferences, serialize_preferences_with_profiles, AppProfile,
    AppearanceTheme, ParseError, Preferences, CONFIG_FORMAT_VERSION,
};

/// The appearance preference is independent of per-session input state, so it
/// can be safely reloaded without rebuilding pipe workers or their dispatchers.
const APPEARANCE_POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_APPEARANCE_POLL_INTERVAL: Duration = Duration::from_secs(30);

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

/// Loads a configuration snapshot for a live engine without performing the
/// one-time format upgrade used during startup. A background watcher must not
/// rewrite a file merely because it observed a new fingerprint; it either
/// publishes this complete, valid snapshot or keeps the last one active.
pub fn load_runtime(path: &Path) -> Result<LoadedConfiguration, LoadError> {
    if !path.exists() {
        let preferences = Preferences::default();
        return Ok(LoadedConfiguration {
            preferences,
            profiles: default_app_profiles(preferences),
            source_version: CONFIG_FORMAT_VERSION,
            backup: None,
        });
    }

    let source = fs::read_to_string(path)?;
    let parsed = parse_preferences(&source)?;
    Ok(LoadedConfiguration {
        preferences: parsed.preferences,
        profiles: parsed.profiles,
        source_version: parsed.source_version,
        backup: None,
    })
}

/// Loads only the appearance field without applying a format upgrade. A
/// background watcher must never rewrite a user file merely because it
/// noticed a change; a malformed or inaccessible file is reported to its
/// caller so the last published theme can remain active.
pub fn load_appearance(path: &Path) -> Result<AppearanceTheme, LoadError> {
    if !path.exists() {
        return Ok(AppearanceTheme::Auto);
    }
    let source = fs::read_to_string(path)?;
    Ok(parse_preferences(&source)?.preferences.appearance_theme)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fingerprint {
    Missing,
    Present {
        bytes: u64,
        modified: Option<SystemTime>,
        created: Option<SystemTime>,
    },
}

fn fingerprint(path: &Path) -> io::Result<Fingerprint> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Fingerprint::Present {
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Fingerprint::Missing),
        Err(error) => Err(error),
    }
}

/// Owns the bounded background reload path for the global renderer appearance.
/// A successful load calls `publish`; failed loads deliberately leave the last
/// valid UI state untouched. Dropping the watcher always signals and joins its
/// thread, so it cannot survive the engine that owns the published state.
#[derive(Debug)]
pub struct AppearanceWatcher {
    stop: Option<SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl AppearanceWatcher {
    pub fn start<F>(path: PathBuf, publish: F) -> io::Result<Self>
    where
        F: Fn(AppearanceTheme) + Send + 'static,
    {
        Self::start_with_interval(path, publish, APPEARANCE_POLL_INTERVAL)
    }

    fn start_with_interval<F>(path: PathBuf, publish: F, interval: Duration) -> io::Result<Self>
    where
        F: Fn(AppearanceTheme) + Send + 'static,
    {
        let initial_fingerprint = fingerprint(&path).ok();
        let (stop, stopped) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("sakura-appearance".to_owned())
            .spawn(move || {
                watch_appearance(&path, &publish, stopped, initial_fingerprint, interval)
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) -> thread::Result<()> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> thread::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.try_send(());
        }
        match self.thread.take() {
            Some(thread) => thread.join(),
            None => Ok(()),
        }
    }
}

impl Drop for AppearanceWatcher {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

/// Watches the complete user configuration for live input-setting changes.
///
/// The callback receives an immutable, fully parsed snapshot. A malformed or
/// inaccessible replacement never reaches the engine, and dropping the
/// watcher always signals and joins its thread before the owner can terminate.
#[derive(Debug)]
pub struct ConfigurationWatcher {
    stop: Option<SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl ConfigurationWatcher {
    pub fn start<F>(path: PathBuf, publish: F) -> io::Result<Self>
    where
        F: Fn(Preferences, Vec<AppProfile>) + Send + 'static,
    {
        Self::start_with_interval(path, publish, APPEARANCE_POLL_INTERVAL)
    }

    fn start_with_interval<F>(path: PathBuf, publish: F, interval: Duration) -> io::Result<Self>
    where
        F: Fn(Preferences, Vec<AppProfile>) + Send + 'static,
    {
        let initial_fingerprint = fingerprint(&path).ok();
        let (stop, stopped) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("sakura-configuration".to_owned())
            .spawn(move || {
                watch_configuration(&path, &publish, stopped, initial_fingerprint, interval)
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) -> thread::Result<()> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> thread::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.try_send(());
        }
        match self.thread.take() {
            Some(thread) => thread.join(),
            None => Ok(()),
        }
    }
}

impl Drop for ConfigurationWatcher {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn watch_appearance<F>(
    path: &Path,
    publish: &F,
    stop: Receiver<()>,
    mut previous: Option<Fingerprint>,
    base_interval: Duration,
) where
    F: Fn(AppearanceTheme),
{
    let mut delay = base_interval;
    loop {
        match stop.recv_timeout(delay) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let observed = match fingerprint(path) {
            Ok(observed) => {
                delay = base_interval;
                observed
            }
            Err(_) => {
                delay = doubled_appearance_delay(delay);
                continue;
            }
        };
        if previous == Some(observed) {
            continue;
        }

        // A failed read/parse is a terminal observation of this file version:
        // wait for a different fingerprint instead of repeatedly reading an
        // invalid file. The callback is invoked only for a complete, valid
        // configuration, making the previously published theme atomic.
        if let Ok(theme) = load_appearance(path) {
            publish(theme);
        }
        previous = Some(observed);
    }
}

fn watch_configuration<F>(
    path: &Path,
    publish: &F,
    stop: Receiver<()>,
    mut previous: Option<Fingerprint>,
    base_interval: Duration,
) where
    F: Fn(Preferences, Vec<AppProfile>),
{
    let mut delay = base_interval;
    loop {
        match stop.recv_timeout(delay) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let observed = match fingerprint(path) {
            Ok(observed) => {
                delay = base_interval;
                observed
            }
            Err(_) => {
                delay = doubled_appearance_delay(delay);
                continue;
            }
        };
        if previous == Some(observed) {
            continue;
        }

        if let Ok(loaded) = load_runtime(path) {
            publish(loaded.preferences, loaded.profiles);
        }
        // Treat a malformed version as the terminal observation for this
        // fingerprint. A later atomic save gets a new fingerprint and is
        // then parsed and published normally.
        previous = Some(observed);
    }
}

fn doubled_appearance_delay(delay: Duration) -> Duration {
    delay
        .checked_mul(2)
        .unwrap_or(MAX_APPEARANCE_POLL_INTERVAL)
        .min(MAX_APPEARANCE_POLL_INTERVAL)
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
    use std::sync::mpsc;
    use std::time::Duration;

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

    #[test]
    fn appearance_watcher_publishes_real_file_changes_and_keeps_the_last_valid_theme() {
        let path = temporary_config("appearance-watcher");
        let (published, received) = mpsc::channel();
        let watcher = AppearanceWatcher::start_with_interval(
            path.clone(),
            move |theme| published.send(theme).expect("test receiver"),
            Duration::from_millis(5),
        )
        .expect("watcher");

        fs::write(
            &path,
            "[meta]\nformat-version = \"4\"\n\n[appearance]\ntheme = \"dark\"\n",
        )
        .expect("dark configuration");
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("dark publication"),
            AppearanceTheme::Dark
        );

        // A transient, malformed save must not reset the already-published
        // dark theme to Auto or wake the renderer with a guessed palette.
        fs::write(&path, "[appearance]\ntheme = \"dark\"\ntheme = \"light\"\n")
            .expect("malformed configuration");
        assert!(received.recv_timeout(Duration::from_millis(80)).is_err());

        fs::write(
            &path,
            "[meta]\nformat-version = \"4\"\n\n[appearance]\ntheme = \"light\"\n",
        )
        .expect("light configuration");
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("light publication"),
            AppearanceTheme::Light
        );

        fs::write(
            &path,
            "[meta]\nformat-version = \"4\"\n\n[appearance]\ntheme = \"auto\"\n",
        )
        .expect("auto configuration");
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("auto publication"),
            AppearanceTheme::Auto
        );

        watcher.stop().expect("watcher stops and joins");
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn configuration_watcher_publishes_complete_valid_snapshots_only() {
        let path = temporary_config("configuration-watcher");
        let (published, received) = mpsc::channel();
        let watcher = ConfigurationWatcher::start_with_interval(
            path.clone(),
            move |preferences, profiles| {
                published
                    .send((preferences, profiles))
                    .expect("test receiver");
            },
            Duration::from_millis(5),
        )
        .expect("watcher");

        fs::write(
            &path,
            "[meta]\nformat-version = \"4\"\n\n[input]\nkeymap-preset = \"atok\"\nprediction-enabled = \"false\"\nassociation-enabled = \"false\"\n",
        )
        .expect("initial configuration");
        let (preferences, profiles) = received
            .recv_timeout(Duration::from_secs(1))
            .expect("initial publication");
        assert_eq!(preferences.keymap_preset, sakura_core::Preset::Atok);
        assert!(!preferences.prediction_enabled);
        assert!(!preferences.association_enabled);
        assert_eq!(profiles, default_app_profiles(preferences));

        // A transient malformed save must never replace a valid snapshot.
        fs::write(
            &path,
            "[input]\nkeymap-preset = \"ms-ime\"\nkeymap-preset = \"atok\"\n",
        )
        .expect("malformed configuration");
        assert!(received.recv_timeout(Duration::from_millis(80)).is_err());

        fs::write(
            &path,
            "[meta]\nformat-version = \"4\"\n\n[input]\nkeymap-preset = \"ms-ime\"\nprediction-enabled = \"true\"\nassociation-enabled = \"true\"\n\n[width]\nalnum = \"full\"\n\n[profile.code.exe]\ndefault-mode = \"hiragana\"\nprediction-enabled = \"true\"\nsuggest-accept = \"tab\"\n",
        )
        .expect("updated configuration");
        let (preferences, profiles) = received
            .recv_timeout(Duration::from_secs(1))
            .expect("updated publication");
        assert_eq!(preferences.keymap_preset, sakura_core::Preset::MsIme);
        assert!(preferences.prediction_enabled);
        assert!(preferences.association_enabled);
        assert_eq!(preferences.normalizer.width.alnum, sakura_core::Width::Full);
        assert!(profiles.iter().any(|profile| profile.matches("code.exe")));

        watcher.stop().expect("watcher stops and joins");
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}
