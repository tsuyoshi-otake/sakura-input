//! The DLL's end of the conversation with `sakura_engine.exe`.
//!
//! Milestone 6 of PLAN.md: the decision about what a keystroke means stops
//! being made here and starts being made in the engine. What this module
//! owns is not that decision but the *cost* of asking for it, because the
//! asking happens on the host application's keystroke thread, inside
//! `ITfKeyEventSink::OnKeyDown`, with the user's editor blocked until it
//! returns.
//!
//! # Everything here is a deadline
//!
//! DESIGN 4.3 gives a keystroke 50 ms. That budget covers the round trip
//! and nothing else, so connecting — the one part that can take real time,
//! since a busy pipe makes `WaitNamedPipeW` wait — gets its own, much
//! smaller one. A reconnection that cannot finish inside
//! [`RECONNECT_BUDGET`] is not worth having: the engine is either not
//! running or not answering, and both are cases where the right move is to
//! give the key back to the application and try again later.
//!
//! # What "later" means
//!
//! Later is [`RETRY_INTERVAL`], not "next keystroke". A machine where the
//! engine failed to start would otherwise pay a failed connect on every
//! single key, turning one broken component into a typing experience worse
//! than having no IME at all. Between attempts the DLL is a pass-through,
//! which is what the user wants from an IME that cannot reach its brain.
//!
//! # Why a timeout does not drop the connection
//!
//! A timed-out request is still in flight, and the engine's session state
//! is keyed to the connection: reconnecting would throw away the user's
//! composition to fix a hiccup. `sakura_ipc::Client` already discards the
//! late reply when it arrives (its request ids exist for exactly this), so
//! the connection is kept and only the keystroke is lost. What is *not*
//! kept is the assumption that both ends still agree — see
//! [`Link::resync`].

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use sakura_ipc::diagnostics::{self, TimeoutOperation};
use sakura_ipc::{Client, Fault};
use sakura_proto::{
    AiTextOperation, AiTextStatus, ErrorCode, InputScope, KeyInput, Mode, Output, Request,
    Response, ScreenRect, SessionId, UndoCommitOutcome, PROTOCOL_VERSION,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

/// DESIGN 4.3's per-keystroke budget. Exceeding it is a dropped keystroke;
/// not having it at all is a frozen application.
const KEY_BUDGET: Duration = Duration::from_millis(50);

/// Placement is cosmetic and must never consume the whole keystroke budget.
/// A missed update leaves the popup hidden or at its last valid rectangle;
/// the next TSF layout notification supplies another bounded opportunity.
const UI_BUDGET: Duration = Duration::from_millis(10);

/// The whole cost of rebuilding a broken link — connect, `Hello` and
/// `CreateSession` together, not each. Deliberately no larger than a
/// keystroke budget: the reconnect happens *on* a keystroke.
const RECONNECT_BUDGET: Duration = Duration::from_millis(50);

/// How long the DLL stays a pass-through after a failed attempt.
///
/// Long enough that a machine with no engine costs nothing per key, short
/// enough that a user who starts the engine by hand sees it work without
/// wondering whether they have to restart their editor.
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// What asking the engine produced.
#[derive(Debug)]
// Keeping `Output` inline avoids a heap allocation on every accepted
// keystroke; the zero-sized terminal variants are deliberately asymmetric.
#[allow(clippy::large_enum_variant)]
pub enum Answer {
    /// The engine answered inside the budget. Whether it wants the key is
    /// [`Output::consumed`].
    Ready(Output),
    /// The engine owns an unfinished transaction for this session. The caller
    /// must consume the key locally until the queued host-side operation sends
    /// its explicit terminal outcome; handing it to the document would race
    /// the exact-text undo.
    Busy,
    /// This request never reached the engine: it failed to encode on this
    /// side of the wire (e.g. a reconversion selection too large to fit
    /// the protocol). The peer was never contacted and never misbehaved,
    /// so only this operation is refused — the link, the session, and any
    /// other work already in flight are untouched.
    Rejected,
    /// No engine, no answer in time, or an answer that made no sense. The
    /// key belongs to the application, and any composition already on
    /// screen has to be finalized rather than left hanging — see the
    /// crash-resilience criterion in PLAN.md Phase 1.
    Unavailable,
}

/// The one focused engine session's input-mode state, used by the TSF
/// `GUID_LBI_INPUTMODE` item. It is deliberately cached at this boundary: the
/// language-bar callbacks must never guess a profile default or issue a new
/// connection attempt merely to paint an icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputModeStatus {
    pub mode: Mode,
    /// Only a positively-classified ordinary-text context may change modes
    /// through the menu. Password-like and unclassified fields keep the
    /// visible A/あ status but expose no mutation path.
    pub can_change: bool,
    /// A one-shot undo for a change made from this menu, not for arbitrary
    /// keyboard mode changes.
    pub can_restore: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AiTextRecord {
    pub operation: AiTextOperation,
    pub status: AiTextStatus,
    pub source: String,
    pub result: String,
    pub model: String,
    pub provider: String,
    pub style: String,
    pub error_code: String,
    pub latency_ms: u64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub attempts: u32,
    pub test_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AiTextResult {
    pub status: AiTextStatus,
    pub result: String,
    pub model: String,
    pub provider: String,
    pub style: String,
    pub error_code: String,
    pub latency_ms: u64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AiTextPoll {
    Pending,
    Complete(AiTextResult),
    Missing,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateCommitPoll {
    Pending { revision: u64, candidate_index: u16 },
    None,
    Unavailable,
}

/// One connection to the engine, plus the policy for not having one.
#[derive(Debug, Default)]
pub struct Engine {
    link: Option<Link>,
    /// When the next connection attempt is allowed. `None` means now.
    blocked_until: Option<Instant>,
}

#[derive(Debug)]
struct Link {
    client: Client,
    session: SessionId,
    /// Exact profile-resolved mode supplied with `SessionCreated`, then kept
    /// current from engine output and explicit menu responses.
    mode: Mode,
    /// The pre-menu mode for the optional one-shot “restore previous input
    /// mode” command. Keyboard actions and scope changes clear it so the menu
    /// never resurrects a stale context's state.
    menu_mode_restore: Option<Mode>,
    /// The mode that was active before a sensitive scope temporarily forced
    /// direct input. This mirrors the engine's scope transition locally so the
    /// status item remains accurate before the next key produces output.
    mode_before_sensitive: Option<Mode>,
    /// The last scope accepted by this engine session. A reconnect starts
    /// unclassified, so the first key on the new link must publish its scope
    /// again before it can be persisted by developer mode.
    input_scope: Option<InputScope>,
    /// Set when a request timed out. The engine may or may not have
    /// applied that keystroke, so the two ends can no longer be assumed to
    /// agree about what is being composed.
    desynchronized: bool,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// An engine already connected to a named pipe, for tests that need a
    /// scripted peer rather than the real one.
    #[cfg(test)]
    pub(crate) fn attached_to(name: &str) -> Self {
        Self {
            link: connect_to(name),
            blocked_until: None,
        }
    }

    /// Opens the connection ahead of the first keystroke.
    ///
    /// Called from activation, where a slow engine costs a moment of
    /// window setup instead of a moment of typing. Failure is not an
    /// error: the first keystroke will try again.
    pub fn warm_up(&mut self) {
        let _ = self.link();
    }

    /// Asks the engine what a keystroke means.
    pub fn send_key(&mut self, key: KeyInput) -> Answer {
        let session = match self.link() {
            Some(link) => link.session,
            None => return Answer::Unavailable,
        };
        self.request(&Request::SendKey { session, key })
    }

    pub(crate) fn apply_ai_composition(&mut self, result: String) -> Answer {
        let session = match self.link() {
            Some(link) => link.session,
            None => return Answer::Unavailable,
        };
        self.request(&Request::ApplyAiComposition { session, result })
    }

    pub(crate) fn record_ai_text(&mut self, record: &AiTextRecord) -> bool {
        let Some(link) = self.link.as_mut() else {
            return false;
        };
        if link.input_scope != Some(InputScope::Normal) {
            return false;
        }
        let request = Request::RecordAiText {
            session: link.session,
            operation: record.operation,
            status: record.status,
            source: record.source.clone(),
            result: record.result.clone(),
            model: record.model.clone(),
            provider: record.provider.clone(),
            style: record.style.clone(),
            error_code: record.error_code.clone(),
            latency_ms: record.latency_ms,
            input_tokens: record.input_tokens,
            output_tokens: record.output_tokens,
            cached_tokens: record.cached_tokens,
            attempts: record.attempts,
            test_only: record.test_only,
        };
        match link.client.call(&request, KEY_BUDGET) {
            Ok(Response::Ok) => true,
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Administration);
                link.desynchronized = true;
                false
            }
            Ok(_) | Err(_) => {
                self.drop_link();
                false
            }
        }
    }

    pub(crate) fn start_ai_text(
        &mut self,
        operation: AiTextOperation,
        text: String,
    ) -> core::result::Result<u64, ErrorCode> {
        let link = self.link().ok_or(ErrorCode::Internal)?;
        if link.input_scope != Some(InputScope::Normal) {
            return Err(ErrorCode::Busy);
        }
        match link.client.call(
            &Request::StartAiText {
                session: link.session,
                operation,
                text,
            },
            KEY_BUDGET,
        ) {
            Ok(Response::AiTextStarted { job }) => Ok(job),
            Ok(Response::Error(code)) => Err(code),
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Administration);
                link.desynchronized = true;
                Err(ErrorCode::Internal)
            }
            Ok(_) | Err(_) => {
                self.drop_link();
                Err(ErrorCode::Internal)
            }
        }
    }

    pub(crate) fn poll_ai_text(&mut self, job: u64) -> AiTextPoll {
        let Some(link) = self.link.as_mut() else {
            return AiTextPoll::Unavailable;
        };
        match link.client.call(
            &Request::PollAiText {
                session: link.session,
                job,
            },
            UI_BUDGET,
        ) {
            Ok(Response::AiTextPending { job: returned }) if returned == job => AiTextPoll::Pending,
            Ok(Response::AiTextResult {
                job: returned,
                status,
                result,
                model,
                provider,
                style,
                error_code,
                latency_ms,
                input_tokens,
                output_tokens,
                cached_tokens,
                attempts,
            }) if returned == job => AiTextPoll::Complete(AiTextResult {
                status,
                result,
                model,
                provider,
                style,
                error_code,
                latency_ms,
                input_tokens,
                output_tokens,
                cached_tokens,
                attempts,
            }),
            Ok(Response::Error(ErrorCode::Malformed | ErrorCode::UnknownSession)) => {
                AiTextPoll::Missing
            }
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Administration);
                AiTextPoll::Pending
            }
            Ok(_) | Err(_) => {
                self.drop_link();
                AiTextPoll::Unavailable
            }
        }
    }

    pub(crate) fn cancel_ai_text(&mut self, job: u64) -> bool {
        let Some(link) = self.link.as_mut() else {
            return false;
        };
        match link.client.call(
            &Request::CancelAiText {
                session: link.session,
                job,
            },
            UI_BUDGET,
        ) {
            Ok(Response::Ok) => true,
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Administration);
                false
            }
            Ok(Response::Error(_)) => false,
            Ok(_) | Err(_) => {
                self.drop_link();
                false
            }
        }
    }

    /// Returns the cached status of the active engine session without opening
    /// or reconnecting a pipe. The caller uses `None` to hide the language-bar
    /// item rather than draw a guessed state.
    pub(crate) fn input_mode_status(&self) -> Option<InputModeStatus> {
        self.link.as_ref().map(|link| InputModeStatus {
            mode: link.mode,
            can_change: link.input_scope == Some(InputScope::Normal),
            can_restore: link.menu_mode_restore.is_some()
                && link.input_scope == Some(InputScope::Normal),
        })
    }

    /// Selects an idle session's persistent mode from the language-bar menu.
    /// This request is explicitly not a synthetic key: the engine rejects it
    /// before any document mutation when the session is composing or its scope
    /// is not known ordinary text.
    pub(crate) fn set_input_mode(&mut self, requested: Mode) -> bool {
        let Some(link) = self.link.as_mut() else {
            return false;
        };
        // A menu click may have been queued just before focus moved. Keep the
        // frontend boundary fail-closed as well as relying on the engine's
        // authoritative scope/composition validation.
        if link.input_scope != Some(InputScope::Normal) {
            return false;
        }
        let session = link.session;
        let prior = link.mode;
        match link.client.call(
            &Request::SetMode {
                session,
                mode: requested,
            },
            KEY_BUDGET,
        ) {
            Ok(Response::InputMode { mode }) => {
                link.mode = mode;
                link.menu_mode_restore = (mode != prior).then_some(prior);
                true
            }
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Administration);
                link.desynchronized = true;
                false
            }
            Ok(Response::Error(ErrorCode::Busy)) => false,
            Ok(_) | Err(_) => {
                self.drop_link();
                false
            }
        }
    }

    /// Restores the mode before the last successful menu-driven change. A
    /// successful restore consumes the record; it is an undo, not a toggle.
    pub(crate) fn restore_input_mode(&mut self) -> bool {
        let Some(mode) = self.link.as_ref().and_then(|link| link.menu_mode_restore) else {
            return false;
        };
        if !self.set_input_mode(mode) {
            return false;
        }
        if let Some(link) = self.link.as_mut() {
            link.menu_mode_restore = None;
        }
        true
    }

    /// Evaluates a test-only key against the supplied host scope without
    /// publishing that scope to the live engine session. The engine applies
    /// the transition only to a fixed-capacity Probe clone, so OnTestKeyDown
    /// cannot reset live composition, clear live prediction state, or update
    /// this link's applied-scope cache.
    #[cfg(test)]
    pub fn probe_key(&mut self, scope: InputScope, key: KeyInput) -> Answer {
        self.probe_key_for_context(scope, key, false)
    }

    /// Evaluates a key against a throwaway session for a replacement TSF
    /// context. The engine receives the explicit fresh-context bit; this
    /// method never publishes the scope or updates the live link cache.
    pub fn probe_key_for_context(
        &mut self,
        scope: InputScope,
        mut key: KeyInput,
        fresh_context: bool,
    ) -> Answer {
        let session = match self.link() {
            Some(link) => link.session,
            None => return Answer::Unavailable,
        };
        key.test_only = true;
        self.request(&Request::ProbeKey {
            session,
            scope,
            fresh_context,
            key,
        })
    }

    /// Publishes the focused field's classification before a key is sent.
    /// Repeating the same scope is local-only; a reconnect or a changed field
    /// sends one bounded administration request. Failure is fail-closed: the
    /// caller must give the key back to the host rather than let an
    /// unclassified key reach the engine.
    pub fn set_input_scope(&mut self, scope: InputScope) -> bool {
        let Some(link) = self.link() else {
            return false;
        };
        if link.input_scope == Some(scope) {
            return true;
        }
        let request = Request::SetInputScope {
            session: link.session,
            scope,
        };
        match link.client.call(&request, KEY_BUDGET) {
            Ok(Response::Ok) => {
                update_mode_for_scope(link, scope);
                link.input_scope = Some(scope);
                true
            }
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Administration);
                link.desynchronized = true;
                link.input_scope = None;
                false
            }
            Ok(_) | Err(_) => {
                self.drop_link();
                false
            }
        }
    }

    /// Retires document-relative context while preserving the explicit input
    /// mode. The caller uses this only after exact TSF range validation fails;
    /// an uncertain outcome desynchronizes or drops the link before reuse.
    pub(crate) fn reset_document_context(&mut self) -> bool {
        let Some(link) = self.link() else {
            return false;
        };
        let request = Request::ResetDocumentContext {
            session: link.session,
        };
        match link.client.call(&request, KEY_BUDGET) {
            Ok(Response::Ok) => {
                link.menu_mode_restore = None;
                true
            }
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Administration);
                link.desynchronized = true;
                false
            }
            Ok(_) | Err(_) => {
                self.drop_link();
                false
            }
        }
    }

    /// Asks the engine to finalize whatever it is composing.
    ///
    /// Used when the document is about to stop being ours — focus loss —
    /// so the engine's idea of the composition and the document's agree
    /// again afterwards.
    pub fn commit(&mut self) -> Answer {
        let session = match self.link() {
            Some(link) => link.session,
            None => return Answer::Unavailable,
        };
        self.request(&Request::Commit { session })
    }

    /// Reads one passive renderer click without reconnecting or mutating the
    /// engine session. The candidate UI timer is cosmetic when no link exists.
    pub(crate) fn poll_candidate_commit(&mut self) -> CandidateCommitPoll {
        let Some(link) = self.link.as_mut() else {
            return CandidateCommitPoll::Unavailable;
        };
        match link.client.call(
            &Request::PollCandidateCommit {
                session: link.session,
            },
            UI_BUDGET,
        ) {
            Ok(Response::CandidateCommitPending {
                request: Some((revision, candidate_index)),
            }) => CandidateCommitPoll::Pending {
                revision,
                candidate_index,
            },
            Ok(Response::CandidateCommitPending { request: None }) => CandidateCommitPoll::None,
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::UiPlacement);
                CandidateCommitPoll::Unavailable
            }
            Ok(_) | Err(_) => {
                self.drop_link();
                CandidateCommitPoll::Unavailable
            }
        }
    }

    pub(crate) fn commit_candidate(&mut self, revision: u64, candidate_index: u16) -> Answer {
        let session = match self.link() {
            Some(link) => link.session,
            None => return Answer::Unavailable,
        };
        self.request(&Request::CommitCandidate {
            session,
            revision,
            candidate_index,
        })
    }

    /// Asks the engine to recover the reading and candidates for text that is
    /// already committed in the host document. Preview is observational;
    /// actual reconversion replaces the engine session with the returned
    /// conversion state.
    pub fn reconvert(&mut self, text: String, preview: bool) -> Answer {
        let session = match self.link() {
            Some(link) => link.session,
            None => return Answer::Unavailable,
        };
        self.request(&Request::Reconvert {
            session,
            text,
            preview,
        })
    }

    /// Discards the engine-side composition after a document-side
    /// reconversion failure. This is intentionally observable as `bool`: an
    /// unsuccessful reset leaves the link desynchronized or closed, never
    /// silently reusable.
    pub fn revert(&mut self) -> bool {
        let Some(link) = self.link.as_mut() else {
            return false;
        };
        let session = link.session;
        match link.client.call(&Request::Revert { session }, KEY_BUDGET) {
            Ok(Response::Ok) => {
                link.desynchronized = false;
                true
            }
            // The reverted composition may have been one reconversion had
            // forced to Hiragana, in which case the engine puts the user's
            // mode back and says so here. There is no `Output` on this path
            // to carry it, so the reply is the only place the mode appears.
            Ok(Response::InputMode { mode }) => {
                link.mode = mode;
                link.desynchronized = false;
                true
            }
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Revert);
                link.desynchronized = true;
                false
            }
            Ok(_) | Err(_) => {
                self.drop_link();
                false
            }
        }
    }

    /// Completes an engine-side exact-text commit undo transaction. The
    /// frontend sends this only after the TSF journal has reached a terminal
    /// host outcome; an unsuccessful acknowledgement drops the link so a
    /// later key cannot reuse an engine session whose pending state is unknown.
    pub fn settle_undo_commit(&mut self, outcome: UndoCommitOutcome) -> bool {
        let Some(link) = self.link.as_mut() else {
            return false;
        };
        let request = Request::UndoCommit {
            session: link.session,
            outcome,
        };
        match link.client.call(&request, KEY_BUDGET) {
            Ok(Response::Ok) => true,
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Revert);
                link.desynchronized = true;
                false
            }
            Ok(_) | Err(_) => {
                self.drop_link();
                false
            }
        }
    }

    /// Best-effort publication of the candidate popup's screen geometry.
    ///
    /// This never reconnects and a timeout does not desynchronize conversion:
    /// unlike a key request it mutates no session text. A broken pipe still
    /// drops the link so the next real keystroke can rebuild it normally.
    pub fn set_ui_placement(
        &mut self,
        anchor: Option<ScreenRect>,
        document: Option<ScreenRect>,
        renderer_visible: bool,
    ) -> bool {
        let Some(link) = self.link.as_mut() else {
            return false;
        };
        let request = Request::SetUiPlacement {
            session: link.session,
            anchor,
            document,
            renderer_visible,
        };
        match link.client.call(&request, UI_BUDGET) {
            Ok(Response::Ok) => true,
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::UiPlacement);
                false
            }
            Ok(_) | Err(_) => {
                self.drop_link();
                false
            }
        }
    }

    /// Whether a connection currently exists.
    ///
    /// Only the tests ask, and only so they can tell "there is no engine on
    /// this machine" apart from "there is one and it answered" — the
    /// answer is stale the moment it is returned, so nothing on the
    /// keystroke path may branch on it.
    #[cfg(test)]
    pub fn is_connected(&self) -> bool {
        self.link.is_some()
    }

    #[cfg(test)]
    fn is_desynchronized(&self) -> bool {
        self.link.as_ref().is_some_and(|link| link.desynchronized)
    }

    fn request(&mut self, request: &Request) -> Answer {
        let Some(link) = self.link.as_mut() else {
            return Answer::Unavailable;
        };

        match link.client.call(request, KEY_BUDGET) {
            Ok(Response::Output(output)) => {
                if matches!(
                    request,
                    Request::SendKey {
                        key: KeyInput {
                            test_only: false,
                            ..
                        },
                        ..
                    }
                ) {
                    if let Some(mode) = output.mode {
                        link.mode = mode;
                        link.menu_mode_restore = None;
                    }
                }
                Answer::Ready(output)
            }

            // The engine forgot this session: it restarted behind a
            // connection that outlived it, or its table was reset. A new
            // link is the only way forward, but not on this keystroke —
            // the user gets the key back and the next one reconnects.
            Ok(Response::Error(ErrorCode::UnknownSession)) => {
                self.drop_link();
                Answer::Unavailable
            }

            // Busy is materially different from an unavailable engine: the
            // session is alive, but a pending exact-text undo still owns the
            // host document boundary. Keep that fact visible to TSF so it can
            // consume the later key without creating a blank write plan.
            Ok(Response::Error(ErrorCode::Busy)) => Answer::Busy,

            // `Ok` and other `Error` answers are legitimate answers to `Commit` and to
            // requests this milestone does not make; neither carries text,
            // so there is nothing to show and nothing to correct.
            Ok(_) => Answer::Unavailable,

            // Kept, not dropped: see the module docs. The flag is what
            // stops the next successful call from building on a
            // composition the engine may have moved on from.
            Err(Fault::Timeout) => {
                note_timeout(timeout_operation(request));
                if session_effect(request) == SessionEffect::MayMutate {
                    link.desynchronized = true;
                }
                Answer::Unavailable
            }

            // Never left this process, so the peer cannot have misbehaved
            // and the link cannot be desynchronized by it. Only this
            // request is refused.
            Err(Fault::Encode(_)) => Answer::Rejected,

            Err(_) => {
                self.drop_link();
                Answer::Unavailable
            }
        }
    }

    /// Returns a usable link, building one if the retry interval allows.
    fn link(&mut self) -> Option<&mut Link> {
        if self.link.is_some() {
            // Resyncing before use rather than at the moment of the
            // timeout: at that moment there was, by definition, no time
            // left to spend on it.
            if let Some(link) = self.link.as_mut() {
                if link.desynchronized && !link.resync() {
                    self.drop_link();
                }
            }
        }

        if self.link.is_none() {
            if let Some(until) = self.blocked_until {
                if Instant::now() < until {
                    return None;
                }
            }
            match connect() {
                Some(link) => {
                    self.link = Some(link);
                    self.blocked_until = None;
                }
                None => {
                    self.blocked_until = Some(Instant::now() + RETRY_INTERVAL);
                    return None;
                }
            }
        }

        self.link.as_mut()
    }

    /// Drops the connection and starts the retry clock.
    ///
    /// The clock is set here rather than only on a failed connect because
    /// an engine that just broke a connection is an engine that is very
    /// likely to refuse the next one too.
    fn drop_link(&mut self) {
        self.link = None;
        self.blocked_until = Some(Instant::now() + RETRY_INTERVAL);
    }
}

