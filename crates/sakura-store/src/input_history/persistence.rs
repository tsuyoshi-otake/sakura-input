//! Developer-history file placement, retention, and replacement policy.
//!
//! The engine owns the writer actor and prepares protected frames. This
//! module owns the stable filesystem and size/retention decisions shared by
//! every consumer of the durable format.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};

use super::InputHistoryRecord;

pub const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const MAX_INPUT_HISTORY_BYTES: u64 = 64 * 1024 * 1024;

pub fn retention_cutoff(now_ms: u64) -> u64 {
    now_ms.saturating_sub(RETENTION.as_millis() as u64)
}

/// Applies the time rule and canonical sequence ordering without I/O.
pub fn retained_records(records: Vec<InputHistoryRecord>, now_ms: u64) -> Vec<InputHistoryRecord> {
    let cutoff = retention_cutoff(now_ms);
    let mut retained: Vec<_> = records
        .into_iter()
        .filter(|record| record.timestamp_ms() >= cutoff)
        .collect();
    retained.sort_by_key(InputHistoryRecord::sequence);
    retained
}

/// Uses saturating arithmetic so corrupted or hostile lengths fail closed.
pub const fn exceeds_history_size(current: u64, additional: u64) -> bool {
    current.saturating_add(additional) > MAX_INPUT_HISTORY_BYTES
}

pub fn default_path() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for the developer input history",
        )
    })?;
    Ok(PathBuf::from(local)
        .join("SakuraInput")
        .join("history")
        .join("input.bin"))
}

pub fn compaction_transaction_path(path: &Path) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "history path has no file name")
        })?
        .to_os_string();
    name.push(".compaction");
    Ok(path.with_file_name(name))
}

#[cfg(windows)]
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut encoded: Vec<_> = path.as_os_str().encode_wide().collect();
    if encoded.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history path contains NUL",
        ));
    }
    encoded.push(0);
    Ok(encoded)
}

/// Atomically replaces the canonical file with a same-volume prepared file.
///
/// The caller owns the store lock and retains the transaction directory until
/// the published canonical is synced and validated.
#[cfg(windows)]
pub fn replace_history_file(path: &Path, replacement: &Path, backup: &Path) -> io::Result<()> {
    let canonical = wide(path)?;
    let replacement = wide(replacement)?;
    let previous = wide(backup)?;
    // SAFETY: all paths are live NUL-terminated buffers. The caller owns the
    // cooperative store lock and supplies same-volume transaction paths. ACL
    // errors remain errors; REPLACEFILE_WRITE_THROUGH is not requested.
    unsafe {
        ReplaceFileW(
            PCWSTR(canonical.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR(previous.as_ptr()),
            REPLACE_FILE_FLAGS::default(),
            None,
            None,
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_history::{HistoryScope, KeyHistoryRecord};
    #[cfg(windows)]
    use std::ffi::OsString;
    #[cfg(windows)]
    use std::os::windows::ffi::OsStringExt;

    fn record(sequence: u64, timestamp_ms: u64) -> InputHistoryRecord {
        InputHistoryRecord::Key(KeyHistoryRecord {
            sequence,
            timestamp_ms,
            session: 1,
            scope: HistoryScope::Normal,
            key_code: 1,
            character: None,
            modifiers: 0,
            repeat: false,
            consumed: true,
            state_before: 0,
            state_after: 0,
            mode_before: 0,
            mode_after: 0,
            preedit_before: String::new(),
            preedit_after: String::new(),
            commit: String::new(),
            delete_before: 0,
            beep: false,
            action: String::new(),
            dropped_before: 0,
        })
    }

    #[test]
    fn retention_is_inclusive_and_sequence_sorted() {
        let now = RETENTION.as_millis() as u64 + 100;
        let records = [record(3, 101), record(1, 99), record(2, 100)];
        let retained = retained_records(records.into(), now);
        assert_eq!(
            retained
                .iter()
                .map(InputHistoryRecord::sequence)
                .collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(retention_cutoff(0), 0);
    }

    #[test]
    fn size_cap_is_inclusive_and_overflow_fails_closed() {
        assert!(!exceeds_history_size(MAX_INPUT_HISTORY_BYTES, 0));
        assert!(exceeds_history_size(MAX_INPUT_HISTORY_BYTES, 1));
        assert!(exceeds_history_size(u64::MAX, 1));
    }

    #[test]
    fn compaction_transaction_path_is_adjacent_to_canonical() {
        assert_eq!(
            compaction_transaction_path(Path::new("fixture/input.bin")).unwrap(),
            Path::new("fixture/input.bin.compaction")
        );
        assert!(compaction_transaction_path(Path::new("/")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn replacement_rejects_embedded_nul_before_windows_call() {
        let malformed = PathBuf::from(OsString::from_wide(&[
            'C' as u16, ':' as u16, 0, 'x' as u16,
        ]));
        let error = replace_history_file(&malformed, Path::new("replacement"), Path::new("backup"))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
