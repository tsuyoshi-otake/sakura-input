//! Running code inside a TSF document lock.
//!
//! Nothing may touch a document's text outside an edit session, so every
//! composition update goes through here.

use std::cell::RefCell;

use windows::Win32::Foundation::E_UNEXPECTED;
use windows::Win32::UI::TextServices::{
    ITfContext, ITfEditSession, ITfEditSession_Impl, TF_ES_ASYNCDONTCARE, TF_ES_READWRITE,
    TF_ES_SYNC,
};
use windows_core::{implement, Error, IUnknownImpl, Result};

/// A document edit, packaged so TSF can call it back whenever it grants a lock.
///
/// `'static` and owning rather than borrowing, because an asynchronous session
/// runs long after the keystroke that requested it has returned.
type Action = Box<dyn FnOnce(u32) -> Result<()>>;

#[implement(ITfEditSession)]
struct EditSession {
    action: RefCell<Option<Action>>,
}

impl EditSession {
    fn new(action: Action) -> Self {
        Self {
            action: RefCell::new(Some(action)),
        }
    }
}

impl ITfEditSession_Impl for EditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        // Taken out of the cell before running, so an action that somehow
        // re-enters this session finds it empty instead of recursing.
        let action = {
            let mut slot = self
                .get_impl()
                .action
                .try_borrow_mut()
                .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant edit session"))?;
            slot.take()
        };
        match action {
            Some(action) => action(ec),
            None => Err(Error::new(E_UNEXPECTED, "edit session ran twice")),
        }
    }
}

/// Runs `action` under a read/write document lock.
///
/// A synchronous lock is preferred, because it lets a keystroke and the preedit
/// it produces land in the same frame — the user never sees a character appear
/// out of order. Hosts are free to refuse one (DESIGN 4.4), so a refusal falls
/// back to an asynchronous session rather than dropping the edit: the keystroke
/// was already consumed, and silently discarding it would eat the user's input.
///
/// The distinction that matters is *refused* versus *ran and failed*. Only the
/// former retries; an action that ran and returned an error has already had its
/// effect on the document and must not be applied twice.
pub fn write_in_document(
    context: &ITfContext,
    client_id: u32,
    action: impl FnOnce(u32) -> Result<()> + 'static,
) -> Result<()> {
    let session: ITfEditSession = EditSession::new(Box::new(action)).into();

    // SAFETY: `context` is the context TSF handed us for this key event and
    // `session` outlives the call.
    let synchronous =
        unsafe { context.RequestEditSession(client_id, &session, TF_ES_SYNC | TF_ES_READWRITE) };
    if let Ok(session_result) = synchronous {
        return session_result.ok();
    }

    // SAFETY: as above. The session still holds its action — a refused request
    // never called `DoEditSession`.
    let queued = unsafe {
        context.RequestEditSession(client_id, &session, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE)?
    };
    queued.ok()
}
