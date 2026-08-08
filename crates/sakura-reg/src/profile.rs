//! TSF-side registration: the language profile and the category claims.
//!
//! Unlike the CLSID entries in [`crate::com_server`], these go through TSF's own
//! COM objects rather than the registry directly, so the caller must hold an
//! initialized apartment — see [`ComApartment`].

use std::path::Path;

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, ITfCategoryMgr,
    ITfInputProcessorProfileMgr, TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE, TF_IPPMF_ENABLEPROFILE,
    TF_IPPMF_FORSESSION, TF_PROFILETYPE_INPUTPROCESSOR,
};
use windows_core::Result;

use crate::guids::{
    CATEGORIES, CLSID_SAKURA_TSF, GUID_PROFILE_JA_JP, LANGID_JA_JP, PROFILE_ICON_INDEX,
    TEXT_SERVICE_DESCRIPTION,
};
use crate::wide::{os_to_wide, to_wide};

/// Keeps a single-threaded apartment alive for the duration of registration.
///
/// If the thread already had COM initialized in a different mode the guard
/// borrows that apartment instead of failing — `regsvr32` and the installer both
/// call in with COM already up, and refusing them would be pure friction.
#[derive(Debug)]
pub struct ComApartment {
    owns_initialization: bool,
}

impl ComApartment {
    /// Initializes an STA, or attaches to whatever apartment the thread has.
    pub fn new() -> Result<Self> {
        // SAFETY: no reserved pointer is passed and the return value is checked
        // before any COM object is created on this thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self {
                owns_initialization: false,
            });
        }
        hr.ok()?;
        Ok(Self {
            owns_initialization: true,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns_initialization {
            // SAFETY: balances exactly one successful CoInitializeEx made by
            // this guard on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn profile_manager() -> Result<ITfInputProcessorProfileMgr> {
    // SAFETY: the CLSID is a compile-time constant and the requested interface
    // matches the binding's generic parameter, so windows-rs does the QI.
    unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) }
}

fn category_manager() -> Result<ITfCategoryMgr> {
    // SAFETY: as in `profile_manager`.
    unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) }
}

/// Publishes the ja-JP language profile.
///
/// `dll_path` is used only as the icon source. `enabled_by_default` decides
/// whether the profile joins the user's input list immediately; an installer
/// wants that, a repair pass that is only fixing a broken CLSID entry does not.
pub fn register_profile(dll_path: &Path, enabled_by_default: bool) -> Result<()> {
    let manager = profile_manager()?;
    // These are pointer-plus-count parameters, so the slices must not carry a
    // NUL — the count would include it and the description would render with a
    // trailing box in the language bar.
    let description = to_wide(TEXT_SERVICE_DESCRIPTION);
    let icon_file = os_to_wide(dll_path.as_os_str());

    // SAFETY: both slices outlive the call and their lengths are the character
    // counts the API expects. A null HKL means "no substitute keyboard layout".
    unsafe {
        manager.RegisterProfile(
            &CLSID_SAKURA_TSF,
            LANGID_JA_JP,
            &GUID_PROFILE_JA_JP,
            &description,
            &icon_file,
            PROFILE_ICON_INDEX,
            HKL(std::ptr::null_mut()),
            0,
            enabled_by_default,
            0,
        )
    }
}

/// Makes Sakura the active Japanese text service for this logon session.
///
/// `RegisterProfile` publishes a profile and `InstallLayoutOrTip` adds it to
/// the user's input list, but neither operation changes the TIP that is
/// currently active in existing applications. Without this explicit session
/// activation, the user's half-width/full-width key continues to toggle the
/// previously selected IME and never reaches Sakura's normal TSF key-event path.
/// The caller must be the interactive user, with the COM apartment already
/// initialized; no administrator rights are required.
pub fn activate_profile_for_session() -> Result<()> {
    let manager = profile_manager()?;
    // SAFETY: the manager is a live TSF profile manager and all GUIDs are
    // compile-time constants. A null HKL asks TSF to use the current layout.
    unsafe {
        manager.ActivateProfile(
            TF_PROFILETYPE_INPUTPROCESSOR,
            LANGID_JA_JP,
            &CLSID_SAKURA_TSF,
            &GUID_PROFILE_JA_JP,
            HKL(std::ptr::null_mut()),
            TF_IPPMF_FORSESSION | TF_IPPMF_ENABLEPROFILE | TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE,
        )
    }
}

/// Withdraws the ja-JP language profile.
pub fn unregister_profile() -> Result<()> {
    let manager = profile_manager()?;
    // SAFETY: all three GUIDs are compile-time constants living in static memory.
    unsafe { manager.UnregisterProfile(&CLSID_SAKURA_TSF, LANGID_JA_JP, &GUID_PROFILE_JA_JP, 0) }
}

/// Claims every category in [`CATEGORIES`].
pub fn register_categories() -> Result<()> {
    let manager = category_manager()?;
    for category in CATEGORIES {
        // SAFETY: the CLSID is a static constant and `category` borrows from a
        // 'static slice, so both pointers stay valid across the call. The item
        // GUID is the CLSID itself, which is how a text service claims a
        // category on its own behalf.
        unsafe { manager.RegisterCategory(&CLSID_SAKURA_TSF, category, &CLSID_SAKURA_TSF)? };
    }
    Ok(())
}

/// Releases every category claim, continuing past failures so that one stuck
/// entry cannot strand the rest.
pub fn unregister_categories() -> Result<()> {
    let manager = category_manager()?;
    let mut first_error = None;
    for category in CATEGORIES {
        // SAFETY: as in `register_categories`.
        let result =
            unsafe { manager.UnregisterCategory(&CLSID_SAKURA_TSF, category, &CLSID_SAKURA_TSF) };
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