impl Link {
    /// Throws away whatever the engine was composing, so both ends start
    /// the next keystroke from nothing.
    ///
    /// Returns whether the link is still usable. This is not data loss:
    /// the text the user could see was committed into the document at the
    /// moment of the timeout (see `text_service`'s `finalize`), so what is
    /// being discarded here is the engine's now-duplicate copy of it.
    fn resync(&mut self) -> bool {
        let session = self.session;
        match self.client.call(&Request::Revert { session }, KEY_BUDGET) {
            Ok(Response::Ok) => {
                self.desynchronized = false;
                true
            }
            Err(Fault::Timeout) => {
                note_timeout(TimeoutOperation::Resynchronize);
                false
            }
            // Still not answering, or answering something unexpected.
            // Either way this connection has stopped being trustworthy.
            _ => false,
        }
    }
}

/// Connects and completes the handshake, all inside [`RECONNECT_BUDGET`].
///
/// The budget is one deadline shared by three round trips rather than
/// three budgets, because what the keystroke thread can afford is a total,
/// not a per-step allowance.
fn connect() -> Option<Link> {
    open(None)
}

/// [`connect`], but against a named pipe of the caller's choosing, so the
/// tests can put a scripted engine on the other end. The well-known name
/// belongs to the logon session and is already taken by the real engine on
/// any machine where one is running.
#[cfg(test)]
fn connect_to(name: &str) -> Option<Link> {
    open(Some(name))
}

