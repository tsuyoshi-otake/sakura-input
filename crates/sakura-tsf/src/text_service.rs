//! The text service object itself.
//!
//! Milestone 6 of PLAN.md: every keystroke becomes a question for
//! `sakura_engine.exe`, and this file's job is to turn the answer into
//! document operations. Nothing here decides what a key means any more.
//!
//! # Which side is the source of truth
//!
//! The engine is, and that inverts the rule milestone 3 was written under.
//! `CompositionState::text` is no longer the preedit — it is a record of
//! what the document is currently *showing*, kept for the one case where
//! the engine cannot be reached and the visible text has to be finalized
//! without asking anyone. Everything else flows from an [`Output`].
//!
//! # The two things a user must never lose
//!
//! *Text*, and *control of their application*. Both survive the engine
//! dying: the composition on screen is committed as ordinary text rather
//! than left attached to a conversation that has ended, and the keystroke
//! is handed back to the host, so an editor with no IME behind it is still
//! an editor. PLAN.md's Phase 1 exit criteria name both cases.
//!
//! # Re-entrancy
//!
//! No `RefCell` borrow is ever held across a call into TSF, because TSF
//! calls back — and under `panic = "abort"` a double borrow is the host
//! application dying with the user's unsaved work.

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

use sakura_proto::{Output, Preedit};

use crate::composition::{self, DocumentEdit, Update};
use crate::display_attributes;
use crate::edit_session;
use crate::engine::{Answer, Engine};
use crate::exports::{on_object_created, on_object_destroyed};
use crate::key_handler;

/// What the text service holds while it is attached to a thread manager.
///
/// Both fields are needed to undo the attachment, so they live or die together
/// rather than as two independently-nullable members.
#[derive(Debug)]
struct Activation {
    thread_mgr: ITfThreadMgr,
    client_id: u32,
}

/// What the document is showing, and the handle that lets us change it.
///
/// `text` is a copy of the visible preedit, not the preedit itself — the
/// engine owns that. It exists so that a lost engine can still be
/// finalized into real text without a round trip that will not complete.
/// `handle` is `Some` only while the document actually has a composition
/// open, which the host can end without asking us.
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
    engine: RefCell<Engine>,
}

impl TextService {
    pub fn new() -> Self {
        on_object_created();
        Self {
            activation: RefCell::new(None),
            composition: RefCell::new(CompositionState::default()),
            category_mgr: RefCell::new(None),
            engine: RefCell::new(Engine::new()),
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
        self.disconnect();

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

    /// Opens the connection to the engine before the first keystroke needs
    /// it, so the cost of connecting lands on activation rather than on a
    /// character the user is waiting to see.
    fn warm_up(&self) {
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            engine.warm_up();
        }
    }

    /// Closes the connection, which is also what tells the engine to forget
    /// this thread's session: the session table is keyed to the connection,
    /// so there is no `DeleteSession` to send and no way to leak one by
    /// failing to send it.
    fn disconnect(&self) {
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            *engine = Engine::new();
        }
    }

    /// Puts one question to the engine.
    ///
    /// The borrow is confined to this function. It has to be: the call
    /// inside blocks for up to the keystroke budget, and TSF must be able
    /// to re-enter this object the moment it returns.
    fn ask(&self, key: sakura_proto::KeyInput) -> Result<Answer> {
        let mut engine = self.engine.try_borrow_mut().map_err(|_| reentrancy())?;
        Ok(engine.send_key(key))
    }

    /// Tells the engine to finalize its composition, discarding whatever it
    /// answers.
    ///
    /// The answer is discarded because the document already shows the text
    /// and the user has already read it; what this call is for is leaving
    /// the engine's session empty, so the next keystroke in this
    /// application does not continue a composition the user walked away
    /// from. An unreachable engine is not a problem here — a connection
    /// that has to be rebuilt starts from an empty session anyway.
    fn ask_to_finalize(&self) {
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            let _ = engine.commit();
        }
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

