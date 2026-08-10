//! TSF's UI-less candidate contract.
//!
//! The renderer owns the visible popup, but applications that suppress TIP UI
//! still need candidate data through `ITfCandidateListUIElement`. This module
//! presents the same protocol list to TSF and records whether `BeginUIElement`
//! told the external renderer to show itself.

use std::cell::RefCell;
use std::rc::Rc;

use sakura_proto::{CandidateList, MAX_CANDIDATES};
use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, E_UNEXPECTED};
use windows::Win32::UI::TextServices::{
    ITfCandidateListUIElement, ITfCandidateListUIElement_Impl, ITfContext, ITfDocumentMgr,
    ITfThreadMgr, ITfUIElement, ITfUIElementMgr, ITfUIElement_Impl, TF_CLUIE_COUNT,
    TF_CLUIE_CURRENTPAGE, TF_CLUIE_DOCUMENTMGR, TF_CLUIE_PAGEINDEX, TF_CLUIE_SELECTION,
    TF_CLUIE_STRING,
};
use windows_core::{implement, Error, IUnknownImpl, Interface, Result, BOOL, BSTR, GUID};

/// Stable identity reported to UI-less hosts for Sakura's candidate element.
const CANDIDATE_UI_GUID: GUID = GUID::from_u128(0x7f92c392_25d9_4c84_9b84_1bada4690d7a);

#[derive(Debug)]
struct ElementState {
    candidates: CandidateList,
    shown: bool,
}

#[implement(ITfCandidateListUIElement)]
#[derive(Debug)]
struct CandidateElement {
    document: ITfDocumentMgr,
    state: Rc<RefCell<ElementState>>,
}

impl CandidateElement {
    fn state(&self) -> Result<std::cell::Ref<'_, ElementState>> {
        self.state
            .try_borrow()
            .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant candidate UI read"))
    }
}

impl ITfUIElement_Impl for CandidateElement_Impl {
    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from("Sakura Input candidates"))
    }

    fn GetGUID(&self) -> Result<GUID> {
        Ok(CANDIDATE_UI_GUID)
    }

    fn Show(&self, show: BOOL) -> Result<()> {
        let mut state = self
            .get_impl()
            .state
            .try_borrow_mut()
            .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant candidate UI update"))?;
        state.shown = show.as_bool();
        Ok(())
    }

    fn IsShown(&self) -> Result<BOOL> {
        Ok(self.get_impl().state()?.shown.into())
    }
}

impl ITfCandidateListUIElement_Impl for CandidateElement_Impl {
    fn GetUpdatedFlags(&self) -> Result<u32> {
        Ok(TF_CLUIE_DOCUMENTMGR
            | TF_CLUIE_COUNT
            | TF_CLUIE_SELECTION
            | TF_CLUIE_STRING
            | TF_CLUIE_PAGEINDEX
            | TF_CLUIE_CURRENTPAGE)
    }

    fn GetDocumentMgr(&self) -> Result<ITfDocumentMgr> {
        Ok(self.get_impl().document.clone())
    }

    fn GetCount(&self) -> Result<u32> {
        u32::try_from(self.get_impl().state()?.candidates.items.len())
            .map_err(|_| Error::from_hresult(E_INVALIDARG))
    }

    fn GetSelection(&self) -> Result<u32> {
        Ok(u32::from(self.get_impl().state()?.candidates.selected))
    }

    fn GetString(&self, index: u32) -> Result<BSTR> {
        let index = usize::try_from(index).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
        let state = self.get_impl().state()?;
        let candidate = state
            .candidates
            .items
            .get(index)
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        Ok(BSTR::from(candidate.text.as_str()))
    }

