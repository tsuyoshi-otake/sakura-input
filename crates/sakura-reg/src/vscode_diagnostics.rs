//! Explicit, administrator-only WER policy for the VS Code host process.
//!
//! This module owns both the HKLM registry boundary and the ownership marker
//! used to prove that a later removal may touch only values Sakura created.
//! Nothing here is called by the TSF key path. The default installer never
//! invokes these functions.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND};
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows_core::{Error, Result};

use crate::registry::{RegKey, RegistryView};

const LOCAL_DUMPS: &str = r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps";
const TARGET_KEY: &str = r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\Code.exe";
const MARKER_KEY: &str = r"SOFTWARE\SakuraInput\Diagnostics\VscodeDumps";
const OWNER_MARKER: &str = "sakura-input-vscode-dumps-v1";
const TARGET_NAME: &str = "Code.exe";

pub const DUMP_FOLDER: &str = r"%LOCALAPPDATA%\SakuraInput\dumps\vscode";
pub const DUMP_TYPE: u32 = 1;
pub const DUMP_COUNT: u32 = 5;

const DUMP_FOLDER_VALUE: &str = "DumpFolder";
const DUMP_TYPE_VALUE: &str = "DumpType";
const DUMP_COUNT_VALUE: &str = "DumpCount";
const OWNER_VALUE: &str = "Owner";
const TARGET_VALUE: &str = "Target";
const MARKER_FOLDER_VALUE: &str = "DumpFolder";
const MARKER_TYPE_VALUE: &str = "DumpType";
const MARKER_COUNT_VALUE: &str = "DumpCount";

/// The operation exposed by `sakura_regtool diagnostics vscode-dumps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Configure,
    Status,
    Remove,
    Clear,
}

/// Every registry branch terminates in one of these observable states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalOutcome {
    Configured,
    AlreadyConfigured,
    UnmanagedConflict,
    Removed,
    NotConfigured,
    Cleared,
    AccessDenied,
    ConfirmationRequired,
    Failed,
}

impl TerminalOutcome {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Configured => "Configured",
            Self::AlreadyConfigured => "AlreadyConfigured",
            Self::UnmanagedConflict => "UnmanagedConflict",
            Self::Removed => "Removed",
            Self::NotConfigured => "NotConfigured",
            Self::Cleared => "Cleared",
            Self::AccessDenied => "AccessDenied",
            Self::ConfirmationRequired => "ConfirmationRequired",
            Self::Failed => "Failed",
        }
    }
}

/// ACL verification is deliberately scoped: HKLM affects all users, but an
/// elevated command can only probe the account under which it currently runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclVerification {
    VerifiedForCurrentUser,
    Unverified,
    InvalidPath,
    AccessDenied,
}

impl AclVerification {
    pub const fn name(self) -> &'static str {
        match self {
            Self::VerifiedForCurrentUser => "VerifiedForCurrentUser",
            Self::Unverified => "Unverified",
            Self::InvalidPath => "InvalidPath",
            Self::AccessDenied => "AccessDenied",
        }
    }
}

/// A serializable command result for the CLI and diagnostics UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub outcome: TerminalOutcome,
    pub acl: AclVerification,
    pub error_code: Option<i32>,
}

impl Report {
    const fn new(outcome: TerminalOutcome) -> Self {
        Self {
            outcome,
            acl: AclVerification::Unverified,
            error_code: None,
        }
    }

    fn with_acl(mut self, acl: AclVerification) -> Self {
        self.acl = acl;
        self
    }

