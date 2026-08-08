//! The `HKLM\Software\Classes\CLSID` entries that let COM find `sakura_tsf.dll`.

use std::path::{Path, PathBuf};

use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
use windows_core::{Error, Result, HRESULT};

use crate::guids::{format_guid, CLSID_SAKURA_TSF, TEXT_SERVICE_DESCRIPTION};
use crate::registry::{RegKey, RegistryView};

/// `HKEY_CLASSES_ROOT` is a merged view whose writes land in HKCU or HKLM
/// depending on what already exists, so the absolute path is used instead:
/// a per-machine install must be unambiguously per-machine.
const CLSID_ROOT: &str = "Software\\Classes\\CLSID";

/// TSF text services are single-threaded-apartment objects; the thread that
/// activates the service owns it for its whole lifetime (DESIGN 4.4).
const THREADING_MODEL: &str = "Apartment";

fn clsid_key_path() -> String {
    format!("{CLSID_ROOT}\\{}", format_guid(&CLSID_SAKURA_TSF))
}

/// Points the given registry view at `dll_path`.
///
/// `dll_path` must match the view's bitness: the 32-bit view has to name the x86
/// DLL, or every 32-bit host will fail to load the text service.
pub fn register(dll_path: &Path, view: RegistryView) -> Result<()> {
    let dll = dll_path.to_str().ok_or_else(|| {
        // A path Windows produced is always valid UTF-16, but not necessarily
        // valid UTF-8. Refusing loudly beats writing a mangled path.
        Error::new(
            HRESULT::from_win32(windows::Win32::Foundation::ERROR_INVALID_NAME.0),
            "DLL path is not valid UTF-8",
        )
    })?;

    let class = RegKey::create(HKEY_LOCAL_MACHINE, &clsid_key_path(), view)?;
    class.set_string(None, TEXT_SERVICE_DESCRIPTION)?;

    let server = RegKey::create(
        HKEY_LOCAL_MACHINE,
        &format!("{}\\InprocServer32", clsid_key_path()),
        view,
    )?;
    server.set_string(None, dll)?;
    server.set_string(Some("ThreadingModel"), THREADING_MODEL)?;
    Ok(())
}

/// Returns the DLL path currently registered for the text service.
///
/// The stable bootstrap executables use this value as the single active
/// payload pointer.  Keeping the pointer in the same registry value COM uses
/// avoids a second "current version" file that could drift away from the
/// registration after a failed upgrade.
pub fn registered_dll(view: RegistryView) -> Result<Option<PathBuf>> {
    let path = format!("{}\\InprocServer32", clsid_key_path());
    let Some(key) = RegKey::open_for_read(HKEY_LOCAL_MACHINE, &path, view)? else {
        return Ok(None);
    };
    Ok(key.get_string(None)?.map(PathBuf::from))
}

/// Returns the directory containing the currently registered payload.
pub fn registered_payload_dir(view: RegistryView) -> Result<Option<PathBuf>> {
    Ok(registered_dll(view)?.and_then(|path| path.parent().map(Path::to_path_buf)))
}

/// Removes the class registration from the given view. Already-absent is success.
pub fn unregister(view: RegistryView) -> Result<()> {
    let Some(root) = RegKey::open_for_delete(HKEY_LOCAL_MACHINE, CLSID_ROOT, view)? else {
        return Ok(());
    };
    root.delete_tree(&format_guid(&CLSID_SAKURA_TSF))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The path is what an installer or `regsvr32` will read back; a typo here
    /// registers a class nothing can find.
    #[test]
    fn clsid_key_path_is_under_software_classes() {
        assert_eq!(
            clsid_key_path(),
            "Software\\Classes\\CLSID\\{C18F44DE-39E0-4B16-8D28-D5DE35BB11BC}"
        );
    }
}
