//! Running code inside a TSF document lock.
//!
//! Nothing may touch a document's text outside an edit session, so every
//! composition update goes through here.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windows::Win32::Foundation::E_UNEXPECTED;
use windows::Win32::UI::TextServices::{
    ITfContext, ITfEditSession, ITfEditSession_Impl, TF_CONTEXT_EDIT_CONTEXT_FLAGS, TF_ES_ASYNC,
    TF_ES_READ, TF_ES_READWRITE, TF_ES_SYNC,
};
use windows_core::{implement, Error, IUnknownImpl, Result};

/// A document edit, packaged so TSF can call it back whenever it grants a lock.
///
/// `'static` and owning rather than borrowing, because an asynchronous session
/// runs long after the keystroke that requested it has returned.
type Action = Box<dyn FnOnce(u32) -> Result<()>>;

/// Whether the request's callback ran before `RequestEditSession` returned or
/// remains owned by TSF for a later asynchronous grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditRequestState {
    Ran,
    Queued,
}

/// The callback action has one owner even if a host incorrectly invokes the
/// same edit-session object more than once. Keeping this COM-free makes the
/// exactly-once contract directly testable.
struct OnceAction {
    action: RefCell<Option<Action>>,
}

impl OnceAction {
    fn new(action: Action) -> Self {
        Self {
            action: RefCell::new(Some(action)),
        }
    }

    fn run(&self, ec: u32) -> Result<()> {
        // Taken out of the cell before running, so an action that somehow
        // re-enters this session finds it empty instead of recursing.
        let action = {
            let mut slot = self
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

#[implement(ITfEditSession)]
struct EditSession {
    action: OnceAction,
}

impl EditSession {
    fn new(action: Action) -> Self {
        Self {
            action: OnceAction::new(action),
        }
    }
}

impl ITfEditSession_Impl for EditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        self.get_impl().action.run(ec)
    }
}

/// Runs a read/write document lock with an explicit synchronous preference.
pub fn write_in_document_with_mode(
    context: &ITfContext,
    client_id: u32,
    synchronous_first: bool,
    action: impl FnOnce(u32) -> Result<()> + 'static,
) -> Result<EditRequestState> {
    in_document(
        context,
        client_id,
        TF_ES_READWRITE,
        synchronous_first,
        action,
    )
}

/// Runs `action` under a read-only asynchronous document lock.
pub fn read_in_document_async(
    context: &ITfContext,
    client_id: u32,
    action: impl FnOnce(u32) -> Result<()> + 'static,
) -> Result<()> {
    in_document(context, client_id, TF_ES_READ, false, action).map(|_| ())
}

/// Runs a value-producing read under a synchronous document lock.
///
/// Return-valued COM methods such as `ITfFnReconversion::GetReconversion`
/// cannot hand their answer back after an asynchronous callback, so unlike
/// [`read_in_document_async`] this deliberately has no queued fallback. Every path
/// either returns the callback's value or an explicit error; a refused lock
/// can never become an accidentally successful empty result.
pub fn read_in_document_sync<T: 'static>(
    context: &ITfContext,
    client_id: u32,
    action: impl FnOnce(u32) -> Result<T> + 'static,
) -> Result<T> {
    let result = Rc::new(RefCell::new(None));
    let callback_result = Rc::clone(&result);
    let session: ITfEditSession = EditSession::new(Box::new(move |ec| {
        let value = action(ec);
        let mut slot = callback_result
            .try_borrow_mut()
            .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant synchronous edit session"))?;
        *slot = Some(value);
        Ok(())
    }))
    .into();

    // SAFETY: `context` is live, `session` owns its callback for the duration
    // of the request, and this function does not return until a synchronous
    // request has either run or been refused.
    let session_result =
        unsafe { context.RequestEditSession(client_id, &session, TF_ES_SYNC | TF_ES_READ)? };
    session_result.ok()?;

    let mut slot = result
        .try_borrow_mut()
        .map_err(|_| Error::new(E_UNEXPECTED, "re-entrant synchronous edit result"))?;
    slot.take()
        .ok_or_else(|| Error::new(E_UNEXPECTED, "synchronous edit session did not run"))?
}

