//! Developer-mode branch trace for Dual TSF, candidate UI, and conversion.
//!
//! Records are fixed tokens plus integers. There is no API that accepts user
//! text, process names, or candidate surfaces. Emission is a no-op until
//! explicitly enabled, and a full file stops appending rather than growing.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Threading::GetCurrentThreadId;

/// Two MiB ceiling. A full file is fail-closed: later events are dropped.
pub const MAX_DEBUG_LOG_BYTES: u64 = 2 * 1024 * 1024;

const HEADER: &str = "unix_ms\tpid\ttid\tcomp\tinst\tevent\tdecision\tk0\tk1\tk2\tk3\n";

static ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy)]
pub struct TraceEvent {
    pub component: &'static str,
    pub instance: u64,
    pub event: &'static str,
    pub decision: &'static str,
    pub k0: u64,
    pub k1: u64,
    pub k2: u64,
    pub k3: u64,
}

pub fn default_path() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for debug trace",
        )
    })?;
    Ok(PathBuf::from(local)
        .join("SakuraInput")
        .join("logs")
        .join("debug.tsv"))
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Opt-in from `SAKURA_DEBUG_TRACE=1` / `on`. Does not disable an already
/// enabled developer-mode trace.
pub fn enable_from_environment() {
    let enabled = std::env::var_os("SAKURA_DEBUG_TRACE")
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("on"));
    if enabled {
        set_enabled(true);
    }
}

/// Best-effort append. Callers on the key path ignore the result.
pub fn emit(record: TraceEvent) {
    if !is_enabled() {
        return;
    }
    let Ok(path) = default_path() else {
        return;
    };
    let _ = emit_at(&path, record);
}

pub fn emit_at(path: &Path, record: TraceEvent) -> io::Result<()> {
    debug_assert!(record.component.bytes().all(|b| b.is_ascii_graphic()));
    debug_assert!(record.event.bytes().all(|b| b.is_ascii_graphic()));
    debug_assert!(record
        .decision
        .bytes()
        .all(|b| b.is_ascii_graphic() || b == b'_'));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        });
    // SAFETY: GetCurrentThreadId has no preconditions.
    let tid = unsafe { GetCurrentThreadId() };
    let line = format!(
        "{timestamp}\t{}\t{tid}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        std::process::id(),
        record.component,
        record.instance,
        record.event,
        record.decision,
        record.k0,
        record.k1,
        record.k2,
        record.k3,
    );

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let current = file.metadata()?.len();
    if current == 0 {
        file.write_all(HEADER.as_bytes())?;
    }
    let next = current
        .saturating_add(if current == 0 { HEADER.len() as u64 } else { 0 })
        .saturating_add(line.len() as u64);
    if next > MAX_DEBUG_LOG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "debug trace reached its hard byte ceiling",
        ));
    }
    match file.write(line.as_bytes())? {
        written if written == line.len() => Ok(()),
        written => Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!("debug trace wrote {written}/{} bytes", line.len()),
        )),
    }
}

pub fn read_text(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

pub fn clear(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "sakura-debug-trace-{}-{}.tsv",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn emit_at_writes_header_and_one_content_free_row() {
        let path = path();
        let _ = fs::remove_file(&path);
        emit_at(
            &path,
            TraceEvent {
                component: "tsf",
                instance: 7,
                event: "candidate_hide",
                decision: "keep",
                k0: 0,
                k1: 1,
                k2: 0,
                k3: 0,
            },
        )
        .expect("write");
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.starts_with(HEADER));
        assert!(text.contains("\ttsf\t7\tcandidate_hide\tkeep\t0\t1\t0\t0\n"));
        assert!(!text.contains("にほんご"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn disabled_emit_is_a_noop() {
        set_enabled(false);
        emit(TraceEvent {
            component: "tsf",
            instance: 1,
            event: "conversion_key",
            decision: "absorb_peer",
            k0: 0,
            k1: 1,
            k2: 2,
            k3: 0,
        });
    }
}
