//! Locating the loaded module on disk.
//!
//! `DllRegisterServer` has to write its own path into the registry, and the only
//! authority on where a DLL was loaded from is the loader.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_core::{Error, Result};

/// Windows caps a path at 32767 characters even with long paths enabled, so a
/// buffer that reaches this size and still truncates means something is wrong
/// rather than merely large.
const MAX_PATH_UNITS: usize = 32768;

/// Returns the full path of a loaded module, growing the buffer until it fits.
///
/// `GetModuleFileNameW` reports truncation by filling the buffer exactly and
/// returning its size, which is indistinguishable from an exact fit — so the
/// loop treats "filled exactly" as truncated and retries larger.
pub fn module_file_name(module: HMODULE) -> Result<PathBuf> {
    let mut buffer = vec![0u16; 512];
    loop {
        // SAFETY: `buffer` is a live, writable slice and its length is passed
        // through by the binding, so the API cannot write past it.
        let written = unsafe { GetModuleFileNameW(Some(module), &mut buffer) } as usize;
        if written == 0 {
            return Err(Error::from_thread());
        }
        if written < buffer.len() {
            buffer.truncate(written);
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
        if buffer.len() >= MAX_PATH_UNITS {
            return Err(Error::from_thread());
        }
        buffer.resize(buffer.len() * 2, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A null module means the running executable, which is the test binary.
    #[test]
    fn resolves_the_current_executable() {
        let path = module_file_name(HMODULE::default()).expect("module path");
        assert!(
            path.is_absolute(),
            "expected an absolute path, got {path:?}"
        );
        assert!(path.exists(), "reported path does not exist: {path:?}");
    }
}
