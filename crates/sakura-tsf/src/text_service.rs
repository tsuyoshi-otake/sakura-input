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

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{
    E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_POINTER, E_UNEXPECTED, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Variant::{
    VariantClear, VARIANT, VT_EMPTY, VT_NULL, VT_TYPEMASK, VT_UNKNOWN,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_KANJI;
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, IEnumTfDisplayAttributeInfo, ITfCandidateList, ITfCategoryMgr,
    ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfContextView,
    ITfDisplayAttributeInfo, ITfDisplayAttributeProvider, ITfDisplayAttributeProvider_Impl,
    ITfFnReconversion, ITfFnReconversion_Impl, ITfFunctionProvider, ITfFunctionProvider_Impl,
    ITfFunction_Impl, ITfInputScope, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr,
    ITfLangBarItem, ITfLangBarItemButton, ITfLangBarItemButton_Impl, ITfLangBarItemMgr,
    ITfLangBarItemSink, ITfLangBarItem_Impl, ITfMenu, ITfRange, ITfSource, ITfSourceSingle,
    ITfSource_Impl, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl,
    ITfTextInputProcessor_Impl, ITfTextLayoutSink, ITfTextLayoutSink_Impl, ITfThreadMgr,
    InputScope as TfInputScope, TfLBIClick, TfLayoutCode, GUID_LBI_INPUTMODE, GUID_PROP_INPUTSCOPE,
    TF_LANGBARITEMINFO, TF_LBI_STYLE_BTN_BUTTON, TF_LBI_STYLE_BTN_MENU,
    TF_LBI_STYLE_HIDDENSTATUSCONTROL, TF_LBI_STYLE_TEXTCOLORICON, TF_PRESERVEDKEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, KillTimer, PostMessageW,
    RegisterClassW, SetTimer, SetWindowLongPtrW, GWLP_USERDATA, WINDOW_EX_STYLE, WM_APP, WM_TIMER,
    WNDCLASSW, WS_OVERLAPPED,
};
use windows_core::{
    implement, Error, IUnknown, IUnknownImpl, Interface, OutRef, Ref, Result, BOOL, BSTR, GUID,
};

use sakura_proto::{
    AiTextOperation, AiTextStatus, CandidateKind, CandidateList as EngineCandidateList, InputScope,
    KeyCode, KeyInput, Mode, Modifiers, Output, Preedit, ScreenRect, UndoCommitOutcome,
    MAX_PREEDIT_BYTES,
};
use sakura_reg::{
    user_preferences::{read_ai_text_key, AiTextKey},
    CLSID_SAKURA_TSF, GUID_PRESERVEDKEY_IME_TOGGLE, TEXT_SERVICE_DESCRIPTION,
};

use crate::candidate_ui::CandidateUi;
use crate::composition::{self, DocumentEdit, Update};
use crate::diagnostic_ring;
use crate::display_attributes;
use crate::edit_session;
use crate::engine::{AiTextPoll, AiTextRecord, AiTextResult, Answer, Engine};
use crate::engine_recovery::{
    EngineRecoveryFence, RecoveryKeyDisposition, RecoveryStart, RecoveryTerminal, RecoveryToken,
};
use crate::exports::{on_object_created, on_object_destroyed};
use crate::key_handler;
use crate::mode_item::{self, MenuCommand};
use crate::reconversion;
use crate::write_coordinator::{
    AdmissionError, CancelReason, Completion, ContextId, Reservation, TerminalOutcome, Ticket,
    UiLease, VisibleState, WriteCoordinator, DEFAULT_WRITE_CAPACITY,
};

const DEFERRED_WORK_MESSAGE: u32 = WM_APP + 29;
const AI_TEXT_TIMER_ID: usize = 2;
const AI_TEXT_POLL_MS: u32 = 50;
const DEFERRED_WINDOW_CLASS: windows_core::PCWSTR = windows_core::w!(r##"SakuraInputTsfDeferred"##);

/// What the text service holds while it is attached to a thread manager.
///
/// All external registrations made during activation live or die together, so
/// deactivation can finish every cleanup attempt from the exact COM interfaces
/// and preserved-key record that this activation created.
#[derive(Debug)]
struct Activation {
    thread_mgr: ITfThreadMgr,
    keystroke_mgr: ITfKeystrokeMgr,
    function_source: ITfSourceSingle,
    lang_bar_mgr: ITfLangBarItemMgr,
    lang_bar_item: ITfLangBarItem,
    client_id: u32,
    preserved_key: &'static PreservedKeyRegistration,
}

/// One TSF-owned physical key binding. The key event itself is normalized
/// separately so the engine never has to depend on TSF registration details.
#[derive(Debug)]
struct PreservedKeyRegistration {
    guid: GUID,
    key: TF_PRESERVEDKEY,
    description: &'static [u16],
}

const IME_TOGGLE_PRESERVED_KEY_DESCRIPTION: &[u16] = &[
    0x0053, 0x0061, 0x006b, 0x0075, 0x0072, 0x0061, 0x0020, 0x0049, 0x006e, 0x0070, 0x0075, 0x0074,
    0x0020, 0x0049, 0x004d, 0x0045, 0x0020, 0x0074, 0x006f, 0x0067, 0x0067, 0x006c, 0x0065,
];

/// Preserve only the physical 半角/全角 key. Henkan, Muhenkan, Kana, and
/// Alt+` stay on the ordinary key-event path because their engine semantics
/// depend on the current state and selected keymap preset.
static PRESERVED_KEY_REGISTRATIONS: [PreservedKeyRegistration; 1] = [PreservedKeyRegistration {
    guid: GUID_PRESERVEDKEY_IME_TOGGLE,
    key: TF_PRESERVEDKEY {
        uVKey: VK_KANJI.0 as u32,
        uModifiers: 0,
    },
    description: IME_TOGGLE_PRESERVED_KEY_DESCRIPTION,
}];

fn preserved_key_registrations() -> &'static [PreservedKeyRegistration] {
    &PRESERVED_KEY_REGISTRATIONS
}

/// Returns the one engine input TSF is allowed to dispatch by GUID. Unknown
/// GUIDs deliberately have no mapping and are returned to the host.
fn preserved_key_input(guid: &GUID) -> Option<KeyInput> {
    if *guid != GUID_PRESERVEDKEY_IME_TOGGLE {
        return None;
    }
    Some(KeyInput {
        code: KeyCode::HankakuZenkaku,
        ch: None,
        modifiers: sakura_proto::Modifiers::NONE,
        repeat: false,
        test_only: false,
    })
}

fn preserve_registered_key(
    keystroke_mgr: &ITfKeystrokeMgr,
    client_id: u32,
    registration: &PreservedKeyRegistration,
) -> Result<()> {
    // SAFETY: the manager belongs to this active thread, and the GUID, key,
    // and UTF-16 description references remain valid for the full call.
    unsafe {
        keystroke_mgr.PreserveKey(
            client_id,
            &registration.guid,
            &registration.key,
            registration.description,
        )
    }
}

fn unpreserve_registered_key(
    keystroke_mgr: &ITfKeystrokeMgr,
    registration: &PreservedKeyRegistration,
) -> Result<()> {
    // SAFETY: `registration` is retained only after this activation's
    // successful `PreserveKey`, so this reverses the exact binding it created.
    unsafe { keystroke_mgr.UnpreserveKey(&registration.guid, &registration.key) }
}

/// What the document is showing, and the handle that lets us change it.
///
/// `text` is a copy of the visible preedit, not the preedit itself — the
/// engine owns that. It exists so that a lost engine can still be
/// finalized into real text without a round trip that will not complete.
/// `handle` is `Some` only while the document actually has a composition
/// open, which the host can end without asking us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompositionFlight {
    id: u64,
    lifecycle: u64,
}

/// A COM identity captured while the canonical composition handle is still
/// retained. Keeping just the identity lets `OnCompositionTerminated` prove it
/// is acknowledging this exact EndComposition without extending any COM borrow
/// across the host call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompositionIdentity(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExpectedSelfTermination {
    flight: CompositionFlight,
    composition: CompositionIdentity,
}

/// The composition handle stays in `CompositionState` while a COM call is in
/// progress.  A callback receives only a clone plus this ownership token.  A
/// lifecycle callback clears the token before it changes state, so a delayed
/// callback can never put its retained handle back after the lifecycle event
/// retired it.
#[derive(Debug, Default)]
struct CompositionWriteOwner {
    next: u64,
    lifecycle: u64,
    in_flight: Option<CompositionFlight>,
}

impl CompositionWriteOwner {
    fn begin(&mut self) -> Option<CompositionFlight> {
        if self.in_flight.is_some() {
            return None;
        }
        self.next = self.next.wrapping_add(1).max(1);
        let flight = CompositionFlight {
            id: self.next,
            lifecycle: self.lifecycle,
        };
        self.in_flight = Some(flight);
        Some(flight)
    }

    fn owns(&self, flight: CompositionFlight) -> bool {
        self.in_flight == Some(flight)
    }

    fn finish(&mut self, flight: CompositionFlight) -> bool {
        if !self.owns(flight) {
            return false;
        }
        self.in_flight = None;
        true
    }

    /// Whether the lifecycle that admitted `flight` is still authoritative.
    /// This remains true after a successful `finish`: the caller needs this
    /// distinction if its document edit succeeded but the journal terminal was
    /// concurrently removed. A lifecycle invalidation advances the epoch before
    /// it changes composition state, making that state authoritative instead.
    fn lifecycle_is_current(&self, flight: CompositionFlight) -> bool {
        self.lifecycle == flight.lifecycle
    }

    fn invalidate(&mut self) {
        self.lifecycle = self.lifecycle.wrapping_add(1);
        self.in_flight = None;
    }
}

#[derive(Debug)]
struct CompositionState {
    text: String,
    handle: Option<ITfComposition>,
    context: Option<ITfContext>,
    /// A failed COM mutation can leave the precise document projection
    /// unknowable.  Until the host terminates or focus leaves that composition,
    /// passing keys through is safer than issuing another speculative edit.
    known: bool,
    write_owner: CompositionWriteOwner,
    expected_self_termination: Option<ExpectedSelfTermination>,
}

impl Default for CompositionState {
    fn default() -> Self {
        Self {
            text: String::new(),
            handle: None,
            context: None,
            known: true,
            write_owner: CompositionWriteOwner::default(),
            expected_self_termination: None,
        }
    }
}

#[derive(Debug)]
struct PendingCandidates {
    context: ITfContext,
    candidates: EngineCandidateList,
    lease: UiLease,
}

#[derive(Debug, Clone)]
struct WritePlan {
    updates: Vec<Update>,
    before: VisibleState,
    after: VisibleState,
}

#[derive(Debug, Clone)]
enum CandidateEffect {
    Show(EngineCandidateList),
    Hide,
}

/// The COM-owning half of a journal entry.  The coordinator owns its ordering,
/// terminal outcome and epochs; this payload is read only after it grants the
/// callback permission to touch the document.
#[derive(Debug, Clone)]
struct PendingWrite {
    context: ITfContext,
    plan: WritePlan,
    target_range: Option<ITfRange>,
    query_layout: bool,
    synchronous_first: bool,
    candidates: CandidateEffect,
    /// The engine has a restored preedit and a pending exact-text undo record
    /// that must receive an explicit host outcome at journal terminalization.
    undo_commit: bool,
    /// Identifies the one engine-timeout finalizer whose lifetime fences host
    /// keys. Ordinary engine outputs and focus finalizers carry no token.
    engine_recovery: Option<RecoveryToken>,
    /// Selected-text AI writes must still target the exact text that was sent
    /// to the provider. The range and source are re-read under the eventual
    /// write cookie immediately before any host mutation.
    ai_source_validation: Option<(ITfRange, String)>,
    /// Developer-history data is terminalized only after the host write has a
    /// journal outcome, never merely when the provider returned a result.
    ai_record: Option<PendingAiRecord>,
}

#[derive(Debug, Clone)]
enum AiTextTarget {
    Composition,
    Selection(ITfRange),
}

#[derive(Debug, Clone)]
struct PendingAiText {
    job: u64,
    operation: AiTextOperation,
    context: ITfContext,
    source: String,
    target: AiTextTarget,
}

#[derive(Debug, Default)]
struct AiTextState {
    pending: Option<PendingAiText>,
}

#[derive(Debug, Clone)]
struct PendingAiRecord {
    operation: AiTextOperation,
    source: String,
    result: AiTextResult,
}

struct OutputSubmission {
    target_range: Option<ITfRange>,
    synchronous_first: bool,
    start_now: bool,
    ai_record: Option<PendingAiRecord>,
}

#[derive(Debug)]
#[allow(dead_code)]
struct UnknownUndoTerminalization<T> {
    completions: Vec<Completion<T>>,
    has_undo: bool,
    journal_drained: bool,
    retry_allowed: bool,
    settlement_confirmed: bool,
    disconnect_required: bool,
}

