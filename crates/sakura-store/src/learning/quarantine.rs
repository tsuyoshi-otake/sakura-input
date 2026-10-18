//! `*.corrupt-*` quarantine copies of a learning log whose middle could not
//! be read (#360 A3).
//!
//! When `open` finds mid-file corruption it copies everything it read aside
//! as `{stem}.corrupt-{YYYYMMDD}T{HHMMSS}Z` (UTC) before truncating back to
//! the valid prefix, so a single damaged byte can no longer delete every
//! later record without leaving a copy. This module owns the copy name, the
//! UTC stamp, the newest-three retention, and the removal `clear` performs.
//! A `corrupt-*` name is deliberately not a publish leftover, so the startup
//! recovery in `log.rs` leaves it alone.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::log::{unique_sibling, write_new_file};

/// How many `corrupt-*` siblings are kept after each quarantine. The copy
/// the call just wrote is always among them — its stamp comes from the
/// current clock, which may sort before older copies written while the
/// clock was ahead, so retention removes only the oldest of the *other*
/// matching names. Older-copy removal is best effort: a failure there
/// never fails `open`.
const RETAINED_COPIES: usize = 3;

/// The UTC stamp in a copy name: 8 digits, `T`, 6 digits, `Z`.
const STAMP_LEN: usize = "00000000T000000Z".len();

/// The `{stem}.corrupt-<stamp>` UTC stamp for `time`, e.g.
/// `corrupt-20261018T031500Z`. Implemented with an integer civil-from-days
/// conversion so no platform date API or dependency is needed. A clock
/// before the epoch stamps as the epoch itself.
pub(crate) fn utc_stamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let within = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        within / 3_600,
        within % 3_600 / 60,
        within % 60
    )
}

/// Days since 1970-01-01 to (year, month, day), the proleptic-Gregorian
/// civil-from-days conversion.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

/// Whether `rest` — the part of a sibling name after `{stem}.` — is exactly
/// `corrupt-` + 8 digits + `T` + 6 digits + `Z`, optionally followed by the
/// `.{pid}.{id}` tail `unique_sibling` appends. Nothing else matches, so a
/// `corrupt-notes.txt` or another stem's copy is never touched.
pub(crate) fn corrupt_copy_name_is(rest: &str) -> bool {
    let Some(stamped) = rest.strip_prefix("corrupt-") else {
        return false;
    };
    if stamped.len() < STAMP_LEN {
        return false;
    }
    let (stamp, tail) = stamped.split_at(STAMP_LEN);
    let bytes = stamp.as_bytes();
    let digits = |slice: &[u8]| slice.iter().all(|byte| byte.is_ascii_digit());
    if !digits(&bytes[..8]) || bytes[8] != b'T' || !digits(&bytes[9..15]) || bytes[15] != b'Z' {
        return false;
    }
    tail.is_empty() || tail.strip_prefix('.').is_some_and(artifact_tail_is)
}

/// The `.{pid}.{id}` tail `unique_sibling` appends, same shape as
/// `leftover_name_is` accepts in `log.rs`.
fn artifact_tail_is(tail: &str) -> bool {
    let mut parts = tail.split('.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(pid), Some(id), None) => [pid, id]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
        _ => false,
    }
}

/// Lists every sibling whose name matches `corrupt_copy_name_is`. Listing
/// errors are propagated: `remove_copies` (and therefore `clear`) fails
/// closed when the directory cannot be read, while `quarantine` drops the
/// result because retention is best effort and must never fail `open`.
fn corrupt_copy_paths(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut copies = Vec::new();
    let Some(parent) = path.parent() else {
        return Ok(copies);
    };
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(copies);
    };
    let prefix = format!("{stem}.");
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(rest) = name
            .to_str()
            .and_then(|name| name.strip_prefix(prefix.as_str()))
        else {
            continue;
        };
        if corrupt_copy_name_is(rest) {
            copies.push(entry.path());
        }
    }
    Ok(copies)
}

/// Writes `bytes` — exactly what `open` read of the current-format log,
/// before any change — to a fresh `{stem}.corrupt-{stamp}` sibling, synced,
/// then keeps at most `RETAINED_COPIES` matching siblings. The copy just
/// written is always kept: its name comes from the current clock and can
/// sort before older copies stamped while the clock was ahead, so only the
/// oldest of the *other* matching names are removed. A copy failure is
/// returned so `open` fails with `log.bin` byte-for-byte unchanged;
/// retention and listing failures are ignored because they are best effort
/// and never worth losing a session of learning over.
pub(crate) fn quarantine(path: &Path, bytes: &[u8], stamp: &str) -> io::Result<()> {
    let copy = unique_sibling(path, &format!("corrupt-{stamp}"));
    write_new_file(&copy, bytes)?;
    if let Ok(mut others) = corrupt_copy_paths(path) {
        others.retain(|candidate| candidate != &copy);
        others.sort();
        while others.len() + 1 > RETAINED_COPIES {
            let _ = fs::remove_file(others.remove(0));
        }
    }
    Ok(())
}

/// Removes every sibling whose name matches `corrupt_copy_name_is`. Called
/// by `clear` before the empty log is published: `NotFound` is fine, and any
/// other removal error is returned so the clear fails without publishing
/// and `log.bin` stays unchanged.
pub(crate) fn remove_copies(path: &Path) -> io::Result<()> {
    for copy in corrupt_copy_paths(path)? {
        match fs::remove_file(&copy) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{corrupt_copy_name_is, utc_stamp};
    use std::time::{Duration, SystemTime};

    fn stamp_at(unix_seconds: u64) -> String {
        utc_stamp(SystemTime::UNIX_EPOCH + Duration::from_secs(unix_seconds))
    }

    /// Expected strings are `date -u -d @<t> +%Y%m%dT%H%M%SZ` output, an
    /// oracle independent of the conversion under test.
    #[test]
    fn utc_stamp_matches_gnu_date() {
        let expected = [
            (0u64, "19700101T000000Z"),
            (951_782_400, "20000229T000000Z"),
            (1_709_164_800, "20240229T000000Z"),
            (1_760_745_600, "20251018T000000Z"),
            (4_102_444_799, "20991231T235959Z"),
        ];
        for (seconds, stamp) in expected {
            assert_eq!(stamp_at(seconds), stamp, "unix {seconds}");
        }
    }

    #[test]
    fn corrupt_copy_name_matches_only_the_stamp_shape() {
        assert!(corrupt_copy_name_is("corrupt-20261018T031500Z"));
        assert!(corrupt_copy_name_is("corrupt-20261018T031500Z.12.3"));
        for rest in [
            "corrupt-2026101T031500Z",
            "corrupt-20261018T031500",
            "corrupt-20261018T031500Z.bak",
            "corrupt-20261018T031500Z.12",
            "corrupt-notes",
            "compact.bak",
        ] {
            assert!(!corrupt_copy_name_is(rest), "{rest:?}");
        }
    }
}