fn open(name: Option<&str>) -> Option<Link> {
    let deadline = Instant::now() + RECONNECT_BUDGET;
    let connected = match name {
        Some(name) => Client::connect_to(name, left(deadline)),
        None => Client::connect(left(deadline)),
    };
    let mut client = match connected {
        Ok(client) => client,
        Err(Fault::Timeout) => {
            note_timeout(TimeoutOperation::Connect);
            return None;
        }
        Err(_) => return None,
    };

    match client.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        left(deadline),
    ) {
        // The version is checked by the engine, which answers `Hello` only
        // when it matches; anything else means this DLL and that engine
        // are from different installs and must not talk.
        Ok(Response::Hello { .. }) => {}
        Err(Fault::Timeout) => {
            note_timeout(TimeoutOperation::Handshake);
            return None;
        }
        _ => return None,
    }

    let created = client.call(
        &Request::CreateSession {
            process_name: host_process_name(),
        },
        left(deadline),
    );
    match created {
        Ok(Response::SessionCreated { session, mode }) => Some(Link {
            client,
            session,
            mode,
            menu_mode_restore: None,
            mode_before_sensitive: None,
            input_scope: None,
            desynchronized: false,
        }),
        Err(Fault::Timeout) => {
            note_timeout(TimeoutOperation::Handshake);
            None
        }
        _ => None,
    }
}