    fn GetPageIndex(&self, indices: *mut u32, size: u32, page_count: *mut u32) -> Result<()> {
        if page_count.is_null() || (size > 0 && indices.is_null()) {
            return Err(Error::from_hresult(E_POINTER));
        }
        let state = self.get_impl().state()?;
        let count = state.candidates.page_count();
        let count_u32 = u32::try_from(count).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
        // SAFETY: both pointers were checked for the elements written. TSF
        // owns the buffers and keeps them valid for this call.
        unsafe {
            page_count.write(count_u32);
            for page in 0..count.min(size as usize) {
                let start = state
                    .candidates
                    .page_start(page)
                    .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
                indices
                    .add(page)
                    .write(u32::try_from(start).map_err(|_| Error::from_hresult(E_INVALIDARG))?);
            }
        }
        Ok(())
    }

    fn SetPageIndex(&self, indices: *const u32, page_count: u32) -> Result<()> {
        if page_count == 0 || indices.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let state = self.get_impl().state()?;
        if usize::try_from(page_count).ok() != Some(state.candidates.page_count()) {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        // SAFETY: the pointer is non-null and `page_count` promises the
        // corresponding number of readable elements for this call.
        for page in 0..page_count as usize {
            // SAFETY: the caller's declared count covers this offset and the
            // pointer was checked for null above.
            let supplied = unsafe { indices.add(page).read() };
            let expected = state
                .candidates
                .page_start(page)
                .and_then(|start| u32::try_from(start).ok())
                .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
            if supplied != expected {
                return Err(Error::from_hresult(E_INVALIDARG));
            }
        }
        Ok(())
    }

    fn GetCurrentPage(&self) -> Result<u32> {
        u32::try_from(self.get_impl().state()?.candidates.current_page())
            .map_err(|_| Error::from_hresult(E_INVALIDARG))
    }
}

/// Owns one active TSF UI element and its exact end path.
#[derive(Debug, Default)]
pub struct CandidateUi {
    active: Option<ActiveCandidateUi>,
}

/// The exact host-owned element state that must remain available until
/// `EndUIElement` has run.  In particular, a post-`BeginUIElement` authority
/// check may fail after TSF accepted the element, so this record is installed
/// before that check can return an error.
#[derive(Debug)]
struct ActiveCandidateUi {
    manager: ITfUIElementMgr,
    /// Retains the COM implementation for the lifetime of the manager id.
    _element: ITfCandidateListUIElement,
    state: Rc<RefCell<ElementState>>,
    id: u32,
}

/// Executes one independently re-entrant candidate host call.  Authority is
/// checked both immediately before the call and after it returns; the latter
/// prevents an earlier callback from allowing later host calls to run after a
/// lifecycle boundary has changed the UI owner.
fn checked_candidate_host_call<T>(
    authority: &mut dyn FnMut() -> Result<()>,
    host_call: impl FnOnce() -> Result<T>,
) -> Result<T> {
    authority()?;
    let value = host_call()?;
    authority()?;
    Ok(value)
}

/// Runs the discovery calls used by the production candidate-UI creation path.
/// Keeping the exact sequence here makes it possible to fault-inject lifecycle
/// invalidation without constructing TSF COM objects in a unit test.
fn prepare_candidate_host_calls<Manager, Document, Element, Base>(
    authority: &mut dyn FnMut() -> Result<()>,
    get_manager: impl FnOnce() -> Result<Manager>,
    get_document: impl FnOnce() -> Result<Document>,
    make_element: impl FnOnce(Document) -> Element,
    get_base: impl FnOnce(&Element) -> Result<Base>,
) -> Result<(Manager, Element, Base)> {
    let manager = checked_candidate_host_call(authority, get_manager)?;
    let document = checked_candidate_host_call(authority, get_document)?;
    let element = make_element(document);
    let base = checked_candidate_host_call(authority, || get_base(&element))?;
    Ok((manager, element, base))
}

/// Begins a host UI element while preserving the exact cleanup owner if the
/// post-call authority check discovers revocation.  `BeginUIElement` has no
/// compensating automatic end path, so dropping a successful result here would
/// leave TSF with an active element that Sakura can no longer end.
fn checked_candidate_begin<T, R>(
    slot: &mut Option<T>,
    authority: &mut dyn FnMut() -> Result<()>,
    begin: impl FnOnce() -> Result<(T, R)>,
) -> Result<R> {
    authority()?;
    let (active, result) = begin()?;
    *slot = Some(active);
    authority()?;
    Ok(result)
}

impl CandidateUi {
    /// Begins or updates the element and returns whether the external
    /// renderer is permitted to draw it.
    pub fn show_or_update(
        &mut self,
        thread_mgr: &ITfThreadMgr,
        context: &ITfContext,
        candidates: &CandidateList,
        authority: &mut dyn FnMut() -> Result<()>,
    ) -> Result<bool> {
        if candidates.items.is_empty()
            || candidates.items.len() > MAX_CANDIDATES
            || usize::from(candidates.selected) >= candidates.items.len()
            || candidates.page_size == 0
        {
            return Err(Error::from_hresult(E_INVALIDARG));
        }

        if let Some(active) = &self.active {
            authority()?;
            {
                let mut state = active
                    .state
                    .try_borrow_mut()
                    .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant candidate UI update"))?;
                state.candidates = candidates.clone();
            }
            // SAFETY: the id was issued by this retained manager and remains
            // live until `end` calls `EndUIElement`.
            let id = active.id;
            checked_candidate_host_call(authority, || {
                // SAFETY: the id was issued by this retained manager and stays
                // live until `end` removes the complete active record.
                unsafe { active.manager.UpdateUIElement(id) }
            })?;
            return self.renderer_visible();
        }

        let state = Rc::new(RefCell::new(ElementState {
            candidates: candidates.clone(),
            shown: true,
        }));
        let (manager, element, base): (ITfUIElementMgr, ITfCandidateListUIElement, ITfUIElement) =
            prepare_candidate_host_calls(
                authority,
                || thread_mgr.cast(),
                // SAFETY: the context is live for this lease-owned candidate
                // operation, and the authority gate brackets the host call.
                || unsafe { context.GetDocumentMgr() },
                |document| {
                    CandidateElement {
                        document,
                        state: Rc::clone(&state),
                    }
                    .into()
                },
                |element: &ITfCandidateListUIElement| element.cast(),
            )?;
        let shown = checked_candidate_begin(&mut self.active, authority, || {
            let mut show: BOOL = true.into();
            let mut id = 0;
            // SAFETY: all out-pointers are valid locals and `base` remains live
            // in the retained active state after the call succeeds.
            unsafe { manager.BeginUIElement(&base, &mut show, &mut id)? };
            Ok((
                ActiveCandidateUi {
                    manager,
                    _element: element,
                    state,
                    id,
                },
                show.as_bool(),
            ))
        })?;
        self.active
            .as_ref()
            .ok_or_else(|| Error::new(E_UNEXPECTED, "candidate UI begin lost its cleanup owner"))?
            .state
            .try_borrow_mut()
            .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant candidate UI update"))?
            .shown = shown;
        Ok(shown)
    }