/// Performs the document-free journal half of an exact undo failure after a
/// host call may already have changed the document.  This helper deliberately
/// knows nothing about COM: the caller has already failed/retired its concrete
/// `ITfContext` composition flight, and supplies only the payload predicate and
/// engine settlement closure.
///
/// The ticket is rejected with `document_may_have_changed = true`, dependent
/// outputs are cancelled, the one pending undo payload is settled as Unknown,
/// and an unconfirmed settlement requires link retirement.  The returned
/// completions are marked as already owning their undo outcome so the caller's
/// ordinary candidate/UI cleanup cannot send a second engine acknowledgement.
fn terminalize_unknown_undo_after_document_access<T>(
    journal: &mut WriteCoordinator<T>,
    ticket: Ticket,
    is_undo: impl FnMut(&T) -> bool,
    mut settle: impl FnMut(UndoCommitOutcome) -> bool,
) -> UnknownUndoTerminalization<T> {
    let mut completions = journal.reject(ticket, true, None);
    if !journal.is_empty() {
        // A malformed/stale ticket must not leave a later payload retryable
        // after the document boundary became unknown.
        completions.extend(journal.cancel_all(CancelReason::RevisionMismatch));
    }
    let has_undo = completions
        .iter()
        .filter_map(|completion| completion.payload.as_ref())
        .any(is_undo);
    let settlement_confirmed = !has_undo || settle(UndoCommitOutcome::Unknown);
    let journal_drained = journal.is_empty();
    UnknownUndoTerminalization {
        completions,
        has_undo,
        journal_drained,
        retry_allowed: false,
        settlement_confirmed,
        disconnect_required: !journal_drained || (has_undo && !settlement_confirmed),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeFence {
    /// The callback may classify the host scope and ask the engine for a
    /// throwaway answer.  No live TSF/engine state has been touched.
    Open,
    /// An exact-undo terminal handoff is still authoritative.  The host must
    /// not edit around it, and Probe must not make the handoff progress.
    Busy,
    /// The host context differs from the journal's current context. The real
    /// callback must own cancellation/disconnect before applying this key;
    /// Probe asks the engine for a fresh-session clone and remains read-only.
    ContextReplacement,
    /// A lifecycle or bounded-admission fence means the host owns the key.
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeAction {
    Busy,
    Declined,
    Ask { fresh_context: bool },
}

fn probe_action(fence: ProbeFence) -> ProbeAction {
    match fence {
        ProbeFence::Busy => ProbeAction::Busy,
        ProbeFence::Declined => ProbeAction::Declined,
        ProbeFence::Open => ProbeAction::Ask {
            fresh_context: false,
        },
        ProbeFence::ContextReplacement => ProbeAction::Ask {
            fresh_context: true,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RealFenceAction {
    /// The deferred document-free owner must settle before this key can be
    /// considered. The physical key is consumed by that terminal handoff.
    DeferredTerminalization,
    /// An exact-undo payload is still in flight, so the host must not edit
    /// around its authoritative document boundary.
    Consume,
    /// The host owns the key because the engine/write path is blocked.
    Decline,
    /// The old context was retired successfully; continue through scope,
    /// reservation, and Apply for this same physical key.
    ReplaceAndApply,
    /// No fence prevents the ordinary Apply path.
    Apply,
}

/// Canonical real-key fence priority. The test-only path maps the same
/// priority to a read-only `ProbeFence`; only the replacement action proceeds
/// to cleanup and then applies the current key.
/// Space / 変換 must convert a live reading. Ctrl/Alt chords stay host-owned
/// (Ctrl+Space is IntelliSense). Shift+Space is still conversion.
fn is_conversion_trigger_key(key: KeyInput) -> bool {
    !key.modifiers.ctrl()
        && !key.modifiers.alt()
        && matches!(key.code, KeyCode::Space | KeyCode::Henkan)
}

fn keep_live_composition_for_convert(key: KeyInput, live_composition: bool) -> bool {
    live_composition && is_conversion_trigger_key(key)
}

/// Electron/Cursor can deliver Space to a different `ITfContext` than the one
/// holding the underlined reading. Treating that as replacement disconnects
/// the composing engine session and then inserts U+3000. Convert keys keep
/// the live session instead.
fn journal_replacement_applies(
    key: KeyInput,
    live_composition: bool,
    journal_replacement: bool,
) -> bool {
    journal_replacement && !keep_live_composition_for_convert(key, live_composition)
}

fn decide_real_fence(
    deferred_terminalization: bool,
    undo_write_pending: bool,
    engine_recovery_pending: bool,
    input_blocked: bool,
    context_replacement: bool,
) -> RealFenceAction {
    if deferred_terminalization {
        RealFenceAction::DeferredTerminalization
    } else if undo_write_pending || engine_recovery_pending {
        RealFenceAction::Consume
    } else if input_blocked {
        RealFenceAction::Decline
    } else if context_replacement {
        RealFenceAction::ReplaceAndApply
    } else {
        RealFenceAction::Apply
    }
}

/// Decides whether a test-only key may run without touching the live write
/// transaction.  This is deliberately generic over the journal payload so the
/// same state-machine decision can be tested without fabricating an
/// `ITfContext`.  It only reads its inputs; settlement, cancellation, context
/// observation, and reservation remain real-key responsibilities.
fn decide_probe_fence<T>(
    undo_terminalization: Option<UndoCommitOutcome>,
    writes: &WriteCoordinator<T>,
    context: ContextId,
    is_undo: impl FnMut(&T) -> bool,
    engine_recovery_pending: bool,
    input_blocked: bool,
) -> ProbeFence {
    if undo_terminalization.is_some() || writes.any_payload(is_undo) || engine_recovery_pending {
        ProbeFence::Busy
    } else if input_blocked {
        ProbeFence::Declined
    } else if writes.is_context_replacement(context) {
        ProbeFence::ContextReplacement
    } else if !writes.can_admit_for_context(context) {
        ProbeFence::Declined
    } else {
        ProbeFence::Open
    }
}

/// Deferred work is held, rather than drained, while candidate UI COM is in
/// progress. `BeginUIElement` and `UpdateUIElement` can pump the hidden-window
/// message, so an inner dispatch must never replace the controller temporarily
/// removed by the outer candidate operation.
#[derive(Debug)]
struct DeferredWork<T> {
    write: bool,
    layout: bool,
    /// One bounded attempt to retire a geometry query whose asynchronous
    /// callback could not borrow the layout state. The out-of-band claim on
    /// `TextService` remains the lifecycle owner if this message is refused.
    layout_abandon: bool,
    focus_loss: bool,
    /// One document-free retry after the engine has committed but a re-entrant
    /// borrow temporarily prevented retirement of the canonical projection.
    /// This is deliberately a bit rather than an unbounded queue: one posted
    /// message owns the retry, and a second refusal transfers ownership to
    /// lifecycle teardown instead of spinning on the host thread.
    focus_reconcile: bool,
    end_candidates: bool,
    candidates: Option<T>,
}

impl<T> Default for DeferredWork<T> {
    fn default() -> Self {
        Self {
            write: false,
            layout: false,
            layout_abandon: false,
            focus_loss: false,
            focus_reconcile: false,
            end_candidates: false,
            candidates: None,
        }
    }
}

impl<T> DeferredWork<T> {
    fn has_work(&self) -> bool {
        self.write
            || self.layout
            || self.layout_abandon
            || self.focus_loss
            || self.focus_reconcile
            || self.end_candidates
            || self.candidates.is_some()
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    fn retain_candidate_end(&mut self) {
        self.candidates = None;
        self.layout = false;
        self.layout_abandon = false;
        self.end_candidates = true;
    }
}

/// The COM-free part of hidden-window message ownership. A posted message is
/// consumed exactly once; when it arrives during candidate COM, its work stays
/// pending and the outer operation schedules one replacement message only after
/// it has restored the controller.
#[derive(Debug)]
struct DeferredDispatchState<T> {
    posted: bool,
    work: DeferredWork<T>,
}

impl<T> Default for DeferredDispatchState<T> {
    fn default() -> Self {
        Self {
            posted: false,
            work: DeferredWork::default(),
        }
    }
}

impl<T> DeferredDispatchState<T> {
    fn take_for_dispatch(&mut self, candidate_operation_active: bool) -> Option<DeferredWork<T>> {
        self.posted = false;
        if candidate_operation_active {
            return None;
        }
        Some(core::mem::take(&mut self.work))
    }

    /// Returns whether the outer operation must post one replacement message.
    /// If work was queued while the COM call ran, `posted` is already true and
    /// its message is left alone. If the nested message consumed that post, this
    /// returns true exactly once after the controller is back in its slot.
    fn needs_repost_after_candidate_operation(&self) -> bool {
        !self.posted && self.work.has_work()
    }
}

/// Focus loss has two materially different recovery boundaries. Before the
/// hidden-window dispatch begins, the engine still agrees with the visible
/// composition and focus regain may retain it. Once `EngineCommitStarted`, the
/// engine may already be empty while the document callback is still pending, so
/// focus regain must retire the projection without issuing another edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FocusFinalizationPhase {
    #[default]
    Idle,
    DeferredQueued,
    EngineCommitStarted,
    /// The initial document-free retirement was refused because a re-entrant
    /// callback still held `CompositionState`. One hidden-window message now
    /// owns a retry after that callback unwinds.
    ReconciliationQueued,
    /// The one deferred retry could not be posted or was itself refused. No
    /// polling is allowed; the next focus lifecycle callback or detach/drop
    /// owns the next document-free retirement attempt, and the service remains
    /// input-blocked meanwhile.
    ReconciliationAwaitingLifecycle,
}

impl FocusFinalizationPhase {
    fn blocks_input(self) -> bool {
        self != Self::Idle
    }
}

#[derive(Debug, Default)]
struct DeferredState {
    window: Option<HWND>,
    dispatch: DeferredDispatchState<PendingCandidates>,
    focus_finalization: FocusFinalizationPhase,
    /// A focus boundary cancelled an engine output that had already acquired a
    /// document payload.  The engine may therefore be ahead of the document
    /// even before the deferred focus-finalization request reaches
    /// `Engine::commit`; focus regain must retire that projection instead of
    /// retaining it merely because it is still locally known.
    focus_reconciliation_required: bool,
}

impl DeferredState {
    /// Gives exactly one hidden-window message ownership of a deferred
    /// document-free retirement. `true` means the caller must ensure a message
    /// is posted; `false` means an existing retry or lifecycle terminal owner
    /// is already authoritative.
    fn queue_focus_reconciliation(&mut self) -> bool {
        match self.focus_finalization {
            FocusFinalizationPhase::EngineCommitStarted => {
                self.focus_finalization = FocusFinalizationPhase::ReconciliationQueued;
                self.focus_reconciliation_required = true;
                self.dispatch.work.focus_reconcile = true;
                true
            }
            FocusFinalizationPhase::Idle if self.focus_reconciliation_required => {
                self.focus_finalization = FocusFinalizationPhase::ReconciliationQueued;
                self.focus_reconciliation_required = true;
                self.dispatch.work.focus_reconcile = true;
                true
            }
            FocusFinalizationPhase::ReconciliationQueued
            | FocusFinalizationPhase::ReconciliationAwaitingLifecycle => false,
            FocusFinalizationPhase::Idle | FocusFinalizationPhase::DeferredQueued => false,
        }
    }

    /// A queued retry must not disappear if its message cannot be delivered.
    /// There is intentionally no immediate retry here: lifecycle teardown is
    /// the explicit next owner after the current re-entrant stack has unwound.
    fn handoff_focus_reconciliation_to_lifecycle(&mut self) {
        if self.focus_finalization == FocusFinalizationPhase::ReconciliationQueued
            || self.dispatch.work.focus_reconcile
        {
            self.focus_finalization = FocusFinalizationPhase::ReconciliationAwaitingLifecycle;
            self.focus_reconciliation_required = true;
            self.dispatch.work.focus_reconcile = false;
        }
    }

    /// Destroying the hidden window invalidates every queued message. An
    /// engine-started finalization or reconciliation retry is therefore handed
    /// to `detach`, which performs the document-free retirement directly and
    /// reports failure to its caller rather than silently stranding the state.
    fn handoff_focus_finalization_to_lifecycle(&mut self) {
        if matches!(
            self.focus_finalization,
            FocusFinalizationPhase::EngineCommitStarted
                | FocusFinalizationPhase::ReconciliationQueued
                | FocusFinalizationPhase::ReconciliationAwaitingLifecycle
        ) || self.focus_reconciliation_required
            || self.dispatch.work.focus_reconcile
        {
            self.focus_finalization = FocusFinalizationPhase::ReconciliationAwaitingLifecycle;
            self.focus_reconciliation_required = true;
            self.dispatch.work.focus_reconcile = false;
        }
    }
}

#[derive(Debug)]
struct LayoutSubscription {
    source: ITfSource,
    context: ITfContext,
    lease: UiLease,
    cookie: u32,
}

/// Every geometry branch is terminal. `QueryQueued` has exactly one owner:
/// the edit-session closure that changes it to one of the other states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum GeometryPhase {
    #[default]
    Idle,
    QueryQueued,
    WaitingForLayout,
    Ready,
    Unavailable,
}

/// COM-free identity of the asynchronous geometry query that owns
/// `GeometryPhase::QueryQueued`. It can live outside `LayoutState`, so a
/// re-entrant borrow refusal still has an explicit terminal owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayoutQueryClaim {
    context: ContextId,
    lease: UiLease,
}

fn abandon_matching_geometry(
    phase: &mut GeometryPhase,
    installed: Option<LayoutQueryClaim>,
    claimed: LayoutQueryClaim,
) {
    if installed == Some(claimed) && *phase == GeometryPhase::QueryQueued {
        *phase = GeometryPhase::Unavailable;
    }
}

#[derive(Debug, Default)]
struct LayoutState {
    subscription: Option<LayoutSubscription>,
    phase: GeometryPhase,
    /// Preserved across `TS_E_NOLAYOUT`; the popup stays at its last valid
    /// anchor until the subscribed layout callback owns one retry.
    last_anchor: Option<ScreenRect>,
    /// The editable area the anchor sits inside, kept beside it so a popup
    /// held at its last valid anchor keeps the box it must not cover.
    last_document: Option<ScreenRect>,
}

impl LayoutState {
    /// A different output owns the same candidate window now. Any queued
    /// geometry query belonged to the old lease and is therefore complete from
    /// this state's perspective; the new owner may request a fresh query.
    fn retire_geometry_for_lease_rollover(&mut self) {
        self.phase = GeometryPhase::Idle;
        self.last_anchor = None;
        self.last_document = None;
    }
}

/// Merges a newly advised layout subscription without ever letting a stale
/// proposal evict the subscription installed by a re-entrant newer UI owner.
/// The caller decides currentness before taking the `LayoutState` borrow, so no
/// write-journal or layout borrow crosses the COM calls that created `proposed`.
fn merge_layout_subscription<T>(
    installed: Option<T>,
    proposed: T,
    proposed_is_current: bool,
) -> (Option<T>, Option<T>) {
    if proposed_is_current {
        (Some(proposed), installed)
    } else {
        (installed, Some(proposed))
    }
}

/// Resolves a lease update for a subscription that already belongs to the
/// same context. A stale proposal must preserve both the installed lease and
/// its geometry phase; only the current UI owner may roll that geometry over.
fn resolve_same_context_layout_lease<T>(
    installed_lease: T,
    proposed_lease: T,
    proposed_is_current: bool,
) -> (T, bool) {
    if proposed_is_current {
        (proposed_lease, true)
    } else {
        (installed_lease, false)
    }
}

/// The exact re-entrant host-call sequence used to retire candidate UI. The
/// caller must hold the candidate-operation exclusion until this sequence and
/// controller restoration are complete.
fn run_candidate_teardown_host_calls<Subscription, Controller>(
    subscription: Option<Subscription>,
    controller: &mut Controller,
    unadvise: impl FnOnce(Subscription),
    end: impl FnOnce(&mut Controller),
) {
    if let Some(subscription) = subscription {
        unadvise(subscription);
    }
    end(controller);
}

/// One instance exists per thread that activates the IME.
#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfCompositionSink,
    ITfDisplayAttributeProvider,
    ITfTextLayoutSink,
    ITfFunctionProvider,
    ITfFnReconversion,
    ITfLangBarItem,
    ITfLangBarItemButton,
    ITfSource
)]
#[derive(Debug)]
pub struct TextService {
    activation: RefCell<Option<Activation>>,
    composition: RefCell<CompositionState>,
    category_mgr: RefCell<Option<ITfCategoryMgr>>,
    candidate_ui: RefCell<CandidateUi>,
    layout: RefCell<LayoutState>,
    /// A callback that cannot borrow `LayoutState` transfers its exact query
    /// identity here before scheduling one bounded hidden-window attempt. If
    /// posting or that attempt fails, candidate teardown/detach remains the
    /// final owner; no retry loop is allowed on the host thread.
    layout_abandon_pending: Cell<Option<LayoutQueryClaim>>,
    deferred: RefCell<DeferredState>,
    /// Candidate creation, update, and teardown all move the controller out of
    /// its slot across re-entrant TSF calls. This out-of-band bit lets nested
    /// hidden-window dispatch retain newer work without borrowing the state
    /// that an outer operation may already own.
    candidate_operation_active: Cell<bool>,
    /// If an end request cannot enter `DeferredState`, detach/drop remains its
    /// explicit lifecycle owner. A later dispatcher promotes this bit into the
    /// normal coalesced work before it can run candidate creation.
    candidate_end_pending: Cell<bool>,
    /// A focus-gain callback can arrive while `DeferredState` is itself
    /// re-entrantly borrowed. This out-of-band, thread-local signal prevents a
    /// stale focus-loss message from becoming authoritative until the next
    /// dispatcher or lifecycle owner can promote it into `DeferredState`.
    focus_gain_reconciliation_pending: Cell<bool>,
    /// A terminal helper may discover that the write journal is temporarily
    /// borrowed by an outer re-entrant callback.  The bit is an explicit
    /// bounded owner for the exact-undo settlement: keys stay consumed until a
    /// later callback can drain the journal, and an Unknown outcome never
    /// becomes an accidental retryable state.
    undo_terminalization: Cell<Option<UndoCommitOutcome>>,
    /// Tokenized one-slot fence for a finalizer queued after an engine key
    /// timeout. It lives outside the write RefCell so re-entrant key probes can
    /// fail closed without borrowing the journal currently in a callback.
    engine_recovery: Cell<EngineRecoveryFence>,
    writes: RefCell<WriteCoordinator<PendingWrite>>,
    engine: RefCell<Engine>,
    /// The TSF input-mode item is deliberately visible only while a document
    /// caret belongs to this service. `Cell` keeps focus loss authoritative
    /// even if the shell re-enters a language-bar callback.
    focus_foreground: Cell<bool>,
    ai_text: RefCell<AiTextState>,
    ai_key_latched: Cell<bool>,
    last_ai_error: RefCell<Option<String>>,
    mode_item: mode_item::ModeItemState,
    /// Last scope read under a live edit cookie. A dedicated synchronous
    /// read session is often refused during OnKeyDown (E_FAIL); the write
    /// callback still has a cookie and refreshes this cache.
    cached_input_scope: Cell<Option<sakura_proto::InputScope>>,
}

impl TextService {
    pub fn new() -> Self {
        on_object_created();
        Self {
            activation: RefCell::new(None),
            composition: RefCell::new(CompositionState::default()),
            category_mgr: RefCell::new(None),
            candidate_ui: RefCell::new(CandidateUi::default()),
            layout: RefCell::new(LayoutState::default()),
            layout_abandon_pending: Cell::new(None),
            deferred: RefCell::new(DeferredState::default()),
            candidate_operation_active: Cell::new(false),
            candidate_end_pending: Cell::new(false),
            focus_gain_reconciliation_pending: Cell::new(false),
            undo_terminalization: Cell::new(None),
            engine_recovery: Cell::new(EngineRecoveryFence::default()),
            writes: RefCell::new(WriteCoordinator::new(DEFAULT_WRITE_CAPACITY)),
            engine: RefCell::new(Engine::new()),
            focus_foreground: Cell::new(false),
            ai_text: RefCell::new(AiTextState::default()),
            ai_key_latched: Cell::new(false),
            last_ai_error: RefCell::new(None),
            mode_item: mode_item::ModeItemState::default(),
            cached_input_scope: Cell::new(None),
        }
    }

    fn create_deferred_window(&self, owner: *const TextService_Impl) -> Result<()> {
        if self
            .deferred
            .try_borrow()
            .map_err(|_| reentrancy())?
            .window
            .is_some()
        {
            return Ok(());
        }

        // The hidden window owns exactly one deferred-work message per thread.
        // SAFETY: the callback and class name are static, and registration does
        // not borrow any Rust state.
        unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(deferred_window_procedure),
                lpszClassName: DEFERRED_WINDOW_CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
        }
        // SAFETY: the class is registered above, all optional handles are null,
        // and the zero-sized window is never shown.
        let window_result = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                DEFERRED_WINDOW_CLASS,
                DEFERRED_WINDOW_CLASS,
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
        };
        let window = window_result?;
        // SAFETY: `window` was returned by `CreateWindowExW` and the value is a
        // pointer-sized owner token read only by this window's procedure.
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, owner as isize);
        }
        self.deferred
            .try_borrow_mut()
            .map_err(|_| reentrancy())?
            .window = Some(window);
        Ok(())
    }

    fn destroy_deferred_window(&self) -> Result<()> {
        let window = {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            // A focus-gain callback can arrive while this state is owned by a
            // re-entrant caller.  If it could not promote its fail-closed
            // request before the window is destroyed, transfer that request
            // directly to the lifecycle terminal owner.  In particular, do
            // not leave the old pre-engine focus-loss bit runnable after the
            // only message that could have carried it has been invalidated.
            if self.focus_gain_reconciliation_pending.replace(false) {
                state.focus_finalization = FocusFinalizationPhase::ReconciliationAwaitingLifecycle;
                state.focus_reconciliation_required = true;
                state.dispatch.work.focus_loss = false;
                state.dispatch.work.focus_reconcile = false;
            }
            // A hidden-window message is no longer a valid owner after its
            // window is destroyed. Preserve an engine-started/reconciliation
            // finalization as an explicit lifecycle obligation so `detach`
            // can make the document-free attempt and surface any failure.
            state.handoff_focus_finalization_to_lifecycle();
            if state.dispatch.work.end_candidates {
                self.candidate_end_pending.set(true);
            }
            state.dispatch = DeferredDispatchState::default();
            if state.focus_finalization != FocusFinalizationPhase::ReconciliationAwaitingLifecycle {
                state.focus_finalization = FocusFinalizationPhase::Idle;
                state.focus_reconciliation_required = false;
            }
            state.window.take()
        };
        let Some(window) = window else {
            return Ok(());
        };
        // SAFETY: the window was created on this thread and its user data is
        // cleared before destruction so no queued callback can use `self`.
        unsafe {
            let _ = KillTimer(Some(window), AI_TEXT_TIMER_ID);
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            let _ = DestroyWindow(window);
        }
        Ok(())
    }

    fn arm_ai_text_timer(&self) -> Result<()> {
        let window = self
            .deferred
            .try_borrow()
            .map_err(|_| reentrancy())?
            .window
            .ok_or_else(|| Error::new(E_UNEXPECTED, "AI poll window is unavailable"))?;
        // SAFETY: this thread owns the hidden window. A null timer callback
        // routes WM_TIMER through its existing window procedure.
        let timer = unsafe { SetTimer(Some(window), AI_TEXT_TIMER_ID, AI_TEXT_POLL_MS, None) };
        if timer == 0 {
            return Err(Error::from_thread());
        }
        Ok(())
    }

    fn stop_ai_text_timer(&self) {
        let window = self
            .deferred
            .try_borrow()
            .ok()
            .and_then(|state| state.window);
        if let Some(window) = window {
            // SAFETY: the timer belongs to this hidden window and thread.
            unsafe {
                let _ = KillTimer(Some(window), AI_TEXT_TIMER_ID);
            }
        }
    }

    fn post_deferred_work(&self) -> Result<()> {
        let window = {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            // A live post already owns this coalesced work. `destroy_deferred_window`
            // clears `posted` before it drops the HWND, so this check is also
            // the proof that the pre-existing message still has an owner.
            if state.dispatch.posted {
                return Ok(());
            }
            let Some(window) = state.window else {
                // A focus-reconciliation retry cannot be silently discarded
                // with a missing window. Its next document-free attempt is
                // now explicitly owned by detach/drop.
                state.handoff_focus_reconciliation_to_lifecycle();
                if state.dispatch.work.end_candidates {
                    self.candidate_end_pending.set(true);
                }
                state.dispatch.work.clear();
                return Err(Error::new(
                    E_UNEXPECTED,
                    "deferred work window is unavailable",
                ));
            };
            state.dispatch.posted = true;
            window
        };

        // SAFETY: `window` is the live hidden window retained in `DeferredState`.
        let post_result = {
            // SAFETY: `window` is the live hidden window retained in
            // `DeferredState`.
            unsafe { PostMessageW(Some(window), DEFERRED_WORK_MESSAGE, WPARAM(0), LPARAM(0)) }
        };
        if let Err(error) = post_result {
            if let Ok(mut state) = self.deferred.try_borrow_mut() {
                state.dispatch.posted = false;
                // `PostMessageW` did not transfer ownership. Preserve a
                // queued focus reconciliation as a lifecycle obligation before
                // clearing the generic deferred-work payload.
                state.handoff_focus_reconciliation_to_lifecycle();
                if state.dispatch.work.end_candidates {
                    self.candidate_end_pending.set(true);
                }
                state.dispatch.work.clear();
            }
            return Err(error);
        }
        Ok(())
    }

    fn queue_candidates(
        &self,
        context: &ITfContext,
        candidates: &EngineCandidateList,
        lease: UiLease,
    ) -> Result<()> {
        {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            state.dispatch.work.candidates = Some(PendingCandidates {
                context: context.clone(),
                candidates: candidates.clone(),
                lease,
            });
        }
        self.post_deferred_work()
    }

    /// Completion never asks TSF for the following operation inline.  The
    /// hidden window serializes that request after the current callback has
    /// returned to the host.
    fn queue_write(&self) -> Result<()> {
        {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            state.dispatch.work.write = true;
        }
        self.post_deferred_work()
    }

    fn queue_end_candidates(&self) -> Result<()> {
        {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            state.dispatch.work.retain_candidate_end();
        }
        self.post_deferred_work()
    }

    fn queue_focus_loss(&self) -> Result<()> {
        {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            match state.focus_finalization {
                FocusFinalizationPhase::Idle => {
                    state.focus_finalization = FocusFinalizationPhase::DeferredQueued;
                }
                FocusFinalizationPhase::DeferredQueued => {}
                // An engine-side finalization already owns this focus epoch.
                // Do not queue a second commit while its document callback is
                // still pending.
                FocusFinalizationPhase::EngineCommitStarted
                | FocusFinalizationPhase::ReconciliationQueued
                | FocusFinalizationPhase::ReconciliationAwaitingLifecycle => return Ok(()),
            }
            state.dispatch.work.retain_candidate_end();
            state.dispatch.work.focus_loss = true;
        }
        match self.post_deferred_work() {
            Ok(()) => Ok(()),
            Err(error) => {
                // No engine commit has started in `DeferredQueued`, so an
                // unavailable hidden window may retire an ordinary request.
                // A cancelled accepted output is different: its engine state
                // can already be ahead, so it must go through the
                // document-free reconciliation terminal path.
                self.cancel_queued_focus_finalization();
                if self.focus_reconciliation_required()
                    && self
                        .abandon_composition_projection(CancelReason::RevisionMismatch)
                        .is_err()
                {
                    // The initial document-free attempt was refused. The
                    // engine/UI terminal is immediate; one deferred retry owns
                    // only retirement of the canonical projection.
                    self.defer_refused_focus_reconciliation();
                }
                Err(error)
            }
        }
    }

    fn queue_layout(&self) -> Result<()> {
        {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            state.dispatch.work.layout = true;
        }
        self.post_deferred_work()
    }

    fn queue_layout_abandon(&self) -> Result<()> {
        {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            state.dispatch.work.layout_abandon = true;
        }
        self.post_deferred_work()
    }

    fn begin_candidate_operation(&self) -> bool {
        !self.candidate_operation_active.replace(true)
    }

    /// Restores deferred dispatch only after the caller has put its local
    /// candidate controller back in the slot. Work pumped by a host call stays
    /// coalesced until this point, so an older operation can never overwrite a
    /// newer controller that ran inside it.
    fn finish_candidate_operation(&self) -> Result<()> {
        self.candidate_operation_active.set(false);
        let repost = {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            if self.candidate_end_pending.replace(false) {
                state.dispatch.work.retain_candidate_end();
            }
            state.dispatch.needs_repost_after_candidate_operation()
        };
        if repost {
            let posted = self.post_deferred_work();
            if posted.is_err() && self.candidate_end_pending.get() {
                // The hidden window is no longer an owner (normally during
                // deactivation re-entered from Begin/End). The controller is
                // back in its slot and the operation bit is clear, so lifecycle
                // teardown can now consume the retained end synchronously.
                self.end_candidates();
            }
            posted
        } else {
            Ok(())
        }
    }

    fn input_blocked(&self) -> bool {
        let focus_gain_reconciliation_pending = self.focus_gain_reconciliation_pending.get();
        let undo_terminalization_pending = self.undo_terminalization.get().is_some();
        let deferred = self
            .deferred
            .try_borrow()
            .map(|state| state.dispatch.work.focus_loss || state.focus_finalization.blocks_input())
            .unwrap_or(true);
        let composition_unknown = self
            .composition
            .try_borrow()
            .map(|state| !state.known)
            .unwrap_or(true);
        undo_terminalization_pending
            || focus_gain_reconciliation_pending
            || deferred
            || composition_unknown
    }

    fn begin_engine_recovery(&self) -> RecoveryStart {
        let mut fence = self.engine_recovery.get();
        let started = fence.begin();
        self.engine_recovery.set(fence);
        started
    }

    fn finish_engine_recovery(&self, token: RecoveryToken, outcome: RecoveryTerminal) {
        let mut fence = self.engine_recovery.get();
        let _ = fence.finish(token, outcome);
        self.engine_recovery.set(fence);
    }

    fn cancel_engine_recovery(&self) {
        let mut fence = self.engine_recovery.get();
        let _ = fence.cancel_pending();
        self.engine_recovery.set(fence);
    }

    fn engine_recovery_pending(&self) -> bool {
        self.engine_recovery.get().is_pending()
    }

    fn engine_recovery_disposition(&self, token: RecoveryToken) -> RecoveryKeyDisposition {
        self.engine_recovery.get().disposition_after_request(token)
    }

    fn probe_fence(&self, context: ContextId) -> Result<ProbeFence> {
        let input_blocked = self.input_blocked();
        let writes = self.writes.try_borrow().map_err(|_| reentrancy())?;
        Ok(decide_probe_fence(
            self.undo_terminalization.get(),
            &writes,
            context,
            |payload| payload.undo_commit,
            self.engine_recovery_pending(),
            input_blocked,
        ))
    }

    fn write_context_is_replacement(&self, context: ContextId) -> Result<bool> {
        self.writes
            .try_borrow()
            .map_err(|_| reentrancy())
            .map(|writes| writes.is_context_replacement(context))
    }

    /// Fail closed: if composition or the journal cannot be inspected, treat
    /// a reading as live so Space cannot replace it with a document space.
    fn has_live_composition(&self) -> bool {
        let composition_live = self
            .composition
            .try_borrow()
            .map(|state| state.handle.is_some() || !state.text.is_empty())
            .unwrap_or(true);
        if composition_live {
            return true;
        }
        self.writes
            .try_borrow()
            .map(|writes| {
                let visible = writes.tail_visible();
                visible.has_composition || !visible.text.is_empty()
            })
            .unwrap_or(true)
    }

    fn defer_undo_terminalization(&self, requested: Option<UndoCommitOutcome>) {
        let requested = requested.unwrap_or(UndoCommitOutcome::Unknown);
        let merged = match (self.undo_terminalization.get(), requested) {
            (Some(UndoCommitOutcome::Unknown), _) | (_, UndoCommitOutcome::Unknown) => {
                UndoCommitOutcome::Unknown
            }
            (Some(UndoCommitOutcome::Applied), _) | (_, UndoCommitOutcome::Applied) => {
                UndoCommitOutcome::Applied
            }
            _ => UndoCommitOutcome::Rejected,
        };
        self.undo_terminalization.set(Some(merged));
        if merged == UndoCommitOutcome::Unknown {
            // The journal could not be inspected, so neither the document nor
            // the engine-side transaction can be paired with confidence. Cut
            // the engine link now; the marker still fences host keys until the
            // journal owner can drain the local payload.
            self.disconnect();
        }
    }

    /// Attempts the one bounded retry owned by a prior journal-borrow failure.
    /// `false` means the outer re-entrant owner still holds the journal; the
    /// caller must consume the key and leave the marker in place.
    fn try_settle_deferred_undo_terminalization(&self) -> bool {
        let Some(_outcome) = self.undo_terminalization.get() else {
            return true;
        };
        let terminal = match self.writes.try_borrow_mut() {
            Ok(mut writes) => {
                let Some(ticket) = writes.head_ticket() else {
                    // The marker is only a bounded handoff for a journal
                    // borrow failure.  If the original owner already drained
                    // the journal, there is no payload left to settle and the
                    // disconnected engine link is already the terminal
                    // boundary.
                    self.undo_terminalization.set(None);
                    return true;
                };
                terminalize_unknown_undo_after_document_access(
                    &mut writes,
                    ticket,
                    |payload| payload.undo_commit,
                    |outcome| self.settle_undo_commit(outcome),
                )
            }
            Err(_) => return false,
        };
        self.undo_terminalization.set(None);
        self.settle_cancelled_writes_after_undo_terminalization(terminal.completions, true);
        if terminal.disconnect_required {
            self.disconnect();
        }
        true
    }

    /// A real key that enters while deferred undo terminalization is pending
    /// belongs to that terminal owner, even when the one bounded settlement
    /// attempt succeeds and clears the marker.  Returning `true` prevents the
    /// same physical key from being applied after the marker was consumed;
    /// Probe uses the matching read-only Busy result without making progress.
    fn deferred_undo_consumes_real_key(&self) -> bool {
        let was_pending = self.undo_terminalization.get().is_some();
        if !self.try_settle_deferred_undo_terminalization() {
            // The re-entrant journal owner still owns the terminal outcome, so
            // fail closed and keep this key consumed until that owner finishes.
            return true;
        }
        was_pending
    }

    /// A focus-gain callback must never return with a pre-engine focus-loss
    /// request still runnable when it could not prove the current composition
    /// projection.  The common case immediately moves that request into the
    /// bounded reconciliation state machine.  If `DeferredState` itself is
    /// re-entrantly borrowed, the cell remains set as an explicit fail-closed
    /// handoff for the next dispatcher or lifecycle teardown; it is not a
    /// second retry queue.
    fn request_focus_gain_reconciliation(&self) {
        self.focus_gain_reconciliation_pending.set(true);
        let _ = self.promote_pending_focus_gain_reconciliation();
    }

    /// Promotes the out-of-band focus-gain handoff into the one existing
    /// deferred/lifecycle reconciliation owner.  This deliberately performs
    /// no composition retirement itself: a re-entrant `CompositionState`
    /// borrow is the reason for this path, so the first document-free attempt
    /// belongs to the later bounded retry instead of this callback stack.
    fn promote_pending_focus_gain_reconciliation(&self) -> Result<()> {
        if !self.focus_gain_reconciliation_pending.get() {
            return Ok(());
        }

        let terminalize = {
            let mut state = self.deferred.try_borrow_mut().map_err(|_| reentrancy())?;
            // Even if a previous lifecycle callback moved the phase already,
            // its pre-engine bit can never be allowed to dispatch after focus
            // has returned without a safe projection proof.
            state.dispatch.work.focus_loss = false;
            state.focus_reconciliation_required = true;
            match state.focus_finalization {
                FocusFinalizationPhase::Idle
                | FocusFinalizationPhase::DeferredQueued
                | FocusFinalizationPhase::EngineCommitStarted => {
                    state.focus_finalization = FocusFinalizationPhase::ReconciliationQueued;
                    state.dispatch.work.focus_reconcile = true;
                    true
                }
                // These phases already have the only permitted retry/lifecycle
                // owner.  Do not repost or terminalize a second time.
                FocusFinalizationPhase::ReconciliationQueued
                | FocusFinalizationPhase::ReconciliationAwaitingLifecycle => false,
            }
        };

        // The phase and stale-work bit now make the handoff authoritative, so
        // a nested callback cannot mistake the signal for an unowned engine
        // commit while candidate/engine teardown runs below.
        self.focus_gain_reconciliation_pending.set(false);

        if terminalize {
            // This only owns engine/UI teardown.  The canonical composition is
            // intentionally left untouched until the deferred/lifecycle owner
            // runs after the re-entrant composition borrow has unwound.
            self.terminalize_cancelled_state(true);
            if self.post_deferred_work().is_err() {
                self.handoff_focus_reconciliation_to_lifecycle();
            }
        }
        Ok(())
    }

    /// Transfers a queued focus-loss request to the engine/document recovery
    /// owner. This transition happens before `Engine::commit`, because that
    /// call can itself re-enter through a focus notification.
    fn begin_focus_finalization(&self) -> bool {
        let Ok(mut state) = self.deferred.try_borrow_mut() else {
            return false;
        };
        if state.focus_finalization != FocusFinalizationPhase::DeferredQueued {
            return false;
        }
        state.focus_finalization = FocusFinalizationPhase::EngineCommitStarted;
        true
    }

    fn focus_finalization_phase(&self) -> FocusFinalizationPhase {
        self.deferred
            .try_borrow()
            // A re-entrant borrow cannot safely admit input over a possibly
            // committed engine session, so use the fail-closed phase.
            .map(|state| state.focus_finalization)
            .unwrap_or(FocusFinalizationPhase::EngineCommitStarted)
    }

    /// Finishes either focus-finalization branch. The document has either been
    /// committed/retired, or the queued request was cancelled before it reached
    /// the engine; no later deferred focus callback may start a new commit.
    fn finish_focus_finalization(&self) {
        if let Ok(mut state) = self.deferred.try_borrow_mut() {
            state.focus_finalization = FocusFinalizationPhase::Idle;
            state.dispatch.work.focus_loss = false;
            state.focus_reconciliation_required = false;
        }
    }

    /// Cancels only the pre-engine branch. If re-entrancy advanced the phase,
    /// its engine owner remains responsible for document-free reconciliation.
    fn cancel_queued_focus_finalization(&self) {
        if let Ok(mut state) = self.deferred.try_borrow_mut() {
            if state.focus_finalization == FocusFinalizationPhase::DeferredQueued {
                state.focus_finalization = FocusFinalizationPhase::Idle;
                state.dispatch.work.focus_loss = false;
            }
        }
    }

    /// Makes a pending focus boundary fail closed. The journal has already
    /// accepted an engine output, but TSF may never have run its callback, so
    /// a locally known composition is not evidence that the engine and
    /// document still agree.
    fn require_focus_reconciliation(&self) {
        if let Ok(mut state) = self.deferred.try_borrow_mut() {
            state.focus_reconciliation_required = true;
        } else {
            // If the phase owner itself is re-entrant, make the independent
            // composition proof unknown so focus recovery selects the same
            // document-free branch.
            self.invalidate_inflight_composition_write_as_unknown();
        }
    }

    fn focus_reconciliation_required(&self) -> bool {
        self.deferred
            .try_borrow()
            .map(|state| state.focus_reconciliation_required)
            // A re-entrant read cannot prove that a cancelled engine output
            // was reconciled, so retain the projection only on a positive
            // proof of safety.
            .unwrap_or(true)
    }

    /// Schedules exactly one document-free retirement retry after the engine
    /// may have committed but a re-entrant borrow prevented us from dropping
    /// the canonical composition projection. The hidden-window message is the
    /// sole retry owner; a delivery failure is converted by
    /// `post_deferred_work` into the explicit lifecycle-owned phase.
    fn queue_focus_reconciliation(&self) {
        let should_post = match self.deferred.try_borrow_mut() {
            Ok(mut state) => state.queue_focus_reconciliation(),
            Err(_) => {
                // The service remains fail-closed while a re-entrant deferred
                // owner exists. It cannot safely admit input; the lifecycle
                // path still performs a document-free retirement on detach.
                self.invalidate_inflight_composition_write_as_unknown();
                return;
            }
        };
        if should_post && self.post_deferred_work().is_err() {
            // The normal error branches hand the bit to lifecycle while they
            // still hold `DeferredState`. This covers only a re-entrant borrow
            // that prevented that handoff itself; no immediate retry loop is
            // permitted on this stack.
            self.handoff_focus_reconciliation_to_lifecycle();
        }
    }

    fn handoff_focus_reconciliation_to_lifecycle(&self) {
        if let Ok(mut state) = self.deferred.try_borrow_mut() {
            state.handoff_focus_reconciliation_to_lifecycle();
        } else {
            self.invalidate_inflight_composition_write_as_unknown();
        }
    }

    /// Consumes the one deferred reconciliation owner. A second temporary
    /// borrow refusal is a terminal transition to lifecycle cleanup, not an
    /// immediate repost/retry loop on the host's message stack.
    fn dispatch_focus_reconciliation(&self) {
        let owns_retry = self
            .deferred
            .try_borrow()
            .map(|state| state.focus_finalization == FocusFinalizationPhase::ReconciliationQueued)
            .unwrap_or(false);
        if !owns_retry {
            return;
        }

        if self
            .abandon_composition_projection(CancelReason::RevisionMismatch)
            .is_ok()
        {
            // The first failed attempt already terminalized engine/UI work,
            // but repeating that idempotent terminalization makes this retry's
            // complete journal/engine/UI outcome explicit even if its original
            // caller unwound.
            self.terminalize_cancelled_state(true);
        } else {
            self.handoff_focus_reconciliation_to_lifecycle();
        }
    }

    /// A document-free retirement has already been attempted once by the
    /// caller and was refused by a re-entrant borrow. Terminalize the
    /// engine/UI side immediately, then transfer the composition retirement to
    /// the bounded deferred owner. Callers must not invoke this before their
    /// first attempt, or it would become an eager retry loop.
    fn defer_refused_focus_reconciliation(&self) {
        self.terminalize_cancelled_state(true);
        self.queue_focus_reconciliation();
    }

    /// Revokes a callback's right to publish its locally retained composition
    /// handle without discarding the host-owned composition itself.  Focus
    /// recovery still needs that handle; deactivation and termination call
    /// `forget_composition` afterwards to retire it completely.
    fn invalidate_inflight_composition_write(&self) {
        if let Ok(mut state) = self.composition.try_borrow_mut() {
            state.write_owner.invalidate();
            state.expected_self_termination = None;
        }
    }

    /// Engine recovery can begin while a previous write callback is inside a
    /// host call. Its document effect is no longer knowable from the canonical
    /// state, so revoke the flight and force the recovery path to abandon the
    /// projection instead of committing an older visible snapshot.
    fn invalidate_inflight_composition_write_as_unknown(&self) {
        if let Ok(mut state) = self.composition.try_borrow_mut() {
            let had_inflight = state.write_owner.in_flight.is_some();
            state.write_owner.invalidate();
            state.expected_self_termination = None;
            if had_inflight {
                state.known = false;
            }
        }
    }

    /// Emits only the fixed-width metadata allowed by the diagnostic ring. The
    /// fast disabled path does not borrow any TSF state.
    fn record_diagnostic_write(
        &self,
        ticket: Ticket,
        path: diagnostic_ring::RequestPath,
        outcome: diagnostic_ring::TerminalOutcome,
        error_code: i32,
    ) {
        if !diagnostic_ring::is_enabled() {
            return;
        }
        let composition_generation = self
            .composition
            .try_borrow()
            .map(|state| state.write_owner.lifecycle)
            .unwrap_or(0);
        diagnostic_ring::record(diagnostic_ring::Metadata::request(
            ticket.context().0 as u64,
            ticket.focus_generation(),
            ticket.document_revision(),
            composition_generation,
            ticket.id(),
            diagnostic_ring::RequestKind::KeyWrite,
            path,
            outcome,
            error_code,
        ));
    }

    fn record_diagnostic_lifecycle(
        &self,
        event: diagnostic_ring::LifecycleEvent,
        context_identity: u64,
    ) {
        if diagnostic_ring::is_enabled() {
            diagnostic_ring::record(diagnostic_ring::Metadata::lifecycle(
                event,
                context_identity,
            ));
        }
    }

    fn activate_write_journal(&self) -> Result<()> {
        if !self.try_settle_deferred_undo_terminalization() {
            return Err(reentrancy());
        }
        let cancelled = self
            .writes
            .try_borrow_mut()
            .map_err(|_| {
                self.defer_undo_terminalization(None);
                reentrancy()
            })?
            .activate();
        self.settle_cancelled_writes(cancelled, false, None);
        Ok(())
    }

    fn deactivate_write_journal(&self) {
        if !self.try_settle_deferred_undo_terminalization() {
            self.defer_undo_terminalization(Some(UndoCommitOutcome::Unknown));
            return;
        }
        self.invalidate_inflight_composition_write();
        let cancelled = match self.writes.try_borrow_mut() {
            Ok(mut writes) => writes.deactivate(),
            Err(_) => {
                self.defer_undo_terminalization(None);
                return;
            }
        };
        self.settle_cancelled_writes(cancelled, false, None);
    }

    fn invalidate_for_focus_change(&self) {
        if !self.try_settle_deferred_undo_terminalization() {
            self.defer_undo_terminalization(Some(UndoCommitOutcome::Unknown));
            return;
        }
        // A focus notification can re-enter from a document mutation before
        // its callback publishes the new projection. In that case the host's
        // visible text is unknowable, so later focus-loss/focus-gain handling
        // must abandon it without issuing another document edit.
        self.invalidate_inflight_composition_write_as_unknown();
        let cancelled = match self.writes.try_borrow_mut() {
            Ok(mut writes) => writes.focus_changed(),
            Err(_) => {
                self.defer_undo_terminalization(None);
                self.require_focus_reconciliation();
                return;
            }
        };
        if cancelled_outputs_require_focus_reconciliation(&cancelled) {
            self.require_focus_reconciliation();
        }
        self.settle_cancelled_writes(cancelled, false, None);
        self.cached_input_scope.set(None);
    }

    fn resume_after_focus_gain(&self) {
        self.invalidate_for_focus_change();
        // A previous re-entrant gain callback may have been unable to borrow
        // `DeferredState` at all. Give that explicit handoff one later
        // lifecycle opportunity before inspecting ordinary focus state; do not
        // let a now-readable `DeferredQueued` state erase the signal and leave
        // its old message runnable.
        if self.focus_gain_reconciliation_pending.get() {
            let _ = self.promote_pending_focus_gain_reconciliation();
            return;
        }
        // Do not use the generic fail-closed accessors here.  Their fallback
        // values are appropriate for admission checks, but on this lifecycle
        // transition a borrow error needs an explicit reconciliation owner;
        // otherwise an old `DeferredQueued` focus-loss message could run after
        // focus gain merely because the phase could not be observed.
        let (phase, reconciliation_required) = match self.deferred.try_borrow() {
            Ok(state) => (
                state.focus_finalization,
                state.focus_reconciliation_required,
            ),
            Err(_) => {
                self.request_focus_gain_reconciliation();
                return;
            }
        };
        let unknown = match self.composition_projection_is_unknown() {
            Ok(unknown) => unknown,
            Err(_) => {
                self.request_focus_gain_reconciliation();
                return;
            }
        };
        // Before deferred dispatch, the engine still owns the same session as
        // the known visible composition, so focus regain simply cancels that
        // request. Once the engine commit started, however, it may be empty
        // while the document callback still has not run; retire the projection
        // without another document edit even when the journal is already empty.
        if phase == FocusFinalizationPhase::EngineCommitStarted
            || reconciliation_required
            || unknown
        {
            if self
                .abandon_composition_projection(CancelReason::RevisionMismatch)
                .is_err()
            {
                // Focus regain is a later lifecycle opportunity, not a reason
                // to spin synchronously while a re-entrant callback still
                // owns `CompositionState`. The helper schedules at most one
                // deferred retirement and otherwise records detach ownership.
                self.defer_refused_focus_reconciliation();
            }
        } else {
            self.finish_focus_finalization();
        }
    }

    fn composition_projection_is_unknown(&self) -> Result<bool> {
        self.composition
            .try_borrow()
            .map_err(|_| reentrancy())
            .map(|state| !state.known)
    }

    /// An engine-side focus finalization has begun but cannot safely continue
    /// to a document callback. Revoke its queued work first, then abandon the
    /// projection through the path that owns engine/UI teardown for both empty
    /// and non-empty journals.
    fn abort_engine_started_focus_finalization(&self) -> Result<()> {
        self.invalidate_for_focus_change();
        let result = self.abandon_composition_projection(CancelReason::RevisionMismatch);
        if result.is_err() {
            // `EngineCommitStarted` means the engine can no longer be paired
            // with a retained document projection. A refused first retirement
            // gets one deferred owner; it is never left as an unowned blocked
            // state on this stack.
            self.defer_refused_focus_reconciliation();
        }
        result
    }

    fn invalidate_for_composition_termination(&self) {
        if !self.try_settle_deferred_undo_terminalization() {
            self.defer_undo_terminalization(Some(UndoCommitOutcome::Unknown));
            return;
        }
        self.invalidate_inflight_composition_write();
        let cancelled = match self.writes.try_borrow_mut() {
            Ok(mut writes) => writes.composition_terminated(),
            Err(_) => {
                self.defer_undo_terminalization(None);
                return;
            }
        };
        self.settle_cancelled_writes(cancelled, false, None);
    }

    /// Establishes the one context the journal is allowed to mutate.  A new
    /// context is a hard boundary: old callbacks are cancelled before the
    /// engine sees a key for the replacement document.
    fn observe_write_context(&self, context: &ITfContext) -> Result<()> {
        let incoming_context = context_id(context);
        let composition_context_changed = self
            .composition
            .try_borrow()
            .map_err(|_| reentrancy())?
            .context
            .as_ref()
            .is_some_and(|current| current.as_raw() != context.as_raw());
        let (context_replaced, cancelled) = {
            let mut writes = self.writes.try_borrow_mut().map_err(|_| reentrancy())?;
            let replaced = writes.is_context_replacement(incoming_context);
            let cancelled = writes.observe_context(incoming_context);
            (replaced, cancelled)
        };
        if cancelled.is_empty() && !composition_context_changed && !context_replaced {
            return Ok(());
        }
        if composition_context_changed || context_replaced {
            self.record_diagnostic_lifecycle(
                diagnostic_ring::LifecycleEvent::ContextReplaced,
                incoming_context.0 as u64,
            );
        }
        // Context replacement is a lifecycle boundary. Revoke any callback's
        // local handle before candidate teardown or engine reset can re-enter.
        self.invalidate_inflight_composition_write();
        let had_cancelled_work = !cancelled.is_empty();
        self.settle_cancelled_writes(cancelled, true, None);
        // Do not call EndComposition on a handle for another context.  Dropping
        // it makes that host document the owner of its remaining visual state.
        self.forget_composition()?;
        if (context_replaced || composition_context_changed) && !had_cancelled_work {
            // `settle_cancelled_writes` has no terminal completion to trigger
            // its reset/UI teardown in this case, but an engine session and a
            // candidate window cannot safely span two document contexts. This
            // also covers an empty journal that still retained the old
            // context: the next key must create the same fresh session as a
            // new engine connection.
            if self.queue_end_candidates().is_err() {
                self.end_candidates();
            }
            self.disconnect();
        }
        Ok(())
    }

    fn can_admit_write_for_context(&self, context: &ITfContext) -> Result<bool> {
        self.writes
            .try_borrow()
            .map_err(|_| reentrancy())
            .map(|writes| {
                writes.can_admit_for_context_unless(context_id(context), |payload| {
                    payload.undo_commit
                })
            })
    }

    /// A queued exact-text undo remains authoritative even when the bounded
    /// journal has spare capacity. Later keys must be consumed locally until
    /// that operation reports Applied, Rejected, or Unknown; otherwise the
    /// host could edit around the caret while its deletion callback is stale.
    fn undo_write_pending(&self) -> Result<bool> {
        self.writes
            .try_borrow()
            .map_err(|_| reentrancy())
            .map(|writes| writes.any_payload(|payload| payload.undo_commit))
    }

    fn ui_lease_is_current(&self, context: &ITfContext, lease: UiLease) -> bool {
        context_id(context) == lease.context()
            && self
                .writes
                .try_borrow()
                .map(|writes| writes.validate_ui_lease(lease))
                .unwrap_or(false)
    }

    fn reserve_write(&self, context: &ITfContext) -> Result<Reservation> {
        self.writes
            .try_borrow_mut()
            .map_err(|_| reentrancy())?
            .reserve(context_id(context))
            .map_err(|error| match error {
                AdmissionError::Inactive | AdmissionError::Full => Error::new(
                    E_UNEXPECTED,
                    "document write journal cannot admit another key",
                ),
                AdmissionError::ReservationLost | AdmissionError::ProjectionMismatch => {
                    Error::new(E_UNEXPECTED, "document write reservation became stale")
                }
            })
    }

    fn cancel_reservation(&self, reservation: Reservation, reason: CancelReason) {
        let cancelled = match self.writes.try_borrow_mut() {
            Ok(mut writes) => writes.cancel_reservation(reservation, reason),
            Err(_) => {
                self.defer_undo_terminalization(None);
                return;
            }
        };
        self.settle_cancelled_writes(cancelled, false, None);
    }

    fn cancel_all_writes(&self, reason: CancelReason, reset_engine: bool) {
        self.cancel_all_writes_with_undo_outcome(reason, reset_engine, None);
    }

    fn cancel_all_writes_with_undo_outcome(
        &self,
        reason: CancelReason,
        reset_engine: bool,
        undo_outcome: Option<UndoCommitOutcome>,
    ) {
        let cancelled = match self.writes.try_borrow_mut() {
            Ok(mut writes) => writes.cancel_all(reason),
            Err(_) => {
                let outcome = undo_outcome.or(Some(UndoCommitOutcome::Unknown));
                self.defer_undo_terminalization(outcome);
                return;
            }
        };
        self.settle_cancelled_writes(cancelled, reset_engine, undo_outcome);
    }

    /// Makes the journal and `CompositionState` agree that no projection can
    /// safely be reused.  This is only for a known-unknown document result;
    /// ordinary cancellation keeps the last committed projection so it can be
    /// finalized safely.
    fn abandon_composition_projection(&self, reason: CancelReason) -> Result<()> {
        let cancelled = self
            .writes
            .try_borrow_mut()
            .map_err(|_| reentrancy())?
            .abandon_projection(reason);
        self.forget_composition()?;
        if cancelled.is_empty() {
            // A focus/lifecycle boundary may already have terminalized the
            // callback's journal entry. This unknown projection still owns an
            // engine/UI terminal transition; do not let an empty completion
            // list make it an accidental terminal state.
            self.terminalize_cancelled_state(true);
        } else {
            self.settle_cancelled_writes(cancelled, true, Some(UndoCommitOutcome::Unknown));
        }
        Ok(())
    }

    /// Performs the non-document terminal work shared by cancelled writes and
    /// an explicit abandoned projection. It is intentionally safe to call
    /// after the journal was already drained.
    fn terminalize_cancelled_state(&self, reset_engine: bool) {
        if let Ok(mut writes) = self.writes.try_borrow_mut() {
            writes.clear_ui_lease();
        }
        let queued = self.queue_end_candidates();
        if queued.is_err() {
            self.end_candidates();
        }
        if reset_engine {
            self.disconnect();
        }
    }

    /// Finalizes terminal operations without touching a document.  Candidate
    /// work is invalidated through the deferred owner so an edit-session
    /// callback never re-enters TSF by starting the next document request.
    fn settle_cancelled_writes(
        &self,
        completions: Vec<Completion<PendingWrite>>,
        reset_engine: bool,
        undo_outcome: Option<UndoCommitOutcome>,
    ) {
        self.settle_cancelled_writes_inner(completions, reset_engine, undo_outcome, false);
    }

    /// Finishes the ordinary document/UI cleanup after the document-free
    /// Unknown helper has already sent the one engine settlement.  The
    /// completion payload still carries the undo marker for auditing, but it
    /// must not cause a second `UndoCommit` acknowledgement here.
    fn settle_cancelled_writes_after_undo_terminalization(
        &self,
        completions: Vec<Completion<PendingWrite>>,
        reset_engine: bool,
    ) {
        self.settle_cancelled_writes_inner(completions, reset_engine, None, true);
    }

    fn settle_cancelled_writes_inner(
        &self,
        completions: Vec<Completion<PendingWrite>>,
        reset_engine: bool,
        undo_outcome: Option<UndoCommitOutcome>,
        undo_already_settled: bool,
    ) {
        for completion in &completions {
            self.settle_engine_recovery_completion(completion);
            if let Some(record) = completion
                .payload
                .as_ref()
                .and_then(|payload| payload.ai_record.clone())
            {
                let (status, reason) = match completion.outcome {
                    TerminalOutcome::Rejected => (AiTextStatus::Rejected, "host_rejected"),
                    TerminalOutcome::Cancelled(_) => (AiTextStatus::Cancelled, "host_cancelled"),
                    TerminalOutcome::Applied => (AiTextStatus::Rejected, "host_terminal_mismatch"),
                };
                self.terminalize_pending_ai_record(record, status, Some(reason));
            }
        }
        // A pending exact-text undo owns an engine transaction in addition to
        // its journal ticket. Resolve that transaction before any generic
        // disconnect/reset path can discard the link. Missing an outcome here
        // is treated as a pre-mutation rejection; callers that know a host
        // call may have changed the document pass `Unknown` explicitly.
        let mut settled_undo = false;
        for completion in &completions {
            let Some(payload) = completion.payload.as_ref() else {
                continue;
            };
            if !payload.undo_commit || settled_undo || undo_already_settled {
                continue;
            }
            settled_undo = true;
            let outcome = undo_outcome.unwrap_or(UndoCommitOutcome::Rejected);
            let _ = self.settle_undo_commit_or_disconnect(outcome);
        }
        if self.focus_finalization_phase() == FocusFinalizationPhase::EngineCommitStarted {
            // The deferred focus owner already asked the engine to commit, but
            // its queued document finalizer was rejected or cancelled. It is
            // not enough to reset the engine: the visible canonical projection
            // must be retired too, even when a lifecycle callback drained the
            // journal before this terminal path observes it.
            let retired = self.forget_composition().is_ok();
            self.terminalize_cancelled_state(true);
            if !retired {
                // A borrowed `CompositionState` cannot be retired from this
                // re-entrant stack. Transfer exactly one later document-free
                // attempt to the hidden-window owner; if it cannot be posted,
                // the state becomes an explicit detach-owned terminal rather
                // than an accidental permanently-blocked phase.
                self.queue_focus_reconciliation();
            }
            return;
        }
        if completions.is_empty() {
            if reset_engine {
                // A lifecycle owner may already have drained the journal. No
                // payload remains to identify the undo, so the connection
                // teardown is the explicit terminal boundary for any engine
                // state that could still be pending.
                self.disconnect();
            }
            return;
        }
        self.terminalize_cancelled_state(reset_engine);
    }

    fn settle_engine_recovery_completion(&self, completion: &Completion<PendingWrite>) {
        let Some(token) = completion
            .payload
            .as_ref()
            .and_then(|payload| payload.engine_recovery)
        else {
            return;
        };
        let outcome = match completion.outcome {
            TerminalOutcome::Applied => RecoveryTerminal::Applied,
            TerminalOutcome::Rejected => RecoveryTerminal::Rejected,
            TerminalOutcome::Cancelled(_) => RecoveryTerminal::Cancelled,
        };
        self.finish_engine_recovery(token, outcome);
    }
    fn attach(
        &self,
        thread_mgr: &ITfThreadMgr,
        client_id: u32,
        key_sink: &ITfKeyEventSink,
        function_provider: &ITfFunctionProvider,
        lang_bar_item: &ITfLangBarItem,
    ) -> Result<()> {
        // TSF should never activate an already-active service, but if it does,
        // silently overwriting the old activation would strand a key sink on a
        // thread manager nobody will ever unadvise.
        self.detach()?;

        // The activation transaction owns exactly one preserved physical key.
        // Refusing a malformed future registry is safer than registering a
        // broader set of keys with state-dependent engine meanings.
        let [preserved_key] = preserved_key_registrations() else {
            return Err(Error::new(
                E_UNEXPECTED,
                "preserved-key registry is not exactly one toggle binding",
            ));
        };

        let keystroke_mgr: ITfKeystrokeMgr = thread_mgr.cast()?;
        // SAFETY: `keystroke_mgr` came from a live thread manager and `sink`
        // borrows from this object, which outlives the call.
        unsafe { keystroke_mgr.AdviseKeyEventSink(client_id, key_sink, true)? };

        let function_source: ITfSourceSingle = match thread_mgr.cast() {
            Ok(source) => source,
            Err(error) => {
                // The key sink has a terminal owner even though activation
                // could not be recorded.
                // SAFETY: `keystroke_mgr` issued the key-sink registration
                // above for this client id, so this reverses that registration.
                unsafe {
                    let _ = keystroke_mgr.UnadviseKeyEventSink(client_id);
                }
                return Err(error);
            }
        };
        // TSF discovers `ITfFnReconversion` through this provider registration;
        // merely exposing the COM interface on our object is not sufficient.
        // SAFETY: `function_provider` is this live service object and the IID
        // exactly identifies the interface being advised.
        if let Err(error) = unsafe {
            function_source.AdviseSingleSink(
                client_id,
                &ITfFunctionProvider::IID,
                function_provider,
            )
        } {
            // The key sink was registered above.
            // SAFETY: `keystroke_mgr` issued the key-sink registration above
            // for this client id, so this reverses that registration.
            unsafe {
                let _ = keystroke_mgr.UnadviseKeyEventSink(client_id);
            }
            return Err(error);
        }

        if let Err(error) = preserve_registered_key(&keystroke_mgr, client_id, preserved_key) {
            // PreserveKey did not succeed, so this activation never claims
            // ownership of that GUID. The earlier registrations still need
            // their normal rollback before reporting the PreserveKey failure.
            // SAFETY: both registrations below were made above for this client
            // id and are independent of the failed preserved-key request.
            let function_result =
                unsafe { function_source.UnadviseSingleSink(client_id, &ITfFunctionProvider::IID) };
            // SAFETY: `keystroke_mgr` issued this key-sink registration above
            // for the same client id, so this reverses that exact registration.
            let key_result = unsafe { keystroke_mgr.UnadviseKeyEventSink(client_id) };
            let preserve_result: Result<()> = Err(error);
            return preserve_result.and(function_result).and(key_result);
        }

        // Windows 11 renders the first `GUID_LBI_INPUTMODE` item as the IME's
        // focused input-mode indicator. Register it with the same activation
        // transaction as every other TSF callback registration so it cannot
        // outlive this service or become a permanent notification icon.
        let lang_bar_mgr: ITfLangBarItemMgr = match thread_mgr.cast() {
            Ok(manager) => manager,
            Err(error) => {
                let preserved_key_result = unpreserve_registered_key(&keystroke_mgr, preserved_key);
                // SAFETY: both interfaces performed their matching registrations
                // earlier in this activation transaction.
                let function_result = unsafe {
                    function_source.UnadviseSingleSink(client_id, &ITfFunctionProvider::IID)
                };
                // SAFETY: this manager issued the key-sink registration above.
                let key_result = unsafe { keystroke_mgr.UnadviseKeyEventSink(client_id) };
                let manager_result: Result<()> = Err(error);
                return manager_result
                    .and(preserved_key_result)
                    .and(function_result)
                    .and(key_result);
            }
        };
        // SAFETY: `lang_bar_item` is this live text service and stays retained
        // by `Activation` until this exact manager removes it at detach.
        if let Err(error) = unsafe { lang_bar_mgr.AddItem(lang_bar_item) } {
            // Defensively request removal even on an error: a shell-side
            // partial registration must never become a permanent stale item.
            // SAFETY: `lang_bar_item` is the same live interface supplied to
            // AddItem and remains retained by this activation transaction.
            let _ = unsafe { lang_bar_mgr.RemoveItem(lang_bar_item) };
            let preserved_key_result = unpreserve_registered_key(&keystroke_mgr, preserved_key);
            // SAFETY: both interfaces performed their matching registrations
            // earlier in this activation transaction.
            let function_result =
                unsafe { function_source.UnadviseSingleSink(client_id, &ITfFunctionProvider::IID) };
            // SAFETY: this manager issued the key-sink registration above.
            let key_result = unsafe { keystroke_mgr.UnadviseKeyEventSink(client_id) };
            let item_result: Result<()> = Err(error);
            return item_result
                .and(preserved_key_result)
                .and(function_result)
                .and(key_result);
        }

        let mut slot = match self.activation.try_borrow_mut() {
            Ok(slot) => slot,
            Err(_) => {
                // All four external registrations have a terminal owner even
                // if re-entrancy prevents publication. PreserveKey succeeded,
                // so its exact UnpreserveKey must run before this branch can
                // return; function-provider and key-sink rollback still run
                // even if that first cleanup reports an error.
                // SAFETY: this is the exact manager/item pair that just
                // completed `AddItem` above.
                let language_bar_result = unsafe { lang_bar_mgr.RemoveItem(lang_bar_item) };
                self.mode_item.reset();
                let preserved_key_result = unpreserve_registered_key(&keystroke_mgr, preserved_key);
                // SAFETY: these calls reverse the registrations made above for
                // this client id; no RefCell borrow is held across either call.
                let function_result = unsafe {
                    function_source.UnadviseSingleSink(client_id, &ITfFunctionProvider::IID)
                };
                // SAFETY: `keystroke_mgr` issued this key-sink registration
                // above for the same client id, so this reverses it after the
                // preserved-key rollback without holding a RefCell borrow.
                let key_result = unsafe { keystroke_mgr.UnadviseKeyEventSink(client_id) };
                let reentrancy_result: Result<()> = Err(reentrancy());
                return reentrancy_result
                    .and(language_bar_result)
                    .and(preserved_key_result)
                    .and(function_result)
                    .and(key_result);
            }
        };
        *slot = Some(Activation {
            thread_mgr: thread_mgr.clone(),
            keystroke_mgr,
            function_source,
            lang_bar_mgr,
            lang_bar_item: lang_bar_item.clone(),
            client_id,
            preserved_key,
        });
        Ok(())
    }

    fn detach(&self) -> Result<()> {
        self.record_diagnostic_lifecycle(diagnostic_ring::LifecycleEvent::Deactivate, 0);
        // The focused indicator has a stricter lifetime than the background
        // engine connection: focus loss/deactivation hides it immediately,
        // before any deferred composition settlement can re-enter TSF.
        self.focus_foreground.set(false);
        self.mode_item.hide();
        self.deactivate_write_journal();
        // Detach is the final lifecycle owner even if a re-entrant journal
        // borrow prevented ordinary completion settlement above. No callback
        // may keep fencing keys after this text-service instance is detached.
        self.cancel_engine_recovery();
        // Deactivation is the last chance to drop the composition handle. The
        // document is going away with it, so there is nothing to end — holding
        // the reference longer would just outlive the context it belongs to.
        // A queued focus-reconciliation retry loses its window owner here.
        // `destroy_deferred_window` records `ReconciliationAwaitingLifecycle`,
        // and this method below is the explicit terminal owner. Continue
        // unadvising even when retirement is re-entrantly refused, then return
        // that error rather than silently stranding the blocked phase.
        self.cancel_pending_ai("service_detached");
        self.ai_key_latched.set(false);
        let deferred_window_result = self.destroy_deferred_window();
        self.end_candidates();
        let composition_result = self.forget_composition();
        self.disconnect();

        // The borrow ends before the TSF call: unadvising re-enters TSF, and a
        // callback arriving while the cell is still borrowed would be a double
        // borrow, which under `panic = "abort"` takes the host process with it.
        let previous = match self.activation.try_borrow_mut() {
            Ok(mut slot) => slot.take(),
            Err(_) => {
                return deferred_window_result
                    .and(composition_result)
                    .and(Err(reentrancy()));
            }
        };
        let Some(activation) = previous else {
            self.mode_item.reset();
            return deferred_window_result.and(composition_result);
        };

        // All cleanup operations are attempted even if the first fails;
        // otherwise one failed cleanup would strand another registration for
        // the host thread. Retaining the exact manager from activation avoids
        // a fallible cast becoming an early terminal before key cleanup.
        // SAFETY: this exact manager added this exact item in `attach`.
        let language_bar_result = unsafe {
            activation
                .lang_bar_mgr
                .RemoveItem(&activation.lang_bar_item)
        };
        self.mode_item.reset();
        let preserved_key_result =
            unpreserve_registered_key(&activation.keystroke_mgr, activation.preserved_key);
        // SAFETY: the retained function source issued this registration for
        // the exact client id during activation.
        let function_result = unsafe {
            activation
                .function_source
                .UnadviseSingleSink(activation.client_id, &ITfFunctionProvider::IID)
        };
        // SAFETY: this retained manager issued the key-event registration for
        // the exact client id during activation.
        let key_result = unsafe {
            activation
                .keystroke_mgr
                .UnadviseKeyEventSink(activation.client_id)
        };
        deferred_window_result
            .and(composition_result)
            .and(language_bar_result)
            .and(preserved_key_result)
            .and(function_result)
            .and(key_result)
    }

    /// Opens the connection to the engine before the first keystroke needs
    /// it, so the cost of connecting lands on activation rather than on a
    /// character the user is waiting to see.
    fn warm_up(&self) {
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            engine.warm_up();
        }
        self.sync_mode_item();
    }

    /// Returns the context currently owning the thread-manager focus. This is
    /// intentionally resolved afresh for a language-bar menu invocation: a
    /// menu can remain open while an application changes documents.
    fn focused_context(&self) -> Result<ITfContext> {
        let manager = self.thread_manager()?;
        // SAFETY: the manager is retained for this active service thread; TSF
        // returns independently owned document/context interfaces.
        unsafe {
            let document = manager.GetFocus()?;
            document.GetTop()
        }
    }

    /// Sends only a cached, already-resolved engine status to the shell. A
    /// missing link is not guessed: the item stays hidden until the active
    /// session has identified its real input mode.
    fn sync_mode_item(&self) {
        if !self.focus_foreground.get() {
            self.mode_item.hide();
            return;
        }
        let status = self
            .engine
            .try_borrow()
            .ok()
            .and_then(|engine| engine.input_mode_status());
        match status {
            Some(status) => self.mode_item.update(
                true,
                Some(status.mode),
                status.can_change,
                status.can_restore,
            ),
            None => self.mode_item.hide(),
        }
    }

    /// Updates the engine's fail-closed scope classification before exposing
    /// a focused input-mode item or accepting a language-bar menu operation.
    fn refresh_mode_item_for_focus(&self) {
        if !self.focus_foreground.get() {
            self.mode_item.hide();
            return;
        }
        match self.focused_context() {
            Ok(context) => {
                let _ = self.publish_input_scope(&context);
            }
            Err(_) => self.mode_item.hide(),
        }
    }

    /// Executes a menu command only for the still-focused, positively
    /// classified ordinary-text session. No TSF document edit session is
    /// requested here; the engine rejects a composing session before changing
    /// any persistent mode, so a menu click cannot commit or rewrite text.
    fn select_mode_menu_command(&self, command: MenuCommand) {
        if !self.focus_foreground.get() {
            return;
        }
        self.refresh_mode_item_for_focus();
        if !self.focus_foreground.get() {
            return;
        }

        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            let status = engine.input_mode_status();
            let can_change = status.is_some_and(|status| status.can_change);
            match command {
                MenuCommand::RestoreMode if status.is_some_and(|status| status.can_restore) => {
                    let _ = engine.restore_input_mode();
                }
                MenuCommand::SetMode(mode) if can_change => {
                    let _ = engine.set_input_mode(mode);
                }
                MenuCommand::ToggleIme if can_change => {
                    let target = if status.is_some_and(|status| status.mode == Mode::Direct) {
                        Mode::Hiragana
                    } else {
                        Mode::Direct
                    };
                    let _ = engine.set_input_mode(target);
                }
                // Settings is handled before this document-scoped command
                // path. Keep the defensive terminal arm explicit in case a
                // future caller bypasses OnMenuSelect.
                MenuCommand::OpenSettings => {}
                MenuCommand::AiTransform | MenuCommand::AiProofread => {}
                _ => {}
            }
        }
        self.sync_mode_item();
    }

    /// Closes the connection, which is also what tells the engine to forget
    /// this thread's session: the session table is keyed to the connection,
    /// so there is no `DeleteSession` to send and no way to leak one by
    /// failing to send it.
    fn disconnect(&self) {
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            *engine = Engine::new();
        }
        self.sync_mode_item();
    }

    /// Puts one question to the engine.
    ///
    /// The borrow is confined to this function. It has to be: the call
    /// inside blocks for up to the keystroke budget, and TSF must be able
    /// to re-enter this object the moment it returns.
    fn ask(&self, key: sakura_proto::KeyInput) -> Result<Answer> {
        let mut engine = self.engine.try_borrow_mut().map_err(|_| reentrancy())?;
        let answer = engine.send_key(key);
        drop(engine);
        self.sync_mode_item();
        Ok(answer)
    }

    fn ask_probe(
        &self,
        scope: sakura_proto::InputScope,
        key: sakura_proto::KeyInput,
        fresh_context: bool,
    ) -> Result<Answer> {
        let mut engine = self.engine.try_borrow_mut().map_err(|_| reentrancy())?;
        Ok(engine.probe_key_for_context(scope, key, fresh_context))
    }

    /// Reads the focused range's TSF input scope under a synchronous read
    /// lock.
    ///
    /// A host that attaches no input-scope value is stating that the field
    /// carries no restriction, and that classifies as `Normal`. A refused
    /// read, a malformed VARIANT, or a scope value this build does not
    /// recognise becomes `Unclassified`; the engine's persistence policy
    /// remains fail-closed for those.
    fn read_input_scope(&self, context: &ITfContext) -> Result<sakura_proto::InputScope> {
        let client_id = self.client_id()?;
        let owned_context = context.clone();
        match edit_session::read_in_document_sync(context, client_id, move |ec| {
            classify_input_scope_with_cookie(&owned_context, ec)
        }) {
            Ok(scope) => {
                self.cached_input_scope.set(Some(scope));
                Ok(scope)
            }
            Err(_) => Ok(self
                .cached_input_scope
                .get()
                .unwrap_or(sakura_proto::InputScope::Unclassified)),
        }
    }

    fn remember_input_scope_from_cookie(&self, context: &ITfContext, ec: u32) {
        let Ok(scope) = classify_input_scope_with_cookie(context, ec) else {
            return;
        };
        self.cached_input_scope.set(Some(scope));
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            let _ = engine.set_input_scope(scope);
        }
    }

    /// Publishes the focused field's scope before a real key request. Probe
    /// calls `read_input_scope` but deliberately does not enter this method.
    fn publish_input_scope(&self, context: &ITfContext) -> Result<bool> {
        let scope = self.read_input_scope(context)?;
        let mut engine = self.engine.try_borrow_mut().map_err(|_| reentrancy())?;
        let published = engine.set_input_scope(scope);
        drop(engine);
        self.sync_mode_item();
        Ok(published)
    }

    fn ai_trigger_matches(&self, key: KeyInput) -> bool {
        if key.modifiers.without_locks() != Modifiers::NONE {
            return false;
        }
        matches!(
            (read_ai_text_key(), key.code),
            (AiTextKey::Henkan, KeyCode::Henkan) | (AiTextKey::CapsLock, KeyCode::CapsLock)
        )
    }

    fn capture_ai_target(
        &self,
        context: &ITfContext,
        operation: AiTextOperation,
    ) -> Result<Option<(String, AiTextTarget)>> {
        if operation == AiTextOperation::Proofread {
            let composition = self.composition.try_borrow().map_err(|_| reentrancy())?;
            if composition.handle.is_some()
                && composition
                    .context
                    .as_ref()
                    .is_some_and(|owned| context_id(owned) == context_id(context))
            {
                return Ok(None);
            }
        }
        if operation == AiTextOperation::Transform {
            let composition = self.composition.try_borrow().map_err(|_| reentrancy())?;
            if composition.known
                && composition.handle.is_some()
                && composition
                    .context
                    .as_ref()
                    .is_some_and(|owned| context_id(owned) == context_id(context))
                && !composition.text.is_empty()
            {
                return Ok(Some((composition.text.clone(), AiTextTarget::Composition)));
            }
        }

        let client_id = self.client_id()?;
        let owned_context = context.clone();
        match edit_session::read_in_document_sync(context, client_id, move |ec| {
            let range = composition::current_selection_range(&owned_context, ec, &mut || Ok(()))?;
            let text = read_range_text(&range, ec)?;
            Ok((text, AiTextTarget::Selection(range)))
        }) {
            Ok(target) => Ok(Some(target)),
            // Empty, oversized, malformed, or unavailable selections are not
            // AI targets. The key is then left to its existing IME behavior.
            Err(_) => Ok(None),
        }
    }

    fn set_ai_error(&self, message: Option<String>) {
        let message = message.map(|mut value| {
            if value.len() > 192 {
                value.truncate(192);
            }
            value
        });
        if let Ok(mut slot) = self.last_ai_error.try_borrow_mut() {
            *slot = message;
        }
        self.mode_item.notify_tooltip();
    }

    fn record_ai_result(
        &self,
        operation: AiTextOperation,
        source: String,
        result: AiTextResult,
        status: AiTextStatus,
        error_override: Option<&str>,
    ) {
        let error_code = error_override
            .map(str::to_owned)
            .unwrap_or_else(|| result.error_code.clone());
        let record = AiTextRecord {
            operation,
            status,
            source,
            result: result.result,
            model: result.model,
            provider: result.provider,
            style: result.style,
            error_code,
            latency_ms: result.latency_ms,
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            cached_tokens: result.cached_tokens,
            attempts: result.attempts,
            test_only: false,
        };
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            let _ = engine.record_ai_text(&record);
        }
    }

    fn record_cancelled_ai(&self, pending: PendingAiText, reason: &str) {
        self.record_ai_result(
            pending.operation,
            pending.source,
            AiTextResult {
                status: AiTextStatus::Cancelled,
                result: String::new(),
                model: "gpt-5.6-luna".to_owned(),
                provider: String::new(),
                style: String::new(),
                error_code: reason.to_owned(),
                latency_ms: 0,
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                attempts: 0,
            },
            AiTextStatus::Cancelled,
            Some(reason),
        );
    }

    fn terminalize_pending_ai_record(
        &self,
        record: PendingAiRecord,
        status: AiTextStatus,
        reason: Option<&str>,
    ) {
        self.record_ai_result(
            record.operation,
            record.source,
            record.result,
            status,
            reason,
        );
    }

    fn cancel_pending_ai(&self, reason: &str) {
        let pending = self
            .ai_text
            .try_borrow_mut()
            .ok()
            .and_then(|mut state| state.pending.take());
        let Some(pending) = pending else {
            return;
        };
        self.stop_ai_text_timer();
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            let _ = engine.cancel_ai_text(pending.job);
        }
        self.record_cancelled_ai(pending, reason);
    }

    fn start_ai_text_request(
        &self,
        context: &ITfContext,
        operation: AiTextOperation,
    ) -> Result<bool> {
        if !self.focus_foreground.get() {
            return Ok(false);
        }
        if self
            .ai_text
            .try_borrow()
            .map_err(|_| reentrancy())?
            .pending
            .is_some()
        {
            return Ok(true);
        }
        if self.read_input_scope(context)? != InputScope::Normal {
            return Ok(false);
        }
        let Some((source, target)) = self.capture_ai_target(context, operation)? else {
            return Ok(false);
        };
        if !self.publish_input_scope(context)? {
            self.set_ai_error(Some("AI機能を利用できない入力欄です".to_owned()));
            return Ok(true);
        }
        let started = self
            .engine
            .try_borrow_mut()
            .map_err(|_| reentrancy())?
            .start_ai_text(operation, source.clone());
        let job = match started {
            Ok(job) => job,
            Err(code) => {
                let error = format!("AIリクエストを開始できませんでした ({code:?})");
                self.set_ai_error(Some(error));
                self.record_ai_result(
                    operation,
                    source,
                    AiTextResult {
                        status: AiTextStatus::Rejected,
                        result: String::new(),
                        model: "gpt-5.6-luna".to_owned(),
                        provider: String::new(),
                        style: String::new(),
                        error_code: format!("start_{code:?}").to_ascii_lowercase(),
                        latency_ms: 0,
                        input_tokens: 0,
                        output_tokens: 0,
                        cached_tokens: 0,
                        attempts: 0,
                    },
                    AiTextStatus::Rejected,
                    None,
                );
                return Ok(true);
            }
        };
        self.ai_text
            .try_borrow_mut()
            .map_err(|_| reentrancy())?
            .pending = Some(PendingAiText {
            job,
            operation,
            context: context.clone(),
            source,
            target,
        });
        if let Err(error) = self.arm_ai_text_timer() {
            self.cancel_pending_ai("timer_unavailable");
            self.set_ai_error(Some("AI結果の監視を開始できませんでした".to_owned()));
            return Err(error);
        }
        self.set_ai_error(None);
        Ok(true)
    }

    fn ask_reconversion(&self, text: String, preview: bool) -> Result<Answer> {
        let mut engine = self.engine.try_borrow_mut().map_err(|_| reentrancy())?;
        Ok(engine.reconvert(text, preview))
    }

    fn revert_engine(&self) {
        if let Ok(mut engine) = self.engine.try_borrow_mut() {
            let _ = engine.revert();
        }
    }

    fn settle_undo_commit(&self, outcome: UndoCommitOutcome) -> bool {
        let Ok(mut engine) = self.engine.try_borrow_mut() else {
            return false;
        };
        engine.settle_undo_commit(outcome)
    }

    fn settle_undo_commit_or_disconnect(&self, outcome: UndoCommitOutcome) -> bool {
        let settled = self.settle_undo_commit(outcome);
        if !settled {
            // A timeout leaves the engine transport marked desynchronized but
            // still connected. No journal completion remains to own a later
            // retry in this helper's early terminal branches, so retire the
            // link explicitly.
            self.disconnect();
        }
        settled
    }

    /// Reads one supplied range under the synchronous lock required by the
    /// return-valued reconversion COM methods.
    fn range_text(&self, range: &ITfRange) -> Result<String> {
        let client_id = self.client_id()?;
        // SAFETY: `range` is a live callback argument and retains its context.
        let context = unsafe { range.GetContext()? };
        let owned_range = range.clone();
        edit_session::read_in_document_sync(&context, client_id, move |ec| {
            read_range_text(&owned_range, ec)
        })
    }

    fn composition_is_idle(&self) -> Result<bool> {
        let state = self.composition.try_borrow().map_err(|_| reentrancy())?;
        Ok(state.known && state.handle.is_none() && state.text.is_empty())
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

    /// Starts the engine half of a deferred focus finalization. The caller
    /// transitions the phase before this call because `Engine::commit` can
    /// synchronously re-enter TSF. A borrow failure means we cannot establish
    /// whether the engine changed, so the caller must document-free abandon.
    fn ask_to_finalize_for_focus(&self) -> bool {
        let Ok(mut engine) = self.engine.try_borrow_mut() else {
            return false;
        };
        let _ = engine.commit();
        true
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
        // Lifecycle owns retirement. Any callback that was holding a clone of
        // this handle loses its publish right before we drop the canonical
        // state, so it cannot resurrect the composition after returning from
        // a COM call.
        state.write_owner.invalidate();
        state.expected_self_termination = None;
        state.text.clear();
        state.handle = None;
        state.context = None;
        state.known = true;
        drop(state);
        self.finish_focus_finalization();
        Ok(())
    }

    fn composition_context(&self) -> Option<ITfContext> {
        self.composition
            .try_borrow()
            .ok()
            .and_then(|state| state.context.clone())
    }

    fn client_id(&self) -> Result<u32> {
        let slot = self.activation.try_borrow().map_err(|_| reentrancy())?;
        match slot.as_ref() {
            Some(activation) => Ok(activation.client_id),
            None => Err(Error::new(E_UNEXPECTED, "key event before activation")),
        }
    }

    fn thread_manager(&self) -> Result<ITfThreadMgr> {
        let slot = self.activation.try_borrow().map_err(|_| reentrancy())?;
        slot.as_ref()
            .map(|activation| activation.thread_mgr.clone())
            .ok_or_else(|| Error::new(E_UNEXPECTED, "candidate UI before activation"))
    }

    /// Ends both candidate-facing contracts: TSF's UI element and the layout
    /// subscription that owns future geometry retries.
    fn end_candidates(&self) {
        if !self.begin_candidate_operation() {
            if self.queue_end_candidates().is_err() {
                self.candidate_end_pending.set(true);
            }
            return;
        }
        // This operation is now the active owner of every earlier end request.
        // A nested lifecycle callback may set the bit again while one of the
        // host calls below is pumping messages.
        self.candidate_end_pending.set(false);

        let subscription = self.layout.try_borrow_mut().ok().and_then(|mut layout| {
            layout.phase = GeometryPhase::Idle;
            layout.last_anchor = None;
            layout.last_document = None;
            self.layout_abandon_pending.set(None);
            layout.subscription.take()
        });
        let mut controller = match self.candidate_ui.try_borrow_mut() {
            Ok(mut slot) => core::mem::take(&mut *slot),
            Err(_) => {
                self.candidate_end_pending.set(true);
                let _ = self.finish_candidate_operation();
                return;
            }
        };
        run_candidate_teardown_host_calls(
            subscription,
            &mut controller,
            |subscription| {
                // SAFETY: the cookie was issued by this retained source and
                // was removed from our state before the call, so it cannot be
                // ended twice even if TSF re-enters us.
                unsafe {
                    let _ = subscription.source.UnadviseSink(subscription.cookie);
                }
            },
            |controller| {
                let _ = controller.end();
            },
        );
        if let Ok(mut slot) = self.candidate_ui.try_borrow_mut() {
            *slot = controller;
        } else {
            // `EndUIElement` has already run, so dropping this inactive local
            // record is safe. Preserve a later end request only for any nested
            // candidate work whose controller could not yet be observed.
            self.candidate_end_pending.set(true);
        }
        let _ = self.finish_candidate_operation();
    }

    fn ensure_layout_subscription(
        &self,
        context: &ITfContext,
        sink: &ITfTextLayoutSink,
        lease: UiLease,
    ) -> Result<()> {
        // An older same-context callback can arrive after a newer output has
        // installed its lease. Check before borrowing or rolling over that
        // subscription: stale work must not replace its lease or reset its
        // geometry phase.
        let proposed_is_current = self.ui_lease_is_current(context, lease);
        if !proposed_is_current {
            return Ok(());
        }
        // A new document output can own the same context before an old async
        // geometry callback returns. Roll that callback's lease over while no
        // RefCell borrow crosses a TSF call; it will fail its lease gate and
        // cannot complete geometry for the newer output.
        let retired = {
            let mut layout = self.layout.try_borrow_mut().map_err(|_| reentrancy())?;
            match layout.subscription.as_mut() {
                Some(subscription) if subscription.context.as_raw() == context.as_raw() => {
                    let (retained_lease, retire_geometry) = resolve_same_context_layout_lease(
                        subscription.lease,
                        lease,
                        proposed_is_current,
                    );
                    subscription.lease = retained_lease;
                    if retire_geometry {
                        // `QueryQueued` belonged to the old lease. Changing
                        // the lease is its terminal abandonment; the new owner
                        // will schedule an independent query.
                        layout.retire_geometry_for_lease_rollover();
                    }
                    return Ok(());
                }
                Some(_) => {
                    layout.retire_geometry_for_lease_rollover();
                    layout.subscription.take()
                }
                None => None,
            }
        };
        if let Some(subscription) = retired {
            // SAFETY: the subscription was removed from state before this COM
            // call, so re-entrancy cannot unadvise it twice.
            unsafe {
                let _ = subscription.source.UnadviseSink(subscription.cookie);
            }
        }

        // Retiring the previous cookie is itself a re-entrant host call. A
        // lifecycle callback entered there may have revoked this proposal, in
        // which case it must not continue into context discovery or AdviseSink.
        if !self.ui_lease_is_current(context, lease) {
            return Ok(());
        }
        let source: ITfSource = context.cast()?;
        // `cast` can re-enter. Do not begin a new registration after a newer
        // candidate/context lease has already claimed the UI owner.
        if !self.ui_lease_is_current(context, lease) {
            return Ok(());
        }
        // SAFETY: `sink` is this live text-service object and the IID exactly
        // identifies the interface it implements.
        let cookie = unsafe { source.AdviseSink(&ITfTextLayoutSink::IID, sink)? };
        let proposed = LayoutSubscription {
            source,
            context: context.clone(),
            lease,
            cookie,
        };
        // `AdviseSink` can synchronously pump messages too. Check again before
        // touching the slot: if an inner callback installed a newer owner, the
        // only object this older proposal may retire is its own cookie.
        let proposed_is_current = self.ui_lease_is_current(context, lease);
        let retired = {
            let mut layout = self.layout.try_borrow_mut().map_err(|_| reentrancy())?;
            let (retained, retired) = merge_layout_subscription(
                layout.subscription.take(),
                proposed,
                proposed_is_current,
            );
            if proposed_is_current {
                // A current proposal replaces the previous geometry owner. A
                // stale proposal deliberately leaves the newer phase untouched.
                layout.retire_geometry_for_lease_rollover();
            }
            layout.subscription = retained;
            retired
        };
        if let Some(subscription) = retired {
            // The subscription was removed (or never installed) before this
            // COM call. For a stale proposal this is specifically its newly
            // advised cookie; a newer retained subscription stays untouched.
            // SAFETY: its cookie was issued by its retained source.
            unsafe {
                let _ = subscription.source.UnadviseSink(subscription.cookie);
            }
        }
        Ok(())
    }

    /// Claims ownership of one query. Repeated layout callbacks coalesce
    /// while an asynchronous edit session is still queued.
    fn begin_layout_query(&self, context: &ITfContext, lease: UiLease) -> Result<bool> {
        // A refused callback owns the previous `QueryQueued` phase until its
        // bounded deferred/lifecycle terminal path has observed it. Never let
        // a newer request overwrite that explicit owner.
        if self.layout_abandon_pending.get().is_some() {
            return Ok(false);
        }
        let active = self
            .candidate_ui
            .try_borrow()
            .map_err(|_| reentrancy())?
            .is_active();
        if !active {
            return Ok(false);
        }
        let mut layout = self.layout.try_borrow_mut().map_err(|_| reentrancy())?;
        let is_current = layout.subscription.as_ref().is_some_and(|subscription| {
            subscription.context.as_raw() == context.as_raw() && subscription.lease == lease
        });
        if !is_current {
            return Ok(false);
        }
        if layout.phase == GeometryPhase::QueryQueued {
            return Ok(false);
        }
        layout.phase = GeometryPhase::QueryQueued;
        Ok(true)
    }

    /// Leaves a claimed query in a terminal state without publishing geometry.
    /// If a newer lease replaced this one, `ensure_layout_subscription` already
    /// abandoned the old phase by resetting it to `Idle`.
    fn abandon_layout_query(&self, context: &ITfContext, lease: UiLease) -> Result<()> {
        let claimed = LayoutQueryClaim {
            context: context_id(context),
            lease,
        };
        let mut layout = self.layout.try_borrow_mut().map_err(|_| reentrancy())?;
        let installed = layout
            .subscription
            .as_ref()
            .map(|subscription| LayoutQueryClaim {
                context: context_id(&subscription.context),
                lease: subscription.lease,
            });
        let previous = layout.phase;
        abandon_matching_geometry(&mut layout.phase, installed, claimed);
        if previous != layout.phase {
            layout.last_anchor = None;
            layout.last_document = None;
        }
        Ok(())
    }

    /// Settles the out-of-band claim once. `Some(true)` means the installed
    /// query was changed to its terminal phase, while `Some(false)` means a
    /// lease rollover had already retired that older claim and a newer owner may
    /// proceed. A borrow refusal leaves the claim intact without reposting, so
    /// the next layout/candidate lifecycle event owns the only later attempt.
    fn settle_pending_layout_abandon(&self) -> Result<Option<bool>> {
        let Some(claimed) = self.layout_abandon_pending.get() else {
            return Ok(None);
        };
        let mut layout = self.layout.try_borrow_mut().map_err(|_| reentrancy())?;
        let installed = layout
            .subscription
            .as_ref()
            .map(|subscription| LayoutQueryClaim {
                context: context_id(&subscription.context),
                lease: subscription.lease,
            });
        let previous = layout.phase;
        abandon_matching_geometry(&mut layout.phase, installed, claimed);
        let terminalized = previous != layout.phase;
        if terminalized {
            layout.last_anchor = None;
            layout.last_document = None;
        }
        self.layout_abandon_pending.set(None);
        Ok(Some(terminalized))
    }

    /// Transfers a query that could not finish synchronously to exactly one
    /// hidden-window attempt. The claim itself survives a failed post or a
    /// refused deferred borrow and is then owned by candidate teardown/detach.
    fn terminalize_layout_query(&self, context: &ITfContext, lease: UiLease) {
        if self.abandon_layout_query(context, lease).is_ok() {
            let claimed = LayoutQueryClaim {
                context: context_id(context),
                lease,
            };
            if self.layout_abandon_pending.get() == Some(claimed) {
                self.layout_abandon_pending.set(None);
            }
            return;
        }
        self.layout_abandon_pending.set(Some(LayoutQueryClaim {
            context: context_id(context),
            lease,
        }));
        // Failure deliberately does not retry here. The out-of-band claim is
        // the observable lifecycle obligation after this single post attempt.
        let _ = self.queue_layout_abandon();
    }

    /// Finalizes the query claimed by [`Self::begin_layout_query`]. The caller
    /// must have validated the lease after its final COM call; this method
    /// repeats the ownership check before changing phase or engine placement.
    fn complete_layout_query(
        &self,
        context: &ITfContext,
        lease: UiLease,
        result: composition::GeometryResult,
        document: Option<ScreenRect>,
    ) -> Result<()> {
        if !self.ui_lease_is_current(context, lease) {
            self.terminalize_layout_query(context, lease);
            return Ok(());
        }
        let visible = self
            .candidate_ui
            .try_borrow()
            .map_err(|_| reentrancy())?
            .renderer_visible()?;
        if !self.ui_lease_is_current(context, lease) {
            self.terminalize_layout_query(context, lease);
            return Ok(());
        }
        let (anchor, document) = {
            let mut layout = self.layout.try_borrow_mut().map_err(|_| reentrancy())?;
            let is_current = layout.subscription.as_ref().is_some_and(|subscription| {
                subscription.context.as_raw() == context.as_raw() && subscription.lease == lease
            });
            if !is_current || layout.phase != GeometryPhase::QueryQueued {
                return Ok(());
            }
            match result {
                composition::GeometryResult::Ready(rect) => {
                    layout.phase = GeometryPhase::Ready;
                    layout.last_anchor = Some(rect);
                    layout.last_document = document;
                }
                composition::GeometryResult::NoLayout => {
                    // No immediate retry. `ITfTextLayoutSink::OnLayoutChange`
                    // is now the sole owner of the next attempt.
                    layout.phase = GeometryPhase::WaitingForLayout;
                }
                composition::GeometryResult::Unavailable => {
                    layout.phase = GeometryPhase::Unavailable;
                    layout.last_anchor = None;
                    layout.last_document = None;
                }
            }
            (layout.last_anchor, layout.last_document)
        };
        if !self.ui_lease_is_current(context, lease) {
            self.terminalize_layout_query(context, lease);
            return Ok(());
        }
        let mut engine = self.engine.try_borrow_mut().map_err(|_| reentrancy())?;
        let _ = engine.set_ui_placement(anchor, document, visible);
        Ok(())
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

    fn composition_projection(&self) -> Result<VisibleState> {
        let state = self.composition.try_borrow().map_err(|_| reentrancy())?;
        if !state.known {
            return Err(Error::new(
                E_UNEXPECTED,
                "composition projection is unknown after a failed document edit",
            ));
        }
        Ok(VisibleState {
            text: state.text.clone(),
            has_composition: state.handle.is_some(),
        })
    }

    /// Begins a document write without removing the canonical handle from
    /// `CompositionState`.  The callback gets a clone and a flight token; only
    /// that exact token may publish or mark the projection unknown afterwards.
    fn begin_composition_write(
        &self,
        expected: &VisibleState,
    ) -> Result<Option<(Option<ITfComposition>, CompositionFlight)>> {
        let mut state = self
            .composition
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        let projection = VisibleState {
            text: state.text.clone(),
            has_composition: state.handle.is_some(),
        };
        if !state.known || projection != *expected {
            return Ok(None);
        }
        let Some(flight) = state.write_owner.begin() else {
            return Ok(None);
        };
        let context_identity = state
            .context
            .as_ref()
            .map(|context| context_id(context).0 as u64)
            .unwrap_or(0);
        let result = Some((state.handle.clone(), flight));
        drop(state);
        self.record_diagnostic_lifecycle(
            diagnostic_ring::LifecycleEvent::CompositionStarted,
            context_identity,
        );
        Ok(result)
    }

    /// Arms the one lifecycle notification expected from this callback's own
    /// `EndComposition`. The canonical handle stays in `CompositionState`; only
    /// its COM identity is recorded, and this borrow ends before the host call.
    fn expect_self_composition_termination(
        &self,
        flight: CompositionFlight,
        composition: &ITfComposition,
    ) -> Result<()> {
        let mut state = self
            .composition
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        let identity = composition_identity(composition);
        let canonical = state.handle.as_ref().map(composition_identity);
        if !state.write_owner.owns(flight)
            || canonical != Some(identity)
            || state.expected_self_termination.is_some()
        {
            return Err(Error::new(
                E_UNEXPECTED,
                "composition termination ownership changed before EndComposition",
            ));
        }
        state.expected_self_termination = Some(ExpectedSelfTermination {
            flight,
            composition: identity,
        });
        Ok(())
    }

    /// Clears an unconsumed expectation immediately after the update returns.
    /// A synchronous self-termination may already have consumed it; that is the
    /// same terminal state and is intentionally a no-op here.
    fn clear_expected_self_composition_termination(&self, flight: CompositionFlight) -> Result<()> {
        let mut state = self
            .composition
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        if state
            .expected_self_termination
            .is_some_and(|expected| expected.flight == flight)
        {
            state.expected_self_termination = None;
        }
        Ok(())
    }

    /// Consumes only the lifecycle callback generated by the currently owning
    /// flight's own `EndComposition`. Every other termination remains external
    /// and must invalidate the write before state is changed.
    fn consume_expected_self_composition_termination(&self, composition: &ITfComposition) -> bool {
        let Ok(mut state) = self.composition.try_borrow_mut() else {
            return false;
        };
        let terminated = composition_identity(composition);
        let canonical = state.handle.as_ref().map(composition_identity);
        let expected = expected_self_termination_matches(
            &state.write_owner,
            state.expected_self_termination,
            canonical,
            terminated,
        );
        if expected {
            state.expected_self_termination = None;
        }
        expected
    }

    fn composition_flight_lifecycle_is_current(&self, flight: CompositionFlight) -> Result<bool> {
        self.composition
            .try_borrow()
            .map_err(|_| reentrancy())
            .map(|state| state.write_owner.lifecycle_is_current(flight))
    }

    fn commit_composition_projection(
        &self,
        flight: CompositionFlight,
        projection: &VisibleState,
        handle: Option<ITfComposition>,
        context: &ITfContext,
    ) -> Result<bool> {
        let composition_idle = {
            let mut state = self
                .composition
                .try_borrow_mut()
                .map_err(|_| reentrancy())?;
            if !state.write_owner.finish(flight) {
                return Ok(false);
            }
            if state
                .expected_self_termination
                .is_some_and(|expected| expected.flight == flight)
            {
                state.expected_self_termination = None;
            }
            state.text = projection.text.clone();
            state.handle = handle;
            state.known = true;
            state.context = if state.handle.is_some() {
                Some(context.clone())
            } else {
                None
            };
            state.handle.is_none() && state.text.is_empty()
        };
        if composition_idle {
            self.finish_focus_finalization();
        }
        Ok(true)
    }

    /// Keeps the real handle only long enough for the host to retire it, but
    /// never uses it as if its text still matched a speculative projection.
    /// Returns `false` when a lifecycle event already retired this callback's
    /// ownership, in which case lifecycle state remains authoritative.
    fn fail_composition_write(
        &self,
        flight: CompositionFlight,
        handle: Option<ITfComposition>,
        context: &ITfContext,
    ) -> Result<bool> {
        let mut state = self
            .composition
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        if !state.write_owner.finish(flight) {
            return Ok(false);
        }
        state.text.clear();
        if state
            .expected_self_termination
            .is_some_and(|expected| expected.flight == flight)
        {
            state.expected_self_termination = None;
        }
        state.handle = merge_canonical_handle(state.handle.take(), handle);
        state.context = Some(context.clone());
        state.known = false;
        Ok(true)
    }

    /// Terminalizes a write after a host call may have changed the document.
    ///
    /// This is deliberately owned by `TextService`, rather than by the COM
    /// callback wrapper, so the document-unknown rule is testable without a
    /// live edit session and every caller reports the same `Unknown` undo
    /// outcome. A stale flight is still terminalized through the same journal
    /// primitive: its lifecycle owner may already have revoked the callback,
    /// but retrying after a host call is still unsafe.
    fn reconcile_document_access_failure(
        &self,
        ticket: Ticket,
        flight: CompositionFlight,
        handle: Option<ITfComposition>,
        context: &ITfContext,
        _stale_reason: CancelReason,
    ) {
        match self.fail_composition_write(flight, handle, context) {
            // Both a current flight and one already revoked by lifecycle
            // re-entry have crossed a document-access boundary.  The shared
            // primitive therefore rejects the ticket with
            // `document_may_have_changed = true`, identifies any undo payload,
            // and sends Unknown exactly once before ordinary UI cleanup.
            Ok(true) | Ok(false) => {
                let terminal = match self.writes.try_borrow_mut() {
                    Ok(mut writes) => terminalize_unknown_undo_after_document_access(
                        &mut writes,
                        ticket,
                        |payload| payload.undo_commit,
                        |outcome| self.settle_undo_commit(outcome),
                    ),
                    Err(_) => {
                        self.cancel_all_writes_with_undo_outcome(
                            CancelReason::RevisionMismatch,
                            true,
                            Some(UndoCommitOutcome::Unknown),
                        );
                        self.disconnect();
                        return;
                    }
                };
                self.settle_cancelled_writes_after_undo_terminalization(terminal.completions, true);
                if terminal.disconnect_required {
                    self.disconnect();
                }
            }
            Err(_) => self.cancel_all_writes_with_undo_outcome(
                CancelReason::RevisionMismatch,
                true,
                Some(UndoCommitOutcome::Unknown),
            ),
        }
        self.disconnect();
    }

    /// Finishes a composition flight whose exact-text undo validation failed
    /// before any document mutation. Unlike `fail_composition_write`, this
    /// preserves the known canonical projection and merely returns the local
    /// handle to that projection.
    fn cancel_composition_write(
        &self,
        flight: CompositionFlight,
        handle: Option<ITfComposition>,
    ) -> Result<bool> {
        let mut state = self
            .composition
            .try_borrow_mut()
            .map_err(|_| reentrancy())?;
        if !state.write_owner.finish(flight) {
            return Ok(false);
        }
        state.handle = merge_canonical_handle(state.handle.take(), handle);
        if state
            .expected_self_termination
            .is_some_and(|expected| expected.flight == flight)
        {
            state.expected_self_termination = None;
        }
        Ok(true)
    }

    /// Turns one engine answer into a pure document plan.  It reads the
    /// journal's tail projection rather than mutating `CompositionState`, so a
    /// later key can be planned correctly while an earlier async callback waits.
    ///
    /// At most two: a commit ends the composition, and a preedit that
    /// survives the commit — the tail the engine is still working on —
    /// opens a new one. The order is fixed and matters, because the second
    /// operation's composition starts where the first one's text ended.
    fn plan(&self, output: &Output) -> Result<WritePlan> {
        let before = self
            .writes
            .try_borrow()
            .map_err(|_| reentrancy())?
            .tail_visible();
        plan_from_visible(before, output)
    }
}

fn is_idle_space_commit(output: &Output) -> bool {
    let preedit_empty = output
        .preedit
        .as_ref()
        .map_or(true, |preedit| visible_text(preedit).is_empty());
    preedit_empty && matches!(output.commit.as_deref(), Some("\u{3000}" | " "))
}

/// Computes a document plan without owning a composition handle or changing a
/// visible-state projection. Keeping this independent from [`TextService`]
/// makes the planning contract directly testable and lets the write journal
/// retain its speculative tail until TSF has actually applied it.
fn plan_from_visible(before: VisibleState, output: &Output) -> Result<WritePlan> {
    let mut after = before.clone();
    let mut updates = Vec::new();

    if !output.delete_before.is_empty() {
        if output.commit.is_some() || !before.text.is_empty() {
            return Err(Error::new(
                E_UNEXPECTED,
                "invalid commit-undo output while text is still composed",
            ));
        }
        updates.push(Update::DeleteBefore(output.delete_before.clone()));
    }

    if let Some(text) = output.commit.as_ref().filter(|text| !text.is_empty()) {
        if is_idle_space_commit(output) && before.has_composition && !before.text.is_empty() {
            // Engine idle-Space against a still-visible reading would replace
            // the whole preedit with U+3000. Keep the reading so Space can
            // convert instead of inserting a document space.
            return Ok(WritePlan {
                updates: Vec::new(),
                before: before.clone(),
                after: before,
            });
        }
        updates.push(Update::Commit(text.clone()));
        after.text.clear();
        after.has_composition = false;
    }

    let preedit_text = output
        .preedit
        .as_ref()
        .map(visible_text)
        .unwrap_or_default();
    if !preedit_text.is_empty() {
        // The segments -- and the per-segment `UnderlineKind` the engine
        // already computed in `render_converted_segments` -- travel to the
        // document layer as-is; `visible_text` above is only the flattened
        // projection this function keeps for staleness/undo comparisons.
        let segments = output
            .preedit
            .as_ref()
            .map(|preedit| preedit.segments.clone())
            .unwrap_or_default();
        updates.push(Update::Show(segments));
        after.text = preedit_text;
        after.has_composition = true;
    } else if !after.text.is_empty() {
        // The engine has nothing to show and committed nothing: the
        // user cancelled. Anything still on screen has to come off it.
        updates.push(Update::Discard);
        after.text.clear();
        after.has_composition = false;
    }

    Ok(WritePlan {
        updates,
        before,
        after,
    })
}

impl Drop for TextService {
    fn drop(&mut self) {
        let _ = self.destroy_deferred_window();
        on_object_destroyed();
    }
}

unsafe extern "system" fn deferred_window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == DEFERRED_WORK_MESSAGE {
        // SAFETY: the window owns this value and `DestroyWindow` clears it
        // before the window can be reused.
        let owner = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const TextService_Impl;
        if !owner.is_null() {
            // SAFETY: the owner pointer is installed when this window is
            // created and remains valid until the user data is cleared.
            unsafe {
                (*owner).dispatch_deferred();
            }
        }
        return LRESULT(0);
    }
    if message == WM_TIMER && wparam.0 == AI_TEXT_TIMER_ID {
        // SAFETY: ownership is identical to the deferred-work message above.
        let owner = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const TextService_Impl;
        if !owner.is_null() {
            // SAFETY: the pointer remains installed until the timer is killed
            // and user data is cleared during window destruction.
            unsafe {
                (*owner).dispatch_ai_text_poll();
            }
        }
        return LRESULT(0);
    }
    // SAFETY: forwarding the untouched window message to the default procedure
    // is the standard Win32 window-procedure contract.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// A borrow that fails means TSF re-entered us mid-update. Reporting it beats
/// the alternative: `RefCell`'s own panic would abort the host application.
fn reentrancy() -> Error {
    Error::new(E_UNEXPECTED, "re-entrant access to text service state")
}

/// Keeps COM identity at the text-service boundary; the write coordinator is
/// intentionally COM-free.
fn context_id(context: &ITfContext) -> ContextId {
    ContextId(context.as_raw() as usize)
}

fn diagnostic_cancel_code(reason: CancelReason) -> i32 {
    match reason {
        CancelReason::ActivationChanged => 1,
        CancelReason::Deactivated => 2,
        CancelReason::FocusChanged => 3,
        CancelReason::CompositionTerminated => 4,
        CancelReason::ContextReplaced => 5,
        CancelReason::RevisionMismatch => 6,
        CancelReason::StaleCallback => 7,
        CancelReason::EngineUnavailable => 8,
        CancelReason::DeferredUnavailable => 9,
        CancelReason::PredecessorFailed => 10,
        CancelReason::RequestRejected => 11,
    }
}

fn composition_identity(composition: &ITfComposition) -> CompositionIdentity {
    CompositionIdentity(composition.as_raw() as usize)
}

/// Whether a normalized key has no engine-visible action.
///
/// The key translator represents ordinary modifier-only messages this way.
/// Declining them before any TSF admission or engine work prevents a bare
/// modifier from creating a speculative write or candidate UI transition.
fn is_unactionable_key_input(key: KeyInput) -> bool {
    key.code == KeyCode::Unknown && key.ch.is_none()
}

fn expected_self_termination_matches(
    owner: &CompositionWriteOwner,
    expected: Option<ExpectedSelfTermination>,
    canonical: Option<CompositionIdentity>,
    terminated: CompositionIdentity,
) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    owner.owns(expected.flight)
        && canonical == Some(expected.composition)
        && expected.composition == terminated
}