fn timeout_operation(request: &Request) -> TimeoutOperation {
    match request {
        Request::ProbeKey { .. } => TimeoutOperation::ProbeKey,
        Request::SendKey {
            key: KeyInput {
                test_only: true, ..
            },
            ..
        } => TimeoutOperation::ProbeKey,
        Request::SendKey { .. } => TimeoutOperation::Key,
        Request::Commit { .. }
        | Request::CommitCandidate { .. }
        | Request::ApplyAiComposition { .. } => TimeoutOperation::Commit,
        Request::Reconvert { .. } => TimeoutOperation::Reconvert,
        Request::Revert { .. } => TimeoutOperation::Revert,
        Request::ResetDocumentContext { .. } => TimeoutOperation::Administration,
        Request::UndoCommit { .. } => TimeoutOperation::Revert,
        Request::SetUiPlacement { .. }
        | Request::WatchUi { .. }
        | Request::PollCandidateCommit { .. } => TimeoutOperation::UiPlacement,
        Request::Hello { .. } | Request::CreateSession { .. } => TimeoutOperation::Handshake,
        Request::ClearLearning
        | Request::ClearInputHistory
        | Request::FlushInputHistory
        | Request::InputHistoryStats
        | Request::DeleteHistoryCandidate { .. }
        | Request::QueueCandidateCommit { .. }
        | Request::SetInputScope { .. }
        | Request::SetMode { .. }
        | Request::RecordAiText { .. }
        | Request::StartAiText { .. }
        | Request::PollAiText { .. }
        | Request::CancelAiText { .. }
        | Request::DeleteSession { .. }
        | Request::Ping
        | Request::Shutdown => TimeoutOperation::Administration,
    }
}

/// Whether a timed-out request can have moved the live engine session.
///
/// Probe and other read-only calls run against a throwaway clone or do not
/// touch composition. Marking the link desynchronized would send `Revert`
/// and discard a live reading the Probe never mutated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEffect {
    ReadOnly,
    MayMutate,
}