    /// The context the user is typing into, found without being handed one.
    ///
    /// `OnSetFocus` reports that focus is leaving but does not say what it
    /// is leaving, and finalizing a composition needs a document to
    /// finalize it in. The thread manager still knows.
    fn focused_context(&self) -> Option<ITfContext> {
        // Cloned out and the borrow released first: both calls below
        // re-enter TSF, which is free to call back into this object.
        let thread_mgr = {
            let slot = self.activation.try_borrow().ok()?;
            slot.as_ref()?.thread_mgr.clone()
        };
        // SAFETY: the thread manager has been retained since activation.
        let documents = unsafe { thread_mgr.GetFocus() }.ok()?;
        // SAFETY: `documents` is a live document manager for this thread.
        unsafe { documents.GetTop() }.ok()
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

    /// Whatever the document is showing, taken out of the record.
    ///
    /// Taking rather than reading: every caller is about to stop showing
    /// it, and leaving a stale copy behind would let it be committed twice.
    fn take_visible_text(&self) -> Result<String> {
        let mut state = self
            .composition
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        Ok(core::mem::take(&mut state.text))
    }

    /// Turns one engine answer into the document operations that realize
    /// it, and records what the document will be showing afterwards.
    ///
    /// At most two: a commit ends the composition, and a preedit that
    /// survives the commit — the tail the engine is still working on —
    /// opens a new one. The order is fixed and matters, because the second
    /// operation's composition starts where the first one's text ended.
    fn plan(&self, output: &Output) -> Result<Vec<Update>> {
        let mut state = self
            .composition
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        let mut updates = Vec::new();

        if let Some(text) = output.commit.as_ref().filter(|text| !text.is_empty()) {
            updates.push(Update::Commit(text.clone()));
            state.text.clear();
        }

        let preedit = output
            .preedit
            .as_ref()
            .map(visible_text)
            .unwrap_or_default();
        if !preedit.is_empty() {
            updates.push(Update::Show(preedit.clone()));
            state.text = preedit;
        } else if !state.text.is_empty() {
            // The engine has nothing to show and committed nothing: the
            // user cancelled. Anything still on screen has to come off it.
            updates.push(Update::Discard);
            state.text.clear();
        }

        Ok(updates)
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

/// The preedit as one string.
///
/// Phase 1's engine marks every segment as raw input, so a segmented
/// document representation would draw exactly what this does. Phase 2
/// brings converted and focused segments, and with them a reason to give
/// each its own range and display attribute; until there is a visible
/// difference, one range is the honest representation.
fn visible_text(preedit: &Preedit) -> String {
    let mut text = String::new();
    for segment in &preedit.segments {
        text.push_str(&segment.text);
    }
    text
}

impl TextService_Impl {
    /// Asks the engine what a keystroke means, and — unless `test_only` —
    /// makes the document say so.
    ///
    /// `OnTestKeyDown` and `OnKeyDown` must agree about which keys are
    /// consumed, so both route through here. They differ only in the flag
    /// that travels to the engine, which answers the test form against a
    /// throwaway copy of the session rather than through a second code
    /// path that could drift from the first.
    fn handle_key(
        &self,
        context: Ref<'_, ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
        test_only: bool,
    ) -> Result<BOOL> {
        // A key with no context has nowhere to go. Declining it leaves the host
        // to handle it, which is the only correct answer.
        let Ok(context) = context.ok() else {
            return Ok(false.into());
        };

        let service = self.get_impl();
        let key = key_handler::translate((wparam.0 & 0xFFFF) as u16, lparam.0, test_only);

        let output = match service.ask(key)? {
            Answer::Ready(output) => output,
            // No engine, or one that did not answer in time. The key is the
            // application's. A test never edits, so it only reports that;
            // a real keystroke also has to rescue what is on screen, since
            // the conversation that would have finished it is over.
            Answer::Unavailable => {
                if !test_only {
                    self.finalize_visible_text(context)?;
                }
                return Ok(false.into());
            }
        };

        if test_only {
            return Ok(output.consumed.into());
        }

        let updates = service.plan(&output)?;
        if !updates.is_empty() {
            self.write(context, updates)?;
        }
        Ok(output.consumed.into())
    }

    /// Commits what the document is showing, as ordinary text.
    ///
    /// The rescue path. Called when the engine has stopped answering and
    /// when focus is leaving, both of which are moments where a live
    /// composition would otherwise be stranded — underlined text attached
    /// to a conversation that will never produce a result for it.
    fn finalize_visible_text(&self, context: &ITfContext) -> Result<()> {
        let text = self.get_impl().take_visible_text()?;
        if text.is_empty() {
            return Ok(());
        }
        self.write(context, vec![Update::Commit(text)])
    }

    /// Applies updates to the document, in order, under one lock.
    fn write(&self, context: &ITfContext, updates: Vec<Update>) -> Result<()> {
        let service = self.get_impl();
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

            let mut result = Ok(());
            for update in &updates {
                result = composition::apply(&edit, ec, &mut handle, update);
                // Stopping at the first failure, but still putting the
                // handle back below: dropping a live composition here would
                // strand an underlined run that nothing can ever end.
                if result.is_err() {
                    break;
                }
            }

            {
                let mut state = owner
                    .composition
                    .try_borrow_mut()
                    .map_err(|_| reentrancy())?;
                state.handle = handle;
            }
            result
        })
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
        let service = self.get_impl();
        service.attach(thread_mgr, tid, &sink)?;
        // After the attachment, not before: a failed activation must not
        // leave a connection behind, and a slow engine must not delay the
        // point at which keys start being delivered correctly.
        service.warm_up();
        Ok(())
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    /// Losing focus finalizes the preedit in place; it is never discarded.
    ///
    /// The alternative — dropping the text — loses work the user can see,
    /// and the other alternative — keeping the composition open — leaves
    /// underlined text in a document we are no longer receiving keys for.
    /// Committing is the only option that both keeps the characters and
    /// ends our claim on them (PLAN.md Phase 1, focus-loss criterion).
    fn OnSetFocus(&self, fforeground: BOOL) -> Result<()> {
        if fforeground.as_bool() {
            return Ok(());
        }

        let service = self.get_impl();
        service.ask_to_finalize();

        match service.focused_context() {
            Some(context) => self.finalize_visible_text(&context),
            // Nothing to write into: the document is already gone. The text
            // stays where it is — what is dropped is only the handle.
            None => service.forget_composition(),
        }
    }

    fn OnTestKeyDown(
        &self,
        pic: Ref<'_, ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<BOOL> {
        self.handle_key(pic, wparam, lparam, true)
    }

    fn OnKeyDown(&self, pic: Ref<'_, ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        self.handle_key(pic, wparam, lparam, false)
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
    /// it, so the handle is dropped without calling `EndComposition` on it. The
    /// engine is told too, because its composition has just been committed on
    /// its behalf and continuing it would duplicate text already in the
    /// document.
    fn OnCompositionTerminated(
        &self,
        _ecwrite: u32,
        _pcomposition: Ref<'_, ITfComposition>,
    ) -> Result<()> {
        let service = self.get_impl();
        service.ask_to_finalize();
        service.forget_composition()
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

// `expect` and `panic!` are denied for this crate because it is loaded into
// applications that are not ours to crash. Test code is not loaded into
// anything, and a test that cannot fail loudly is not a test.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use sakura_proto::{Segment, UnderlineKind};

    fn preedit(parts: &[&str]) -> Preedit {
        Preedit {
            segments: parts
                .iter()
                .map(|text| Segment {
                    text: (*text).to_owned(),
                    underline: UnderlineKind::Raw,
                })
                .collect(),
            cursor: 0,
        }
    }

    fn output(commit: Option<&str>, shown: Option<&[&str]>) -> Output {
        Output {
            consumed: true,
            beep: false,
            mode: None,
            preedit: shown.map(preedit),
            commit: commit.map(str::to_owned),
        }
    }

    /// The engine emits the normalized kana and the romaji still being
    /// typed as separate segments; the user sees one run of text.
    #[test]
    fn segments_are_shown_as_one_run() {
        assert_eq!(visible_text(&preedit(&["か", "t"])), "かt");
        assert_eq!(visible_text(&preedit(&[])), "");
    }

    #[test]
    fn a_preedit_is_shown_and_remembered() {
        let service = TextService::new();
        let updates = service.plan(&output(None, Some(&["か"]))).expect("plan");
        assert!(matches!(updates.as_slice(), [Update::Show(text)] if text == "か"));
        assert_eq!(service.take_visible_text().expect("visible"), "か");
    }

    /// Enter mid-word: the converted text is committed and the tail the
    /// engine is still working on stays underlined. The commit has to come
    /// first, because the new composition starts where its text ended.
    #[test]
    fn a_commit_with_a_tail_ends_one_composition_and_opens_another() {
        let service = TextService::new();
        let updates = service
            .plan(&output(Some("漢字"), Some(&["か"])))
            .expect("plan");
        match updates.as_slice() {
            [Update::Commit(committed), Update::Show(shown)] => {
                assert_eq!(committed, "漢字");
                assert_eq!(shown, "か");
            }
            other => panic!("expected a commit then a show, got {other:?}"),
        }
        assert_eq!(service.take_visible_text().expect("visible"), "か");
    }

    /// Escape: nothing committed, nothing left to show, and something on
    /// screen that has to come off it.
    #[test]
    fn an_empty_answer_discards_what_is_on_screen() {
        let service = TextService::new();
        service.plan(&output(None, Some(&["か"]))).expect("plan");

        let updates = service.plan(&output(None, None)).expect("plan");
        assert!(matches!(updates.as_slice(), [Update::Discard]));
        assert_eq!(service.take_visible_text().expect("visible"), "");
    }

    /// The idle case, and by far the most common one: a key that touched no
    /// composition must not cost the document an edit session.
    #[test]
    fn an_empty_answer_with_nothing_on_screen_does_nothing() {
        let service = TextService::new();
        assert!(service.plan(&output(None, None)).expect("plan").is_empty());
    }

    /// A commit that empties the preedit must not also emit a discard: the
    /// commit already closed the composition, and discarding afterwards
    /// would reopen and clear one.
    #[test]
    fn a_plain_commit_is_a_single_operation() {
        let service = TextService::new();
        service.plan(&output(None, Some(&["か"]))).expect("plan");

        let updates = service.plan(&output(Some("か"), None)).expect("plan");
        assert!(matches!(updates.as_slice(), [Update::Commit(text)] if text == "か"));
        assert_eq!(service.take_visible_text().expect("visible"), "");
    }

    /// Taking the visible text is what the rescue path does, and doing it
    /// twice must not commit the same characters twice.
    #[test]
    fn visible_text_can_only_be_taken_once() {
        let service = TextService::new();
        service.plan(&output(None, Some(&["かん"]))).expect("plan");
        assert_eq!(service.take_visible_text().expect("visible"), "かん");
        assert_eq!(service.take_visible_text().expect("visible"), "");
    }
}