/// A failed update can consume the callback-local clone with `take()` before
/// the canonical state knows whether the host actually ended it. Preserve a
/// canonical handle when the local copy has become `None`; a returned/new
/// `Some` remains the more specific handle to retain.
fn merge_canonical_handle<T>(canonical: Option<T>, local: Option<T>) -> Option<T> {
    local.or(canonical)
}

/// A payload exists only after an engine answer has been attached to the
/// journal. Cancelling that work at a focus boundary can leave the engine ahead
/// of the document even when the visible composition state is still known.
fn cancelled_outputs_require_focus_reconciliation<T>(completions: &[Completion<T>]) -> bool {
    completions
        .iter()
        .any(|completion| completion.payload.is_some())
}

fn classify_input_scope_with_cookie(context: &ITfContext, ec: u32) -> Result<InputScope> {
    let range = match composition::current_selection_range(context, ec, &mut || Ok(())) {
        Ok(range) => range,
        Err(_) => unsafe { context.GetStart(ec)? },
    };
    // SAFETY: `context` and `range` belong to the active edit session that
    // issued `ec`.
    let property = match unsafe { context.GetProperty(&GUID_PROP_INPUTSCOPE) } {
        Ok(property) => property,
        // WinForms and classic EDIT often refuse the property object with
        // E_FAIL instead of handing back VT_EMPTY. That is "no restriction",
        // not a classification failure.
        Err(_) => return Ok(InputScope::Normal),
    };
    // SAFETY: the property and range belong to this context and `ec` is the
    // active edit cookie.
    let value = match unsafe { property.GetValue(ec, &range) } {
        Ok(value) => value,
        Err(_) => return Ok(InputScope::Normal),
    };
    classify_input_scope_variant(value)
}