fn session_effect(request: &Request) -> SessionEffect {
    match request {
        Request::ProbeKey { .. }
        | Request::SendKey {
            key: KeyInput {
                test_only: true, ..
            },
            ..
        }
        | Request::Reconvert { preview: true, .. }
        | Request::WatchUi { .. }
        | Request::PollCandidateCommit { .. }
        | Request::Ping
        | Request::PollAiText { .. }
        | Request::InputHistoryStats
        | Request::Hello { .. } => SessionEffect::ReadOnly,
        Request::SendKey { .. }
        | Request::Commit { .. }
        | Request::Revert { .. }
        | Request::ResetDocumentContext { .. }
        | Request::UndoCommit { .. }
        | Request::Reconvert { preview: false, .. }
        | Request::CreateSession { .. }
        | Request::ClearLearning
        | Request::ClearInputHistory
        | Request::FlushInputHistory
        | Request::DeleteHistoryCandidate { .. }
        | Request::QueueCandidateCommit { .. }
        | Request::CommitCandidate { .. }
        | Request::SetInputScope { .. }
        | Request::SetMode { .. }
        | Request::ApplyAiComposition { .. }
        | Request::RecordAiText { .. }
        | Request::StartAiText { .. }
        | Request::CancelAiText { .. }
        | Request::DeleteSession { .. }
        | Request::Shutdown
        | Request::SetUiPlacement { .. } => SessionEffect::MayMutate,
    }
}

fn update_mode_for_scope(link: &mut Link, scope: InputScope) {
    let was_sensitive = link.input_scope.is_some_and(scope_is_sensitive);
    if scope_is_sensitive(scope) {
        if !was_sensitive {
            link.mode_before_sensitive = Some(link.mode);
        }
        link.mode = Mode::Direct;
        link.menu_mode_restore = None;
    } else if was_sensitive {
        if let Some(mode) = link.mode_before_sensitive.take() {
            link.mode = mode;
        }
        link.menu_mode_restore = None;
    }
}

const fn scope_is_sensitive(scope: InputScope) -> bool {
    matches!(
        scope,
        InputScope::Password | InputScope::Url | InputScope::Email | InputScope::Digits
    )
}

#[cfg(not(test))]
fn note_timeout(operation: TimeoutOperation) {
    // Diagnostic failure must never replace the original, recoverable timeout
    // with a host-application error.
    let _ = diagnostics::record_timeout(operation);
}

#[cfg(test)]
fn note_timeout(operation: TimeoutOperation) {
    // Unit tests deliberately manufacture timeout paths. Keep that evidence
    // under the system temporary directory so `cargo test` can never append to
    // the installed user's durable diagnostics profile.
    let _ = diagnostics::record_timeout_at(&test_timeout_log_path(), operation);
}

#[cfg(test)]
fn test_timeout_log_path() -> std::path::PathBuf {
    std::env::temp_dir()
        .join("sakura-input-tests")
        .join(format!("sakura-tsf-{}", std::process::id()))
        .join("ipc-timeouts.bin")
}

fn left(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// The host executable's file name, for the engine's per-application
/// settings and for anything a user has to read in a diagnostics dump.
///
/// Computed once: it cannot change for the life of a process, and this is
/// called on a path where a `GetModuleFileNameW` per keystroke would be
/// pure waste.
fn host_process_name() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        // Long enough for any real executable path; a name that did not
        // fit would be truncated at the front, which is the end this
        // function throws away anyway.
        let mut buffer = [0u16; 1024];
        // SAFETY: `None` asks for the running executable rather than a
        // module handle, and the buffer is passed with its own length.
        let written = unsafe { GetModuleFileNameW(None, &mut buffer) } as usize;
        let Some(path) = buffer.get(..written) else {
            return UNKNOWN_HOST.to_owned();
        };
        if path.is_empty() {
            return UNKNOWN_HOST.to_owned();
        }
        let path = String::from_utf16_lossy(path);
        path.rsplit(['\\', '/'])
            .next()
            .filter(|leaf| !leaf.is_empty())
            .unwrap_or(UNKNOWN_HOST)
            .to_owned()
    })
    .clone()
}

/// Used when the host will not say what it is. The engine only uses the
/// name to look up per-application settings, so an unidentified host gets
/// the defaults rather than an error.
const UNKNOWN_HOST: &str = "unknown.exe";

