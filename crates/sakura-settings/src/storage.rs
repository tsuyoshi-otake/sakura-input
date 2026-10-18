//! Transactional publication helpers for per-user files.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

static NEXT_ARTIFACT: AtomicU64 = AtomicU64::new(1);

pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = unique_sibling(path, "settings.tmp");

    let write_result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    // A single rename replaces an existing file on Windows, so the canonical
    // path is never missing between a backup rename and a replacement rename
    // (#360 A4, original item #6). Any failure above leaves it untouched and
    // removes the temporary file.
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    sweep_stale_publish_artifacts(path);
    Ok(())
}

/// How old a `{name}.settings.bak.*` / `{name}.settings.tmp.*` sibling must be
/// before `atomic_write` removes it. A live writer's temporary file exists for
/// milliseconds, so the bound only matches artifacts older builds abandoned.
const STALE_PUBLISH_ARTIFACT_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Best-effort removal of publish leftovers the pre-#360-A4 two-step
/// implementation could strand. Only this file's own `{name}.settings.bak.*`
/// and `{name}.settings.tmp.*` siblings older than 24 hours are candidates —
/// another base name's files are never touched, and a fresh leftover (possibly
/// an in-flight write by a racing process) is kept.
fn sweep_stale_publish_artifacts(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let backup_prefix = format!("{name}.settings.bak.");
    let temporary_prefix = format!("{name}.settings.tmp.");
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let is_stale_artifact = (|| -> io::Result<bool> {
            if !entry.file_type()?.is_file() {
                return Ok(false);
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Ok(false);
            };
            if !(name.starts_with(backup_prefix.as_str())
                || name.starts_with(temporary_prefix.as_str()))
            {
                return Ok(false);
            }
            let modified = entry.metadata()?.modified()?;
            Ok(now
                .duration_since(modified)
                .map(|age| age > STALE_PUBLISH_ARTIFACT_AGE)
                .unwrap_or(false))
        })();
        if matches!(is_stale_artifact, Ok(true)) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn unique_sibling(path: &Path, suffix: &str) -> PathBuf {
    let id = NEXT_ARTIFACT.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings");
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{name}.{suffix}.{}.{id}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_write_and_replacement_publish_complete_files() {
        let directory = std::env::temp_dir().join(format!(
            "sakura-settings-storage-{}-{}",
            std::process::id(),
            NEXT_ARTIFACT.fetch_add(1, Ordering::Relaxed)
        ));
        let path = directory.join("nested").join("value.txt");
        atomic_write(&path, b"first").expect("first write");
        assert_eq!(fs::read(&path).expect("first read"), b"first");
        atomic_write(&path, b"second and complete").expect("replace");
        assert_eq!(
            fs::read(&path).expect("second read"),
            b"second and complete"
        );
        let _ = fs::remove_dir_all(directory);
    }

    fn sibling_names(directory: &Path) -> Vec<String> {
        let mut names = fs::read_dir(directory)
            .expect("list siblings")
            .map(|entry| {
                entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn test_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "sakura-settings-storage-{name}-{}-{}",
            std::process::id(),
            NEXT_ARTIFACT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).expect("test directory");
        directory
    }

    // E1: a successful publish leaves only the canonical file — no `*.tmp*`
    // and no rollback `*.bak*` sibling (#360 A4).
    #[test]
    fn successful_publish_leaves_no_artifacts() {
        let directory = test_directory("clean");
        let path = directory.join("value.txt");
        atomic_write(&path, b"first").expect("first write");
        atomic_write(&path, b"second").expect("replace");

        assert_eq!(fs::read(&path).expect("read"), b"second");
        assert_eq!(sibling_names(&directory), vec!["value.txt".to_owned()]);
        let _ = fs::remove_dir_all(directory);
    }

    // E2: when the publish rename cannot replace the canonical name — here a
    // non-empty directory — the write fails and no temporary is left behind.
    // The two-step implementation renamed the directory aside and reported
    // success, so this test fails on the pre-fix code.
    #[test]
    fn publish_failure_keeps_canonical_and_removes_temporary() {
        let directory = test_directory("blocked");
        let path = directory.join("value.txt");
        fs::create_dir(&path).expect("canonical directory");
        fs::write(path.join("occupied"), b"keep").expect("directory contents");

        atomic_write(&path, b"payload").expect_err("publish rename blocked");

        assert!(path.is_dir(), "canonical name is still the directory");
        assert_eq!(sibling_names(&path), vec!["occupied".to_owned()]);
        assert_eq!(sibling_names(&directory), vec!["value.txt".to_owned()]);
        let _ = fs::remove_dir_all(directory);
    }

    // E5: after a successful write, stale `settings.bak`/`settings.tmp`
    // leftovers older than 24h are swept; a fresh one and another base name's
    // artifacts are kept.
    #[test]
    fn stale_publish_artifacts_are_swept_after_success() {
        let directory = test_directory("sweep");
        let path = directory.join("value.txt");
        atomic_write(&path, b"current").expect("initial write");

        let stale_backup = directory.join("value.txt.settings.bak.111.1");
        let stale_temporary = directory.join("value.txt.settings.tmp.111.2");
        let fresh_backup = directory.join("value.txt.settings.bak.222.3");
        let other_base = directory.join("other.txt.settings.bak.111.1");
        for artifact in [&stale_backup, &stale_temporary, &fresh_backup, &other_base] {
            fs::write(artifact, b"artifact").expect("artifact");
        }
        let stale_time = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        for artifact in [&stale_backup, &stale_temporary, &other_base] {
            fs::File::options()
                .write(true)
                .open(artifact)
                .expect("open artifact")
                .set_modified(stale_time)
                .expect("age artifact");
        }

        atomic_write(&path, b"new").expect("replace");

        assert!(!stale_backup.exists(), "stale backup was swept");
        assert!(!stale_temporary.exists(), "stale temporary was swept");
        assert!(fresh_backup.exists(), "a fresh leftover is kept");
        assert!(other_base.exists(), "another base name is kept");
        let _ = fs::remove_dir_all(directory);
    }
}
