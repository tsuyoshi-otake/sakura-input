//! The text service object itself.
//!
//! Phase 1 milestone 3 (PLAN.md): activation, a key sink, and a composition that
//! echoes what was typed and commits it on Enter. The conversion logic is
//! hardcoded here on purpose — milestone 6 replaces the body of `handle_key`
//! with an IPC round trip to `sakura_engine.exe` and deletes nothing else.
//!
//! Two invariants hold throughout. The in-memory preedit is the source of truth
//! and the document is a projection of it, so a late edit session cannot produce
//! a document that disagrees with what the user typed. And no `RefCell` borrow
//! is ever held across a call into TSF, because TSF calls back — and under
//! `panic = "abort"` a double borrow is the host application dying.

use std::cell::RefCell;

use windows::Win32::Foundation::{E_UNEXPECTED, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, IEnumTfDisplayAttributeInfo, ITfCategoryMgr, ITfComposition,
    ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfDisplayAttributeInfo,
    ITfDisplayAttributeProvider, ITfDisplayAttributeProvider_Impl, ITfKeyEventSink,
    ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl,
    ITfTextInputProcessor_Impl, ITfThreadMgr,
};
use windows_core::{implement, Error, IUnknownImpl, Interface, Ref, Result, BOOL, GUID};

use crate::composition::{self, DocumentEdit, Update};
use crate::display_attributes;
use crate::edit_session;
use crate::exports::{on_object_created, on_object_destroyed};
use crate::key_handler::{self, KeyAction, Modifiers};

/// What the text service holds while it is attached to a thread manager.
///
/// Both fields are needed to undo the attachment, so they live or die together
/// rather than as two independently-nullable members.
#[derive(Debug)]
struct Activation {
    thread_mgr: ITfThreadMgr,
    client_id: u32,
}

/// The preedit, and the document-side handle projecting it.
///
/// `text` is authoritative; `handle` is `Some` only while the document actually
/// has a composition open, which the host can end without asking us.
#[derive(Debug, Default)]
struct CompositionState {
    text: String,
    handle: Option<ITfComposition>,
}

/// One instance exists per thread that activates the IME.
#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfCompositionSink,
    ITfDisplayAttributeProvider
)]
#[derive(Debug)]
pub struct TextService {
    activation: RefCell<Option<Activation>>,
    composition: RefCell<CompositionState>,
    category_mgr: RefCell<Option<ITfCategoryMgr>>,
}

impl TextService {
    pub fn new() -> Self {
        on_object_created();
        Self {
            activation: RefCell::new(None),
            composition: RefCell::new(CompositionState::default()),
            category_mgr: RefCell::new(None),
        }
    }

    fn attach(
        &self,
        thread_mgr: &ITfThreadMgr,
        client_id: u32,
        sink: &ITfKeyEventSink,
    ) -> Result<()> {
        // TSF should never activate an already-active service, but if it does,
        // silently overwriting the old activation would strand a key sink on a
        // thread manager nobody will ever unadvise.
        self.detach()?;

        let keystroke_mgr: ITfKeystrokeMgr = thread_mgr.cast()?;
        // SAFETY: `keystroke_mgr` came from a live thread manager and `sink`
        // borrows from this object, which outlives the call.
        unsafe { keystroke_mgr.AdviseKeyEventSink(client_id, sink, true)? };

        let mut slot = self.activation.try_borrow_mut().map_err(|_| reentrancy())?;
        *slot = Some(Activation {
            thread_mgr: thread_mgr.clone(),
            client_id,
        });
        Ok(())
    }

