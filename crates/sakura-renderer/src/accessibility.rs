//! UI Automation surface for the renderer-owned candidate popup.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard};

use sakura_proto::{CandidateDetail, CandidateList};
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

    pub fn update(&self, candidates: &CandidateList, detail: Option<&CandidateDetail>) {
        let name = announcement(candidates, detail);
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

fn announcement(candidates: &CandidateList, detail: Option<&CandidateDetail>) -> String {
    let visible = candidates.visible_range();
    let page = candidates.current_page().saturating_add(1);
    let pages = candidates.page_count();
    let selected = usize::from(candidates.selected).saturating_add(1);
    let mut result = format!(
        "Sakura Input candidates, {} candidates, page {page} of {pages}, selected {selected} of {}.",
        candidate_kind_name(candidates.kind),
        candidates.items.len()
    );
    for global_index in visible {
        let candidate = &candidates.items[global_index];
        result.push(' ');
        result.push_str("Candidate ");
        result.push_str(&(global_index + 1).to_string());
        result.push_str(" of ");
        result.push_str(&candidates.items.len().to_string());
        if global_index == usize::from(candidates.selected) {
            result.push_str(" (selected)");
        }
        result.push_str(": ");
        result.push_str(&candidate.text);
        if !candidate.annotation.is_empty() {
            result.push_str(" — ");
            result.push_str(&candidate.annotation);
        }
        if candidate.deletable_history {
            result.push_str("; learned-history deletion is available from the trash button.");
        }
        result.push('.');
    }
    if let Some(detail) = detail {
        result.push_str(" Detail for selected candidate: ");
        let surface = candidates
            .items
            .get(usize::from(candidates.selected))
            .map(|candidate| candidate.text.as_str())
            .unwrap_or("");
        result.push_str(surface);
        if detail.reading != surface {
            result.push_str(" (reading: ");
            result.push_str(&detail.reading);
            result.push(')');
        }
        result.push_str(". Definition: ");
        // UI Automation receives every definition character carried by the
        // protocol, independent of any work-area ellipsis in the visual popup.
        // When the protocol carries only a bounded preview, the announcement
        // explicitly says that more source text exists.
        result.push_str(&detail.definition);
        if detail.definition_truncated {
            result.push_str(" Definition continues.");
        }
        for (label, group) in [
            ("Aliases", &detail.aliases),
            ("Related", &detail.related),
            ("Similar", &detail.similar),
            ("Antonyms", &detail.antonyms),
        ] {
            if group.is_empty() {
                continue;
            }
            result.push(' ');
            result.push_str(label);
            result.push_str(": ");
            result.push_str(&group.join(", "));
            result.push('.');
        }
    }
    result
}

fn candidate_kind_name(kind: sakura_proto::CandidateKind) -> &'static str {
    match kind {
        sakura_proto::CandidateKind::Conversion => "conversion",
        sakura_proto::CandidateKind::Suggestion => "suggestion",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_proto::types::CandidatePresentation;
    use sakura_proto::{Candidate, CandidateKind};

    #[test]
    fn announcement_exposes_annotation_kind_page_and_selected_position() {
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
                    deletable_history: false,
                })
                .collect(),
            selected: 9,
            page_size: 9,
        };

        let name = announcement(&candidates, None);
        assert!(name.contains("conversion candidates, page 2 of 2, selected 10 of 14"));
        assert!(name.contains("Candidate 10 of 14 (selected): candidate-10 — annotation."));
        assert!(name.contains("Candidate 14 of 14: candidate-14."));
        assert!(!name.contains("candidate-9"));
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
                    deletable_history: false,
                })
                .collect(),
            selected: 1,
            page_size: 9,
        };

        let name = announcement(&candidates, None);
        assert!(name.contains("Candidate 2 of 3 (selected): candidate-2."));
        assert!(!name.contains("candidate-1"));
        assert!(!name.contains("candidate-3"));
    }

    #[test]
    fn deletion_affordance_uses_only_the_typed_engine_capability() {
        let candidates = CandidateList {
            kind: CandidateKind::Suggestion,
            presentation: CandidatePresentation::Expanded,
            items: vec![
                Candidate {
                    text: "annotation-only".to_owned(),
                    annotation: "履歴".to_owned(),
                    deletable_history: false,
                },
                Candidate {
                    text: "engine-marked".to_owned(),
                    annotation: String::new(),
                    deletable_history: true,
                },
            ],
            selected: 0,
            page_size: 9,
        };
        let name = announcement(&candidates, None);
        assert_eq!(
            name.matches("learned-history deletion is available")
                .count(),
            1
        );
        assert!(
            name.contains("Candidate 2 of 2: engine-marked; learned-history deletion is available")
        );
    }

    #[test]
    fn announcement_covers_candidate_semantics_across_kinds_presentations_pages_and_annotations() {
        let kinds = [CandidateKind::Conversion, CandidateKind::Suggestion];
        for kind in kinds {
            for presentation in CandidatePresentation::ALL {
                for annotations_present in [false, true] {
                    for item_count in 1usize..=14 {
                        for page_size in 1usize..=5 {
                            for selected in 0..item_count {
                                let candidates = CandidateList {
                                    kind,
                                    presentation,
                                    items: (0..item_count)
                                        .map(|index| Candidate {
                                            text: format!("surface[{index}]"),
                                            annotation: if annotations_present {
                                                format!("annotation[{index}]")
                                            } else {
                                                String::new()
                                            },
                                            deletable_history: false,
                                        })
                                        .collect(),
                                    selected: selected as u16,
                                    page_size: page_size as u16,
                                };

                                let name = announcement(&candidates, None);
                                let expected_page = selected / page_size + 1;
                                let expected_pages = item_count.div_ceil(page_size);
                                assert!(name.starts_with("Sakura Input candidates,"));
                                assert!(name.contains(&format!(
                                    "{} candidates, page {expected_page} of {expected_pages}, selected {} of {item_count}",
                                    candidate_kind_name(kind),
                                    selected + 1,
                                )));

                                let visible = candidates.visible_range();
                                for index in 0..item_count {
                                    let row = format!("Candidate {} of {item_count}", index + 1);
                                    assert_eq!(
                                        name.contains(&row),
                                        visible.contains(&index),
                                        "{kind:?} {presentation:?}, annotation={annotations_present}, items={item_count}, page_size={page_size}, selected={selected}, index={index}: {name}"
                                    );
                                }

                                let selected_row = format!(
                                    "Candidate {} of {item_count} (selected): surface[{selected}]",
                                    selected + 1
                                );
                                assert!(name.contains(&selected_row));
                                if annotations_present {
                                    for index in visible {
                                        assert!(name.contains(&format!(
                                            "surface[{index}] — annotation[{index}]."
                                        )));
                                    }
                                } else {
                                    assert!(!name.contains(" — "));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn announcement_keeps_the_full_definition_and_only_non_empty_groups() {
        let candidates = CandidateList {
            kind: CandidateKind::Conversion,
            presentation: CandidatePresentation::Compact,
            items: vec![Candidate {
                text: "用語".to_string(),
                annotation: String::new(),
                deletable_history: false,
            }],
            selected: 0,
            page_size: 9,
        };
        let definition =
            "これは画面の二行制限より長い完全な説明です。スクリーンリーダーには省略しません。";
        let detail = CandidateDetail {
            reading: "ようご".to_string(),
            definition: definition.to_string(),
            definition_truncated: false,
            aliases: vec!["別名A".to_string(), "別名B".to_string()],
            related: Vec::new(),
            similar: vec!["類似語".to_string()],
            antonyms: Vec::new(),
        };

        let name = announcement(&candidates, Some(&detail));
        assert!(name.contains("Detail for selected candidate: 用語 (reading: ようご)."));
        assert!(name.contains(definition));
        assert!(name.contains("Aliases: 別名A, 別名B."));
        assert!(name.contains("Similar: 類似語."));
        assert!(!name.contains("Related:"));
        assert!(!name.contains("Antonyms:"));
        assert!(!name.contains("Definition continues."));

        let truncated = CandidateDetail {
            definition: "省略された説明のプレビュー".to_string(),
            definition_truncated: true,
            ..detail
        };
        let truncated_name = announcement(&candidates, Some(&truncated));
        assert!(truncated_name.contains("省略された説明のプレビュー Definition continues."));
    }
}
