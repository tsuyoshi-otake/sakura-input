//! Machine-wide Windows Error Reporting policy for Sakura processes.
//!
//! WER's `LocalDumps` keys are the operating-system crash-capture mechanism;
//! Sakura never uploads these sensitive files. Every executable writes into the
//! same per-user directory and the system retains at most five dumps per image.

use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows_core::Result;

use crate::registry::{RegKey, RegistryView};

const LOCAL_DUMPS: &str = r"SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps";
pub const DUMP_FOLDER: &str = r"%LOCALAPPDATA%\SakuraInput\dumps";
pub const DUMP_COUNT: u32 = 5;
pub const DUMP_TYPE_MINI: u32 = 1;

/// Executables whose crashes should leave a local diagnostic artifact.
pub const EXECUTABLES: [&str; 6] = [
    "sakura_engine.exe",
    "sakura_logon.exe",
    "sakura_renderer.exe",
    "sakura_regtool.exe",
    "sakura_settings.exe",
    "sakura_settings_payload.exe",
];

/// Installs the bounded WER policy. Requires administrator rights.
pub fn configure_local_dumps() -> Result<()> {
    for executable in EXECUTABLES {
        let key = RegKey::create(
            HKEY_LOCAL_MACHINE,
            &format!(r"{LOCAL_DUMPS}\{executable}"),
            RegistryView::Native,
        )?;
        key.set_expand_string("DumpFolder", DUMP_FOLDER)?;
        key.set_dword("DumpCount", DUMP_COUNT)?;
        key.set_dword("DumpType", DUMP_TYPE_MINI)?;
    }
    Ok(())
}

/// Removes Sakura's WER policy while deliberately leaving existing sensitive
/// dump files for the uninstaller's explicit `/PURGE=1` decision.
pub fn remove_local_dumps() -> Result<()> {
    let Some(root) =
        RegKey::open_for_delete(HKEY_LOCAL_MACHINE, LOCAL_DUMPS, RegistryView::Native)?
    else {
        return Ok(());
    };
    let mut first_error = None;
    for executable in EXECUTABLES {
        if let Err(error) = root.delete_tree(executable) {
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_is_bounded_and_names_only_owned_processes() {
        assert_eq!(DUMP_COUNT, 5);
        assert_eq!(DUMP_TYPE_MINI, 1);
        assert_eq!(DUMP_FOLDER, r"%LOCALAPPDATA%\SakuraInput\dumps");
        assert_eq!(EXECUTABLES.len(), 6);
        for executable in EXECUTABLES {
            assert!(executable.starts_with("sakura_"));
            assert!(executable.ends_with(".exe"));
            assert!(!executable.contains(['\\', '/']));
        }
    }
}
