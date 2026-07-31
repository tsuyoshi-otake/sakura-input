//! Registration for the Sakura Input text service.
//!
//! This crate is the single place that knows what Sakura Input looks like to the
//! operating system: its GUIDs, its registry footprint, and the TSF categories it
//! claims. Both `sakura_regtool.exe` and the DLL's own `DllRegisterServer` route
//! through here, so a machine registered by either route ends up identical.
//!
//! Nothing here runs on the input path. Correctness and reversibility matter;
//! speed does not.

#![cfg(windows)]

pub mod com_server;
pub mod guids;
pub mod launcher;
pub mod module;
pub mod profile;
pub mod registry;
pub mod user_profile;
pub mod wide;

use std::path::Path;

use windows_core::{Error, Result};

pub use guids::{
    format_guid, CATEGORIES, CLSID_SAKURA_TSF, GUID_DISPLAY_ATTRIBUTE_CONVERTED,
    GUID_DISPLAY_ATTRIBUTE_FOCUSED, GUID_DISPLAY_ATTRIBUTE_RAW, GUID_PRESERVEDKEY_IME_OFF,
    GUID_PRESERVEDKEY_IME_ON, GUID_PRESERVEDKEY_IME_TOGGLE, GUID_PROFILE_JA_JP, LANGID_JA_JP,
    TEXT_SERVICE_DESCRIPTION,
};
pub use module::module_file_name;
pub use profile::ComApartment;
pub use registry::RegistryView;

/// Registers the text service end to end.
///
/// `native_dll` is the DLL matching this process's bitness; `wow64_dll` is the
/// x86 build, needed so that 32-bit host applications can load the text service
/// on a 64-bit machine. Passing `None` for it leaves those hosts without an IME,
/// which is a valid choice only for a 32-bit-only or ARM64-only install.
///
/// Steps run in dependency order — the class must exist before TSF is told about
/// a profile that resolves to it.
///
/// Requires an initialized apartment ([`ComApartment`]) and administrator rights.
pub fn register_all(
    native_dll: &Path,
    wow64_dll: Option<&Path>,
    enabled_by_default: bool,
) -> Result<()> {
    com_server::register(native_dll, RegistryView::Native)?;
    if let Some(dll) = wow64_dll {
        com_server::register(dll, RegistryView::Bits32)?;
    }
    profile::register_categories()?;
    profile::register_profile(native_dll, enabled_by_default)?;
    Ok(())
}

/// Removes the text service end to end.
///
/// Ordering is the reverse of registration and is a safety property, not a
/// symmetry preference (DESIGN 12.1): the language profile goes first so no host
/// can activate a text service whose class is about to disappear, and the CLSID
/// entries go last.
///
/// Every step runs even if an earlier one fails, and the first error is returned
/// afterwards. Stopping at the first failure is what leaves a machine with a
/// half-removed IME that neither loads nor uninstalls.
pub fn unregister_all() -> Result<()> {
    let mut first_error: Option<Error> = None;
    let mut record = |result: Result<()>| {
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    };

    record(profile::unregister_profile());
    record(profile::unregister_categories());
    record(com_server::unregister(RegistryView::Bits32));
    record(com_server::unregister(RegistryView::Native));

    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
