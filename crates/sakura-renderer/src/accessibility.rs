//! UI Automation surface for the renderer-owned candidate popup.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard};

use sakura_proto::CandidateList;
use windows::core::{implement, Error, IUnknown, Result};
use windows::Win32::Foundation::{
    E_NOTIMPL, E_UNEXPECTED, HWND, LPARAM, LRESULT, RPC_E_CHANGED_MODE, WPARAM,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    IRawElementProviderSimple, IRawElementProviderSimple_Impl, Polite, ProviderOptions,
    ProviderOptions_ServerSideProvider, ProviderOptions_UseComThreading,
    UIA_AutomationIdPropertyId as UIA_AUTOMATION_ID_PROPERTY_ID,
    UIA_ClassNamePropertyId as UIA_CLASS_NAME_PROPERTY_ID,
    UIA_ControlTypePropertyId as UIA_CONTROL_TYPE_PROPERTY_ID,
    UIA_FrameworkIdPropertyId as UIA_FRAMEWORK_ID_PROPERTY_ID,
    UIA_IsContentElementPropertyId as UIA_IS_CONTENT_ELEMENT_PROPERTY_ID,
    UIA_IsControlElementPropertyId as UIA_IS_CONTROL_ELEMENT_PROPERTY_ID,
    UIA_IsEnabledPropertyId as UIA_IS_ENABLED_PROPERTY_ID,
    UIA_IsKeyboardFocusablePropertyId as UIA_IS_KEYBOARD_FOCUSABLE_PROPERTY_ID,
    UIA_IsOffscreenPropertyId as UIA_IS_OFFSCREEN_PROPERTY_ID, UIA_ListControlTypeId,
    UIA_LiveRegionChangedEventId, UIA_LiveSettingPropertyId as UIA_LIVE_SETTING_PROPERTY_ID,
    UIA_NamePropertyId as UIA_NAME_PROPERTY_ID, UiaDisconnectProvider, UiaHostProviderFromHwnd,
    UiaRaiseAutomationEvent, UiaReturnRawElementProvider, UIA_PATTERN_ID, UIA_PROPERTY_ID,
};
use windows_core::IUnknownImpl;

/// Owns COM initialization for the renderer's window thread.
#[derive(Debug)]
pub struct ComApartment {
    owns_initialization: bool,
}

impl ComApartment {
    pub fn new() -> Result<Self> {
        // SAFETY: called once on the main thread before any provider/window is
        // created. A pre-existing apartment is borrowed, never uninitialized.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self {
                owns_initialization: false,
            });
        }
        result.ok()?;
        Ok(Self {
            owns_initialization: true,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns_initialization {
            // SAFETY: balances this object's one successful CoInitializeEx on
            // the same main thread.
            unsafe { CoUninitialize() };
        }
    }
}

#[derive(Debug)]
struct ProviderState {
    name: String,
    offscreen: bool,
}

impl Default for ProviderState {
    fn default() -> Self {
        Self {
            name: "Sakura Input candidates".to_string(),
            offscreen: true,
        }
    }
}

#[implement(IRawElementProviderSimple)]
#[derive(Debug)]
struct CandidateProvider {
    window: isize,
    state: Arc<Mutex<ProviderState>>,
}

impl CandidateProvider {
    fn state(&self) -> Result<MutexGuard<'_, ProviderState>> {
        self.state
            .lock()
            .map_err(|_| Error::new(E_UNEXPECTED, "candidate accessibility state was poisoned"))
    }

    fn window(&self) -> HWND {
        HWND(self.window as *mut c_void)
    }
}

impl IRawElementProviderSimple_Impl for CandidateProvider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, _pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        // This root is a live announcing surface. Candidate navigation stays
        // keyboard-owned, so it deliberately advertises no control patterns.
        Err(Error::from_hresult(E_NOTIMPL))
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let implementation = self.get_impl();
        match property {
            UIA_NAME_PROPERTY_ID => Ok(VARIANT::from(implementation.state()?.name.as_str())),
            UIA_AUTOMATION_ID_PROPERTY_ID => Ok(VARIANT::from("SakuraInputCandidates")),
            UIA_CLASS_NAME_PROPERTY_ID => Ok(VARIANT::from("SakuraInputCandidates")),
            UIA_FRAMEWORK_ID_PROPERTY_ID => Ok(VARIANT::from("Win32")),
            UIA_CONTROL_TYPE_PROPERTY_ID => Ok(VARIANT::from(UIA_ListControlTypeId.0)),
            UIA_IS_CONTENT_ELEMENT_PROPERTY_ID
            | UIA_IS_CONTROL_ELEMENT_PROPERTY_ID
            | UIA_IS_ENABLED_PROPERTY_ID => Ok(VARIANT::from(true)),
            UIA_IS_KEYBOARD_FOCUSABLE_PROPERTY_ID => Ok(VARIANT::from(false)),
            UIA_IS_OFFSCREEN_PROPERTY_ID => Ok(VARIANT::from(implementation.state()?.offscreen)),
            UIA_LIVE_SETTING_PROPERTY_ID => Ok(VARIANT::from(Polite.0)),
            _ => Ok(VARIANT::default()),
        }
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        // SAFETY: the HWND belongs to the live CandidateWindow that owns this
        // provider, and UI Automation only retains the returned COM interface.
        unsafe { UiaHostProviderFromHwnd(self.get_impl().window()) }
    }
}

