//! Bounded, COM-free ownership for document-write outputs.
//!
//! TSF is free to delay an asynchronous edit session until after focus, the
//! active context, or a later engine output has changed.  This module keeps the
//! part of that problem which can be tested without COM: admission, ordering,
//! lifetime epochs, document revisions, and exactly-once terminal outcomes.
//! The text service owns the COM payloads and asks this journal for permission
//! immediately before it touches a document.

use std::collections::VecDeque;

/// A deliberately small bound: it covers a short burst while a host delays one
/// asynchronous lock without allowing an unbounded backlog of engine state.
pub(crate) const DEFAULT_WRITE_CAPACITY: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct OperationId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ContextId(pub(crate) usize);

/// The visible part of a composition state.  This is a projection, not a
/// mutable document handle, so planning can stay pure while an earlier write is
/// still waiting for TSF.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct VisibleState {
    pub(crate) text: String,
    pub(crate) has_composition: bool,
}

impl VisibleState {
    pub(crate) fn empty() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelReason {
    ActivationChanged,
    Deactivated,
    FocusChanged,
    CompositionTerminated,
    ContextReplaced,
    RevisionMismatch,
    StaleCallback,
    EngineUnavailable,
    DeferredUnavailable,
    PredecessorFailed,
    /// This operation's own request was refused before it ever reached the
    /// engine (a local encode failure). Distinct from
    /// [`PredecessorFailed`](CancelReason::PredecessorFailed): no earlier
    /// operation is at fault here, this one's own request was.
    RequestRejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalOutcome {
    Applied,
    Rejected,
    Cancelled(CancelReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    Inactive,
    Full,
    ReservationLost,
    ProjectionMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Epoch {
    activation: u64,
    focus: u64,
    context: ContextId,
    revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reservation {
    id: OperationId,
}

/// Identifies the one queued operation that is currently allowed to request or
/// run an edit session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ticket {
    id: OperationId,
    epoch: Epoch,
    result_revision: u64,
}

impl Ticket {
    pub(crate) fn id(self) -> u64 {
        self.id.0
    }

    pub(crate) fn context(self) -> ContextId {
        self.epoch.context
    }

    pub(crate) fn focus_generation(self) -> u64 {
        self.epoch.focus
    }

    pub(crate) fn document_revision(self) -> u64 {
        self.epoch.revision
    }
}

/// A lease for deferred candidate/layout work.  It includes the operation id so
/// an older deferred UI request cannot make itself visible after a newer output
/// owns the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiLease {
    id: OperationId,
    activation: u64,
    focus: u64,
    context: ContextId,
    revision: u64,
}

impl UiLease {
    pub(crate) fn context(self) -> ContextId {
        self.context
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Reserved,
    Ready,
    Requested,
}

#[derive(Debug)]
struct Operation<T> {
    id: OperationId,
    activation: u64,
    focus: u64,
    context: ContextId,
    phase: Phase,
    base_revision: u64,
    result_revision: u64,
    writes_document: bool,
    before: VisibleState,
    after: VisibleState,
    payload: Option<T>,
}

#[derive(Debug, Clone)]
pub(crate) struct Request<T> {
    pub(crate) ticket: Ticket,
    pub(crate) payload: T,
    pub(crate) writes_document: bool,
}

#[derive(Debug)]
pub(crate) struct Completion<T> {
    pub(crate) outcome: TerminalOutcome,
    /// A reservation cancelled before the engine answered has no payload.  It
    /// is still recorded as terminal, but it owns no candidate/layout cleanup.
    pub(crate) payload: Option<T>,
    pub(crate) ui_lease: Option<UiLease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalRecord {
    pub(crate) id: OperationId,
    pub(crate) outcome: TerminalOutcome,
}

/// An ordered, bounded journal of outputs accepted from the engine.
///
/// A reservation is made *before* the real engine is advanced.  It becomes a
/// ready operation only after its pure document plan is known.  There can be at
/// most one unresolved reservation, so a re-entrant key cannot create a gap in
/// revision assignment while the engine call is in flight.
#[derive(Debug)]
pub(crate) struct WriteCoordinator<T> {
    capacity: usize,
    terminal_capacity: usize,
    next_id: u64,
    activation: u64,
    focus: u64,
    active: bool,
    context: Option<ContextId>,
    committed_revision: u64,
    tail_revision: u64,
    committed_visible: VisibleState,
    tail_visible: VisibleState,
    operations: VecDeque<Operation<T>>,
    terminals: VecDeque<TerminalRecord>,
    ui_lease: Option<UiLease>,
}

impl<T> WriteCoordinator<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            terminal_capacity: capacity.max(1),
            next_id: 1,
            activation: 0,
            focus: 0,
            active: false,
            context: None,
            committed_revision: 0,
            tail_revision: 0,
            committed_visible: VisibleState::empty(),
            tail_visible: VisibleState::empty(),
            operations: VecDeque::new(),
            terminals: VecDeque::new(),
            ui_lease: None,
        }
    }

    pub(crate) fn activate(&mut self) -> Vec<Completion<T>> {
        let cancelled = self.cancel_all(CancelReason::ActivationChanged);
        self.active = true;
        self.activation = self.activation.wrapping_add(1);
        self.focus = self.focus.wrapping_add(1);
        self.context = None;
        self.committed_revision = 0;
        self.tail_revision = 0;
        self.committed_visible = VisibleState::empty();
        self.tail_visible = VisibleState::empty();
        self.ui_lease = None;
        cancelled
    }

    pub(crate) fn deactivate(&mut self) -> Vec<Completion<T>> {
        self.active = false;
        self.activation = self.activation.wrapping_add(1);
        self.focus = self.focus.wrapping_add(1);
        self.context = None;
        self.ui_lease = None;
        self.cancel_all(CancelReason::Deactivated)
    }

    pub(crate) fn focus_changed(&mut self) -> Vec<Completion<T>> {
        self.focus = self.focus.wrapping_add(1);
        self.ui_lease = None;
        self.cancel_all(CancelReason::FocusChanged)
    }

    pub(crate) fn composition_terminated(&mut self) -> Vec<Completion<T>> {
        self.committed_revision = self.committed_revision.wrapping_add(1);
        self.tail_revision = self.committed_revision;
        self.committed_visible = VisibleState::empty();
        self.tail_visible = VisibleState::empty();
        self.ui_lease = None;
        self.cancel_all(CancelReason::CompositionTerminated)
    }

    #[cfg(test)]
    /// Records a document revision the journal did not apply (for example, a
    /// host lifecycle event).  Every queued base revision is now stale.
    pub(crate) fn document_changed(&mut self) -> Vec<Completion<T>> {
        self.committed_revision = self.committed_revision.wrapping_add(1);
        self.tail_revision = self.committed_revision;
        self.ui_lease = None;
        self.cancel_all(CancelReason::RevisionMismatch)
    }

    /// Starts tracking `context`.  Replacing a context cancels all dependent
    /// work before the caller can advance the engine for the new document.
    pub(crate) fn observe_context(&mut self, context: ContextId) -> Vec<Completion<T>> {
        match self.context {
            Some(current) if current == context => Vec::new(),
            None => {
                self.context = Some(context);
                Vec::new()
            }
            Some(_) => {
                let cancelled = self.cancel_all(CancelReason::ContextReplaced);
                self.context = Some(context);
                self.committed_revision = 0;
                self.tail_revision = 0;
                self.committed_visible = VisibleState::empty();
                self.tail_visible = VisibleState::empty();
                self.ui_lease = None;
                cancelled
            }
        }
    }

    pub(crate) fn can_admit_for_context(&self, context: ContextId) -> bool {
        if !self.active {
            return false;
        }
        if self.context.is_some_and(|current| current != context) {
            // The actual key path will cancel the old-context backlog first.
            return true;
        }
        self.operations.len() < self.capacity && !self.has_reservation()
    }

    /// Returns whether `context` would replace the context currently owned by
    /// the journal.  This is a read-only observation: the real callback must
    /// still call [`Self::observe_context`] to cancel the old backlog, while a
    /// Probe can use this fact to return a conservative transition answer
    /// without touching the journal.
    pub(crate) fn is_context_replacement(&self, context: ContextId) -> bool {
        self.context.is_some_and(|current| current != context)
    }

    /// Returns whether any live operation carries a payload matching the
    /// supplied predicate. TextService uses this bounded query to distinguish
    /// an ordinary full journal from an exact-text undo whose host document
    /// boundary must fence all later input.
    pub(crate) fn any_payload(&self, predicate: impl FnMut(&T) -> bool) -> bool {
        self.operations
            .iter()
            .filter_map(|operation| operation.payload.as_ref())
            .any(predicate)
    }

    /// A pending payload-specific transaction can impose a stricter admission
    /// policy than the ordinary capacity limit. Keeping that predicate at the
    /// coordinator boundary prevents a caller from reserving a later write
    /// merely because there is still a free journal slot.
    pub(crate) fn can_admit_for_context_unless(
        &self,
        context: ContextId,
        reject: impl FnMut(&T) -> bool,
    ) -> bool {
        !self.any_payload(reject) && self.can_admit_for_context(context)
    }

    pub(crate) fn reserve(&mut self, context: ContextId) -> Result<Reservation, AdmissionError> {
        if !self.active {
            return Err(AdmissionError::Inactive);
        }
        if self.context != Some(context) {
            return Err(AdmissionError::ReservationLost);
        }
        if self.operations.len() >= self.capacity || self.has_reservation() {
            return Err(AdmissionError::Full);
        }
        let id = OperationId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.operations.push_back(Operation {
            id,
            activation: self.activation,
            focus: self.focus,
            context,
            phase: Phase::Reserved,
            base_revision: self.tail_revision,
            result_revision: self.tail_revision,
            writes_document: false,
            before: self.tail_visible.clone(),
            after: self.tail_visible.clone(),
            payload: None,
        });
        Ok(Reservation { id })
    }

    /// Attaches a pure plan after the engine has answered.  The plan's `before`
    /// projection must be the journal tail, not necessarily the document's
    /// current projection, because an earlier async operation may still be
    /// waiting for TSF.
    pub(crate) fn attach(
        &mut self,
        reservation: Reservation,
        payload: T,
        writes_document: bool,
        before: VisibleState,
        after: VisibleState,
    ) -> Result<OperationId, AdmissionError> {
        let Some(operation) = self
            .operations
            .iter_mut()
            .find(|operation| operation.id == reservation.id)
        else {
            return Err(AdmissionError::ReservationLost);
        };
        if operation.phase != Phase::Reserved
            || operation.activation != self.activation
            || operation.focus != self.focus
            || self.context != Some(operation.context)
        {
            return Err(AdmissionError::ReservationLost);
        }
        if before != self.tail_visible {
            return Err(AdmissionError::ProjectionMismatch);
        }
        operation.phase = Phase::Ready;
        operation.base_revision = self.tail_revision;
        operation.result_revision =
            self.tail_revision
                .wrapping_add(if writes_document { 1 } else { 0 });
        operation.writes_document = writes_document;
        operation.before = before;
        operation.after = after.clone();
        operation.payload = Some(payload);
        self.tail_revision = operation.result_revision;
        self.tail_visible = after;
        Ok(operation.id)
    }

    pub(crate) fn cancel_reservation(
        &mut self,
        reservation: Reservation,
        reason: CancelReason,
    ) -> Vec<Completion<T>> {
        let Some(index) = self.operations.iter().position(|operation| {
            operation.id == reservation.id && operation.phase == Phase::Reserved
        }) else {
            return Vec::new();
        };
        let Some(operation) = self.operations.remove(index) else {
            return Vec::new();
        };
        self.record_terminal(operation.id, TerminalOutcome::Cancelled(reason));
        vec![Completion {
            outcome: TerminalOutcome::Cancelled(reason),
            payload: None,
            ui_lease: None,
        }]
    }

    pub(crate) fn begin_head(&mut self) -> Option<Request<T>>
    where
        T: Clone,
    {
        let operation = self.operations.front_mut()?;
        if operation.phase != Phase::Ready {
            return None;
        }
        let payload = operation.payload.as_ref()?.clone();
        operation.phase = Phase::Requested;
        let epoch = Epoch {
            activation: operation.activation,
            focus: operation.focus,
            context: operation.context,
            revision: operation.base_revision,
        };
        Some(Request {
            ticket: Ticket {
                id: operation.id,
                epoch,
                result_revision: operation.result_revision,
            },
            payload,
            writes_document: operation.writes_document,
        })
    }

    /// Returns the current head's authority without changing its phase.  A
    /// document-unknown recovery owner uses this to feed the same exact
    /// rejection primitive as a callback that already has its request ticket.
    pub(crate) fn head_ticket(&self) -> Option<Ticket> {
        let operation = self.operations.front()?;
        Some(Ticket {
            id: operation.id,
            epoch: Epoch {
                activation: operation.activation,
                focus: operation.focus,
                context: operation.context,
                revision: operation.base_revision,
            },
            result_revision: operation.result_revision,
        })
    }

    /// This must be called before *any* document or UI access from a queued
    /// callback.  A failure means the caller has no permission to inspect a
    /// selection, take a composition handle, or update a popup.
    pub(crate) fn validate_callback(&self, ticket: Ticket) -> Result<(), CancelReason> {
        if !self.active {
            return Err(CancelReason::Deactivated);
        }
        if self.activation != ticket.epoch.activation {
            return Err(CancelReason::ActivationChanged);
        }
        if self.focus != ticket.epoch.focus {
            return Err(CancelReason::FocusChanged);
        }
        if self.context != Some(ticket.epoch.context) {
            return Err(CancelReason::ContextReplaced);
        }
        if self.committed_revision != ticket.epoch.revision {
            return Err(CancelReason::RevisionMismatch);
        }
        match self.operations.front() {
            Some(operation) if operation.id == ticket.id && operation.phase == Phase::Requested => {
                Ok(())
            }
            _ => Err(CancelReason::StaleCallback),
        }
    }

    pub(crate) fn complete_applied(&mut self, ticket: Ticket) -> Option<Completion<T>> {
        self.finish_head(ticket, TerminalOutcome::Applied, true, None)
            .into_iter()
            .next()
    }

    /// Rejects the current operation and every dependent later output.  If a
    /// completed prefix is known, that projection is committed; if the failed
    /// COM call could have partially mutated the document, pass `None` so the
    /// text service cannot pretend the speculative projection is still valid.
    pub(crate) fn reject(
        &mut self,
        ticket: Ticket,
        document_may_have_changed: bool,
        known_prefix: Option<VisibleState>,
    ) -> Vec<Completion<T>> {
        self.cancel_from(
            ticket.id,
            TerminalOutcome::Rejected,
            document_may_have_changed,
            known_prefix,
        )
    }

    pub(crate) fn cancel_ticket(
        &mut self,
        ticket: Ticket,
        reason: CancelReason,
    ) -> Vec<Completion<T>> {
        self.cancel_from(ticket.id, TerminalOutcome::Cancelled(reason), false, None)
    }

    pub(crate) fn cancel_all(&mut self, reason: CancelReason) -> Vec<Completion<T>> {
        let Some(first) = self.operations.front().map(|operation| operation.id) else {
            self.tail_revision = self.committed_revision;
            self.tail_visible = self.committed_visible.clone();
            return Vec::new();
        };
        self.cancel_from(first, TerminalOutcome::Cancelled(reason), false, None)
    }

    /// Deliberately abandons an unknowable document projection.
    ///
    /// This is stronger than [`Self::cancel_all`]: callers use it only after a
    /// failed edit or lifecycle boundary means neither the speculative tail nor
    /// the formerly committed composition projection is safe to reuse. Every
    /// pending operation receives its one terminal outcome first, then the
    /// coordinator moves to the same empty projection held by the text service
    /// and advances the revision so an old callback cannot regain authority.
    pub(crate) fn abandon_projection(&mut self, reason: CancelReason) -> Vec<Completion<T>> {
        let cancelled = self.cancel_all(reason);
        self.committed_revision = self.committed_revision.wrapping_add(1);
        self.tail_revision = self.committed_revision;
        self.committed_visible = VisibleState::empty();
        self.tail_visible = VisibleState::empty();
        self.ui_lease = None;
        cancelled
    }

    pub(crate) fn adopt_ui_lease(&mut self, lease: UiLease) -> bool {
        if self.active
            && self.activation == lease.activation
            && self.focus == lease.focus
            && self.context == Some(lease.context)
            && self.committed_revision == lease.revision
        {
            self.ui_lease = Some(lease);
            true
        } else {
            false
        }
    }

    pub(crate) fn validate_ui_lease(&self, lease: UiLease) -> bool {
        self.ui_lease == Some(lease)
            && self.active
            && self.activation == lease.activation
            && self.focus == lease.focus
            && self.context == Some(lease.context)
            && self.committed_revision == lease.revision
    }

    pub(crate) fn clear_ui_lease(&mut self) {
        self.ui_lease = None;
    }

    pub(crate) fn tail_visible(&self) -> VisibleState {
        self.tail_visible.clone()
    }

    #[cfg(test)]
    pub(crate) fn committed_visible(&self) -> VisibleState {
        self.committed_visible.clone()
    }

    #[cfg(test)]
    pub(crate) fn terminal_records(&self) -> Vec<TerminalRecord> {
        self.terminals.iter().copied().collect()
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.operations.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    fn has_reservation(&self) -> bool {
        self.operations
            .iter()
            .any(|operation| operation.phase == Phase::Reserved)
    }

    fn finish_head(
        &mut self,
        ticket: Ticket,
        outcome: TerminalOutcome,
        document_may_have_changed: bool,
        known_prefix: Option<VisibleState>,
    ) -> Vec<Completion<T>> {
        self.cancel_from(ticket.id, outcome, document_may_have_changed, known_prefix)
    }

    fn cancel_from(
        &mut self,
        id: OperationId,
        first_outcome: TerminalOutcome,
        document_may_have_changed: bool,
        known_prefix: Option<VisibleState>,
    ) -> Vec<Completion<T>> {
        let Some(index) = self
            .operations
            .iter()
            .position(|operation| operation.id == id)
        else {
            return Vec::new();
        };
        if index != 0 {
            // Only the head can become terminal through a request/callback.  A
            // non-head reservation can only be cancelled by a lifecycle event,
            // which enters through `cancel_all` above.
            return Vec::new();
        }

        let Some(first) = self.operations.front() else {
            return Vec::new();
        };
        if first.phase == Phase::Reserved && !matches!(first_outcome, TerminalOutcome::Cancelled(_))
        {
            return Vec::new();
        }
        let Some(first) = self.operations.pop_front() else {
            return Vec::new();
        };

        let mut completed = Vec::new();
        let first_lease = if first_outcome == TerminalOutcome::Applied {
            if first.writes_document {
                self.committed_revision = first.result_revision;
                self.ui_lease = None;
            }
            self.committed_visible = first.after.clone();
            Some(UiLease {
                id: first.id,
                activation: first.activation,
                focus: first.focus,
                context: first.context,
                revision: first.result_revision,
            })
        } else {
            if document_may_have_changed {
                self.committed_revision = first.result_revision;
            }
            if let Some(prefix) = known_prefix {
                self.committed_visible = prefix;
            }
            None
        };
        completed.push(self.complete(first, first_outcome, first_lease));

        if first_outcome != TerminalOutcome::Applied {
            self.ui_lease = None;
            while let Some(dependent) = self.operations.pop_front() {
                completed.push(self.complete(
                    dependent,
                    TerminalOutcome::Cancelled(CancelReason::PredecessorFailed),
                    None,
                ));
            }
            self.tail_revision = self.committed_revision;
            self.tail_visible = self.committed_visible.clone();
            return completed;
        }

        if self.operations.is_empty() {
            self.tail_revision = self.committed_revision;
            self.tail_visible = self.committed_visible.clone();
        }
        completed
    }

    fn complete(
        &mut self,
        mut operation: Operation<T>,
        outcome: TerminalOutcome,
        ui_lease: Option<UiLease>,
    ) -> Completion<T> {
        self.record_terminal(operation.id, outcome);
        // Reservations have no payload and are only terminalized as a
        // cancellation before the engine has supplied a COM payload.  The text
        // service never needs a payload for that branch, so callers use the
        // explicit `ReservationCompletion` path below instead.
        Completion {
            outcome,
            payload: operation.payload.take(),
            ui_lease,
        }
    }

    fn record_terminal(&mut self, id: OperationId, outcome: TerminalOutcome) {
        if self.terminals.len() == self.terminal_capacity {
            let _ = self.terminals.pop_front();
        }
        self.terminals.push_back(TerminalRecord { id, outcome });
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    const FIRST: ContextId = ContextId(1);
    const SECOND: ContextId = ContextId(2);

    fn state(text: &str, has_composition: bool) -> VisibleState {
        VisibleState {
            text: text.to_owned(),
            has_composition,
        }
    }

    fn active(capacity: usize) -> WriteCoordinator<&'static str> {
        let mut journal = WriteCoordinator::new(capacity);
        assert!(journal.activate().is_empty());
        assert!(journal.observe_context(FIRST).is_empty());
        journal
    }

    fn reserve_and_attach(
        journal: &mut WriteCoordinator<&'static str>,
        payload: &'static str,
        writes_document: bool,
        after: VisibleState,
    ) -> Reservation {
        let reservation = journal.reserve(FIRST).expect("reserve");
        let before = journal.tail_visible();
        journal
            .attach(reservation, payload, writes_document, before, after)
            .expect("attach");
        reservation
    }

    fn request(journal: &mut WriteCoordinator<&'static str>) -> Request<&'static str> {
        journal.begin_head().expect("head request")
    }

    #[test]
    fn synchronous_completion_applies_once_and_commits_the_projection() {
        let mut journal = active(2);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let request = request(&mut journal);
        assert_eq!(journal.validate_callback(request.ticket), Ok(()));
        let completion = journal
            .complete_applied(request.ticket)
            .expect("completion");
        assert_eq!(completion.outcome, TerminalOutcome::Applied);
        assert_eq!(completion.payload, Some("first"));
        assert_eq!(journal.committed_visible(), state("a", true));
        assert_eq!(journal.pending_len(), 0);
        assert!(journal.complete_applied(request.ticket).is_none());
        assert_eq!(journal.terminal_records().len(), 1);
    }

    #[test]
    fn synchronous_refusal_then_delayed_async_grant_keeps_the_same_head() {
        let mut journal = active(2);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let request = request(&mut journal);
        // A refused synchronous RequestEditSession does not terminalize this
        // ticket; the edit-session wrapper asks asynchronously with it.
        assert_eq!(journal.pending_len(), 1);
        assert_eq!(journal.validate_callback(request.ticket), Ok(()));
        assert!(journal.complete_applied(request.ticket).is_some());
        assert_eq!(
            journal.terminal_records()[0].outcome,
            TerminalOutcome::Applied
        );
    }

    #[test]
    fn stale_callback_is_document_free_and_cannot_terminalize_twice() {
        let mut journal = active(3);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let first = request(&mut journal);
        assert!(journal.complete_applied(first.ticket).is_some());
        let mut document_accesses = 0usize;
        if journal.validate_callback(first.ticket).is_ok() {
            document_accesses += 1;
        }
        assert_eq!(document_accesses, 0);
        assert!(journal
            .cancel_ticket(first.ticket, CancelReason::StaleCallback)
            .is_empty());
        assert_eq!(journal.terminal_records().len(), 1);
    }

    #[test]
    fn later_outputs_wait_for_the_head_and_use_its_revision_as_a_base() {
        let mut journal = active(3);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let first = request(&mut journal);
        reserve_and_attach(&mut journal, "second", true, state("ab", true));
        assert!(journal.begin_head().is_none());
        assert!(journal.complete_applied(first.ticket).is_some());
        let second = request(&mut journal);
        assert_eq!(second.payload, "second");
        assert_eq!(journal.validate_callback(second.ticket), Ok(()));
        assert!(journal.complete_applied(second.ticket).is_some());
        assert_eq!(journal.committed_visible(), state("ab", true));
    }

    #[test]
    fn focus_loss_cancels_queued_work_and_invalidates_ui() {
        let mut journal = active(2);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let first = request(&mut journal);
        let applied = journal.complete_applied(first.ticket).expect("applied");
        let lease = applied.ui_lease.expect("lease");
        assert!(journal.adopt_ui_lease(lease));
        reserve_and_attach(&mut journal, "second", true, state("ab", true));
        let cancelled = journal.focus_changed();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            cancelled[0].outcome,
            TerminalOutcome::Cancelled(CancelReason::FocusChanged)
        );
        assert!(!journal.validate_ui_lease(lease));
    }

    #[test]
    fn deactivation_cancels_a_delayed_callback() {
        let mut journal = active(2);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let request = request(&mut journal);
        let cancelled = journal.deactivate();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            journal.validate_callback(request.ticket),
            Err(CancelReason::Deactivated)
        );
    }

    #[test]
    fn composition_termination_cancels_work_and_resets_visible_projection() {
        let mut journal = active(2);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let request = request(&mut journal);
        let cancelled = journal.composition_terminated();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            cancelled[0].outcome,
            TerminalOutcome::Cancelled(CancelReason::CompositionTerminated)
        );
        assert_eq!(
            journal.validate_callback(request.ticket),
            Err(CancelReason::RevisionMismatch)
        );
        assert_eq!(journal.tail_visible(), VisibleState::empty());
    }

    #[test]
    fn activation_and_context_replacement_cancel_old_context_work() {
        let mut journal = active(2);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let request = request(&mut journal);
        let cancelled = journal.observe_context(SECOND);
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            cancelled[0].outcome,
            TerminalOutcome::Cancelled(CancelReason::ContextReplaced)
        );
        assert_eq!(
            journal.validate_callback(request.ticket),
            Err(CancelReason::ContextReplaced)
        );
        assert!(journal.activate().is_empty());
    }