    fn detach(&self) -> Result<()> {
        // Deactivation is the last chance to drop the composition handle. The
        // document is going away with it, so there is nothing to end — holding
        // the reference longer would just outlive the context it belongs to.
        self.forget_composition()?;

        // The borrow ends before the TSF call: unadvising re-enters TSF, and a
        // callback arriving while the cell is still borrowed would be a double
        // borrow, which under `panic = "abort"` takes the host process with it.
        let previous = {
            let mut slot = self.activation.try_borrow_mut().map_err(|_| reentrancy())?;
            slot.take()
        };
        let Some(activation) = previous else {
            return Ok(());
        };

        let keystroke_mgr: ITfKeystrokeMgr = activation.thread_mgr.cast()?;
        // SAFETY: the thread manager was retained since activation, so it is
        // still valid, and `client_id` is the id it issued.
        unsafe { keystroke_mgr.UnadviseKeyEventSink(activation.client_id) }
    }

    /// Drops all preedit state without touching the document.
    ///
    /// Used when the document has already disposed of the composition — ending
    /// it again would be a call on a handle the host has retired.
    fn forget_composition(&self) -> Result<()> {
        let mut state = self
            .composition
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        state.text.clear();
        state.handle = None;
        Ok(())
    }

    fn client_id(&self) -> Result<u32> {
        let slot = self.activation.try_borrow().map_err(|_| reentrancy())?;
        match slot.as_ref() {
            Some(activation) => Ok(activation.client_id),
            None => Err(Error::new(E_UNEXPECTED, "key event before activation")),
        }
    }

    /// The category manager, created on first use and cached for the thread.
    ///
    /// Only needed to turn a display-attribute GUID into the atom TSF wants, so
    /// a failure here costs the underline and nothing else.
    fn category_manager(&self) -> Result<ITfCategoryMgr> {
        {
            let cached = self.category_mgr.try_borrow().map_err(|_| reentrancy())?;
            if let Some(manager) = cached.as_ref() {
                return Ok(manager.clone());
            }
        }

        // SAFETY: an in-process COM class with no outer unknown; the CLSID is a
        // valid `'static` constant.
        let manager: ITfCategoryMgr =
            unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)? };

        let mut cached = self
            .category_mgr
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        *cached = Some(manager.clone());
        Ok(manager)
    }

    fn is_composing(&self) -> Result<bool> {
        let state = self.composition.try_borrow().map_err(|_| reentrancy())?;
        Ok(!state.text.is_empty())
    }

    /// Applies `action` to the preedit and reports what the document now needs.
    fn advance(&self, action: KeyAction) -> Result<Option<Update>> {
        let mut state = self
            .composition
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        Ok(match action {
            KeyAction::Insert(character) => {
                state.text.push(character);
                Some(Update::Show(state.text.clone()))
            }
            KeyAction::Erase => {
                state.text.pop();
                Some(Update::Show(state.text.clone()))
            }
            KeyAction::Commit => Some(Update::Commit(core::mem::take(&mut state.text))),
            KeyAction::Cancel => {
                state.text.clear();
                Some(Update::Discard)
            }
            KeyAction::PassThrough => None,
        })
    }
}

impl Drop for TextService {
    fn drop(&mut self) {
        on_object_destroyed();
    }
}

/// A borrow that fails means TSF re-entered us mid-update. Reporting it beats
/// the alternative: `RefCell`'s own panic would abort the host application.
fn reentrancy() -> Error {
    Error::new(E_UNEXPECTED, "re-entrant access to text service state")
}