    fn with_error(mut self, error: &Error) -> Self {
        self.error_code = Some(error.code().0);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    Absent,
    Exact,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    marker: Presence,
    target: Presence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetRemovalScope {
    KeepKey,
    DeleteEmptyKey,
}

fn target_removal_scope(counts: crate::registry::RegistryCounts) -> TargetRemovalScope {
    if counts.values == 0 && counts.subkeys == 0 {
        TargetRemovalScope::DeleteEmptyKey
    } else {
        TargetRemovalScope::KeepKey
    }
}

/// Pure ownership decision used by the Windows implementation and by tests
/// that never touch the real HKLM hive.
fn decide(snapshot: Snapshot) -> TerminalOutcome {
    match (snapshot.marker, snapshot.target) {
        (Presence::Absent, Presence::Absent) => TerminalOutcome::NotConfigured,
        (Presence::Exact, Presence::Exact) => TerminalOutcome::AlreadyConfigured,
        _ => TerminalOutcome::UnmanagedConflict,
    }
}

/// Configures `HKLM\\...\\LocalDumps\\Code.exe` without overwriting an
/// existing key or value. An existing key is opened only for inspection.
pub fn configure() -> Report {
    if let Err(error) = require_machine_write_access() {
        return report_for_error(&error);
    }
    let snapshot = match inspect() {
        Ok(snapshot) => snapshot,
        Err(error) => return report_for_error(&error),
    };
    match decide(snapshot) {
        TerminalOutcome::AlreadyConfigured => {
            return Report::new(TerminalOutcome::AlreadyConfigured)
                .with_acl(verify_dump_folder_acl());
        }
        TerminalOutcome::UnmanagedConflict => {
            return Report::new(TerminalOutcome::UnmanagedConflict);
        }
        TerminalOutcome::NotConfigured => {}
        _ => return Report::new(TerminalOutcome::Failed),
    }

    let (target, created) =
        match RegKey::create_with_disposition(HKEY_LOCAL_MACHINE, TARGET_KEY, RegistryView::Native)
        {
            Ok(value) => value,
            Err(error) => return report_for_error(&error),
        };
    if !created {
        // A race opened a key after inspection. Do not overwrite it.
        return Report::new(TerminalOutcome::UnmanagedConflict);
    }
    if let Err(error) = target
        .set_expand_string(DUMP_FOLDER_VALUE, DUMP_FOLDER)
        .and_then(|_| target.set_dword(DUMP_TYPE_VALUE, DUMP_TYPE))
        .and_then(|_| target.set_dword(DUMP_COUNT_VALUE, DUMP_COUNT))
    {
        return report_for_error(&error);
    }

    let acl = verify_dump_folder_acl();
    if !matches!(acl, AclVerification::VerifiedForCurrentUser) {
        return Report::new(TerminalOutcome::Failed).with_acl(acl);
    }

    let (marker, marker_created) =
        match RegKey::create_with_disposition(HKEY_LOCAL_MACHINE, MARKER_KEY, RegistryView::Native)
        {
            Ok(value) => value,
            Err(error) => return report_for_error(&error),
        };
    if !marker_created {
        // Never overwrite a marker that is not ours. The target remains visible
        // as an unmanaged conflict for a later operator to inspect.
        return Report::new(TerminalOutcome::UnmanagedConflict).with_acl(acl);
    }
    if let Err(error) = marker
        .set_string(Some(OWNER_VALUE), OWNER_MARKER)
        .and_then(|_| marker.set_string(Some(TARGET_VALUE), TARGET_NAME))
        .and_then(|_| marker.set_expand_string(MARKER_FOLDER_VALUE, DUMP_FOLDER))
        .and_then(|_| marker.set_dword(MARKER_TYPE_VALUE, DUMP_TYPE))
        .and_then(|_| marker.set_dword(MARKER_COUNT_VALUE, DUMP_COUNT))
    {
        return report_for_error(&error);
    }

    Report::new(TerminalOutcome::Configured).with_acl(acl)
}

/// Reports the current ownership state without modifying HKLM.
pub fn status() -> Report {
    match inspect() {
        Ok(snapshot) => Report::new(match decide(snapshot) {
            TerminalOutcome::NotConfigured => TerminalOutcome::NotConfigured,
            TerminalOutcome::AlreadyConfigured => TerminalOutcome::AlreadyConfigured,
            _ => TerminalOutcome::UnmanagedConflict,
        })
        .with_acl(verify_dump_folder_acl()),
        Err(error) => report_for_error(&error),
    }
}

/// Removes only values that match Sakura's ownership marker and desired values.
/// Unmanaged or modified target values are never overwritten or deleted.
pub fn remove() -> Report {
    if let Err(error) = require_machine_write_access() {
        return report_for_error(&error);
    }
    let snapshot = match inspect() {
        Ok(snapshot) => snapshot,
        Err(error) => return report_for_error(&error),
    };
    match (snapshot.marker, snapshot.target) {
        (Presence::Absent, Presence::Absent) => return Report::new(TerminalOutcome::NotConfigured),
        (Presence::Exact, Presence::Exact) => {}
        _ => return Report::new(TerminalOutcome::UnmanagedConflict),
    }

    let target = match RegKey::open_for_delete(HKEY_LOCAL_MACHINE, TARGET_KEY, RegistryView::Native)
    {
        Ok(Some(key)) => key,
        Ok(None) => return remove_marker(TerminalOutcome::Removed),
        Err(error) => return report_for_error(&error),
    };
    for name in [DUMP_FOLDER_VALUE, DUMP_TYPE_VALUE, DUMP_COUNT_VALUE] {
        if let Err(error) = target.delete_value(name) {
            return report_for_error(&error);
        }
    }
    let counts = match target.counts() {
        Ok(counts) => counts,
        Err(error) => return report_for_error(&error),
    };
    if target_removal_scope(counts) == TargetRemovalScope::KeepKey {
        // User/third-party additions remain. We removed only the exact Sakura
        // values and intentionally leave the target key in place.
        return remove_marker(TerminalOutcome::Removed);
    }

    let parent =
        match RegKey::open_for_delete(HKEY_LOCAL_MACHINE, LOCAL_DUMPS, RegistryView::Native) {
            Ok(Some(key)) => key,
            Ok(None) => return remove_marker(TerminalOutcome::Removed),
            Err(error) => return report_for_error(&error),
        };
    if let Err(error) = parent.delete_empty_subkey(TARGET_NAME, RegistryView::Native) {
        return report_for_error(&error);
    }
    remove_marker(TerminalOutcome::Removed)
}

fn require_machine_write_access() -> Result<()> {
    match RegKey::open_for_write(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows\Windows Error Reporting",
        RegistryView::Native,
    )? {
        Some(_) => Ok(()),
        None => Err(ERROR_FILE_NOT_FOUND.into()),
    }
}

fn remove_marker(success: TerminalOutcome) -> Report {
    let parent = match RegKey::open_for_delete(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\SakuraInput\Diagnostics",
        RegistryView::Native,
    ) {
        Ok(Some(key)) => key,
        Ok(None) => return Report::new(success),
        Err(error) => return report_for_error(&error),
    };
    let marker = match RegKey::open_for_delete(HKEY_LOCAL_MACHINE, MARKER_KEY, RegistryView::Native)
    {
        Ok(Some(key)) => key,
        Ok(None) => return Report::new(success),
        Err(error) => return report_for_error(&error),
    };
    for name in [
        OWNER_VALUE,
        TARGET_VALUE,
        MARKER_FOLDER_VALUE,
        MARKER_TYPE_VALUE,
        MARKER_COUNT_VALUE,
    ] {
        if let Err(error) = marker.delete_value(name) {
            return report_for_error(&error);
        }
    }
    let counts = match marker.counts() {
        Ok(counts) => counts,
        Err(error) => return report_for_error(&error),
    };
    if counts.values == 0 && counts.subkeys == 0 {
        if let Err(error) = parent.delete_tree("VscodeDumps") {
            return report_for_error(&error);
        }
    }
    Report::new(success)
}

/// Clears the dump directory only after the caller has made this irreversible
/// operation explicit (the CLI requires `--confirm`).
pub fn clear() -> Report {
    let path = match validated_dump_directory() {
        Ok(path) => path,
        Err(ClearError::AccessDenied) => return Report::new(TerminalOutcome::AccessDenied),
        Err(ClearError::InvalidPath | ClearError::Io) => {
            return Report::new(TerminalOutcome::Failed)
        }
    };
    if !path.exists() {
        return Report::new(TerminalOutcome::NotConfigured);
    }
    match fs::remove_dir_all(&path) {
        Ok(()) => Report::new(TerminalOutcome::Cleared),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            Report::new(TerminalOutcome::AccessDenied)
        }
        Err(_) => Report::new(TerminalOutcome::Failed),
    }
}

fn inspect() -> Result<Snapshot> {
    Ok(Snapshot {
        marker: inspect_marker()?,
        target: inspect_target()?,
    })
}

fn inspect_marker() -> Result<Presence> {
    let Some(marker) = RegKey::open_for_read(HKEY_LOCAL_MACHINE, MARKER_KEY, RegistryView::Native)?
    else {
        return Ok(Presence::Absent);
    };
    let exact = marker.get_string(Some(OWNER_VALUE))?.as_deref() == Some(OWNER_MARKER)
        && marker.get_string(Some(TARGET_VALUE))?.as_deref() == Some(TARGET_NAME)
        && marker.get_string(Some(MARKER_FOLDER_VALUE))?.as_deref() == Some(DUMP_FOLDER)
        && marker.get_dword(MARKER_TYPE_VALUE)? == Some(DUMP_TYPE)
        && marker.get_dword(MARKER_COUNT_VALUE)? == Some(DUMP_COUNT);
    Ok(if exact {
        Presence::Exact
    } else {
        Presence::Conflict
    })
}

fn inspect_target() -> Result<Presence> {
    let Some(target) = RegKey::open_for_read(HKEY_LOCAL_MACHINE, TARGET_KEY, RegistryView::Native)?
    else {
        return Ok(Presence::Absent);
    };
    let exact = target
        .get_string(Some(DUMP_FOLDER_VALUE))
        .ok()
        .flatten()
        .as_deref()
        == Some(DUMP_FOLDER)
        && target.get_dword(DUMP_TYPE_VALUE).ok().flatten() == Some(DUMP_TYPE)
        && target.get_dword(DUMP_COUNT_VALUE).ok().flatten() == Some(DUMP_COUNT)
        && target.counts()?.values >= 3;
    Ok(if exact {
        Presence::Exact
    } else {
        Presence::Conflict
    })
}

fn report_for_error(error: &Error) -> Report {
    let outcome = if error.code() == windows_core::HRESULT::from_win32(ERROR_ACCESS_DENIED.0) {
        TerminalOutcome::AccessDenied
    } else {
        TerminalOutcome::Failed
    };
    Report::new(outcome).with_error(error)
}

fn verify_dump_folder_acl() -> AclVerification {
    let path = match validated_dump_directory() {
        Ok(path) => path,
        Err(ClearError::AccessDenied) => return AclVerification::AccessDenied,
        Err(ClearError::InvalidPath | ClearError::Io) => return AclVerification::InvalidPath,
    };
    if fs::create_dir_all(&path).is_err() {
        return AclVerification::AccessDenied;
    }
    let probe = path.join(format!(".sakura-acl-probe-{}", std::process::id()));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(probe);
            AclVerification::VerifiedForCurrentUser
        }
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            AclVerification::AccessDenied
        }
        Err(_) => AclVerification::Unverified,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClearError {
    AccessDenied,
    InvalidPath,
    Io,
}

