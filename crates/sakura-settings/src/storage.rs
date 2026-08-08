//! Transactional publication helpers for per-user files.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ARTIFACT: AtomicU64 = AtomicU64::new(1);

pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = unique_sibling(path, "settings.tmp");
    let backup = unique_sibling(path, "settings.bak");

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

    if !path.exists() {
        return fs::rename(&temporary, path).inspect_err(|_| {
            let _ = fs::remove_file(&temporary);
        });
    }

    if let Err(error) = fs::rename(path, &backup) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let rollback = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(io::Error::new(
                rollback_error.kind(),
                format!("publish failed ({error}); rollback failed ({rollback_error})"),
            )),
        };
    }
    let _ = fs::remove_file(backup);
    Ok(())
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
}
