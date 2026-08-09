//! Bounded, content-free lifecycle diagnostics for the engine process.
//!
//! This log deliberately has no API that accepts user text. Callers choose a
//! typed event and numeric metadata; rendering happens here from fixed tokens.
//! That keeps composition text, candidate surfaces, process names, and paths out
//! of diagnostics by construction (DESIGN 9).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Each retained engine log is at most five MiB.
pub const MAX_ENGINE_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// The current file and one previous generation are retained.
pub const ENGINE_LOG_GENERATIONS: usize = 2;

/// WER is configured to the same ceiling. The startup prune is a second line
/// of defense for files left by an interrupted policy transition.
pub const MAX_LOCAL_DUMPS: usize = 5;

/// Concrete width-scan strategy selected during process bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthScanStrategy {
    Scalar,
    AvxSsse3Xmm,
    Avx2Hybrid,
    Avx512BwVlFrom64,
    Avx512BwVlFrom128,
    Avx512BwVlFrom256,
}

impl WidthScanStrategy {
    pub fn from_name(name: &str) -> Self {
        match name {
            "avx" | "avx-ssse3-128" => Self::AvxSsse3Xmm,
            "avx2" | "avx2-hybrid" => Self::Avx2Hybrid,
            "avx512bw-vl-from-64" => Self::Avx512BwVlFrom64,
            "avx512bw-vl-from-128" => Self::Avx512BwVlFrom128,
            "avx512bw-vl-from-256" => Self::Avx512BwVlFrom256,
            _ => Self::Scalar,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::AvxSsse3Xmm => "avx-ssse3-128",
            Self::Avx2Hybrid => "avx2-hybrid",
            Self::Avx512BwVlFrom64 => "avx512bw-vl-from-64",
            Self::Avx512BwVlFrom128 => "avx512bw-vl-from-128",
            Self::Avx512BwVlFrom256 => "avx512bw-vl-from-256",
        }
    }
}

/// Terminal and lifecycle events safe to persist without user content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineEvent {
    Startup { width_scan: WidthScanStrategy },
    Ready { elapsed_ms: u64 },
    Stopped,
    AlreadyRunning,
    UnsupportedCpu,
    StartupFailed { hresult: i32 },
    DumpsPruned { removed: u64, failures: u64 },
    DumpPruneFailed { os_error: i32 },
}

/// A bounded two-generation event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn open_default() -> io::Result<Self> {
        let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "LOCALAPPDATA is unavailable for engine diagnostics",
            )
        })?;
        Ok(Self::at(
            PathBuf::from(local)
                .join("SakuraInput")
                .join("logs")
                .join("engine.log"),
        ))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one typed record, rotating before either file can exceed its cap.
    pub fn record(&self, event: EngineEvent) -> io::Result<()> {
        self.record_at(unix_timestamp_ms(), event)
    }

    fn record_at(&self, timestamp_ms: u64, event: EngineEvent) -> io::Result<()> {
        let record = render_record(timestamp_ms, event);
        debug_assert!(record.len() as u64 <= MAX_ENGINE_LOG_BYTES);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let current_bytes = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if current_bytes > MAX_ENGINE_LOG_BYTES {
            // An externally enlarged file is not copied into the retained
            // generation: doing so would preserve the very disk-bound
            // violation this component exists to repair.
            truncate(&self.path)?;
        } else if current_bytes.saturating_add(record.len() as u64) > MAX_ENGINE_LOG_BYTES {
            self.rotate()?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(record.as_bytes())?;
        file.flush()
    }

    fn rotate(&self) -> io::Result<()> {
        let previous = previous_path(&self.path);
        match fs::remove_file(&previous) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match fs::rename(&self.path, &previous) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => {
                // Antivirus or an inspector may hold a non-delete share on the
                // current file. Truncation loses the previous generation but
                // still enforces the hard disk ceiling, which is the safety
                // property. Surface a truncation failure to the caller.
                truncate(&self.path)
            }
        }
    }
}