fn validated_dump_directory() -> std::result::Result<PathBuf, ClearError> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or(ClearError::InvalidPath)?;
    let base = PathBuf::from(local);
    if !base.is_absolute() {
        return Err(ClearError::InvalidPath);
    }
    let path = base.join("SakuraInput").join("dumps").join("vscode");
    if path.components().count() < 4 {
        return Err(ClearError::InvalidPath);
    }
    if path.exists() {
        let canonical_base = base.canonicalize().map_err(map_io)?;
        let canonical_path = path.canonicalize().map_err(map_io)?;
        if !canonical_path.starts_with(&canonical_base) {
            return Err(ClearError::InvalidPath);
        }
    }
    Ok(path)
}

fn map_io(error: io::Error) -> ClearError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        ClearError::AccessDenied
    } else {
        ClearError::Io
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(marker: Presence, target: Presence) -> Snapshot {
        Snapshot { marker, target }
    }

    #[test]
    fn absent_policy_is_not_configured() {
        assert_eq!(
            decide(snapshot(Presence::Absent, Presence::Absent)),
            TerminalOutcome::NotConfigured
        );
    }

    #[test]
    fn exact_marker_and_values_are_idempotent() {
        assert_eq!(
            decide(snapshot(Presence::Exact, Presence::Exact)),
            TerminalOutcome::AlreadyConfigured
        );
    }

    #[test]
    fn matching_values_without_marker_are_unmanaged() {
        assert_eq!(
            decide(snapshot(Presence::Absent, Presence::Exact)),
            TerminalOutcome::UnmanagedConflict
        );
    }

    #[test]
    fn marker_without_target_is_unmanaged_and_not_removed_implicitly() {
        assert_eq!(
            decide(snapshot(Presence::Exact, Presence::Absent)),
            TerminalOutcome::UnmanagedConflict
        );
    }

    #[test]
    fn changed_marker_or_target_is_unmanaged() {
        for snapshot in [
            snapshot(Presence::Conflict, Presence::Absent),
            snapshot(Presence::Absent, Presence::Conflict),
            snapshot(Presence::Conflict, Presence::Exact),
            snapshot(Presence::Exact, Presence::Conflict),
        ] {
            assert_eq!(decide(snapshot), TerminalOutcome::UnmanagedConflict);
        }
    }

    #[test]
    fn constants_are_machine_wide_and_bounded() {
        assert_eq!(
            TARGET_KEY,
            r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\Code.exe"
        );
        assert_eq!(DUMP_TYPE, 1);
        assert_eq!(DUMP_COUNT, 5);
        assert!(DUMP_FOLDER.starts_with(r"%LOCALAPPDATA%\"));
        assert_eq!(OWNER_MARKER, "sakura-input-vscode-dumps-v1");
    }

    #[test]
    fn outcomes_are_stable_for_cli_and_reports() {
        assert_eq!(TerminalOutcome::Configured.name(), "Configured");
        assert_eq!(TerminalOutcome::AccessDenied.name(), "AccessDenied");
        assert_eq!(
            AclVerification::VerifiedForCurrentUser.name(),
            "VerifiedForCurrentUser"
        );
    }

    #[test]
    fn registry_removal_scope_preserves_foreign_values_and_subkeys() {
        assert_eq!(
            target_removal_scope(crate::registry::RegistryCounts {
                values: 0,
                subkeys: 0,
            }),
            TargetRemovalScope::DeleteEmptyKey
        );
        for counts in [
            crate::registry::RegistryCounts {
                values: 1,
                subkeys: 0,
            },
            crate::registry::RegistryCounts {
                values: 0,
                subkeys: 1,
            },
            crate::registry::RegistryCounts {
                values: 2,
                subkeys: 3,
            },
        ] {
            assert_eq!(target_removal_scope(counts), TargetRemovalScope::KeepKey);
        }
    }
}
