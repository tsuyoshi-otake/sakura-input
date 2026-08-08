//! Bounded hot reload for the per-user dictionary.
//!
//! File I/O and parsing happen on one background thread. A conversion only
//! clones the last validated `Arc`, so malformed or partially written updates
//! never replace usable state and never move disk I/O onto the keystroke path.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use sakura_core::{UserDictionary, UserDictionaryError};

use crate::dictionary::ConversionService;

/// The parser already caps entries and fields; this outer cap also prevents a
/// hostile file made mostly of comments or whitespace from growing memory.
pub const MAX_USER_DICTIONARY_FILE_BYTES: u64 = 40 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum ReloadError {
    Io(io::Error),
    TooLarge(u64),
    Parse(UserDictionaryError),
}

impl core::fmt::Display for ReloadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "user dictionary file: {error}"),
            Self::TooLarge(bytes) => write!(
                f,
                "user dictionary is {bytes} bytes (limit {MAX_USER_DICTIONARY_FILE_BYTES})"
            ),
            Self::Parse(error) => write!(f, "user dictionary syntax: {error}"),
        }
    }
}

impl std::error::Error for ReloadError {}

impl From<io::Error> for ReloadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<UserDictionaryError> for ReloadError {
    fn from(error: UserDictionaryError) -> Self {
        Self::Parse(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReloadStats {
    pub successful_reloads: u64,
    pub failed_reloads: u64,
}

#[derive(Debug, Default)]
struct Status {
    successful_reloads: AtomicU64,
    failed_reloads: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl Status {
    fn record(&self, result: &Result<usize, ReloadError>) {
        match result {
            Ok(_) => {
                self.successful_reloads.fetch_add(1, Ordering::Relaxed);
                *self
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            }
            Err(error) => {
                self.failed_reloads.fetch_add(1, Ordering::Relaxed);
                *self
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.to_string());
            }
        }
    }

    fn stats(&self) -> ReloadStats {
        ReloadStats {
            successful_reloads: self.successful_reloads.load(Ordering::Relaxed),
            failed_reloads: self.failed_reloads.load(Ordering::Relaxed),
        }
    }

    fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
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

/// Loads and publishes one complete snapshot. A missing file means an empty
/// dictionary; every other failure leaves the previously published snapshot
/// untouched.
pub fn reload_now(service: &ConversionService, path: &Path) -> Result<usize, ReloadError> {
    let dictionary = read_dictionary(path)?;
    let entries = dictionary.len();
    service.replace_user_dictionary(dictionary);
    Ok(entries)
}

fn read_dictionary(path: &Path) -> Result<UserDictionary, ReloadError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(UserDictionary::default())
        }
        Err(error) => return Err(error.into()),
    };
    let bytes = file.metadata()?.len();
    if bytes > MAX_USER_DICTIONARY_FILE_BYTES {
        return Err(ReloadError::TooLarge(bytes));
    }

    let mut source = String::with_capacity(usize::try_from(bytes).unwrap_or(0));
    file.by_ref()
        .take(MAX_USER_DICTIONARY_FILE_BYTES + 1)
        .read_to_string(&mut source)?;
    let actual = u64::try_from(source.len()).unwrap_or(u64::MAX);
    if actual > MAX_USER_DICTIONARY_FILE_BYTES {
        return Err(ReloadError::TooLarge(actual));
    }
    UserDictionary::parse_tsv(&source).map_err(Into::into)
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

/// Owns exactly one polling thread. [`stop`](Self::stop) and `Drop` both signal
/// that thread and join it, so no watcher can become an accidental terminal or
/// outlive the service lifecycle.
#[derive(Debug)]
pub struct UserDictionaryWatcher {
    path: PathBuf,
    stop: Option<SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
    status: Arc<Status>,
}

impl UserDictionaryWatcher {
    pub fn start(path: PathBuf, service: Arc<ConversionService>) -> io::Result<Self> {
        Self::start_with_interval(path, service, POLL_INTERVAL)
    }

    fn start_with_interval(
        path: PathBuf,
        service: Arc<ConversionService>,
        interval: Duration,
    ) -> io::Result<Self> {
        let status = Arc::new(Status::default());
        let initial = reload_now(&service, &path);
        status.record(&initial);
        let initial_fingerprint = fingerprint(&path).ok();
        let (stop, stopped) = mpsc::sync_channel(1);
        let owned_path = path.clone();
        let owned_status = Arc::clone(&status);
        let thread = thread::Builder::new()
            .name("sakura-user-dictionary".to_owned())
            .spawn(move || {
                watch(
                    &owned_path,
                    &service,
                    &owned_status,
                    stopped,
                    initial_fingerprint,
                    interval,
                )
            })?;
        Ok(Self {
            path,
            stop: Some(stop),
            thread: Some(thread),
            status,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn stats(&self) -> ReloadStats {
        self.status.stats()
    }

    pub fn last_error(&self) -> Option<String> {
        self.status.last_error()
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

impl Drop for UserDictionaryWatcher {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn watch(
    path: &Path,
    service: &ConversionService,
    status: &Status,
    stop: Receiver<()>,
    mut previous: Option<Fingerprint>,
    base_interval: Duration,
) {
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
            Err(error) => {
                status.record(&Err(ReloadError::Io(error)));
                delay = doubled_delay(delay);
                continue;
            }
        };
        if previous == Some(observed) {
            continue;
        }

        let result = reload_now(service, path);
        status.record(&result);
        // Record a malformed generation too: it is retried only after the file
        // changes, rather than hammering the same invalid input every second.
        previous = Some(observed);
    }
}

fn doubled_delay(delay: Duration) -> Duration {
    delay
        .checked_mul(2)
        .unwrap_or(MAX_ERROR_BACKOFF)
        .min(MAX_ERROR_BACKOFF)
}

pub fn default_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("SAKURA_USER_DICTIONARY") {
        return Ok(PathBuf::from(path));
    }
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for the per-user dictionary",
        )
    })?;
    Ok(PathBuf::from(local)
        .join("SakuraInput")
        .join("dictionary")
        .join("user.tsv"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_core::{ConversionOptions, Converter};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    fn image() -> &'static [u8] {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t0\t100\t100\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        )
    }

    fn temporary_dictionary(name: &str) -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("tmp")
            .join(format!(
                "sakura-user-dictionary-{}-{name}-{id}",
                std::process::id()
            ));
        fs::create_dir_all(&root).expect("temporary directory");
        root.join("user.tsv")
    }

    fn source(entries: &str) -> String {
        format!("reading\tsurface\tpos\tcomment\n{entries}")
    }

    #[test]
    fn rejected_reload_preserves_the_last_valid_snapshot() {
        let path = temporary_dictionary("atomic");
        let service = ConversionService::from_static_bytes(image()).expect("service");
        fs::write(
            &path,
            source("さくら\tSakura Input\tproper-noun\tproject\n"),
        )
        .expect("valid source");

        assert_eq!(reload_now(&service, &path).expect("reload"), 1);
        fs::write(&path, "not a valid header\n").expect("invalid replacement");
        assert!(matches!(
            reload_now(&service, &path),
            Err(ReloadError::Parse(_))
        ));
        assert_eq!(service.user_dictionary_snapshot().len(), 1);

        let mut converter = Converter::new();
        let snapshot = service.user_dictionary_snapshot();
        let dictionary = sakura_core::Dictionary::parse(image()).expect("dictionary");
        let candidates = converter
            .convert_with_user_dictionary(
                &dictionary,
                Some(&snapshot),
                "さくら",
                ConversionOptions::default(),
            )
            .expect("conversion");
        assert_eq!(candidates[0].text(), "Sakura Input");
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn watcher_observes_a_new_generation_and_joins_explicitly() {
        let path = temporary_dictionary("watch");
        let service = Arc::new(ConversionService::from_static_bytes(image()).expect("service"));
        fs::write(
            &path,
            source("さくら\tSakura Input\tproper-noun\tproject\n"),
        )
        .expect("initial source");
        let watcher = UserDictionaryWatcher::start_with_interval(
            path.clone(),
            Arc::clone(&service),
            Duration::from_millis(10),
        )
        .expect("watcher");
        assert_eq!(service.user_dictionary_snapshot().len(), 1);

        fs::write(
            &path,
            source(
                "さくら\tSakura Input\tproper-noun\tproject\nこでっくす\tCodex\tproper-noun\ttool\n",
            ),
        )
        .expect("second generation");
        for _ in 0..200 {
            if service.user_dictionary_snapshot().len() == 2 {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(service.user_dictionary_snapshot().len(), 2);
        assert!(watcher.stats().successful_reloads >= 2);
        assert_eq!(watcher.last_error(), None);
        watcher.stop().expect("watcher thread joined");
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}