impl TextService_Impl {
    /// Decides what a keystroke means, and — unless `test_only` — carries it out.
    ///
    /// `OnTestKeyDown` and `OnKeyDown` must agree about which keys are consumed,
    /// so both route through here and differ only in whether they act.
    fn handle_key(
        &self,
        context: Ref<'_, ITfContext>,
        wparam: WPARAM,
        test_only: bool,
    ) -> Result<BOOL> {
        // A key with no context has nowhere to go. Declining it leaves the host
        // to handle it, which is the only correct answer.
        let Ok(context) = context.ok() else {
            return Ok(false.into());
        };

        let service = self.get_impl();
        let virtual_key = (wparam.0 & 0xFFFF) as u16;
        let action =
            key_handler::classify(virtual_key, Modifiers::current(), service.is_composing()?);

        if !action.consumes_key() {
            return Ok(false.into());
        }
        if test_only {
            return Ok(true.into());
        }

        let Some(update) = service.advance(action)? else {
            return Ok(false.into());
        };

        let client_id = service.client_id()?;
        let edit = DocumentEdit {
            context: context.clone(),
            sink: self.to_interface(),
            category_mgr: service.category_manager().ok(),
        };

        // The closure owns a reference to this object because an asynchronous
        // session runs after `OnKeyDown` has returned.
        let owner = self.to_object();
        edit_session::write_in_document(context, client_id, move |ec| {
            // Taken out and put back rather than borrowed across the document
            // calls below, all of which can re-enter this text service.
            let mut handle = {
                let mut state = owner
                    .composition
                    .try_borrow_mut()
                    .map_err(|_| reentrancy())?;
                state.handle.take()
            };
            let result = composition::apply(&edit, ec, &mut handle, &update);
            {
                let mut state = owner
                    .composition
                    .try_borrow_mut()
                    .map_err(|_| reentrancy())?;
                state.handle = handle;
            }
            result
        })?;

        Ok(true.into())
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<'_, ITfThreadMgr>, tid: u32) -> Result<()> {
        ITfTextInputProcessorEx_Impl::ActivateEx(self, ptim, tid, 0)
    }

    fn Deactivate(&self) -> Result<()> {
        self.get_impl().detach()
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(&self, ptim: Ref<'_, ITfThreadMgr>, tid: u32, _dwflags: u32) -> Result<()> {
        let thread_mgr = ptim.ok()?;
        let sink: ITfKeyEventSink = self.to_interface();
        self.get_impl().attach(thread_mgr, tid, &sink)
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    /// Losing focus abandons the preedit rather than committing it.
    ///
    /// There is no context to write into here, so a commit is not on offer; the
    /// alternative — keeping the text — would have it reappear in whatever
    /// document the user switched to.
    fn OnSetFocus(&self, fforeground: BOOL) -> Result<()> {
        if !fforeground.as_bool() {
            self.get_impl().forget_composition()?;
        }
        Ok(())
    }

    fn OnTestKeyDown(
        &self,
        pic: Ref<'_, ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<BOOL> {
        self.handle_key(pic, wparam, true)
    }

    fn OnKeyDown(&self, pic: Ref<'_, ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        self.handle_key(pic, wparam, false)
    }

    /// Key-up never edits the document, so declining it keeps auto-repeat and
    /// accelerator handling in the application's hands.
    fn OnTestKeyUp(
        &self,
        _pic: Ref<'_, ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> Result<BOOL> {
        Ok(false.into())
    }

    fn OnKeyUp(&self, _pic: Ref<'_, ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(false.into())
    }

    /// Mode-switching keys are registered in milestone 7, once there is a mode
    /// to switch.
    fn OnPreservedKey(&self, _pic: Ref<'_, ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
        Ok(false.into())
    }
}

impl ITfCompositionSink_Impl for TextService_Impl {
    /// The host ended our composition — the user clicked elsewhere, or the
    /// application decided the preedit was over.
    ///
    /// The text it contained stays in the document; what is gone is our claim on
    /// it, so the handle is dropped without calling `EndComposition` on it.
    fn OnCompositionTerminated(
        &self,
        _ecwrite: u32,
        _pcomposition: Ref<'_, ITfComposition>,
    ) -> Result<()> {
        self.get_impl().forget_composition()
    }
}

impl ITfDisplayAttributeProvider_Impl for TextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(display_attributes::enumerate())
    }

    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        if guid.is_null() {
            return Err(Error::from_hresult(windows::Win32::Foundation::E_POINTER));
        }
        // SAFETY: `guid` was just checked non-null and TSF guarantees it points
        // at a readable GUID for the duration of the call.
        let guid = unsafe { *guid };
        display_attributes::lookup(&guid)
    }
}
