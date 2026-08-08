//! Adding and removing the text service from *this user's* input list.
//!
//! Machine registration ([`crate::register_all`]) makes Sakura Input
//! installable; it does not put it in anybody's language bar. That second
//! step is per user, lives in HKCU, and is the reason `--enable-profile`
//! exists separately from `--register` (DESIGN 12.2).
//!
//! # Why this loads a DLL by hand
//!
//! The list Windows shows under Settings > Language > Keyboards is not a
//! registry key with a documented layout — it is HKCU state maintained by
//! `input.dll`, and writing the keys directly produces an entry that looks
//! right and does not survive a language change or a feature update.
//! `InstallLayoutOrTip` is the function that maintains it. It is exported
//! by name and has been stable since Windows Vista, but it appears in no
//! import library and no SDK header, so it is resolved at run time. That
//! is a deliberate trade: an undocumented export that every IME vendor
//! depends on, versus hand-editing state whose invariants we would be
//! guessing at.
//!
//! Resolution failing is reported as an error rather than ignored — an
//! install that silently does not appear in the language bar is the worst
//! outcome, because the user has no way to tell it from a broken IME.

use windows::Win32::Foundation::{FreeLibrary, E_INVALIDARG, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_core::{Error, Result, BOOL, PCSTR, PCWSTR};

use crate::guids::{format_guid, CLSID_SAKURA_TSF, GUID_PROFILE_JA_JP, LANGID_JA_JP};
use crate::wide::to_wide_nul;

/// Remove the profile from the user's list instead of adding it.
const ILOT_UNINSTALL: u32 = 0x0000_0001;

type InstallLayoutOrTipFn = unsafe extern "system" fn(PCWSTR, u32) -> BOOL;

/// The profile in the form `input.dll` parses: `langid:{clsid}{guid}`.
///
/// The language id is four lowercase hex digits and the two GUIDs are
/// brace-delimited and run together with no separator. That is not a
/// format anyone would guess, and getting it subtly wrong (uppercase
/// language id, a separator between the GUIDs) fails silently — the call
/// returns success and the profile does not appear.
pub fn profile_spec() -> String {
    format!(
        "{:04x}:{}{}",
        LANGID_JA_JP,
        format_guid(&CLSID_SAKURA_TSF),
        format_guid(&GUID_PROFILE_JA_JP)
    )
}

/// Adds Sakura Input to the calling user's input methods and activates it for
/// the current logon session.
///
/// Must run as the interactive user. Under an elevated installer token
/// HKCU is a *different hive*, so this would enable the IME for the
/// wrong account while reporting success (DESIGN 12.2) — the caller is
/// responsible for getting that right, because from inside the process
/// there is no reliable way to tell.
pub fn enable() -> Result<()> {
    install(&profile_spec(), 0)?;
    crate::profile::activate_profile_for_session()
}

/// Removes Sakura Input from the calling user's input methods.
///
/// Runs before machine unregistration, so that no user is left with a
/// language-bar entry pointing at a class that no longer exists.
pub fn disable() -> Result<()> {
    install(&profile_spec(), ILOT_UNINSTALL)
}

fn install(spec: &str, flags: u32) -> Result<()> {
    let library = Library::load("input.dll")?;
    // SAFETY: the name is resolved from `input.dll`, whose
    // `InstallLayoutOrTip` has this signature on every Windows version
    // this project supports. `library` outlives the call.
    let install: InstallLayoutOrTipFn =
        unsafe { core::mem::transmute(library.symbol(c"InstallLayoutOrTip")?) };

    let wide = to_wide_nul(spec);
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let ok = unsafe { install(PCWSTR(wide.as_ptr()), flags) };
    if ok.as_bool() {
        Ok(())
    } else {
        // The function reports failure as a plain FALSE and does not
        // always set a last error, so a thread-error read can produce a
        // stale success code. A fixed, honest error beats a misleading
        // specific one.
        Err(Error::from_hresult(E_INVALIDARG))
    }
}

/// A module handle that unloads itself.
struct Library(HMODULE);

impl Library {
    fn load(name: &str) -> Result<Self> {
        let wide = to_wide_nul(name);
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        let module = unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }?;
        Ok(Library(module))
    }

    /// `FARPROC` is an `Option<fn>`; unwrapping it here means the caller
    /// never transmutes a null.
    fn symbol(&self, name: &core::ffi::CStr) -> Result<unsafe extern "system" fn() -> isize> {
        // SAFETY: the module handle is live and the name is
        // NUL-terminated by construction.
        unsafe { GetProcAddress(self.0, PCSTR(name.as_ptr().cast())) }
            .ok_or_else(Error::from_thread)
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: balances exactly one successful LoadLibraryW.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact string `input.dll` expects. Written out in full rather
    /// than rebuilt from the same helpers the code uses, so that a change
    /// to either GUID shows up here as a failing test instead of a
    /// silently regenerated expectation.
    #[test]
    fn the_profile_spec_has_the_shape_input_dll_parses() {
        assert_eq!(
            profile_spec(),
            "0411:{C18F44DE-39E0-4B16-8D28-D5DE35BB11BC}\
             {8466B5F0-210F-408B-A3FE-8D18ECBA711D}"
        );
    }

    #[test]
    fn the_language_id_is_lowercase_and_four_digits() {
        let spec = profile_spec();
        let (langid, rest) = spec.split_once(':').expect("a language id and a profile");
        assert_eq!(langid, "0411");
        assert!(rest.starts_with('{') && rest.ends_with('}'));
        assert_eq!(rest.matches('{').count(), 2, "two GUIDs, no separator");
    }

    /// If this ever fails, every IME registration path in the project is
    /// broken and the error would otherwise surface as "the profile does
    /// not appear" with no explanation.
    #[test]
    fn install_layout_or_tip_is_still_exported() {
        let library = Library::load("input.dll").expect("input.dll is a system component");
        assert!(library.symbol(c"InstallLayoutOrTip").is_ok());
    }
}
