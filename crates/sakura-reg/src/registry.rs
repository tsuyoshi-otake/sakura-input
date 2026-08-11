//! Thin RAII wrapper over the handful of registry calls registration needs.
//!
//! Everything here is deliberately small: registration runs once per install, so
//! clarity beats efficiency, and a leaked `HKEY` in an installer is the kind of
//! bug that only shows up as a locked hive on someone else's machine.

use windows::Win32::Foundation::{ERROR_DATATYPE_MISMATCH, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW,
    RegQueryInfoKeyW, RegQueryValueExW, RegSetValueExW, HKEY, KEY_ALL_ACCESS, KEY_READ,
    KEY_WOW64_32KEY, KEY_WOW64_64KEY, KEY_WRITE, REG_CREATED_NEW_KEY, REG_DWORD, REG_EXPAND_SZ,
    REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
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

/// Counts the direct children of a key without enumerating or deleting them.
///
/// The counts are used by ownership checks so a Sakura removal can delete only
/// values it created and leave user or third-party additions untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryCounts {
    pub subkeys: u32,
    pub values: u32,
}

impl RegKey {
    /// Creates the key if absent, opens it if present.
    pub fn create(root: HKEY, subkey: &str, view: RegistryView) -> Result<Self> {
        Ok(Self::create_with_disposition(root, subkey, view)?.0)
    }

