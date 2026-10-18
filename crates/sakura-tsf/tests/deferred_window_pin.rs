#![cfg(windows)]
#![allow(clippy::expect_used, clippy::panic, clippy::undocumented_unsafe_blocks)]

//! Issue #360 A2 contract D1/D2: the real `sakura_tsf.dll` must pin itself
//! into the host process on the first `DllGetClassObject(CLSID_SAKURA_TSF)`,
//! so the process-wide deferred-window class never points at an unloaded
//! wndproc. This test loads the built DLL itself — never TSF/COM — in a
//! dedicated test binary so the pinned module cannot leak into other tests.
//!
//! Fail-before evidence: on the pre-fix code the phase-2 assertion fails
//! because `FreeLibrary` actually unloads the DLL.

use std::ffi::c_void;
use std::path::PathBuf;

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GetProcAddress, LoadLibraryW, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows_core::{s, HRESULT};

/// `DllGetClassObject` as exported by `sakura_tsf.dll`.
type DllGetClassObjectFn = unsafe extern "system" fn(
    *const windows_core::GUID,
    *const windows_core::GUID,
    *mut *mut c_void,
) -> HRESULT;

/// The test executable lives in `target/<triple>/<profile>/deps/`; the
/// cdylib sits one level up. `cargo test` never builds a cdylib lib target
/// (nothing links it), so the test builds it itself — otherwise it would be
/// asserting on a stale or absent DLL. Only the library is built
/// (`cargo build -p sakura-tsf --lib --locked`, matching CI's `--locked`):
/// building the package's bins too would unify features differently from
/// the outer `cargo test --workspace --features ...` run.
fn locate_sakura_tsf_dll() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = std::process::Command::new(cargo)
        .args(["build", "-p", "sakura-tsf", "--lib", "--locked"])
        .env("CARGO_HTTP_CHECK_REVOKE", "false")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("spawn cargo build -p sakura-tsf");
    assert!(status.success(), "cargo build -p sakura-tsf failed");

    let exe = std::env::current_exe().expect("current exe");
    let profile_dir = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile dir");
    let dll = profile_dir.join("sakura_tsf.dll");
    assert!(
        dll.is_file(),
        "sakura_tsf.dll not found at {} after cargo build",
        dll.display()
    );
    dll
}

fn wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn module_handle_if_loaded(path: &std::path::Path) -> Option<HMODULE> {
    let name = wide(path);
    let mut module = HMODULE::default();
    // SAFETY: UNCHANGED_REFCOUNT queries without loading or pinning; `name`
    // is null-terminated and `module` is writable.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(name.as_ptr()),
            &mut module,
        )
    }
    .ok()
    .map(|_| module)
}

#[test]
fn dll_get_class_object_pins_the_module_until_process_exit() {
    let dll = locate_sakura_tsf_dll();

    // Negative control: a plain LoadLibrary/FreeLibrary pair — no
    // DllGetClassObject call — must still unload the module. This proves the
    // pin below comes from DllGetClassObject and not from LoadLibrary itself.
    let control =
        unsafe { LoadLibraryW(PCWSTR(wide(&dll).as_ptr())) }.expect("control LoadLibrary");
    unsafe { FreeLibrary(control) }.expect("control FreeLibrary");
    assert!(
        module_handle_if_loaded(&dll).is_none(),
        "control: module must unload without the pin"
    );

    // D1: after one DllGetClassObject, FreeLibrary must not unload the DLL.
    let module = unsafe { LoadLibraryW(PCWSTR(wide(&dll).as_ptr())) }.expect("LoadLibrary");
    let address = unsafe { GetProcAddress(module, s!("DllGetClassObject")) }
        .expect("DllGetClassObject export");
    let get_class_object: DllGetClassObjectFn = unsafe { std::mem::transmute(address) };

    let mut raw: *mut c_void = std::ptr::null_mut();
    // SAFETY: both GUIDs are live for the call and `raw` is a writable
    // out-pointer, matching the COM contract for this export.
    let hr =
        unsafe { get_class_object(&sakura_reg::CLSID_SAKURA_TSF, &IClassFactory::IID, &mut raw) };
    assert!(hr.is_ok(), "DllGetClassObject failed: {hr:?}");
    assert!(!raw.is_null(), "no class factory returned");
    // SAFETY: `raw` is a live IClassFactory returned add-ref'd by the export;
    // dropping the wrapper releases exactly that reference.
    let factory = unsafe { IClassFactory::from_raw(raw) };
    drop(factory);

    unsafe { FreeLibrary(module) }.expect("FreeLibrary");
    // A leaked +1 refcount (PIN flag dropped) survives one FreeLibrary but
    // not an over-release: only a real pin keeps the module mapped after
    // the count would have gone negative. Results are ignored; on an
    // unpinned build the first free already unmapped the DLL and these are
    // harmless ERROR_INVALID_HANDLE failures.
    for _ in 0..3 {
        let _ = unsafe { FreeLibrary(module) };
    }
    assert!(
        module_handle_if_loaded(&dll).is_some(),
        "pin: module must remain loaded after over-release"
    );
}