    #[test]
    fn base_revision_mismatch_rejects_document_access() {
        let mut journal = active(2);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let request = request(&mut journal);
        let cancelled = journal.document_changed();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            journal.validate_callback(request.ticket),
            Err(CancelReason::RevisionMismatch)
        );
    }

    #[test]
    fn complete_request_rejection_rejects_head_and_cancels_dependents() {
        let mut journal = active(3);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let first = request(&mut journal);
        reserve_and_attach(&mut journal, "second", true, state("ab", true));
        let terminal = journal.reject(first.ticket, false, None);
        assert_eq!(terminal.len(), 2);
        assert_eq!(terminal[0].outcome, TerminalOutcome::Rejected);
        assert_eq!(
            terminal[1].outcome,
            TerminalOutcome::Cancelled(CancelReason::PredecessorFailed)
        );
        assert_eq!(journal.committed_visible(), VisibleState::empty());
    }

    #[test]
    fn callback_failure_before_application_preserves_the_committed_projection() {
        let mut journal = active(2);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let request = request(&mut journal);
        let terminal = journal.reject(request.ticket, false, None);
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].outcome, TerminalOutcome::Rejected);
        assert_eq!(journal.committed_visible(), VisibleState::empty());
    }

    #[test]
    fn failure_after_a_known_prefix_commits_only_that_prefix_and_cancels_dependents() {
        let mut journal = active(3);
        reserve_and_attach(&mut journal, "first", true, state("ab", true));
        let first = request(&mut journal);
        reserve_and_attach(&mut journal, "second", true, state("abc", true));
        let terminal = journal.reject(first.ticket, true, Some(state("a", true)));
        assert_eq!(terminal.len(), 2);
        assert_eq!(journal.committed_visible(), state("a", true));
        assert_eq!(journal.pending_len(), 0);
    }

    #[test]
    fn queue_capacity_is_reserved_before_engine_work_and_overflow_is_rejected() {
        let mut journal = active(1);
        let reservation = journal.reserve(FIRST).expect("first reservation");
        assert_eq!(journal.reserve(FIRST), Err(AdmissionError::Full));
        let before = journal.tail_visible();
        journal
            .attach(reservation, "first", true, before, state("a", true))
            .expect("attach");
    }

    #[test]
    fn pending_commit_undo_payload_fences_later_admission_even_with_capacity() {
        let mut journal = active(2);
        reserve_and_attach(&mut journal, "commit-undo", true, state("a", true));
        let _request = request(&mut journal);

        assert!(journal.any_payload(|payload| *payload == "commit-undo"));
        assert!(
            journal.can_admit_for_context(FIRST),
            "ordinary capacity still has a free slot"
        );
        assert!(
            !journal.can_admit_for_context_unless(FIRST, |payload| *payload == "commit-undo"),
            "a queued exact undo must fence later input despite free capacity"
        );
    }

    #[test]
    fn non_cancellation_cannot_consume_a_reserved_head() {
        let mut journal = active(2);
        let reservation = journal.reserve(FIRST).expect("reservation");

        // A malformed callback terminal must leave the reservation available
        // for the engine/error owner that is responsible for cancelling it.
        assert!(journal
            .cancel_from(reservation.id, TerminalOutcome::Rejected, false, None)
            .is_empty());
        assert_eq!(journal.pending_len(), 1);
        assert!(journal.terminal_records().is_empty());

        let cancelled = journal.cancel_reservation(reservation, CancelReason::EngineUnavailable);
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            cancelled[0].outcome,
            TerminalOutcome::Cancelled(CancelReason::EngineUnavailable)
        );
        assert_eq!(journal.pending_len(), 0);
        assert_eq!(journal.terminal_records().len(), 1);
        assert!(journal
            .cancel_reservation(reservation, CancelReason::EngineUnavailable)
            .is_empty());
        assert_eq!(journal.terminal_records().len(), 1);
    }

    #[test]
    fn every_accepted_operation_gets_exactly_one_terminal_outcome() {
        let mut journal = active(3);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let first = request(&mut journal);
        reserve_and_attach(&mut journal, "second", true, state("ab", true));
        let _ = journal.reject(first.ticket, false, None);
        let records = journal.terminal_records();
        assert_eq!(records.len(), 2);
        assert_ne!(records[0].id, records[1].id);
        assert_eq!(records[0].outcome, TerminalOutcome::Rejected);
        assert_eq!(
            records[1].outcome,
            TerminalOutcome::Cancelled(CancelReason::PredecessorFailed)
        );
    }

    #[test]
    fn candidate_and_layout_lease_is_invalidated_after_rejection_or_cancellation() {
        let mut journal = active(3);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let first = request(&mut journal);
        let applied = journal.complete_applied(first.ticket).expect("applied");
        let lease = applied.ui_lease.expect("lease");
        assert!(journal.adopt_ui_lease(lease));

        reserve_and_attach(&mut journal, "second", true, state("ab", true));
        let second = request(&mut journal);
        let _ = journal.reject(second.ticket, false, None);
        assert!(!journal.validate_ui_lease(lease));

        reserve_and_attach(&mut journal, "third", false, state("a", true));
        let third = request(&mut journal);
        let applied = journal.complete_applied(third.ticket).expect("applied");
        let lease = applied.ui_lease.expect("lease");
        assert!(journal.adopt_ui_lease(lease));
        let _ = journal.focus_changed();
        assert!(!journal.validate_ui_lease(lease));
    }

    #[test]
    fn abandoning_an_unknown_projection_resets_the_tail_and_invalidates_old_callbacks() {
        let mut journal = active(4);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let first = request(&mut journal);
        assert!(journal.complete_applied(first.ticket).is_some());

        reserve_and_attach(&mut journal, "stale", true, state("ab", true));
        let stale = request(&mut journal);
        let later = journal.reserve(FIRST).expect("later reservation");

        let cancelled = journal.abandon_projection(CancelReason::RevisionMismatch);
        assert_eq!(cancelled.len(), 2);
        assert_eq!(
            cancelled[0].outcome,
            TerminalOutcome::Cancelled(CancelReason::RevisionMismatch)
        );
        assert_eq!(
            cancelled[1].outcome,
            TerminalOutcome::Cancelled(CancelReason::PredecessorFailed)
        );
        assert_eq!(
            journal.validate_callback(stale.ticket),
            Err(CancelReason::RevisionMismatch)
        );
        assert_eq!(journal.committed_visible(), VisibleState::empty());
        assert_eq!(journal.tail_visible(), VisibleState::empty());
        assert_eq!(journal.pending_len(), 0);

        let rescue = journal.reserve(FIRST).expect("rescue reservation");
        let before = journal.tail_visible();
        assert_eq!(before, VisibleState::empty());
        journal
            .attach(rescue, "rescue", true, before, VisibleState::empty())
            .expect("empty rescue attaches");
        assert!(journal
            .cancel_reservation(later, CancelReason::StaleCallback)
            .is_empty());
    }

    #[test]
    fn engine_failure_cancels_backlog_and_leaves_committed_projection_for_rescue() {
        let mut journal = active(4);
        reserve_and_attach(&mut journal, "first", true, state("a", true));
        let first = request(&mut journal);
        assert!(journal.complete_applied(first.ticket).is_some());

        reserve_and_attach(&mut journal, "requested", true, state("ab", true));
        let requested = request(&mut journal);
        let later = journal.reserve(FIRST).expect("later reservation");

        let cancelled = journal.cancel_all(CancelReason::EngineUnavailable);
        assert_eq!(cancelled.len(), 2);
        assert_eq!(
            cancelled[0].outcome,
            TerminalOutcome::Cancelled(CancelReason::EngineUnavailable)
        );
        assert_eq!(
            cancelled[1].outcome,
            TerminalOutcome::Cancelled(CancelReason::PredecessorFailed)
        );
        assert_eq!(journal.committed_visible(), state("a", true));
        assert_eq!(journal.tail_visible(), state("a", true));
        assert_eq!(journal.pending_len(), 0);
        assert_eq!(
            journal.validate_callback(requested.ticket),
            Err(CancelReason::StaleCallback)
        );

        let rescue = journal.reserve(FIRST).expect("rescue reservation");
        let before = journal.tail_visible();
        assert_eq!(before, state("a", true));
        journal
            .attach(rescue, "finalize", true, before, VisibleState::empty())
            .expect("finalization attaches to committed projection");
        assert!(journal
            .cancel_reservation(later, CancelReason::StaleCallback)
            .is_empty());
    }

    #[test]
    fn shift_latin_backspace_retype_plans_commit_in_order_and_never_aiuoeo() {
        let mut journal = active(4);
        for (payload, after) in [
            ("type-aiueo", "AIUEO"),
            ("shift-backspace", "AIUE"),
            ("retype-o", "AIUEO"),
        ] {
            reserve_and_attach(&mut journal, payload, true, state(after, true));
            let request = request(&mut journal);
            assert_eq!(journal.validate_callback(request.ticket), Ok(()));
            assert!(journal.complete_applied(request.ticket).is_some());
            assert_ne!(
                journal.committed_visible().text,
                "AIUOEO",
                "a host-stolen Backspace projection must not become committed"
            );
        }
        assert_eq!(journal.committed_visible(), state("AIUEO", true));
        assert_eq!(journal.tail_visible(), state("AIUEO", true));

        let stolen = journal.reserve(FIRST).expect("host-stolen reservation");
        let attach = journal.attach(
            stolen,
            "host-stolen-aiuoeo",
            true,
            state("AIUE", true),
            state("AIUOEO", true),
        );
        assert_eq!(attach, Err(AdmissionError::ProjectionMismatch));
        assert_eq!(journal.committed_visible(), state("AIUEO", true));
    }
}