fn render_record(timestamp_ms: u64, event: EngineEvent) -> String {
    match event {
        EngineEvent::Startup { width_scan } => format!(
            "unix_ms={timestamp_ms}\tevent=startup\twidth_scan={}\n",
            width_scan.name()
        ),
        EngineEvent::Ready { elapsed_ms } => {
            format!("unix_ms={timestamp_ms}\tevent=ready\telapsed_ms={elapsed_ms}\n")
        }
        EngineEvent::Stopped => format!("unix_ms={timestamp_ms}\tevent=stopped\n"),
        EngineEvent::AlreadyRunning => {
            format!("unix_ms={timestamp_ms}\tevent=already_running\n")
        }
        EngineEvent::UnsupportedCpu => {
            format!("unix_ms={timestamp_ms}\tevent=unsupported_cpu\n")
        }
        EngineEvent::StartupFailed { hresult } => format!(
            "unix_ms={timestamp_ms}\tevent=startup_failed\thresult=0x{:08X}\n",
            hresult as u32
        ),
        EngineEvent::DumpsPruned { removed, failures } => format!(
            "unix_ms={timestamp_ms}\tevent=dumps_pruned\tremoved={removed}\tfailures={failures}\n"
        ),
        EngineEvent::DumpPruneFailed { os_error } => {
            format!("unix_ms={timestamp_ms}\tevent=dump_prune_failed\tos_error={os_error}\n")
        }
    }
}

/// Observable terminal counts for one non-recursive dump-retention pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DumpPruneReport {
    pub matched: u64,
    pub removed: u64,
    pub retained: u64,
    pub failures: u64,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DumpFile {
    modified: SystemTime,
    path: PathBuf,
}

/// Prunes the default WER directory to [`MAX_LOCAL_DUMPS`] files.
pub fn prune_default_dumps() -> io::Result<DumpPruneReport> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for dump retention",
        )
    })?;
    prune_dump_directory(
        &PathBuf::from(local).join("SakuraInput").join("dumps"),
        MAX_LOCAL_DUMPS,
    )
}

/// Keeps only the newest `max_files` ordinary `.dmp` files in one directory.
///
/// The heap never holds more than `max_files + 1` paths, so this is
/// O(N log K) time and O(K) memory. K is the fixed WER retention limit (five),
/// making startup work linear even after an abnormal crash storm. Directories,
/// reparse points, and unrelated files are never followed or removed.
pub fn prune_dump_directory(directory: &Path, max_files: usize) -> io::Result<DumpPruneReport> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(DumpPruneReport {
                matched: 0,
                removed: 0,
                retained: 0,
                failures: 0,
            });
        }
        Err(error) => return Err(error),
    };

    let mut newest = BinaryHeap::<Reverse<DumpFile>>::new();
    let mut report = DumpPruneReport {
        matched: 0,
        removed: 0,
        retained: 0,
        failures: 0,
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                report.failures += 1;
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                report.failures += 1;
                continue;
            }
        };
        if !file_type.is_file()
            || !entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dmp"))
        {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                report.failures += 1;
                continue;
            }
        };
        report.matched += 1;
        newest.push(Reverse(DumpFile {
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            path: entry.path(),
        }));
        if newest.len() > max_files {
            let Reverse(oldest) = newest.pop().expect("heap exceeded its nonzero length");
            match fs::remove_file(oldest.path) {
                Ok(()) => report.removed += 1,
                Err(_) => report.failures += 1,
            }
        }
    }
    report.retained = newest.len() as u64;
    Ok(report)
}

fn previous_path(current: &Path) -> PathBuf {
    let mut previous = current.as_os_str().to_owned();
    previous.push(".1");
    PathBuf::from(previous)
}

fn truncate(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map(|_| ())
}

fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    fn temporary_log(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "sakura-engine-log-{}-{name}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ))
            .join("engine.log")
    }

    #[test]
    fn width_scan_log_names_match_the_core_dispatch_vocabulary() {
        assert_eq!(
            WidthScanStrategy::from_name("avx-ssse3-128").name(),
            "avx-ssse3-128"
        );
        assert_eq!(
            WidthScanStrategy::from_name("avx2-hybrid").name(),
            "avx2-hybrid"
        );
        assert_eq!(
            WidthScanStrategy::from_name("avx512bw-vl-from-64").name(),
            "avx512bw-vl-from-64"
        );
        assert_eq!(
            WidthScanStrategy::from_name("avx512bw-vl-from-128").name(),
            "avx512bw-vl-from-128"
        );
        assert_eq!(
            WidthScanStrategy::from_name("avx512bw-vl-from-256").name(),
            "avx512bw-vl-from-256"
        );
        assert_eq!(WidthScanStrategy::from_name("unknown").name(), "scalar");
    }

    #[test]
    fn records_are_fixed_vocabulary_and_numeric_metadata_only() {
        let path = temporary_log("content");
        let log = EventLog::at(path.clone());
        log.record_at(
            123,
            EngineEvent::Startup {
                width_scan: WidthScanStrategy::Avx2Hybrid,
            },
        )
        .expect("startup");
        log.record_at(124, EngineEvent::Ready { elapsed_ms: 17 })
            .expect("ready");
        log.record_at(
            125,
            EngineEvent::StartupFailed {
                hresult: 0x8000_4005u32 as i32,
            },
        )
        .expect("failure");

        assert_eq!(
            fs::read_to_string(&path).expect("log"),
            "unix_ms=123\tevent=startup\twidth_scan=avx2-hybrid\n\
             unix_ms=124\tevent=ready\telapsed_ms=17\n\
             unix_ms=125\tevent=startup_failed\thresult=0x80004005\n"
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn rotation_keeps_exactly_two_files_within_the_five_mib_cap() {
        let path = temporary_log("rotation");
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        fs::write(&path, vec![b'x'; MAX_ENGINE_LOG_BYTES as usize]).expect("full current");
        let log = EventLog::at(path.clone());
        log.record_at(1, EngineEvent::Stopped).expect("rotate once");

        let previous = previous_path(&path);
        assert_eq!(
            fs::metadata(&previous).expect("previous").len(),
            MAX_ENGINE_LOG_BYTES
        );
        assert!(fs::metadata(&path).expect("current").len() < MAX_ENGINE_LOG_BYTES);

        fs::write(&path, vec![b'y'; MAX_ENGINE_LOG_BYTES as usize]).expect("refill current");
        log.record_at(2, EngineEvent::AlreadyRunning)
            .expect("rotate twice");
        assert_eq!(
            fs::metadata(&previous).expect("replaced previous").len(),
            MAX_ENGINE_LOG_BYTES
        );
        assert!(fs::metadata(&path).expect("new current").len() <= MAX_ENGINE_LOG_BYTES);
        assert_eq!(ENGINE_LOG_GENERATIONS, 2);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn an_externally_oversized_file_is_repaired_without_preserving_it() {
        let path = temporary_log("oversized");
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        fs::write(&path, vec![b'z'; MAX_ENGINE_LOG_BYTES as usize + 1]).expect("oversized current");
        let log = EventLog::at(path.clone());
        log.record_at(3, EngineEvent::Stopped).expect("repair");

        assert!(fs::metadata(&path).expect("current").len() <= MAX_ENGINE_LOG_BYTES);
        assert!(!previous_path(&path).exists());
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn dump_retention_is_non_recursive_bounded_and_observable() {
        let path = temporary_log("dumps");
        let directory = path.parent().expect("parent");
        fs::create_dir_all(directory.join("nested")).expect("directories");
        for index in 0..8 {
            fs::write(directory.join(format!("engine.{index}.dmp")), [index])
                .expect("dump fixture");
        }
        fs::write(directory.join("keep.txt"), b"not a dump").expect("unrelated fixture");
        fs::write(directory.join("nested").join("nested.dmp"), b"nested").expect("nested fixture");

        let report = prune_dump_directory(directory, MAX_LOCAL_DUMPS).expect("prune");
        assert_eq!(
            report,
            DumpPruneReport {
                matched: 8,
                removed: 3,
                retained: 5,
                failures: 0,
            }
        );
        let remaining_dumps = fs::read_dir(directory)
            .expect("directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("dmp"))
            })
            .count();
        assert_eq!(remaining_dumps, MAX_LOCAL_DUMPS);
        assert!(directory.join("keep.txt").exists());
        assert!(directory.join("nested").join("nested.dmp").exists());
        let _ = fs::remove_dir_all(directory);
    }
}