    /// Creates a key and reports whether this call created it. Callers that
    /// protect user-owned registry values must use the disposition instead of
    /// blindly treating an opened existing key as writable.
    pub fn create_with_disposition(
        root: HKEY,
        subkey: &str,
        view: RegistryView,
    ) -> Result<(Self, bool)> {
        let path = to_wide_nul(subkey);
        let mut handle = HKEY::default();
        let mut disposition = REG_CREATED_NEW_KEY;
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
                Some(&mut disposition),
            )
        };
        status.ok()?;
        Ok((Self(handle), disposition == REG_CREATED_NEW_KEY))
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

    /// Opens an existing key for reading, or returns `Ok(None)` when it is not
    /// present.  The bootstrap executables use this to resolve the currently
    /// registered payload without needing write access to the machine hive.
    pub fn open_for_read(root: HKEY, subkey: &str, view: RegistryView) -> Result<Option<Self>> {
        let path = to_wide_nul(subkey);
        let mut handle = HKEY::default();
        // SAFETY: `path` is NUL terminated and `handle` is a valid writable
        // out-parameter.  The requested access is read-only.
        let status = unsafe {
            RegOpenKeyExW(
                root,
                PCWSTR(path.as_ptr()),
                None,
                KEY_READ | view.sam(),
                &mut handle,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok()?;
        Ok(Some(Self(handle)))
    }

    /// Opens an existing key with write access. This is used for an explicit
    /// elevation probe before destructive machine-wide operations; it never
    /// creates a missing key.
    pub fn open_for_write(root: HKEY, subkey: &str, view: RegistryView) -> Result<Option<Self>> {
        let path = to_wide_nul(subkey);
        let mut handle = HKEY::default();
        // SAFETY: the path is NUL-terminated and the output handle is valid
        // storage for the synchronous registry call.
        let status = unsafe {
            RegOpenKeyExW(
                root,
                PCWSTR(path.as_ptr()),
                None,
                KEY_WRITE | view.sam(),
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
        self.set_wide_string(name, value, REG_SZ)
    }

    /// Writes a `REG_EXPAND_SZ` value such as a per-user dump path containing
    /// `%LOCALAPPDATA%`.
    pub fn set_expand_string(&self, name: &str, value: &str) -> Result<()> {
        self.set_wide_string(Some(name), value, REG_EXPAND_SZ)
    }

    fn set_wide_string(
        &self,
        name: Option<&str>,
        value: &str,
        value_type: windows::Win32::System::Registry::REG_VALUE_TYPE,
    ) -> Result<()> {
        let name_w = name.map(to_wide_nul);
        let name_ptr = name_w
            .as_ref()
            .map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
        let data = to_reg_sz_bytes(value);
        // SAFETY: `name_ptr` is either null (meaning the default value) or points
        // at NUL-terminated storage alive for the call, and `data` is a byte
        // image of a NUL-terminated UTF-16 string as REG_SZ requires.
        let status = unsafe { RegSetValueExW(self.0, name_ptr, None, value_type, Some(&data)) };
        status.ok()
    }

    /// Writes a little-endian `REG_DWORD` value.
    pub fn set_dword(&self, name: &str, value: u32) -> Result<()> {
        let name = to_wide_nul(name);
        let data = value.to_le_bytes();
        // SAFETY: `name` is NUL terminated and both it and the four-byte DWORD
        // image remain alive for the duration of the call.
        let status =
            unsafe { RegSetValueExW(self.0, PCWSTR(name.as_ptr()), None, REG_DWORD, Some(&data)) };
        status.ok()
    }

    /// Reads a `REG_DWORD` value, returning `None` when the value is absent.
    pub fn get_dword(&self, name: &str) -> Result<Option<u32>> {
        let name = to_wide_nul(name);
        let mut value_type = REG_VALUE_TYPE(0);
        let mut byte_count = 0u32;
        // SAFETY: the name is NUL-terminated and the size/type pointers are
        // valid for this synchronous query.
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut value_type),
                None,
                Some(&mut byte_count),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok()?;
        if value_type != REG_DWORD || byte_count != 4 {
            return Err(ERROR_DATATYPE_MISMATCH.into());
        }

        let mut bytes = [0u8; 4];
        // SAFETY: `bytes` is the exact DWORD size reported above and remains
        // alive for the duration of the call.
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut value_type),
                Some(bytes.as_mut_ptr()),
                Some(&mut byte_count),
            )
        };
        status.ok()?;
        if byte_count != bytes.len() as u32 {
            return Err(ERROR_DATATYPE_MISMATCH.into());
        }
        Ok(Some(u32::from_le_bytes(bytes)))
    }

    /// Reads the registry kind without decoding its data. Ownership-sensitive
    /// callers use this to distinguish Sakura's `REG_EXPAND_SZ` path from an
    /// otherwise identical user-created `REG_SZ` value.
    pub fn get_value_type(&self, name: Option<&str>) -> Result<Option<REG_VALUE_TYPE>> {
        let name_w = name.map(to_wide_nul);
        let name_ptr = name_w
            .as_ref()
            .map_or(PCWSTR::null(), |value| PCWSTR(value.as_ptr()));
        let mut value_type = REG_VALUE_TYPE(0);
        let mut byte_count = 0u32;
        // SAFETY: the name is either null (the default value) or NUL-terminated
        // storage alive for this synchronous query; output pointers are valid.
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                name_ptr,
                None,
                Some(&mut value_type),
                None,
                Some(&mut byte_count),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok()?;
        Ok(Some(value_type))
    }

    /// Counts direct values and subkeys without exposing their names.
    pub fn counts(&self) -> Result<RegistryCounts> {
        let mut subkeys = 0u32;
        let mut values = 0u32;
        // SAFETY: all optional output pointers are either valid local storage
        // or `None`; no class/name buffers are requested.
        let status = unsafe {
            RegQueryInfoKeyW(
                self.0,
                None,
                None,
                None,
                Some(&mut subkeys),
                None,
                None,
                Some(&mut values),
                None,
                None,
                None,
                None,
            )
        };
        status.ok()?;
        Ok(RegistryCounts { subkeys, values })
    }

    /// Deletes one direct value. Missing is success; other values and subkeys
    /// are never touched.
    pub fn delete_value(&self, name: &str) -> Result<()> {
        let name = to_wide_nul(name);
        // SAFETY: the key is owned by this wrapper and the value name remains
        // alive for the synchronous call.
        let status = unsafe { RegDeleteValueW(self.0, PCWSTR(name.as_ptr())) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(status.into())
        }
    }

    /// Reads a `REG_SZ` or `REG_EXPAND_SZ` value.  Registry strings are
    /// returned without the terminating NUL and are not environment-expanded;
    /// callers that store paths need the literal value that COM will use.
    pub fn get_string(&self, name: Option<&str>) -> Result<Option<String>> {
        let name_w = name.map(to_wide_nul);
        let name_ptr = name_w
            .as_ref()
            .map_or(PCWSTR::null(), |value| PCWSTR(value.as_ptr()));
        let mut value_type = REG_VALUE_TYPE(0);
        let mut byte_count = 0u32;
        // SAFETY: this first call only queries the value size and type.  The
        // optional data pointer is intentionally null.
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                name_ptr,
                None,
                Some(&mut value_type),
                None,
                Some(&mut byte_count),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok()?;
        if value_type != REG_SZ && value_type != REG_EXPAND_SZ {
            return Err(ERROR_DATATYPE_MISMATCH.into());
        }
        if byte_count == 0 {
            return Ok(Some(String::new()));
        }

        let mut bytes = vec![0u8; byte_count as usize];
        // SAFETY: `bytes` has exactly the size reported by the first query and
        // remains alive for this synchronous read.
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                name_ptr,
                None,
                Some(&mut value_type),
                Some(bytes.as_mut_ptr()),
                Some(&mut byte_count),
            )
        };
        status.ok()?;
        if byte_count as usize > bytes.len() || !byte_count.is_multiple_of(2) {
            return Err(ERROR_DATATYPE_MISMATCH.into());
        }
        let units = &bytes[..byte_count as usize]
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        String::from_utf16(&units[..end])
            .map(Some)
            .map_err(|_| ERROR_DATATYPE_MISMATCH.into())
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

    /// Deletes one empty direct subkey without recursively touching children.
    /// A concurrent/user-owned value or subkey makes the operation fail rather
    /// than broadening Sakura's deletion scope.
    pub fn delete_empty_subkey(&self, subkey: &str, view: RegistryView) -> Result<()> {
        let path = to_wide_nul(subkey);
        // SAFETY: `self.0` is an open parent key and `path` is NUL terminated
        // storage alive for the synchronous call.
        let status = unsafe { RegDeleteKeyExW(self.0, PCWSTR(path.as_ptr()), view.sam().0, None) };
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
