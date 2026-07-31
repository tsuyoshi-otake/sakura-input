//! The five entry points Windows knows this DLL by, plus the module-wide state
//! they need: where the DLL was loaded from, and how many live COM objects it
//! is still handing out.

use core::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, E_POINTER, E_UNEXPECTED, HINSTANCE, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows_core::{Interface, GUID, HRESULT};

use crate::class_factory::TextServiceFactory;

// From winnt.h. Pulling in `Win32_System_SystemServices` for two integers would
// widen the binding surface of a DLL that is trying to stay under 1 MB.
const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_PROCESS_DETACH: u32 = 0;

/// The `HINSTANCE` handed to `DllMain`, kept so `DllRegisterServer` can ask the
/// loader where this DLL actually lives rather than guessing a path.
static MODULE_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// Live COM objects. `DllCanUnloadNow` reports "not yet" while this is nonzero;
/// unloading with objects outstanding turns every later call into a jump into
/// freed memory.
static LIVE_OBJECTS: AtomicU32 = AtomicU32::new(0);

/// Returns the module this code was loaded as.
pub fn module_handle() -> HMODULE {
    HMODULE(MODULE_HANDLE.load(Ordering::Relaxed) as *mut c_void)
}

/// Called when a COM object owned by this DLL comes into existence.
pub fn on_object_created() {
    LIVE_OBJECTS.fetch_add(1, Ordering::Relaxed);
}

/// Called when one goes away. Saturates at zero rather than wrapping: an
/// underflow here would report "safe to unload" forever after.
pub fn on_object_destroyed() {
    let _ = LIVE_OBJECTS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
        Some(count.saturating_sub(1))
    });
}

/// # Safety
/// Called by the loader. Must not do anything that takes the loader lock.
#[no_mangle]
pub extern "system" fn DllMain(module: HINSTANCE, reason: u32, _reserved: *mut c_void) -> bool {
    match reason {
        DLL_PROCESS_ATTACH => {
            MODULE_HANDLE.store(module.0 as isize, Ordering::Relaxed);
            // TSF calls in on the thread that activated the text service; we
            // never need per-thread attach notifications, and declining them
            // saves a loader-lock round trip in every thread every host spawns.
            // SAFETY: `module` is the handle the loader just gave us, and this
            // is the documented place to call it.
            let _ = unsafe { DisableThreadLibraryCalls(HMODULE(module.0)) };
        }
        DLL_PROCESS_DETACH => {}
        _ => {}
    }
    true
}

/// Hands COM a class factory for our text service.
///
/// # Safety
/// `rclsid` and `riid` must point to readable GUIDs and `ppv` to a writable
/// interface pointer, as COM guarantees.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    // Clear the out-parameter first: a caller that ignores the HRESULT must not
    // be left holding whatever happened to be in that memory.
    // SAFETY: `ppv` was just checked non-null and COM guarantees it is writable.
    unsafe { *ppv = core::ptr::null_mut() };

    if rclsid.is_null() || riid.is_null() {
        return E_POINTER;
    }
    // SAFETY: both pointers are non-null and COM guarantees they are readable
    // GUIDs for the duration of the call.
    if unsafe { *rclsid } != sakura_reg::CLSID_SAKURA_TSF {
        return CLASS_E_CLASSNOTAVAILABLE;
    }

    let factory: IClassFactory = TextServiceFactory::new().into();
    // SAFETY: `riid` is non-null and readable, `ppv` is non-null and writable.
    unsafe { factory.query(riid, ppv) }
}

/// Reports whether COM may unload this DLL.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if LIVE_OBJECTS.load(Ordering::Acquire) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

/// Registers the text service for the whole machine. Requires elevation.
///
/// The DLL can only see its own path, so this registers a single bitness.
/// `sakura_regtool.exe` is what registers the x64 and x86 DLLs as a pair.
#[no_mangle]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    to_hresult(register())
}

/// Removes the text service registration.
#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    to_hresult(unregister())
}

fn register() -> windows_core::Result<()> {
    let _apartment = sakura_reg::ComApartment::new()?;
    let dll_path = sakura_reg::module_file_name(module_handle())?;
    sakura_reg::register_all(&dll_path, None, true)
}

fn unregister() -> windows_core::Result<()> {
    let _apartment = sakura_reg::ComApartment::new()?;
    sakura_reg::unregister_all()
}

/// Collapses a `Result` into the `HRESULT` an exported COM function must return.
///
/// A failure carrying `S_OK` would tell the caller everything worked, so an
/// error that somehow reports success is rewritten rather than passed on.
fn to_hresult(result: windows_core::Result<()>) -> HRESULT {
    match result {
        Ok(()) => S_OK,
        Err(error) => {
            let code = error.code();
            if code.is_ok() {
                E_UNEXPECTED
            } else {
                code
            }
        }
    }
}