// `expect` and `panic!` are denied for this crate because it is loaded into
// applications that are not ours to crash. Test code is not loaded into
// anything, and a test that cannot fail loudly is not a test.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use sakura_ipc::{Descriptor, PipeInstance};
    use sakura_proto::{
        decode_request, encode_response, InputScope, KeyCode, Modifiers, Preedit, Segment,
    };

    fn scratch_name(tag: &str) -> String {
        format!(r"\\.\pipe\sakura_tsf_test_{tag}_{}", std::process::id())
    }

    fn a_key(ch: char) -> KeyInput {
        KeyInput {
            code: KeyCode::Char,
            ch: Some(ch),
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        }
    }

    /// Reads one request and answers it, reusing its id so the client
    /// accepts the reply instead of discarding it as stale.
    fn answer(pipe: &PipeInstance, buffer: &mut Vec<u8>, response: &Response) {
        let payload = pipe.read_frame(buffer).expect("a request");
        let (id, _) = decode_request(payload).expect("a decodable request");
        let mut reply = Vec::new();
        encode_response(response, id, &mut reply).expect("encode");
        pipe.write_all(&reply).expect("write");
    }

    /// Stands up a peer that completes the handshake and then behaves
    /// however `then` says — including badly.
    fn fake_engine<F>(tag: &str, then: F) -> (String, std::thread::JoinHandle<()>)
    where
        F: FnOnce(&PipeInstance, &mut Vec<u8>) + Send + 'static,
    {
        let name = scratch_name(tag);
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("create");
        let handle = std::thread::spawn(move || {
            server.wait_for_client().expect("a client");
            let mut buffer = Vec::new();
            answer(
                &server,
                &mut buffer,
                &Response::Hello {
                    server_version: PROTOCOL_VERSION,
                    engine_version: [0, 1, 0],
                },
            );
            answer(
                &server,
                &mut buffer,
                &Response::SessionCreated {
                    session: 1,
                    mode: Mode::Hiragana,
                },
            );
            then(&server, &mut buffer);
        });
        (name, handle)
    }

    fn latin_preedit(text: &str) -> Output {
        Output {
            consumed: true,
            beep: false,
            mode: None,
            preedit: Some(Preedit {
                segments: vec![Segment {
                    text: text.to_owned(),
                    underline: sakura_proto::UnderlineKind::Raw,
                }],
                cursor: text.chars().count() as u32,
            }),
            commit: None,
            delete_before: String::new(),
            candidates: None,
            candidate_detail: None,
        }
    }

    fn some_output() -> Output {
        Output {
            consumed: true,
            beep: false,
            mode: None,
            preedit: Some(Preedit {
                segments: vec![Segment {
                    text: "か".to_owned(),
                    underline: sakura_proto::UnderlineKind::Raw,
                }],
                cursor: 1,
            }),
            commit: None,
            delete_before: String::new(),
            candidates: None,
            candidate_detail: None,
        }
    }

    #[test]
    fn timeout_diagnostics_for_unit_tests_never_use_the_installed_user_profile() {
        let test_path = test_timeout_log_path();
        assert!(test_path.starts_with(std::env::temp_dir()));
        if let Ok(installed_path) = diagnostics::default_timeout_log_path() {
            assert_ne!(test_path, installed_path);
        }
    }

    #[test]
    fn a_handshake_then_a_keystroke_comes_back_as_an_answer() {
        let (name, server) = fake_engine("roundtrip", |pipe, buffer| {
            answer(pipe, buffer, &Response::Output(some_output()));
        });

        let mut engine = Engine::attached_to(&name);
        assert!(engine.is_connected(), "the handshake must have completed");

        match engine.send_key(a_key('k')) {
            Answer::Ready(output) => {
                assert!(output.consumed);
                assert_eq!(
                    output.preedit.map(|p| p.segments.len()),
                    Some(1),
                    "the preedit did not survive the round trip"
                );
            }
            other => panic!("expected an answer, got {other:?}"),
        }

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn candidate_click_poll_and_commit_preserve_revision_and_index() {
        let (name, server) = fake_engine("candidate-click", |pipe, buffer| {
            let payload = pipe.read_frame(buffer).expect("candidate poll request");
            let (id, request) = decode_request(payload).expect("decodable candidate poll");
            assert_eq!(request, Request::PollCandidateCommit { session: 1 });
            let mut reply = Vec::new();
            encode_response(
                &Response::CandidateCommitPending {
                    request: Some((41, 7)),
                },
                id,
                &mut reply,
            )
            .expect("encode candidate poll");
            pipe.write_all(&reply).expect("write candidate poll");

            let payload = pipe.read_frame(buffer).expect("candidate commit request");
            let (id, request) = decode_request(payload).expect("decodable candidate commit");
            assert_eq!(
                request,
                Request::CommitCandidate {
                    session: 1,
                    revision: 41,
                    candidate_index: 7,
                }
            );
            let mut reply = Vec::new();
            encode_response(&Response::Output(some_output()), id, &mut reply)
                .expect("encode candidate commit output");
            pipe.write_all(&reply)
                .expect("write candidate commit output");
        });

        let mut engine = Engine::attached_to(&name);
        assert_eq!(
            engine.poll_candidate_commit(),
            CandidateCommitPoll::Pending {
                revision: 41,
                candidate_index: 7,
            }
        );
        assert!(matches!(engine.commit_candidate(41, 7), Answer::Ready(_)));

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn document_context_reset_preserves_mode_and_clears_menu_restore() {
        let (name, server) = fake_engine("document-context-reset", |pipe, buffer| {
            let payload = pipe
                .read_frame(buffer)
                .expect("ResetDocumentContext request");
            let (id, request) =
                decode_request(payload).expect("decodable ResetDocumentContext request");
            assert_eq!(request, Request::ResetDocumentContext { session: 1 });
            let mut reply = Vec::new();
            encode_response(&Response::Ok, id, &mut reply)
                .expect("encode ResetDocumentContext response");
            pipe.write_all(&reply)
                .expect("write ResetDocumentContext response");
        });

        let mut engine = Engine::attached_to(&name);
        let link = engine.link.as_mut().expect("connected link");
        link.mode = Mode::Katakana;
        link.menu_mode_restore = Some(Mode::Hiragana);

        assert!(engine.reset_document_context());
        let link = engine.link.as_ref().expect("reset keeps healthy link");
        assert_eq!(link.mode, Mode::Katakana);
        assert_eq!(link.menu_mode_restore, None);

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn refused_document_context_reset_retires_the_link() {
        let (name, server) = fake_engine("document-context-reset-refused", |pipe, buffer| {
            let payload = pipe
                .read_frame(buffer)
                .expect("ResetDocumentContext request");
            let (id, request) =
                decode_request(payload).expect("decodable ResetDocumentContext request");
            assert_eq!(request, Request::ResetDocumentContext { session: 1 });
            let mut reply = Vec::new();
            encode_response(&Response::Error(ErrorCode::Busy), id, &mut reply)
                .expect("encode refused reset response");
            pipe.write_all(&reply)
                .expect("write refused reset response");
        });

        let mut engine = Engine::attached_to(&name);
        assert!(engine.is_connected());
        assert!(!engine.reset_document_context());
        assert!(
            !engine.is_connected(),
            "an unconfirmed reset cannot leave document-relative state reusable"
        );

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn shift_latin_then_shift_backspace_roundtrip_keeps_press_order() {
        let (name, server) = fake_engine("shift-latin-bs", |pipe, buffer| {
            let expected = [
                (KeyCode::Char, Some('A'), true, "A"),
                (KeyCode::Char, Some('I'), true, "AI"),
                (KeyCode::Char, Some('U'), true, "AIU"),
                (KeyCode::Char, Some('E'), true, "AIUE"),
                (KeyCode::Char, Some('O'), true, "AIUEO"),
                (KeyCode::Backspace, None, true, "AIUE"),
                (KeyCode::Char, Some('O'), true, "AIUEO"),
            ];
            for (code, ch, shift, preedit) in expected {
                let payload = pipe.read_frame(buffer).expect("shift-latin request");
                let (id, request) = decode_request(payload).expect("decodable shift-latin request");
                match request {
                    Request::SendKey { key, .. } => {
                        assert_eq!(key.code, code, "unexpected key {key:?}");
                        assert_eq!(key.ch, ch, "unexpected character {key:?}");
                        assert_eq!(
                            key.modifiers.contains(Modifiers::SHIFT),
                            shift,
                            "Shift bit for {key:?}"
                        );
                    }
                    other => panic!("expected SendKey, got {other:?}"),
                }
                let mut reply = Vec::new();
                encode_response(&Response::Output(latin_preedit(preedit)), id, &mut reply)
                    .expect("encode shift-latin output");
                pipe.write_all(&reply).expect("write shift-latin output");
            }
        });

        let mut engine = Engine::attached_to(&name);
        assert!(engine.is_connected(), "the handshake must have completed");
        let keys = [
            KeyInput {
                modifiers: Modifiers::SHIFT,
                ..a_key('A')
            },
            KeyInput {
                modifiers: Modifiers::SHIFT,
                ..a_key('I')
            },
            KeyInput {
                modifiers: Modifiers::SHIFT,
                ..a_key('U')
            },
            KeyInput {
                modifiers: Modifiers::SHIFT,
                ..a_key('E')
            },
            KeyInput {
                modifiers: Modifiers::SHIFT,
                ..a_key('O')
            },
            KeyInput {
                code: KeyCode::Backspace,
                ch: None,
                modifiers: Modifiers::SHIFT,
                repeat: false,
                test_only: false,
            },
            KeyInput {
                modifiers: Modifiers::SHIFT,
                ..a_key('O')
            },
        ];
        let mut last = String::new();
        for key in keys {
            match engine.send_key(key) {
                Answer::Ready(output) => {
                    assert!(
                        output.consumed,
                        "Shift+Latin / Shift+Backspace must be consumed"
                    );
                    last = output
                        .preedit
                        .as_ref()
                        .and_then(|preedit| preedit.segments.first())
                        .map(|segment| segment.text.clone())
                        .unwrap_or_default();
                    assert_ne!(last, "AIUOEO");
                }
                other => panic!("expected an answer, got {other:?}"),
            }
        }
        assert_eq!(last, "AIUEO");

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn language_bar_mode_menu_tracks_scope_and_one_shot_restore() {
        let (name, server) = fake_engine("mode-menu", |pipe, buffer| {
            let steps = [
                (
                    Request::SetInputScope {
                        session: 1,
                        scope: InputScope::Normal,
                    },
                    Response::Ok,
                ),
                (
                    Request::SetMode {
                        session: 1,
                        mode: Mode::Katakana,
                    },
                    Response::InputMode {
                        mode: Mode::Katakana,
                    },
                ),
                (
                    Request::SetMode {
                        session: 1,
                        mode: Mode::Hiragana,
                    },
                    Response::InputMode {
                        mode: Mode::Hiragana,
                    },
                ),
            ];
            for (expected, response) in steps {
                let payload = pipe.read_frame(buffer).expect("mode-menu request");
                let (id, request) = decode_request(payload).expect("decodable mode-menu request");
                assert_eq!(request, expected);
                let mut reply = Vec::new();
                encode_response(&response, id, &mut reply).expect("encode mode-menu response");
                pipe.write_all(&reply).expect("write mode-menu response");
            }
        });

        let mut engine = Engine::attached_to(&name);
        assert_eq!(
            engine.input_mode_status(),
            Some(InputModeStatus {
                mode: Mode::Hiragana,
                can_change: false,
                can_restore: false,
            })
        );

        assert!(engine.set_input_scope(InputScope::Normal));
        assert!(engine.set_input_mode(Mode::Katakana));
        assert_eq!(
            engine.input_mode_status(),
            Some(InputModeStatus {
                mode: Mode::Katakana,
                can_change: true,
                can_restore: true,
            })
        );

        assert!(engine.restore_input_mode());
        assert_eq!(
            engine.input_mode_status(),
            Some(InputModeStatus {
                mode: Mode::Hiragana,
                can_change: true,
                can_restore: false,
            })
        );

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn test_only_probe_key_carries_scope_without_publishing_or_changing_link_cache() {
        let (name, server) = fake_engine("probe-scope", |pipe, buffer| {
            let payload = pipe.read_frame(buffer).expect("ProbeKey request");
            let (id, request) = decode_request(payload).expect("decodable ProbeKey");
            assert!(matches!(
                request,
                Request::ProbeKey {
                    session: 1,
                    scope: InputScope::Password,
                    fresh_context: false,
                    key: KeyInput {
                        test_only: true,
                        ..
                    },
                }
            ));
            let mut reply = Vec::new();
            encode_response(&Response::Output(some_output()), id, &mut reply)
                .expect("encode ProbeKey response");
            pipe.write_all(&reply).expect("write ProbeKey response");
        });

        let mut engine = Engine::attached_to(&name);
        assert_eq!(
            engine.link.as_ref().and_then(|link| link.input_scope),
            None,
            "the probe starts without a published scope"
        );
        assert!(matches!(
            engine.probe_key(InputScope::Password, a_key('k')),
            Answer::Ready(_)
        ));
        assert_eq!(
            engine.link.as_ref().and_then(|link| link.input_scope),
            None,
            "ProbeKey must not update the live link scope cache"
        );

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn test_only_context_replacement_probe_carries_fresh_session_mode_without_publishing_scope() {
        let (name, server) = fake_engine("probe-fresh-context", |pipe, buffer| {
            let payload = pipe
                .read_frame(buffer)
                .expect("fresh-context ProbeKey request");
            let (id, request) = decode_request(payload).expect("decodable ProbeKey");
            assert!(matches!(
                request,
                Request::ProbeKey {
                    session: 1,
                    scope: InputScope::Normal,
                    fresh_context: true,
                    key: KeyInput {
                        test_only: true,
                        ..
                    },
                }
            ));
            let mut reply = Vec::new();
            encode_response(&Response::Output(some_output()), id, &mut reply)
                .expect("encode fresh-context ProbeKey response");
            pipe.write_all(&reply)
                .expect("write fresh-context ProbeKey response");
        });

        let mut engine = Engine::attached_to(&name);
        assert!(matches!(
            engine.probe_key_for_context(InputScope::Normal, a_key('k'), true),
            Answer::Ready(_)
        ));
        assert_eq!(
            engine.link.as_ref().and_then(|link| link.input_scope),
            None,
            "a replacement Probe must not publish the live link scope"
        );

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn commit_undo_busy_is_distinct_and_keeps_the_engine_link_alive() {
        let (name, server) = fake_engine("busy", |pipe, buffer| {
            answer(pipe, buffer, &Response::Error(ErrorCode::Busy));
        });

        let mut engine = Engine::attached_to(&name);
        assert!(matches!(engine.send_key(a_key('k')), Answer::Busy));
        assert!(
            engine.is_connected(),
            "Busy is a live pending transaction, not an unavailable engine"
        );

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn reconversion_carries_the_selected_text_and_preview_flag() {
        let (name, server) = fake_engine("reconversion", |pipe, buffer| {
            let payload = pipe.read_frame(buffer).expect("reconversion request");
            let (id, request) = decode_request(payload).expect("decodable reconversion");
            assert!(matches!(
                request,
                Request::Reconvert {
                    session: 1,
                    ref text,
                    preview: true,
                } if text == "仮名"
            ));
            let mut reply = Vec::new();
            encode_response(&Response::Output(some_output()), id, &mut reply).expect("encode");
            pipe.write_all(&reply).expect("write");
        });

        let mut engine = Engine::attached_to(&name);
        assert!(matches!(
            engine.reconvert("仮名".to_owned(), true),
            Answer::Ready(_)
        ));

        drop(engine);
        server.join().expect("the server thread");
    }

    /// The crash-resilience case from PLAN.md Phase 1: the engine dies with
    /// a composition open. The keystroke has to come back as unavailable so
    /// the caller can finalize what is on screen and hand the key to the
    /// application.
    #[test]
    fn an_engine_that_dies_gives_the_keystroke_back_and_drops_the_link() {
        let (name, server) = fake_engine("death", |pipe, buffer| {
            let _ = pipe.read_frame(buffer);
            // Gone, without answering.
        });

        let mut engine = Engine::attached_to(&name);
        assert!(engine.is_connected());

        assert!(matches!(engine.send_key(a_key('k')), Answer::Unavailable));
        assert!(
            !engine.is_connected(),
            "a dead peer must not be left in place as a live link"
        );

        server.join().expect("the server thread");
    }

    /// `Client::call` splits a purely local `encode_request` failure
    /// (`Fault::Encode`, `client.rs:180`) from a hostile peer's malformed
    /// frame (`Fault::Protocol`, `client.rs:215`), and `Engine::request`
    /// (this file) routes the two to different `Answer`s: `Rejected` keeps
    /// the link, `Unavailable`'s catch-all drops it. This test proves that
    /// split holds end to end -- including that the link is not just
    /// "still marked connected" but actually still usable for the next
    /// request.
    #[test]
    fn local_reconvert_encode_failure_answers_rejected_and_keeps_the_link_usable() {
        let (name, server) = fake_engine("oversized-local", |pipe, buffer| {
            // The oversized `Reconvert` below never reaches the wire:
            // encoding fails inside `Client::call`, before any byte is
            // written to this pipe (`client.rs:180`). So this fake engine
            // only ever sees the two healthy verification keys.
            answer(pipe, buffer, &Response::Output(some_output()));
            answer(pipe, buffer, &Response::Output(some_output()));
        });

        let mut engine = Engine::attached_to(&name);
        assert!(engine.is_connected(), "the handshake must have completed");

        // Prove the link is healthy -- not "an engine that was never
        // really there" -- before the oversized request touches it.
        assert!(
            matches!(engine.send_key(a_key('k')), Answer::Ready(_)),
            "the link must answer normally before the oversized request"
        );
        assert!(engine.is_connected());

        // A `text` this large fails `write_str`'s own `MAX_STRING_BYTES`
        // (4096 bytes, see `wire.rs`) check well before the request could
        // even approach `MAX_PAYLOAD` (64 KiB) -- and comfortably exceeds
        // `MAX_PAYLOAD` too, so the failure holds regardless of which
        // internal check trips first.
        let huge_text = "あ".repeat(30_000); // 90,000 bytes
        let outcome = engine.reconvert(huge_text, false);

        assert!(
            matches!(outcome, Answer::Rejected),
            "a local encode failure never reached the peer, so it must not \
             be answered the same way as a dead or misbehaving engine -- \
             got {outcome:?}"
        );
        assert!(
            engine.is_connected(),
            "a local encode failure must not drop an otherwise healthy \
             link; the peer never saw this request and never misbehaved"
        );

        // Not just "still marked connected" -- the next, unrelated request
        // must actually succeed on the same link.
        assert!(
            matches!(engine.send_key(a_key('k')), Answer::Ready(_)),
            "the link must still answer a normal request after rejecting \
             the oversized one"
        );

        drop(engine);
        server.join().expect("the server thread");
    }

    /// The contrasting case: a corrupted/hostile frame really did reach the
    /// wire. Unlike the local-encode case above, keeping this link would
    /// mean trusting bytes this process cannot even parse. `payload_len`
    /// (`message.rs`) is the first thing that reads it, and its own
    /// `Error::TooLarge` also becomes `Fault::Protocol` (`client.rs:215`)
    /// -- proving the transport cannot tell the two cases apart on its
    /// own, even though this one must drop the link and the one above must
    /// not.
    #[test]
    fn oversized_remote_frame_is_unavailable_and_drops_the_link() {
        let (name, server) = fake_engine("oversized-remote", |pipe, buffer| {
            // Consume the real request, then answer with a frame whose
            // declared length alone already exceeds `MAX_PAYLOAD` -- the
            // client rejects this from the 4-byte header, before it would
            // ever try to read a body this large.
            let _ = pipe.read_frame(buffer);
            let bogus_len = (sakura_proto::MAX_PAYLOAD + 1) as u32;
            pipe.write_all(&bogus_len.to_le_bytes())
                .expect("write an oversized frame header");
        });

        let mut engine = Engine::attached_to(&name);
        assert!(engine.is_connected(), "the handshake must have completed");

        assert!(matches!(engine.send_key(a_key('k')), Answer::Unavailable));
        assert!(
            !engine.is_connected(),
            "a peer that sends a frame this large is not a peer this link \
             can keep trusting -- it must be dropped, unlike a local \
             encode failure"
        );

        server.join().expect("the server thread");
    }

    /// A timeout is not a death. The request is still in flight and the
    /// engine still holds the session, so reconnecting would throw away the
    /// user's composition to fix a hiccup.
    #[test]
    fn a_slow_answer_costs_the_budget_but_not_the_connection() {
        let (name, server) = fake_engine("slow", |pipe, buffer| {
            let _ = pipe.read_frame(buffer);
            // Long enough that the client is certain to have given up, and
            // still holding the connection open while it does.
            std::thread::sleep(Duration::from_millis(400));
        });

        let mut engine = Engine::attached_to(&name);
        let started = Instant::now();
        let answer = engine.send_key(a_key('k'));
        let waited = started.elapsed();

        assert!(matches!(answer, Answer::Unavailable));
        assert!(
            engine.is_connected(),
            "a slow answer must not cost the session"
        );
        assert!(
            engine.is_desynchronized(),
            "a mutating SendKey timeout must mark the live session desynchronized"
        );
        // Generous, because this asserts the deadline was honoured at all,
        // not scheduler precision on a loaded machine.
        assert!(
            waited < Duration::from_millis(300),
            "a keystroke waited {waited:?}, well past its budget"
        );

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn probe_key_timeout_does_not_desynchronize_the_live_session() {
        let (name, server) = fake_engine("probe-timeout", |pipe, buffer| {
            let _ = pipe.read_frame(buffer);
            std::thread::sleep(Duration::from_millis(400));
        });

        let mut engine = Engine::attached_to(&name);
        let key = KeyInput {
            code: KeyCode::Space,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: true,
        };
        let answer = engine.probe_key(InputScope::Normal, key);
        assert!(matches!(answer, Answer::Unavailable));
        assert!(
            engine.is_connected(),
            "a Probe timeout must keep the connection"
        );
        assert!(
            !engine.is_desynchronized(),
            "ProbeKey is a throwaway clone; timeout must not Revert the live reading"
        );

        drop(engine);
        server.join().expect("the server thread");
    }

    #[test]
    fn session_effect_classifies_probe_as_read_only() {
        let probe = Request::ProbeKey {
            session: 1,
            scope: InputScope::Normal,
            fresh_context: false,
            key: KeyInput {
                code: KeyCode::Space,
                ch: None,
                modifiers: Modifiers::NONE,
                repeat: false,
                test_only: true,
            },
        };
        assert_eq!(session_effect(&probe), SessionEffect::ReadOnly);
        assert_eq!(timeout_operation(&probe), TimeoutOperation::ProbeKey);
        let apply = Request::SendKey {
            session: 1,
            key: a_key('k'),
        };
        assert_eq!(session_effect(&apply), SessionEffect::MayMutate);
        assert_eq!(timeout_operation(&apply), TimeoutOperation::Key);
    }

    /// The DLL is loaded into applications that may never have an engine
    /// to talk to. Asking one that is not there must be cheap and must
    /// answer `Unavailable`, not block and not fail.
    #[test]
    fn a_missing_engine_answers_unavailable_without_waiting() {
        let mut engine = Engine::new();
        let key = KeyInput {
            code: sakura_proto::KeyCode::Char,
            ch: Some('a'),
            modifiers: sakura_proto::Modifiers::NONE,
            repeat: false,
            test_only: false,
        };

        let started = Instant::now();
        let answer = engine.send_key(key);
        let waited = started.elapsed();

        // If an engine happens to be running on this machine the answer is
        // legitimately `Ready`; what is being asserted either way is that
        // the call returned promptly.
        if !engine.is_connected() {
            assert!(matches!(answer, Answer::Unavailable));
        }
        assert!(
            waited < RECONNECT_BUDGET * 2,
            "a keystroke waited {waited:?} on a connection attempt"
        );
    }

    /// The retry interval is what keeps a machine with no engine from
    /// paying a failed connect on every key.
    #[test]
    fn a_failed_attempt_stops_the_next_key_from_trying_again() {
        let mut engine = Engine::new();
        engine.warm_up();
        if engine.is_connected() {
            return; // An engine is running here; this test has no subject.
        }

        assert!(
            engine.blocked_until.is_some(),
            "a failed attempt must start the retry clock"
        );
        let blocked = engine.blocked_until;
        assert!(engine.link().is_none());
        assert_eq!(
            engine.blocked_until, blocked,
            "the second attempt must have been skipped, not retried"
        );
    }

    #[test]
    fn the_host_name_is_a_file_name_not_a_path() {
        let name = host_process_name();
        assert!(!name.contains('\\'), "{name} is a path, not a name");
        assert!(!name.is_empty());
    }
}