    pub fn renderer_visible(&self) -> Result<bool> {
        match &self.active {
            Some(active) => Ok(active
                .state
                .try_borrow()
                .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant candidate UI read"))?
                .shown),
            None => Ok(false),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Ends the element exactly once. State is cleared before calling TSF so
    /// re-entrant callbacks observe the terminal inactive state.
    pub fn end(&mut self) -> Result<()> {
        match self.active.take() {
            Some(active) => {
                // SAFETY: `id` was issued by this manager and has not been
                // ended before; the fields were taken above.
                unsafe { active.manager.EndUIElement(active.id) }
            }
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use sakura_proto::{Candidate, CANDIDATE_PAGE_SIZE};
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::TextServices::CLSID_TF_ThreadMgr;

    #[derive(Debug)]
    struct ComApartment {
        owns_initialization: bool,
    }

    impl ComApartment {
        fn new() -> Result<Self> {
            // SAFETY: the test initializes COM before creating any TSF object
            // on this thread and balances a successful call in `Drop`.
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
                // SAFETY: this balances this guard's successful COM
                // initialization on the same test thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum HostCall {
        ThreadManagerCast,
        GetDocumentManager,
        ElementCast,
    }

    #[test]
    fn invalidation_during_first_candidate_call_suppresses_later_calls() {
        let invalidated = Cell::new(false);
        let trace = RefCell::new(Vec::new());
        let mut authority = || {
            if invalidated.get() {
                Err(Error::new(E_UNEXPECTED, "candidate lease revoked"))
            } else {
                Ok(())
            }
        };

        let result = prepare_candidate_host_calls(
            &mut authority,
            || {
                trace.borrow_mut().push(HostCall::ThreadManagerCast);
                invalidated.set(true);
                Ok(())
            },
            || {
                trace.borrow_mut().push(HostCall::GetDocumentManager);
                Ok(())
            },
            |_| (),
            |_| {
                trace.borrow_mut().push(HostCall::ElementCast);
                Ok(())
            },
        );

        assert!(result.is_err());
        assert_eq!(&*trace.borrow(), &[HostCall::ThreadManagerCast]);
    }

    #[test]
    fn successful_begin_retains_cleanup_owner_before_post_call_revocation() {
        let invalidated = Cell::new(false);
        let mut cleanup_owner = None;
        let mut authority = || {
            if invalidated.get() {
                Err(Error::new(E_UNEXPECTED, "candidate lease revoked"))
            } else {
                Ok(())
            }
        };

        let result = checked_candidate_begin(&mut cleanup_owner, &mut authority, || {
            invalidated.set(true);
            Ok(("active-host-element", true))
        });

        assert!(result.is_err());
        assert_eq!(cleanup_owner, Some("active-host-element"));
    }

    #[test]
    fn uiless_candidate_contract_exposes_strings_selection_and_pages() -> Result<()> {
        let _com = ComApartment::new()?;
        // SAFETY: COM is initialized and the in-proc TSF class is requested
        // through its declared `ITfThreadMgr` interface.
        let thread_manager: ITfThreadMgr =
            unsafe { CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)? };
        // SAFETY: the thread manager is live for the document manager's
        // lifetime and the returned COM interface owns its reference.
        let document = unsafe { thread_manager.CreateDocumentMgr()? };
        let candidates = CandidateList {
            kind: sakura_proto::CandidateKind::Conversion,
            presentation: sakura_proto::types::CandidatePresentation::Compact,
            items: (1..=14)
                .map(|index| Candidate {
                    text: format!("candidate-{index}"),
                    annotation: format!("annotation-{index}"),
                    deletable_history: false,
                })
                .collect(),
            selected: 9,
            page_size: CANDIDATE_PAGE_SIZE as u16,
        };
        let element: ITfCandidateListUIElement = CandidateElement {
            document,
            state: Rc::new(RefCell::new(ElementState {
                candidates,
                shown: true,
            })),
        }
        .into();

        // SAFETY: all output buffers remain valid for each COM call.
        unsafe {
            assert_eq!(element.GetCount()?, 14);
            assert_eq!(element.GetSelection()?, 9);
            assert_eq!(element.GetString(9)?.to_string(), "candidate-10");

            let mut starts = [u32::MAX; 2];
            let mut page_count = 0;
            element.GetPageIndex(&mut starts, &mut page_count)?;
            assert_eq!(page_count, 2);
            assert_eq!(starts, [0, CANDIDATE_PAGE_SIZE as u32]);
            element.SetPageIndex(&starts)?;
            assert!(element.SetPageIndex(&starts[..1]).is_err());
            assert_eq!(element.GetCurrentPage()?, 1);
        }
        Ok(())
    }
}
