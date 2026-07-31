//! Thin RAII wrapper over the handful of registry calls registration needs.
//!
//! Everything here is deliberately small: registration runs once per install, so
//! clarity beats efficiency, and a leaked `HKEY` in an installer is the kind of
//! bug that only shows up as a locked hive on someone else's machine.

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW, HKEY,
    KEY_ALL_ACCESS, KEY_WOW64_32KEY, KEY_WOW64_64KEY, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    REG_SAM_FLAGS, REG_SZ,
};
use windows_core::{Result, PCWSTR};

use crate::wide::{to_reg_sz_bytes, to_wide_nul};

/// Which of the two registry views on a 64-bit Windows to act on.
///
/// A 64-bit host process and a 32-bit host process each look up the text service
/// CLSID in their own view, so an IME that only registers one view is invisible
/// to half the applications on the machine (DESIGN 12.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryView {
    /// The view matching the bitness of the calling process.
    Native,
    /// The 32-bit view — `Wow6432Node` when the OS is 64-bit.
    Bits32,
    /// The 64-bit view. Meaningless on a 32-bit OS, where it is ignored.
    Bits64,
}

impl RegistryView {
    fn sam(self) -> REG_SAM_FLAGS {
        match self {
            RegistryView::Native => REG_SAM_FLAGS(0),
            RegistryView::Bits32 => KEY_WOW64_32KEY,
            RegistryView::Bits64 => KEY_WOW64_64KEY,
        }
    }
}

/// An open registry key that closes itself.
#[derive(Debug)]
pub struct RegKey(HKEY);

impl RegKey {
    /// Creates the key if absent, opens it if present.
    pub fn create(root: HKEY, subkey: &str, view: RegistryView) -> Result<Self> {
        let path = to_wide_nul(subkey);
        let mut handle = HKEY::default();
        // SAFETY: `path` is NUL-terminated and outlives the call; `handle` is a
        // valid writable out-parameter. Every optional argument is None.
        let status = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(path.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE | view.sam(),
                None,
                &mut handle,
                None,
            )
        };
        status.ok()?;
        Ok(Self(handle))
    }

    /// Opens an existing key for full access, or returns `Ok(None)` if it is not
    /// there. Absence is the normal case when unregistering a partial install.
    pub fn open_for_delete(root: HKEY, subkey: &str, view: RegistryView) -> Result<Option<Self>> {
        let path = to_wide_nul(subkey);
        let mut handle = HKEY::default();
        // SAFETY: as in `create` — NUL-terminated input, valid out-parameter.
        let status = unsafe {
            RegOpenKeyExW(
                root,
                PCWSTR(path.as_ptr()),
                None,
                KEY_ALL_ACCESS | view.sam(),
                &mut handle,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok()?;
        Ok(Some(Self(handle)))
    }

    /// Writes a `REG_SZ` value. `name` of `None` writes the key's default value.
    pub fn set_string(&self, name: Option<&str>, value: &str) -> Result<()> {
        let name_w = name.map(to_wide_nul);
        let name_ptr = name_w
            .as_ref()
            .map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
        let data = to_reg_sz_bytes(value);
        // SAFETY: `name_ptr` is either null (meaning the default value) or points
        // at NUL-terminated storage alive for the call, and `data` is a byte
        // image of a NUL-terminated UTF-16 string as REG_SZ requires.
        let status = unsafe { RegSetValueExW(self.0, name_ptr, None, REG_SZ, Some(&data)) };
        status.ok()
    }

    /// Deletes a subkey and everything beneath it. Missing is success — an
    /// uninstall must not fail because a previous uninstall already ran.
    pub fn delete_tree(&self, subkey: &str) -> Result<()> {
        let path = to_wide_nul(subkey);
        // SAFETY: `self.0` is an open key held by this wrapper and `path` is
        // NUL-terminated storage alive for the call.
        let status = unsafe { RegDeleteTreeW(self.0, PCWSTR(path.as_ptr())) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(status.into())
        }
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: `self.0` was produced by RegCreateKeyExW/RegOpenKeyExW in this
        // module and has not been closed elsewhere; the wrapper is not Copy.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
