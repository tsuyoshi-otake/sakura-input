//! The five entry points Windows knows this DLL by, plus the module-wide state
//! they need: where the DLL was loaded from, and how many live COM objects it
//! is still handing out.

use core::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::OnceLock;

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, E_POINTER, E_UNEXPECTED, HINSTANCE, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_PIN,
};
use windows_core::{Interface, GUID, HRESULT, PCWSTR};

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

/// Result of the one-time module pin, kept so `DllGetClassObject` can replay
/// the same outcome (including the failure HRESULT) on every later call.
static MODULE_PIN: OnceLock<HRESULT> = OnceLock::new();

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
    get_class_object(rclsid, riid, ppv, ensure_module_pinned)
}

/// The body of `DllGetClassObject`, with the module pin behind `pin` so
/// tests can drive the failure path without kernel fault injection (D2).
fn get_class_object(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
    pin: impl FnOnce() -> HRESULT,
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

    // Pin the module before handing out anything that can register the
    // deferred-window class. The class's wndproc is process-wide state that
    // survives `CoFreeUnusedLibraries`, so an unloadable DLL would leave a
    // dangling procedure pointer for the next `versions\...\sakura_tsf.dll`
    // this host loads (issue #360, A2). Fail closed: an unpinned DLL must
    // not create COM objects that register the class.
    let pin_result = pin();
    if pin_result.is_err() {
        return pin_result;
    }

    let factory: IClassFactory = TextServiceFactory::new().into();
    // SAFETY: `riid` is non-null and readable, `ppv` is non-null and writable.
    unsafe { factory.query(riid, ppv) }
}

/// Pins this DLL into the process exactly once.
///
/// `GetModuleHandleExW` with `FLAG_PIN` raises the module's reference count
/// to a value the loader never decrements, so the DLL stays mapped until
/// process exit no matter what `DllCanUnloadNow` or `CoFreeUnusedLibraries`
/// decide. It is deliberately called here, on the first real class-object
/// request, and never inside `DllMain` (loader lock). The outcome is cached:
/// a first-call failure is replayed, not retried, so the pin remains a
/// once-per-process operation.
fn ensure_module_pinned() -> HRESULT {
    *MODULE_PIN.get_or_init(|| {
        let mut module = HMODULE::default();
        // SAFETY: with FROM_ADDRESS, the loader interprets the address of a
        // function inside this DLL as a module reference, not a string. PIN
        // asks it to keep the module mapped for the rest of the process.
        match unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                PCWSTR(ensure_module_pinned as *const () as *const u16),
                &mut module,
            )
        } {
            Ok(()) => S_OK,
            Err(error) => pin_error_hresult(error.code()),
        }
    })
}

/// A failed pin must never be reported as success. `Error::code()` normally
/// carries a real failure HRESULT, but a `GetModuleHandleExW` failure with
/// no thread last-error would surface `S_OK`; remap any non-failure code to
/// `E_UNEXPECTED` so `DllGetClassObject` stays fail closed (D2).
fn pin_error_hresult(code: HRESULT) -> HRESULT {
    if code.is_err() {
        code
    } else {
        E_UNEXPECTED
    }
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

// `expect` and `panic!` are denied for this crate because it is loaded into
// applications that are not ours to crash. Test code is not loaded into
// anything, and a test that cannot fail loudly is not a test.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use windows::Win32::Foundation::E_ACCESSDENIED;

    /// D2: a failed pin returns its exact HRESULT and hands out no factory.
    #[test]
    fn class_object_fails_closed_when_the_module_pin_fails() {
        // A non-null sentinel proves the body clears the out-pointer before
        // failing, not merely that it never wrote.
        let mut ppv: *mut c_void = std::ptr::dangling_mut::<c_void>();
        let hr = get_class_object(
            &sakura_reg::CLSID_SAKURA_TSF,
            &IClassFactory::IID,
            &mut ppv,
            || E_ACCESSDENIED,
        );
        assert_eq!(hr, E_ACCESSDENIED);
        assert!(ppv.is_null(), "no factory may escape a failed pin");
    }

    /// D1 companion: a successful pin hands out a real class factory.
    #[test]
    fn class_object_returns_a_factory_when_the_pin_succeeds() {
        let mut ppv: *mut c_void = core::ptr::null_mut();
        let hr = get_class_object(
            &sakura_reg::CLSID_SAKURA_TSF,
            &IClassFactory::IID,
            &mut ppv,
            || S_OK,
        );
        assert_eq!(hr, S_OK);
        assert!(!ppv.is_null(), "a factory must be returned");
        // SAFETY: `ppv` is a live IClassFactory returned add-ref'd; dropping
        // the wrapper releases exactly that reference.
        drop(unsafe { IClassFactory::from_raw(ppv) });
    }

    /// A foreign CLSID is rejected before the pin is even consulted.
    #[test]
    fn class_object_rejects_other_clsids_without_pinning() {
        let pin_called = Cell::new(false);
        let mut ppv: *mut c_void = core::ptr::null_mut();
        let unknown = GUID::from_u128(0x9aabbccd_0011_2233_4455_66778899aabb);
        let hr = get_class_object(&unknown, &IClassFactory::IID, &mut ppv, || {
            pin_called.set(true);
            S_OK
        });
        assert_eq!(hr, CLASS_E_CLASSNOTAVAILABLE);
        assert!(!pin_called.get(), "pin must not run for a foreign CLSID");
        assert!(ppv.is_null());
    }

    /// D2 belt: a pin failure that somehow reports a non-failure code is
    /// rewritten to E_UNEXPECTED, never to S_OK.
    #[test]
    fn pin_error_mapping_never_reports_success() {
        assert_eq!(pin_error_hresult(S_OK), E_UNEXPECTED);
        assert_eq!(pin_error_hresult(S_FALSE), E_UNEXPECTED);
        assert_eq!(pin_error_hresult(E_ACCESSDENIED), E_ACCESSDENIED);
    }
}