fn classify_input_scope_variant(mut value: VARIANT) -> Result<InputScope> {
    let result = (|| {
        // An ordinary text field carries no GUID_PROP_INPUTSCOPE value, and
        // TSF reports that as a successful read of an empty VARIANT. That is
        // a positive observation -- the host declared no restriction -- not a
        // failure to classify, so it must not be confused with the error
        // paths below. Conflating the two rejected every real keystroke
        // Notepad, VS Code, and every other plain text host ever produced.
        // SAFETY: `value` is an initialized owning VARIANT returned by TSF;
        // reading its discriminant is valid before VariantClear below.
        let vt = unsafe { value.Anonymous.Anonymous.vt };
        if vt.0 == VT_EMPTY.0 || vt.0 == VT_NULL.0 {
            return Ok(InputScope::Normal);
        }
        // Any other non-VT_UNKNOWN shape is a property this code does not
        // understand. Reject it instead of guessing from a VARIANT integer
        // that could belong to another property provider.
        if (vt.0 & VT_TYPEMASK.0) != VT_UNKNOWN.0 {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        // SAFETY: VT_UNKNOWN selects the `punkVal` union member. Cloning the
        // contained Option retains the interface independently until the
        // owning VARIANT is cleared after this closure.
        let unknown = unsafe { (*value.Anonymous.Anonymous.Anonymous.punkVal).clone() }
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        let input_scope: ITfInputScope = unknown.cast()?;

        let mut scopes = core::ptr::null_mut::<TfInputScope>();
        let mut count = 0u32;
        // SAFETY: both output pointers are valid for the call, and the
        // interface owns the returned CoTaskMemAlloc array until we free it
        // below.
        let scope_result = unsafe { input_scope.GetInputScopes(&mut scopes, &mut count) };
        let classified = if count > 4096 {
            // A count this large is malformed rather than descriptive.
            InputScope::Unclassified
        } else if scopes.is_null() || count == 0 {
            classify_declared_scopes(None)
        } else {
            // SAFETY: TSF returned `count` contiguous InputScope values and
            // the allocation remains alive until the matching free below.
            let values = unsafe { core::slice::from_raw_parts(scopes, count as usize) };
            classify_declared_scopes(Some(values))
        };
        if !scopes.is_null() {
            // SAFETY: GetInputScopes documents CoTaskMemAlloc ownership for
            // its array; freeing it exactly once avoids a per-key leak.
            unsafe { CoTaskMemFree(Some(scopes.cast())) };
        }
        scope_result.map(|()| classified)
    })();

    // GetValue returns an owning VARIANT. Always release a possible COM
    // interface, including when shape validation or scope conversion fails.
    // SAFETY: `value` is still initialized and has not previously been
    // cleared; VariantClear releases its active union member exactly once.
    let clear_result = unsafe { VariantClear(&mut value) };
    match result {
        Ok(scope) => clear_result.map(|()| scope),
        Err(error) => {
            let _ = clear_result;
            Err(error)
        }
    }
}

/// Maps what the host actually declared to a persistence class.
///
/// `None` means the provider answered successfully and named no scope at all.
/// Like an absent property value, that is the host stating it puts no
/// restriction on the field, so it classifies as ordinary text. It is
/// deliberately distinct from a read that failed, a VARIANT of an unexpected
/// shape, or a scope value this build does not recognise -- those remain
/// `Unclassified` and therefore fail-closed for content persistence.
fn classify_declared_scopes(scopes: Option<&[TfInputScope]>) -> InputScope {
    match scopes {
        None => InputScope::Normal,
        Some(values) => classify_tf_input_scopes(values),
    }
}

fn classify_tf_input_scopes(scopes: &[TfInputScope]) -> InputScope {
    let mut best = InputScope::Unclassified;
    for scope in scopes {
        let Some(mapped) = map_tf_input_scope(*scope) else {
            // A future Windows scope must not be downgraded to ordinary text
            // merely because the same property also contains a known scope.
            // The caller can safely decline this key until the mapping is
            // updated and reviewed.
            return InputScope::Unclassified;
        };
        if input_scope_priority(mapped) > input_scope_priority(best) {
            best = mapped;
        }
    }
    best
}

fn map_tf_input_scope(scope: TfInputScope) -> Option<InputScope> {
    Some(match scope.0 {
        // URL and e-mail scopes must not be persisted even though they are
        // not password boxes: identifiers and tokens routinely appear there.
        1 => InputScope::Url,
        4 | 5 | 60 => InputScope::Email,
        // Login names, private fields, passwords, numeric PINs, and
        // alphanumeric PINs are all credential-bearing in practice.
        6 | 31 | 61 | 63..=66 => InputScope::Password,
        // Number, telephone, date/time, currency, and formula-number fields
        // are treated as digit-sensitive for the developer replay store.
        20..=21 | 28..=29 | 32..=39 | 67 => InputScope::Digits,
        // Known non-sensitive text scopes. The explicit list keeps a future
        // Windows value fail-closed rather than silently becoming normal.
        0 | 2..=3 | 7..=19 | 22..=27 | 30 | 40..=59 | 62 | 68 | -5..=-1 => InputScope::Normal,
        _ => return None,
    })
}

const fn input_scope_priority(scope: InputScope) -> u8 {
    match scope {
        InputScope::Unclassified => 0,
        InputScope::Normal => 1,
        InputScope::Digits => 2,
        InputScope::Email => 3,
        InputScope::Url => 4,
        InputScope::Password => 5,
    }
}

/// The preedit as one string, discarding each segment's `UnderlineKind`.
///
/// This flattened form is what `VisibleState` compares for staleness and
/// commit-undo purposes -- those checks only ever cared about *which*
/// characters are on screen. The per-segment underline the engine computes
/// still reaches the document: `plan_from_visible` sends `Preedit::segments`
/// itself (not this string) to `Update::Show`, and `composition::write_text`
/// gives each one its own display-attribute range.
fn visible_text(preedit: &Preedit) -> String {
    let mut text = String::new();
    for segment in &preedit.segments {
        text.push_str(&segment.text);
    }
    text
}

fn ai_terminal_result(status: AiTextStatus, error_code: &str) -> AiTextResult {
    AiTextResult {
        status,
        result: String::new(),
        model: "gpt-5.6-luna".to_owned(),
        provider: String::new(),
        style: String::new(),
        error_code: error_code.to_owned(),
        latency_ms: 0,
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        attempts: 0,
    }
}

fn ai_error_message(status: AiTextStatus, error_code: &str) -> String {
    let reason = match status {
        AiTextStatus::Timeout => "AIリクエストがタイムアウトしました",
        AiTextStatus::MissingKey => "APIキーが設定されていません",
        AiTextStatus::ApiError => "APIがリクエストを拒否しました",
        AiTextStatus::WorkerError => "AIワーカーを実行できませんでした",
        AiTextStatus::Cancelled => "AIリクエストをキャンセルしました",
        AiTextStatus::Rejected => "AI結果を適用できませんでした",
        AiTextStatus::Applied => "AIから空の結果が返されました",
    };
    let safe_code: String = error_code
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        .take(64)
        .collect();
    if safe_code.is_empty() {
        reason.to_owned()
    } else {
        format!("{reason} ({safe_code})")
    }
}

fn read_range_text(range: &ITfRange, ec: u32) -> Result<String> {
    // One extra UTF-16 unit makes truncation observable without an unbounded
    // allocation: anything that fills it cannot fit the engine's UTF-8 bound.
    let mut wide = vec![0u16; MAX_PREEDIT_BYTES + 1];
    let mut copied = 0u32;
    // SAFETY: `range` belongs to the context that granted `ec`; `wide` and the
    // count pointer remain live and writable for the call.
    unsafe { range.GetText(ec, 0, &mut wide, &mut copied)? };
    let copied = usize::try_from(copied).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
    if copied == 0 || copied > MAX_PREEDIT_BYTES || copied > wide.len() {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    wide.truncate(copied);
    let text = String::from_utf16(&wide).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
    if text.len() > MAX_PREEDIT_BYTES {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    Ok(text)
}

impl TextService_Impl {
    fn validate_pending_write(&self, ticket: Ticket) -> core::result::Result<(), CancelReason> {
        self.get_impl()
            .writes
            .try_borrow()
            .map_err(|_| CancelReason::StaleCallback)?
            .validate_callback(ticket)
    }

    fn dispatch_deferred(&self) {
        let service = self.get_impl();
        // A focus-gain callback may have been unable to borrow DeferredState
        // while a pre-engine focus-loss message was still queued. Promote that
        // explicit fail-closed handoff *before* extracting any work, so the old
        // focus-loss bit can never reach `Engine::commit` after focus returned.
        // If the state is still re-entrantly borrowed, leave the handoff cell
        // set and keep input blocked; lifecycle teardown remains its terminal
        // owner rather than letting this message dispatch stale work.
        if service.focus_gain_reconciliation_pending.get()
            && service.promote_pending_focus_gain_reconciliation().is_err()
        {
            return;
        }
        let work = {
            let Ok(mut state) = service.deferred.try_borrow_mut() else {
                return;
            };
            if service.candidate_end_pending.replace(false) {
                state.dispatch.work.retain_candidate_end();
            }
            // `show_or_update` may pump this hidden-window message. In that
            // nested dispatch, leave *all* deferred work in its owner state.
            // The outer candidate operation restores its controller, clears
            // the operation bit, and posts the retained work afterwards.
            let Some(work) = state
                .dispatch
                .take_for_dispatch(service.candidate_operation_active.get())
            else {
                return;
            };
            work
        };
        let DeferredWork {
            write,
            focus_loss,
            focus_reconcile,
            end_candidates,
            layout,
            layout_abandon,
            candidates,
        } = work;

        if end_candidates {
            service.end_candidates();
        }
        if focus_reconcile {
            service.dispatch_focus_reconciliation();
        }
        if focus_loss {
            self.dispatch_focus_loss_finalization();
        }

        if let Some(pending) = candidates {
            // A stale deferred candidate payload must not tear down a newer
            // output's UI. Only an error from the still-current owner ends the
            // candidate contract.
            if service.ui_lease_is_current(&pending.context, pending.lease)
                && self
                    .show_candidates(&pending.context, &pending.candidates, pending.lease)
                    .is_err()
                && service.ui_lease_is_current(&pending.context, pending.lease)
            {
                service.end_candidates();
            }
        }

        if write {
            let _ = self.request_next_write();
        }

        let layout_terminal_observed = layout_abandon
            && match service.settle_pending_layout_abandon() {
                Ok(Some(terminalized)) => terminalized,
                Ok(None) => false,
                Err(_) => true,
            };

        if layout && !layout_terminal_observed {
            let context = service.layout.try_borrow().ok().and_then(|layout| {
                layout
                    .subscription
                    .as_ref()
                    .map(|subscription| (subscription.context.clone(), subscription.lease))
            });
            if let Some((context, lease)) = context {
                let _ = self.request_candidate_layout(&context, lease);
            }
        }
    }

    fn dispatch_ai_text_poll(&self) {
        let service = self.get_impl();
        let job = match service.ai_text.try_borrow() {
            Ok(state) => state.pending.as_ref().map(|pending| pending.job),
            Err(_) => None,
        };
        let Some(job) = job else {
            service.stop_ai_text_timer();
            return;
        };
        let poll = match service.engine.try_borrow_mut() {
            Ok(mut engine) => engine.poll_ai_text(job),
            Err(_) => return,
        };
        if poll == AiTextPoll::Pending {
            return;
        }
        let pending = match service.ai_text.try_borrow_mut() {
            Ok(mut state) if state.pending.as_ref().is_some_and(|value| value.job == job) => {
                state.pending.take()
            }
            _ => None,
        };
        let Some(pending) = pending else {
            return;
        };
        service.stop_ai_text_timer();

        let result = match poll {
            AiTextPoll::Complete(result) => result,
            AiTextPoll::Missing => ai_terminal_result(AiTextStatus::WorkerError, "job_missing"),
            AiTextPoll::Unavailable => {
                ai_terminal_result(AiTextStatus::WorkerError, "engine_unavailable")
            }
            AiTextPoll::Pending => return,
        };
        if result.status != AiTextStatus::Applied || result.result.is_empty() {
            let status = if result.status == AiTextStatus::Applied {
                AiTextStatus::WorkerError
            } else {
                result.status
            };
            let error_code = if result.result.is_empty() && result.error_code.is_empty() {
                "empty_result".to_owned()
            } else {
                result.error_code.clone()
            };
            service.set_ai_error(Some(ai_error_message(status, &error_code)));
            service.record_ai_result(
                pending.operation,
                pending.source,
                result,
                status,
                Some(&error_code),
            );
            return;
        }

        let focus_matches = service
            .focused_context()
            .ok()
            .is_some_and(|focused| context_id(&focused) == context_id(&pending.context));
        if !service.focus_foreground.get()
            || !focus_matches
            || service.read_input_scope(&pending.context).ok() != Some(InputScope::Normal)
        {
            service.record_ai_result(
                pending.operation,
                pending.source,
                result,
                AiTextStatus::Cancelled,
                Some("focus_or_scope_changed"),
            );
            return;
        }

        if self.apply_completed_ai_text(pending, result).is_err() {
            service.set_ai_error(Some("AI結果を文書へ安全に適用できませんでした".to_owned()));
        }
    }

    fn apply_completed_ai_text(&self, pending: PendingAiText, result: AiTextResult) -> Result<()> {
        let service = self.get_impl();
        match pending.target.clone() {
            AiTextTarget::Composition => {
                let visible = service.composition_projection()?;
                if !visible.has_composition || visible.text != pending.source {
                    service.record_ai_result(
                        pending.operation,
                        pending.source,
                        result,
                        AiTextStatus::Cancelled,
                        Some("composition_changed"),
                    );
                    return Ok(());
                }
                service.observe_write_context(&pending.context)?;
                let reservation = service.reserve_write(&pending.context)?;
                let output = match service
                    .engine
                    .try_borrow_mut()
                    .map_err(|_| reentrancy())?
                    .apply_ai_composition(result.result.clone())
                {
                    Answer::Ready(output) => output,
                    Answer::Busy => {
                        service.cancel_reservation(reservation, CancelReason::PredecessorFailed);
                        service.record_ai_result(
                            pending.operation,
                            pending.source,
                            result,
                            AiTextStatus::Rejected,
                            Some("composition_busy"),
                        );
                        return Ok(());
                    }
                    Answer::Rejected | Answer::Unavailable => {
                        service.cancel_reservation(reservation, CancelReason::RequestRejected);
                        service.record_ai_result(
                            pending.operation,
                            pending.source,
                            result,
                            AiTextStatus::Rejected,
                            Some("composition_apply_failed"),
                        );
                        return Ok(());
                    }
                };
                let record = PendingAiRecord {
                    operation: pending.operation,
                    source: pending.source,
                    result,
                };
                self.submit_output(
                    &pending.context,
                    reservation,
                    output,
                    OutputSubmission {
                        target_range: None,
                        synchronous_first: true,
                        start_now: true,
                        ai_record: Some(record),
                    },
                )
            }
            AiTextTarget::Selection(range) => {
                let client_id = service.client_id()?;
                let validation_range = range.clone();
                let expected = pending.source.clone();
                let unchanged =
                    edit_session::read_in_document_sync(&pending.context, client_id, move |ec| {
                        Ok(read_range_text(&validation_range, ec)? == expected)
                    })
                    .unwrap_or(false);
                if !unchanged || service.composition_projection()?.has_composition {
                    service.record_ai_result(
                        pending.operation,
                        pending.source,
                        result,
                        AiTextStatus::Cancelled,
                        Some("selection_changed"),
                    );
                    return Ok(());
                }
                service.observe_write_context(&pending.context)?;
                let reservation = service.reserve_write(&pending.context)?;
                let before = service
                    .writes
                    .try_borrow()
                    .map_err(|_| reentrancy())?
                    .tail_visible();
                if before.has_composition || !before.text.is_empty() {
                    service.cancel_reservation(reservation, CancelReason::RevisionMismatch);
                    service.record_ai_result(
                        pending.operation,
                        pending.source,
                        result,
                        AiTextStatus::Cancelled,
                        Some("composition_started"),
                    );
                    return Ok(());
                }
                let record = PendingAiRecord {
                    operation: pending.operation,
                    source: pending.source.clone(),
                    result: result.clone(),
                };
                let payload = PendingWrite {
                    context: pending.context,
                    plan: WritePlan {
                        updates: vec![Update::Commit(result.result)],
                        before,
                        after: VisibleState::empty(),
                    },
                    target_range: Some(range.clone()),
                    query_layout: false,
                    synchronous_first: true,
                    candidates: CandidateEffect::Hide,
                    undo_commit: false,
                    engine_recovery: None,
                    ai_source_validation: Some((range, pending.source)),
                    ai_record: Some(record),
                };
                self.submit_pending_write(reservation, payload, true)
            }
        }
    }

    /// Runs the one deferred focus-loss owner. Every branch either leaves the
    /// document callback as the owner (`EngineCommitStarted`) or retires the
    /// engine/document projection through the document-free terminal path.
    /// In particular, a focus regain re-entered by `Engine::commit` changes the
    /// phase before this method resumes, so it cannot enqueue an old document
    /// edit afterwards.
    fn dispatch_focus_loss_finalization(&self) {
        let service = self.get_impl();
        if !service.begin_focus_finalization() {
            // A duplicate hidden-window message, or a focus regain that
            // cancelled the request before dispatch, has already assigned the
            // terminal owner. It must not send a second engine commit.
            return;
        }

        if !service.ask_to_finalize_for_focus() {
            let _ = service.abort_engine_started_focus_finalization();
            return;
        }

        if service.focus_finalization_phase() != FocusFinalizationPhase::EngineCommitStarted {
            // `Engine::commit` re-entered a lifecycle callback. That callback
            // already retired the projection, so no older document request may
            // continue from this stack frame.
            return;
        }

        let Some(context) = service.composition_context() else {
            // The engine-side commit has happened, but there is no safe
            // document context to carry the visible text. Reset both local
            // owners rather than leave an empty engine paired with a stale
            // composition projection.
            let _ = service.abort_engine_started_focus_finalization();
            return;
        };

        if self.finalize_visible_text_async(&context).is_err()
            && service.focus_finalization_phase() == FocusFinalizationPhase::EngineCommitStarted
        {
            // A posting/projection error has no later edit-session owner. The
            // phase remains engine-started only while this branch owns the
            // required document-free reconciliation.
            let _ = service.abort_engine_started_focus_finalization();
        }
    }

    fn show_candidates(
        &self,
        context: &ITfContext,
        candidates: &EngineCandidateList,
        lease: UiLease,
    ) -> Result<bool> {
        let service = self.get_impl();
        if !service.ui_lease_is_current(context, lease) {
            return Err(Error::new(E_UNEXPECTED, "stale candidate UI operation"));
        }
        let thread_mgr = service.thread_manager()?;
        if !service.begin_candidate_operation() {
            return Err(Error::new(
                E_UNEXPECTED,
                "re-entrant candidate UI operation",
            ));
        }
        let mut controller = {
            let slot = service
                .candidate_ui
                .try_borrow_mut()
                .map_err(|_| reentrancy());
            let mut slot = match slot {
                Ok(slot) => slot,
                Err(error) => {
                    let _ = service.finish_candidate_operation();
                    return Err(error);
                }
            };
            core::mem::take(&mut *slot)
        };
        let mut authority = || {
            if service.ui_lease_is_current(context, lease) {
                Ok(())
            } else {
                Err(Error::new(E_UNEXPECTED, "candidate UI ownership changed"))
            }
        };
        let shown = controller.show_or_update(&thread_mgr, context, candidates, &mut authority);
        let restore_result = service
            .candidate_ui
            .try_borrow_mut()
            .map_err(|_| reentrancy())
            .map(|mut slot| *slot = controller);
        let finish_result = service.finish_candidate_operation();
        restore_result?;
        finish_result?;
        let shown = shown?;

        let sink: ITfTextLayoutSink = self.to_interface();
        if !service.ui_lease_is_current(context, lease) {
            return Err(Error::new(E_UNEXPECTED, "candidate UI ownership changed"));
        }
        service.ensure_layout_subscription(context, &sink, lease)?;
        Ok(shown)
    }

    /// Requests one read lock in response to a layout notification. An
    /// already queued request absorbs duplicate notifications, and every
    /// accepted request finishes as Ready/Waiting/Unavailable.
    fn request_candidate_layout(&self, context: &ITfContext, lease: UiLease) -> Result<()> {
        let service = self.get_impl();
        // A previous callback that lost its `LayoutState` borrow gets one
        // terminal attempt before this event may claim new work. Even after a
        // successful settlement, defer the next query to a later layout event.
        if service.settle_pending_layout_abandon()? == Some(true) {
            return Ok(());
        }
        if !service.ui_lease_is_current(context, lease) {
            service.terminalize_layout_query(context, lease);
            return Ok(());
        }
        // Take the immutable canonical handle before claiming `QueryQueued`.
        // A refused composition borrow therefore cannot strand the geometry
        // state, while the lease gates below still revoke this retained handle
        // before every host call.
        let handle = service
            .composition
            .try_borrow()
            .map_err(|_| reentrancy())?
            .handle
            .clone();
        if !service.begin_layout_query(context, lease)? {
            return Ok(());
        }
        let client_id = match service.client_id() {
            Ok(client_id) => client_id,
            Err(error) => {
                // `begin_layout_query` transferred ownership to this method.
                // With no edit session to run, this branch must finalize it.
                if service.ui_lease_is_current(context, lease) {
                    if service
                        .complete_layout_query(
                            context,
                            lease,
                            composition::GeometryResult::Unavailable,
                            None,
                        )
                        .is_err()
                    {
                        service.terminalize_layout_query(context, lease);
                    }
                } else {
                    service.terminalize_layout_query(context, lease);
                }
                return Err(error);
            }
        };
        let owner = self.to_object();
        let owned_context = context.clone();
        let requested = edit_session::read_in_document_async(context, client_id, move |ec| {
            if !owner.get_impl().ui_lease_is_current(&owned_context, lease) {
                owner
                    .get_impl()
                    .terminalize_layout_query(&owned_context, lease);
                return Ok(());
            }
            let mut authority = || {
                if owner.get_impl().ui_lease_is_current(&owned_context, lease) {
                    Ok(())
                } else {
                    Err(Error::new(
                        E_UNEXPECTED,
                        "candidate geometry lease is no longer current",
                    ))
                }
            };
            // Each GetRange/GetActiveView/GetTextExt call is individually
            // authority-gated. A lifecycle callback that invalidates this
            // lease inside an earlier host call prevents the later calls from
            // running at all.
            let result =
                composition::candidate_rect(&owned_context, ec, handle.as_ref(), &mut authority);
            // The editable area only matters when there is a composition to
            // place a popup against, and it is strictly supplementary: any
            // non-ready outcome leaves the renderer with the composition-only
            // placement rather than blocking the anchor it already has.
            let document = match result {
                composition::GeometryResult::Ready(_) => {
                    match composition::document_rect(&owned_context, &mut authority) {
                        composition::GeometryResult::Ready(rect) => Some(rect),
                        composition::GeometryResult::NoLayout
                        | composition::GeometryResult::Unavailable => None,
                    }
                }
                composition::GeometryResult::NoLayout
                | composition::GeometryResult::Unavailable => None,
            };
            // Any failed geometry authority gate produces a non-ready safe
            // terminal outcome. Recheck here to distinguish a stale lease,
            // which must abandon the claimed query rather than publish
            // geometry or leave QueryQueued behind.
            if !owner.get_impl().ui_lease_is_current(&owned_context, lease) {
                owner
                    .get_impl()
                    .terminalize_layout_query(&owned_context, lease);
                return Ok(());
            }
            let completed =
                owner
                    .get_impl()
                    .complete_layout_query(&owned_context, lease, result, document);
            if completed.is_err() {
                // The asynchronous caller cannot observe this delayed error.
                // Transfer it to the bounded deferred/lifecycle terminal owner
                // before returning control to TSF.
                owner
                    .get_impl()
                    .terminalize_layout_query(&owned_context, lease);
            }
            Ok(())
        });
        if requested.is_err() {
            // A refused lock has no callback that could own finalization.
            // Mark this query terminal and hide unless a future layout event
            // gives us another opportunity.
            if service.ui_lease_is_current(context, lease) {
                if service
                    .complete_layout_query(
                        context,
                        lease,
                        composition::GeometryResult::Unavailable,
                        None,
                    )
                    .is_err()
                {
                    service.terminalize_layout_query(context, lease);
                }
            } else {
                service.terminalize_layout_query(context, lease);
            }
        }
        Ok(())
    }

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
        let key = key_handler::translate((wparam.0 & 0xFFFF) as u16, lparam.0, test_only);
        self.handle_key_input(context, key)
    }

    /// Applies one already-normalized logical key to the active document.
    ///
    /// Ordinary keys are translated from a Win32 message by [`Self::handle_key`],
    /// while TSF passes preserved IME keys only by their registration GUID.
    fn handle_key_input(&self, context: Ref<'_, ITfContext>, key: KeyInput) -> Result<BOOL> {
        // A key with no context has nowhere to go. Declining it leaves the host
        // to handle it, which is the only correct answer.
        let Ok(context) = context.ok() else {
            return Ok(false.into());
        };

        if is_unactionable_key_input(key) {
            return Ok(false.into());
        }

        let service = self.get_impl();
        if service.ai_trigger_matches(key) {
            let already_owned = service.ai_key_latched.get()
                || service
                    .ai_text
                    .try_borrow()
                    .map(|state| state.pending.is_some())
                    .unwrap_or(true);
            if already_owned {
                return Ok(true.into());
            }
            if key.test_only {
                if service.focus_foreground.get()
                    && service.read_input_scope(context)? == InputScope::Normal
                    && service
                        .capture_ai_target(context, AiTextOperation::Transform)?
                        .is_some()
                {
                    return Ok(true.into());
                }
            } else if service.start_ai_text_request(context, AiTextOperation::Transform)? {
                service.ai_key_latched.set(true);
                return Ok(true.into());
            }
        }
        let current_context = context_id(context);
        let keep_convert = keep_live_composition_for_convert(key, service.has_live_composition());
        let composition_context = if keep_convert {
            service.composition_context()
        } else {
            None
        };
        if key.test_only {
            // Probe is deliberately decided before every live settlement,
            // journal, context-admission, and scope-publication path.  A
            // pending handoff is a local Busy answer; it must not be advanced
            // merely because TSF asked whether the key would be consumed.
            let fence = match service.probe_fence(current_context) {
                Ok(fence) => fence,
                // A borrowed fence cannot be proven open.  Fail closed as the
                // local Busy equivalent and leave the real owner untouched.
                Err(_) => return Ok(true.into()),
            };
            return match probe_action(fence) {
                ProbeAction::Busy => Ok(true.into()),
                ProbeAction::Declined => Ok(false.into()),
                ProbeAction::Ask { mut fresh_context } => {
                    // A convert key against a live reading must probe the
                    // composing session, not a fresh idle clone. Otherwise
                    // OnTestKeyDown looks like idle Space while OnKeyDown
                    // converts — or worse, the real path used to replace
                    // the context and insert U+3000.
                    if keep_convert {
                        fresh_context = false;
                    }
                    // Read the current host classification, but carry it in a
                    // throwaway Probe request. Publishing it here would mutate
                    // the live engine session before OnKeyDown had arrived. A
                    // replacement additionally asks the engine for a fresh
                    // session clone, matching the link that the real callback
                    // will create after retiring the old context.
                    let scope = service.read_input_scope(context)?;
                    match service.ask_probe(scope, key, fresh_context)? {
                        Answer::Ready(output) => Ok(output.consumed.into()),
                        Answer::Busy => Ok(true.into()),
                        // Never left this process; nothing was reserved for
                        // a read-only probe, so there is nothing to release.
                        Answer::Rejected => Ok(false.into()),
                        Answer::Unavailable => Ok(false.into()),
                    }
                }
            };
        }
        // Snapshot the fences in priority order without probing lower-priority
        // live state after a higher-priority terminal owner is found. This
        // keeps the exact-undo handoff authoritative and gives the real path
        // the same order as the read-only Probe decision.
        let deferred_terminalization = service.undo_terminalization.get().is_some();
        let undo_write_pending = if deferred_terminalization {
            true
        } else {
            service.undo_write_pending().unwrap_or(true)
        };
        let input_blocked = if deferred_terminalization || undo_write_pending {
            true
        } else {
            service.input_blocked()
        };
        let journal_replacement = if deferred_terminalization || undo_write_pending || input_blocked
        {
            false
        } else {
            service.write_context_is_replacement(current_context)?
        };
        let context_replacement =
            journal_replacement_applies(key, service.has_live_composition(), journal_replacement);

        match decide_real_fence(
            deferred_terminalization,
            undo_write_pending,
            service.engine_recovery_pending(),
            input_blocked,
            context_replacement,
        ) {
            RealFenceAction::DeferredTerminalization => {
                // A re-entrant journal owner still owns the exact-undo
                // boundary. Even when settlement clears the marker, this
                // physical key belongs to that terminal handoff and must not
                // be applied on the same stack.
                let _ = service.deferred_undo_consumes_real_key();
                return Ok(true.into());
            }
            RealFenceAction::Consume => {
                // Do not ask the engine, reserve a second write, or let the
                // host handle the key. The existing undo callback owns this
                // document boundary until its explicit outcome arrives.
                return Ok(true.into());
            }
            RealFenceAction::Decline => {
                // A blocked/lifecycle path has an explicit host-owned
                // terminal result and cannot fall through to Apply.
                return Ok(false.into());
            }
            RealFenceAction::ReplaceAndApply => {
                // The Probe path asked against a fresh throwaway context. The
                // real callback now owns the old-context cancellation/disconnect,
                // then continues through scope publication/reservation so this
                // physical key is applied to the fresh engine session in this
                // same callback.
                if service.observe_write_context(context).is_err() {
                    service.disconnect();
                    // Cleanup could not establish a fresh context authority.
                    // Do not hand the key to the host around an unknown old
                    // write; this consumed return is the explicit terminal.
                    return Ok(true.into());
                }
            }
            RealFenceAction::Apply => {}
        }
        let write_context = composition_context.as_ref().unwrap_or(context);
        if keep_convert && composition_context.is_none() {
            // A live reading exists, but this callback does not own its
            // ITfContext. Absorb Space rather than inserting a document space
            // into the idle peer context.
            return Ok(true.into());
        }
        if !service.can_admit_write_for_context(write_context)? {
            return Ok(false.into());
        }
        if !service.publish_input_scope(write_context)? {
            // Scope publication is part of the privacy boundary for a real
            // key. A refused publication leaves the key to the host.
            return Ok(false.into());
        }

        if !keep_convert || composition_context.is_some() {
            service.observe_write_context(write_context)?;
        }
        // Context replacement may have cancelled a full old-context queue.
        if !service.can_admit_write_for_context(write_context)? {
            return Ok(false.into());
        }
        let reservation = match service.reserve_write(write_context) {
            Ok(reservation) => reservation,
            // This is an admission failure, not an engine failure: leave the
            // key to the host and never advance the real engine.
            Err(_) => return Ok(false.into()),
        };

        let answer = match service.ask(key) {
            Ok(answer) => answer,
            Err(_) => {
                let disposition = self
                    .recover_from_engine_unavailable(write_context)
                    .unwrap_or(RecoveryKeyDisposition::Consume);
                return Ok(matches!(disposition, RecoveryKeyDisposition::Consume).into());
            }
        };
        let output = match answer {
            Answer::Ready(output) => output,
            Answer::Busy => {
                // The engine-side guard won a race with this reservation.
                // Cancel only this empty reservation; the older undo payload
                // remains queued and authoritative.
                service.cancel_reservation(reservation, CancelReason::PredecessorFailed);
                return Ok(true.into());
            }
            // This request never reached the engine -- it failed to encode
            // on this side of the wire. Only this reservation is at fault;
            // the link and everything else queued on it are untouched, so
            // there is nothing to recover.
            Answer::Rejected => {
                service.cancel_reservation(reservation, CancelReason::RequestRejected);
                return Ok(false.into());
            }
            // The key belongs to the application.  Release the pre-reserved
            // capacity before attempting the document-only rescue path.  A
            // previous async output may already own the head, so this has to
            // terminalize the full journal rather than only this reservation.
            Answer::Unavailable => {
                let disposition = self
                    .recover_from_engine_unavailable(write_context)
                    .unwrap_or(RecoveryKeyDisposition::Consume);
                return Ok(matches!(disposition, RecoveryKeyDisposition::Consume).into());
            }
        };

        let consumed = output.consumed;
        if self
            .submit_output(
                write_context,
                reservation,
                output,
                OutputSubmission {
                    target_range: None,
                    synchronous_first: true,
                    start_now: true,
                    ai_record: None,
                },
            )
            .is_err()
        {
            // The engine has already advanced, so giving a consumed key back to
            // the host could type it twice.  Terminalize the reservation and
            // reset our engine-side session instead.
            service.cancel_reservation(reservation, CancelReason::PredecessorFailed);
            service.disconnect();
            let _ = service.queue_end_candidates();
        }
        Ok(consumed.into())
    }

    /// Commits what the document is showing, as ordinary text.
    ///
    /// The rescue path. Called when the engine has stopped answering and
    /// when focus is leaving, both of which are moments where a live
    /// composition would otherwise be stranded — underlined text attached
    /// to a conversation that will never produce a result for it.
    fn finalize_visible_text_async(&self, context: &ITfContext) -> Result<()> {
        self.enqueue_finalization(context, false, false)
    }

    /// The engine can fail after an earlier async output was accepted.  One
    /// owner must terminalize that entire speculative tail before a rescue
    /// commit is planned from the actually committed composition state.
    fn recover_from_engine_unavailable(
        &self,
        context: &ITfContext,
    ) -> Result<RecoveryKeyDisposition> {
        let service = self.get_impl();
        // An already-running callback is part of the speculative tail too. Its
        // document effect is not yet committed in CompositionState, so recovery
        // abandons that projection; when the callback returns it cannot alter
        // the recovery state or restore its local handle.
        service.invalidate_inflight_composition_write_as_unknown();
        service.cancel_all_writes(CancelReason::EngineUnavailable, true);
        // `cancel_all_writes` only resets when it terminalized an entry. The
        // engine failure itself is still authoritative when the queue was
        // empty, so reset it unconditionally.
        service.disconnect();

        let visible = match service.composition_projection() {
            Ok(visible) => visible,
            Err(_) => {
                service.abandon_composition_projection(CancelReason::RevisionMismatch)?;
                return Ok(RecoveryKeyDisposition::Host);
            }
        };
        if visible.text.is_empty() && !visible.has_composition {
            service.finish_focus_finalization();
            return Ok(RecoveryKeyDisposition::Host);
        }

        let recovery = service.begin_engine_recovery();
        let token = recovery.token();
        if recovery.is_deduplicated() {
            return Ok(RecoveryKeyDisposition::Consume);
        }

        if let Err(finalize_error) =
            self.enqueue_finalization_for_visible(context, true, true, visible, Some(token))
        {
            service.finish_engine_recovery(token, RecoveryTerminal::Rejected);
            // A projection mismatch is not an ignorable finalization failure:
            // retire both owners before the next key can be admitted.
            service
                .abandon_composition_projection(CancelReason::RevisionMismatch)
                .map_err(|_| finalize_error)?;
            return Ok(RecoveryKeyDisposition::Host);
        }

        // A synchronous callback terminalized the token before
        // RequestEditSession returned, so the host can receive this physical
        // key in order. A queued callback still owns the old composition and
        // must fence this key (and following keys) until terminalization.
        Ok(service.engine_recovery_disposition(token))
    }

    fn enqueue_finalization(
        &self,
        context: &ITfContext,
        synchronous_first: bool,
        start_now: bool,
    ) -> Result<()> {
        let service = self.get_impl();
        let visible = match service.composition_projection() {
            Ok(visible) => visible,
            Err(_) => {
                // A failed COM mutation made the visible text unknowable.  Do
                // not turn that uncertainty into an empty Commit; releasing the
                // handle is document-free and lets the host own the remainder.
                service.abandon_composition_projection(CancelReason::RevisionMismatch)?;
                return Ok(());
            }
        };
        self.enqueue_finalization_for_visible(context, synchronous_first, start_now, visible, None)
    }

    fn enqueue_finalization_for_visible(
        &self,
        context: &ITfContext,
        synchronous_first: bool,
        start_now: bool,
        visible: VisibleState,
        engine_recovery: Option<RecoveryToken>,
    ) -> Result<()> {
        let service = self.get_impl();
        if visible.text.is_empty() && !visible.has_composition {
            if let Some(token) = engine_recovery {
                service.finish_engine_recovery(token, RecoveryTerminal::Cancelled);
            }
            service.finish_focus_finalization();
            return Ok(());
        }
        service.observe_write_context(context)?;
        let reservation = service.reserve_write(context)?;
        let before = service
            .writes
            .try_borrow()
            .map_err(|_| reentrancy())?
            .tail_visible();
        // A focus-loss cancellation removes queued speculative outputs first,
        // so this projection must agree with the visible committed composition.
        if before != visible {
            service.abandon_composition_projection(CancelReason::RevisionMismatch)?;
            return Err(Error::new(
                E_UNEXPECTED,
                "finalization projection does not match the write journal",
            ));
        }
        let payload = PendingWrite {
            context: context.clone(),
            plan: WritePlan {
                updates: vec![Update::Commit(visible.text.clone())],
                before,
                after: VisibleState::empty(),
            },
            target_range: None,
            query_layout: false,
            synchronous_first,
            candidates: CandidateEffect::Hide,
            undo_commit: false,
            engine_recovery,
            ai_source_validation: None,
            ai_record: None,
        };
        self.submit_pending_write(reservation, payload, start_now)
    }

    /// Attaches an engine answer to a slot reserved before the engine was
    /// advanced.  Candidate effects remain in the payload until the document
    /// operation reaches `Applied`.
    fn submit_output(
        &self,
        context: &ITfContext,
        reservation: Reservation,
        output: Output,
        submission: OutputSubmission,
    ) -> Result<()> {
        let service = self.get_impl();
        let OutputSubmission {
            target_range,
            synchronous_first,
            start_now,
            ai_record,
        } = submission;
        let undo_commit = !output.delete_before.is_empty();
        let plan = match service.plan(&output) {
            Ok(plan) => plan,
            Err(error) => {
                if undo_commit {
                    // No journal entry was attached, so this branch is the
                    // only remaining owner of the engine-side undo. The helper
                    // retires an unconfirmed transport before returning.
                    let _ = service.settle_undo_commit_or_disconnect(UndoCommitOutcome::Rejected);
                }
                return Err(error);
            }
        };
        let candidates = match output.candidates {
            Some(candidates) => CandidateEffect::Show(candidates),
            None => CandidateEffect::Hide,
        };
        let payload = PendingWrite {
            context: context.clone(),
            plan,
            target_range,
            query_layout: false,
            synchronous_first,
            candidates,
            undo_commit,
            engine_recovery: None,
            ai_source_validation: None,
            ai_record,
        };
        self.submit_pending_write(reservation, payload, start_now)
    }

    fn submit_pending_write(
        &self,
        reservation: Reservation,
        payload: PendingWrite,
        start_now: bool,
    ) -> Result<()> {
        let service = self.get_impl();
        let writes_document = !payload.plan.updates.is_empty() || payload.target_range.is_some();
        let mut writes = match service.writes.try_borrow_mut() {
            Ok(writes) => writes,
            Err(_) => {
                if payload.undo_commit {
                    service.defer_undo_terminalization(Some(UndoCommitOutcome::Unknown));
                }
                return Err(reentrancy());
            }
        };
        let attached = writes.attach(
            reservation,
            payload.clone(),
            writes_document,
            payload.plan.before.clone(),
            payload.plan.after.clone(),
        );
        drop(writes);
        if attached.is_err() {
            if payload.undo_commit {
                // The failed attach leaves no journal completion to retry the
                // acknowledgement; the helper has retired any unconfirmed
                // transport before this branch returns.
                let _ = service.settle_undo_commit_or_disconnect(UndoCommitOutcome::Rejected);
            }
            service.cancel_reservation(reservation, CancelReason::StaleCallback);
            return Err(Error::new(E_UNEXPECTED, "write journal attachment failed"));
        }

        if start_now {
            self.request_next_write()
        } else {
            match service.queue_write() {
                Ok(()) => Ok(()),
                Err(error) => {
                    service.cancel_all_writes(CancelReason::DeferredUnavailable, true);
                    Err(error)
                }
            }
        }
    }

    /// Starts exactly the journal head.  The caller is either the originating
    /// key event (where a sync request is valuable) or deferred work after a
    /// prior callback has returned.
    fn request_next_write(&self) -> Result<()> {
        let service = self.get_impl();
        if !service.try_settle_deferred_undo_terminalization() {
            return Ok(());
        }
        let request = match service.writes.try_borrow_mut() {
            Ok(mut writes) => writes.begin_head(),
            Err(_) => {
                service.cancel_all_writes(CancelReason::StaleCallback, true);
                return Ok(());
            }
        };
        let Some(request) = request else {
            return Ok(());
        };
        if !request.writes_document {
            return self.complete_no_document_write(request);
        }

        let client_id = match service.client_id() {
            Ok(client_id) => client_id,
            Err(_) => {
                service.record_diagnostic_write(
                    request.ticket,
                    diagnostic_ring::RequestPath::None,
                    diagnostic_ring::TerminalOutcome::Failed,
                    1,
                );
                self.reject_requested_write(request.ticket, false, None);
                return Ok(());
            }
        };
        let context = request.payload.context.clone();
        let payload = request.payload.clone();
        let ticket = request.ticket;
        let synchronous_first = payload.synchronous_first;
        let owner = self.to_object();
        let requested = edit_session::write_in_document_with_mode(
            &context,
            client_id,
            payload.synchronous_first,
            move |ec| owner.apply_queued_write(ticket, payload, ec),
        );
        let path = match &requested {
            Ok(edit_session::EditRequestState::Ran) if synchronous_first => {
                diagnostic_ring::RequestPath::Sync
            }
            Ok(edit_session::EditRequestState::Ran)
            | Ok(edit_session::EditRequestState::Queued) => diagnostic_ring::RequestPath::Async,
            Err(_) => diagnostic_ring::RequestPath::Async,
        };
        match &requested {
            Ok(edit_session::EditRequestState::Ran) => service.record_diagnostic_write(
                ticket,
                path,
                diagnostic_ring::TerminalOutcome::Admitted,
                0,
            ),
            Ok(edit_session::EditRequestState::Queued) => service.record_diagnostic_write(
                ticket,
                path,
                diagnostic_ring::TerminalOutcome::Deferred,
                0,
            ),
            Err(error) => service.record_diagnostic_write(
                ticket,
                path,
                diagnostic_ring::TerminalOutcome::Failed,
                error.code().0,
            ),
        }
        if requested.is_err() {
            // If DoEditSession ran, it already terminalized the ticket.  If it
            // did not, this is the complete RequestEditSession rejection owner.
            self.reject_requested_write(ticket, false, None);
        }
        Ok(())
    }

    fn complete_no_document_write(
        &self,
        request: crate::write_coordinator::Request<PendingWrite>,
    ) -> Result<()> {
        let service = self.get_impl();
        if context_id(&request.payload.context) != request.ticket.context() {
            self.cancel_stale_write(request.ticket, CancelReason::ContextReplaced);
            return Ok(());
        }
        if let Err(reason) = self.validate_pending_write(request.ticket) {
            self.cancel_stale_write(request.ticket, reason);
            return Ok(());
        }
        let completion = match service.writes.try_borrow_mut() {
            Ok(mut writes) => writes.complete_applied(request.ticket),
            Err(_) => {
                service.cancel_all_writes(CancelReason::StaleCallback, true);
                return Ok(());
            }
        };
        if let Some(completion) = completion {
            service.record_diagnostic_write(
                request.ticket,
                diagnostic_ring::RequestPath::None,
                diagnostic_ring::TerminalOutcome::Applied,
                0,
            );
            self.settle_applied_write(completion);
        }
        Ok(())
    }

    /// The edit-session callback.  Its first operation is the journal gate;
    /// before that point it deliberately does not take a composition handle,
    /// call GetSelection, or update candidate/layout state.
    fn apply_queued_write(&self, ticket: Ticket, payload: PendingWrite, ec: u32) -> Result<()> {
        let service = self.get_impl();
        if context_id(&payload.context) != ticket.context() {
            self.cancel_stale_write(ticket, CancelReason::ContextReplaced);
            return Ok(());
        }
        if let Err(reason) = self.validate_pending_write(ticket) {
            self.cancel_stale_write(ticket, reason);
            return Ok(());
        }
        service.remember_input_scope_from_cookie(&payload.context, ec);

        if let Some((range, expected)) = payload.ai_source_validation.as_ref() {
            let actual = read_range_text(range, ec);
            if let Err(reason) = self.validate_pending_write(ticket) {
                self.cancel_stale_write(ticket, reason);
                return Ok(());
            }
            if actual.as_deref() != Ok(expected.as_str()) {
                service.set_ai_error(Some(
                    "選択範囲が変わったためAI結果を適用しませんでした".to_owned(),
                ));
                self.reject_requested_write(ticket, false, None);
                return Ok(());
            }
        }

        // Category-manager creation can itself enter COM.  Finish it before the
        // final document gate so a focus or lifecycle callback cannot make us
        // take a composition handle after its ticket was invalidated.
        let category_mgr = service.category_manager().ok();
        if let Err(reason) = self.validate_pending_write(ticket) {
            self.cancel_stale_write(ticket, reason);
            return Ok(());
        }
        let edit = DocumentEdit {
            context: payload.context.clone(),
            sink: self.to_interface(),
            category_mgr,
        };
        let Some((mut handle, flight)) =
            (match service.begin_composition_write(&payload.plan.before) {
                Ok(write) => write,
                Err(_) => {
                    self.reject_requested_write(ticket, false, None);
                    return Ok(());
                }
            })
        else {
            // The document state and journal stopped agreeing before we made a
            // COM call.  This is a fail-closed projection boundary, so reset
            // both owners rather than letting a later key inherit either tail.
            if service
                .abandon_composition_projection(CancelReason::RevisionMismatch)
                .is_err()
            {
                service.cancel_all_writes(CancelReason::RevisionMismatch, true);
                service.disconnect();
            }
            return Ok(());
        };

        // `composition` invokes several TSF methods for a single update. Each
        // one can synchronously re-enter focus/lifecycle code, so it validates
        // this same ticket before *and* after every host call. The outer
        // callback still derives the concrete cancellation reason before
        // choosing its terminal journal path.
        let mut authority = || {
            self.validate_pending_write(ticket).map_err(|_| {
                Error::new(
                    E_UNEXPECTED,
                    "queued write ownership changed during document operation",
                )
            })
        };

        if let Some(range) = payload.target_range.clone() {
            if composition::select_range(&edit.context, ec, range, &mut authority).is_err() {
                // SetSelection is a host call too. A failing HRESULT cannot
                // prove that the host left the range untouched, so preserve no
                // speculative projection and make the engine session restart.
                let reason = self
                    .validate_pending_write(ticket)
                    .err()
                    .unwrap_or(CancelReason::RevisionMismatch);
                self.fail_after_document_access(ticket, flight, handle, &payload.context, reason);
                return Ok(());
            }
            if let Err(reason) = self.validate_pending_write(ticket) {
                self.fail_after_document_access(ticket, flight, handle, &payload.context, reason);
                return Ok(());
            }
        }

        let mut failed_update = None;
        for update in &payload.plan.updates {
            if let Update::DeleteBefore(expected) = update {
                if handle.is_some() || expected.is_empty() {
                    let _ = service.cancel_composition_write(flight, handle.clone());
                    self.reject_requested_write(ticket, false, None);
                    return Ok(());
                }
                match composition::delete_before_caret(&edit, ec, expected, &mut authority) {
                    Ok(()) => {}
                    Err(composition::DeleteBeforeError::Validation(_)) => {
                        // Selection, caret, exact-text, or authority mismatch
                        // happened before SetText. The host document is
                        // unchanged, so restore the engine's post-commit
                        // terminal state through the explicit Rejected ack.
                        let _ = service.cancel_composition_write(flight, handle.clone());
                        self.reject_requested_write(ticket, false, None);
                        return Ok(());
                    }
                    Err(composition::DeleteBeforeError::Mutation(_)) => {
                        failed_update = Some(
                            self.validate_pending_write(ticket)
                                .err()
                                .unwrap_or(CancelReason::RevisionMismatch),
                        );
                        break;
                    }
                }
            } else if composition::apply_with_end_composition_callbacks(
                &edit,
                ec,
                &mut handle,
                update,
                &mut authority,
                |composition| service.expect_self_composition_termination(flight, composition),
                |_| service.clear_expected_self_composition_termination(flight),
            )
            .is_err()
            {
                failed_update = Some(
                    self.validate_pending_write(ticket)
                        .err()
                        .unwrap_or(CancelReason::RevisionMismatch),
                );
                break;
            }
            if let Err(reason) = self.validate_pending_write(ticket) {
                self.fail_after_document_access(ticket, flight, handle, &payload.context, reason);
                return Ok(());
            }
        }
        if let Some(reason) = failed_update {
            // The document update may have changed a COM range before an
            // HRESULT. Its exact final text is unknowable here, so do not
            // commit the planned state or claim the known prefix is complete.
            self.fail_after_document_access(ticket, flight, handle, &payload.context, reason);
            return Ok(());
        }

        // COM calls above may re-enter and invalidate this operation.  Once a
        // document mutation happened, fail closed rather than publishing a
        // projection after that invalidation.
        if let Err(reason) = self.validate_pending_write(ticket) {
            self.fail_after_document_access(ticket, flight, handle, &payload.context, reason);
            return Ok(());
        }

        let retained_handle = handle.clone();
        match service.commit_composition_projection(
            flight,
            &payload.plan.after,
            handle,
            &payload.context,
        ) {
            Ok(true) => {}
            Ok(false) => {
                // A lifecycle callback retired this flight while the document
                // operation was running. The host document may already have
                // accepted the edit, so this is not a pre-mutation rejection:
                // report Unknown and retire both sides of the transaction.
                self.cancel_stale_write_with_undo_outcome(
                    ticket,
                    CancelReason::StaleCallback,
                    Some(UndoCommitOutcome::Unknown),
                );
                service.disconnect();
                return Ok(());
            }
            Err(_) => {
                self.fail_after_document_access(
                    ticket,
                    flight,
                    retained_handle,
                    &payload.context,
                    CancelReason::RevisionMismatch,
                );
                return Ok(());
            }
        }
        let completion = match service.writes.try_borrow_mut() {
            Ok(mut writes) => writes.complete_applied(ticket),
            Err(_) => {
                // The document was applied but its journal terminal could not
                // be recorded. Retire the projection rather than trying to
                // reconstruct it from the callback-local handle.
                if service
                    .abandon_composition_projection(CancelReason::RevisionMismatch)
                    .is_err()
                {
                    service.cancel_all_writes_with_undo_outcome(
                        CancelReason::StaleCallback,
                        true,
                        Some(UndoCommitOutcome::Unknown),
                    );
                }
                return Ok(());
            }
        };
        let applied = if let Some(completion) = completion {
            service.record_diagnostic_write(
                ticket,
                diagnostic_ring::RequestPath::None,
                if completion.outcome == TerminalOutcome::Applied {
                    diagnostic_ring::TerminalOutcome::Applied
                } else {
                    diagnostic_ring::TerminalOutcome::Unknown
                },
                0,
            );
            self.settle_applied_write(completion);
            true
        } else {
            // If this callback still owns its composition lifecycle, the
            // document was applied but the journal has no matching terminal.
            // That is an unknowable split projection, so abandon both owners.
            // If lifecycle already revoked the flight, its state is the sole
            // authority and must not be overwritten by this late callback.
            match service.composition_flight_lifecycle_is_current(flight) {
                Ok(true) => {
                    if service
                        .abandon_composition_projection(CancelReason::RevisionMismatch)
                        .is_err()
                    {
                        service.cancel_all_writes_with_undo_outcome(
                            CancelReason::RevisionMismatch,
                            true,
                            Some(UndoCommitOutcome::Unknown),
                        );
                    }
                }
                Ok(false) => {}
                Err(_) => {
                    // Re-entrancy makes lifecycle ownership unknowable. Do not
                    // clobber CompositionState; terminalize only the journal
                    // work that can be reached without a composition borrow.
                    service.cancel_all_writes_with_undo_outcome(
                        CancelReason::RevisionMismatch,
                        true,
                        Some(UndoCommitOutcome::Unknown),
                    );
                }
            }
            service.disconnect();
            if service.queue_end_candidates().is_err() {
                service.end_candidates();
            }
            false
        };
        if applied && payload.query_layout {
            // Candidate geometry is lease-owned and therefore requested by the
            // deferred UI path only after `settle_applied_write` has adopted
            // the completion's UI lease.
            let _ = service.queue_layout();
        }
        Ok(())
    }

    /// Handles a failure after a host call might have changed selection or
    /// text. A valid flight marks the composition unknown and rejects the head;
    /// an invalid flight belongs to a lifecycle callback, which remains the
    /// authoritative owner and must not be overwritten by this callback.
    fn fail_after_document_access(
        &self,
        ticket: Ticket,
        flight: CompositionFlight,
        handle: Option<ITfComposition>,
        context: &ITfContext,
        stale_reason: CancelReason,
    ) {
        self.get_impl().reconcile_document_access_failure(
            ticket,
            flight,
            handle,
            context,
            stale_reason,
        );
    }

    fn cancel_stale_write(&self, ticket: Ticket, reason: CancelReason) {
        self.cancel_stale_write_with_undo_outcome(ticket, reason, None);
    }

    fn cancel_stale_write_with_undo_outcome(
        &self,
        ticket: Ticket,
        reason: CancelReason,
        undo_outcome: Option<UndoCommitOutcome>,
    ) {
        let service = self.get_impl();
        service.record_diagnostic_write(
            ticket,
            diagnostic_ring::RequestPath::None,
            if undo_outcome == Some(UndoCommitOutcome::Unknown) {
                diagnostic_ring::TerminalOutcome::Unknown
            } else {
                diagnostic_ring::TerminalOutcome::Cancelled
            },
            diagnostic_cancel_code(reason),
        );
        let cancelled = match service.writes.try_borrow_mut() {
            Ok(mut writes) => writes.cancel_ticket(ticket, reason),
            Err(_) => {
                service.defer_undo_terminalization(undo_outcome);
                return;
            }
        };
        service.settle_cancelled_writes(cancelled, true, undo_outcome);
    }

    fn reject_requested_write(
        &self,
        ticket: Ticket,
        document_may_have_changed: bool,
        known_prefix: Option<VisibleState>,
    ) {
        self.reject_requested_write_with_undo_outcome(
            ticket,
            document_may_have_changed,
            known_prefix,
            None,
        );
    }

    fn reject_requested_write_with_undo_outcome(
        &self,
        ticket: Ticket,
        document_may_have_changed: bool,
        known_prefix: Option<VisibleState>,
        undo_outcome: Option<UndoCommitOutcome>,
    ) {
        let service = self.get_impl();
        service.record_diagnostic_write(
            ticket,
            diagnostic_ring::RequestPath::None,
            if document_may_have_changed {
                diagnostic_ring::TerminalOutcome::Unknown
            } else {
                diagnostic_ring::TerminalOutcome::Rejected
            },
            undo_outcome
                .map(|outcome| match outcome {
                    UndoCommitOutcome::Applied => 0,
                    UndoCommitOutcome::Rejected => 1,
                    UndoCommitOutcome::Unknown => 2,
                })
                .unwrap_or(0),
        );
        let terminal = match service.writes.try_borrow_mut() {
            Ok(mut writes) => writes.reject(ticket, document_may_have_changed, known_prefix),
            Err(_) => {
                service.defer_undo_terminalization(undo_outcome);
                return;
            }
        };
        service.settle_cancelled_writes(terminal, true, undo_outcome);
    }

    fn settle_applied_write(&self, completion: Completion<PendingWrite>) {
        let service = self.get_impl();
        service.settle_engine_recovery_completion(&completion);
        if completion.outcome != TerminalOutcome::Applied {
            service.settle_cancelled_writes(
                vec![completion],
                true,
                Some(UndoCommitOutcome::Unknown),
            );
            return;
        }
        let Some(payload) = completion.payload else {
            if service.queue_write().is_err() {
                service.cancel_all_writes(CancelReason::DeferredUnavailable, true);
            }
            return;
        };
        if payload.undo_commit && !service.settle_undo_commit(UndoCommitOutcome::Applied) {
            // The host document is already in the planned state, but the
            // engine did not acknowledge consumption of its pending record.
            // Retire the composition projection and the link together rather
            // than allowing a new session to write over a possibly live host
            // composition.
            let _ = service.abandon_composition_projection(CancelReason::RevisionMismatch);
            service.disconnect();
            return;
        }
        if let Some(record) = payload.ai_record.clone() {
            service.terminalize_pending_ai_record(record, AiTextStatus::Applied, None);
            service.set_ai_error(None);
        }
        match payload.candidates {
            CandidateEffect::Show(candidates) => {
                let shown = completion.ui_lease.is_some_and(|lease| {
                    service
                        .writes
                        .try_borrow_mut()
                        .map(|mut writes| writes.adopt_ui_lease(lease))
                        .unwrap_or(false)
                        && service
                            .queue_candidates(&payload.context, &candidates, lease)
                            .is_ok()
                });
                if shown {
                    let _ = service.queue_layout();
                } else {
                    if let Ok(mut writes) = service.writes.try_borrow_mut() {
                        writes.clear_ui_lease();
                    }
                    let _ = service.queue_end_candidates();
                }
            }
            CandidateEffect::Hide => {
                if let Ok(mut writes) = service.writes.try_borrow_mut() {
                    writes.clear_ui_lease();
                }
                let _ = service.queue_end_candidates();
            }
        }
        if service.queue_write().is_err() {
            service.cancel_all_writes(CancelReason::DeferredUnavailable, true);
        }
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
        diagnostic_ring::initialize_from_environment();
        let thread_mgr = ptim.ok()?;
        let key_sink: ITfKeyEventSink = self.to_interface();
        let function_provider: ITfFunctionProvider = self.to_interface();
        let lang_bar_item: ITfLangBarItem = self.to_interface();
        let service = self.get_impl();
        service.attach(
            thread_mgr,
            tid,
            &key_sink,
            &function_provider,
            &lang_bar_item,
        )?;
        if let Err(error) = service.create_deferred_window(self as *const TextService_Impl) {
            let _ = service.detach();
            return Err(error);
        }
        if let Err(error) = service.activate_write_journal() {
            let _ = service.detach();
            return Err(error);
        }
        // After the attachment, not before: a failed activation must not
        // leave a connection behind, and a slow engine must not delay the
        // point at which keys start being delivered correctly.
        service.warm_up();
        service.record_diagnostic_lifecycle(diagnostic_ring::LifecycleEvent::Activate, 0);
        Ok(())
    }
}

impl ITfLangBarItem_Impl for TextService_Impl {
    fn GetInfo(&self, pinfo: *mut TF_LANGBARITEMINFO) -> Result<()> {
        if pinfo.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let mut info = TF_LANGBARITEMINFO {
            clsidService: CLSID_SAKURA_TSF,
            guidItem: GUID_LBI_INPUTMODE,
            // The combined styles make this a split button: the language bar
            // owns the mode menu through `InitMenu`, while `OnClick` receives
            // the click identity so a right-click can safely expose Settings.
            // Do not remove BTN_BUTTON: a menu-only item never receives
            // `OnClick`, so taskbar right-clicks would have no route here.
            dwStyle: TF_LBI_STYLE_BTN_BUTTON
                | TF_LBI_STYLE_BTN_MENU
                | TF_LBI_STYLE_HIDDENSTATUSCONTROL
                | TF_LBI_STYLE_TEXTCOLORICON,
            ulSort: 0,
            ..Default::default()
        };
        for (slot, unit) in info
            .szDescription
            .iter_mut()
            .zip("Sakura Input".encode_utf16())
        {
            *slot = unit;
        }
        // SAFETY: the caller supplied a non-null output pointer that is valid
        // for this COM call, and `info` is fully initialized.
        unsafe { pinfo.write(info) };
        Ok(())
    }

    fn GetStatus(&self) -> Result<u32> {
        Ok(self.get_impl().mode_item.status())
    }

    fn Show(&self, fshow: BOOL) -> Result<()> {
        let service = self.get_impl();
        // The TSF shell can request a status refresh, but it must not turn a
        // background service into a persistent taskbar/tray indicator.
        if fshow.as_bool() && service.focus_foreground.get() {
            service.refresh_mode_item_for_focus();
        } else {
            service.mode_item.hide();
        }
        Ok(())
    }

    fn GetTooltipString(&self) -> Result<BSTR> {
        let service = self.get_impl();
        let state = service.mode_item.snapshot();
        let mut text = match state.mode {
            Some(mode) => format!("Sakura Input — {}", mode_item::description(mode)),
            None => "Sakura Input".to_owned(),
        };
        if let Ok(error) = service.last_ai_error.try_borrow() {
            if let Some(error) = error.as_ref() {
                text.push_str(" / AIエラー: ");
                text.push_str(error);
            }
        }
        Ok(BSTR::from(text))
    }
}

impl ITfLangBarItemButton_Impl for TextService_Impl {
    fn OnClick(
        &self,
        click: TfLBIClick,
        _point: &windows::Win32::Foundation::POINT,
        _area: *const windows::Win32::Foundation::RECT,
    ) -> Result<()> {
        // Keep every non-context interaction free of engine or document work.
        // The context action is process-only and cannot retain this COM service
        // or request an edit session.
        if mode_item::is_settings_click(click) {
            mode_item::open_settings()?;
        }
        Ok(())
    }

    fn InitMenu(&self, pmenu: Ref<'_, ITfMenu>) -> Result<()> {
        let menu = pmenu.ok()?;
        let service = self.get_impl();
        let state = if service.focus_foreground.get() {
            service.mode_item.snapshot()
        } else {
            mode_item::Snapshot {
                visible: false,
                mode: None,
                can_change: false,
                can_restore: false,
            }
        };
        let last_error = service
            .last_ai_error
            .try_borrow()
            .ok()
            .and_then(|value| value.clone());
        mode_item::populate_menu(menu, state, last_error.as_deref())
    }

    fn OnMenuSelect(&self, wid: u32) -> Result<()> {
        if let Some(command) = mode_item::menu_command(wid) {
            if command == MenuCommand::OpenSettings {
                return mode_item::open_settings();
            }
            if matches!(command, MenuCommand::AiTransform | MenuCommand::AiProofread) {
                let service = self.get_impl();
                let operation = if command == MenuCommand::AiProofread {
                    AiTextOperation::Proofread
                } else {
                    AiTextOperation::Transform
                };
                let started = service
                    .focused_context()
                    .and_then(|context| service.start_ai_text_request(&context, operation))
                    .unwrap_or(false);
                if !started {
                    let message = if operation == AiTextOperation::Proofread {
                        "校正する文字列を選択してください"
                    } else {
                        "変換する入力中または選択中の文字列がありません"
                    };
                    service.set_ai_error(Some(message.to_owned()));
                }
                return Ok(());
            }
            self.get_impl().select_mode_menu_command(command);
        }
        Ok(())
    }

    fn GetIcon(&self) -> Result<windows::Win32::UI::WindowsAndMessaging::HICON> {
        let state = self.get_impl().mode_item.snapshot();
        let mode = state.mode.ok_or_else(|| Error::from_hresult(E_FAIL))?;
        mode_item::icon_for(mode)
    }

    fn GetText(&self) -> Result<BSTR> {
        let state = self.get_impl().mode_item.snapshot();
        let text = state.mode.map(mode_item::label).unwrap_or("Sakura Input");
        Ok(BSTR::from(text))
    }
}

impl ITfSource_Impl for TextService_Impl {
    fn AdviseSink(&self, riid: *const GUID, punk: Ref<'_, IUnknown>) -> Result<u32> {
        if riid.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: `riid` was checked non-null and is readable for this COM
        // call. The language bar only supplies its `ITfLangBarItemSink` here.
        if unsafe { *riid } != ITfLangBarItemSink::IID {
            return Err(mode_item::cannot_connect());
        }
        let sink: ITfLangBarItemSink = punk.ok()?.cast()?;
        self.get_impl().mode_item.advise_sink(sink)
    }

    fn UnadviseSink(&self, cookie: u32) -> Result<()> {
        self.get_impl().mode_item.unadvise_sink(cookie)
    }
}

impl ITfFunctionProvider_Impl for TextService_Impl {
    fn GetType(&self) -> Result<GUID> {
        Ok(CLSID_SAKURA_TSF)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from(TEXT_SERVICE_DESCRIPTION))
    }

    fn GetFunction(&self, rguid: *const GUID, riid: *const GUID) -> Result<IUnknown> {
        if rguid.is_null() || riid.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: both pointers were checked and COM guarantees readable GUIDs
        // for the duration of this call.
        let (function_guid, interface_id) = unsafe { (*rguid, *riid) };
        if function_guid != GUID::from_u128(0) || interface_id != ITfFnReconversion::IID {
            return Err(Error::from_hresult(E_NOINTERFACE));
        }
        let function: ITfFnReconversion = self.to_interface();
        function.cast()
    }
}

impl ITfFunction_Impl for TextService_Impl {
    fn GetDisplayName(&self) -> Result<BSTR> {
        Ok(BSTR::from("Sakura Input reconversion"))
    }
}

impl ITfFnReconversion_Impl for TextService_Impl {
    fn QueryRange(
        &self,
        prange: Ref<'_, ITfRange>,
        ppnewrange: OutRef<'_, ITfRange>,
        pfconvertable: *mut BOOL,
    ) -> Result<()> {
        let range = prange.ok()?;
        if pfconvertable.is_null() {
            return Err(Error::from_hresult(E_INVALIDARG));
        }

        let convertible = self.get_impl().range_text(range).is_ok();
        if !ppnewrange.is_null() {
            let returned = if convertible {
                // SAFETY: the callback range is live; cloning does not require
                // an edit cookie and gives the caller independent ownership.
                Some(unsafe { range.Clone()? })
            } else {
                None
            };
            ppnewrange.write(returned)?;
        }
        // SAFETY: non-null checked above and COM guarantees a writable BOOL.
        unsafe { pfconvertable.write(convertible.into()) };
        Ok(())
    }

    fn GetReconversion(&self, prange: Ref<'_, ITfRange>) -> Result<ITfCandidateList> {
        let range = prange.ok()?;
        let service = self.get_impl();
        let text = service.range_text(range)?;
        let output = match service.ask_reconversion(text, true)? {
            Answer::Ready(output) => output,
            Answer::Busy => return Err(Error::from_hresult(E_FAIL)),
            // Never left this process; this is a read-only candidate query
            // with no reservation to release.
            Answer::Rejected => return Err(Error::from_hresult(E_FAIL)),
            Answer::Unavailable => return Err(Error::from_hresult(E_FAIL)),
        };
        let candidates = output
            .candidates
            .as_ref()
            .filter(|candidates| candidates.kind == CandidateKind::Conversion)
            .ok_or_else(|| Error::from_hresult(E_FAIL))?;
        reconversion::candidate_list(candidates)
    }

    fn Reconvert(&self, prange: Ref<'_, ITfRange>) -> Result<()> {
        let range = prange.ok()?;
        let service = self.get_impl();
        if !service.composition_is_idle()? {
            return Err(Error::from_hresult(E_FAIL));
        }

        // SAFETY: the callback range is live and the clone is retained until
        // the write edit session selects it.
        let target = unsafe { range.Clone()? };
        // SAFETY: the same live callback range retains its owning context for
        // the duration of this synchronous query.
        let context = unsafe { range.GetContext()? };
        let text = service.range_text(range)?;
        service.observe_write_context(&context)?;
        if !service.can_admit_write_for_context(&context)? {
            return Err(Error::from_hresult(E_FAIL));
        }
        // Reconvert advances the engine session too, so it uses the exact same
        // pre-admission boundary as a key. Never ask the engine when no bounded
        // document slot is available to own its result.
        let reservation = service
            .reserve_write(&context)
            .map_err(|_| Error::from_hresult(E_FAIL))?;
        let answer = match service.ask_reconversion(text, false) {
            Ok(answer) => answer,
            Err(_) => {
                if self.recover_from_engine_unavailable(&context).is_err() {
                    service.disconnect();
                }
                return Err(Error::from_hresult(E_FAIL));
            }
        };
        let output = match answer {
            Answer::Ready(output) => output,
            Answer::Busy => {
                service.cancel_reservation(reservation, CancelReason::PredecessorFailed);
                return Err(Error::from_hresult(E_FAIL));
            }
            // This request never reached the engine -- it failed to encode
            // on this side of the wire (e.g. a selection too large for the
            // wire format). The peer never saw it and never misbehaved, so
            // only this reservation is released; the link and any other
            // queued write are left exactly as they were.
            Answer::Rejected => {
                service.cancel_reservation(reservation, CancelReason::RequestRejected);
                return Err(Error::from_hresult(E_FAIL));
            }
            Answer::Unavailable => {
                if self.recover_from_engine_unavailable(&context).is_err() {
                    service.disconnect();
                }
                return Err(Error::from_hresult(E_FAIL));
            }
        };
        let valid_candidates = output
            .candidates
            .as_ref()
            .filter(|candidates| candidates.kind == CandidateKind::Conversion);
        let Some(candidates) = valid_candidates else {
            service.cancel_reservation(reservation, CancelReason::PredecessorFailed);
            service.revert_engine();
            return Err(Error::from_hresult(E_FAIL));
        };

        let plan = match service.plan(&output) {
            Ok(plan) if !plan.updates.is_empty() => plan,
            Ok(_) | Err(_) => {
                service.cancel_reservation(reservation, CancelReason::PredecessorFailed);
                service.revert_engine();
                return Err(Error::from_hresult(E_FAIL));
            }
        };
        let payload = PendingWrite {
            context: context.clone(),
            plan,
            target_range: Some(target),
            query_layout: false,
            // Reconvert is itself a synchronous TSF request. Preserve its
            // synchronous-first document path; `edit_session` still falls
            // back once to an ordered async request if the host refuses it.
            synchronous_first: true,
            candidates: CandidateEffect::Show(candidates.clone()),
            undo_commit: false,
            engine_recovery: None,
            ai_source_validation: None,
            ai_record: None,
        };
        if let Err(error) = self.submit_pending_write(reservation, payload, true) {
            service.revert_engine();
            return Err(error);
        }
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
            let service = self.get_impl();
            service.record_diagnostic_lifecycle(diagnostic_ring::LifecycleEvent::FocusChanged, 0);
            service.resume_after_focus_gain();
            service.focus_foreground.set(true);
            service.refresh_mode_item_for_focus();
            return Ok(());
        }

        let service = self.get_impl();
        service.record_diagnostic_lifecycle(diagnostic_ring::LifecycleEvent::FocusChanged, 0);
        // The OS indicator follows the caret, not the engine lifetime. Hide it
        // before scheduling focus-loss finalization, which can run after a
        // different document has already acquired focus.
        service.cancel_pending_ai("focus_changed");
        service.ai_key_latched.set(false);
        service.focus_foreground.set(false);
        service.mode_item.hide();
        service.invalidate_for_focus_change();
        service.queue_focus_loss()
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
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Result<BOOL> {
        let key = key_handler::translate((wparam.0 & 0xFFFF) as u16, lparam.0, true);
        let service = self.get_impl();
        Ok((service.ai_key_latched.get() && service.ai_trigger_matches(key)).into())
    }

    fn OnKeyUp(&self, _pic: Ref<'_, ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let key = key_handler::translate((wparam.0 & 0xFFFF) as u16, lparam.0, false);
        let service = self.get_impl();
        if service.ai_key_latched.get() && service.ai_trigger_matches(key) {
            service.ai_key_latched.set(false);
            return Ok(true.into());
        }
        Ok(false.into())
    }

    /// TSF delivers the registered physical 半角/全角 key by GUID even while
    /// the engine is in Direct mode. Normalize it once, then reuse the same
    /// admission, IPC, and document-write path as an ordinary key event.
    fn OnPreservedKey(&self, pic: Ref<'_, ITfContext>, rguid: *const GUID) -> Result<BOOL> {
        if rguid.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        // SAFETY: the pointer was checked and TSF guarantees a readable GUID
        // for this callback. Copy it before any path that can re-enter COM.
        let guid = unsafe { *rguid };
        let Some(key) = preserved_key_input(&guid) else {
            return Ok(false.into());
        };
        self.handle_key_input(pic, key)
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
        composition: Ref<'_, ITfComposition>,
    ) -> Result<()> {
        let service = self.get_impl();
        // EndComposition from the currently owning edit callback notifies this
        // sink too. Its marker covers only that one host call, and must match
        // both the live canonical handle and its exact flight. Every mismatch
        // is external and therefore revokes the callback before state changes.
        let expected_self_termination = match composition.ok() {
            Ok(composition) => service.consume_expected_self_composition_termination(composition),
            Err(_) => false,
        };
        if expected_self_termination {
            return Ok(());
        }
        service.record_diagnostic_lifecycle(diagnostic_ring::LifecycleEvent::CompositionEnded, 0);
        service.invalidate_for_composition_termination();
        let _ = service.queue_end_candidates();
        service.ask_to_finalize();
        service.forget_composition()
    }
}

impl ITfTextLayoutSink_Impl for TextService_Impl {
    fn OnLayoutChange(
        &self,
        context: Ref<'_, ITfContext>,
        _code: TfLayoutCode,
        _view: Ref<'_, ITfContextView>,
    ) -> Result<()> {
        let callback_context = context.ok()?;
        let service = self.get_impl();
        let tracked = service
            .layout
            .try_borrow()
            .map_err(|_| reentrancy())?
            .subscription
            .as_ref()
            .map(|subscription| (subscription.context.clone(), subscription.lease));
        match tracked {
            Some((tracked, lease))
                if tracked.as_raw() == callback_context.as_raw()
                    && service.ui_lease_is_current(&tracked, lease) =>
            {
                service.queue_layout()
            }
            _ => Ok(()),
        }
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
    use sakura_ipc::{Descriptor, PipeInstance};
    use sakura_proto::{
        decode_request, encode_response, Modifiers, Request, Response, Segment, UnderlineKind,
    };

    fn fake_engine_for_unknown_undo(tag: &str) -> (String, std::thread::JoinHandle<()>) {
        let name = format!(
            r"\\.\pipe\sakura_tsf_unknown_undo_{tag}_{}",
            std::process::id()
        );
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("create");
        let handle = std::thread::spawn(move || {
            server.wait_for_client().expect("client");
            let mut buffer = Vec::new();

            let payload = server.read_frame(&mut buffer).expect("Hello request");
            let (id, request) = decode_request(payload).expect("decode Hello");
            assert!(matches!(request, Request::Hello { .. }));
            let mut reply = Vec::new();
            encode_response(
                &Response::Hello {
                    server_version: sakura_proto::PROTOCOL_VERSION,
                    engine_version: [0, 1, 0],
                },
                id,
                &mut reply,
            )
            .expect("encode Hello");
            server.write_all(&reply).expect("write Hello");

            let payload = server
                .read_frame(&mut buffer)
                .expect("CreateSession request");
            let (id, request) = decode_request(payload).expect("decode CreateSession");
            assert!(matches!(request, Request::CreateSession { .. }));
            reply.clear();
            encode_response(
                &Response::SessionCreated {
                    session: 1,
                    mode: Mode::Hiragana,
                },
                id,
                &mut reply,
            )
            .expect("encode CreateSession");
            server.write_all(&reply).expect("write CreateSession");

            let payload = server.read_frame(&mut buffer).expect("UndoCommit request");
            let (id, request) = decode_request(payload).expect("decode UndoCommit");
            assert!(matches!(
                request,
                Request::UndoCommit {
                    outcome: UndoCommitOutcome::Unknown,
                    ..
                }
            ));
            reply.clear();
            encode_response(&Response::Ok, id, &mut reply).expect("encode UndoCommit");
            server.write_all(&reply).expect("write UndoCommit");
        });
        (name, handle)
    }

    fn fake_engine_for_undo_timeout(tag: &str) -> (String, std::thread::JoinHandle<()>) {
        let name = format!(
            r"\\.\pipe\sakura_tsf_undo_timeout_{tag}_{}",
            std::process::id()
        );
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("create");
        let handle = std::thread::spawn(move || {
            server.wait_for_client().expect("client");
            let mut buffer = Vec::new();

            let payload = server.read_frame(&mut buffer).expect("Hello request");
            let (id, request) = decode_request(payload).expect("decode Hello");
            assert!(matches!(request, Request::Hello { .. }));
            let mut reply = Vec::new();
            encode_response(
                &Response::Hello {
                    server_version: sakura_proto::PROTOCOL_VERSION,
                    engine_version: [0, 1, 0],
                },
                id,
                &mut reply,
            )
            .expect("encode Hello");
            server.write_all(&reply).expect("write Hello");

            let payload = server
                .read_frame(&mut buffer)
                .expect("CreateSession request");
            let (id, request) = decode_request(payload).expect("decode CreateSession");
            assert!(matches!(request, Request::CreateSession { .. }));
            reply.clear();
            encode_response(
                &Response::SessionCreated {
                    session: 1,
                    mode: Mode::Hiragana,
                },
                id,
                &mut reply,
            )
            .expect("encode CreateSession");
            server.write_all(&reply).expect("write CreateSession");

            let payload = server.read_frame(&mut buffer).expect("UndoCommit request");
            let (_id, request) = decode_request(payload).expect("decode UndoCommit");
            assert!(matches!(
                request,
                Request::UndoCommit {
                    outcome: UndoCommitOutcome::Rejected,
                    ..
                }
            ));
            // Let the fixed 50 ms engine budget expire. This is the one engine
            // failure mode that returns false while retaining a desynchronized
            // transport, which the early terminal helper must explicitly drop.
            std::thread::sleep(std::time::Duration::from_millis(100));
        });
        (name, handle)
    }

    fn layout_claim(context: ContextId) -> LayoutQueryClaim {
        let mut journal: WriteCoordinator<()> = WriteCoordinator::new(1);
        assert!(journal.activate().is_empty());
        assert!(journal.observe_context(context).is_empty());
        let reservation = journal.reserve(context).expect("reserve");
        let visible = journal.tail_visible();
        journal
            .attach(reservation, (), false, visible.clone(), visible)
            .expect("attach");
        let request = journal.begin_head().expect("request");
        let lease = journal
            .complete_applied(request.ticket)
            .expect("completion")
            .ui_lease
            .expect("UI lease");
        LayoutQueryClaim { context, lease }
    }

    fn fake_engine_with_no_further_requests(tag: &str) -> (String, std::thread::JoinHandle<()>) {
        let name = format!(r"\\.\pipe\sakura_tsf_recovery_{tag}_{}", std::process::id());
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("create");
        let handle = std::thread::spawn(move || {
            server.wait_for_client().expect("client");
            let mut buffer = Vec::new();

            let payload = server.read_frame(&mut buffer).expect("Hello request");
            let (id, request) = decode_request(payload).expect("decode Hello");
            assert!(matches!(request, Request::Hello { .. }));
            let mut reply = Vec::new();
            encode_response(
                &Response::Hello {
                    server_version: sakura_proto::PROTOCOL_VERSION,
                    engine_version: [0, 1, 0],
                },
                id,
                &mut reply,
            )
            .expect("encode Hello");
            server.write_all(&reply).expect("write Hello");

            let payload = server
                .read_frame(&mut buffer)
                .expect("CreateSession request");
            let (id, request) = decode_request(payload).expect("decode CreateSession");
            assert!(matches!(request, Request::CreateSession { .. }));
            reply.clear();
            encode_response(
                &Response::SessionCreated {
                    session: 1,
                    mode: Mode::Hiragana,
                },
                id,
                &mut reply,
            )
            .expect("encode CreateSession");
            server.write_all(&reply).expect("write CreateSession");

            // A local encode failure never reaches the wire (`client.rs`
            // rejects it before the first `write_all`), so a healthy peer
            // modeling that scenario has nothing further to read or answer.
        });
        (name, handle)
    }

    /// Traces what `recover_from_engine_unavailable` (this file, reached
    /// from `Reconvert` on `Answer::Unavailable`) actually does to a
    /// *healthy* connection and an unrelated in-flight write -- not from
    /// reading the source, but by executing its three COM-free steps
    /// directly.
    ///
    /// `Reconvert`'s own precondition (`composition_is_idle`) means a local
    /// `TooLarge` there can never reach this rescue path with a live
    /// composition on screen, and `enqueue_finalization`'s early return for
    /// an idle/empty projection means the one step this test cannot exercise
    /// (`finalize_visible_text_async`, which needs a real `ITfContext` this
    /// crate's test suite has no way to construct without a live COM host)
    /// is a documented no-op in that exact case. What remains observable
    /// here is the blast radius of the first three steps: do they discard
    /// more than the one operation that actually failed?
    #[test]
    fn recovering_from_a_local_encode_failure_still_drops_a_healthy_link_and_its_own_reservation() {
        let (name, server) = fake_engine_with_no_further_requests("recovery");

        let service = TextService::new();
        *service.engine.borrow_mut() = Engine::attached_to(&name);
        assert!(
            service.engine.borrow().is_connected(),
            "the handshake must have completed"
        );

        // Stands in for the reservation `Reconvert` takes out before it ever
        // asks the engine anything (`reserve_write`, called before
        // `ask_reconversion`) -- still unattached, exactly as it is at the
        // moment a local encode failure answers `Unavailable` and this
        // rescue path runs.
        let context = ContextId(1);
        {
            let mut writes = service.writes.borrow_mut();
            assert!(writes.activate().is_empty());
            assert!(writes.observe_context(context).is_empty());
            writes
                .reserve(context)
                .expect("the journal must admit one reservation");
        }
        assert_eq!(service.writes.borrow().pending_len(), 1);

        // `recover_from_engine_unavailable`'s three COM-free steps, in
        // order, verbatim from `text_service.rs`.
        service.invalidate_inflight_composition_write_as_unknown();
        service.cancel_all_writes(CancelReason::EngineUnavailable, true);
        service.disconnect();

        assert_eq!(
            service.writes.borrow().pending_len(),
            0,
            "the rescue path drains the whole journal -- including a \
             reservation the failed local request never actually used -- \
             not just the one operation that failed"
        );
        assert!(
            !service.engine.borrow().is_connected(),
            "current behavior: recovering from a request this process's \
             own encoder rejected locally still tears down an otherwise \
             healthy engine connection"
        );

        drop(service);
        server.join().expect("the server thread");
    }

    fn fake_engine_for_reject_then_key(tag: &str) -> (String, std::thread::JoinHandle<()>) {
        let name = format!(
            r"\\.\pipe\sakura_tsf_reject_then_key_{tag}_{}",
            std::process::id()
        );
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("create");
        let handle = std::thread::spawn(move || {
            server.wait_for_client().expect("client");
            let mut buffer = Vec::new();

            let payload = server.read_frame(&mut buffer).expect("Hello request");
            let (id, request) = decode_request(payload).expect("decode Hello");
            assert!(matches!(request, Request::Hello { .. }));
            let mut reply = Vec::new();
            encode_response(
                &Response::Hello {
                    server_version: sakura_proto::PROTOCOL_VERSION,
                    engine_version: [0, 1, 0],
                },
                id,
                &mut reply,
            )
            .expect("encode Hello");
            server.write_all(&reply).expect("write Hello");

            let payload = server
                .read_frame(&mut buffer)
                .expect("CreateSession request");
            let (id, request) = decode_request(payload).expect("decode CreateSession");
            assert!(matches!(request, Request::CreateSession { .. }));
            reply.clear();
            encode_response(
                &Response::SessionCreated {
                    session: 1,
                    mode: Mode::Hiragana,
                },
                id,
                &mut reply,
            )
            .expect("encode CreateSession");
            server.write_all(&reply).expect("write CreateSession");

            // The oversized `Reconvert` never reaches the wire -- encoding
            // fails inside `Client::call`, before any byte is written to
            // this pipe (`client.rs`). So the next, and only remaining,
            // request this peer ever sees is the verification key sent
            // right after it.
            let payload = server.read_frame(&mut buffer).expect("SendKey request");
            let (id, request) = decode_request(payload).expect("decode SendKey");
            assert!(matches!(request, Request::SendKey { .. }));
            reply.clear();
            encode_response(&Response::Output(output(None, None)), id, &mut reply)
                .expect("encode SendKey");
            server.write_all(&reply).expect("write SendKey");
        });
        (name, handle)
    }

    /// The full contract for a local reconvert encode failure. `Engine::request`
    /// answers `Answer::Rejected` without dropping the link (`engine.rs`), and
    /// `Reconvert`'s `Answer::Rejected` arm (this file) cancels only its own
    /// reservation -- with the accurately-named `CancelReason::RequestRejected`,
    /// not the `PredecessorFailed` used for the `Busy` arm right next to it --
    /// and never calls `recover_from_engine_unavailable`. Contrast with
    /// `recovering_from_a_local_encode_failure_still_drops_a_healthy_link_and_its_own_reservation`
    /// above, which proves what that rescue path *does* do when it runs: this
    /// test's surviving unrelated write and surviving link are the observable
    /// proof that path was never invoked.
    ///
    /// `Reconvert`'s live `service.writes: WriteCoordinator<PendingWrite>`
    /// needs a real `ITfContext` to `attach` a second, unrelated write --
    /// unavailable in this crate's test suite without a live COM host (the
    /// same constraint documented on `layout_claim`). `cancel_reservation`'s
    /// scoping (`write_coordinator.rs`) has no special case per payload type,
    /// so a standalone `WriteCoordinator<()>` proves the exact mechanism the
    /// fix calls.
    #[test]
    fn local_reconvert_encode_failure_rejects_only_that_operation() {
        // The scoped-cancellation half of the contract: does cancelling the
        // failing operation's own reservation leave an unrelated,
        // already-attached write alone?
        let context = ContextId(1);
        let mut journal: WriteCoordinator<()> = WriteCoordinator::new(2);
        assert!(journal.activate().is_empty());
        assert!(journal.observe_context(context).is_empty());

        let unrelated = journal
            .reserve(context)
            .expect("reserve the unrelated write");
        let visible = journal.tail_visible();
        journal
            .attach(unrelated, (), false, visible.clone(), visible.clone())
            .expect("attach the unrelated write ahead of the failing one");

        let own = journal.reserve(context).expect(
            "the coordinator must still admit the failing operation's own \
             reservation behind the already-attached unrelated one",
        );
        assert_eq!(journal.pending_len(), 2);

        let cancelled = journal.cancel_reservation(own, CancelReason::RequestRejected);
        assert_eq!(
            cancelled.len(),
            1,
            "only the failing operation's own reservation is cancelled"
        );
        assert_eq!(
            journal.pending_len(),
            1,
            "the unrelated, already-attached write must still be pending"
        );

        // The link-and-request half of the contract, against the real
        // `Engine` and a real peer: `Answer::Rejected`, the link stays
        // usable, and a normal request right after succeeds on it.
        let (name, server) = fake_engine_for_reject_then_key("reject-scope");
        let mut engine = Engine::attached_to(&name);
        assert!(engine.is_connected(), "the handshake must have completed");

        // A `text` this large fails `write_str`'s own `MAX_STRING_BYTES`
        // (4096 bytes, see `wire.rs`) check well before the request could
        // even approach `MAX_PAYLOAD` (64 KiB).
        let huge_text = "あ".repeat(30_000); // 90,000 bytes
        assert!(
            matches!(engine.reconvert(huge_text, false), Answer::Rejected),
            "a local encode failure must answer Rejected, not Unavailable"
        );
        assert!(
            engine.is_connected(),
            "a local encode failure must not drop an otherwise healthy link"
        );
        assert!(
            matches!(
                engine.send_key(KeyInput {
                    code: KeyCode::Char,
                    ch: Some('k'),
                    modifiers: Modifiers::NONE,
                    repeat: false,
                    test_only: false,
                }),
                Answer::Ready(_)
            ),
            "the immediately following normal request must succeed on the \
             same link"
        );

        drop(engine);
        server.join().expect("the server thread");
    }

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

    /// Builds a `Preedit` out of explicit `(text, underline)` pairs, for
    /// tests that need something other than every segment defaulting to
    /// `UnderlineKind::Raw`.
    fn preedit_with_underlines(parts: &[(&str, UnderlineKind)]) -> Preedit {
        Preedit {
            segments: parts
                .iter()
                .map(|(text, underline)| Segment {
                    text: (*text).to_owned(),
                    underline: *underline,
                })
                .collect(),
            cursor: 0,
        }
    }

    /// The concatenation of every segment's text, discarding its underline --
    /// the same flattening `visible_text` does, but taking the segments
    /// `Update::Show` now carries directly rather than a whole `Preedit`.
    fn segments_text(segments: &[Segment]) -> String {
        segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect()
    }

    fn output(commit: Option<&str>, shown: Option<&[&str]>) -> Output {
        Output {
            consumed: true,
            beep: false,
            mode: None,
            preedit: shown.map(preedit),
            commit: commit.map(str::to_owned),
            delete_before: String::new(),
            candidates: None,
            candidate_detail: None,
        }
    }

    fn state(text: &str, has_composition: bool) -> VisibleState {
        VisibleState {
            text: text.to_owned(),
            has_composition,
        }
    }

    #[test]
    fn commit_undo_post_document_access_projection_failure_reports_unknown_and_terminalizes_link() {
        let context_id = ContextId(77);
        let mut journal: WriteCoordinator<bool> = WriteCoordinator::new(2);
        assert!(journal.activate().is_empty());
        assert!(journal.observe_context(context_id).is_empty());
        let reservation = journal.reserve(context_id).expect("reservation");
        let before = journal.tail_visible();
        journal
            .attach(reservation, true, true, before.clone(), state("復元", true))
            .expect("attach pending undo");
        let ticket = journal.begin_head().expect("requested head").ticket;
        let (name, peer) = fake_engine_for_unknown_undo("projection-failure");
        let service = TextService::new();
        *service.engine.borrow_mut() = Engine::attached_to(&name);
        assert!(service.engine.borrow().is_connected());
        let settlement_count = Cell::new(0);
        let terminal = terminalize_unknown_undo_after_document_access(
            &mut journal,
            ticket,
            |payload| *payload,
            |outcome| {
                settlement_count.set(settlement_count.get() + 1);
                assert_eq!(outcome, UndoCommitOutcome::Unknown);
                assert!(service.settle_undo_commit(outcome));
                false
            },
        );

        assert_eq!(terminal.completions.len(), 1);
        assert_eq!(settlement_count.get(), 1);
        assert!(terminal.has_undo);
        assert!(terminal.journal_drained);
        assert!(!terminal.retry_allowed);
        assert!(!terminal.settlement_confirmed);
        assert!(terminal.disconnect_required);
        assert_eq!(journal.pending_len(), 0);

        // The production caller retires the link when this shared primitive
        // cannot confirm the settlement. The callback/context integration is
        // intentionally not fabricated here; the real PendingWrite invariant
        // remains `context: ITfContext` in production.
        if terminal.disconnect_required {
            service.disconnect();
        }
        assert!(!service.engine.borrow().is_connected());
        peer.join().expect("fake engine terminal outcome");
    }

    #[test]
    fn commit_undo_reentrant_journal_borrow_has_a_bounded_terminal_owner() {
        // A COM-free test cannot fabricate the real `PendingWrite.context`.
        // Exercise the shared production Probe decision and journal primitive
        // with its equivalent payload first, then exercise the TextService
        // marker that owns a refused `RefCell` borrow. Together these are the
        // production units used by the real callback path.
        let context_id = ContextId(78);
        let mut journal: WriteCoordinator<bool> = WriteCoordinator::new(2);
        assert!(journal.activate().is_empty());
        assert!(journal.observe_context(context_id).is_empty());
        let reservation = journal.reserve(context_id).expect("reservation");
        let before = journal.tail_visible();
        journal
            .attach(reservation, true, true, before.clone(), state("x", true))
            .expect("attach pending undo");
        let ticket = journal.begin_head().expect("requested head").ticket;
        let service = TextService::new();
        service
            .undo_terminalization
            .set(Some(UndoCommitOutcome::Unknown));
        let marker_before = service.undo_terminalization.get();
        let pending_before = journal.pending_len();
        let terminals_before = journal.terminal_records();
        let settlement_count = Cell::new(0);
        assert_eq!(
            service.probe_fence(context_id).expect("Probe fence"),
            ProbeFence::Busy
        );
        assert_eq!(
            decide_probe_fence(
                service.undo_terminalization.get(),
                &journal,
                context_id,
                |payload| *payload,
                false,
                false,
            ),
            ProbeFence::Busy
        );
        // Probe has only read the shared production decision inputs: the
        // deferred marker, journal payload, terminal records, and settlement
        // callback state are byte-for-byte/logically unchanged.
        assert_eq!(service.undo_terminalization.get(), marker_before);
        assert_eq!(journal.pending_len(), pending_before);
        assert_eq!(journal.terminal_records(), terminals_before);
        assert_eq!(settlement_count.get(), 0);

        // The corresponding real-key owner settles once before returning the
        // same consumed result. An empty journal has no payload to settle, but
        // its marker still must be cleared rather than allowing the key to be
        // applied after the handoff.
        assert!(service.deferred_undo_consumes_real_key());
        assert_eq!(service.undo_terminalization.get(), None);
        assert_eq!(settlement_count.get(), 0);
        service
            .undo_terminalization
            .set(Some(UndoCommitOutcome::Unknown));

        let terminal = terminalize_unknown_undo_after_document_access(
            &mut journal,
            ticket,
            |payload| *payload,
            |outcome| {
                settlement_count.set(settlement_count.get() + 1);
                assert_eq!(outcome, UndoCommitOutcome::Unknown);
                false
            },
        );
        assert_eq!(terminal.completions.len(), 1);
        assert!(terminal.has_undo);
        assert!(terminal.journal_drained);
        assert!(!terminal.retry_allowed);
        assert!(!terminal.settlement_confirmed);
        assert!(terminal.disconnect_required);
        assert_eq!(settlement_count.get(), 1);
        assert_eq!(journal.pending_len(), 0);

        let writes = service.writes.borrow_mut();

        // This is the failure point shared by cancel-all and the callback
        // cancellation helpers. Returning here without an owner would leave
        // a real pending undo transaction fenced forever; the non-empty
        // generic journal above proves the payload side before this COM-free
        // marker test.
        service.cancel_all_writes_with_undo_outcome(
            CancelReason::StaleCallback,
            true,
            Some(UndoCommitOutcome::Unknown),
        );
        assert_eq!(
            service.undo_terminalization.get(),
            Some(UndoCommitOutcome::Unknown)
        );
        drop(writes);

        // The next owner drains any journal left by the re-entrant owner and
        // clears the marker exactly once; no later key can observe a stale
        // Busy/pending state.
        assert!(service.try_settle_deferred_undo_terminalization());
        assert_eq!(service.undo_terminalization.get(), None);
        assert_eq!(service.writes.borrow().pending_len(), 0);
        assert!(service.try_settle_deferred_undo_terminalization());
        assert_eq!(service.undo_terminalization.get(), None);
    }

    #[test]
    fn commit_undo_test_only_context_fences_preserve_replacement_cleanup_ownership() {
        let first = ContextId(81);
        let second = ContextId(82);

        let mut same_context_full: WriteCoordinator<bool> = WriteCoordinator::new(1);
        assert!(same_context_full.activate().is_empty());
        assert!(same_context_full.observe_context(first).is_empty());
        let reservation = same_context_full
            .reserve(first)
            .expect("same-context reservation");
        assert!(!same_context_full.can_admit_for_context(first));
        assert_eq!(
            decide_probe_fence(None, &same_context_full, first, |_| false, false, false),
            ProbeFence::Declined,
            "a same-context full/reserved journal remains an ordinary host decline"
        );
        assert_eq!(same_context_full.pending_len(), 1);
        assert!(
            same_context_full
                .cancel_reservation(reservation, CancelReason::PredecessorFailed)
                .len()
                == 1
        );

        let mut different_context_full: WriteCoordinator<bool> = WriteCoordinator::new(1);
        assert!(different_context_full.activate().is_empty());
        assert!(different_context_full.observe_context(first).is_empty());
        let _reservation = different_context_full
            .reserve(first)
            .expect("different-context reservation");
        assert!(different_context_full.can_admit_for_context(second));
        assert_eq!(
            decide_probe_fence(
                None,
                &different_context_full,
                second,
                |_| false,
                false,
                false,
            ),
            ProbeFence::ContextReplacement,
            "Probe must fence a replacement instead of using the old session"
        );
        assert_eq!(different_context_full.pending_len(), 1);
        let cancelled = different_context_full.observe_context(second);
        assert_eq!(cancelled.len(), 1, "real replacement owns old cleanup once");
        assert_eq!(different_context_full.pending_len(), 0);
        assert!(!different_context_full.is_context_replacement(second));
        assert!(different_context_full.can_admit_for_context(second));
        // The reservation was terminalized by the one real replacement owner;
        // a second cleanup attempt has no journal entry to consume.
        assert!(different_context_full.observe_context(second).is_empty());

        let mut different_context_non_full: WriteCoordinator<bool> = WriteCoordinator::new(2);
        assert!(different_context_non_full.activate().is_empty());
        assert!(different_context_non_full.observe_context(first).is_empty());
        assert!(different_context_non_full.can_admit_for_context(second));
        assert_eq!(
            decide_probe_fence(
                None,
                &different_context_non_full,
                second,
                |_| false,
                false,
                false,
            ),
            ProbeFence::ContextReplacement,
            "a non-full replacement is still a context transition, not an old-session Probe"
        );
        assert_eq!(different_context_non_full.pending_len(), 0);
        assert!(different_context_non_full
            .observe_context(second)
            .is_empty());
        assert!(!different_context_non_full.is_context_replacement(second));
    }

    #[test]
    fn commit_undo_real_context_replacement_continues_first_and_toggle_keys_to_apply() {
        let first = ContextId(85);
        let second = ContextId(86);
        let mut journal: WriteCoordinator<()> = WriteCoordinator::new(1);
        assert!(journal.activate().is_empty());
        assert!(journal.observe_context(first).is_empty());
        let reservation = journal.reserve(first).expect("old-context reservation");
        assert!(journal.is_context_replacement(second));

        // This is the exact action selected by the production
        // `handle_key_input` branch before it calls `observe_write_context`.
        // The action is independent of the physical key, so both the first
        // character and the preserved HankakuZenkaku key must continue into
        // the same Apply path after cleanup.
        for (name, key) in [
            (
                "first character",
                KeyInput {
                    code: KeyCode::Char,
                    ch: Some('a'),
                    modifiers: Modifiers::NONE,
                    repeat: false,
                    test_only: false,
                },
            ),
            (
                "HankakuZenkaku",
                KeyInput {
                    code: KeyCode::HankakuZenkaku,
                    ch: None,
                    modifiers: Modifiers::NONE,
                    repeat: false,
                    test_only: false,
                },
            ),
        ] {
            assert!(!key.test_only, "{name} must use the real-key action");
            assert_eq!(
                decide_real_fence(false, false, false, false, true),
                RealFenceAction::ReplaceAndApply,
                "successful context replacement must continue {name} to Apply"
            );
        }

        let cancelled = journal.observe_context(second);
        assert_eq!(cancelled.len(), 1, "replacement cleanup has one owner");
        assert_eq!(journal.pending_len(), 0);
        assert!(!journal.is_context_replacement(second));
        assert_eq!(
            decide_real_fence(false, false, false, false, false),
            RealFenceAction::Apply,
            "after successful cleanup the same callback reaches Apply"
        );

        // Every non-Apply branch remains an explicit terminal result; none
        // may silently fall through to a raw host key or a second Apply.
        assert_eq!(
            decide_real_fence(true, true, true, true, true),
            RealFenceAction::DeferredTerminalization
        );
        assert_eq!(
            decide_real_fence(false, true, true, true, true),
            RealFenceAction::Consume
        );
        assert_eq!(
            decide_real_fence(false, false, false, true, true),
            RealFenceAction::Decline
        );

        // The reservation was terminalized by replacement ownership; no
        // second cleanup or callback remains to interfere with the Apply path.
        assert!(journal.observe_context(second).is_empty());
        assert!(journal
            .cancel_reservation(reservation, CancelReason::PredecessorFailed)
            .is_empty());
    }

    #[test]
    fn converting_space_keeps_a_live_composition_across_context_replacement() {
        let space = KeyInput {
            code: KeyCode::Space,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        };
        let henkan = KeyInput {
            code: KeyCode::Henkan,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        };
        let letter = KeyInput {
            code: KeyCode::Char,
            ch: Some('a'),
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        };
        assert!(keep_live_composition_for_convert(space, true));
        assert!(keep_live_composition_for_convert(henkan, true));
        assert!(!keep_live_composition_for_convert(letter, true));
        assert!(!keep_live_composition_for_convert(space, false));
        assert!(!journal_replacement_applies(space, true, true));
        assert!(journal_replacement_applies(letter, true, true));
        assert_eq!(
            decide_real_fence(false, false, false, false, false),
            RealFenceAction::Apply,
            "Space during a live reading must convert, not replace-and-insert"
        );
        assert_eq!(
            decide_real_fence(false, false, false, false, true),
            RealFenceAction::ReplaceAndApply,
            "non-convert keys still follow replacement"
        );
    }

    #[test]
    fn queued_engine_recovery_consumes_keys_until_the_finalizer_terminates() {
        assert_eq!(
            decide_real_fence(false, false, true, false, false),
            RealFenceAction::Consume,
            "returning the key to the host while the old composition finalizer is queued allows stale text to be replayed after host edits"
        );

        let context = ContextId(57);
        let mut journal = WriteCoordinator::<()>::new(1);
        assert!(journal.activate().is_empty());
        assert!(journal.observe_context(context).is_empty());
        assert_eq!(
            decide_probe_fence(None, &journal, context, |_| false, true, false),
            ProbeFence::Busy,
            "OnTestKeyDown must report the same recovery fence without mutating it"
        );
    }

    #[test]
    fn commit_undo_probe_fence_priority_matches_real_terminal_order() {
        assert_eq!(
            probe_action(ProbeFence::ContextReplacement),
            ProbeAction::Ask {
                fresh_context: true
            }
        );
        assert_eq!(
            probe_action(ProbeFence::Open),
            ProbeAction::Ask {
                fresh_context: false
            }
        );

        struct Case {
            name: &'static str,
            marker: Option<UndoCommitOutcome>,
            payload_is_undo: Option<bool>,
            input_blocked: bool,
            requested_context: ContextId,
            expected: ProbeFence,
        }

        let first = ContextId(91);
        let second = ContextId(92);
        let cases = [
            Case {
                name: "marker dominates every other fence",
                marker: Some(UndoCommitOutcome::Unknown),
                payload_is_undo: Some(true),
                input_blocked: true,
                requested_context: second,
                expected: ProbeFence::Busy,
            },
            Case {
                name: "undo payload dominates input block and replacement",
                marker: None,
                payload_is_undo: Some(true),
                input_blocked: true,
                requested_context: second,
                expected: ProbeFence::Busy,
            },
            Case {
                name: "input block dominates replacement",
                marker: None,
                payload_is_undo: Some(false),
                input_blocked: true,
                requested_context: second,
                expected: ProbeFence::Declined,
            },
            Case {
                name: "replacement dominates full admission",
                marker: None,
                payload_is_undo: Some(false),
                input_blocked: false,
                requested_context: second,
                expected: ProbeFence::ContextReplacement,
            },
            Case {
                name: "same-context full journal declines",
                marker: None,
                payload_is_undo: Some(false),
                input_blocked: false,
                requested_context: first,
                expected: ProbeFence::Declined,
            },
            Case {
                name: "open admission probes",
                marker: None,
                payload_is_undo: None,
                input_blocked: false,
                requested_context: first,
                expected: ProbeFence::Open,
            },
        ];

        for case in cases {
            let capacity = usize::from(case.payload_is_undo.is_some()).max(1);
            let mut journal: WriteCoordinator<bool> = WriteCoordinator::new(capacity);
            assert!(journal.activate().is_empty(), "{}", case.name);
            assert!(journal.observe_context(first).is_empty(), "{}", case.name);
            if let Some(payload_is_undo) = case.payload_is_undo {
                let reservation = journal.reserve(first).expect(case.name);
                let before = journal.tail_visible();
                journal
                    .attach(
                        reservation,
                        payload_is_undo,
                        true,
                        before,
                        state("fence", true),
                    )
                    .expect(case.name);
            }

            let pending_before = journal.pending_len();
            let terminals_before = journal.terminal_records();
            let actual = decide_probe_fence(
                case.marker,
                &journal,
                case.requested_context,
                |payload| *payload,
                false,
                case.input_blocked,
            );
            assert_eq!(actual, case.expected, "{}", case.name);
            assert_eq!(journal.pending_len(), pending_before, "{}", case.name);
            assert_eq!(
                journal.terminal_records(),
                terminals_before,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn commit_undo_early_rejected_timeout_drops_the_live_link() {
        let (name, peer) = fake_engine_for_undo_timeout("early-rejected");
        let service = TextService::new();
        *service.engine.borrow_mut() = Engine::attached_to(&name);
        assert!(service.engine.borrow().is_connected());

        assert!(!service.settle_undo_commit_or_disconnect(UndoCommitOutcome::Rejected));
        assert!(!service.engine.borrow().is_connected());
        peer.join().expect("timeout fake engine");
    }

    #[test]
    fn preserved_key_registration_is_only_the_unmodified_vk_kanji_toggle() {
        let registrations = preserved_key_registrations()
            .iter()
            .map(|registration| {
                (
                    registration.guid,
                    registration.key.uVKey,
                    registration.key.uModifiers,
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            registrations,
            vec![(GUID_PRESERVEDKEY_IME_TOGGLE, VK_KANJI.0 as u32, 0)]
        );
    }

    #[test]
    fn preserved_key_input_maps_only_the_toggle_guid() {
        assert_eq!(
            preserved_key_input(&GUID_PRESERVEDKEY_IME_TOGGLE),
            Some(KeyInput {
                code: KeyCode::HankakuZenkaku,
                ch: None,
                modifiers: Modifiers::NONE,
                repeat: false,
                test_only: false,
            })
        );

        for guid in [
            sakura_reg::GUID_PRESERVEDKEY_IME_ON,
            sakura_reg::GUID_PRESERVEDKEY_IME_OFF,
            GUID::from_u128(0),
        ] {
            assert_eq!(preserved_key_input(&guid), None);
        }
    }

    #[test]
    fn unknown_key_without_character_is_declined_before_tsf_work() {
        let modifier_only = KeyInput {
            code: KeyCode::Unknown,
            ch: None,
            modifiers: Modifiers::SHIFT,
            repeat: false,
            test_only: false,
        };
        assert!(is_unactionable_key_input(modifier_only));

        let enter = KeyInput {
            code: KeyCode::Enter,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        };
        assert!(!is_unactionable_key_input(enter));
    }

    #[test]
    fn shifted_ascii_letter_is_not_declined_by_modifier_guard() {
        let shifted_letter = KeyInput {
            code: KeyCode::Char,
            ch: Some('A'),
            modifiers: Modifiers::SHIFT,
            repeat: false,
            test_only: false,
        };

        assert!(!is_unactionable_key_input(shifted_letter));
    }

    #[test]
    fn input_scope_mapping_is_sensitive_and_unknown_values_fail_closed() {
        assert_eq!(
            map_tf_input_scope(TfInputScope(0)),
            Some(InputScope::Normal)
        );
        assert_eq!(map_tf_input_scope(TfInputScope(1)), Some(InputScope::Url));
        assert_eq!(map_tf_input_scope(TfInputScope(4)), Some(InputScope::Email));
        assert_eq!(
            map_tf_input_scope(TfInputScope(6)),
            Some(InputScope::Password)
        );
        assert_eq!(
            map_tf_input_scope(TfInputScope(20)),
            Some(InputScope::Digits)
        );

        assert_eq!(
            classify_tf_input_scopes(&[TfInputScope(0), TfInputScope(6)]),
            InputScope::Password
        );
        assert_eq!(
            classify_tf_input_scopes(&[TfInputScope(0), TfInputScope(999)]),
            InputScope::Unclassified
        );
    }

    #[test]
    fn a_host_that_declares_no_scope_is_ordinary_text_not_a_classification_failure() {
        // Notepad, VS Code, and every plain text field answer this way. When
        // it was treated as a failure the developer history discarded 100% of
        // real input while reporting zero drops and zero persistence errors.
        assert_eq!(classify_declared_scopes(None), InputScope::Normal);

        // A declared scope still decides the class, and an unrecognised value
        // still fails closed.
        assert_eq!(
            classify_declared_scopes(Some(&[TfInputScope(0)])),
            InputScope::Normal
        );
        assert_eq!(
            classify_declared_scopes(Some(&[TfInputScope(6)])),
            InputScope::Password
        );
        assert_eq!(
            classify_declared_scopes(Some(&[TfInputScope(0), TfInputScope(999)])),
            InputScope::Unclassified
        );
    }

    #[test]
    fn an_empty_variant_classifies_as_normal_and_other_shapes_stay_fail_closed() {
        assert_eq!(
            classify_input_scope_variant(VARIANT::default()).expect("an empty VARIANT classifies"),
            InputScope::Normal
        );

        // A VARIANT carrying something other than an ITfInputScope is a
        // property this build does not understand, so it must not be guessed.
        assert!(classify_input_scope_variant(VARIANT::from(42i32)).is_err());
    }

    #[test]
    fn lifecycle_revocation_prevents_an_inflight_callback_from_republishing_state() {
        let mut owner = CompositionWriteOwner::default();
        let flight = owner.begin().expect("first composition flight");
        assert!(owner.owns(flight));
        assert!(owner.lifecycle_is_current(flight));

        // This models OnCompositionTerminated, detach, or context replacement
        // running while the callback is inside a host COM call. The callback's
        // later publish/fail attempt must be a no-op rather than restoring its
        // cloned handle into the lifecycle-retired CompositionState.
        owner.invalidate();
        assert!(!owner.owns(flight));
        assert!(!owner.finish(flight));
        assert!(!owner.lifecycle_is_current(flight));

        let next = owner.begin().expect("next composition flight");
        assert_ne!(flight, next);
        assert!(owner.owns(next));
    }

    #[test]
    fn expected_self_termination_is_not_an_external_lifecycle_event() {
        let mut owner = CompositionWriteOwner::default();
        let flight = owner.begin().expect("composition flight");
        let canonical = CompositionIdentity(7);

        // Before the precise EndComposition callback arms the marker, a host
        // termination for this same composition is still external and must
        // take the lifecycle cancellation path.
        assert!(!expected_self_termination_matches(
            &owner,
            None,
            Some(canonical),
            canonical
        ));

        let expected = Some(ExpectedSelfTermination {
            flight,
            composition: canonical,
        });
        assert!(expected_self_termination_matches(
            &owner,
            expected,
            Some(canonical),
            canonical
        ));
        assert!(!expected_self_termination_matches(
            &owner,
            expected,
            Some(canonical),
            CompositionIdentity(8)
        ));

        owner.invalidate();
        assert!(!expected_self_termination_matches(
            &owner,
            expected,
            Some(canonical),
            canonical
        ));
    }

    #[test]
    fn failed_write_keeps_a_canonical_handle_when_the_callback_clone_is_consumed() {
        assert_eq!(
            merge_canonical_handle(Some("canonical"), None),
            Some("canonical")
        );
        assert_eq!(
            merge_canonical_handle(Some("canonical"), Some("replacement")),
            Some("replacement")
        );
        assert_eq!(merge_canonical_handle::<&str>(None, None), None);
    }

    #[test]
    fn stale_layout_proposal_keeps_the_newer_subscription_and_retires_only_itself() {
        let newer = "newer subscription";
        let proposed = "older proposal";
        let (retained, retired) = merge_layout_subscription(Some(newer), proposed, false);
        assert_eq!(retained, Some(newer));
        assert_eq!(retired, Some(proposed));
    }

    #[test]
    fn stale_same_context_lease_keeps_the_newer_subscription_and_geometry() {
        let newer_lease = "lease B";
        let stale_lease = "lease A";
        let mut layout = LayoutState {
            phase: GeometryPhase::QueryQueued,
            ..Default::default()
        };

        let (retained_lease, retire_geometry) =
            resolve_same_context_layout_lease(newer_lease, stale_lease, false);
        if retire_geometry {
            layout.retire_geometry_for_lease_rollover();
        }
        assert_eq!(retained_lease, newer_lease);
        assert_eq!(layout.phase, GeometryPhase::QueryQueued);

        let (retained_lease, retire_geometry) =
            resolve_same_context_layout_lease(retained_lease, "lease C", true);
        if retire_geometry {
            layout.retire_geometry_for_lease_rollover();
        }
        assert_eq!(retained_lease, "lease C");
        assert_eq!(layout.phase, GeometryPhase::Idle);
    }

    #[test]
    fn focus_change_during_an_inflight_mutation_revokes_publish_and_marks_unknown() {
        let service = TextService::new();
        let flight = {
            let mut state = service.composition.borrow_mut();
            state.text = "visible before focus loss".to_owned();
            state.write_owner.begin().expect("composition flight")
        };

        service.invalidate_for_focus_change();

        let mut state = service.composition.borrow_mut();
        assert!(!state.known);
        assert!(!state.write_owner.owns(flight));
        // A callback returning after the focus notification cannot finish the
        // revoked flight and therefore cannot republish its local projection.
        assert!(!state.write_owner.finish(flight));
        drop(state);
        assert!(service.input_blocked());
    }

    #[test]
    fn focus_regain_before_deferred_dispatch_keeps_a_known_projection_without_cancelled_output() {
        let service = TextService::new();
        {
            let mut state = service.composition.borrow_mut();
            state.text = "known visible preedit".to_owned();
        }
        {
            let mut deferred = service.deferred.borrow_mut();
            deferred.focus_finalization = FocusFinalizationPhase::DeferredQueued;
            deferred.dispatch.work.focus_loss = true;
        }

        service.resume_after_focus_gain();

        let state = service.composition.borrow();
        assert!(state.known);
        assert_eq!(state.text, "known visible preedit");
        drop(state);
        let deferred = service.deferred.borrow();
        assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
        assert!(!deferred.dispatch.work.focus_loss);
        assert!(!deferred.focus_reconciliation_required);
        drop(deferred);
        assert!(!service.input_blocked());
    }

    #[test]
    fn focus_regain_after_engine_commit_started_abandons_an_empty_journal() {
        let service = TextService::new();
        {
            let mut writes = service.writes.borrow_mut();
            assert!(writes.activate().is_empty());
            assert!(writes.observe_context(ContextId(31337)).is_empty());
        }
        {
            let mut state = service.composition.borrow_mut();
            state.text = "engine may have committed before this callback".to_owned();
        }
        {
            let mut deferred = service.deferred.borrow_mut();
            deferred.dispatch.work.focus_loss = true;
            deferred.focus_finalization = FocusFinalizationPhase::EngineCommitStarted;
        }

        service.resume_after_focus_gain();

        let state = service.composition.borrow();
        assert!(state.known);
        assert!(state.text.is_empty());
        assert!(state.handle.is_none());
        assert!(state.context.is_none());
        drop(state);
        let writes = service.writes.borrow();
        assert_eq!(writes.pending_len(), 0);
        assert_eq!(writes.committed_visible(), VisibleState::empty());
        assert_eq!(writes.tail_visible(), VisibleState::empty());
        drop(writes);
        let deferred = service.deferred.borrow();
        assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
        assert!(!deferred.dispatch.work.focus_loss);
        assert!(!service.input_blocked());
    }

    #[test]
    fn rejected_finalizer_after_engine_commit_retires_the_visible_projection() {
        let service = TextService::new();
        {
            let mut state = service.composition.borrow_mut();
            state.text = "visible while RequestEditSession is refused".to_owned();
        }
        {
            let mut deferred = service.deferred.borrow_mut();
            deferred.focus_finalization = FocusFinalizationPhase::EngineCommitStarted;
            deferred.dispatch.work.focus_loss = true;
        }

        // This models an outer/session HRESULT refusal after the focus owner
        // already asked the engine to commit. The write payload has already
        // been terminalized by the journal, so terminal cleanup must not rely
        // on it being present.
        service.settle_cancelled_writes(
            vec![Completion::<PendingWrite> {
                outcome: TerminalOutcome::Rejected,
                payload: None,
                ui_lease: None,
            }],
            true,
            None,
        );

        let state = service.composition.borrow();
        assert!(state.known);
        assert!(state.text.is_empty());
        assert!(state.handle.is_none());
        assert!(state.context.is_none());
        drop(state);
        let deferred = service.deferred.borrow();
        assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
        assert!(!deferred.dispatch.work.focus_loss);
        drop(deferred);
        assert!(!service.input_blocked());
    }

    #[test]
    fn reentrant_focus_retirement_gets_one_deferred_retry_then_retires_every_owner() {
        let service = TextService::new();
        {
            let mut writes = service.writes.borrow_mut();
            assert!(writes.activate().is_empty());
            assert!(writes.observe_context(ContextId(31_338)).is_empty());
        }
        {
            let mut composition = service.composition.borrow_mut();
            composition.text = "visible while a re-entrant owner holds the projection".to_owned();
        }
        {
            let mut deferred = service.deferred.borrow_mut();
            deferred.focus_finalization = FocusFinalizationPhase::EngineCommitStarted;
            deferred.dispatch.work.focus_loss = true;
            // Model the one hidden-window message already in the queue. The
            // retry must coalesce into this ownership rather than post a
            // second message (and this keeps the test COM-free).
            deferred.dispatch.posted = true;
        }

        // The first retirement attempt is synthetically refused by the active
        // composition borrow. `settle_cancelled_writes` still terminalizes the
        // journal/engine/UI side immediately, then leaves exactly one deferred
        // document-free retry for after this borrow is released.
        let held_composition = service.composition.borrow_mut();
        service.settle_cancelled_writes(
            vec![Completion::<PendingWrite> {
                outcome: TerminalOutcome::Rejected,
                payload: None,
                ui_lease: None,
            }],
            true,
            None,
        );
        service.queue_focus_reconciliation();
        {
            let deferred = service.deferred.borrow();
            assert_eq!(
                deferred.focus_finalization,
                FocusFinalizationPhase::ReconciliationQueued
            );
            assert!(deferred.dispatch.work.focus_reconcile);
            assert!(deferred.dispatch.posted);
            // `terminalize_cancelled_state` explicitly queued candidate UI
            // teardown before the delayed composition retirement.
            assert!(deferred.dispatch.work.end_candidates);
        }
        assert!(service.input_blocked());
        assert_eq!(service.writes.borrow().pending_len(), 0);
        drop(held_composition);

        // Consume the one message's work. A duplicate scheduler invocation
        // above did not create a second focus-reconcile bit/message.
        let dispatched = {
            let mut deferred = service.deferred.borrow_mut();
            let work = deferred
                .dispatch
                .take_for_dispatch(false)
                .expect("the one deferred retry message");
            assert!(work.focus_reconcile);
            assert!(!deferred.dispatch.work.focus_reconcile);
            assert!(!deferred.dispatch.work.has_work());
            work
        };
        assert!(dispatched.end_candidates);
        service.end_candidates();
        service.dispatch_focus_reconciliation();

        let composition = service.composition.borrow();
        assert!(composition.known);
        assert!(composition.text.is_empty());
        assert!(composition.handle.is_none());
        assert!(composition.context.is_none());
        drop(composition);
        let deferred = service.deferred.borrow();
        assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
        assert!(!deferred.focus_reconciliation_required);
        assert!(!deferred.dispatch.work.focus_loss);
        assert!(!deferred.dispatch.work.focus_reconcile);
        drop(deferred);
        let writes = service.writes.borrow();
        assert_eq!(writes.pending_len(), 0);
        assert_eq!(writes.committed_visible(), VisibleState::empty());
        assert_eq!(writes.tail_visible(), VisibleState::empty());
        drop(writes);
        // The engine/UI terminal path is idempotently invoked both before and
        // after the retry, so an unavailable engine cannot be reused here.
        assert!(!service.engine.borrow().is_connected());
        assert!(!service.input_blocked());
    }

    #[test]
    fn borrowed_focus_gain_projection_replaces_pre_engine_finalizer_with_one_retry() {
        let service = TextService::new();
        {
            let mut writes = service.writes.borrow_mut();
            assert!(writes.activate().is_empty());
            assert!(writes.observe_context(ContextId(31_339)).is_empty());
        }
        {
            let mut composition = service.composition.borrow_mut();
            composition.text =
                "visible while focus gain re-enters a composition callback".to_owned();
        }
        {
            let mut deferred = service.deferred.borrow_mut();
            deferred.focus_finalization = FocusFinalizationPhase::DeferredQueued;
            deferred.dispatch.work.focus_loss = true;
            // Model the one live hidden-window message. The focus-gain path
            // must reuse this owner rather than append a second retry.
            deferred.dispatch.posted = true;
        }

        // A focus-gain callback re-entering while another composition callback
        // holds this RefCell cannot prove which projection is visible. It must
        // therefore revoke the *pre-engine* focus-loss request before returning
        // instead of leaving it free to call Engine::commit later.
        let held_composition = service.composition.borrow_mut();
        service.resume_after_focus_gain();
        // A second focus notification still has only the existing one-bit
        // reconciliation owner; it cannot post or terminalize a duplicate.
        service.resume_after_focus_gain();
        {
            let deferred = service.deferred.borrow();
            assert_eq!(
                deferred.focus_finalization,
                FocusFinalizationPhase::ReconciliationQueued
            );
            assert!(deferred.focus_reconciliation_required);
            assert!(!deferred.dispatch.work.focus_loss);
            assert!(deferred.dispatch.work.focus_reconcile);
            assert!(deferred.dispatch.work.end_candidates);
            assert!(deferred.dispatch.posted);
        }
        assert!(!service.focus_gain_reconciliation_pending.get());
        assert!(service.input_blocked());
        drop(held_composition);

        let dispatched = {
            let mut deferred = service.deferred.borrow_mut();
            let work = deferred
                .dispatch
                .take_for_dispatch(false)
                .expect("the existing hidden-window message owns the retry");
            assert!(!work.focus_loss);
            assert!(work.focus_reconcile);
            assert!(!deferred.dispatch.work.focus_reconcile);
            assert!(!deferred.dispatch.work.has_work());
            work
        };
        // The original focus-loss finalizer no longer has a state transition it
        // can begin, so it cannot commit the engine after focus has returned.
        assert!(!service.begin_focus_finalization());
        assert!(dispatched.end_candidates);
        service.end_candidates();
        service.dispatch_focus_reconciliation();

        let composition = service.composition.borrow();
        assert!(composition.known);
        assert!(composition.text.is_empty());
        assert!(composition.handle.is_none());
        assert!(composition.context.is_none());
        drop(composition);
        let deferred = service.deferred.borrow();
        assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
        assert!(!deferred.focus_reconciliation_required);
        assert!(!deferred.dispatch.work.focus_loss);
        assert!(!deferred.dispatch.work.focus_reconcile);
        drop(deferred);
        let writes = service.writes.borrow();
        assert_eq!(writes.pending_len(), 0);
        assert_eq!(writes.committed_visible(), VisibleState::empty());
        assert_eq!(writes.tail_visible(), VisibleState::empty());
        drop(writes);
        assert!(!service.input_blocked());
    }

    #[test]
    fn unpostable_focus_reconciliation_becomes_an_explicit_lifecycle_owner() {
        let service = TextService::new();
        {
            let mut deferred = service.deferred.borrow_mut();
            deferred.focus_finalization = FocusFinalizationPhase::EngineCommitStarted;
        }

        // `TextService::new` has no hidden window. A failed PostMessage path
        // must never leave `ReconciliationQueued` without a message owner.
        service.queue_focus_reconciliation();

        let deferred = service.deferred.borrow();
        assert_eq!(
            deferred.focus_finalization,
            FocusFinalizationPhase::ReconciliationAwaitingLifecycle
        );
        assert!(deferred.focus_reconciliation_required);
        assert!(!deferred.dispatch.work.focus_reconcile);
        assert!(!deferred.dispatch.posted);
        drop(deferred);
        assert!(service.input_blocked());
    }

    #[test]
    fn detach_is_the_terminal_owner_after_destroying_a_reconciliation_message() {
        let service = TextService::new();
        {
            let mut composition = service.composition.borrow_mut();
            composition.text = "projection awaiting lifecycle cleanup".to_owned();
        }
        {
            let mut deferred = service.deferred.borrow_mut();
            deferred.focus_finalization = FocusFinalizationPhase::ReconciliationQueued;
            deferred.focus_reconciliation_required = true;
            deferred.dispatch.work.focus_reconcile = true;
        }

        // `destroy_deferred_window` first transfers the lost message to the
        // lifecycle phase; `detach` then owns the document-free retirement and
        // reports a failure if that final attempt cannot acquire the state.
        service
            .detach()
            .expect("lifecycle cleanup retires the projection");

        let composition = service.composition.borrow();
        assert!(composition.known);
        assert!(composition.text.is_empty());
        assert!(composition.handle.is_none());
        drop(composition);
        let deferred = service.deferred.borrow();
        assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
        assert!(!deferred.focus_reconciliation_required);
        assert!(!deferred.dispatch.work.focus_reconcile);
        drop(deferred);
        assert!(!service.input_blocked());
    }

    #[test]
    fn cancelled_accepted_output_forces_pre_dispatch_focus_reconciliation() {
        let reservation_only = Completion {
            outcome: TerminalOutcome::Cancelled(CancelReason::FocusChanged),
            payload: None::<()>,
            ui_lease: None,
        };
        let accepted_output = Completion {
            outcome: TerminalOutcome::Cancelled(CancelReason::FocusChanged),
            payload: Some(()),
            ui_lease: None,
        };
        assert!(!cancelled_outputs_require_focus_reconciliation(&[
            reservation_only
        ]));
        assert!(cancelled_outputs_require_focus_reconciliation(&[
            accepted_output
        ]));

        let service = TextService::new();
        {
            let mut state = service.composition.borrow_mut();
            // The projection is locally known, but the accepted output that
            // was cancelled at the focus boundary may already have advanced
            // the engine without reaching the document.
            state.text = "known only on this side".to_owned();
        }
        {
            let mut deferred = service.deferred.borrow_mut();
            deferred.focus_finalization = FocusFinalizationPhase::DeferredQueued;
            deferred.dispatch.work.focus_loss = true;
        }
        service.require_focus_reconciliation();

        service.resume_after_focus_gain();

        let state = service.composition.borrow();
        assert!(state.known);
        assert!(state.text.is_empty());
        assert!(state.handle.is_none());
        assert!(state.context.is_none());
        drop(state);
        let deferred = service.deferred.borrow();
        assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
        assert!(!deferred.focus_reconciliation_required);
        assert!(!deferred.dispatch.work.focus_loss);
        drop(deferred);
        assert!(!service.input_blocked());
    }

    #[test]
    fn focus_change_without_an_inflight_mutation_keeps_known_text_for_finalization() {
        let service = TextService::new();
        {
            let mut state = service.composition.borrow_mut();
            state.text = "known visible preedit".to_owned();
        }

        service.invalidate_for_focus_change();

        let state = service.composition.borrow();
        assert!(state.known);
        assert_eq!(state.text, "known visible preedit");
    }

    #[test]
    fn candidate_operation_holds_reentrant_deferred_work_until_controller_restore() {
        let mut dispatch = DeferredDispatchState {
            posted: true,
            work: DeferredWork {
                write: true,
                layout: true,
                layout_abandon: true,
                focus_loss: false,
                focus_reconcile: false,
                end_candidates: true,
                candidates: Some("newer candidates"),
            },
        };

        // The nested hidden-window message consumes its post but leaves every
        // kind of work in place while the outer controller is out of its slot.
        assert!(dispatch.take_for_dispatch(true).is_none());
        assert!(!dispatch.posted);
        assert!(dispatch.work.write);
        assert!(dispatch.work.layout);
        assert!(dispatch.work.layout_abandon);
        assert!(dispatch.work.end_candidates);
        assert_eq!(dispatch.work.candidates, Some("newer candidates"));

        // Once the controller is restored, exactly one replacement post is
        // needed. The next dispatch drains the retained work normally.
        assert!(dispatch.needs_repost_after_candidate_operation());
        dispatch.posted = true;
        let drained = dispatch.take_for_dispatch(false).expect("deferred work");
        assert!(drained.write);
        assert!(drained.layout);
        assert!(drained.layout_abandon);
        assert!(drained.end_candidates);
        assert_eq!(drained.candidates, Some("newer candidates"));
        assert!(!dispatch.work.has_work());
    }

    #[test]
    fn candidate_operation_does_not_post_again_when_work_already_has_a_message() {
        let dispatch: DeferredDispatchState<()> = DeferredDispatchState {
            posted: true,
            work: DeferredWork {
                write: true,
                ..Default::default()
            },
        };

        assert!(!dispatch.needs_repost_after_candidate_operation());
        assert!(dispatch.posted);
        assert!(dispatch.work.write);
    }

    #[derive(Debug)]
    struct CandidateTeardownProbe {
        active: Option<&'static str>,
        old_end_count: usize,
        newer_end_count: usize,
    }

    impl CandidateTeardownProbe {
        fn end_old(&mut self) {
            assert_eq!(self.active.take(), Some("old"));
            self.old_end_count += 1;
        }

        fn install_and_end_newer(&mut self, candidate: &'static str) {
            assert!(self.active.replace(candidate).is_none());
            assert_eq!(self.active.take(), Some("newer"));
            self.newer_end_count += 1;
        }
    }

    fn assert_newer_candidate_survives_teardown_reentry(
        reenter_during_unadvise: bool,
    ) -> CandidateTeardownProbe {
        let dispatch = RefCell::new(DeferredDispatchState {
            posted: true,
            work: DeferredWork {
                candidates: Some("newer"),
                ..Default::default()
            },
        });
        let mut controller = CandidateTeardownProbe {
            active: Some("old"),
            old_end_count: 0,
            newer_end_count: 0,
        };
        let nested_dispatch = || {
            assert!(dispatch.borrow_mut().take_for_dispatch(true).is_none());
            assert_eq!(dispatch.borrow().work.candidates, Some("newer"));
        };

        run_candidate_teardown_host_calls(
            Some(()),
            &mut controller,
            |_| {
                if reenter_during_unadvise {
                    nested_dispatch();
                }
            },
            |controller| {
                controller.end_old();
                if !reenter_during_unadvise {
                    nested_dispatch();
                }
            },
        );

        let mut dispatch = dispatch.borrow_mut();
        assert!(dispatch.needs_repost_after_candidate_operation());
        dispatch.posted = true;
        let newer = dispatch
            .take_for_dispatch(false)
            .expect("retained newer candidate")
            .candidates
            .expect("newer candidate payload");
        drop(dispatch);
        controller.install_and_end_newer(newer);
        controller
    }

    #[test]
    fn newer_candidate_survives_reentry_during_subscription_unadvise() {
        let controller = assert_newer_candidate_survives_teardown_reentry(true);
        assert_eq!(controller.old_end_count, 1);
        assert_eq!(controller.newer_end_count, 1);
        assert!(controller.active.is_none());
    }

    #[test]
    fn newer_candidate_survives_reentry_during_end_ui_element() {
        let controller = assert_newer_candidate_survives_teardown_reentry(false);
        assert_eq!(controller.old_end_count, 1);
        assert_eq!(controller.newer_end_count, 1);
        assert!(controller.active.is_none());
    }

    #[test]
    fn stale_geometry_is_abandoned_and_a_new_lease_starts_from_idle() {
        let mut layout = LayoutState {
            phase: GeometryPhase::QueryQueued,
            ..Default::default()
        };
        layout.retire_geometry_for_lease_rollover();
        assert_eq!(layout.phase, GeometryPhase::Idle);
    }

    #[test]
    fn matching_layout_abandonment_has_an_explicit_terminal_phase() {
        let claim = layout_claim(ContextId(41_001));
        let mut phase = GeometryPhase::QueryQueued;

        abandon_matching_geometry(&mut phase, Some(claim), claim);

        assert_eq!(phase, GeometryPhase::Unavailable);
    }

    #[test]
    fn refused_layout_abandonment_remains_owned_until_one_later_attempt() {
        let service = TextService::new();
        let claim = layout_claim(ContextId(41_002));
        service.layout_abandon_pending.set(Some(claim));

        let borrowed = service.layout.borrow_mut();
        assert!(service.settle_pending_layout_abandon().is_err());
        assert_eq!(service.layout_abandon_pending.get(), Some(claim));
        drop(borrowed);

        assert_eq!(
            service
                .settle_pending_layout_abandon()
                .expect("later lifecycle attempt"),
            Some(false)
        );
        assert_eq!(service.layout_abandon_pending.get(), None);
        assert_eq!(
            service
                .settle_pending_layout_abandon()
                .expect("no duplicate attempt"),
            None
        );
    }

    /// The engine emits the normalized kana and the romaji still being
    /// typed as separate segments; the user sees one run of text.
    #[test]
    fn segments_are_shown_as_one_run() {
        assert_eq!(visible_text(&preedit(&["か", "t"])), "かt");
        assert_eq!(visible_text(&preedit(&[])), "");
    }

    #[test]
    fn a_preedit_is_planned_without_mutating_composition_state() {
        let service = TextService::new();
        let plan = service.plan(&output(None, Some(&["か"]))).expect("plan");
        assert!(
            matches!(plan.updates.as_slice(), [Update::Show(segments)] if segments_text(segments) == "か")
        );
        assert_eq!(plan.before, VisibleState::empty());
        assert_eq!(plan.after, state("か", true));
        assert_eq!(
            service.composition_projection().expect("projection"),
            VisibleState::empty()
        );
    }

    /// The engine's per-segment `UnderlineKind` -- raw input, a converted
    /// clause, or the one clause currently focused -- has to survive the
    /// trip from `Output` into the `Update::Show` the document write
    /// actually applies. Losing it here (e.g. by flattening to a string
    /// before planning, as this function used to) would draw every clause
    /// with the same underline no matter how the engine tagged it.
    #[test]
    fn plan_carries_each_segments_underline_kind_into_the_show_update() {
        let mut answer = output(None, None);
        answer.preedit = Some(preedit_with_underlines(&[
            ("わたし", UnderlineKind::Converted),
            ("は", UnderlineKind::Focused),
            ("にほん", UnderlineKind::Raw),
        ]));

        let plan = plan_from_visible(VisibleState::empty(), &answer).expect("plan");

        match plan.updates.as_slice() {
            [Update::Show(segments)] => {
                let [converted, focused, raw] = segments.as_slice() else {
                    panic!("expected three preedit segments, got {segments:?}");
                };
                assert_eq!(converted.text, "わたし");
                assert_eq!(converted.underline, UnderlineKind::Converted);
                assert_eq!(focused.text, "は");
                assert_eq!(focused.underline, UnderlineKind::Focused);
                assert_eq!(raw.text, "にほん");
                assert_eq!(raw.underline, UnderlineKind::Raw);
            }
            other => panic!("expected one multi-segment show, got {other:?}"),
        }
        assert_eq!(plan.after, state("わたしはにほん", true));
    }

    /// Enter mid-word: the converted text is committed and the tail the
    /// engine is still working on stays underlined. The commit has to come
    /// first, because the new composition starts where its text ended.
    #[test]
    fn a_commit_with_a_tail_ends_one_composition_and_opens_another() {
        let plan = plan_from_visible(state("かな", true), &output(Some("漢字"), Some(&["か"])))
            .expect("plan");
        match plan.updates.as_slice() {
            [Update::Commit(committed), Update::Show(shown)] => {
                assert_eq!(committed, "漢字");
                assert_eq!(segments_text(shown), "か");
            }
            other => panic!("expected a commit then a show, got {other:?}"),
        }
        assert_eq!(plan.before, state("かな", true));
        assert_eq!(plan.after, state("か", true));
    }

    /// Escape: nothing committed, nothing left to show, and something on
    /// screen that has to come off it.
    #[test]
    fn an_empty_answer_discards_what_is_on_screen() {
        let plan = plan_from_visible(state("か", true), &output(None, None)).expect("plan");
        assert!(matches!(plan.updates.as_slice(), [Update::Discard]));
        assert_eq!(plan.after, VisibleState::empty());
    }

    /// The idle case, and by far the most common one: a key that touched no
    /// composition must not cost the document an edit session.
    #[test]
    fn an_empty_answer_with_nothing_on_screen_does_nothing() {
        let plan = plan_from_visible(VisibleState::empty(), &output(None, None)).expect("plan");
        assert!(plan.updates.is_empty());
        assert_eq!(plan.after, VisibleState::empty());
    }

    /// A commit that empties the preedit must not also emit a discard: the
    /// commit already closed the composition, and discarding afterwards
    /// would reopen and clear one.
    #[test]
    fn a_plain_commit_is_a_single_operation() {
        let plan = plan_from_visible(state("か", true), &output(Some("か"), None)).expect("plan");
        assert!(matches!(plan.updates.as_slice(), [Update::Commit(text)] if text == "か"));
        assert_eq!(plan.after, VisibleState::empty());
    }

    #[test]
    fn idle_space_commit_does_not_replace_a_live_reading() {
        let before = state("にほんごにゅうりょくのてすと", true);
        let plan =
            plan_from_visible(before.clone(), &output(Some("\u{3000}"), None)).expect("plan");
        assert!(plan.updates.is_empty());
        assert_eq!(plan.after, before);
        let ascii =
            plan_from_visible(state("にほんご", true), &output(Some(" "), None)).expect("plan");
        assert!(ascii.updates.is_empty());
        assert_eq!(ascii.after.text, "にほんご");
    }

    #[test]
    fn commit_undo_deletes_the_committed_run_before_restoring_preedit() {
        let mut restored = output(None, Some(&["かな"]));
        restored.delete_before = "加奈".to_owned();

        let plan = plan_from_visible(VisibleState::empty(), &restored).expect("commit undo plan");

        assert!(matches!(
            plan.updates.as_slice(),
            [Update::DeleteBefore(text), Update::Show(shown)]
                if text == "加奈" && segments_text(shown) == "かな"
        ));
        assert_eq!(plan.after, state("かな", true));
    }

    #[test]
    fn commit_undo_malformed_is_rejected_without_mutating_visible_state() {
        let composing = state("編集中", true);
        let mut while_composing = output(None, Some(&["復元"]));
        while_composing.delete_before = "行った".to_owned();
        assert!(plan_from_visible(composing.clone(), &while_composing).is_err());
        assert_eq!(composing, state("編集中", true));

        let mut with_commit = output(Some("競合"), None);
        with_commit.delete_before = "加奈".to_owned();
        assert!(plan_from_visible(VisibleState::empty(), &with_commit).is_err());
    }

    #[test]
    fn shift_latin_backspace_retype_plans_chain_to_aiueo_not_aiuoeo() {
        let typed = plan_from_visible(VisibleState::empty(), &output(None, Some(&["AIUEO"])))
            .expect("type AIUEO");
        assert!(
            matches!(typed.updates.as_slice(), [Update::Show(segments)] if segments_text(segments) == "AIUEO")
        );
        let erased = plan_from_visible(typed.after.clone(), &output(None, Some(&["AIUE"])))
            .expect("Shift+Backspace");
        assert!(
            matches!(erased.updates.as_slice(), [Update::Show(segments)] if segments_text(segments) == "AIUE")
        );
        let retyped = plan_from_visible(erased.after.clone(), &output(None, Some(&["AIUEO"])))
            .expect("retype O");
        assert!(
            matches!(retyped.updates.as_slice(), [Update::Show(segments)] if segments_text(segments) == "AIUEO")
        );
        assert_ne!(retyped.after.text, "AIUOEO");
        assert_eq!(retyped.after, state("AIUEO", true));
    }

    #[test]
    fn plans_chain_through_explicit_projections() {
        let first = plan_from_visible(VisibleState::empty(), &output(None, Some(&["かん"])))
            .expect("first plan");
        let second = plan_from_visible(first.after.clone(), &output(Some("かん"), None))
            .expect("second plan");
        assert_eq!(first.before, VisibleState::empty());
        assert_eq!(first.after, state("かん", true));
        assert!(matches!(second.updates.as_slice(), [Update::Commit(text)] if text == "かん"));
        assert_eq!(second.after, VisibleState::empty());
    }

    #[test]
    fn function_provider_exposes_the_reconversion_interface() {
        let service: IUnknown = TextService::new().into();
        let provider: ITfFunctionProvider = service.cast().expect("function provider");
        // SAFETY: both GUID pointers are live for the call and the returned
        // object is retained by the interface wrapper.
        let function = unsafe {
            provider
                .GetFunction(&GUID::from_u128(0), &ITfFnReconversion::IID)
                .expect("reconversion function")
        };
        let _: ITfFnReconversion = function.cast().expect("ITfFnReconversion");
        assert_eq!(
            // SAFETY: `provider` is a live COM interface and the call has no
            // borrowed output pointer beyond its returned value.
            unsafe { provider.GetType().expect("type") },
            CLSID_SAKURA_TSF
        );
        assert_eq!(
            // SAFETY: `provider` is live and the returned BSTR is owned by its
            // interface wrapper.
            unsafe { provider.GetDescription().expect("description") }.to_string(),
            TEXT_SERVICE_DESCRIPTION
        );
    }

    #[test]
    fn input_mode_item_exposes_a_split_menu_button_to_tsf() {
        let service: IUnknown = TextService::new().into();
        let item: ITfLangBarItem = service.cast().expect("language-bar item");
        let mut info = TF_LANGBARITEMINFO::default();

        // SAFETY: `item` is a live in-process COM object and `info` is a
        // writable output structure for the duration of this call.
        unsafe { item.GetInfo(&mut info).expect("language-bar info") };

        assert_eq!(info.guidItem, GUID_LBI_INPUTMODE);
        assert_ne!(info.dwStyle & TF_LBI_STYLE_BTN_BUTTON, 0);
        assert_ne!(info.dwStyle & TF_LBI_STYLE_BTN_MENU, 0);
    }
}