/// Requests a synchronous lock once when the caller prefers it, then retries
/// exactly once with the explicitly asynchronous flag only when the callback
/// did not run. The requester is injected so the policy can be tested without a
/// TSF context or a COM host.
fn request_with_sync_fallback<Request, CallbackRan>(
    access: TF_CONTEXT_EDIT_CONTEXT_FLAGS,
    synchronous_first: bool,
    mut request: Request,
    mut callback_ran: CallbackRan,
) -> Result<EditRequestState>
where
    Request: FnMut(TF_CONTEXT_EDIT_CONTEXT_FLAGS) -> Result<()>,
    CallbackRan: FnMut() -> bool,
{
    if synchronous_first {
        match request(TF_ES_SYNC | access) {
            Ok(()) if callback_ran() => return Ok(EditRequestState::Ran),
            Err(error) if callback_ran() => return Err(error),
            // A host may refuse the synchronous lock either as the outer
            // HRESULT or as the returned session HRESULT. If DoEditSession did
            // not run, the action is still owned by the session and can be
            // retried once as a queued async request.
            Ok(()) | Err(_) => {}
        }
    }

    request(TF_ES_ASYNC | access)?;
    Ok(if callback_ran() {
        EditRequestState::Ran
    } else {
        EditRequestState::Queued
    })
}

fn in_document(
    context: &ITfContext,
    client_id: u32,
    access: TF_CONTEXT_EDIT_CONTEXT_FLAGS,
    synchronous_first: bool,
    action: impl FnOnce(u32) -> Result<()> + 'static,
) -> Result<EditRequestState> {
    let ran = Rc::new(Cell::new(false));
    let callback_ran = Rc::clone(&ran);
    let session: ITfEditSession = EditSession::new(Box::new(move |ec| {
        callback_ran.set(true);
        action(ec)
    }))
    .into();

    request_with_sync_fallback(
        access,
        synchronous_first,
        |flags| {
            // SAFETY: `context` is the context TSF handed us for this key
            // event and `session` outlives each request. `TF_ES_ASYNC` is
            // deliberate: the zero-valued ASYNCDONTCARE flag may run the
            // callback inline, which can re-enter a host while it is still
            // dispatching the keystroke that requested this edit.
            let session_result = unsafe { context.RequestEditSession(client_id, &session, flags)? };
            session_result.ok()
        },
        || ran.get(),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use windows::Win32::Foundation::E_UNEXPECTED;
    use windows_core::Error;

    use super::*;

    #[test]
    fn refused_synchronous_request_issues_exactly_one_async_retry() {
        let calls = RefCell::new(Vec::new());
        let result = request_with_sync_fallback(
            TF_ES_READWRITE,
            true,
            |flags| {
                calls.borrow_mut().push(flags);
                if flags == TF_ES_SYNC | TF_ES_READWRITE {
                    Err(Error::from_hresult(E_UNEXPECTED))
                } else {
                    Ok(())
                }
            },
            || false,
        );

        assert_eq!(result, Ok(EditRequestState::Queued));
        assert_eq!(
            &*calls.borrow(),
            &[TF_ES_SYNC | TF_ES_READWRITE, TF_ES_ASYNC | TF_ES_READWRITE]
        );
    }

    #[test]
    fn synchronous_callback_never_falls_back_to_async() {
        let ran = Cell::new(false);
        let calls = RefCell::new(Vec::new());
        let action_count = Cell::new(0);
        let result = request_with_sync_fallback(
            TF_ES_READWRITE,
            true,
            |flags| {
                calls.borrow_mut().push(flags);
                if flags == TF_ES_SYNC | TF_ES_READWRITE {
                    action_count.set(action_count.get() + 1);
                    ran.set(true);
                    Ok(())
                } else {
                    Err(Error::from_hresult(E_UNEXPECTED))
                }
            },
            || ran.get(),
        );

        assert_eq!(result, Ok(EditRequestState::Ran));
        assert_eq!(&*calls.borrow(), &[TF_ES_SYNC | TF_ES_READWRITE]);
        assert_eq!(action_count.get(), 1);
    }

    #[test]
    fn inline_async_completion_reports_ran() {
        let ran = Cell::new(false);
        let calls = RefCell::new(Vec::new());
        let action_count = Cell::new(0);
        let result = request_with_sync_fallback(
            TF_ES_READWRITE,
            true,
            |flags| {
                calls.borrow_mut().push(flags);
                if flags == TF_ES_SYNC | TF_ES_READWRITE {
                    Err(Error::from_hresult(E_UNEXPECTED))
                } else {
                    action_count.set(action_count.get() + 1);
                    ran.set(true);
                    Ok(())
                }
            },
            || ran.get(),
        );

        assert_eq!(result, Ok(EditRequestState::Ran));
        assert_eq!(
            &*calls.borrow(),
            &[TF_ES_SYNC | TF_ES_READWRITE, TF_ES_ASYNC | TF_ES_READWRITE]
        );
        assert_eq!(action_count.get(), 1);
    }

    #[test]
    fn second_callback_cannot_execute_the_action() {
        let action_count = Rc::new(Cell::new(0));
        let callback_count = Rc::clone(&action_count);
        let action = OnceAction::new(Box::new(move |_| {
            callback_count.set(callback_count.get() + 1);
            Ok(())
        }));

        assert!(action.run(1).is_ok());
        assert!(action.run(2).is_err());
        assert_eq!(action_count.get(), 1);
    }
}
