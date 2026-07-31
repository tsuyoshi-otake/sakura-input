//! The `IClassFactory` COM asks for before it will make a text service.

use core::ffi::c_void;

use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_POINTER};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows_core::{implement, Error, IUnknown, Interface, Ref, GUID};

use crate::exports::{on_object_created, on_object_destroyed};
use crate::text_service::TextService;

/// Produces [`TextService`] instances. Stateless — every host gets its own
/// text service, and the factory exists only to satisfy COM's protocol.
#[implement(IClassFactory)]
#[derive(Debug)]
pub struct TextServiceFactory;

impl TextServiceFactory {
    pub fn new() -> Self {
        on_object_created();
        Self
    }
}

impl Drop for TextServiceFactory {
    fn drop(&mut self) {
        on_object_destroyed();
    }
}

impl IClassFactory_Impl for TextServiceFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> windows_core::Result<()> {
        if ppvobject.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: `ppvobject` was just checked non-null and COM guarantees it is
        // writable for an interface pointer.
        unsafe { *ppvobject = core::ptr::null_mut() };

        if riid.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // A text service owns a per-thread conversation with TSF, so it cannot
        // be a piece of somebody else's aggregate identity.
        if !punkouter.is_null() {
            return Err(Error::from_hresult(CLASS_E_NOAGGREGATION));
        }

        let service: IUnknown = TextService::new().into();
        // SAFETY: `riid` is non-null and readable, `ppvobject` is non-null and
        // writable; `query` writes the out-parameter only on success.
        unsafe { service.query(riid, ppvobject).ok() }
    }

    /// COM's "keep the DLL loaded" hint. The live-object count already keeps
    /// the DLL alive for as long as anything can call into it, so a lock is
    /// tracked the same way rather than through a second, parallel mechanism.
    fn LockServer(&self, flock: windows_core::BOOL) -> windows_core::Result<()> {
        if flock.as_bool() {
            on_object_created();
        } else {
            on_object_destroyed();
        }
        Ok(())
    }
}
