//! Safe cleanup for inactive side-by-side payload generations.
//!
//! The active generation is selected by the machine-wide COM registration,
//! not by a filename convention. Cleanup therefore accepts the registered
//! payload directory explicitly and refuses to remove anything when that
//! directory is not a direct child of the installed `versions` directory.
//! This code is called by the elevated maintenance task at logon, where it
//! has permission to remove files under `Program Files`.

use std::fs;
use std::io;
use std::path::Path;

use std::os::windows::fs::MetadataExt;

const VERSION_BUILD_ID_LENGTH: usize = 16;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// The result of one best-effort cleanup pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// Inactive version directories or legacy payload files removed.
    pub removed: u32,
    /// Entries that could not be removed, usually because a host still has a
    /// DLL mapped. They are deliberately retained for the next pass.
    pub kept: u32,
    /// Entries ignored because they are not payload generations owned by us.
    pub skipped: u32,
}

impl CleanupReport {
    pub fn status_record(self) -> String {
        format!(
            "removed={}\tkept={}\tskipped={}\n",
            self.removed, self.kept, self.skipped
        )
    }
}

/// Removes inactive payload generations and legacy root-level payload files.
///
/// A failed removal is not an error for the caller: an old TSF DLL may still
/// be mapped by an unrelated host process. Returning that case as `kept`
/// makes the maintenance task successful while leaving the generation for a
/// later logon retry. Structural errors, such as an invalid active pointer,
/// are returned and make the task visible as failed in Task Scheduler.
pub fn cleanup_inactive_payloads(
    install_dir: &Path,
    active_payload_dir: &Path,
) -> io::Result<CleanupReport> {
    if is_reparse_point(install_dir)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the Sakura Input install directory is a reparse point",
        ));
    }
    let versions_dir = install_dir.join("versions");
    if !versions_dir.is_dir() {
        return Ok(cleanup_legacy_root_payloads(install_dir));
    }
    if is_reparse_point(&versions_dir)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the Sakura Input versions directory is a reparse point",
        ));
    }

    let canonical_versions = fs::canonicalize(&versions_dir)?;
    let canonical_active = fs::canonicalize(active_payload_dir)?;
    let Some(active_parent) = canonical_active.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the active Sakura Input payload has no parent directory",
        ));
    };
    if active_parent != canonical_versions {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the active Sakura Input payload is outside the versions directory",
        ));
    }

    let active_name = canonical_active.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the active Sakura Input payload has no directory name",
        )
    })?;
    if !is_version_dir_name(active_name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the active Sakura Input payload has an unexpected directory name",
        ));
    }

    let mut report = cleanup_legacy_root_payloads(install_dir);
    for entry in fs::read_dir(&canonical_versions)? {
        let entry = entry?;
        let entry_type = entry.file_type()?;
        let name = entry.file_name();
        if !entry_type.is_dir() || entry_type.is_symlink() || is_reparse_point(&entry.path())? {
            report.skipped += 1;
            continue;
        }
        if name == active_name {
            continue;
        }
        if !is_version_dir_name(&name) {
            report.skipped += 1;
            continue;
        }

        match fs::remove_dir_all(entry.path()) {
            Ok(()) => report.removed += 1,
            Err(_) => report.kept += 1,
        }
    }

    Ok(report)
}

fn cleanup_legacy_root_payloads(install_dir: &Path) -> CleanupReport {
    let mut report = CleanupReport::default();
    for leaf in ["sakura_tsf.dll", "sakura_engine.exe", "sakura_renderer.exe"] {
        let path = install_dir.join(leaf);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_file() {
            report.skipped += 1;
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => report.removed += 1,
            Err(_) => report.kept += 1,
        }
    }
    for leaf in ["dict", "docs", "licenses"] {
        let path = install_dir.join(leaf);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
            report.skipped += 1;
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => report.removed += 1,
            Err(_) => report.kept += 1,
        }
    }
    report
}

fn is_version_dir_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some((version, build_id)) = name.rsplit_once('-') else {
        return false;
    };
    !version.is_empty()
        && build_id.len() == VERSION_BUILD_ID_LENGTH
        && build_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_reparse_point(path: &Path) -> io::Result<bool> {
    Ok(fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn removes_inactive_generations_but_keeps_the_active_one() {
        let root = temporary_root();
        let versions = root.join("versions");
        let active = versions.join("1.0.0-2222222222222222");
        let old = versions.join("1.0.0-1111111111111111");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("sakura_tsf.dll"), b"old").unwrap();

        let report = cleanup_inactive_payloads(&root, &active).unwrap();

        assert_eq!(report.removed, 1);
        assert_eq!(report.kept, 0);
        assert!(active.is_dir());
        assert!(!old.exists());
        remove_temporary_root(&root);
    }

    #[test]
    fn ignores_unrecognized_entries_under_versions() {
        let root = temporary_root();
        let versions = root.join("versions");
        let active = versions.join("1.0.0-2222222222222222");
        let unrelated = versions.join("backup");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&unrelated).unwrap();

        let report = cleanup_inactive_payloads(&root, &active).unwrap();

        assert_eq!(report.removed, 0);
        assert_eq!(report.skipped, 1);
        assert!(unrelated.is_dir());
        remove_temporary_root(&root);
    }

    #[test]
    fn removes_only_known_legacy_root_payloads() {
        let root = temporary_root();
        let versions = root.join("versions");
        let active = versions.join("1.0.0-2222222222222222");
        let unrelated = root.join("operator-notes");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(root.join("dict")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("licenses")).unwrap();
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(root.join("sakura_tsf.dll"), b"legacy").unwrap();

        let report = cleanup_inactive_payloads(&root, &active).unwrap();

        assert_eq!(report.removed, 4);
        assert_eq!(report.kept, 0);
        assert_eq!(report.skipped, 0);
        assert!(active.is_dir());
        assert!(unrelated.is_dir());
        assert!(!root.join("dict").exists());
        assert!(!root.join("docs").exists());
        assert!(!root.join("licenses").exists());
        assert!(!root.join("sakura_tsf.dll").exists());
        remove_temporary_root(&root);
    }

    #[test]
    fn refuses_an_active_directory_outside_versions() {
        let root = temporary_root();
        let active = root.join("elsewhere");
        fs::create_dir_all(root.join("versions")).unwrap();
        fs::create_dir_all(&active).unwrap();

        let error = cleanup_inactive_payloads(&root, &active).expect_err("outside pointer");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        remove_temporary_root(&root);
    }

    #[test]
    fn accepts_only_version_directories_with_a_build_id() {
        assert!(is_version_dir_name(std::ffi::OsStr::new(
            "1.0.0-abcdef0123456789"
        )));
        assert!(!is_version_dir_name(std::ffi::OsStr::new("backup")));
        assert!(!is_version_dir_name(std::ffi::OsStr::new(
            "1.0.0-abcdef012345678"
        )));
        assert!(!is_version_dir_name(std::ffi::OsStr::new(
            "1.0.0-abcdef012345678g"
        )));
    }

    fn temporary_root() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "sakura-input-payload-cleanup-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn remove_temporary_root(root: &Path) {
        fs::remove_dir_all(root).unwrap();
    }
}