/// Stable provider identity retained for the lifetime of one candidate HWND.
#[derive(Debug)]
pub struct CandidateAccessibility {
    provider: IRawElementProviderSimple,
    state: Arc<Mutex<ProviderState>>,
}

impl CandidateAccessibility {
    pub fn new(window: HWND) -> Self {
        let state = Arc::new(Mutex::new(ProviderState::default()));
        let provider = CandidateProvider {
            window: window.0 as isize,
            state: Arc::clone(&state),
        }
        .into();
        Self { provider, state }
    }

    pub fn update(&self, candidates: &CandidateList) {
        let name = announcement(candidates);
        match self.state.lock() {
            Ok(mut state) => {
                state.name = name;
                state.offscreen = false;
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.name = name;
                state.offscreen = false;
            }
        }
        // SAFETY: this retained provider is live and the event contains no
        // borrowed payload. No listener is also a successful no-op.
        unsafe {
            let _ = UiaRaiseAutomationEvent(&self.provider, UIA_LiveRegionChangedEventId);
        }
    }

    pub fn hide(&self) {
        match self.state.lock() {
            Ok(mut state) => state.offscreen = true,
            Err(poisoned) => poisoned.into_inner().offscreen = true,
        }
    }

    pub fn return_provider(&self, window: HWND, w: WPARAM, l: LPARAM) -> LRESULT {
        // SAFETY: called directly from this HWND's WM_GETOBJECT handler and
        // forwards the unmodified message parameters as required by UIA.
        unsafe { UiaReturnRawElementProvider(window, w, l, &self.provider) }
    }

    pub fn disconnect(&self) {
        // SAFETY: this is called once while the HWND and provider are still
        // alive, immediately before the window is destroyed.
        unsafe {
            let _ = UiaDisconnectProvider(&self.provider);
        }
    }
}

fn announcement(candidates: &CandidateList) -> String {
    let visible = candidates.visible_range();
    let page_start = candidates.current_page_range().start;
    let page = candidates.current_page().saturating_add(1);
    let pages = candidates.page_count();
    let selected = usize::from(candidates.selected).saturating_add(1);
    let mut result = format!(
        "Sakura Input candidates, page {page} of {pages}, selected {selected} of {}.",
        candidates.items.len()
    );
    for global_index in visible {
        let candidate = &candidates.items[global_index];
        result.push(' ');
        result.push_str(&(global_index.saturating_sub(page_start) + 1).to_string());
        result.push_str(". ");
        result.push_str(&candidate.text);
        result.push('.');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_proto::types::CandidatePresentation;
    use sakura_proto::{Candidate, CandidateKind};

    #[test]
    fn announcement_contains_only_the_visible_page_and_selected_position_without_annotations() {
        let candidates = CandidateList {
            kind: CandidateKind::Conversion,
            presentation: CandidatePresentation::Expanded,
            items: (1..=14)
                .map(|index| Candidate {
                    text: format!("candidate-{index}"),
                    annotation: if index == 10 {
                        "annotation".to_string()
                    } else {
                        String::new()
                    },
                })
                .collect(),
            selected: 9,
            page_size: 9,
        };

        let name = announcement(&candidates);
        assert!(name.contains("page 2 of 2, selected 10 of 14"));
        assert!(name.contains("1. candidate-10."));
        assert!(name.contains("5. candidate-14."));
        assert!(!name.contains("candidate-9."));
        assert!(!name.contains("annotation"));
    }

    #[test]
    fn compact_conversion_announcement_contains_only_the_selected_row() {
        let candidates = CandidateList {
            kind: CandidateKind::Conversion,
            presentation: CandidatePresentation::Compact,
            items: (1..=3)
                .map(|index| Candidate {
                    text: format!("candidate-{index}"),
                    annotation: String::new(),
                })
                .collect(),
            selected: 1,
            page_size: 9,
        };

        let name = announcement(&candidates);
        assert!(name.contains("2. candidate-2."));
        assert!(!name.contains("candidate-1."));
        assert!(!name.contains("candidate-3."));
    }
}
