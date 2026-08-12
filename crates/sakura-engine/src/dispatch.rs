//! Request in, response out.
//!
//! [`Dispatcher`] is the whole engine's pure-logic core: it owns the shipped
//! romaji table, key map, and width normalizer, a bounded table of live
//! sessions (see [`crate::session`]), and one scratch buffer, and it turns
//! one [`Request`] plus a session's state into one [`Reply`] plus whatever
//! got written into the caller's [`OutputBuf`]. Nothing in this module opens
//! a handle, spawns a thread, or reads a clock — see the crate's module
//! docs for why that split exists — which is what lets every behaviour
//! below be driven from a `#[test]` instead of a running pipe.
//!
//! # M0 scope
//!
//! Phase 1 (DESIGN's M0) has no conversion: romaji in, hiragana preedit out,
//! Enter commits it whole. [`Action`] has a bounded payload-free variant set because the key
//! map format already describes every phase's bindings, but this dispatcher
//! only *implements* the handful M0 needs (`Commit`, `Cancel`, `DeleteBack`,
//! the mode switches, and the IME on/off toggles); everything else a key
//! map might resolve to — conversion, candidate movement, prediction,
//! segment editing, transforms, reconversion — is accepted and silently
//! swallowed (see [`apply_action`]'s fallback arm) rather than crashing or
//! falling through into the host document, leaving a clean seam for the
//! phase that implements it to fill in one match arm at a time.
//!
//! `Mode::Katakana` and `Mode::HalfKatakana` compose through the same
//! hiragana-only romaji FSM as `Mode::Hiragana`; the selected mode is applied
//! before rendering.
//! Half-width voiced kana may expand to a base kana plus a dakuten, so the
//! render path owns the cursor mapping as well. Only
//! `Mode::HalfAlnum` and `Mode::FullAlnum` behave distinctly today — they
//! never build a composition at all, committing each keystroke immediately
//! through the width normalizer (see [`apply_alnum_char`]).

use std::sync::Arc;
use std::time::Duration;

use sakura_core::dictionary::DetailRelationKind;
use sakura_core::keymap::{Action, KeyMap, KeyMapError, Preset, State};
use sakura_core::romaji::{Table, TableError};
use sakura_core::width::Normalizer;
use sakura_core::{
    default_app_profiles, resolve_context_preferences, transform_into, AppProfile,
    ConversionCandidate, ConversionOptions, ConversionSegment, EntryFlags, Input, Preferences,
    SegmentTransform, SuggestAccept, TextSink,
};
use sakura_proto::{
    CandidateDetailInput, ErrorCode, FixedStr, FixedVec, InputScope, KeyInput, Mode, OutputBuf,
    Overflow, Request, Response, SessionId, UnderlineKind, UndoCommitOutcome, CANDIDATE_PAGE_SIZE,
    MAX_CANDIDATE_DETAIL_DEFINITION_BYTES, MAX_CANDIDATE_DETAIL_RELATIONS,
    MAX_CANDIDATE_DETAIL_RELATION_BYTES, MAX_PREEDIT_BYTES, MAX_SEGMENTS, PROTOCOL_VERSION,
};

use crate::dictionary::{ConversionService, ConvertFailure};
use crate::input_history::{clear_path, default_path, InputHistoryService, ScopeClass};
use crate::learning::{ForgetPredictionOutcome, LearningPreference, LearningService};
use crate::long_conversion::LongConversionService;
use crate::prediction::{PredictionResult, PredictionService, PredictionSource};
use crate::session::{scope_is_sensitive, text_hash, Session, SessionTable};

/// Reported to a client that asks `Hello`. Not the protocol version (that is
/// [`PROTOCOL_VERSION`], checked separately by `sakura_proto::decode_request`
/// before a request ever reaches here) — this is the engine build itself,
/// for diagnostics. There is no released engine yet to track compatibility
/// against, so it simply mirrors the workspace version.
const ENGINE_VERSION: [u16; 3] = [1, 0, 0];
const PREDICTION_TIMEOUT: Duration = Duration::from_millis(10);

/// Why [`Dispatcher::new`] could not build itself from the shipped defaults.
///
/// Both sources are compiled from data files this same workspace ships
/// (`Table::builtin`, `KeyMap::preset`), so in a correctly built binary this
/// is unreachable; it exists so a corrupted or mis-packaged build fails with
/// a diagnosable message (`server.rs` logs its `Display` output) instead of
/// a panic with no context.
#[derive(Debug)]
pub enum NewError {
    Romaji(TableError),
    KeyMap(KeyMapError),
}

impl core::fmt::Display for NewError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NewError::Romaji(error) => write!(f, "romaji table: {error}"),
            NewError::KeyMap(error) => write!(f, "key map: {error}"),
        }
    }
}

impl std::error::Error for NewError {}

/// What answering a [`Request`] produced.
///
/// [`Dispatcher::dispatch`] always writes into the caller's [`OutputBuf`] (it
/// starts every call by clearing it), but only `Output` means that buffer is
/// the answer; `Message` and `Shutdown` carry their own [`Response`] and the
/// `OutputBuf` for that call is left empty. Keeping these as one enum rather
/// than always returning a `Response` is what lets `SendKey`'s hot path
/// answer through the allocation-free `OutputBuf`/`encode_frame` route while
/// every other request still gets the ordinary allocating `Response` path
/// (`server.rs` branches on exactly this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The answer is in the `OutputBuf` passed to `dispatch`.
    Output,
    /// The answer is this `Response`; the `OutputBuf` is empty.
    Message(Response),
    /// The client asked the engine to stop. Answer with this `Response`,
    /// then stop serving — see `crate::server`'s module docs for why
    /// acting on it is the caller's job, not this one's.
    Shutdown(Response),
}

/// The engine's whole pure-logic core for one connection.
///
/// A `Dispatcher` owns everything a session's keystrokes are resolved
/// against — the romaji table, the key map, the width normalizer — plus the
/// bounded table of sessions those keystrokes belong to and one scratch
/// buffer used to build normalized text before it is copied into an
/// `OutputBuf`. Per `crate::server`'s share-nothing design, one
/// `Dispatcher` belongs to one pipe-instance thread for that thread's whole
/// life, reused across sequential connections via [`Dispatcher::reset`].
#[derive(Debug)]
pub struct Dispatcher {
    table: Table,
    keymap: KeyMap,
    normalizer: Normalizer,
    conversion: Option<Arc<ConversionService>>,
    learning: Option<Arc<LearningService>>,
    input_history: Option<Arc<InputHistoryService>>,
    prediction: Option<Arc<PredictionService>>,
    long_conversion: Option<Arc<LongConversionService>>,
    long_conversion_owner: u64,
    prediction_enabled: bool,
    suggest_accept: SuggestAccept,
    app_profiles: Arc<[AppProfile]>,
    /// Last process-wide learning epoch observed by this pipe worker.
    observed_learning_generation: u64,
    /// One cache per connection, boxed so nested dispatcher constructors do
    /// not copy its fixed suggestion buffers through the 128 KiB pipe stack.
    prediction_cache: Box<PredictionCache>,
    sessions: SessionTable,
    /// Scratch space for building normalized text before it is copied into
    /// an `OutputBuf` segment or commit field. Living here rather than as a
    /// stack-local in the functions that use it is what keeps the `SendKey`
    /// path allocation-free: a stack-local `FixedStr` would be reinitialized
    /// (and, at debug-build stack-probe granularity, freshly touched) on
    /// every call, where this one is zeroed once, for the dispatcher's
    /// whole life.
    scratch: FixedStr<MAX_PREEDIT_BYTES>,
}

impl Dispatcher {
    /// Builds a dispatcher from the engine's shipped defaults: the built-in
    /// romaji table, the MS-IME key map preset, and a default (never-widen)
    /// width normalizer.
    pub fn new() -> Result<Self, NewError> {
        let table = Table::builtin().map_err(NewError::Romaji)?;
        let keymap = KeyMap::preset(Preset::MsIme).map_err(NewError::KeyMap)?;
        Ok(Self::with_parts(table, keymap, Normalizer::default()))
    }

    /// Builds the shipped dispatcher with dictionary conversion enabled.
    pub fn new_with_conversion(conversion: Arc<ConversionService>) -> Result<Self, NewError> {
        let mut dispatcher = Self::new()?;
        dispatcher.conversion = Some(conversion);
        Ok(dispatcher)
    }

    /// Builds the shipped dispatcher with process-shared conversion and
    /// personalization services enabled.
    pub fn new_with_services(
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
    ) -> Result<Self, NewError> {
        Self::new_with_configuration(conversion, learning, Preferences::default())
    }

    /// Builds a production dispatcher from validated per-user preferences.
    pub fn new_with_configuration(
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        preferences: Preferences,
    ) -> Result<Self, NewError> {
        let profiles = Arc::<[AppProfile]>::from(default_app_profiles(preferences));
        Self::new_with_configuration_and_profiles(conversion, learning, preferences, profiles)
    }

    /// Builds a production dispatcher with explicitly loaded application
    /// profiles. Profile values are copied into each new context exactly once.
    pub fn new_with_configuration_and_profiles(
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        preferences: Preferences,
        profiles: Arc<[AppProfile]>,
    ) -> Result<Self, NewError> {
        let table = Table::builtin().map_err(NewError::Romaji)?;
        let keymap = KeyMap::preset(preferences.keymap_preset).map_err(NewError::KeyMap)?;
        let mut dispatcher = Self::with_parts(table, keymap, preferences.normalizer);
        dispatcher.conversion = Some(conversion);
        dispatcher.observed_learning_generation = learning.generation();
        dispatcher.learning = Some(learning);
        dispatcher.prediction_enabled = preferences.prediction_enabled;
        dispatcher.suggest_accept = preferences.suggest_accept;
        dispatcher.app_profiles = profiles;
        Ok(dispatcher)
    }

    /// Builds a production dispatcher with the process-wide prediction worker.
    pub fn new_with_runtime_configuration(
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        prediction: Arc<PredictionService>,
        preferences: Preferences,
    ) -> Result<Self, NewError> {
        let profiles = Arc::<[AppProfile]>::from(default_app_profiles(preferences));
        Self::new_with_runtime_configuration_and_profiles(
            conversion,
            learning,
            prediction,
            preferences,
            profiles,
        )
    }

    pub fn new_with_runtime_configuration_and_profiles(
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        prediction: Arc<PredictionService>,
        preferences: Preferences,
        profiles: Arc<[AppProfile]>,
    ) -> Result<Self, NewError> {
        let mut dispatcher =
            Self::new_with_configuration_and_profiles(conversion, learning, preferences, profiles)?;
        dispatcher.prediction = Some(prediction);
        Ok(dispatcher)
    }

    pub(crate) fn set_input_history(&mut self, input_history: Arc<InputHistoryService>) {
        self.input_history = Some(input_history);
    }

    pub(crate) fn set_long_conversion(&mut self, long_conversion: Arc<LongConversionService>) {
        self.long_conversion_owner = long_conversion.allocate_owner();
        self.long_conversion = Some(long_conversion);
    }

    /// Builds a dispatcher from already-built parts. Used by tests that need
    /// a romaji table, key map, or normalizer the shipped defaults cannot
    /// reach (for example the MS-IME preset binds no key to a direct
    /// mode-switch action, so exercising `Mode::FullAlnum` needs a key map
    /// that does).
    pub fn with_parts(table: Table, keymap: KeyMap, normalizer: Normalizer) -> Self {
        Dispatcher {
            table,
            keymap,
            normalizer,
            conversion: None,
            learning: None,
            input_history: None,
            prediction: None,
            long_conversion: None,
            long_conversion_owner: 0,
            prediction_enabled: false,
            suggest_accept: SuggestAccept::Disabled,
            app_profiles: Arc::from([]),
            observed_learning_generation: 0,
            prediction_cache: Box::new(PredictionCache::new()),
            sessions: SessionTable::new(),
            scratch: FixedStr::new(),
        }
    }

    /// Drops every session, ready for a new connection to start clean.
    ///
    /// Leaves the romaji table, key map, and normalizer untouched — those
    /// are the engine's shipped configuration, not connection state. See
    /// `crate::server`'s `worker`, which calls this between connections on
    /// the same pipe instance.
    pub fn reset(&mut self) {
        self.sessions.clear();
        self.prediction_cache.clear();
    }

    /// Answers one request, writing `Output` replies into `out`.
    ///
    /// `out` is cleared unconditionally before anything else, so a caller
    /// that reuses the same `OutputBuf` across calls (as `crate::server`
    /// does) never sees a previous call's leftovers on a `Message` or
    /// `Shutdown` reply.
    pub fn dispatch(&mut self, request: &Request, out: &mut OutputBuf) -> Reply {
        out.clear();
        if self.is_blocked_by_pending_undo(request) {
            // The host-side exact-text deletion is the only operation allowed
            // to settle a pending undo. Check this before live cache
            // invalidation so a re-entrant request cannot mutate any engine
            // state while the document outcome is still unknown.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        let probe = matches!(
            request,
            Request::SendKey {
                key: KeyInput {
                    test_only: true,
                    ..
                },
                ..
            }
        ) || matches!(request, Request::ProbeKey { .. });
        if !probe {
            self.invalidate_stale_prediction_cache();
        }
        match request {
            Request::Hello { client_version } => self.hello(*client_version),
            Request::CreateSession { process_name } => self.create_session(process_name),
            Request::SendKey { session, key } => self.send_key(*session, key, out),
            Request::ProbeKey {
                session,
                scope,
                fresh_context,
                key,
            } => self.probe_key(*session, *scope, *fresh_context, key, out),
            Request::Commit { session } => self.commit(*session, out),
            Request::Reconvert {
                session,
                text,
                preview,
            } => self.reconvert(*session, text, *preview, out),
            Request::Revert { session } => self.revert(*session),
            Request::UndoCommit { session, outcome } => {
                self.undo_commit_outcome(*session, *outcome)
            }
            Request::ClearLearning => self.clear_learning(),
            Request::ClearInputHistory => self.clear_input_history(),
            Request::FlushInputHistory => self.flush_input_history(),
            Request::InputHistoryStats => self.input_history_stats(),
            Request::SetInputScope { session, scope } => self.set_input_scope(*session, *scope),
            Request::SetMode { session, mode } => self.set_mode(*session, *mode),
            // The renderer has no dispatcher-owned session. `server` resolves
            // this request through its shared revision-stamped UiBoard before
            // it reaches a worker; a direct dispatcher call must remain a
            // fail-closed no-op rather than deleting by a guessed surface.
            Request::DeleteHistoryCandidate { .. } => {
                Reply::Message(Response::HistoryCandidateDeleted { removed: false })
            }
            Request::DeleteSession { session } => self.delete_session(*session),
            Request::Ping => Reply::Message(Response::Pong),
            Request::Shutdown => Reply::Shutdown(Response::Ok),
            // `crate::server` answers this before the request ever gets
            // here, from state shared across connections and by blocking on
            // a condition variable — a clock and a shared mutable, the two
            // things this module's docs promise it does not have. Reaching
            // this arm would mean that interception was removed, so it says
            // so rather than inventing an answer.
            Request::WatchUi { .. } | Request::SetUiPlacement { .. } => {
                Reply::Message(Response::Error(ErrorCode::Internal))
            }
        }
    }

    fn is_blocked_by_pending_undo(&self, request: &Request) -> bool {
        let session = match request {
            Request::SendKey { session, .. }
            | Request::ProbeKey { session, .. }
            | Request::Commit { session }
            | Request::Reconvert { session, .. }
            | Request::Revert { session }
            | Request::SetInputScope { session, .. }
            | Request::SetMode { session, .. }
            | Request::DeleteSession { session } => *session,
            Request::Hello { .. }
            | Request::CreateSession { .. }
            | Request::UndoCommit { .. }
            | Request::ClearLearning
            | Request::ClearInputHistory
            | Request::FlushInputHistory
            | Request::InputHistoryStats
            | Request::DeleteHistoryCandidate { .. }
            | Request::Ping
            | Request::Shutdown
            | Request::WatchUi { .. }
            | Request::SetUiPlacement { .. } => return false,
        };
        self.sessions
            .get(session)
            .is_some_and(Session::undo_pending)
    }

    fn invalidate_stale_prediction_cache(&mut self) {
        let Some(learning) = self.learning.as_deref() else {
            return;
        };
        let generation = learning.generation();
        if generation != self.observed_learning_generation {
            self.prediction_cache.clear();
            self.observed_learning_generation = generation;
        }
    }

    fn hello(&self, client_version: u16) -> Reply {
        if client_version == PROTOCOL_VERSION {
            Reply::Message(Response::Hello {
                server_version: PROTOCOL_VERSION,
                engine_version: ENGINE_VERSION,
            })
        } else {
            Reply::Message(Response::Error(ErrorCode::UnsupportedVersion))
        }
    }

    fn create_session(&mut self, process_name: &str) -> Reply {
        match self.sessions.create(process_name) {
            Ok(session) => {
                let global = Preferences {
                    keymap_preset: Preset::MsIme,
                    normalizer: self.normalizer,
                    prediction_enabled: self.prediction_enabled,
                    suggest_accept: self.suggest_accept,
                    developer_mode: self.input_history.is_some(),
                };
                let resolved =
                    resolve_context_preferences(global, &self.app_profiles, process_name);
                if let Some(created) = self.sessions.get_mut(session) {
                    let history_session_id = self
                        .input_history
                        .as_ref()
                        .map_or(session, |history| history.allocate_session_id());
                    created.set_history_session_id(history_session_id);
                    created.apply_context_preferences(resolved);
                }
                let mode = self
                    .sessions
                    .get(session)
                    .map(Session::mode)
                    .unwrap_or(Mode::Hiragana);
                Reply::Message(Response::SessionCreated { session, mode })
            }
            Err(code) => Reply::Message(Response::Error(code)),
        }
    }

    fn delete_session(&mut self, id: SessionId) -> Reply {
        if self.sessions.get(id).is_some_and(Session::undo_pending) {
            // A pending exact-text undo still owns the session's restored
            // composition and its host-side journal ticket. Deleting the
            // session would make the only terminal outcome unaddressable.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        if self.sessions.delete(id) {
            self.prediction_cache.clear_if_session(id);
            Reply::Message(Response::Ok)
        } else {
            Reply::Message(Response::Error(ErrorCode::UnknownSession))
        }
    }

    fn clear_learning(&mut self) -> Reply {
        let Some(learning) = self.learning.as_ref() else {
            return Reply::Message(Response::Error(ErrorCode::Internal));
        };
        match learning.clear() {
            Ok(_) => {
                // Every per-connection suggestion cache can otherwise retain
                // a result ranked by the just-cleared history. Clearing this
                // dispatcher's cache makes the administrative request reach
                // an explicit, observable terminal state for this client;
                // other workers recompute on their next changed preedit.
                self.prediction_cache.clear();
                self.observed_learning_generation = learning.generation();
                Reply::Message(Response::Ok)
            }
            Err(_) => Reply::Message(Response::Error(ErrorCode::Internal)),
        }
    }

    fn clear_input_history(&mut self) -> Reply {
        let result = if let Some(history) = self.input_history.as_ref() {
            history.clear()
        } else {
            default_path().and_then(|path| clear_path(&path))
        };
        match result {
            Ok(_) => Reply::Message(Response::Ok),
            Err(_) => Reply::Message(Response::Error(ErrorCode::Internal)),
        }
    }

    fn flush_input_history(&mut self) -> Reply {
        let result = self
            .input_history
            .as_ref()
            .map_or(Ok(()), |history| history.flush());
        match result {
            Ok(()) => Reply::Message(Response::Ok),
            Err(_) => Reply::Message(Response::Error(ErrorCode::Internal)),
        }
    }

    fn input_history_stats(&mut self) -> Reply {
        let active = self.input_history.is_some();
        let stats = self
            .input_history
            .as_ref()
            .map_or_else(Default::default, |history| history.stats().snapshot());
        Reply::Message(Response::InputHistoryStats {
            active,
            dropped_events: stats.dropped_events,
            persistence_failures: stats.persistence_failures,
            excluded_unclassified_events: stats.excluded_unclassified_events,
            excluded_sensitive_events: stats.excluded_sensitive_events,
            excluded_test_only_events: stats.excluded_test_only_events,
        })
    }

    fn set_input_scope(&mut self, id: SessionId, scope: InputScope) -> Reply {
        let clear_cache = {
            let Some(session) = self.sessions.get_mut(id) else {
                return Reply::Message(Response::Error(ErrorCode::UnknownSession));
            };
            if session.undo_pending() {
                // Scope changes reset or clear personal context in sensitive
                // fields. They must wait for the host to settle the exact undo
                // so neither path can silently disarm the transaction.
                return Reply::Message(Response::Error(ErrorCode::Busy));
            }
            session.apply_input_scope(scope)
        };
        if clear_cache {
            self.prediction_cache.clear_if_session(id);
        }
        Reply::Message(Response::Ok)
    }

    /// Applies an explicit input-mode choice from the focused TSF input-mode
    /// item. Unlike a keyboard mode action, this path never commits or edits a
    /// document: a menu callback has no edit-session transaction to settle.
    ///
    /// A mode menu is deliberately fail-closed until TSF has classified the
    /// field as ordinary text. That prevents a menu click immediately after
    /// focus enters a password/URL/e-mail/digits field from reviving kana
    /// composition before the normal key path publishes its scope.
    fn set_mode(&mut self, id: SessionId, mode: Mode) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if session.undo_pending()
            || session.is_composing()
            || !session.scope_classified()
            || scope_is_sensitive(session.scope())
        {
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        session.mode = mode;
        Reply::Message(Response::InputMode { mode })
    }

    fn send_key(&mut self, id: SessionId, key: &KeyInput, out: &mut OutputBuf) -> Reply {
        if key.test_only {
            let Some(existing) = self.sessions.get(id) else {
                return Reply::Message(Response::Error(ErrorCode::UnknownSession));
            };
            if existing.undo_pending() {
                return Reply::Message(Response::Error(ErrorCode::Busy));
            }
            return self.probe_session(id, existing.clone(), key, out, false);
        }
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if session.undo_pending() {
            // The exact-text undo output is still owned by the TSF journal.
            // Do not let a re-entrant or later key advance either the live
            // session or its history while the host-side outcome is unknown.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        let normalizer = session.normalizer;
        let services = KeyServices {
            table: &self.table,
            keymap: &self.keymap,
            normalizer: &normalizer,
            conversion: self.conversion.as_deref(),
            learning: self.learning.as_deref(),
            input_history: self.input_history.as_deref(),
            prediction: self.prediction.as_deref(),
            long_conversion: self.long_conversion.as_deref(),
            long_conversion_owner: self.long_conversion_owner,
            prediction_enabled: session.prediction_enabled,
            suggest_accept: session.suggest_accept,
        };

        let state_before = session.state();
        let action_name = self.keymap.lookup(state_before, key).map_or(
            if key.ch.is_some() { "char" } else { "unbound" },
            Action::name,
        );
        let mode_before = session.mode();
        let preedit_before = if services.input_history.is_some() {
            // The persisted before/after fields describe what the user saw,
            // not the engine's raw reading. Render a fixed-size clone so a
            // diagnostic snapshot cannot mutate the live composition or
            // consume the output that the real key operation will fill.
            let mut before_session = session.clone();
            let _ = render_preedit(
                &mut before_session,
                services.table,
                services.normalizer,
                services.conversion,
                &mut self.scratch,
                out,
            );
            let before_cache = PredictionCacheWork::Probe {
                cache: &self.prediction_cache,
                stale: false,
            };
            let _ = render_prediction_projection(
                id,
                &mut before_session,
                services.normalizer,
                &before_cache,
                &mut self.scratch,
                out,
            );
            let rendered = out.preedit_text().to_owned();
            out.clear();
            rendered
        } else {
            String::new()
        };
        match apply_key(
            id,
            session,
            &services,
            key,
            KeyWork {
                policy: ExecutionPolicy::Apply,
                prediction_cache: PredictionCacheWork::Apply(&mut self.prediction_cache),
                scratch: &mut self.scratch,
                out,
            },
        ) {
            Ok(()) => {
                schedule_long_conversion(id, session, &services);
                if let Some(history) = services.input_history {
                    history.record_key(
                        session.history_session_id(),
                        ScopeClass::from_scope(session.scope, session.scope_classified()),
                        key.code as u16,
                        key.ch,
                        key.modifiers.0,
                        key.repeat,
                        key.test_only,
                        out.consumed,
                        state_code(state_before),
                        state_code(session.state()),
                        mode_before as u8,
                        session.mode() as u8,
                        &preedit_before,
                        out.preedit_text(),
                        out.commit_text().unwrap_or(""),
                        out.delete_before_utf16(),
                        out.beep,
                        action_name,
                    );
                }
                Reply::Output
            }
            Err(Overflow) => {
                // The composition (or the host's `OutputBuf`) would not
                // fit. Every function that mutates `Session` fields on the
                // way here is internally atomic (see feed_character,
                // flush_pending, commit_suggestion_at, commit_pending,
                // commit_converted_segments): a failing fallible write
                // leaves the fields it would have touched exactly as they
                // were, so there is nothing to reconcile in the general
                // case -- unlike the old blanket `session.reset()`, which
                // used to erase a long in-progress sentence just because a
                // single keystroke could not grow it any further.
                //
                // `Action::UndoCommit` is the one exception: `undo_commit()`
                // unconditionally arms `undo_pending` and overwrites the
                // composition with the restored prior reading before this
                // key's rendering can fail, so an overflow afterwards really
                // does leave a transaction the host was never told about.
                // No Output reached TSF on that local failure, so the host
                // document is definitely unchanged; reject the transaction
                // locally rather than leave it permanently pending.
                if session.undo_pending() {
                    let _ = session.reject_undo_commit();
                }
                out.clear();
                if let Some(history) = services.input_history {
                    history.record_key(
                        session.history_session_id(),
                        ScopeClass::from_scope(session.scope, session.scope_classified()),
                        key.code as u16,
                        key.ch,
                        key.modifiers.0,
                        key.repeat,
                        key.test_only,
                        false,
                        state_code(state_before),
                        state_code(session.state()),
                        mode_before as u8,
                        session.mode() as u8,
                        &preedit_before,
                        "",
                        "",
                        0,
                        false,
                        action_name,
                    );
                }
                Reply::Message(Response::Error(ErrorCode::TooLarge))
            }
        }
    }

    fn probe_key(
        &mut self,
        id: SessionId,
        scope: InputScope,
        fresh_context: bool,
        key: &KeyInput,
        out: &mut OutputBuf,
    ) -> Reply {
        let Some(existing) = self.sessions.get(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if existing.undo_pending() {
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        let mut probe = if fresh_context {
            // `observe_write_context` retires the old TSF engine link before a
            // real replacement key reaches this point. Mirror the resulting
            // CreateSession defaults without touching the live session or
            // allocating a temporary process-name String.
            let process_name = existing.process_name();
            let global = Preferences {
                keymap_preset: Preset::MsIme,
                normalizer: self.normalizer,
                prediction_enabled: self.prediction_enabled,
                suggest_accept: self.suggest_accept,
                developer_mode: self.input_history.is_some(),
            };
            let resolved = resolve_context_preferences(global, &self.app_profiles, process_name);
            let mut fresh = Session::new(process_name);
            fresh.apply_context_preferences(resolved);
            fresh
        } else {
            existing.clone()
        };
        // This is the same transition used by the real SetInputScope request,
        // but it is applied only to the fixed-capacity clone. In particular,
        // no link/session/cache state is changed by a TSF test callback.
        probe.apply_input_scope(scope);
        self.probe_session(id, probe, key, out, fresh_context)
    }

    fn probe_session(
        &mut self,
        id: SessionId,
        mut probe: Session,
        key: &KeyInput,
        out: &mut OutputBuf,
        fresh_context: bool,
    ) -> Reply {
        let normalizer = probe.normalizer;
        let services = KeyServices {
            table: &self.table,
            keymap: &self.keymap,
            normalizer: &normalizer,
            conversion: self.conversion.as_deref(),
            learning: self.learning.as_deref(),
            input_history: self.input_history.as_deref(),
            prediction: self.prediction.as_deref(),
            long_conversion: None,
            long_conversion_owner: 0,
            prediction_enabled: probe.prediction_enabled,
            suggest_accept: probe.suggest_accept,
        };
        // Run the real key logic against a throwaway clone. `Session` is
        // fixed-capacity plain data, so this clone is allocation-free; the
        // Probe zero-allocation regression covers both the legacy SendKey
        // test-only path and the scope-carrying ProbeKey path. Probe may read
        // the live candidate result through an immutable view, while stale
        // learning epochs and fresh-context transitions are represented as an
        // empty logical cache.
        let probe_cache = PredictionCacheWork::Probe {
            cache: &self.prediction_cache,
            stale: fresh_context
                || self.learning.as_deref().is_some_and(|learning| {
                    learning.generation() != self.observed_learning_generation
                }),
        };
        let _ = apply_key(
            id,
            &mut probe,
            &services,
            key,
            KeyWork {
                policy: ExecutionPolicy::Probe,
                prediction_cache: probe_cache,
                scratch: &mut self.scratch,
                out,
            },
        );
        Reply::Output
    }

    fn commit(&mut self, id: SessionId, out: &mut OutputBuf) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if session.undo_pending() {
            // Focus finalization is not an undo terminal outcome. Leave the
            // restored composition and exact record untouched until TSF
            // reports Applied, Rejected, or Unknown explicitly.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        session.disarm_commit_undo();
        let normalizer = session.normalizer;
        match commit_pending(
            session,
            &self.table,
            &normalizer,
            self.conversion.as_deref(),
            self.learning.as_deref(),
            self.input_history.as_deref(),
            ExecutionPolicy::Apply,
            &mut self.scratch,
            out,
        ) {
            Ok(()) => {
                self.prediction_cache.clear_if_session(id);
                Reply::Output
            }
            Err(Overflow) => {
                // `commit_pending` is internally atomic (see its own docs
                // and `commit_converted_segments`'s): a failing fallible
                // write inside it leaves every `Session` field untouched.
                // `commit()` never reaches `Action::UndoCommit`'s
                // undo-pending path (that is a `send_key` action, and
                // `commit()` calls `commit_pending` directly), so there is
                // no transaction to reconcile here either.
                self.prediction_cache.clear_if_session(id);
                out.clear();
                Reply::Message(Response::Error(ErrorCode::TooLarge))
            }
        }
    }

    /// Builds conversion candidates for text that already exists in the host
    /// document. Preview requests are evaluated against a cloned session for
    /// `ITfFnReconversion::GetReconversion`; an actual request replaces the
    /// live session state and is later rendered over the selected TSF range.
    fn reconvert(
        &mut self,
        id: SessionId,
        text: &str,
        preview: bool,
        out: &mut OutputBuf,
    ) -> Reply {
        let Some(existing) = self.sessions.get(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if existing.undo_pending() {
            // Reconversion would replace the restored reading and can never
            // be the terminal acknowledgement for the pending host edit.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        if text.is_empty() {
            return Reply::Message(Response::Error(ErrorCode::Malformed));
        }
        if text.len() > MAX_PREEDIT_BYTES {
            return Reply::Message(Response::Error(ErrorCode::TooLarge));
        }
        if scope_is_sensitive(existing.scope) {
            // Reconversion must never pull selected password text into either
            // the dictionary or learning path. An actual request also leaves
            // the context in an explicit empty terminal state.
            if !preview {
                if let Some(session) = self.sessions.get_mut(id) {
                    session.reset();
                }
                self.prediction_cache.clear_if_session(id);
            }
            return Reply::Message(Response::Error(ErrorCode::Malformed));
        }
        let Some(conversion) = self.conversion.as_deref() else {
            return Reply::Message(Response::Error(ErrorCode::Busy));
        };

        if preview {
            let mut probe = existing.clone();
            return match build_reconversion(
                &mut probe,
                text,
                &self.table,
                conversion,
                self.learning.as_deref(),
                &mut self.scratch,
                out,
            ) {
                Ok(()) => Reply::Output,
                Err(code) => {
                    out.clear();
                    Reply::Message(Response::Error(code))
                }
            };
        }

        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        self.prediction_cache.clear_if_session(id);
        match build_reconversion(
            session,
            text,
            &self.table,
            conversion,
            self.learning.as_deref(),
            &mut self.scratch,
            out,
        ) {
            Ok(()) => Reply::Output,
            Err(code) => {
                session.reset();
                out.clear();
                Reply::Message(Response::Error(code))
            }
        }
    }

    fn revert(&mut self, id: SessionId) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if session.undo_pending() {
            // Revert would discard the restored reading without telling the
            // host whether its exact deletion happened.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        session.disarm_commit_undo();
        session.reset();
        self.prediction_cache.clear_if_session(id);
        Reply::Message(Response::Ok)
    }

    fn undo_commit_outcome(&mut self, id: SessionId, outcome: UndoCommitOutcome) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        let terminal = match outcome {
            UndoCommitOutcome::Applied => session.acknowledge_undo_commit(),
            UndoCommitOutcome::Rejected => session.reject_undo_commit(),
            UndoCommitOutcome::Unknown => session.abort_undo_commit(),
        };
        if !terminal {
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        // The undo preview may have invalidated or filled a prediction entry;
        // every terminal outcome starts the cache from the reconciled session
        // state, including a rejection that restored the committed document.
        self.prediction_cache.clear_if_session(id);
        Reply::Message(Response::Ok)
    }
}

/// Read-only services needed to resolve one key. Grouping these disjoint
/// dispatcher fields keeps the stateful operation's ownership explicit.
struct KeyServices<'a> {
    table: &'a Table,
    keymap: &'a KeyMap,
    normalizer: &'a Normalizer,
    conversion: Option<&'a ConversionService>,
    learning: Option<&'a LearningService>,
    input_history: Option<&'a InputHistoryService>,
    prediction: Option<&'a PredictionService>,
    long_conversion: Option<&'a LongConversionService>,
    long_conversion_owner: u64,
    prediction_enabled: bool,
    suggest_accept: SuggestAccept,
}

/// Whether a key is being evaluated for `OnTestKeyDown` or applied to the
/// live session. Probe shares the Apply state machine, but every operation
/// capable of reaching durable services or the live prediction cache must be
/// explicitly unavailable in Probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecutionPolicy {
    Probe,
    Apply,
}

impl ExecutionPolicy {
    const fn allows_persistence(self) -> bool {
        matches!(self, Self::Apply)
    }

    const fn allows_prediction_request(self) -> bool {
        matches!(self, Self::Apply)
    }

    const fn allows_prediction_cache_mutation(self) -> bool {
        matches!(self, Self::Apply)
    }
}

fn state_code(state: State) -> u8 {
    match state {
        State::Idle => 0,
        State::Composing => 1,
        State::Converting => 2,
        State::Predicting => 3,
    }
}

/// Mutable single-keystroke work state. Keeping these buffers together makes
/// the hot path's ownership and prediction-request policy one explicit unit.
struct KeyWork<'a> {
    policy: ExecutionPolicy,
    prediction_cache: PredictionCacheWork<'a>,
    scratch: &'a mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &'a mut OutputBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PredictionCache {
    attempted: bool,
    explicit_retry_attempted: bool,
    session: SessionId,
    generation: u64,
    has_result: bool,
    result: PredictionResult,
}

/// Cache access granted to one key-resolution pass.
///
/// Apply owns the mutable cache and may refresh it. Probe only borrows the
/// existing result and can make a stale generation appear empty. Keeping the
/// two capabilities in distinct enum variants prevents a future Probe branch
/// from accidentally clearing, filling, or requesting through the live cache,
/// while avoiding a fixed-result clone on every physical test key.
enum PredictionCacheWork<'a> {
    Apply(&'a mut PredictionCache),
    Probe {
        cache: &'a PredictionCache,
        stale: bool,
    },
}

impl PredictionCacheWork<'_> {
    fn candidates(
        &self,
        session: SessionId,
        generation: u64,
    ) -> Option<&[crate::prediction::PredictionCandidate]> {
        match self {
            Self::Apply(cache) => cache.candidates(session, generation),
            Self::Probe { cache, stale } if !stale => cache.candidates(session, generation),
            Self::Probe { .. } => None,
        }
    }

    fn attempted_for(&self, session: SessionId, generation: u64) -> bool {
        match self {
            Self::Apply(cache) => cache.attempted_for(session, generation),
            Self::Probe { cache, stale } if !stale => cache.attempted_for(session, generation),
            Self::Probe { .. } => false,
        }
    }

    fn explicit_retry_attempted_for(&self, session: SessionId, generation: u64) -> bool {
        match self {
            Self::Apply(cache) => cache.explicit_retry_attempted_for(session, generation),
            Self::Probe { cache, stale } if !stale => {
                cache.explicit_retry_attempted_for(session, generation)
            }
            Self::Probe { .. } => false,
        }
    }

    fn clear(&mut self) {
        if let Self::Apply(cache) = self {
            cache.clear();
        }
    }

    fn clear_if_session(&mut self, session: SessionId) {
        if let Self::Apply(cache) = self {
            cache.clear_if_session(session);
        }
    }

    fn apply_mut(&mut self) -> Option<&mut PredictionCache> {
        match self {
            Self::Apply(cache) => Some(*cache),
            Self::Probe { .. } => None,
        }
    }
}

impl PredictionCache {
    fn new() -> Self {
        Self {
            attempted: false,
            explicit_retry_attempted: false,
            session: 0,
            generation: 0,
            has_result: false,
            result: PredictionResult::default(),
        }
    }

    fn clear(&mut self) {
        self.attempted = false;
        self.explicit_retry_attempted = false;
        self.session = 0;
        self.generation = 0;
        self.has_result = false;
    }

    fn clear_if_session(&mut self, session: SessionId) {
        if self.attempted && self.session == session {
            self.clear();
        }
    }

    fn attempted_for(&self, session: SessionId, generation: u64) -> bool {
        self.attempted && self.session == session && self.generation == generation
    }

    fn explicit_retry_attempted_for(&self, session: SessionId, generation: u64) -> bool {
        self.attempted_for(session, generation) && self.explicit_retry_attempted
    }

    fn candidates(
        &self,
        session: SessionId,
        generation: u64,
    ) -> Option<&[crate::prediction::PredictionCandidate]> {
        (self.has_result
            && self.result.session() == session
            && self.result.generation() == generation)
            .then(|| self.result.candidates())
    }
}

fn conversion_options(session: &Session, initial_right_id: u16) -> ConversionOptions {
    let mut options = ConversionOptions {
        initial_right_id,
        ..ConversionOptions::default()
    };
    // The recent IT ratio strengthens the shipped prior gradually and with a
    // hard cap, preventing the positive-feedback loop DESIGN §5.8 warns about.
    let coherence = session.domain_it_ratio_per_mille();
    options.it_bias_per_mille = options
        .it_bias_per_mille
        .saturating_add((coherence / 5).min(150));
    options
}

fn schedule_long_conversion(session_id: SessionId, session: &Session, services: &KeyServices<'_>) {
    let Some(long_conversion) = services.long_conversion else {
        return;
    };
    if !session.scope_classified()
        || session.scope() != InputScope::Normal
        || session.converting
        || session.shifted_ascii
        || session.preedit.is_empty()
        || usize::from(session.cursor) != session.preedit.as_str().chars().count()
    {
        return;
    }
    let options = conversion_options(session, session.carry_right_id());
    let _ = long_conversion.schedule(
        services.long_conversion_owner,
        session_id,
        session.prediction_generation,
        session.preedit.as_str(),
        options,
    );
}

fn preferred_candidate_index(
    candidates: &[ConversionCandidate],
    requested: i16,
    cached: Option<(u64, u16)>,
    learned: LearningPreference,
) -> usize {
    if candidates.is_empty() {
        return 0;
    }
    if requested == 0 {
        if let Some(index) = learned.exact.filter(|index| *index < candidates.len()) {
            return index;
        }
        if let Some((surface_hash, surface_len)) = cached {
            if let Some(index) = candidates.iter().position(|candidate| {
                u16::try_from(candidate.text().len()).ok() == Some(surface_len)
                    && text_hash(candidate.text()) == surface_hash
            }) {
                return index;
            }
        }
        if let Some(index) = learned.general.filter(|index| *index < candidates.len()) {
            return index;
        }
    }
    requested.rem_euclid(candidates.len() as i16) as usize
}

fn has_authoritative_candidate_preference(
    candidates: &[ConversionCandidate],
    requested: i16,
    cached: Option<(u64, u16)>,
    learned: LearningPreference,
) -> bool {
    if requested != 0 {
        return true;
    }
    if learned.exact.is_some_and(|index| index < candidates.len())
        || learned
            .general
            .is_some_and(|index| index < candidates.len())
    {
        return true;
    }
    cached.is_some_and(|(surface_hash, surface_len)| {
        candidates.iter().any(|candidate| {
            u16::try_from(candidate.text().len()).ok() == Some(surface_len)
                && text_hash(candidate.text()) == surface_hash
        })
    })
}

#[derive(Clone, Copy, Default)]
struct CommitSegmentMeta {
    right_id: u16,
    it_words: u8,
    total_words: u8,
}

fn candidate_meta(candidate: &ConversionCandidate) -> CommitSegmentMeta {
    let segments = candidate.segments();
    CommitSegmentMeta {
        right_id: segments.last().map_or(0, |segment| segment.right_id),
        it_words: u8::try_from(
            segments
                .iter()
                .filter(|segment| segment.flags.contains(EntryFlags::IT))
                .count(),
        )
        .unwrap_or(u8::MAX),
        total_words: u8::try_from(segments.len()).unwrap_or(u8::MAX),
    }
}

/// Persists a commit while its reading and left context are still present in
/// the session. Sensitive scopes never reach the store, even if a future
/// frontend changes their composition policy.
fn record_learning(
    session: &Session,
    learning: Option<&LearningService>,
    input_history: Option<&InputHistoryService>,
    policy: ExecutionPolicy,
    surface: &str,
    chosen_right_id: u16,
) {
    if !policy.allows_persistence() {
        return;
    }
    let scope = ScopeClass::from_scope(session.scope, session.scope_classified());
    if scope != ScopeClass::Sensitive {
        if let Some(history) = input_history {
            history.record_commit(
                session.history_session_id(),
                scope,
                session.preedit.as_str(),
                surface,
                session.carry_right_id(),
                chosen_right_id,
            );
        }
    }
    if scope_is_sensitive(session.scope) {
        return;
    }
    if let Some(service) = learning {
        service.learn(
            session.preedit.as_str(),
            surface,
            session.carry_right_id(),
            chosen_right_id,
        );
    }
}

fn candidate_learning_key(candidate: &ConversionCandidate) -> (&str, u16) {
    (
        candidate.text(),
        candidate
            .segments()
            .last()
            .map_or(0, |segment| segment.right_id),
    )
}

/// Returns whether an action is allowed to leave normal Direct mode.
///
/// Password fields are rejected before the key map is consulted. This helper
/// therefore governs only ordinary application contexts, where an IME needs
/// to let its own explicit mode keys through while continuing to hand all
/// normal typing back to the host.
fn is_mode_switch(action: Option<Action>) -> bool {
    matches!(
        action,
        Some(
            Action::ImeToggle
                | Action::ImeOn
                | Action::ImeOff
                | Action::ModeHiragana
                | Action::ModeKatakana
                | Action::ModeHalfKatakana
                | Action::ModeFullAlnum
                | Action::ModeHalfAlnum
                | Action::ModeDirect
                | Action::ModeKanaToggle
                | Action::ModeKanaCycle
                | Action::ModeAlnumToggle
                | Action::ModeAlnumWidthToggle
        )
    )
}

/// Resolves one keystroke against `session`, mutating its composition and
/// writing the visible result into `out`. Free rather than a `Dispatcher`
/// method so its callers (`Dispatcher::send_key`, and transitively every
/// mode switch) can hold a `&mut Session` borrowed from `self.sessions`
/// alongside `&self.table`/`&self.keymap`/`&self.normalizer`/`&mut
/// self.scratch` at the same time -- those are disjoint fields of the same
/// `Dispatcher`, which the borrow checker can only see through when they
/// are named directly, not funnelled through a `&mut self` method.
fn apply_key(
    session_id: SessionId,
    session: &mut Session,
    services: &KeyServices<'_>,
    key: &KeyInput,
    work: KeyWork<'_>,
) -> Result<(), Overflow> {
    let KeyWork {
        policy,
        mut prediction_cache,
        scratch,
        out,
    } = work;
    if scope_is_sensitive(session.scope) {
        // DESIGN 9: a password field is a full bypass. In particular, do not
        // perform a key-map lookup here: custom maps can bind character keys,
        // and even deciding whether one matched would inspect sensitive input.
        out.consumed = false;
        return Ok(());
    }

    if session.suggestion_focused
        && prediction_cache
            .candidates(session_id, session.prediction_generation)
            .is_none()
    {
        // Focus is meaningful only while the live cache proves the selected
        // surface. Clear it before resolving state or action so a missing or
        // stale cache cannot select a Predicting-only key binding.
        session.hide_suggestions();
    }
    let state = session.state();
    let mut action = services.keymap.lookup(state, key);
    // A number is only a candidate shortcut once the list it addresses owns
    // keyboard focus. Honouring it from a merely visible suggestion list took
    // every digit away from the composition: typing `22` selected the second
    // suggestion offered for `２` instead of producing `２２`, and there was no
    // way to type a digit at all while suggestions happened to be on screen.
    // Tab or ↓ focuses the list and restores the shortcuts, and the conversion
    // candidate window reached with Space keeps them throughout.
    //
    // History deletion is different: it is a chord no composition can produce,
    // and leaving it unclaimed would forward Ctrl+Delete to the application
    // mid-composition. It stays claimed here so the unfocused case is rejected
    // by the IME rather than deleting a word in the user's editor.
    if action.is_none() && !session.converting && session.suggestions_visible {
        action = services
            .keymap
            .lookup(State::Predicting, key)
            .filter(|action| *action == Action::DeletePredictionHistory);
    }
    if session.mode == Mode::Direct && !is_mode_switch(action) {
        // Direct mode passes normal typing and all non-mode bindings through
        // to the application. It must still admit an explicit IME mode switch,
        // otherwise `ImeToggle` can turn the IME off but can never turn it
        // back on. This is how the Japanese half-width/full-width key behaves
        // in other Windows IMEs.
        out.consumed = false;
        return Ok(());
    }
    let prediction_direction = match action {
        Some(Action::PredictNext) => Some(1),
        Some(Action::PredictPrev) => Some(-1),
        _ => None,
    };
    if !matches!(action, Some(Action::UndoCommit)) {
        session.disarm_commit_undo();
    }
    match action {
        Some(action) => apply_action(
            session_id,
            session,
            action,
            services,
            policy,
            &mut prediction_cache,
            scratch,
            out,
        )?,

        // A Ctrl or Alt chord the key map did not claim is an application
        // shortcut. The DLL reports it as the letter plus the modifier
        // bits -- deliberately, so that a key map *can* bind Ctrl+J -- and
        // without this arm the fall-through below would treat the letter
        // as input and make Ctrl+S insert "s" instead of saving the user's
        // file. Nothing an IME converts is worth breaking every editor it
        // is installed in.
        None if key.modifiers.ctrl() || key.modifiers.alt() => out.consumed = false,

        // Typing a new character while candidates are open accepts the
        // focused candidate first, then starts the next composition. This is
        // an explicit transition out of `Converting`; the new character can
        // never be appended invisibly to the old reading.
        None if session.converting && key.ch.is_some() => {
            commit_pending(
                session,
                services.table,
                services.normalizer,
                services.conversion,
                services.learning,
                services.input_history,
                policy,
                scratch,
                out,
            )?;
            out.consumed = true;
            feed_character(
                session,
                services.table,
                key.ch.expect("guarded by is_some"),
                key.modifiers.shift(),
                scratch,
            )?;
            // This same keystroke both accepted the old candidate and began a
            // new composition, so the depth-one undo window has already
            // expired by definition.
            session.disarm_commit_undo();
        }

        None if session.mode == Mode::FullAlnum || session.mode == Mode::HalfAlnum => {
            apply_alnum_char(session, services.normalizer, key, out)?;
        }
        None => match key.ch {
            Some(ch) => {
                out.consumed = true;
                feed_character(session, services.table, ch, key.modifiers.shift(), scratch)?;
            }
            None => out.consumed = false,
        },
    }

    let explicit_prediction = prediction_direction.filter(|_| out.consumed);
    refresh_prediction(
        session_id,
        session,
        services,
        &mut prediction_cache,
        policy,
        explicit_prediction,
    );
    if let Some(direction) = explicit_prediction {
        focus_prediction_after_refresh(session_id, session, &prediction_cache, direction, out);
    }
    render_preedit(
        session,
        services.table,
        services.normalizer,
        services.conversion,
        scratch,
        out,
    )?;
    render_suggestions(
        session_id,
        session,
        services.normalizer,
        services.conversion,
        &prediction_cache,
        scratch,
        out,
    )?;
    render_prediction_projection(
        session_id,
        session,
        services.normalizer,
        &prediction_cache,
        scratch,
        out,
    )
}

/// How many leading characters of `raw` (ASCII keystrokes) resolve to
/// exactly `target` characters of `expected`, found by replaying `raw`
/// through a romaji FSM state.
///
/// `expected` is the ground truth `raw` is supposed to reproduce: the slice
/// of `preedit` that starts at the same position `raw` does (see callers
/// for the exact slice each one passes). A from-scratch replay is *not*
/// sound on its own -- it assumes every raw byte still in `raw_input` was
/// fed to the FSM continuously, live, in one unbroken run. That assumption
/// is false once a carry survives a Backspace that clears
/// `romaji.pending()` to empty without deleting a raw byte, exactly what
/// happens whenever `pending_source_range.end <= raw_boundary` (see the
/// provenance contract above): `raw_input` keeps no record that the
/// discarded carry's live FSM continuity ended there, so a blind replay can
/// re-extend it into whatever raw bytes come next, hit no matching entry,
/// and fall back to a literal passthrough of a byte that, live, was never
/// part of any emission at all. That fallback is correct, load-bearing FSM
/// behavior in isolation -- see
/// `every_carrying_entry_resolves_deterministically_when_flushed_alone` in
/// romaji.rs -- so it cannot change; the unsoundness is entirely in trusting
/// a replay that cannot see the Backspace to reproduce it faithfully.
///
/// The fix is to not just trust the replay's own emissions: `expected` is
/// always a genuinely known-good answer (real `preedit`, produced by real
/// live typing), so each emission is compared against it, character by
/// character, as it comes out. The moment one does not match, the FSM's
/// current candidate must be a replay artifact -- live typing could not
/// have produced it, since `expected` says otherwise -- so it is discarded:
/// state resets to fresh and the walk resumes from the raw index right
/// after the last *verified* checkpoint, not from the mismatch itself.
///
/// A checkpoint only advances on a step that actually emitted something
/// `expected` confirmed -- `candidate_matched` strictly increasing. A step
/// that merely extends `pending` with no emission at all "matches" only
/// vacuously (there was nothing to compare), so it cannot anchor a rewind
/// target: replaying a discarded carry into unrelated text can wait
/// silently for one or more further characters before the mismatch it
/// causes becomes visible (a carried `t` plus a fresh `s` is itself a valid
/// prefix -- of `tsu` -- so it keeps waiting rather than failing at `s`; the
/// mismatch only surfaces once a vowel completes it into the wrong kana).
/// Rewinding only to the last *step that produced output* correctly lands
/// before that silent stretch, right where the carry's live continuity
/// actually ended.
///
/// Retrying can still reach the exact same mismatch again -- if nothing
/// between two resets ever advances the checkpoint, replaying the same raw
/// characters from the same fresh state is deterministic and repeats
/// itself -- so a reset onto a checkpoint already retried without any
/// progress since is refused rather than looped.
///
/// This runs on the key-input hot path (`next_raw_boundary`, read by every
/// `feed_character` insertion) and must stay allocation-free like the rest
/// of it (`zero_alloc_dispatch` enforces this): [`MatchSink`] compares each
/// emitted character against `expected` as `table.feed` produces it, so
/// nothing here ever buffers a `String` or collects a `Vec<char>`.
fn raw_chars_for_emitted(table: &Table, raw: &str, expected: &str, target: usize) -> usize {
    if target == 0 {
        return 0;
    }
    // `raw` is ASCII-only (see callers), so its bytes are its characters and
    // there is no need to collect `raw.chars()` into an owned buffer first.
    let raw_bytes = raw.as_bytes();
    let mut state = Input::new();
    let mut matched = 0usize;
    let mut safe_index = 0usize;
    let mut safe_matched = 0usize;
    let mut last_reset: Option<usize> = None;
    let mut index = 0usize;
    while index < raw_bytes.len() {
        let mut sink = MatchSink {
            expected,
            matched,
            mismatch: false,
        };
        // Mirror `feed_character`'s direct decimal path during provenance
        // replay. `expected` proves that the live state chose the literal
        // output; without it, a user-customized table remains authoritative.
        let literal_decimal_period = raw_bytes[index] == b'.'
            && index > 0
            && raw_bytes[index - 1].is_ascii_digit()
            && state.is_empty()
            && expected.chars().nth(matched) == Some('.');
        let feed_result = if literal_decimal_period {
            sink.push('.')
        } else {
            table.feed(&mut state, raw_bytes[index] as char, &mut sink)
        };
        if feed_result.is_err() {
            // This exact prefix already produced a bounded `preedit` once,
            // live; if replaying it here still overflows the sink, stop at
            // what has been read rather than guess past it.
            return index + 1;
        }
        if sink.mismatch {
            if last_reset == Some(safe_index) {
                // Already retried from this exact checkpoint once with no
                // verified progress in between; a deterministic replay from
                // the same fresh state over the same raw characters would
                // only repeat the same outcome, so stop instead of looping.
                return index + 1;
            }
            last_reset = Some(safe_index);
            state = Input::new();
            matched = safe_matched;
            index = safe_index;
            continue;
        }
        let progressed = sink.matched > matched;
        matched = sink.matched;
        if matched >= target {
            return index + 1;
        }
        index += 1;
        if progressed {
            safe_index = index;
            safe_matched = matched;
        }
    }
    raw_bytes.len()
}

/// Compares each character [`Table::feed`] emits, live, against `expected`
/// starting at char offset `matched`, without buffering the emission first
/// -- see [`raw_chars_for_emitted`] for why this replay must not allocate.
/// `mismatch` latches: once one character disagrees with `expected`, later
/// characters in the same emission are still consumed (a sink must accept
/// what it is given) but no longer compared, since the caller discards the
/// whole step on any mismatch regardless of how much of it was right.
struct MatchSink<'a> {
    expected: &'a str,
    matched: usize,
    mismatch: bool,
}

impl TextSink for MatchSink<'_> {
    fn push_str(&mut self, s: &str) -> Result<(), Overflow> {
        for ch in s.chars() {
            if self.mismatch {
                continue;
            }
            let expected = self.expected.chars().nth(self.matched);
            if expected == Some(ch) {
                self.matched += 1;
            } else {
                self.mismatch = true;
            }
        }
        Ok(())
    }
}

/// Removes `count` characters from `buf` starting at character offset
/// `start`. `raw_input` only ever holds ASCII (see `feed_character`), so
/// character and byte offsets coincide, but this still walks
/// [`FixedStr::remove_char_at`] one character at a time rather than assume
/// that in a byte-range splice, since removing at a fixed offset repeatedly
/// is exactly a contiguous range removal regardless.
fn remove_raw_chars<const N: usize>(buf: &mut FixedStr<N>, start: usize, count: usize) {
    for _ in 0..count {
        if buf.remove_char_at(start).is_none() {
            break;
        }
    }
}

/// Removes byte range `range` from `buf`. Both ends must already fall on
/// character boundaries -- every caller derives `range` from
/// [`raw_chars_for_emitted`] and [`FixedStr::byte_index`], which only ever
/// produce boundaries. Walks [`FixedStr::remove_char_at`] one character at a
/// time from `range.start`: removing a character shifts everything after it
/// left, so `range.start` stays valid for the next character in the span
/// throughout.
fn remove_raw_range<const N: usize>(buf: &mut FixedStr<N>, range: core::ops::Range<usize>) {
    let count = buf
        .as_str()
        .get(range.clone())
        .map_or(0, |s| s.chars().count());
    for _ in 0..count {
        if buf.remove_char_at(range.start).is_none() {
            break;
        }
    }
}

/// # `raw_input`'s provenance contract (#16 findings B/C)
///
/// `raw_input` holds the raw keystrokes behind the *currently visible*
/// `preedit` and `romaji.pending()` -- it is not an unwound log of every
/// key ever pressed this session (Backspace/Delete-forward already remove
/// from it as they run). Three positions describe where in it a given
/// piece of state lives:
///
/// - `raw_boundary` ([`raw_byte_offset_for_preedit_cursor`]): the byte
///   offset immediately after the keystrokes behind `preedit[..cursor]`.
/// - [`pending_source_range`]: the byte range that is `romaji.pending()`'s
///   own raw source. Ordinarily this starts exactly at `raw_boundary` --
///   pending is whatever was typed right after the last resolved kana. A
///   sokuon/carry breaks that assumption: the second "t" of "tt" resolves
///   to "っ" *and* is carried forward as the new pending "t", so that one
///   raw byte is simultaneously "っ"'s source (already counted in
///   `raw_boundary`) and the carried pending's source.
///   `pending_source_range` accounts for this by starting
///   `romaji.carry_overlap()` bytes *before* `raw_boundary` instead of
///   assuming pending's source can never precede it.
/// - [`next_raw_boundary`]: `max(raw_boundary, pending_source_range.end)`
///   -- the one correct place to look for "the next raw span", used both
///   to insert a fresh keystroke (`feed_character`) and to locate the next
///   resolved kana's raw span to remove (`raw_range_for_next_emitted`,
///   Delete-forward). `raw_boundary` alone is wrong here whenever
///   something is pending; `pending_source_range.end` alone is wrong
///   precisely in the carry case above, where that end lands *at*
///   `raw_boundary` rather than past it.
///
/// Backspace over pending romaji follows the same contract: it deletes a
/// raw byte only when `pending_source_range.end > raw_boundary` -- pending
/// owns a byte no emitted kana's provenance depends on. When
/// `pending_source_range.end <= raw_boundary`, pending is wholly a carry
/// with no byte of its own to give back; clearing it (`Input::backspace`)
/// reverts FSM state only, and `raw_input` -- still that emitted kana's
/// provenance -- stays untouched. See `apply_backspace`.
///
/// The byte offset in `raw_input` immediately after the keystrokes that
/// produced `preedit[..session.cursor]`, and immediately before whatever
/// romaji is currently pending (`raw_boundary` above).
///
/// Derived fresh from `session.cursor` on every call instead of being
/// cached anywhere on `Session`: a second persisted offset would need to
/// stay in lock-step with `cursor` across every edit, commit, reset and
/// reconversion path, which is exactly the kind of desynchronization #16
/// findings B/C were caused by in the first place. A pure function of the
/// one existing cursor cannot drift out of sync with it.
fn raw_byte_offset_for_preedit_cursor(table: &Table, session: &Session) -> usize {
    let cursor = usize::from(session.cursor);
    let raw = session.raw_input.as_str();
    let raw_chars = raw_chars_for_emitted(table, raw, session.preedit.as_str(), cursor);
    session.raw_input.byte_index(raw_chars).unwrap_or(raw.len())
}

/// The byte range in `raw_input` that is the currently pending romaji's own
/// raw source (see the provenance contract above). Empty (a zero-length
/// range at `raw_boundary`) when nothing is pending.
///
/// `start` is `raw_boundary` pulled back by `romaji.carry_overlap()` bytes
/// rather than always `raw_boundary` itself: a carry's overlap bytes are
/// *also* counted in `raw_boundary`, since they are part of some
/// already-emitted kana's source too, so counting them again ahead of
/// `raw_boundary` would double their length in `raw_input` instead of
/// sharing it. `end` is defensively clamped to `raw_input`'s actual length
/// -- not load-bearing for the single carry the shipped table produces
/// (whose overlap already keeps `end` within bounds exactly), but cheap
/// insurance against a custom table whose carries compose in a way this
/// crate has not been asked to reason about.
///
/// Deliberately does not assert the slice equals `romaji.pending()`: that
/// is false whenever `carry_overlap() > 0` (the overlapping bytes belong to
/// emitted text too, so the slice is shorter than a literal copy would be)
/// and in `shifted_ascii` mode, where the table folds case for lookup so
/// `pending()` can be lowercase while `raw_input` keeps the literal typed
/// case. Both are legitimate and out of scope here.
fn pending_source_range(table: &Table, session: &Session) -> core::ops::Range<usize> {
    let boundary = raw_byte_offset_for_preedit_cursor(table, session);
    let start = boundary.saturating_sub(session.romaji.carry_overlap());
    let raw_len = session.raw_input.as_str().len();
    let end = raw_len.min(start.saturating_add(session.romaji.pending().len()));
    start..end
}

/// `max(raw_boundary, pending_source_range.end)` -- see the provenance
/// contract above for why neither alone is correct once a carry is
/// involved.
fn next_raw_boundary(table: &Table, session: &Session) -> usize {
    let boundary = raw_byte_offset_for_preedit_cursor(table, session);
    let pending_end = pending_source_range(table, session).end;
    boundary.max(pending_end)
}

/// Returns whether the character immediately before the caret is a literal
/// half-width digit, both in the visible preedit and its raw keystroke
/// provenance. Requiring an empty pending-romaji state prevents `1n.` from
/// being treated as a decimal merely because the unresolved `n` has not yet
/// appeared in `preedit`.
fn ascii_digit_immediately_before_cursor(session: &Session, table: &Table) -> bool {
    if !session.romaji.is_empty() {
        return false;
    }
    let cursor_at = session
        .preedit
        .byte_index(usize::from(session.cursor))
        .unwrap_or(session.preedit.len());
    let previous_preedit_is_digit = session
        .preedit
        .as_str()
        .get(..cursor_at)
        .and_then(|prefix| prefix.chars().next_back())
        .is_some_and(|character| character.is_ascii_digit());
    if !previous_preedit_is_digit {
        return false;
    }
    let raw_boundary = raw_byte_offset_for_preedit_cursor(table, session);
    session
        .raw_input
        .as_str()
        .get(..raw_boundary)
        .and_then(|prefix| prefix.as_bytes().last())
        .is_some_and(|byte| byte.is_ascii_digit())
}

/// The byte range in `raw_input` for the single resolved kana character at
/// `preedit[cursor]` -- the raw keystrokes Delete-forward must remove.
///
/// Always starts at [`next_raw_boundary`], not merely after any pending
/// romaji (#16 finding B/C): pending has not resolved into `preedit` yet,
/// so it is never "the next emitted character", and once a carry is
/// involved `pending_source_range.end` alone can land inside
/// already-emitted text rather than past it (see the provenance contract
/// above). Once the caret has moved ahead of already-resolved text and a
/// keystroke is pending there, that resolved text sits in `raw_input`
/// *after* the pending span, not at the cursor's raw offset directly.
/// Replays only the raw text after that boundary from a fresh FSM state,
/// rather than all of `raw_input` from index 0 -- a pending span spliced
/// ahead of already-resolved text breaks a whole-buffer replay's
/// correspondence to `preedit` positions, which is exactly the bug this
/// replaces.
///
/// Returns `None` when the cursor is already at the end of `preedit` and
/// there is nothing to delete.
fn raw_range_for_next_emitted(table: &Table, session: &Session) -> Option<core::ops::Range<usize>> {
    let cursor = usize::from(session.cursor);
    let preedit = session.preedit.as_str();
    if cursor >= preedit.chars().count() {
        return None;
    }
    let boundary = next_raw_boundary(table, session);
    let raw = session.raw_input.as_str();
    let after = raw.get(boundary..).unwrap_or("");
    // `after`'s ground truth is `preedit[cursor..]`, not the whole of
    // `preedit`: `after` itself already starts past `preedit[..cursor]`'s
    // own raw source, so the two must stay aligned to the same start.
    let expected_after = session
        .preedit
        .byte_index(cursor)
        .and_then(|at| preedit.get(at..))
        .unwrap_or("");
    let raw_chars = raw_chars_for_emitted(table, after, expected_after, 1);
    let end = after
        .char_indices()
        .nth(raw_chars)
        .map_or(raw.len(), |(offset, _)| boundary + offset);
    Some(boundary..end)
}

/// The slice of `raw_input` that produced `preedit[range]` -- a single
/// segment's reading -- for handing to a segment's own
/// `SegmentTransform::FullAlnum`/`HalfAlnum` render/commit instead of the
/// whole composition's keystrokes (#16 finding D). Degenerates to the whole
/// of `raw_input` when `range` spans all of `preedit`, which is the common
/// single-segment F6-F10 case (`apply_transform`), so that path is
/// unaffected.
fn segment_raw_text<'a>(
    table: &Table,
    preedit: &str,
    raw_input: &'a str,
    range: core::ops::Range<usize>,
) -> &'a str {
    let start_chars = preedit.get(..range.start).map_or(0, |s| s.chars().count());
    let end_chars = preedit
        .get(..range.end)
        .map_or(start_chars, |s| s.chars().count());
    let raw_start = raw_chars_for_emitted(table, raw_input, preedit, start_chars);
    let raw_end = raw_chars_for_emitted(table, raw_input, preedit, end_chars).max(raw_start);
    // ASCII-only, so these character offsets are also valid byte offsets.
    raw_input.get(raw_start..raw_end).unwrap_or("")
}

fn feed_character(
    session: &mut Session,
    table: &Table,
    character: char,
    shifted: bool,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
) -> Result<(), Overflow> {
    let starts_shifted_ascii = session.raw_input.is_empty()
        && session.preedit.is_empty()
        && session.romaji.is_empty()
        && shifted
        && character.is_ascii_alphabetic();
    // Every write below lands in a local clone first, and only replaces the
    // matching `Session` field once every fallible step in this function has
    // already succeeded. This keeps a single keystroke that cannot fit
    // atomic: on `Err(Overflow)`, `session` is left exactly as it was on
    // entry, instead of holding a `raw_input`/`romaji` advanced past a
    // `preedit` that never received the matching text.
    // The Shift on the first ASCII letter chooses the temporary English
    // composition. It stays chosen for following ASCII input even after Shift
    // is released, and ends only when the composition ends or receives a
    // non-ASCII character.
    let shifted_ascii = if starts_shifted_ascii {
        true
    } else if session.shifted_ascii && !character.is_ascii() {
        false
    } else {
        session.shifted_ascii
    };

    let mut raw_input = session.raw_input.clone();
    if character.is_ascii() {
        // `raw_input` must stay caret-ordered, not append-only: the new
        // keystroke lands right after whatever romaji is already pending,
        // which itself sits right after the raw source of `preedit[..cursor]`
        // -- exactly `next_raw_boundary`, read *before* `table.feed` below
        // extends that pending state (#16 finding B/C).
        let raw_byte_at = next_raw_boundary(table, session);
        let mut buf = [0u8; 4];
        raw_input.insert_str(raw_byte_at, character.encode_utf8(&mut buf))?;
    }
    let mut romaji = session.romaji.clone();
    scratch.clear();
    if character == '.' && ascii_digit_immediately_before_cursor(session, table) {
        // A half-width number owns its decimal separator. Flush defensively
        // before inserting the literal period so this direct path has the
        // same ordering and terminal-state guarantee as `Table::feed`.
        table.flush(&mut romaji, scratch)?;
        scratch.push('.')?;
    } else {
        table.feed(&mut romaji, character, scratch)?;
    }
    let mut preedit = session.preedit.clone();
    let mut cursor = session.cursor;
    if !scratch.is_empty() {
        let at = preedit
            .byte_index(usize::from(cursor))
            .unwrap_or(preedit.len());
        preedit.insert_str(at, scratch.as_str())?;
        cursor = cursor
            .saturating_add(u16::try_from(scratch.as_str().chars().count()).unwrap_or(u16::MAX));
    }

    session.shifted_ascii = shifted_ascii;
    session.raw_input = raw_input;
    session.romaji = romaji;
    session.preedit = preedit;
    session.cursor = cursor;
    session.invalidate_prediction();
    Ok(())
}

fn flush_pending(
    session: &mut Session,
    table: &Table,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
) -> Result<(), Overflow> {
    // Same atomic-by-construction shape as `feed_character`: `table.flush`
    // mutates a local clone of `romaji`, and nothing is written back to
    // `session` until the fallible `preedit` insert has already succeeded.
    let mut romaji = session.romaji.clone();
    scratch.clear();
    table.flush(&mut romaji, scratch)?;
    if scratch.is_empty() {
        session.romaji = romaji;
        return Ok(());
    }
    let mut preedit = session.preedit.clone();
    let cursor = usize::from(session.cursor);
    let at = preedit.byte_index(cursor).unwrap_or(preedit.len());
    preedit.insert_str(at, scratch.as_str())?;
    session.romaji = romaji;
    session.preedit = preedit;
    session.cursor = session
        .cursor
        .saturating_add(u16::try_from(scratch.as_str().chars().count()).unwrap_or(u16::MAX));
    session.invalidate_prediction();
    Ok(())
}

/// Handles a key map action. M0 implements the small subset it has real
/// behaviour for; everything else is swallowed (`consumed = true`, no state
/// change) rather than passed through, so a stray Space or Tab mid-
/// composition cannot land in the host document underneath an active
/// preedit -- see this module's docs on the clean seam that leaves for
/// later phases.
#[allow(clippy::too_many_arguments)]
fn apply_action(
    session_id: SessionId,
    session: &mut Session,
    action: Action,
    services: &KeyServices<'_>,
    policy: ExecutionPolicy,
    prediction_cache: &mut PredictionCacheWork<'_>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    out.consumed = true;
    if let Some(offset) = action.candidate_offset() {
        return if session.converting {
            commit_numbered_candidate(
                session,
                services.table,
                services.normalizer,
                services.conversion,
                services.learning,
                services.input_history,
                policy,
                scratch,
                offset,
                out,
            )
        } else {
            commit_numbered_suggestion(
                session_id,
                session,
                services.normalizer,
                services.learning,
                services.input_history,
                policy,
                prediction_cache,
                scratch,
                offset,
                out,
            )
        };
    }
    match action {
        Action::ImeToggle => {
            let mode = if session.mode == Mode::Direct {
                Mode::Hiragana
            } else {
                Mode::Direct
            };
            switch_mode(session, services, policy, scratch, mode, out)?;
        }
        Action::ImeOn => switch_mode(session, services, policy, scratch, Mode::Hiragana, out)?,
        Action::ImeOff => switch_mode(session, services, policy, scratch, Mode::Direct, out)?,
        Action::ModeHiragana => {
            switch_mode(session, services, policy, scratch, Mode::Hiragana, out)?;
        }
        Action::ModeKatakana => {
            switch_mode(session, services, policy, scratch, Mode::Katakana, out)?;
        }
        Action::ModeHalfKatakana => {
            switch_mode(session, services, policy, scratch, Mode::HalfKatakana, out)?;
        }
        Action::ModeFullAlnum => {
            switch_mode(session, services, policy, scratch, Mode::FullAlnum, out)?;
        }
        Action::ModeHalfAlnum => {
            switch_mode(session, services, policy, scratch, Mode::HalfAlnum, out)?;
        }
        Action::ModeDirect => switch_mode(session, services, policy, scratch, Mode::Direct, out)?,
        Action::ModeKanaToggle => {
            let next = if session.mode == Mode::Hiragana {
                Mode::Katakana
            } else {
                Mode::Hiragana
            };
            switch_mode(session, services, policy, scratch, next, out)?;
        }
        Action::ModeKanaCycle => {
            if session.is_composing() {
                // With a preedit, NonConvert is a temporary surface
                // transform.  In particular, do not update `session.mode`:
                // after Enter the next composition must still follow the
                // user's original input mode.
                let transform = if session.converting {
                    match session.segment_transform(session.focused_segment()).0 {
                        SegmentTransform::Katakana => SegmentTransform::HalfKatakana,
                        SegmentTransform::HalfKatakana => SegmentTransform::Hiragana,
                        SegmentTransform::Hiragana | SegmentTransform::None => {
                            SegmentTransform::Katakana
                        }
                        _ => SegmentTransform::Katakana,
                    }
                } else {
                    match session.mode {
                        Mode::Hiragana => SegmentTransform::Katakana,
                        Mode::Katakana => SegmentTransform::HalfKatakana,
                        Mode::HalfKatakana => SegmentTransform::Hiragana,
                        _ => SegmentTransform::Katakana,
                    }
                };
                apply_transform(session, services.table, scratch, transform, out)?;
            } else {
                // With no input, NonConvert is the persistent input-mode
                // cycle users expect from a Japanese keyboard.
                let next = match session.mode {
                    Mode::Hiragana => Mode::Katakana,
                    Mode::Katakana => Mode::HalfKatakana,
                    Mode::HalfKatakana => Mode::Hiragana,
                    _ => Mode::Hiragana,
                };
                switch_mode(session, services, policy, scratch, next, out)?;
            }
        }
        Action::ModeAlnumToggle => {
            let next = match session.mode {
                Mode::HalfAlnum | Mode::FullAlnum => Mode::Hiragana,
                _ => Mode::HalfAlnum,
            };
            switch_mode(session, services, policy, scratch, next, out)?;
        }
        Action::ModeAlnumWidthToggle => {
            let next = match session.mode {
                Mode::HalfAlnum => Mode::FullAlnum,
                Mode::FullAlnum => Mode::HalfAlnum,
                _ => Mode::HalfAlnum,
            };
            switch_mode(session, services, policy, scratch, next, out)?;
        }
        Action::Commit => {
            if !commit_selected_suggestion(
                session_id,
                session,
                services.normalizer,
                services.learning,
                services.input_history,
                policy,
                prediction_cache,
                scratch,
                out,
            )? {
                commit_pending(
                    session,
                    services.table,
                    services.normalizer,
                    services.conversion,
                    services.learning,
                    services.input_history,
                    policy,
                    scratch,
                    out,
                )?;
            }
        }
        Action::CommitFirst => {
            if !commit_suggestion_at(
                session_id,
                session,
                0,
                services.normalizer,
                services.learning,
                services.input_history,
                policy,
                prediction_cache,
                scratch,
                out,
            )? {
                commit_pending(
                    session,
                    services.table,
                    services.normalizer,
                    services.conversion,
                    services.learning,
                    services.input_history,
                    policy,
                    scratch,
                    out,
                )?;
            }
        }
        Action::Cancel => {
            if session.suggestion_focused {
                session.hide_suggestions();
            } else if session.converting {
                session.cancel_conversion();
            } else {
                session.reset();
            }
        }
        Action::Convert => {
            session.hide_suggestions();
            begin_conversion(
                session_id,
                session,
                services.table,
                services.conversion,
                services.learning,
                services.long_conversion,
                services.long_conversion_owner,
                scratch,
                0,
                out,
            )?;
        }
        Action::ConvertPrev => {
            begin_conversion(
                session_id,
                session,
                services.table,
                services.conversion,
                services.learning,
                services.long_conversion,
                services.long_conversion_owner,
                scratch,
                -1,
                out,
            )?;
        }
        Action::CandidateNext => {
            let _ = session.expand_conversion();
            let index = session.focused_segment();
            session.clear_segment_transform(index);
            let next = session.segment_selection(index).saturating_add(1);
            session.set_segment_selection(index, next);
        }
        Action::CandidatePrev => {
            let _ = session.expand_conversion();
            let index = session.focused_segment();
            session.clear_segment_transform(index);
            let next = session.segment_selection(index).saturating_sub(1);
            session.set_segment_selection(index, next);
        }
        Action::CandidatePageDown => {
            let _ = session.expand_conversion();
            let index = session.focused_segment();
            session.clear_segment_transform(index);
            let next = session
                .segment_selection(index)
                .saturating_add(CANDIDATE_PAGE_SIZE as i16);
            session.set_segment_selection(index, next);
        }
        Action::CandidatePageUp => {
            let _ = session.expand_conversion();
            let index = session.focused_segment();
            session.clear_segment_transform(index);
            let next = session
                .segment_selection(index)
                .saturating_sub(CANDIDATE_PAGE_SIZE as i16);
            session.set_segment_selection(index, next);
        }
        Action::CandidateExpand => {
            // Conversion starts compact. Repeating the action after expansion
            // is an explicit idempotent success; attempting it outside
            // conversion is recoverable and leaves the current composition
            // untouched.
            if !session.expand_conversion() {
                out.beep = true;
            }
        }
        Action::PredictNext | Action::PredictPrev => {
            if !services.prediction_enabled
                || services.suggest_accept != SuggestAccept::Tab
                || services.prediction.is_none()
            {
                out.consumed = false;
            } else {
                let count = prediction_cache
                    .candidates(session_id, session.prediction_generation)
                    .map_or(0, <[_]>::len);
                let direction = if action == Action::PredictPrev { -1 } else { 1 };
                if !session.focus_suggestion(direction, count) {
                    out.beep = true;
                }
            }
        }
        Action::DeletePredictionHistory => {
            if !delete_focused_prediction_history(
                session_id,
                session,
                services.learning,
                policy,
                prediction_cache,
            ) {
                out.beep = true;
            }
        }
        Action::SegmentPrev => session.focus_previous_segment(),
        Action::SegmentNext => session.focus_next_segment(),
        Action::SegmentHome => session.focus_first_segment(),
        Action::SegmentEnd => session.focus_last_segment(),
        Action::SegmentShrink => {
            if !session.resize_focused_segment(false) {
                out.beep = true;
            }
        }
        Action::SegmentGrow => {
            if !session.resize_focused_segment(true) {
                out.beep = true;
            }
        }
        Action::CaretLeft => move_caret(session, services.table, scratch, CaretMove::Left)?,
        Action::CaretRight => move_caret(session, services.table, scratch, CaretMove::Right)?,
        Action::CaretHome => move_caret(session, services.table, scratch, CaretMove::Home)?,
        Action::CaretEnd => move_caret(session, services.table, scratch, CaretMove::End)?,
        Action::DeleteBack => apply_backspace(session, services.table),
        Action::DeleteForward => apply_delete_forward(session, services.table),
        Action::TransformHiragana => apply_transform(
            session,
            services.table,
            scratch,
            SegmentTransform::Hiragana,
            out,
        )?,
        Action::TransformKatakana => apply_transform(
            session,
            services.table,
            scratch,
            SegmentTransform::Katakana,
            out,
        )?,
        Action::TransformHalfKatakana => apply_transform(
            session,
            services.table,
            scratch,
            SegmentTransform::HalfKatakana,
            out,
        )?,
        Action::TransformFullAlnum => apply_transform(
            session,
            services.table,
            scratch,
            SegmentTransform::FullAlnum,
            out,
        )?,
        Action::TransformHalfAlnum => apply_transform(
            session,
            services.table,
            scratch,
            SegmentTransform::HalfAlnum,
            out,
        )?,
        Action::UndoCommit => match session.undo_commit() {
            Some(surface) => out.set_delete_before(surface.as_str())?,
            None => out.consumed = false,
        },
        Action::Reconvert => {
            // TSF reconversion supplies the selected text through its own
            // protocol request; a bare idle key action has no text to recover.
            out.consumed = false;
        }
        // Named explicitly so a keymap author can see, at the binding site,
        // that a key is deliberately claimed with no effect rather than
        // left unbound (issue #16 finding E) -- reaches the same outcome as
        // the catch-all below, which is this function's documented default
        // for any action with no bespoke arm.
        Action::Swallow => {}
        _ => {}
    }
    Ok(())
}

/// Commits a 1-9 shortcut from the page containing the current selection.
/// Invalid numbers on a short final page have an explicit, recoverable
/// outcome: the candidate list stays open and the client is asked to beep.
#[allow(clippy::too_many_arguments)]
fn commit_numbered_candidate(
    session: &mut Session,
    table: &Table,
    normalizer: &Normalizer,
    conversion: Option<&ConversionService>,
    learning: Option<&LearningService>,
    input_history: Option<&InputHistoryService>,
    policy: ExecutionPolicy,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    offset: usize,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.converting || offset >= CANDIDATE_PAGE_SIZE {
        out.beep = true;
        return Ok(());
    }

    let focused = session.focused_segment();
    let Some(range) = session.segment_range(focused) else {
        out.beep = true;
        return Ok(());
    };
    let selected = session.segment_selection(focused);
    let mut chosen = FixedStr::<MAX_PREEDIT_BYTES>::new();
    let mut chosen_meta = CommitSegmentMeta::default();
    let options = conversion_options(session, session.carry_right_id());
    let result = match conversion {
        Some(service) => service.with_candidates(
            &session.preedit.as_str()[range],
            options,
            |candidates| -> Result<Option<i16>, Overflow> {
                if candidates.is_empty() {
                    return Ok(None);
                }
                let current = selected.rem_euclid(candidates.len() as i16) as usize;
                let page_start = current / CANDIDATE_PAGE_SIZE * CANDIDATE_PAGE_SIZE;
                let target = page_start.saturating_add(offset);
                let Some(candidate) = candidates.get(target) else {
                    return Ok(None);
                };
                chosen.push_str(candidate.text())?;
                chosen_meta = candidate_meta(candidate);
                Ok(i16::try_from(target).ok())
            },
        ),
        None => {
            out.beep = true;
            return Ok(());
        }
    };

    match result {
        Ok(Ok(Some(_target))) => {
            // `commit_converted_segments` only honours `candidate_override`
            // once it sees this segment's transform as `None` (a segment
            // transform reading takes priority over a chosen candidate), so
            // the clear has to be visible to it and cannot be deferred until
            // after the call succeeds. Snapshot the prior value first so a
            // commit that does not go through -- `Ok(false)` for an
            // unrelated segment, or `Err(Overflow)` -- can put it back: the
            // doc comment above promises the candidate list "stays open"
            // unchanged on any non-terminal outcome, and a stray cleared
            // transform would break that promise. `set_segment_selection`
            // is deliberately not called here: `commit_converted_segments`
            // never reads it for an overridden segment, a successful commit
            // erases it via `session.reset()` regardless, and setting it
            // speculatively would be exactly this same kind of mutation
            // with no rollback on failure.
            let previous_transform = session.segment_transform(focused);
            session.clear_segment_transform(focused);
            match commit_converted_segments(
                session,
                table,
                normalizer,
                conversion,
                learning,
                input_history,
                policy,
                scratch,
                out,
                Some(CandidateOverride {
                    segment: focused,
                    text: chosen.as_str(),
                    meta: chosen_meta,
                }),
            ) {
                Ok(true) => {}
                Ok(false) => {
                    session.restore_segment_transform(
                        focused,
                        previous_transform.0,
                        previous_transform.1,
                    );
                    out.beep = true;
                }
                Err(overflow) => {
                    session.restore_segment_transform(
                        focused,
                        previous_transform.0,
                        previous_transform.1,
                    );
                    return Err(overflow);
                }
            }
        }
        Ok(Err(overflow)) => return Err(overflow),
        Ok(Ok(None)) | Err(_) => out.beep = true,
    }
    Ok(())
}

/// Commits a zero-based numbered suggestion from the one visible suggestion
/// page. Unlike conversion numbers, suggestions are accepted without first
/// focusing the list. An unavailable number is a terminal recoverable beep
/// and does not alter the preedit or the current visible list.
#[allow(clippy::too_many_arguments)]
fn commit_numbered_suggestion(
    session_id: SessionId,
    session: &mut Session,
    normalizer: &Normalizer,
    learning: Option<&LearningService>,
    input_history: Option<&InputHistoryService>,
    policy: ExecutionPolicy,
    cache: &PredictionCacheWork<'_>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    offset: usize,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.suggestions_visible || offset >= CANDIDATE_PAGE_SIZE {
        out.beep = true;
        return Ok(());
    }
    if !commit_suggestion_at(
        session_id,
        session,
        offset,
        normalizer,
        learning,
        input_history,
        policy,
        cache,
        scratch,
        out,
    )? {
        out.beep = true;
    }
    Ok(())
}

/// Deletes exactly the currently focused learned suggestion. All rejection and
/// durable-failure paths intentionally leave cache, selection, and preedit
/// authoritative; only a successful durable publish can invalidate them.
fn delete_focused_prediction_history(
    session_id: SessionId,
    session: &mut Session,
    learning: Option<&LearningService>,
    policy: ExecutionPolicy,
    cache: &mut PredictionCacheWork<'_>,
) -> bool {
    if !session.suggestion_focused {
        return false;
    }
    let Some(candidates) = cache.candidates(session_id, session.prediction_generation) else {
        return false;
    };
    let Some(index) = session.selected_suggestion(candidates.len()) else {
        return false;
    };
    let Some(candidate) = candidates.get(index).cloned() else {
        return false;
    };
    if candidate.source() != PredictionSource::History {
        return false;
    }
    let Some(learning) = learning else {
        return false;
    };

    // Probe answers the same consumption question as Apply, but the durable
    // forget operation is intentionally unavailable in that policy.
    if !policy.allows_persistence() {
        return true;
    }

    match learning.forget_prediction_exact(candidate.reading(), candidate.surface()) {
        Ok(ForgetPredictionOutcome::Removed) => {
            // The session generation makes in-flight worker replies stale;
            // clearing this connection's cache ensures the next bounded
            // request is freshly ranked against the durable post-delete log.
            cache.clear();
            session.invalidate_prediction();
            true
        }
        Ok(ForgetPredictionOutcome::NotFound | ForgetPredictionOutcome::Unavailable) | Err(_) => {
            false
        }
    }
}

#[derive(Clone, Copy)]
struct CandidateOverride<'a> {
    segment: usize,
    text: &'a str,
    meta: CommitSegmentMeta,
}

/// Appends one segment at the width-policy choke point. Explicit F6-F10
/// transforms own their width and casing, while ordinary candidates pass
/// through the configured normalizer exactly once.
fn append_segment_surface(
    source: &str,
    raw_input: &str,
    transform: SegmentTransform,
    cycle: u8,
    normalizer: &Normalizer,
    mode: Mode,
    target: &mut FixedStr<MAX_PREEDIT_BYTES>,
) -> Result<(), Overflow> {
    if transform == SegmentTransform::None {
        normalizer.normalize_into(source, mode, target)
    } else {
        transform_into(source, raw_input, transform, cycle, target)
    }
}

/// Materializes and commits every pinned segment. Each unoverridden segment
/// performs exactly one bounded conversion after the previous slot has been
/// released, avoiding nested pool locks. Failure is observable as `false` and
/// leaves the converting session intact for its caller to finalize.
#[allow(clippy::too_many_arguments)]
fn commit_converted_segments(
    session: &mut Session,
    table: &Table,
    normalizer: &Normalizer,
    conversion: Option<&ConversionService>,
    learning: Option<&LearningService>,
    input_history: Option<&InputHistoryService>,
    policy: ExecutionPolicy,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
    candidate_override: Option<CandidateOverride<'_>>,
) -> Result<bool, Overflow> {
    if !session.converting || session.segment_count() == 0 {
        return Ok(false);
    }

    scratch.clear();
    let count = session.segment_count();
    let mut context_right_id = session.carry_right_id();
    let mut it_words = 0u8;
    let mut total_words = 0u8;
    let mut transformed = false;
    for index in 0..count {
        let Some(range) = session.segment_range(index) else {
            scratch.clear();
            return Ok(false);
        };
        let reading = &session.preedit.as_str()[range.clone()];
        // Each segment commits only its own share of `raw_input`, not the
        // whole composition's keystrokes -- see the identical note in
        // `render_converted_segments` (#16 finding D).
        let raw_segment = segment_raw_text(
            table,
            session.preedit.as_str(),
            session.raw_input.as_str(),
            range,
        );
        let (transform, cycle) = session.segment_transform(index);

        if transform != SegmentTransform::None {
            append_segment_surface(
                reading,
                raw_segment,
                transform,
                cycle,
                normalizer,
                session.mode,
                scratch,
            )?;
            context_right_id = 0;
            transformed = true;
            continue;
        }

        if let Some(override_candidate) = candidate_override.filter(|item| item.segment == index) {
            append_segment_surface(
                override_candidate.text,
                raw_segment,
                transform,
                cycle,
                normalizer,
                session.mode,
                scratch,
            )?;
            context_right_id = override_candidate.meta.right_id;
            it_words = it_words.saturating_add(override_candidate.meta.it_words);
            total_words = total_words.saturating_add(override_candidate.meta.total_words);
            continue;
        }

        let Some(service) = conversion else {
            scratch.clear();
            return Ok(false);
        };
        let selection = session.segment_selection(index);
        let options = conversion_options(session, context_right_id);
        match service.with_candidates(
            reading,
            options,
            |candidates| -> Result<Option<CommitSegmentMeta>, Overflow> {
                if candidates.is_empty() {
                    return Ok(None);
                }
                let selected = selection.rem_euclid(candidates.len() as i16) as usize;
                append_segment_surface(
                    candidates[selected].text(),
                    raw_segment,
                    transform,
                    cycle,
                    normalizer,
                    session.mode,
                    scratch,
                )?;
                Ok(Some(candidate_meta(&candidates[selected])))
            },
        ) {
            Ok(Ok(Some(meta))) => {
                context_right_id = meta.right_id;
                it_words = it_words.saturating_add(meta.it_words);
                total_words = total_words.saturating_add(meta.total_words);
            }
            Ok(Err(overflow)) => return Err(overflow),
            Ok(Ok(None)) | Err(_) => {
                scratch.clear();
                return Ok(false);
            }
        }
    }

    out.set_commit(scratch.as_str())?;
    // A segment transform (無変換, F6-F10) mechanically rewrites the reading the
    // user already typed; it is not a choice between conversion candidates.
    // Learning it taught ordinary readings to prefer their katakana form -- a
    // real store on this machine had `と` biased towards `ﾄ` after three such
    // commits. The commit still reaches the developer input history, which is a
    // faithful record of what happened, but never the learning store.
    let learnable = if transformed { None } else { learning };
    record_learning(
        session,
        learnable,
        input_history,
        policy,
        scratch.as_str(),
        context_right_id,
    );
    session.record_current_commit(scratch.as_str(), context_right_id, it_words, total_words);
    session.reset();
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn begin_conversion(
    session_id: SessionId,
    session: &mut Session,
    table: &Table,
    conversion: Option<&ConversionService>,
    learning: Option<&LearningService>,
    long_conversion: Option<&LongConversionService>,
    long_conversion_owner: u64,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    initial_selection: i16,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if session.converting {
        let focused = session.focused_segment();
        session.clear_segment_transform(focused);
        let next = session
            .segment_selection(focused)
            .saturating_add(initial_selection.signum());
        session.set_segment_selection(focused, next);
        return Ok(());
    }
    flush_pending(session, table, scratch)?;
    if session.preedit.is_empty() {
        out.beep = true;
        return Ok(());
    }
    let shifted_ascii_dictionary_hit = prepare_shifted_ascii_reading(session, conversion)?;
    if session.shifted_ascii && !shifted_ascii_dictionary_hit {
        // A Shift-started ASCII sequence is an explicit English composition. Never
        // reinterpret an unknown word as kana merely because the romaji table
        // can produce a phonetic fallback; keep the raw text available for
        // Enter/commit instead.
        out.beep = true;
        return Ok(());
    }
    session.begin_conversion();
    session.selected_candidate = initial_selection;
    let mut segments: FixedVec<ConversionSegment, MAX_SEGMENTS> = FixedVec::new();
    let cached = session.cached_surface_fingerprint(session.preedit.as_str());
    let initial_context = session.carry_right_id();
    let options = conversion_options(session, initial_context);
    let mut chosen_selection = initial_selection;
    let initialized = conversion.and_then(|service| {
        match service.with_conversion(
            session.preedit.as_str(),
            options,
            |candidates, _diagnostics| {
                if candidates.is_empty() {
                    return false;
                }
                let learned = learning.map_or(
                    LearningPreference {
                        exact: None,
                        general: None,
                    },
                    |service| {
                        service.preference(
                            session.preedit.as_str(),
                            initial_context,
                            candidates.iter().map(candidate_learning_key),
                        )
                    },
                );
                let authoritative = has_authoritative_candidate_preference(
                    candidates,
                    initial_selection,
                    cached,
                    learned,
                );
                let preferred =
                    preferred_candidate_index(candidates, initial_selection, cached, learned);
                let selected = if !authoritative {
                    long_conversion
                        .and_then(|service| {
                            service.selection(
                                long_conversion_owner,
                                session_id,
                                session.prediction_generation,
                                session.preedit.as_str(),
                                candidates,
                            )
                        })
                        .unwrap_or(preferred)
                } else {
                    preferred
                };
                chosen_selection = i16::try_from(selected).unwrap_or(i16::MAX);
                let candidate = &candidates[selected];
                for segment in candidate.segments() {
                    if segments.push(*segment).is_err() {
                        return false;
                    }
                }
                true
            },
        ) {
            Ok(initialized) => Some(initialized),
            Err(ConvertFailure::Busy) => None,
            Err(ConvertFailure::Conversion(_)) => None,
        }
    });
    if !matches!(initialized, Some(true)) || !session.set_segments(segments.as_slice()) {
        session.cancel_conversion();
        out.beep = true;
    } else {
        session.set_segment_selection(0, chosen_selection);
    }
    Ok(())
}

/// Uses the Shift-started ASCII sequence as the dictionary reading when the
/// dictionary proves that it has a technical entry for that sequence.
///
/// The conversion core always supplies a synthetic reading fallback and also
/// generates identifier-case variants, so merely checking for a non-empty
/// candidate list would make every unknown word look like a dictionary hit.
/// Generated English aliases carry the IT flag; that flag is the deliberate
/// boundary between a real term and the fallback path.
fn prepare_shifted_ascii_reading(
    session: &mut Session,
    conversion: Option<&ConversionService>,
) -> Result<bool, Overflow> {
    if !session.shifted_ascii || session.raw_input.is_empty() {
        return Ok(false);
    }
    let mut reading = FixedStr::<MAX_PREEDIT_BYTES>::new();
    for character in session.raw_input.as_str().chars() {
        reading.push(character.to_ascii_lowercase())?;
    }
    let Some(service) = conversion else {
        return Ok(false);
    };
    let options = conversion_options(session, session.carry_right_id());
    let has_dictionary_candidate = service
        .with_candidates(reading.as_str(), options, |candidates| {
            candidates.iter().any(|candidate| {
                candidate
                    .segments()
                    .iter()
                    .any(|segment| segment.flags.contains(EntryFlags::IT))
            })
        })
        .ok()
        .unwrap_or(false);
    if !has_dictionary_candidate {
        return Ok(false);
    }

    session.romaji.clear();
    session.preedit.clear();
    session.preedit.push_str(reading.as_str())?;
    session.cursor = u16::try_from(reading.as_str().chars().count()).unwrap_or(u16::MAX);
    Ok(true)
}

/// Replaces all composition state with a reading recovered from an exact
/// committed surface, then enters the ordinary conversion state machine. A
/// surface absent from the reverse scan is treated as its own reading so kana
/// selections and user-entered terms still get a deterministic result.
fn build_reconversion(
    session: &mut Session,
    selected_text: &str,
    table: &Table,
    conversion: &ConversionService,
    learning: Option<&LearningService>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), ErrorCode> {
    session.reset();
    session.mode = Mode::Hiragana;
    out.consumed = true;
    out.mode = Some(session.mode);

    let recovered = conversion
        .reconversion_reading(selected_text, scratch)
        .map_err(|_| ErrorCode::Internal)?;
    if !recovered {
        scratch.clear();
        scratch
            .push_str(selected_text)
            .map_err(|_| ErrorCode::TooLarge)?;
    }
    session
        .preedit
        .push_str(scratch.as_str())
        .map_err(|_| ErrorCode::TooLarge)?;
    session.cursor =
        u16::try_from(session.preedit.as_str().chars().count()).map_err(|_| ErrorCode::TooLarge)?;

    begin_conversion(
        SessionId::default(),
        session,
        table,
        Some(conversion),
        learning,
        None,
        0,
        scratch,
        0,
        out,
    )
    .map_err(|_| ErrorCode::TooLarge)?;
    let normalizer = session.normalizer;
    render_preedit(session, table, &normalizer, Some(conversion), scratch, out)
        .map_err(|_| ErrorCode::TooLarge)
}

/// Enters a one-segment editing state when an F6-F10 transform is invoked
/// directly from raw composition, then applies the transform to the focused
/// segment. A dictionary is deliberately not required: these are textual
/// editing operations and must remain usable during dictionary reload/failure.
fn apply_transform(
    session: &mut Session,
    table: &Table,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    transform: SegmentTransform,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.converting {
        flush_pending(session, table, scratch)?;
        if session.preedit.is_empty() {
            out.beep = true;
            return Ok(());
        }
        let Ok(reading_end) = u16::try_from(session.preedit.len()) else {
            return Err(Overflow);
        };
        let segment = ConversionSegment {
            reading_end,
            text_end: reading_end,
            ..ConversionSegment::default()
        };
        if !session.set_segments(&[segment]) {
            out.beep = true;
            return Ok(());
        }
        session.begin_conversion();
    }
    session.apply_segment_transform(transform);
    Ok(())
}

/// Tries to delete pending romaji first, falling back to the last resolved
/// kana character only once nothing is pending -- the required "Backspace
/// removes pending romaji first, then emitted kana" behaviour.
///
/// Removing one *kana* character from `preedit` must remove exactly the raw
/// keystrokes that produced it, which is not always one: `ka`, `kyo`, and a
/// sokuon pair all resolve from more than one ASCII character, and leaving
/// the extra ones behind in `raw_input` is #16 finding B. Shifted-ASCII
/// composition is the one exception, handled first: it shows and (if
/// committed) emits `raw_input` verbatim rather than the kana `preedit`
/// tracks underneath it (see `render_preedit`, `commit_pending`), so there
/// one Backspace undoes exactly one typed letter -- the unit the user is
/// looking at -- matching the historical behaviour this mode already had.
fn apply_backspace(session: &mut Session, table: &Table) {
    if session.shifted_ascii {
        let _ = session.raw_input.pop_char();
        if session.romaji.backspace() {
            session.invalidate_prediction();
            return;
        }
        let cursor = usize::from(session.cursor);
        if cursor == 0 {
            return;
        }
        if let Some(at) = session.preedit.byte_index(cursor - 1) {
            let _ = session.preedit.remove_char_at(at);
            session.cursor = session.cursor.saturating_sub(1);
            session.invalidate_prediction();
        }
        return;
    }
    if !session.romaji.is_empty() {
        // Pending romaji has not resolved into `preedit` yet, so the raw
        // keystrokes behind it are exactly `pending_source_range` --
        // wherever the caret was when they were typed, not necessarily at
        // the end of `raw_input` (#16 finding B/C). But a sokuon/carry can
        // leave pending wholly overlapping already-emitted text (the
        // provenance contract on `pending_source_range`): the second "t"
        // of "tt" -> "っ" carries forward as pending "t", and that raw
        // byte is still "っ"'s own source, untouched by this Backspace.
        // Only remove a raw byte when pending's source reaches past
        // `raw_boundary` -- a byte no emitted kana's provenance depends
        // on; otherwise this Backspace reverts FSM state only, matching
        // `Input::backspace`'s own one-character-at-a-time contract.
        let raw_boundary = raw_byte_offset_for_preedit_cursor(table, session);
        let pending_end = pending_source_range(table, session).end;
        if pending_end > raw_boundary {
            if let Some((last_start, _)) = session
                .raw_input
                .as_str()
                .get(..pending_end)
                .and_then(|s| s.char_indices().last())
            {
                session.raw_input.remove_char_at(last_start);
            }
        }
        session.romaji.backspace();
        session.invalidate_prediction();
        return;
    }
    let cursor = usize::from(session.cursor);
    if cursor == 0 {
        return;
    }
    let before = raw_chars_for_emitted(
        table,
        session.raw_input.as_str(),
        session.preedit.as_str(),
        cursor,
    );
    let after = raw_chars_for_emitted(
        table,
        session.raw_input.as_str(),
        session.preedit.as_str(),
        cursor - 1,
    );
    remove_raw_chars(&mut session.raw_input, after, before.saturating_sub(after));
    if let Some(at) = session.preedit.byte_index(cursor - 1) {
        let _ = session.preedit.remove_char_at(at);
        session.cursor = session.cursor.saturating_sub(1);
        session.invalidate_prediction();
    }
}

/// Mirrors `apply_backspace`'s kana-group-wise removal on the raw side: the
/// character sitting at `cursor` may likewise have taken more than one
/// keystroke to produce, and forward-delete must drop exactly those to keep
/// `raw_input` faithful (#16 finding C). Uses `raw_range_for_next_emitted`
/// rather than a whole-buffer replay so a keystroke still pending ahead of
/// the deleted character -- possible once the caret has moved -- is left
/// untouched instead of being folded into the deleted span (#16 finding
/// B/C).
fn apply_delete_forward(session: &mut Session, table: &Table) {
    let cursor = usize::from(session.cursor);
    if let Some(range) = raw_range_for_next_emitted(table, session) {
        remove_raw_range(&mut session.raw_input, range);
    }
    if let Some(at) = session.preedit.byte_index(cursor) {
        let _ = session.preedit.remove_char_at(at);
        session.invalidate_prediction();
    }
}

#[derive(Clone, Copy)]
enum CaretMove {
    Left,
    Right,
    Home,
    End,
}

fn move_caret(
    session: &mut Session,
    table: &Table,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    movement: CaretMove,
) -> Result<(), Overflow> {
    flush_pending(session, table, scratch)?;
    let end = u16::try_from(session.preedit.as_str().chars().count()).unwrap_or(u16::MAX);
    session.cursor = match movement {
        CaretMove::Left => session.cursor.saturating_sub(1),
        CaretMove::Right => session.cursor.saturating_add(1).min(end),
        CaretMove::Home => 0,
        CaretMove::End => end,
    };
    session.hide_suggestions();
    Ok(())
}

/// Flushes any trailing romaji, normalizes the resolved composition, and
/// commits it -- or does nothing if there was no composition in progress.
///
/// Used for both `Action::Commit` (Enter mid-composition) and the top-level
/// `Request::Commit`/`Request::Revert`... no -- `Revert` never commits (see
/// `Dispatcher::revert`); this is `Commit`'s and `Action::Commit`'s shared
/// path, plus every mode switch (`switch_mode`), which must not silently
/// drop a composition in progress just because the user pressed a mode key
/// instead of Enter (DESIGN 1: never lose user text).
#[allow(clippy::too_many_arguments)]
fn commit_pending(
    session: &mut Session,
    table: &Table,
    normalizer: &Normalizer,
    conversion: Option<&ConversionService>,
    learning: Option<&LearningService>,
    input_history: Option<&InputHistoryService>,
    policy: ExecutionPolicy,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.is_composing() {
        return Ok(());
    }
    if !session.converting && session.shifted_ascii && !session.raw_input.is_empty() {
        let surface = session.raw_input.clone();
        out.set_commit(surface.as_str())?;
        record_learning(
            session,
            learning,
            input_history,
            policy,
            surface.as_str(),
            0,
        );
        session.record_current_commit(surface.as_str(), 0, 0, 0);
        session.reset();
        return Ok(());
    }
    // Resolve whatever romaji is still pending as if no more input were
    // coming (a half-typed sokuon consonant, a lone waiting "n") so it
    // commits instead of silently vanishing.
    flush_pending(session, table, scratch)?;

    if session.converting
        && commit_converted_segments(
            session,
            table,
            normalizer,
            conversion,
            learning,
            input_history,
            policy,
            scratch,
            out,
            None,
        )?
    {
        return Ok(());
    }
    // No service, a busy pool, or a rejected image must never lose the
    // reading. Fall through to the normalized original text and finish
    // the commit deterministically.
    scratch.clear();
    normalizer.normalize_into(session.preedit.as_str(), session.mode, scratch)?;
    if !scratch.is_empty() {
        out.set_commit(scratch.as_str())?;
        record_learning(
            session,
            learning,
            input_history,
            policy,
            scratch.as_str(),
            0,
        );
        session.record_current_commit(scratch.as_str(), 0, 0, 0);
    }
    session.reset();
    Ok(())
}

/// Commits any pending composition, then switches to `mode`.
fn switch_mode(
    session: &mut Session,
    services: &KeyServices<'_>,
    policy: ExecutionPolicy,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    mode: Mode,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    commit_pending(
        session,
        services.table,
        services.normalizer,
        services.conversion,
        services.learning,
        services.input_history,
        policy,
        scratch,
        out,
    )?;
    session.disarm_commit_undo();
    session.reset_carryover();
    session.mode = mode;
    out.mode = Some(mode);
    Ok(())
}

/// `Mode::HalfAlnum`/`Mode::FullAlnum`: no preedit ever forms here (DESIGN
/// 5.6's width normalizer is the choke point, applied per keystroke, right
/// before the character leaves the engine) -- each keystroke normalizes and
/// commits immediately.
fn apply_alnum_char(
    session: &Session,
    normalizer: &Normalizer,
    key: &KeyInput,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    match key.ch {
        Some(c) => {
            out.consumed = true;
            let mapped = normalizer.normalize_char(c, session.mode);
            let mut buf = [0u8; 4];
            out.set_commit(mapped.encode_utf8(&mut buf))
        }
        None => {
            out.consumed = false;
            Ok(())
        }
    }
}

fn prediction_is_eligible(session: &Session, services: &KeyServices<'_>) -> bool {
    services.prediction_enabled
        && services.suggest_accept != SuggestAccept::Disabled
        && services.prediction.is_some()
        && !scope_is_sensitive(session.scope)
        && !session.converting
        && !session.shifted_ascii
        && !session.preedit.is_empty()
        && usize::from(session.cursor) == session.preedit.as_str().chars().count()
}

/// Runs one automatic request, or one bounded explicit retry, for a
/// `(session, generation)` pair. A timeout is remembered as a terminal empty
/// result until the preedit changes; an explicit PredictNext/Prev may reopen
/// that terminal once, but never more than once for the same generation.
fn refresh_prediction(
    session_id: SessionId,
    session: &mut Session,
    services: &KeyServices<'_>,
    cache: &mut PredictionCacheWork<'_>,
    policy: ExecutionPolicy,
    explicit_direction: Option<i16>,
) {
    if !prediction_is_eligible(session, services) {
        session.hide_suggestions();
        if policy.allows_prediction_cache_mutation() {
            cache.clear_if_session(session_id);
        }
        return;
    }
    let generation = session.prediction_generation;
    let attempted = cache.attempted_for(session_id, generation);
    let can_retry = explicit_direction.is_some()
        && attempted
        && !cache.explicit_retry_attempted_for(session_id, generation)
        && cache.candidates(session_id, generation).is_none();
    if (!can_retry && attempted) || !policy.allows_prediction_request() {
        return;
    }

    let Some(cache) = cache.apply_mut() else {
        // Probe deliberately has no request or mutation capability. Its
        // immutable view above is sufficient for action-consumption parity.
        return;
    };
    if cache.attempted_for(session_id, generation) {
        // The first automatic request already reached a terminal empty state.
        // This flag is the fixed-capacity owner of the one explicit retry.
        cache.explicit_retry_attempted = true;
    } else {
        cache.attempted = true;
        cache.explicit_retry_attempted = false;
        cache.session = session_id;
        cache.generation = generation;
    }
    let learning_generation = services.learning.map(LearningService::generation);
    cache.has_result = services.prediction.is_some_and(|service| {
        service.request_into(
            session_id,
            generation,
            session.preedit.as_str(),
            session.domain_it_ratio_per_mille(),
            PREDICTION_TIMEOUT,
            &mut cache.result,
        )
    });
    // A clear or commit in another connection may have changed history while
    // the prediction worker was ranking. Never expose a result from the old
    // epoch; the next key gets one bounded retry against current history.
    if services.learning.map(LearningService::generation) != learning_generation {
        cache.clear();
        session.hide_suggestions();
        return;
    }
    let available = cache.has_result
        && cache.result.session() == session_id
        && cache.result.generation() == generation
        && !cache.result.is_empty();
    if !available {
        cache.has_result = false;
    }
    session.show_suggestions(available);
}

fn focus_prediction_after_refresh(
    session_id: SessionId,
    session: &mut Session,
    cache: &PredictionCacheWork<'_>,
    direction: i16,
    out: &mut OutputBuf,
) {
    if session.suggestion_focused {
        return;
    }
    let Some(candidates) = cache.candidates(session_id, session.prediction_generation) else {
        return;
    };
    if session.focus_suggestion(direction, candidates.len()) {
        // The explicit navigation key was initially evaluated before the
        // bounded retry completed. A successful retry makes that same key a
        // real focus transition rather than a misleading beep.
        out.beep = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn commit_selected_suggestion(
    session_id: SessionId,
    session: &mut Session,
    normalizer: &Normalizer,
    learning: Option<&LearningService>,
    input_history: Option<&InputHistoryService>,
    policy: ExecutionPolicy,
    cache: &PredictionCacheWork<'_>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<bool, Overflow> {
    if !session.suggestion_focused {
        return Ok(false);
    }
    let Some(candidates) = cache.candidates(session_id, session.prediction_generation) else {
        session.hide_suggestions();
        return Ok(false);
    };
    let Some(index) = session.selected_suggestion(candidates.len()) else {
        session.hide_suggestions();
        return Ok(false);
    };
    commit_suggestion_at(
        session_id,
        session,
        index,
        normalizer,
        learning,
        input_history,
        policy,
        cache,
        scratch,
        out,
    )
}

#[allow(clippy::too_many_arguments)]
fn commit_suggestion_at(
    session_id: SessionId,
    session: &mut Session,
    index: usize,
    normalizer: &Normalizer,
    learning: Option<&LearningService>,
    input_history: Option<&InputHistoryService>,
    policy: ExecutionPolicy,
    cache: &PredictionCacheWork<'_>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<bool, Overflow> {
    let Some(candidate) = cache
        .candidates(session_id, session.prediction_generation)
        .and_then(|candidates| candidates.get(index))
        .cloned()
    else {
        return Ok(false);
    };

    scratch.clear();
    normalizer.normalize_into(candidate.surface(), session.mode, scratch)?;
    if scratch.is_empty() {
        return Ok(false);
    }
    // Build the replacement preedit and render the commit before touching
    // `session` at all: `push_str`/`set_commit` failing here must never
    // leave `romaji`/`raw_input`/`preedit` cleared with nothing committed.
    let mut preedit = FixedStr::<MAX_PREEDIT_BYTES>::new();
    preedit.push_str(candidate.reading())?;
    out.set_commit(scratch.as_str())?;

    session.romaji.clear();
    session.raw_input.clear();
    session.preedit = preedit;
    session.cursor = u16::try_from(candidate.reading().chars().count()).unwrap_or(u16::MAX);
    record_learning(
        session,
        learning,
        input_history,
        policy,
        scratch.as_str(),
        candidate.right_id(),
    );
    session.record_current_commit(
        scratch.as_str(),
        candidate.right_id(),
        u8::from(candidate.flags().contains(EntryFlags::IT)),
        1,
    );
    session.reset();
    Ok(true)
}

fn render_suggestions(
    session_id: SessionId,
    session: &mut Session,
    normalizer: &Normalizer,
    conversion: Option<&ConversionService>,
    cache: &PredictionCacheWork<'_>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.suggestions_visible || session.converting || !session.is_composing() {
        return Ok(());
    }
    let Some(candidates) = cache.candidates(session_id, session.prediction_generation) else {
        session.hide_suggestions();
        return Ok(());
    };
    let Some(selected) = session.selected_suggestion(candidates.len()) else {
        session.hide_suggestions();
        return Ok(());
    };
    out.begin_suggestions(
        u16::try_from(selected).map_err(|_| Overflow)?,
        CANDIDATE_PAGE_SIZE as u16,
    )?;
    for candidate in candidates {
        scratch.clear();
        normalizer.normalize_into(candidate.surface(), session.mode, scratch)?;
        if candidate.source() == PredictionSource::History {
            out.push_history_candidate(
                scratch.as_str(),
                candidate.annotation(),
                candidate.reading(),
                candidate.surface(),
            )?;
        } else {
            out.push_candidate(scratch.as_str(), candidate.annotation())?;
        }
    }
    if let (Some(service), Some(entry_index)) = (
        conversion,
        candidates
            .get(selected)
            .and_then(|candidate| candidate.system_entry_index()),
    ) {
        publish_system_candidate_detail(service, entry_index, candidates[selected].reading(), out);
    }
    Ok(())
}

/// Publishes only an exact, source-backed system entry.  Any malformed mapped
/// data, unavailable detail record, or relation that cannot fit the bounded
/// protocol clears the optional panel instead of guessing from candidate text.
fn publish_system_candidate_detail(
    conversion: &ConversionService,
    entry_index: u32,
    reading: &str,
    out: &mut OutputBuf,
) {
    out.clear_candidate_detail();
    let Ok(entry_index) = usize::try_from(entry_index) else {
        return;
    };
    let Ok(Some(detail)) = conversion.dictionary().detail_at(entry_index) else {
        return;
    };
    let mut definition = FixedStr::<MAX_CANDIDATE_DETAIL_DEFINITION_BYTES>::new();
    let Ok(truncated) =
        detail.write_description_preview(&mut definition, MAX_CANDIDATE_DETAIL_DEFINITION_BYTES)
    else {
        return;
    };
    if definition.is_empty() {
        return;
    }
    let mut aliases = [[0u8; MAX_CANDIDATE_DETAIL_RELATION_BYTES]; MAX_CANDIDATE_DETAIL_RELATIONS];
    let mut related = [[0u8; MAX_CANDIDATE_DETAIL_RELATION_BYTES]; MAX_CANDIDATE_DETAIL_RELATIONS];
    let mut similar = [[0u8; MAX_CANDIDATE_DETAIL_RELATION_BYTES]; MAX_CANDIDATE_DETAIL_RELATIONS];
    let mut antonyms = [[0u8; MAX_CANDIDATE_DETAIL_RELATION_BYTES]; MAX_CANDIDATE_DETAIL_RELATIONS];
    let mut lengths = [0usize; 4];
    let mut byte_lengths = [[0usize; MAX_CANDIDATE_DETAIL_RELATIONS]; 4];
    let mut valid = true;
    let relations = detail.visit_relations(|kind, text| {
        if text.is_empty() || text.len() > MAX_CANDIDATE_DETAIL_RELATION_BYTES {
            valid = false;
            return false;
        }
        match kind {
            DetailRelationKind::Alias if lengths[0] < aliases.len() => {
                aliases[lengths[0]][..text.len()].copy_from_slice(text.as_bytes());
                byte_lengths[0][lengths[0]] = text.len();
                lengths[0] += 1;
            }
            DetailRelationKind::Related if lengths[1] < related.len() => {
                related[lengths[1]][..text.len()].copy_from_slice(text.as_bytes());
                byte_lengths[1][lengths[1]] = text.len();
                lengths[1] += 1;
            }
            DetailRelationKind::Synonym if lengths[2] < similar.len() => {
                similar[lengths[2]][..text.len()].copy_from_slice(text.as_bytes());
                byte_lengths[2][lengths[2]] = text.len();
                lengths[2] += 1;
            }
            DetailRelationKind::Antonym if lengths[3] < antonyms.len() => {
                antonyms[lengths[3]][..text.len()].copy_from_slice(text.as_bytes());
                byte_lengths[3][lengths[3]] = text.len();
                lengths[3] += 1;
            }
            _ => {}
        }
        true
    });
    if relations.is_err() || !valid {
        return;
    }
    let mut alias_terms = [""; MAX_CANDIDATE_DETAIL_RELATIONS];
    let mut related_terms = [""; MAX_CANDIDATE_DETAIL_RELATIONS];
    let mut similar_terms = [""; MAX_CANDIDATE_DETAIL_RELATIONS];
    let mut antonym_terms = [""; MAX_CANDIDATE_DETAIL_RELATIONS];
    for index in 0..MAX_CANDIDATE_DETAIL_RELATIONS {
        alias_terms[index] =
            core::str::from_utf8(&aliases[index][..byte_lengths[0][index]]).unwrap_or("");
        related_terms[index] =
            core::str::from_utf8(&related[index][..byte_lengths[1][index]]).unwrap_or("");
        similar_terms[index] =
            core::str::from_utf8(&similar[index][..byte_lengths[2][index]]).unwrap_or("");
        antonym_terms[index] =
            core::str::from_utf8(&antonyms[index][..byte_lengths[3][index]]).unwrap_or("");
    }
    let _ = out.set_candidate_detail(CandidateDetailInput {
        reading,
        definition: definition.as_str(),
        definition_truncated: truncated,
        aliases: &alias_terms[..lengths[0]],
        related: &related_terms[..lengths[1]],
        similar: &similar_terms[..lengths[2]],
        antonyms: &antonym_terms[..lengths[3]],
    });
}

/// Projects the focused prediction surface into the output preedit without
/// changing the session's raw reading, romaji, or cursor authority. The cache
/// and generation remain the only authority for this volatile view; if they
/// no longer match, the caller keeps the raw preedit rendered above.
fn render_prediction_projection(
    session_id: SessionId,
    session: &mut Session,
    normalizer: &Normalizer,
    cache: &PredictionCacheWork<'_>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.suggestion_focused {
        return Ok(());
    }
    let Some(candidates) = cache.candidates(session_id, session.prediction_generation) else {
        session.hide_suggestions();
        return Ok(());
    };
    let Some(index) = session.selected_suggestion(candidates.len()) else {
        session.hide_suggestions();
        return Ok(());
    };
    let Some(candidate) = candidates.get(index) else {
        session.hide_suggestions();
        return Ok(());
    };
    scratch.clear();
    normalizer.normalize_into(candidate.surface(), session.mode, scratch)?;
    if scratch.is_empty() {
        session.hide_suggestions();
        return Ok(());
    }
    out.begin_preedit();
    out.push_segment(scratch.as_str(), UnderlineKind::Raw)?;
    out.set_cursor(u32::try_from(scratch.as_str().chars().count()).unwrap_or(u32::MAX));
    Ok(())
}

/// Rebuilds `out`'s preedit fields from `session`'s current composition, or
/// leaves them empty if nothing is composing (an empty `OutputBuf.preedit`
/// is exactly the "hide the preedit" signal a caller needs after a commit
/// or a cancel).
///
/// The composition is shown as (up to) two segments: the resolved kana
/// (`session.preedit`, normalized fresh into `scratch` -- DESIGN 5.6's
/// choke point, applied here rather than stored normalized, so that a
/// normalizer swapped mid-connection, or one applied twice by a future
/// change, could never double-widen anything) followed by any trailing
/// romaji not yet resolved to kana, shown as typed (real IMEs show
/// in-progress romaji as plain half-width ASCII, not run through the width
/// policy meant for committed alnum text).
fn render_preedit(
    session: &mut Session,
    table: &Table,
    normalizer: &Normalizer,
    conversion: Option<&ConversionService>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.is_composing() {
        return Ok(());
    }

    if session.converting {
        match render_converted_segments(session, table, normalizer, conversion, scratch, out)? {
            true => return Ok(()),
            false => {
                // A missing/busy/broken conversion service has a visible and
                // recoverable terminal state: beep and return to raw preedit.
                session.cancel_conversion();
                let consumed = out.consumed;
                let mode = out.mode;
                out.clear();
                out.consumed = consumed;
                out.mode = mode;
                out.beep = true;
            }
        }
    }

    if session.shifted_ascii && !session.raw_input.is_empty() {
        out.begin_preedit();
        out.push_segment(session.raw_input.as_str(), UnderlineKind::Raw)?;
        out.set_cursor(
            u32::try_from(session.raw_input.as_str().chars().count()).unwrap_or(u32::MAX),
        );
        return Ok(());
    }

    let pending = session.romaji.pending();
    out.begin_preedit();

    if pending.is_empty() {
        scratch.clear();
        normalizer.normalize_into(session.preedit.as_str(), session.mode, scratch)?;
        if !scratch.is_empty() {
            out.push_segment(scratch.as_str(), UnderlineKind::Raw)?;
        }
        let rendered_end = scratch.as_str().chars().count();
        let cursor = usize::from(session.cursor).min(session.preedit.as_str().chars().count());
        if cursor == session.preedit.as_str().chars().count() {
            out.set_cursor(u32::try_from(rendered_end).unwrap_or(u32::MAX));
        } else {
            scratch.clear();
            normalizer.normalize_into(
                &session.preedit.as_str()[..session.preedit.byte_index(cursor).unwrap_or(0)],
                session.mode,
                scratch,
            )?;
            out.set_cursor(u32::try_from(scratch.as_str().chars().count()).unwrap_or(u32::MAX));
        }
        return Ok(());
    }

    let at = session
        .preedit
        .byte_index(usize::from(session.cursor))
        .unwrap_or(session.preedit.len());
    let (prefix, suffix) = session.preedit.as_str().split_at(at);
    scratch.clear();
    normalizer.normalize_into(prefix, session.mode, scratch)?;
    let prefix_chars = scratch.as_str().chars().count();
    if !scratch.is_empty() {
        out.push_segment(scratch.as_str(), UnderlineKind::Raw)?;
    }
    out.push_segment(pending, UnderlineKind::Raw)?;
    scratch.clear();
    normalizer.normalize_into(suffix, session.mode, scratch)?;
    if !scratch.is_empty() {
        out.push_segment(scratch.as_str(), UnderlineKind::Raw)?;
    }
    out.set_cursor(
        u32::try_from(prefix_chars.saturating_add(pending.chars().count())).unwrap_or(u32::MAX),
    );
    Ok(())
}

/// Renders each pinned segment independently. Only the focused segment owns a
/// candidate table; every other segment keeps its own selection and underline.
/// Returning `false` is a terminal, recoverable conversion-service failure.
fn render_converted_segments(
    session: &mut Session,
    table: &Table,
    normalizer: &Normalizer,
    conversion: Option<&ConversionService>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<bool, Overflow> {
    if session.segment_count() == 0 {
        return Ok(false);
    }

    out.begin_preedit();
    let focused_segment = session.focused_segment();
    let mut cursor = 0usize;
    let mut context_right_id = session.carry_right_id();
    // A later segment's render can still overflow `scratch`/`out` after an
    // earlier segment's has already normalized its selection index (folding
    // it into `0..candidates.len()` via `rem_euclid`). Stage every write
    // here and only apply it once the whole pass has succeeded, so a
    // rendering failure never leaves a normalized selection behind for a
    // conversion that was never actually redrawn.
    let mut pending_selections: [Option<i16>; MAX_SEGMENTS] = [None; MAX_SEGMENTS];
    for (index, pending_selection) in pending_selections
        .iter_mut()
        .enumerate()
        .take(session.segment_count())
    {
        let Some(range) = session.segment_range(index) else {
            return Ok(false);
        };
        let reading = &session.preedit.as_str()[range.clone()];
        // Each segment gets only its own share of `raw_input`, not the whole
        // composition's keystrokes -- otherwise F10 on one segment of a
        // multi-segment conversion renders every segment's raw text as that
        // segment's surface (#16 finding D).
        let raw_segment = segment_raw_text(
            table,
            session.preedit.as_str(),
            session.raw_input.as_str(),
            range,
        );
        let (transform, cycle) = session.segment_transform(index);
        let underline = if index == focused_segment {
            UnderlineKind::Focused
        } else {
            UnderlineKind::Converted
        };

        if transform != SegmentTransform::None {
            scratch.clear();
            append_segment_surface(
                reading,
                raw_segment,
                transform,
                cycle,
                normalizer,
                session.mode,
                scratch,
            )?;
            cursor = cursor.saturating_add(scratch.as_str().chars().count());
            out.push_segment(scratch.as_str(), underline)?;
            context_right_id = 0;
            continue;
        }

        let Some(service) = conversion else {
            return Ok(false);
        };
        let requested_selection = session.segment_selection(index);
        let options = conversion_options(session, context_right_id);
        let rendered = service.with_candidates(
            reading,
            options,
            |candidates| -> Result<Option<(i16, usize, CommitSegmentMeta)>, Overflow> {
                if candidates.is_empty() {
                    return Ok(None);
                }
                let selected = requested_selection.rem_euclid(candidates.len() as i16) as usize;
                scratch.clear();
                append_segment_surface(
                    candidates[selected].text(),
                    raw_segment,
                    SegmentTransform::None,
                    0,
                    normalizer,
                    session.mode,
                    scratch,
                )?;
                let rendered_chars = scratch.as_str().chars().count();
                out.push_segment(scratch.as_str(), underline)?;

                if index == focused_segment {
                    out.begin_conversion_candidates(
                        session.conversion_presentation(),
                        u16::try_from(selected).map_err(|_| Overflow)?,
                        CANDIDATE_PAGE_SIZE as u16,
                    )?;
                    for candidate in candidates {
                        scratch.clear();
                        append_segment_surface(
                            candidate.text(),
                            raw_segment,
                            SegmentTransform::None,
                            0,
                            normalizer,
                            session.mode,
                            scratch,
                        )?;
                        out.push_candidate(scratch.as_str(), candidate.annotation())?;
                    }
                    if let Some(entry_index) = candidates[selected].system_entry_index() {
                        publish_system_candidate_detail(service, entry_index, reading, out);
                    }
                }

                Ok(Some((
                    i16::try_from(selected).map_err(|_| Overflow)?,
                    rendered_chars,
                    candidate_meta(&candidates[selected]),
                )))
            },
        );

        match rendered {
            Ok(Ok(Some((selected, rendered_chars, meta)))) => {
                *pending_selection = Some(selected);
                cursor = cursor.saturating_add(rendered_chars);
                context_right_id = meta.right_id;
            }
            Ok(Err(overflow)) => return Err(overflow),
            Ok(Ok(None)) | Err(_) => return Ok(false),
        }
    }
    for (index, selected) in pending_selections.into_iter().enumerate() {
        if let Some(selected) = selected {
            session.set_segment_selection(index, selected);
        }
    }
    out.set_cursor(u32::try_from(cursor).unwrap_or(u32::MAX));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::input_history::{InputHistoryRecord, InputHistoryService, ScopeClass};
    use sakura_core::keymap::{KeyMap, State};
    use sakura_core::width::{PunctuationStyle, Width, WidthPolicy};
    use sakura_core::UserDictionary;
    use sakura_proto::types::CandidatePresentation;
    use sakura_proto::{CandidateKind, KeyCode, Modifiers, CANDIDATE_PAGE_SIZE};

    fn builtin_dispatcher() -> Dispatcher {
        Dispatcher::new().expect("the shipped defaults must compile")
    }

    fn conversion_fixture() -> Arc<ConversionService> {
        let mut source = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t0\t100\t100\tit\tIT用語\nかな\t加奈\t0\t0\t200\t200\t\t人名\n",
        );
        for index in 3..=14 {
            writeln!(
                source,
                "かな\t候補{index:02}\t0\t0\t{}\t{}\t\tfixture",
                index * 100,
                index * 100
            )
            .expect("write fixture entry");
        }
        let entries = dictc::parse_entries("conversion.tsv", &source).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        Arc::new(ConversionService::from_static_bytes(image).expect("conversion service fixture"))
    }

    fn conversion_dispatcher() -> Dispatcher {
        Dispatcher::new_with_conversion(conversion_fixture()).expect("shipped defaults")
    }

    fn detail_conversion_fixture() -> Arc<ConversionService> {
        let source = concat!(
            "# license: MIT\n",
            "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
            "kana\tKana\t0\t0\t100\t100\t\tfixture\n",
            "a\tA\t0\t0\t100\t100\t\tfixture\n",
            "b\tB\t0\t0\t100\t100\t\tfixture\n",
        );
        let entries = dictc::parse_entries("details-conversion.tsv", source).expect("entries");
        let matrix = dictc::parse_connection(
            "details-conversion-matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile_with_details(
                &entries,
                &matrix,
                &[dictc::SourceDetail {
                    reading: "kana".into(),
                    surface: "Kana".into(),
                    left_id: 0,
                    right_id: 0,
                    description: "A source-backed test definition.".into(),
                    relations: vec![],
                }],
            )
            .expect("detail image")
            .into_boxed_slice(),
        );
        Arc::new(ConversionService::from_static_bytes(image).expect("detail conversion service"))
    }

    #[test]
    fn only_one_exact_system_edge_publishes_its_source_backed_detail() {
        let conversion = detail_conversion_fixture();
        conversion
            .with_candidates("kana", ConversionOptions::default(), |candidates| {
                let candidate = candidates
                    .iter()
                    .find(|candidate| candidate.text() == "Kana")
                    .expect("exact dictionary candidate");
                let mut output = OutputBuf::new();
                output.begin_candidates(0, 9).expect("candidate list");
                output
                    .push_candidate(candidate.text(), candidate.annotation())
                    .expect("candidate");
                publish_system_candidate_detail(
                    conversion.as_ref(),
                    candidate.system_entry_index().expect("exact entry ordinal"),
                    "kana",
                    &mut output,
                );
                let detail = output.to_output().candidate_detail.expect("exact detail");
                assert_eq!(detail.reading, "kana");
                assert_eq!(detail.definition, "A source-backed test definition.");
            })
            .expect("conversion");

        conversion
            .with_candidates("ab", ConversionOptions::default(), |candidates| {
                let compound = candidates
                    .iter()
                    .find(|candidate| candidate.text() == "AB")
                    .expect("compound candidate");
                assert_eq!(compound.system_entry_index(), None);
            })
            .expect("compound conversion");
    }

    fn shifted_ascii_english_conversion_dispatcher() -> Dispatcher {
        let source = concat!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
            "\u{3042}\u{3044}\u{3042}\u{3080}\tIAM\t0\t0\t100\t100\t\tfixture\n",
            "claude\tClaude\t0\t0\t100\t100\tit\tfixture\n",
            "claude\tClaude Code\t0\t0\t150\t150\tit\tfixture\n",
            "openai\tOpenAI\t0\t0\t100\t100\tit\tfixture\n",
        );
        let mut entries = dictc::parse_entries("shifted-ascii.tsv", source).expect("entries");
        let mut curated = dictc::parse_entries(
            "data/curated-terms.tsv",
            include_str!("../../../data/curated-terms.tsv"),
        )
        .expect("curated Shift terms");
        for entry in &mut curated {
            entry.left_id = 0;
            entry.right_id = 0;
        }
        entries.extend(curated);
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        let conversion = Arc::new(
            ConversionService::from_static_bytes(image).expect("shifted ASCII conversion fixture"),
        );
        Dispatcher::new_with_conversion(conversion).expect("shipped defaults")
    }

    fn prediction_dispatcher() -> (Dispatcher, crate::prediction::PredictionRuntime) {
        let source = "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t1\t100\t100\tpredict\tcommon\nかなた\t彼方\t0\t2\t200\t200\tpredict\tdirection\nかながわ\t神奈川\t0\t3\t300\t300\tpredict\tprefecture\n";
        let entries = dictc::parse_entries("prediction.tsv", source).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t4\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        let conversion = Arc::new(
            ConversionService::from_static_bytes(image).expect("prediction conversion fixture"),
        );
        let learning = Arc::new(LearningService::memory());
        let runtime = crate::prediction::PredictionRuntime::start(Arc::clone(&conversion))
            .expect("prediction runtime");
        let dispatcher = Dispatcher::new_with_runtime_configuration(
            conversion,
            learning,
            runtime.service(),
            Preferences::default(),
        )
        .expect("shipped defaults");
        (dispatcher, runtime)
    }

    fn phase_one_prediction_conversion() -> Arc<ConversionService> {
        let source = concat!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
            "\u{304b}\u{306a}\tsystem-first\t0\t0\t100\t100\tpredict\tsystem\n",
            "\u{304b}\u{306a}\u{306b}\tsystem-second\t0\t0\t200\t200\tpredict\tsystem\n",
        );
        prediction_conversion_from_source("phase-one-prediction.tsv", source)
    }

    fn prediction_conversion_from_source(file_name: &str, source: &str) -> Arc<ConversionService> {
        let entries = dictc::parse_entries(file_name, source).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        Arc::new(ConversionService::from_static_bytes(image).expect("conversion service"))
    }

    fn equal_reading_prediction_conversion() -> Arc<ConversionService> {
        prediction_conversion_from_source(
            "equal-reading-prediction.tsv",
            concat!(
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
                "\u{304b}\u{306a}\t\u{304b}\u{306a}\t0\t0\t100\t100\tpredict\tequal\n",
            ),
        )
    }

    fn empty_prediction_conversion() -> Arc<ConversionService> {
        prediction_conversion_from_source(
            "empty-prediction.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        )
    }

    fn phase_one_prediction_dispatcher(
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
    ) -> (Dispatcher, crate::prediction::PredictionRuntime) {
        let runtime = crate::prediction::PredictionRuntime::start_with_learning(
            Arc::clone(&conversion),
            Arc::clone(&learning),
        )
        .expect("prediction runtime");
        let dispatcher = Dispatcher::new_with_runtime_configuration(
            conversion,
            learning,
            runtime.service(),
            Preferences::default(),
        )
        .expect("shipped defaults");
        (dispatcher, runtime)
    }

    fn phase_one_learning_path(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "sakura-phase-one-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create learning test directory");
        root.join("learning.log")
    }

    fn segmented_conversion_dispatcher() -> Dispatcher {
        let source = "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nきょう\t今日\t0\t0\t100\t100\t\tcommon\nきょう\t京\t0\t0\t200\t200\t\talternative\nです\tです\t0\t0\t100\t100\t\tcommon\nです\tDESU\t0\t0\t200\t200\tit\tIT\n";
        let entries = dictc::parse_entries("segments.tsv", source).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        let conversion = Arc::new(
            ConversionService::from_static_bytes(image).expect("conversion service fixture"),
        );
        Dispatcher::new_with_conversion(conversion).expect("shipped defaults")
    }

    fn contextual_conversion_dispatcher() -> Dispatcher {
        let source = "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nいしゃ\t医者\t3\t3\t100\t100\t\tcontext source\nに\tに\t3\t3\t100\t100\t\tparticle\nいった\t言った\t1\t1\t50\t50\t\tgeneric\nいった\t行った\t2\t2\t100\t100\t\tcontextual\nおわり\t終わり。\t3\t3\t100\t100\t\tboundary\n";
        let entries = dictc::parse_entries("context.tsv", source).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t4\ndefault\t0\ncost\t3\t1\t1000\ncost\t3\t2\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        let conversion = Arc::new(
            ConversionService::from_static_bytes(image).expect("conversion service fixture"),
        );
        Dispatcher::new_with_conversion(conversion).expect("shipped defaults")
    }

    fn char_key(c: char) -> KeyInput {
        KeyInput {
            code: KeyCode::Char,
            ch: Some(c),
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        }
    }

    fn shifted_char_key(c: char) -> KeyInput {
        KeyInput {
            modifiers: Modifiers::SHIFT,
            ..char_key(c)
        }
    }

    fn test_only_char_key(c: char) -> KeyInput {
        KeyInput {
            test_only: true,
            ..char_key(c)
        }
    }

    fn named_key(code: KeyCode) -> KeyInput {
        KeyInput {
            code,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        }
    }

    fn modified_named_key(code: KeyCode, modifiers: Modifiers) -> KeyInput {
        KeyInput {
            modifiers,
            ..named_key(code)
        }
    }

    fn create_session(dispatcher: &mut Dispatcher, out: &mut OutputBuf, name: &str) -> SessionId {
        match dispatcher.dispatch(
            &Request::CreateSession {
                process_name: name.to_string(),
            },
            out,
        ) {
            Reply::Message(Response::SessionCreated { session, .. }) => session,
            other => panic!("expected SessionCreated, got {other:?}"),
        }
    }

    fn type_word(dispatcher: &mut Dispatcher, session: SessionId, word: &str, out: &mut OutputBuf) {
        for c in word.chars() {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(c),
                },
                out,
            );
        }
    }

    fn raw_preedit(dispatcher: &mut Dispatcher, session: SessionId) -> String {
        let mut session = dispatcher
            .sessions
            .get(session)
            .expect("raw preedit session")
            .clone();
        let mut out = OutputBuf::new();
        render_preedit(
            &mut session,
            &dispatcher.table,
            &dispatcher.normalizer,
            dispatcher.conversion.as_deref(),
            &mut dispatcher.scratch,
            &mut out,
        )
        .expect("raw preedit renders");
        out.preedit_text().to_owned()
    }

    #[test]
    fn developer_history_captures_real_keys_but_not_test_unclassified_or_sensitive_keys() {
        let path = std::env::temp_dir().join(format!(
            "sakura-dispatch-history-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let history = InputHistoryService::open(&path).expect("history");
        let mut dispatcher = builtin_dispatcher();
        dispatcher.set_input_history(Arc::clone(&history));
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");

        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Unclassified,
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('u'),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('k'),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: test_only_char_key('x'),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Password,
            },
            &mut out,
        );
        for (scope, character) in [
            (InputScope::Password, 's'),
            (InputScope::Url, 'u'),
            (InputScope::Email, 'e'),
            (InputScope::Digits, 'd'),
        ] {
            dispatcher.dispatch(&Request::SetInputScope { session, scope }, &mut out);
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(character),
                },
                &mut out,
            );
        }
        history.flush().expect("flush");

        let snapshot = crate::input_history::read_snapshot(&path).expect("snapshot");
        assert_eq!(snapshot.records.len(), 2);
        let stats = history.stats().snapshot();
        assert_eq!(stats.excluded_unclassified_events, 1);
        assert_eq!(stats.excluded_sensitive_events, 4);
        // Probe evaluation is deliberately side-effect free. The history
        // service still exposes a defensive test-only admission counter, but
        // a real Probe never reaches that admission boundary.
        assert_eq!(stats.excluded_test_only_events, 0);
        let InputHistoryRecord::Key(record) = &snapshot.records[0] else {
            panic!("expected key history record");
        };
        assert_eq!(record.character, Some('k'));
        assert_eq!(record.scope, ScopeClass::Normal);
        assert_eq!(record.session, 1);
        let InputHistoryRecord::Key(next) = &snapshot.records[1] else {
            panic!("expected second key history record");
        };
        assert_eq!(next.character, Some('a'));
        assert_eq!(next.preedit_before, record.preedit_after);
        assert_ne!(next.preedit_before, "ka");
        history.stop().expect("stop");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_only_enter_preserves_learning_preference_and_input_history_before_real_enter() {
        let reading = "\u{304b}\u{306a}";
        let learning = Arc::new(LearningService::memory());
        learning.learn(reading, "preferred", 0, 0);
        let preference_before = learning.preference(reading, 0, [("preferred", 0)]);
        let generation_before = learning.generation();

        let history_root = phase_one_learning_path("test-only-enter-history");
        let history_path = history_root.with_file_name("input-history.bin");
        let history = InputHistoryService::open(&history_path).expect("history");
        let mut dispatcher =
            Dispatcher::new_with_services(conversion_fixture(), Arc::clone(&learning))
                .expect("dispatcher");
        dispatcher.set_input_history(Arc::clone(&history));
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "test-only-enter.exe");
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        );
        type_word(&mut dispatcher, session, "kana", &mut out);
        history.clear().expect("clear setup history");
        history.flush().expect("flush setup history");
        let session_before = dispatcher.sessions.get(session).expect("session").clone();
        assert!(crate::input_history::read_snapshot(&history_path)
            .expect("empty setup history")
            .records
            .is_empty());

        let probe_enter = KeyInput {
            test_only: true,
            ..named_key(KeyCode::Enter)
        };
        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: probe_enter,
                },
                &mut out,
            ),
            Reply::Output
        );
        let probe_output = out.to_output();
        assert!(probe_output.consumed);
        assert_eq!(learning.generation(), generation_before);
        assert_eq!(
            learning.preference(reading, 0, [("preferred", 0)]),
            preference_before,
            "Probe must not mutate the learning preference"
        );
        history.flush().expect("flush test-only Enter");
        assert!(
            crate::input_history::read_snapshot(&history_path)
                .expect("test-only Enter history")
                .records
                .is_empty(),
            "test-only Enter must emit neither a key nor a commit record"
        );
        assert_eq!(
            dispatcher.sessions.get(session).expect("session"),
            &session_before,
            "test-only Enter must leave the live session unchanged"
        );

        dispatcher
            .sessions
            .get_mut(session)
            .expect("session")
            .clone_from(&session_before);
        let real_enter = named_key(KeyCode::Enter);
        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: real_enter,
                },
                &mut out,
            ),
            Reply::Output
        );
        let real_output = out.to_output();
        assert_eq!(real_output.consumed, probe_output.consumed);
        assert_eq!(real_output.commit, probe_output.commit);
        assert!(real_output.commit.is_some());
        history.flush().expect("flush real Enter");
        let records = crate::input_history::read_snapshot(&history_path)
            .expect("real Enter history")
            .records;
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record, InputHistoryRecord::Commit(_)))
                .count(),
            1,
            "the real Enter must emit one commit record"
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record, InputHistoryRecord::Key(_)))
                .count(),
            1,
            "the real Enter must emit one key record"
        );
        let commit = records.iter().find_map(|record| match record {
            InputHistoryRecord::Commit(record) => Some(record),
            InputHistoryRecord::Key(_) => None,
        });
        assert_eq!(commit.map(|record| record.reading.as_str()), Some(reading));
        assert_eq!(learning.generation(), generation_before + 1);

        history.stop().expect("stop history");
        std::fs::remove_dir_all(history_root.parent().expect("history root"))
            .expect("remove history test directory");
    }

    #[test]
    fn test_only_scope_sensitive_transition_is_pure_and_matches_real_scope_key() {
        let mut probe_dispatcher = builtin_dispatcher();
        let mut apply_dispatcher = builtin_dispatcher();
        let mut probe_out = OutputBuf::new();
        let mut apply_out = OutputBuf::new();
        let probe_session =
            create_session(&mut probe_dispatcher, &mut probe_out, "probe-scope.exe");
        let apply_session =
            create_session(&mut apply_dispatcher, &mut apply_out, "apply-scope.exe");

        for (dispatcher, session, out) in [
            (&mut probe_dispatcher, probe_session, &mut probe_out),
            (&mut apply_dispatcher, apply_session, &mut apply_out),
        ] {
            assert_eq!(
                dispatcher.dispatch(
                    &Request::SetInputScope {
                        session,
                        scope: InputScope::Normal,
                    },
                    out,
                ),
                Reply::Message(Response::Ok)
            );
            type_word(dispatcher, session, "ka", out);
        }

        let session_before = probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("probe session")
            .clone();
        let cache_before = (*probe_dispatcher.prediction_cache).clone();
        assert_eq!(
            probe_dispatcher.dispatch(
                &Request::ProbeKey {
                    session: probe_session,
                    scope: InputScope::Password,
                    fresh_context: false,
                    key: test_only_char_key('x'),
                },
                &mut probe_out,
            ),
            Reply::Output
        );
        let probe_output = probe_out.to_output();
        assert_eq!(
            probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("unchanged probe session"),
            &session_before,
            "a sensitive Probe transition must not reset the live composition"
        );
        assert_eq!(
            *probe_dispatcher.prediction_cache, cache_before,
            "a sensitive Probe transition must not clear the live cache"
        );

        assert_eq!(
            apply_dispatcher.dispatch(
                &Request::SetInputScope {
                    session: apply_session,
                    scope: InputScope::Password,
                },
                &mut apply_out,
            ),
            Reply::Message(Response::Ok)
        );
        assert_eq!(
            apply_dispatcher.dispatch(
                &Request::SendKey {
                    session: apply_session,
                    key: char_key('x'),
                },
                &mut apply_out,
            ),
            Reply::Output
        );
        assert_eq!(
            probe_output,
            apply_out.to_output(),
            "Probe must match the subsequent real scope publication and key"
        );
    }

    #[test]
    fn test_only_scope_from_sensitive_restores_mode_purely_and_matches_real_scope_key() {
        let mut probe_dispatcher = builtin_dispatcher();
        let mut apply_dispatcher = builtin_dispatcher();
        let mut probe_out = OutputBuf::new();
        let mut apply_out = OutputBuf::new();
        let probe_session =
            create_session(&mut probe_dispatcher, &mut probe_out, "probe-restore.exe");
        let apply_session =
            create_session(&mut apply_dispatcher, &mut apply_out, "apply-restore.exe");

        for (dispatcher, session, out) in [
            (&mut probe_dispatcher, probe_session, &mut probe_out),
            (&mut apply_dispatcher, apply_session, &mut apply_out),
        ] {
            dispatcher.sessions.get_mut(session).expect("session").mode = Mode::Katakana;
            assert_eq!(
                dispatcher.dispatch(
                    &Request::SetInputScope {
                        session,
                        scope: InputScope::Password,
                    },
                    out,
                ),
                Reply::Message(Response::Ok)
            );
        }

        let session_before = probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("sensitive probe session")
            .clone();
        assert_eq!(session_before.mode(), Mode::Direct);
        let cache_before = (*probe_dispatcher.prediction_cache).clone();
        assert_eq!(
            probe_dispatcher.dispatch(
                &Request::ProbeKey {
                    session: probe_session,
                    scope: InputScope::Normal,
                    fresh_context: false,
                    key: test_only_char_key('k'),
                },
                &mut probe_out,
            ),
            Reply::Output
        );
        let probe_output = probe_out.to_output();
        assert_eq!(
            probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("unchanged sensitive session"),
            &session_before
        );
        assert_eq!(*probe_dispatcher.prediction_cache, cache_before);

        assert_eq!(
            apply_dispatcher.dispatch(
                &Request::SetInputScope {
                    session: apply_session,
                    scope: InputScope::Normal,
                },
                &mut apply_out,
            ),
            Reply::Message(Response::Ok)
        );
        assert_eq!(
            apply_dispatcher.dispatch(
                &Request::SendKey {
                    session: apply_session,
                    key: char_key('k'),
                },
                &mut apply_out,
            ),
            Reply::Output
        );
        assert_eq!(
            probe_output,
            apply_out.to_output(),
            "Probe must match the real sensitive-to-Normal transition"
        );
    }

    #[test]
    fn test_only_context_replacement_probe_uses_fresh_session_for_first_character() {
        let mut probe_dispatcher = builtin_dispatcher();
        let mut apply_dispatcher = builtin_dispatcher();
        let mut probe_out = OutputBuf::new();
        let mut apply_out = OutputBuf::new();
        let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "notepad.exe");
        let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "notepad.exe");

        // Make the old context observably non-fresh. A replacement Probe must
        // not carry this composition or its user-selected mode into the new
        // document; the live old session must remain untouched.
        probe_dispatcher
            .sessions
            .get_mut(probe_session)
            .expect("session")
            .mode = Mode::Katakana;
        type_word(&mut probe_dispatcher, probe_session, "ka", &mut probe_out);
        let old_session = probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("old session")
            .clone();

        assert_eq!(
            probe_dispatcher.dispatch(
                &Request::ProbeKey {
                    session: probe_session,
                    scope: InputScope::Normal,
                    fresh_context: true,
                    key: test_only_char_key('k'),
                },
                &mut probe_out,
            ),
            Reply::Output
        );
        let probe_output = probe_out.to_output();
        assert_eq!(
            probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("unchanged old session"),
            &old_session,
            "fresh-context Probe must not mutate the old session"
        );

        assert_eq!(
            apply_dispatcher.dispatch(
                &Request::SetInputScope {
                    session: apply_session,
                    scope: InputScope::Normal,
                },
                &mut apply_out,
            ),
            Reply::Message(Response::Ok)
        );
        assert_eq!(
            apply_dispatcher.dispatch(
                &Request::SendKey {
                    session: apply_session,
                    key: char_key('k'),
                },
                &mut apply_out,
            ),
            Reply::Output
        );
        assert_eq!(
            probe_output,
            apply_out.to_output(),
            "replacement Probe and fresh-context Apply must consume the first character identically"
        );
    }

    #[test]
    fn test_only_context_replacement_probe_preserves_half_full_key_parity() {
        let mut probe_dispatcher = builtin_dispatcher();
        let mut apply_dispatcher = builtin_dispatcher();
        let mut probe_out = OutputBuf::new();
        let mut apply_out = OutputBuf::new();
        let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "notepad.exe");
        let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "notepad.exe");

        probe_dispatcher
            .sessions
            .get_mut(probe_session)
            .expect("session")
            .mode = Mode::Katakana;
        type_word(&mut probe_dispatcher, probe_session, "ka", &mut probe_out);
        let old_session = probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("old session")
            .clone();
        let key = named_key(KeyCode::HankakuZenkaku);

        assert_eq!(
            probe_dispatcher.dispatch(
                &Request::ProbeKey {
                    session: probe_session,
                    scope: InputScope::Normal,
                    fresh_context: true,
                    key: KeyInput {
                        test_only: true,
                        ..key
                    },
                },
                &mut probe_out,
            ),
            Reply::Output
        );
        let probe_output = probe_out.to_output();
        assert_eq!(
            probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("unchanged old session"),
            &old_session
        );

        assert_eq!(
            apply_dispatcher.dispatch(
                &Request::SetInputScope {
                    session: apply_session,
                    scope: InputScope::Normal,
                },
                &mut apply_out,
            ),
            Reply::Message(Response::Ok)
        );
        assert_eq!(
            apply_dispatcher.dispatch(
                &Request::SendKey {
                    session: apply_session,
                    key,
                },
                &mut apply_out,
            ),
            Reply::Output
        );
        assert_eq!(
            probe_output,
            apply_out.to_output(),
            "replacement Probe and fresh-context Apply must preserve half/full consumed parity"
        );
    }

    #[test]
    fn test_only_unclassified_scope_preserves_real_composition_without_persistence_or_probe_work() {
        let learning = Arc::new(LearningService::memory());
        let history_root = phase_one_learning_path("test-only-unclassified-scope");
        let history_path = history_root.with_file_name("input-history.bin");
        let history = InputHistoryService::open(&history_path).expect("history");
        let (mut dispatcher, runtime) = phase_one_prediction_dispatcher(
            phase_one_prediction_conversion(),
            Arc::clone(&learning),
        );
        dispatcher.set_input_history(Arc::clone(&history));
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "probe-unclassified.exe");
        assert_eq!(
            dispatcher.dispatch(
                &Request::SetInputScope {
                    session,
                    scope: InputScope::Normal,
                },
                &mut out,
            ),
            Reply::Message(Response::Ok)
        );
        type_word(&mut dispatcher, session, "ka", &mut out);
        history.clear().expect("clear setup history");
        history.flush().expect("flush setup history");

        let session_before = dispatcher.sessions.get(session).expect("session").clone();
        let cache_before = (*dispatcher.prediction_cache).clone();
        let generation_before = learning.generation();
        let request_count_before = runtime.service().request_count();
        assert!(crate::input_history::read_snapshot(&history_path)
            .expect("empty setup history")
            .records
            .is_empty());

        assert_eq!(
            dispatcher.dispatch(
                &Request::ProbeKey {
                    session,
                    scope: InputScope::Unclassified,
                    fresh_context: false,
                    key: test_only_char_key('x'),
                },
                &mut out,
            ),
            Reply::Output
        );
        let probe_output = out.to_output();
        assert_eq!(
            dispatcher.sessions.get(session).expect("unchanged session"),
            &session_before
        );
        assert_eq!(*dispatcher.prediction_cache, cache_before);
        assert_eq!(learning.generation(), generation_before);
        assert_eq!(runtime.service().request_count(), request_count_before);
        history.flush().expect("flush test-only unclassified");
        assert!(
            crate::input_history::read_snapshot(&history_path)
                .expect("test-only unclassified history")
                .records
                .is_empty(),
            "Unclassified Probe must not persist input history"
        );

        dispatcher
            .sessions
            .get_mut(session)
            .expect("restore session")
            .clone_from(&session_before);
        dispatcher
            .prediction_cache
            .as_mut()
            .clone_from(&cache_before);
        assert_eq!(
            dispatcher.dispatch(
                &Request::SetInputScope {
                    session,
                    scope: InputScope::Unclassified,
                },
                &mut out,
            ),
            Reply::Message(Response::Ok)
        );
        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key('x'),
                },
                &mut out,
            ),
            Reply::Output
        );
        let real_output = out.to_output();
        assert_eq!(probe_output.consumed, real_output.consumed);
        assert_eq!(probe_output.beep, real_output.beep);
        assert_eq!(probe_output.mode, real_output.mode);
        assert_eq!(probe_output.preedit, real_output.preedit);
        assert_eq!(probe_output.commit, real_output.commit);
        assert_eq!(probe_output.delete_before, real_output.delete_before);
        history.flush().expect("flush real unclassified");
        assert!(
            crate::input_history::read_snapshot(&history_path)
                .expect("real unclassified history")
                .records
                .is_empty(),
            "Unclassified Apply must remain unrecorded"
        );

        runtime.stop().expect("prediction worker joins");
        history.stop().expect("stop history");
        drop(dispatcher);
        drop(learning);
        std::fs::remove_dir_all(history_root.parent().expect("history root"))
            .expect("remove unclassified test directory");
    }

    #[test]
    fn test_only_delete_prediction_history_preserves_durable_state_and_cache_before_real_control_delete(
    ) {
        let learning_path = phase_one_learning_path("test-only-delete-history");
        let history_path = learning_path.with_file_name("input-history.bin");
        let reading = "\u{304b}\u{306a}";
        let learning = Arc::new(LearningService::open(&learning_path).expect("durable learning"));
        learning.learn(reading, "history-only", 0, 7);
        learning.maintain().expect("flush durable learning");
        let learning_bytes_before = std::fs::read(&learning_path).expect("learning bytes");
        let learning_generation_before = learning.generation();
        let history = InputHistoryService::open(&history_path).expect("history");
        let (mut dispatcher, runtime) = phase_one_prediction_dispatcher(
            phase_one_prediction_conversion(),
            Arc::clone(&learning),
        );
        dispatcher.set_input_history(Arc::clone(&history));
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "test-only-delete.exe");
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        );
        type_word(&mut dispatcher, session, "kana", &mut out);
        assert_eq!(
            dispatcher
                .prediction_cache
                .candidates(
                    session,
                    dispatcher
                        .sessions
                        .get(session)
                        .expect("session")
                        .prediction_generation,
                )
                .expect("history prediction")[0]
                .source(),
            PredictionSource::History
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .suggestion_focused
        );
        history.clear().expect("clear setup history");
        history.flush().expect("flush setup history");
        let session_before = dispatcher.sessions.get(session).expect("session").clone();
        let cache_before = (*dispatcher.prediction_cache).clone();
        let history_before = crate::input_history::read_snapshot(&history_path)
            .expect("empty setup history")
            .records;

        let probe_key = KeyInput {
            test_only: true,
            ..modified_named_key(KeyCode::Delete, Modifiers::CTRL)
        };
        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: probe_key,
                },
                &mut out,
            ),
            Reply::Output
        );
        let probe_output = out.to_output();
        assert!(probe_output.consumed);
        assert!(!probe_output.beep);
        assert_eq!(
            dispatcher.sessions.get(session).expect("session"),
            &session_before
        );
        assert_eq!(*dispatcher.prediction_cache, cache_before);
        assert_eq!(learning.generation(), learning_generation_before);
        assert_eq!(
            std::fs::read(&learning_path).expect("unchanged learning bytes"),
            learning_bytes_before
        );
        history.flush().expect("flush test-only delete");
        assert_eq!(
            crate::input_history::read_snapshot(&history_path)
                .expect("unchanged history")
                .records,
            history_before,
            "test-only DeletePredictionHistory must not write input history"
        );

        dispatcher
            .sessions
            .get_mut(session)
            .expect("session")
            .clone_from(&session_before);
        dispatcher
            .prediction_cache
            .as_mut()
            .clone_from(&cache_before);
        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
                },
                &mut out,
            ),
            Reply::Output
        );
        let real_output = out.to_output();
        assert_eq!(real_output.consumed, probe_output.consumed);
        assert_eq!(real_output.beep, probe_output.beep);
        assert!(
            learning.generation() > learning_generation_before,
            "real DeletePredictionHistory must advance learning generation"
        );
        let mut found = false;
        learning.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
            found |= candidate_reading == reading && surface == "history-only";
            true
        });
        assert!(
            !found,
            "real control delete must remove the durable history pair"
        );
        history.flush().expect("flush real delete");
        let real_history = crate::input_history::read_snapshot(&history_path)
            .expect("real delete history")
            .records;
        assert_eq!(
            real_history
                .iter()
                .filter(|record| matches!(record, InputHistoryRecord::Key(_)))
                .count(),
            1,
            "the real control delete must emit one key record"
        );
        assert!(dispatcher
            .prediction_cache
            .candidates(
                session,
                dispatcher
                    .sessions
                    .get(session)
                    .expect("session")
                    .prediction_generation,
            )
            .into_iter()
            .flatten()
            .all(|candidate| candidate.surface() != "history-only"));

        runtime.stop().expect("prediction worker joins");
        history.stop().expect("stop history");
        drop(dispatcher);
        drop(learning);
        std::fs::remove_dir_all(learning_path.parent().expect("learning root"))
            .expect("remove learning test directory");
    }

    #[test]
    fn test_only_probe_does_not_enqueue_prediction_work_and_real_key_keeps_parity() {
        let learning = Arc::new(LearningService::memory());
        let (mut dispatcher, runtime) = phase_one_prediction_dispatcher(
            phase_one_prediction_conversion(),
            Arc::clone(&learning),
        );
        let prediction = runtime.service();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "test-only-prediction.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.prediction_cache.clear();
        let cache_before = (*dispatcher.prediction_cache).clone();
        let request_count_before = prediction.request_count();
        let probe_key = KeyInput {
            test_only: true,
            ..named_key(KeyCode::Tab)
        };
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: probe_key,
            },
            &mut out,
        );
        let probe_output = out.to_output();
        assert_eq!(prediction.request_count(), request_count_before);
        assert_eq!(*dispatcher.prediction_cache, cache_before);

        let real_key = named_key(KeyCode::Tab);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: real_key,
            },
            &mut out,
        );
        let real_output = out.to_output();
        assert_eq!(real_output.consumed, probe_output.consumed);
        assert!(
            prediction.request_count() > request_count_before,
            "Apply must enqueue the equivalent eligible prediction request"
        );
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn test_only_probe_uses_ephemeral_stale_cache_and_keeps_live_cache_unchanged() {
        let learning = Arc::new(LearningService::memory());
        let conversion = phase_one_prediction_conversion();
        let (mut probe_dispatcher, probe_runtime) =
            phase_one_prediction_dispatcher(Arc::clone(&conversion), Arc::clone(&learning));
        let (mut apply_dispatcher, apply_runtime) =
            phase_one_prediction_dispatcher(conversion, Arc::clone(&learning));
        let mut probe_out = OutputBuf::new();
        let mut apply_out = OutputBuf::new();
        let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "probe.exe");
        let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "apply.exe");
        type_word(&mut probe_dispatcher, probe_session, "kana", &mut probe_out);
        type_word(&mut apply_dispatcher, apply_session, "kana", &mut apply_out);
        probe_dispatcher.dispatch(
            &Request::SendKey {
                session: probe_session,
                key: named_key(KeyCode::Tab),
            },
            &mut probe_out,
        );
        apply_dispatcher.dispatch(
            &Request::SendKey {
                session: apply_session,
                key: named_key(KeyCode::Tab),
            },
            &mut apply_out,
        );
        let raw_reading = probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("probe session")
            .preedit
            .as_str()
            .to_owned();
        let cache_before = (*probe_dispatcher.prediction_cache).clone();

        learning.learn(&raw_reading, "stale-generation", 0, 0);
        let probe_key = KeyInput {
            test_only: true,
            ..named_key(KeyCode::Tab)
        };
        probe_dispatcher.dispatch(
            &Request::SendKey {
                session: probe_session,
                key: probe_key,
            },
            &mut probe_out,
        );
        let probe_preedit = probe_out.preedit_text().to_owned();
        let probe_output = probe_out.to_output();
        assert_eq!(*probe_dispatcher.prediction_cache, cache_before);
        assert_eq!(probe_preedit, raw_reading);

        apply_dispatcher.dispatch(
            &Request::SendKey {
                session: apply_session,
                key: named_key(KeyCode::Tab),
            },
            &mut apply_out,
        );
        let apply_output = apply_out.to_output();
        assert_eq!(apply_output.consumed, probe_output.consumed);

        probe_runtime.stop().expect("probe prediction worker joins");
        apply_runtime.stop().expect("apply prediction worker joins");
    }

    #[test]
    fn app_profile_is_copied_once_when_the_context_is_created() {
        let profile = AppProfile {
            process_name: "custom.exe".to_owned(),
            default_mode: Mode::HalfAlnum,
            normalizer: Normalizer {
                width: WidthPolicy {
                    alnum: Width::Full,
                    number: Width::Half,
                    symbol: Width::FollowMode,
                },
                punctuation: PunctuationStyle::CommaPeriod,
            },
            prediction_enabled: false,
            suggest_accept: SuggestAccept::Disabled,
        };
        let mut dispatcher = Dispatcher::new_with_configuration_and_profiles(
            conversion_fixture(),
            Arc::new(LearningService::memory()),
            Preferences::default(),
            Arc::from(vec![profile]),
        )
        .expect("dispatcher");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "CUSTOM.EXE");
        let configured = dispatcher.sessions.get(session).expect("session");
        assert_eq!(configured.mode, Mode::HalfAlnum);
        assert_eq!(configured.normalizer.width.alnum, Width::Full);
        assert!(!configured.prediction_enabled);
        assert_eq!(configured.suggest_accept, SuggestAccept::Disabled);

        dispatcher.sessions.get_mut(session).expect("session").mode = Mode::Katakana;
        assert!(matches!(
            dispatcher.dispatch(
                &Request::SetInputScope {
                    session,
                    scope: InputScope::Normal,
                },
                &mut out,
            ),
            Reply::Message(Response::Ok)
        ));
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode,
            Mode::Katakana,
            "focus/input-scope updates must not reapply a profile"
        );
    }

    #[test]
    fn reconversion_preview_returns_conversion_candidates_without_mutating_the_session() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");

        let reply = dispatcher.dispatch(
            &Request::Reconvert {
                session,
                text: "仮名".to_owned(),
                preview: true,
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Output);
        assert_eq!(out.preedit_text(), "仮名");
        assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Idle,
            "GetReconversion must be observational"
        );
    }

    #[test]
    fn actual_reconversion_enters_conversion_over_the_recovered_reading() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");

        let reply = dispatcher.dispatch(
            &Request::Reconvert {
                session,
                text: "仮名".to_owned(),
                preview: false,
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Output);
        assert!(out.consumed);
        assert_eq!(out.preedit_text(), "仮名");
        assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.state(), State::Converting);
        assert_eq!(live.preedit.as_str(), "かな");
    }

    #[test]
    fn password_scope_reconversion_is_refused_and_finishes_idle() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        assert_eq!(
            dispatcher.dispatch(
                &Request::SetInputScope {
                    session,
                    scope: InputScope::Password,
                },
                &mut out,
            ),
            Reply::Message(Response::Ok)
        );

        assert_eq!(
            dispatcher.dispatch(
                &Request::Reconvert {
                    session,
                    text: "secret".to_owned(),
                    preview: false,
                },
                &mut out,
            ),
            Reply::Message(Response::Error(ErrorCode::Malformed))
        );
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Idle
        );
        assert!(out.to_output().preedit.is_none());
    }

    #[test]
    fn conversion_starts_compact_and_expansion_is_idempotent() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        let compact = out.to_output().candidates.expect("compact candidates");
        assert_eq!(compact.kind, CandidateKind::Conversion);
        assert_eq!(compact.presentation, CandidatePresentation::Compact);
        assert_eq!(
            compact.visible_range(),
            usize::from(compact.selected)..usize::from(compact.selected) + 1
        );
        let selected = compact.selected;
        let count = compact.items.len();

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let expanded = out.to_output().candidates.expect("expanded candidates");
        assert!(!out.beep);
        assert_eq!(expanded.kind, CandidateKind::Conversion);
        assert_eq!(expanded.presentation, CandidatePresentation::Expanded);
        assert_eq!(expanded.selected, selected);
        assert_eq!(expanded.items.len(), count);
        assert_eq!(expanded.visible_range(), expanded.current_page_range());

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let repeated = out.to_output().candidates.expect("expanded candidates");
        assert!(!out.beep, "repeated expansion is an idempotent success");
        assert_eq!(repeated, expanded);
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Converting
        );
    }

    #[test]
    fn conversion_navigation_expands_without_changing_candidate_kind() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert_eq!(
            out.to_output()
                .candidates
                .expect("compact candidates")
                .presentation,
            CandidatePresentation::Compact
        );

        for code in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::PageDown,
            KeyCode::PageUp,
        ] {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(code),
                },
                &mut out,
            );
            let candidates = out.to_output().candidates.expect("navigation candidates");
            assert_eq!(candidates.kind, CandidateKind::Conversion);
            assert_eq!(candidates.presentation, CandidatePresentation::Expanded);
            assert_eq!(candidates.visible_range(), candidates.current_page_range());
        }
    }

    #[test]
    fn custom_candidate_expand_outside_conversion_beeps_without_mutating_preedit() {
        let table = Table::builtin().expect("builtin table");
        let keymap =
            KeyMap::parse("[composing]\ntab = \"candidate_expand\"\n").expect("custom keymap");
        let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        let before = out.preedit_text().to_owned();

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );

        assert!(out.consumed);
        assert!(out.beep);
        assert_eq!(out.preedit_text(), before);
        assert!(!out.has_candidates());
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Composing
        );
    }

    #[test]
    fn microsoft_numbers_type_until_the_suggestion_list_takes_focus() {
        let (mut dispatcher, runtime) = prediction_dispatcher();
        let mut out = OutputBuf::new();

        // A visible-but-unfocused suggestion list is what Microsoft IME shows
        // while the user is still typing. A number there is text, not a
        // shortcut: taking it as a shortcut made `２` + `2` commit the second
        // suggestion instead of producing `２２`.
        let typing = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, typing, "kana", &mut out);
        let composed = out.preedit_text().to_owned();
        let suggestion = out.candidate(1).expect("second suggestion").0.to_owned();
        assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));
        assert_eq!(
            dispatcher.sessions.get(typing).expect("session").state(),
            State::Composing
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session: typing,
                key: char_key('2'),
            },
            &mut out,
        );
        assert!(!out.beep);
        assert_eq!(out.commit_text(), None, "a number must not commit here");
        assert_ne!(out.preedit_text(), suggestion);
        assert!(
            out.preedit_text().starts_with(composed.as_str())
                && out.preedit_text().len() > composed.len(),
            "the number extends the composition: {composed} -> {}",
            out.preedit_text()
        );
        assert_eq!(
            dispatcher.sessions.get(typing).expect("session").state(),
            State::Composing
        );

        // Tab focuses the list, which is exactly when Microsoft IME starts
        // honouring the numbers it draws beside each suggestion.
        let focused = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, focused, "kana", &mut out);
        let expected = out.candidate(1).expect("second suggestion").0.to_owned();
        dispatcher.dispatch(
            &Request::SendKey {
                session: focused,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher.sessions.get(focused).expect("session").state(),
            State::Predicting
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session: focused,
                key: char_key('2'),
            },
            &mut out,
        );
        assert!(!out.beep);
        assert_eq!(out.commit_text(), Some(expected.as_str()));
        assert_eq!(
            dispatcher.sessions.get(focused).expect("session").state(),
            State::Idle
        );

        // A slot the focused list does not have stays a recoverable beep.
        let invalid = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, invalid, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session: invalid,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let before = out.preedit_text().to_owned();
        let candidate_count = out.candidate_count();
        dispatcher.dispatch(
            &Request::SendKey {
                session: invalid,
                key: char_key('9'),
            },
            &mut out,
        );
        assert!(out.beep);
        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), before);
        assert_eq!(out.candidate_count(), candidate_count);
        assert_eq!(
            dispatcher.sessions.get(invalid).expect("session").state(),
            State::Predicting
        );
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn prediction_history_deletion_rejects_unfocused_system_and_user_candidates() {
        let (mut dispatcher, runtime) = prediction_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        let before = out.preedit_text().to_owned();

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
            },
            &mut out,
        );
        assert!(
            out.beep,
            "deletion without a focused prediction is rejected"
        );
        assert_eq!(out.preedit_text(), before);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let focused = out.preedit_text().to_owned();
        let generation = dispatcher
            .sessions
            .get(session)
            .expect("session")
            .prediction_generation;
        assert_eq!(
            dispatcher
                .prediction_cache
                .candidates(session, generation)
                .expect("system candidates")[0]
                .source(),
            PredictionSource::System
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
            },
            &mut out,
        );
        assert!(out.beep, "system candidates are never deletable");
        assert_eq!(out.preedit_text(), focused);
        runtime.stop().expect("prediction worker joins");

        let conversion = phase_one_prediction_conversion();
        conversion.replace_user_dictionary(
            UserDictionary::parse_tsv(
                "reading\tsurface\tpos\tcomment\n\u{304b}\u{306a}\tuser-only\tnoun\tuser\n",
            )
            .expect("user dictionary"),
        );
        let learning = Arc::new(LearningService::memory());
        let (mut dispatcher, runtime) = phase_one_prediction_dispatcher(conversion, learning);
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let focused = out.preedit_text().to_owned();
        let generation = dispatcher
            .sessions
            .get(session)
            .expect("session")
            .prediction_generation;
        assert_eq!(
            dispatcher
                .prediction_cache
                .candidates(session, generation)
                .expect("user candidates")[0]
                .source(),
            PredictionSource::User
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
            },
            &mut out,
        );
        assert!(out.beep, "user dictionary candidates are never deletable");
        assert_eq!(out.preedit_text(), focused);
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn focused_history_deletion_committed_observation_failure_refreshes_dispatch_and_restart() {
        let path = phase_one_learning_path("forget-history-committed-observation-failure");
        let temporary = path.with_extension("forget.tmp");
        let recovery = path.with_extension("forget.recovery");
        let learning = Arc::new(LearningService::open(&path).expect("durable learning"));
        let reading = "\u{304b}\u{306a}";
        learning.learn(reading, "history-only", 0, 7);
        let learning_generation = learning.generation();
        let (mut dispatcher, runtime) = phase_one_prediction_dispatcher(
            phase_one_prediction_conversion(),
            Arc::clone(&learning),
        );
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        let prediction_generation = dispatcher
            .sessions
            .get(session)
            .expect("session")
            .prediction_generation;
        assert_eq!(
            dispatcher
                .prediction_cache
                .candidates(session, prediction_generation)
                .expect("history candidates")[0]
                .source(),
            PredictionSource::History
        );
        let before = out.preedit_text().to_owned();

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let fault = crate::learning::ForgetPredictionCommittedObservationFault::install();
        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
            },
            &mut out,
        );
        drop(fault);

        assert_eq!(reply, Reply::Output, "the bound Ctrl+Delete is terminal");
        assert!(out.consumed);
        assert!(!out.beep);
        assert_eq!(out.preedit_text(), before);
        assert_eq!(learning.generation(), learning_generation + 1);
        assert_eq!(learning.maintenance_failures(), 1);
        let refreshed_generation = dispatcher
            .sessions
            .get(session)
            .expect("session")
            .prediction_generation;
        assert_ne!(refreshed_generation, prediction_generation);
        assert!(
            dispatcher
                .prediction_cache
                .candidates(session, refreshed_generation)
                .into_iter()
                .flatten()
                .all(|candidate| candidate.surface() != "history-only"),
            "the refreshed list must never replay the removed history entry"
        );
        assert!(
            out.to_output()
                .candidates
                .expect("refreshed output candidates")
                .items
                .iter()
                .all(|candidate| candidate.text != "history-only"),
            "the visible list must reflect the committed deletion"
        );
        assert!(path.exists(), "the filtered replacement is canonical");
        assert!(!temporary.exists(), "the replacement temp was consumed");
        assert!(!recovery.exists(), "the old backup cleanup completed");
        let mut live_history = false;
        learning.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
            live_history |= candidate_reading == reading && surface == "history-only";
            true
        });
        assert!(!live_history, "live history commits the durable removal");

        runtime.stop().expect("prediction worker joins");
        drop(dispatcher);
        drop(learning);
        let reopened = LearningService::open(&path).expect("reopen learning");
        let mut found = false;
        reopened.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
            found |= candidate_reading == reading && surface == "history-only";
            true
        });
        assert!(
            !found,
            "the removed history pair must stay absent after reopen"
        );
        drop(reopened);
        std::fs::remove_dir_all(path.parent().expect("test directory"))
            .expect("remove learning test directory");
    }

    #[test]
    fn focused_history_deletion_after_first_rename_observation_failure_preserves_dispatch_and_restart(
    ) {
        let path = phase_one_learning_path("forget-history-recovery-failure");
        let temporary = path.with_extension("forget.tmp");
        let recovery = path.with_extension("forget.recovery");
        let learning = Arc::new(LearningService::open(&path).expect("durable learning"));
        let reading = "\u{304b}\u{306a}";
        learning.learn(reading, "history-only", 0, 7);
        let old_bytes = std::fs::read(&path).expect("old canonical bytes");
        let learning_generation = learning.generation();
        let (mut dispatcher, runtime) = phase_one_prediction_dispatcher(
            phase_one_prediction_conversion(),
            Arc::clone(&learning),
        );
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        let prediction_generation = dispatcher
            .sessions
            .get(session)
            .expect("session")
            .prediction_generation;
        let cached_before = dispatcher
            .prediction_cache
            .candidates(session, prediction_generation)
            .expect("history candidates")
            .to_vec();
        assert_eq!(cached_before[0].source(), PredictionSource::History);

        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Tab),
                },
                &mut out,
            ),
            Reply::Output
        );
        assert!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .suggestion_focused,
            "Ctrl+Delete must target the focused history candidate"
        );
        let focused = out.preedit_text().to_owned();
        let listed_before = out
            .to_output()
            .candidates
            .expect("focused history candidate list");

        let fault = crate::learning::ForgetPredictionDeepRecoveryFault::install();
        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
            },
            &mut out,
        );
        drop(fault);

        assert_eq!(reply, Reply::Output, "the bound Ctrl+Delete is terminal");
        assert!(out.consumed, "the failed deletion remains consumed");
        assert!(out.beep, "the failed deletion is observable");
        assert_eq!(out.preedit_text(), focused);
        assert_eq!(out.commit_text(), None);
        assert_eq!(learning.generation(), learning_generation);
        assert_eq!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .prediction_generation,
            prediction_generation,
            "durable failure must not invalidate the prediction generation"
        );
        assert_eq!(
            dispatcher
                .prediction_cache
                .candidates(session, prediction_generation)
                .expect("preserved history cache"),
            cached_before.as_slice(),
            "the exact history candidate remains cached"
        );
        assert_eq!(
            out.to_output()
                .candidates
                .expect("preserved history candidate list"),
            listed_before,
            "the rendered output list remains authoritative"
        );
        let mut in_memory = false;
        learning.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
            in_memory |= candidate_reading == reading && surface == "history-only";
            true
        });
        assert!(in_memory, "the old history remains in memory");
        assert!(
            !path.exists(),
            "failed immediate recovery must not recreate the canonical log"
        );
        assert_eq!(
            std::fs::read(&recovery).expect("old recovery bytes"),
            old_bytes,
            "the old canonical log is the deterministic recovery authority"
        );
        assert!(
            temporary.exists(),
            "the filtered replacement remains cleanup-only until recovery"
        );

        runtime.stop().expect("prediction worker joins");
        drop(dispatcher);
        drop(learning);

        let reopened = LearningService::open(&path).expect("restart restores old history");
        assert!(path.exists(), "startup restores the canonical log first");
        assert!(!recovery.exists(), "recovery was consumed at startup");
        assert_eq!(
            std::fs::read(&path).expect("restored canonical bytes"),
            old_bytes
        );
        let mut restored = false;
        reopened.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
            restored |= candidate_reading == reading && surface == "history-only";
            true
        });
        assert!(restored, "restart restores the original history entry");
        reopened
            .maintain()
            .expect("cleanup stale filtered replacement");
        assert!(
            !temporary.exists(),
            "recovery cleanup removes the stale temp"
        );
        drop(reopened);
        std::fs::remove_dir_all(path.parent().expect("test directory"))
            .expect("remove learning test directory");
    }

    #[test]
    fn suggestions_focus_cycle_escape_and_commit_without_becoming_conversion_candidates() {
        let (mut dispatcher, runtime) = prediction_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        let raw_reading = out.preedit_text().to_owned();

        assert_eq!(out.preedit_text(), raw_reading);
        assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));
        assert_eq!(out.candidate_count(), 3);
        assert_eq!(out.candidate(0), Some(("仮名", "common")));
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Composing
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Predicting
        );
        assert_eq!(out.selected_candidate(), Some(0));
        let first_surface = out
            .candidate(0)
            .expect("first suggestion surface")
            .0
            .to_owned();
        assert_ne!(first_surface, raw_reading);
        assert_eq!(out.preedit_text(), first_surface);
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().preedit.as_str(),
            raw_reading
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(out.selected_candidate(), Some(1));
        let second_surface = out
            .candidate(1)
            .expect("second suggestion surface")
            .0
            .to_owned();
        assert_eq!(out.preedit_text(), second_surface);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Tab, Modifiers::SHIFT),
            },
            &mut out,
        );
        assert_eq!(out.selected_candidate(), Some(0));
        assert_eq!(out.preedit_text(), first_surface);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Escape),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Composing
        );
        assert!(!out.has_candidates());
        assert_eq!(out.preedit_text(), raw_reading);
        assert_eq!(out.preedit_text(), "かな");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("仮名"));
        assert_eq!(out.commit_text(), Some(first_surface.as_str()));
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Idle
        );
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn prediction_input_history_uses_visible_projection() {
        let path = phase_one_learning_path("prediction-projection-history");
        let history = InputHistoryService::open(&path).expect("history");
        let learning = Arc::new(LearningService::memory());
        let (mut dispatcher, runtime) =
            phase_one_prediction_dispatcher(phase_one_prediction_conversion(), learning);
        dispatcher.set_input_history(Arc::clone(&history));
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "history-projection.exe");
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        );
        type_word(&mut dispatcher, session, "kana", &mut out);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let first_surface = out
            .candidate(0)
            .expect("first suggestion surface")
            .0
            .to_owned();
        assert_eq!(out.preedit_text(), first_surface);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let second_surface = out
            .candidate(1)
            .expect("second suggestion surface")
            .0
            .to_owned();
        assert_eq!(out.preedit_text(), second_surface);
        history.flush().expect("flush projection history");

        let snapshot = crate::input_history::read_snapshot(&path).expect("history snapshot");
        let prediction_keys: Vec<_> = snapshot
            .records
            .iter()
            .filter_map(|record| match record {
                InputHistoryRecord::Key(record) if record.action == "predict_next" => Some(record),
                _ => None,
            })
            .collect();
        assert!(prediction_keys.len() >= 2);
        assert_eq!(prediction_keys[0].preedit_after, first_surface);
        assert_eq!(
            prediction_keys[1].preedit_before,
            prediction_keys[0].preedit_after
        );
        assert_eq!(prediction_keys[1].preedit_after, second_surface);

        history.stop().expect("stop history");
        runtime.stop().expect("prediction worker joins");
        std::fs::remove_dir_all(path.parent().expect("history root"))
            .expect("remove history directory");
    }

    #[test]
    fn prediction_focus_succeeds_when_candidate_surface_equals_reading() {
        let learning = Arc::new(LearningService::memory());
        let (mut dispatcher, runtime) =
            phase_one_prediction_dispatcher(equal_reading_prediction_conversion(), learning);
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "equal-reading.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        let reading = out.preedit_text().to_owned();

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Predicting
        );
        assert_eq!(out.selected_candidate(), Some(0));
        assert_eq!(out.preedit_text(), reading);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some(reading.as_str()));
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Idle
        );
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn prediction_explicit_retry_is_bounded_per_generation() {
        let learning = Arc::new(LearningService::memory());
        let (mut dispatcher, runtime) =
            phase_one_prediction_dispatcher(empty_prediction_conversion(), learning);
        let prediction = runtime.service();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "empty-prediction.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        let automatic_requests = prediction.request_count();
        assert!(!out.has_candidates());

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let retry_requests = prediction.request_count();
        assert_eq!(retry_requests, automatic_requests + 1);
        assert!(out.consumed);
        assert!(out.beep);
        assert!(!out.has_candidates());
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Composing
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(prediction.request_count(), retry_requests);
        assert!(out.consumed);
        assert!(out.beep);
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Composing
        );
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn prediction_explicit_retry_success_focuses_same_key_without_second_retry() {
        let learning = Arc::new(LearningService::memory());
        let (mut dispatcher, runtime) =
            phase_one_prediction_dispatcher(empty_prediction_conversion(), learning);
        let prediction = runtime.service();
        prediction.test_script_prediction("\u{304b}", "\u{58f2}\u{4e0a}");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "retry-success.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        let automatic_requests = prediction.request_count();
        assert!(automatic_requests > 0);
        assert!(!out.has_candidates());
        let generation = dispatcher
            .sessions
            .get(session)
            .unwrap()
            .prediction_generation;
        assert!(dispatcher
            .prediction_cache
            .attempted_for(session, generation));

        prediction.test_set_scripted_prediction_available(true);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(prediction.request_count(), automatic_requests + 1);
        assert!(out.consumed);
        assert!(!out.beep);
        assert_eq!(out.selected_candidate(), Some(0));
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Predicting
        );
        let selected_surface = out
            .candidate(0)
            .expect("successful retry candidate")
            .0
            .to_owned();
        assert_eq!(out.preedit_text(), selected_surface);
        assert!(dispatcher.prediction_cache.explicit_retry_attempted);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(prediction.request_count(), automatic_requests + 1);
        assert_eq!(out.selected_candidate(), Some(0));
        assert_eq!(out.preedit_text(), selected_surface);
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn prediction_live_cache_authority_loss_returns_to_raw_composing() {
        let learning = Arc::new(LearningService::memory());
        let (mut dispatcher, runtime) =
            phase_one_prediction_dispatcher(phase_one_prediction_conversion(), learning);
        let prediction = runtime.service();
        prediction.test_script_prediction("\u{304b}", "\u{58f2}\u{4e0a}");
        prediction.test_set_scripted_prediction_available(true);
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "authority-loss.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let focused_surface = out.preedit_text().to_owned();
        let raw_session = dispatcher
            .sessions
            .get(session)
            .unwrap()
            .preedit
            .as_str()
            .to_owned();
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Predicting
        );

        prediction.test_set_scripted_prediction_available(false);
        dispatcher.prediction_cache.clear();
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
            },
            &mut out,
        );
        let after = dispatcher.sessions.get(session).unwrap();
        assert_eq!(after.state(), State::Composing);
        assert!(!after.suggestion_focused);
        assert!(!after.suggestions_visible);
        assert!(!out.beep, "Composing must not run Predicting-only deletion");
        assert_eq!(after.preedit.as_str(), raw_session);
        assert_eq!(out.preedit_text(), raw_preedit(&mut dispatcher, session));
        assert_ne!(out.preedit_text(), focused_surface);

        // Enter is the strongest commit boundary: after stale focus is
        // cleared, it must follow the ordinary Composing commit path rather
        // than trying to commit a missing prediction candidate.
        prediction.test_set_scripted_prediction_available(true);
        let enter_session = create_session(&mut dispatcher, &mut out, "authority-enter.exe");
        type_word(&mut dispatcher, enter_session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session: enter_session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let enter_raw = dispatcher
            .sessions
            .get(enter_session)
            .unwrap()
            .preedit
            .as_str()
            .to_owned();
        prediction.test_set_scripted_prediction_available(false);
        dispatcher.prediction_cache.clear();
        dispatcher.dispatch(
            &Request::SendKey {
                session: enter_session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some(enter_raw.as_str()));
        assert_eq!(
            dispatcher.sessions.get(enter_session).unwrap().state(),
            State::Idle
        );
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn prediction_focus_is_cleared_when_typing_after_projection() {
        let learning = Arc::new(LearningService::memory());
        let (mut dispatcher, runtime) =
            phase_one_prediction_dispatcher(phase_one_prediction_conversion(), learning);
        let prediction = runtime.service();
        prediction.test_script_prediction("\u{304b}", "\u{58f2}\u{4e0a}");
        prediction.test_set_scripted_prediction_available(true);
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "typing-invalidation.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let focused_surface = out.preedit_text().to_owned();
        let generation_before = dispatcher
            .sessions
            .get(session)
            .unwrap()
            .prediction_generation;

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('x'),
            },
            &mut out,
        );
        let rendered_raw = raw_preedit(&mut dispatcher, session);
        let after = dispatcher.sessions.get(session).unwrap();
        assert_ne!(after.prediction_generation, generation_before);
        assert_eq!(after.state(), State::Composing);
        assert!(!after.suggestion_focused);
        assert_eq!(out.preedit_text(), rendered_raw);
        assert_ne!(out.preedit_text(), focused_surface);
        assert!(
            after.suggestions_visible,
            "fresh automatic result remains visible"
        );
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn prediction_focus_is_cleared_when_backspace_after_projection() {
        let learning = Arc::new(LearningService::memory());
        let (mut dispatcher, runtime) =
            phase_one_prediction_dispatcher(phase_one_prediction_conversion(), learning);
        let prediction = runtime.service();
        prediction.test_script_prediction("\u{304b}", "\u{58f2}\u{4e0a}");
        prediction.test_set_scripted_prediction_available(true);
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "backspace-invalidation.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        let focused_surface = out.preedit_text().to_owned();
        let generation_before = dispatcher
            .sessions
            .get(session)
            .unwrap()
            .prediction_generation;

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        let rendered_raw = raw_preedit(&mut dispatcher, session);
        let after = dispatcher.sessions.get(session).unwrap();
        assert_ne!(after.prediction_generation, generation_before);
        assert_eq!(after.state(), State::Composing);
        assert!(!after.suggestion_focused);
        assert_eq!(out.preedit_text(), rendered_raw);
        assert_ne!(out.preedit_text(), focused_surface);
        assert!(
            after.suggestions_visible,
            "fresh automatic result remains visible"
        );
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn named_shift_enter_commits_top_suggestion_and_shift_space_enters_conversion() {
        let (mut dispatcher, runtime) = prediction_dispatcher();
        let mut out = OutputBuf::new();
        let first = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, first, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session: first,
                key: modified_named_key(KeyCode::Enter, Modifiers::SHIFT),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("仮名"));

        let second = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, second, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session: second,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session: second,
                key: modified_named_key(KeyCode::Space, Modifiers::SHIFT),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher.sessions.get(second).unwrap().state(),
            State::Converting
        );
        assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn typing_konnnichiha_produces_the_hiragana_preedit() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "konnnichiha", &mut out);

        assert_eq!(out.preedit_text(), "こんにちは");
    }

    #[test]
    fn enter_commits_the_composition_and_leaves_the_preedit_empty() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "konnnichiha", &mut out);

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Output);
        assert!(out.consumed);
        assert_eq!(out.commit_text(), Some("こんにちは"));
        assert_eq!(out.preedit_text(), "");
    }

    #[test]
    fn decimal_period_stays_ascii_after_a_half_width_digit() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "1.", &mut out);

        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.preedit.as_str(), "1.");
        assert_eq!(live.raw_input.as_str(), "1.");
        assert_eq!(out.preedit_text(), "1.");
        assert_eq!(raw_preedit(&mut dispatcher, session), "1.");

        type_word(&mut dispatcher, session, "23", &mut out);
        assert_eq!(out.preedit_text(), "1.23");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("1.23"));

        // Provenance replay remains correct with a literal decimal `.`:
        // deleting digits and typing another one must retain the same
        // raw/preedit alignment instead of inserting at the old `。` byte
        // boundary.
        let editing = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, editing, "1.23", &mut out);
        for expected in ["1.2", "1."] {
            dispatcher.dispatch(
                &Request::SendKey {
                    session: editing,
                    key: named_key(KeyCode::Backspace),
                },
                &mut out,
            );
            assert_eq!(out.preedit_text(), expected);
            assert_eq!(
                dispatcher
                    .sessions
                    .get(editing)
                    .expect("editing session")
                    .raw_input
                    .as_str(),
                expected
            );
        }
        dispatcher.dispatch(
            &Request::SendKey {
                session: editing,
                key: char_key('4'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "1.4");
        assert_eq!(
            dispatcher
                .sessions
                .get(editing)
                .expect("editing session")
                .raw_input
                .as_str(),
            "1.4"
        );

        // The decision follows the character immediately before the caret,
        // not just the last character of the composition.
        let inserted = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, inserted, "12", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session: inserted,
                key: named_key(KeyCode::Left),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session: inserted,
                key: char_key('.'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "1.2");
        assert_eq!(
            dispatcher
                .sessions
                .get(inserted)
                .expect("inserted session")
                .raw_input
                .as_str(),
            "1.2"
        );

        let ordinary = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, ordinary, "a.", &mut out);
        assert_eq!(
            out.preedit_text(),
            "あ。",
            "a period stays Japanese punctuation unless its previous input is a digit"
        );
    }

    #[test]
    fn escape_cancels_the_composition_without_committing() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "sakura", &mut out);
        assert!(!out.preedit_text().is_empty());

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Escape),
            },
            &mut out,
        );

        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), "");
    }

    #[test]
    fn backspace_removes_pending_romaji_before_emitted_kana() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        // "ka" resolves fully to "か" (nothing pending); the trailing "k"
        // then waits on a vowel.
        type_word(&mut dispatcher, session, "kak", &mut out);
        assert_eq!(out.preedit_text(), "かk");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "か",
            "first backspace removes the pending romaji"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "",
            "second backspace removes the emitted kana"
        );
    }

    /// #16 finding B: `apply_backspace` used to pop exactly one character of
    /// `raw_input` no matter how many keystrokes the deleted kana actually
    /// took, leaving stale keystrokes behind until the next `reset()`. Most
    /// kana take more than one ASCII character (`ka`, `ki`, ...), so this is
    /// the common case, not an edge case.
    #[test]
    fn backspace_after_a_multi_character_kana_removes_every_keystroke_behind_it() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "ka", &mut out);
        assert_eq!(out.preedit_text(), "か");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "", "the whole kana か is undone");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "",
            "both keystrokes that produced か are undone with it, not just the last one"
        );

        type_word(&mut dispatcher, session, "kaki", &mut out);
        assert_eq!(out.preedit_text(), "かき");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "か", "only the trailing き is removed");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "ka",
            "raw_input keeps exactly the keystrokes behind the remaining preedit"
        );
    }

    /// Companion to the multi-character case above: while romaji is still
    /// pending (has not resolved into a kana yet), Backspace must keep
    /// removing one raw keystroke at a time, exactly as before -- there is no
    /// kana to group by yet.
    #[test]
    fn backspace_over_pending_romaji_still_removes_one_keystroke_at_a_time() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "kak", &mut out);
        assert_eq!(out.preedit_text(), "かk");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "か");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "ka",
            "the pending k is undone alone, not the resolved か behind it"
        );
    }

    /// Characterizes (issue #16 audit, prerequisite to a B/C fix) what
    /// currently happens when the caret moves while romaji is still
    /// pending. Every caret motion -- Left, Right, Home, End -- shares one
    /// `move_caret` implementation that calls `flush_pending`
    /// unconditionally before evaluating the new cursor position. So "type
    /// pending romaji, then move the caret" is not a reachable state: the
    /// pending romaji is always resolved first (emitted as kana, or passed
    /// through raw if it cannot resolve to anything, exactly as an explicit
    /// commit would) at the *old* cursor position, and only then does the
    /// cursor move. The only reachable order is "move the caret, then type
    /// pending romaji" -- which is what the residual-gap tests around this
    /// one exercise. Any `raw_input`/caret redesign can therefore ignore
    /// "pending survives a caret move" as a case: it cannot occur.
    #[test]
    fn characterize_caret_movement_while_romaji_is_pending() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "sakura", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );
        type_word(&mut dispatcher, session, "k", &mut out);
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.romaji.pending(),
            "k",
            "k is pending before the caret moves"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Right),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "kさくら",
            "the pending k is flushed as a literal raw passthrough character \
             at the old cursor position before the caret moves"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert!(
            live.romaji.is_empty(),
            "flush_pending inside move_caret already resolved the pending k \
             -- nothing is pending anymore once the caret has moved"
        );
        assert_eq!(
            live.raw_input.as_str(),
            "ksakura",
            "flush_pending never touches raw_input, only preedit/cursor/romaji"
        );
    }

    /// #16 finding B/C (residual, found auditing 065200b): the pending-romaji
    /// branch of `apply_backspace` assumes whatever is pending sits at the
    /// *end* of `raw_input` and pops `raw_input`'s last character
    /// unconditionally. That assumption only holds if the pending keystroke
    /// was typed with the caret at the true end of the composition. Once the
    /// caret has moved (e.g. Home) before the pending keystroke lands,
    /// `feed_character` correctly inserts that keystroke's raw character at
    /// the caret's raw offset -- not at the end -- so the character
    /// `pop_char()` removes is some unrelated, already-resolved keystroke
    /// instead of the pending one.
    ///
    /// This is invisible from `out.preedit_text()` alone: `session.preedit`
    /// (the resolved kana) is untouched by either the correct or the buggy
    /// path, so the on-screen text looks identical either way. Only
    /// `raw_input` itself diverges, which is why this assertion has to read
    /// `live.raw_input.as_str()` directly rather than trust the rendered
    /// text.
    #[test]
    fn backspace_after_inserting_pending_romaji_at_a_moved_caret_preserves_raw_input_alignment() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "sakura", &mut out);
        assert_eq!(out.preedit_text(), "さくら");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );

        type_word(&mut dispatcher, session, "k", &mut out);
        assert_eq!(
            out.preedit_text(),
            "kさくら",
            "the pending k renders at the caret, ahead of the untouched さくら"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.raw_input.as_str(), "ksakura");
        assert!(
            !live.romaji.is_empty(),
            "k must still be pending, not resolved"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "さくら",
            "only the pending k is undone; さくら is untouched on screen"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "sakura",
            "raw_input must lose exactly the pending k, not sakura's trailing a"
        );
        assert!(
            live.romaji.is_empty(),
            "the pending k must be fully consumed by the backspace, not left dangling"
        );
    }

    /// #16 finding C: `feed_character` used to always *append* to
    /// `raw_input` while inserting the resolved kana at the caret, so moving
    /// the caret and typing produced a `raw_input` whose keystroke order no
    /// longer matched what was on screen. This is the bug report's own
    /// example: type "sakura", go Home, type "o" -- the preedit becomes
    /// "おさくら" and `raw_input` must read "osakura", not "sakurao".
    #[test]
    fn typing_after_a_caret_move_inserts_the_keystroke_in_visual_order() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "sakura", &mut out);
        assert_eq!(out.preedit_text(), "さくら");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );
        type_word(&mut dispatcher, session, "o", &mut out);
        assert_eq!(out.preedit_text(), "おさくら");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "osakura",
            "the new keystroke lands where the caret was, not at the end"
        );
    }

    /// #16 finding C (forward-delete half): `apply_delete_forward` never
    /// touched `raw_input` at all, so it went stale on every forward-delete.
    #[test]
    fn delete_forward_removes_every_keystroke_behind_the_deleted_kana() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "kaki", &mut out);
        assert_eq!(out.preedit_text(), "かき");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Delete),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "き",
            "the leading か is deleted forward"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "ki",
            "both keystrokes behind the deleted か are gone, not just one"
        );
    }

    /// #16 finding B/C (residual, found auditing 065200b): `apply_delete_forward`
    /// has no pending-romaji branch at all -- unlike `apply_backspace`, it
    /// unconditionally replays `raw_input` from scratch through
    /// `raw_chars_for_emitted` regardless of whether a keystroke is still
    /// pending. `raw_chars_for_emitted`'s own soundness argument only covers
    /// offsets `move_caret` produces (which always flushes pending first,
    /// see its doc comment); an offset taken while pending is non-empty and
    /// the caret sits ahead of already-resolved text is outside that
    /// contract, and the replay mis-segments the buffer as a result.
    ///
    /// Same setup as the Backspace companion above: type "sakura", Home,
    /// type "k" (stays pending, inserted at the caret's raw offset by
    /// finding C's fix, landing ahead of "sakura" as "ksakura"). A
    /// delete-forward at that point should remove only the resolved kana
    /// directly behind the pending k -- さ, produced by raw "sa" -- leaving
    /// the pending k and the untouched "kura" behind くら:
    /// `raw_input == "kkura"`. The rendered preedit ("kくら") looks correct
    /// either way, which is exactly the false-GREEN risk this test guards
    /// against by asserting `raw_input` and the pending romaji directly.
    #[test]
    fn delete_after_inserting_pending_romaji_at_a_moved_caret_preserves_raw_input_alignment() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "sakura", &mut out);
        assert_eq!(out.preedit_text(), "さくら");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );

        type_word(&mut dispatcher, session, "k", &mut out);
        assert_eq!(
            out.preedit_text(),
            "kさくら",
            "the pending k renders at the caret, ahead of the untouched さくら"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "ksakura",
            "setup must actually land k ahead of sakura via the moved-caret \
             insertion path, or this is not exercising finding C at all"
        );
        assert_eq!(
            live.romaji.pending(),
            "k",
            "k must still be pending, not resolved, going into the delete"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Delete),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "kくら",
            "delete-forward removes the first resolved kana (さ) behind the pending k"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "kkura",
            "raw_input must lose exactly \"sa\" (the keystrokes behind さ), \
             keeping the pending k and the untouched \"kura\" behind くら"
        );
        assert_eq!(
            live.romaji.pending(),
            "k",
            "delete-forward must not consume the pending k -- it only removed \
             an already-resolved kana ahead of it"
        );
    }

    /// #16 finding B/C, carry provenance: `raw_input` holds the raw
    /// provenance of the *currently visible* preedit, not an unwound
    /// keystroke log. A sokuon/carry (the second "t" of "tt" resolves
    /// "tt" to "っ" *and* is carried forward as the new pending) makes the
    /// same raw byte simultaneously っ's source and the carried pending's
    /// source. Clearing that pending on Backspace must not delete the
    /// shared byte -- doing so would corrupt っ's own provenance, which is
    /// still visible on screen and untouched by this Backspace. Backspace
    /// only removes a raw byte when pending owns one exclusively (its
    /// source range extends strictly past the already-resolved boundary);
    /// a fully carried/shared pending has no byte of its own to remove.
    #[test]
    fn backspace_over_carried_pending_romaji_at_a_moved_caret_preserves_raw_provenance() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "sakura", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );

        type_word(&mut dispatcher, session, "tt", &mut out);
        assert_eq!(
            out.preedit_text(),
            "っtさくら",
            "っ must be emitted and the carried t must render as pending, \
             ahead of the untouched さくら"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "ttsakura",
            "setup must actually produce a carry ahead of a moved caret, \
             or this is not exercising carry provenance at all"
        );
        assert_eq!(live.romaji.pending(), "t");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "っさくら",
            "backspace clears the carried pending t; っ, which shares its \
             raw source with that pending, must remain untouched"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.romaji.pending(), "", "the pending unit is gone");
        assert_eq!(
            live.raw_input.as_str(),
            "ttsakura",
            "raw_input must stay whole -- the only raw byte the carried \
             pending could claim is the same byte っ already depends on, so \
             backspacing pending must not delete any raw byte here"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F10),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "ttsakura",
            "F10's half-width alnum surface, a real downstream consumer of \
             raw_input, must reproduce the exact keystrokes behind っさくら, \
             not a corrupted leftover from a wrongly-deleted raw byte"
        );
    }

    /// #16 finding B/C, carry provenance: the Delete-forward analogue of
    /// the Backspace test above. Unlike Backspace, deleting the *next
    /// resolved kana* (さ) ahead of the carried pending must remove that
    /// kana's own raw source ("sa") in full -- さ does not share its
    /// source with the carry the way っ does, so nothing here should be
    /// preserved. The carried pending t itself must survive untouched.
    #[test]
    fn delete_forward_over_carried_pending_romaji_at_a_moved_caret_preserves_raw_provenance() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "sakura", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );

        type_word(&mut dispatcher, session, "tt", &mut out);
        assert_eq!(out.preedit_text(), "っtさくら");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "ttsakura",
            "setup must actually produce a carry ahead of a moved caret, \
             or this is not exercising carry provenance at all"
        );
        assert_eq!(live.romaji.pending(), "t");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Delete),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "っtくら",
            "delete-forward removes the next resolved kana (さ) behind the \
             carried pending t, leaving っ, the pending t and くら"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "ttkura",
            "raw_input must lose exactly \"sa\" -- さ's own raw source -- \
             not \"a\" alone. The carried t's source (the second \"t\") is \
             also っ's source and must neither be double-deleted nor left \
             stranded"
        );
        assert_eq!(
            live.romaji.pending(),
            "t",
            "delete-forward must not consume the carried pending t"
        );
    }

    /// #16 finding B/C, carry provenance: the `feed_character` (typing)
    /// analogue of the two tests above. Each new keystroke is spliced into
    /// `raw_input` at `next_raw_boundary`, the same boundary
    /// Backspace/Delete-forward read. Before that boundary was carry-aware
    /// (when it was merely `pending_raw_range(...).end`, clamped but not
    /// overlap-aware), it landed one byte too late -- right after っ's
    /// *shared* raw source -- splicing the new keystroke into the middle of
    /// さくら's own "sa" instead of ahead of it, where the caret actually
    /// is. "s" cannot tell the two candidate offsets apart --
    /// inserting it one byte earlier or later produces the same string by
    /// coincidence, since "sakura" already starts with "s" -- so this uses
    /// "y" (extends the carried "t" toward "tya"/"tyu"/"tyo", and never
    /// appears in "sakura") to make the two offsets produce visibly
    /// different strings.
    #[test]
    fn typing_after_carried_pending_romaji_at_a_moved_caret_preserves_raw_provenance() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "sakura", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );

        type_word(&mut dispatcher, session, "tt", &mut out);
        assert_eq!(out.preedit_text(), "っtさくら");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "ttsakura",
            "setup must actually produce a carry ahead of a moved caret, \
             or this is not exercising carry provenance at all"
        );
        assert_eq!(live.romaji.pending(), "t");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('y'),
            },
            &mut out,
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.romaji.pending(),
            "ty",
            "y extends the carried t (toward tya/tyu/tyo) rather than \
             resolving, so it must still be waiting as pending, not emitted"
        );
        assert_eq!(
            live.raw_input.as_str(),
            "ttysakura",
            "the new y must land right after \"tt\" and before \"sakura\" \
             -- at the boundary the carried t shares with っ, not one byte \
             later, inside さくら's own \"sa\""
        );
        assert_eq!(
            out.preedit_text(),
            "っtyさくら",
            "nothing resolved yet, so preedit keeps rendering っ plus the \
             now-two-character pending ty ahead of the untouched さくら"
        );
    }

    /// #16 finding B/C, carry provenance: no moved caret at all -- typing
    /// "tt" (a sokuon that carries the second "t" forward as pending),
    /// Backspace (clearing that pending without deleting the raw byte it
    /// shares with っ, per the Backspace test above), then continuing to
    /// type "sakura" left-to-right must not corrupt `raw_input`. This is not
    /// about caret movement or Delete-forward's own raw span: it is about
    /// `next_raw_boundary` itself, read by every `feed_character` insertion,
    /// depending on [`raw_byte_offset_for_preedit_cursor`]'s replay of
    /// `raw_input` from a fresh romaji FSM. That replay has no way to see
    /// that the live FSM's pending was actually cleared by the Backspace
    /// above -- `raw_input` alone does not record it -- so a naive replay
    /// re-extends the discarded carry "t" into the next raw bytes ("sa"),
    /// hits no matching entry, falls back to a literal "t" passthrough
    /// (correct, tested FSM behavior in isolation -- see
    /// `every_carrying_entry_resolves_deterministically_when_flushed_alone`
    /// in romaji.rs), and that spurious literal throws off every insertion
    /// point after it. Each subsequent character then lands one raw byte
    /// too early, scrambling the tail into "raraku"-style corruption instead
    /// of "sakura".
    #[test]
    fn continued_typing_after_backspacing_a_carry_does_not_corrupt_raw_input() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "tt", &mut out);
        assert_eq!(out.preedit_text(), "っt");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.raw_input.as_str(), "tt");
        assert_eq!(live.romaji.pending(), "t");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "っ");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.romaji.pending(), "", "the carried pending is gone");
        assert_eq!(
            live.raw_input.as_str(),
            "tt",
            "backspacing the fully-carried pending must not delete っ's own \
             raw source"
        );

        type_word(&mut dispatcher, session, "sakura", &mut out);
        assert_eq!(
            out.preedit_text(),
            "っさくら",
            "さくら must resolve normally -- the discarded carry must not \
             leak a literal t into freshly-typed, unrelated kana"
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "ttsakura",
            "each new keystroke must land at the true end of raw_input, in \
             the order it was typed -- not reordered by a replay that \
             thinks the discarded carry is still live"
        );
        assert_eq!(live.romaji.pending(), "");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F10),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "ttsakura",
            "F10's half-width alnum surface, a real downstream consumer of \
             raw_input, must reproduce the exact keystrokes behind っさくら"
        );
    }

    /// #16 finding B/C, downstream check: the two regression tests above
    /// assert `raw_input` directly, which is exactly the kind of assertion
    /// a fix could satisfy while still leaving `raw_input` wrong for every
    /// *other* reader -- the false-GREEN risk `out.preedit_text()` alone
    /// already demonstrated twice over in this file. F10's half-width alnum
    /// surface is a real, independent consumer of `raw_input` (via
    /// `segment_raw_text`, degenerating to the whole buffer for this
    /// single-segment case): if the Backspace fix left `raw_input` merely
    /// looking right to a direct field comparison, this would still catch
    /// it.
    #[test]
    fn raw_input_alignment_after_moved_caret_backspace_is_observed_by_f10() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "sakura", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );
        type_word(&mut dispatcher, session, "k", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            live.raw_input.as_str(),
            "sakura",
            "setup must actually recover a clean raw_input via the Backspace fix, \
             or this is not exercising the fix at all"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F10),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "sakura",
            "F10's half-width alnum surface must come from the corrected raw_input \
             (\"sakura\"), not a corrupted leftover like the old pop_char() bug's \
             \"ksakur\""
        );
    }

    /// #16 finding D: F6-F10 transforms rendered and committed a segment's
    /// `SegmentTransform::FullAlnum`/`HalfAlnum` surface from the *whole*
    /// composition's `raw_input`, not that segment's own share of it. Applied
    /// to one segment of a multi-segment conversion, that leaked every other
    /// segment's keystrokes into it.
    #[test]
    fn f10_on_one_segment_of_a_multi_segment_conversion_uses_only_its_own_keystrokes() {
        let mut dispatcher = segmented_conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "kyoudesu", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        let initial = out.to_output().preedit.expect("converted preedit");
        assert_eq!(initial.segments.len(), 2);
        assert_eq!(initial.segments[0].text, "今日");
        assert_eq!(initial.segments[1].text, "です");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Right),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F10),
            },
            &mut out,
        );
        let transformed = out.to_output().preedit.expect("transformed preedit");
        assert_eq!(
            transformed.segments[0].text, "今日",
            "the unfocused first segment is untouched by a transform on the second"
        );
        assert_eq!(
            transformed.segments[1].text, "desu",
            "the second segment renders only its own keystrokes (\"desu\"), \
             not the whole composition's (\"kyoudesu\")"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(
            out.commit_text(),
            Some("今日desu"),
            "committing mirrors what was rendered: only the second segment's \
             own keystrokes are used for its transformed surface"
        );
    }

    /// The DLL reports Ctrl+S as the character `s` with the Ctrl bit set,
    /// so that a key map can bind chords by the letter the user sees on the
    /// key. Anything the key map does not claim has to reach the
    /// application untouched — an IME that turns Ctrl+S into "s" has eaten
    /// a save, and mid-composition is exactly when that hurts most.
    #[test]
    fn an_unbound_shortcut_reaches_the_application_and_leaves_the_composition_alone() {
        for held in [
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers(Modifiers::CTRL.0 | Modifiers::SHIFT.0),
            Modifiers(Modifiers::ALT.0 | Modifiers::SHIFT.0),
        ] {
            let mut dispatcher = builtin_dispatcher();
            let mut out = OutputBuf::new();
            let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
            type_word(&mut dispatcher, session, "sakura", &mut out);
            let before = out.preedit_text().to_owned();

            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: KeyInput {
                        modifiers: held,
                        ..char_key(if held.shift() { 'S' } else { 's' })
                    },
                },
                &mut out,
            );

            assert!(!out.consumed, "{held:?}+S was swallowed");
            assert_eq!(out.commit_text(), None);
            assert_eq!(
                out.preedit_text(),
                before,
                "{held:?}+S changed the composition"
            );
        }
    }

    // Issue #16 finding E: every named key exercised below used to fall
    // through `apply_key`'s final arm and reach the host application while
    // the IME owned a composition, conversion or focused suggestion list.
    // The keymap now binds each one to a real action or `Action::Swallow`;
    // `sakura_core::keymap`'s `ms_ime_*_is_bound_to_*` tests pin down
    // exactly which action each key resolves to. The tests below pin down
    // the resulting *behaviour* instead: the key must be consumed under
    // both a `test_only` probe and a real dispatch, a probe must never
    // mutate the live session, and a `Swallow` binding must have no
    // modelled effect at all. Every case gets its own fresh dispatcher and
    // session -- probe and real dispatch never share one either -- so a
    // case that unexpectedly changes state cannot contaminate the next
    // case, and neither dispatch can hide a mutation the other would have
    // caught.

    fn composing_state_dispatcher(word: &str, name: &str) -> (Dispatcher, SessionId) {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, name);
        type_word(&mut dispatcher, session, word, &mut out);
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Composing,
            "setup must reach State::Composing"
        );
        (dispatcher, session)
    }

    fn converting_state_dispatcher(word: &str, name: &str) -> (Dispatcher, SessionId) {
        let mut dispatcher = contextual_conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, name);
        type_word(&mut dispatcher, session, word, &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Converting,
            "setup must reach State::Converting"
        );
        (dispatcher, session)
    }

    fn predicting_state_dispatcher(
        word: &str,
        name: &str,
    ) -> (Dispatcher, crate::prediction::PredictionRuntime, SessionId) {
        let (mut dispatcher, runtime) = prediction_dispatcher();
        // The state-machine assertion must not depend on the prediction worker
        // winning a 10 ms production timeout while other Cargo test binaries
        // are running.  The runtime exposes a test-only scripted response so
        // this helper exercises the same action path with a deterministic
        // terminal result; worker scheduling remains covered by prediction.rs.
        let prediction_service = runtime.service();
        prediction_service.test_script_prediction(word, "test-prediction");
        prediction_service.test_set_scripted_prediction_available(true);
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, name);
        type_word(&mut dispatcher, session, word, &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Predicting,
            "setup must reach State::Predicting"
        );
        (dispatcher, runtime, session)
    }

    #[test]
    fn ms_ime_composing_named_keys_do_not_leak_to_the_host_application() {
        // Both cases bind to `Action::Swallow` (issue #16 finding E), so
        // both additionally must leave the session completely unchanged.
        let cases: [(KeyCode, Modifiers); 2] = [
            (KeyCode::PageUp, Modifiers::NONE),
            (KeyCode::PageDown, Modifiers::NONE),
        ];
        let mut failures = Vec::new();
        for (code, modifiers) in cases {
            let (mut probe_dispatcher, probe_session) =
                composing_state_dispatcher("ka", "leak-composing-probe.exe");
            let before_probe = probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("session")
                .clone();
            let mut probe_out = OutputBuf::new();
            probe_dispatcher.dispatch(
                &Request::SendKey {
                    session: probe_session,
                    key: KeyInput {
                        test_only: true,
                        ..modified_named_key(code, modifiers)
                    },
                },
                &mut probe_out,
            );
            if !probe_out.consumed {
                failures.push(format!("composing {code:?} test_only leaked to the host"));
            }
            if probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("session")
                != &before_probe
            {
                failures.push(format!("composing {code:?} probe mutated the session"));
            }
            if probe_out.commit_text().is_some() {
                failures.push(format!("composing {code:?} probe produced a commit"));
            }

            let (mut real_dispatcher, real_session) =
                composing_state_dispatcher("ka", "leak-composing-real.exe");
            let before_real = real_dispatcher
                .sessions
                .get(real_session)
                .expect("session")
                .clone();
            let mut real_out = OutputBuf::new();
            real_dispatcher.dispatch(
                &Request::SendKey {
                    session: real_session,
                    key: modified_named_key(code, modifiers),
                },
                &mut real_out,
            );
            if !real_out.consumed {
                failures.push(format!(
                    "composing {code:?} real dispatch leaked to the host"
                ));
            }
            if real_out.commit_text().is_some() {
                failures.push(format!(
                    "composing {code:?} real dispatch produced a commit"
                ));
            }
            if real_dispatcher.sessions.get(real_session).expect("session") != &before_real {
                failures.push(format!(
                    "composing {code:?} swallow mutated the session instead of doing nothing"
                ));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn ms_ime_converting_named_keys_do_not_leak_to_the_host_application() {
        // `swallow` marks the three cases bound to `Action::Swallow`
        // (delete, shift+tab, ctrl+delete): those must additionally leave
        // the session untouched. `home`/`end` bind to `SegmentHome`/
        // `SegmentEnd`, which legitimately move `focused_segment` -- real
        // movement across 3+ segments is proven separately, by the
        // dedicated test below, not here.
        let cases: [(KeyCode, Modifiers, bool); 5] = [
            (KeyCode::Delete, Modifiers::NONE, true),
            (KeyCode::Home, Modifiers::NONE, false),
            (KeyCode::End, Modifiers::NONE, false),
            (KeyCode::Tab, Modifiers::SHIFT, true),
            (KeyCode::Delete, Modifiers::CTRL, true),
        ];
        let mut failures = Vec::new();
        for (code, modifiers, swallow) in cases {
            let (mut probe_dispatcher, probe_session) =
                converting_state_dispatcher("ishaniittaowari", "leak-converting-probe.exe");
            let before_probe = probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("session")
                .clone();
            let mut probe_out = OutputBuf::new();
            probe_dispatcher.dispatch(
                &Request::SendKey {
                    session: probe_session,
                    key: KeyInput {
                        test_only: true,
                        ..modified_named_key(code, modifiers)
                    },
                },
                &mut probe_out,
            );
            if !probe_out.consumed {
                failures.push(format!("converting {code:?} test_only leaked to the host"));
            }
            if probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("session")
                != &before_probe
            {
                failures.push(format!("converting {code:?} probe mutated the session"));
            }
            if probe_out.commit_text().is_some() {
                failures.push(format!("converting {code:?} probe produced a commit"));
            }

            let (mut real_dispatcher, real_session) =
                converting_state_dispatcher("ishaniittaowari", "leak-converting-real.exe");
            let before_real = real_dispatcher
                .sessions
                .get(real_session)
                .expect("session")
                .clone();
            let mut real_out = OutputBuf::new();
            real_dispatcher.dispatch(
                &Request::SendKey {
                    session: real_session,
                    key: modified_named_key(code, modifiers),
                },
                &mut real_out,
            );
            if !real_out.consumed {
                failures.push(format!(
                    "converting {code:?} real dispatch leaked to the host"
                ));
            }
            if real_out.commit_text().is_some() {
                failures.push(format!(
                    "converting {code:?} real dispatch produced a commit"
                ));
            }
            if swallow
                && real_dispatcher.sessions.get(real_session).expect("session") != &before_real
            {
                failures.push(format!(
                    "converting {code:?} swallow mutated the session instead of doing nothing"
                ));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn ms_ime_predicting_named_keys_do_not_leak_to_the_host_application() {
        // `commits` marks the one case bound to `Action::CommitFirst`
        // (shift+enter): unlike the other ten, a commit there is correct,
        // modelled behaviour -- Microsoft IME's own "confirm the top pick"
        // shortcut -- so it is excluded from the "must not commit"
        // assertion the other ten get.
        let cases: [(KeyCode, Modifiers, bool); 11] = [
            (KeyCode::Left, Modifiers::NONE, false),
            (KeyCode::Right, Modifiers::NONE, false),
            (KeyCode::Home, Modifiers::NONE, false),
            (KeyCode::End, Modifiers::NONE, false),
            (KeyCode::Delete, Modifiers::NONE, false),
            (KeyCode::Enter, Modifiers::SHIFT, true),
            (KeyCode::F6, Modifiers::NONE, false),
            (KeyCode::F7, Modifiers::NONE, false),
            (KeyCode::F8, Modifiers::NONE, false),
            (KeyCode::F9, Modifiers::NONE, false),
            (KeyCode::F10, Modifiers::NONE, false),
        ];
        let mut failures = Vec::new();
        for (code, modifiers, commits) in cases {
            let (mut probe_dispatcher, _probe_runtime, probe_session) =
                predicting_state_dispatcher("kana", "leak-predicting-probe.exe");
            let before_probe = probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("session")
                .clone();
            let mut probe_out = OutputBuf::new();
            probe_dispatcher.dispatch(
                &Request::SendKey {
                    session: probe_session,
                    key: KeyInput {
                        test_only: true,
                        ..modified_named_key(code, modifiers)
                    },
                },
                &mut probe_out,
            );
            if !probe_out.consumed {
                failures.push(format!("predicting {code:?} test_only leaked to the host"));
            }
            if probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("session")
                != &before_probe
            {
                failures.push(format!("predicting {code:?} probe mutated the session"));
            }
            if !commits && probe_out.commit_text().is_some() {
                failures.push(format!("predicting {code:?} probe produced a commit"));
            }

            let (mut real_dispatcher, _real_runtime, real_session) =
                predicting_state_dispatcher("kana", "leak-predicting-real.exe");
            let mut real_out = OutputBuf::new();
            real_dispatcher.dispatch(
                &Request::SendKey {
                    session: real_session,
                    key: modified_named_key(code, modifiers),
                },
                &mut real_out,
            );
            if !real_out.consumed {
                failures.push(format!(
                    "predicting {code:?} real dispatch leaked to the host"
                ));
            }
            if !commits && real_out.commit_text().is_some() {
                failures.push(format!(
                    "predicting {code:?} real dispatch produced an unexpected commit"
                ));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    // Issue #16 finding G-1: unlike MS-IME, ATOK restates `muhenkan` in
    // every state's key-map section instead of inheriting it from
    // `[global]` (see the comment above `[global]` in
    // `data/keymap-atok.toml`), and `[predicting]` used to be the one
    // state where that restatement was missing -- so muhenkan fell
    // through to the host application while a suggestion was focused
    // under the ATOK preset specifically. `sakura_core::keymap`'s
    // `atok_predicting_muhenkan_is_bound_to_mode_kana_cycle` pins down
    // the exact key-map binding; this test proves the ATOK preset
    // actually *reaches* that action end-to-end through the engine, and
    // that the resulting temporary transform behaves exactly like the
    // already-proven composing-state case (issue #16 finding E's
    // `ModeKanaCycle` coverage): it changes only the rendered surface,
    // commits nothing, and never leaks to the host. Expected values are
    // taken from an empirical dispatch of this exact sequence, not
    // assumed from reading the transform code.
    #[test]
    fn atok_predicting_muhenkan_applies_a_temporary_katakana_transform_without_leaking() {
        let source = "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t1\t100\t100\tpredict\tcommon\nかなた\t彼方\t0\t2\t200\t200\tpredict\tdirection\nかながわ\t神奈川\t0\t3\t300\t300\tpredict\tprefecture\n";
        let entries = dictc::parse_entries("prediction.tsv", source).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t4\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        let conversion = Arc::new(
            ConversionService::from_static_bytes(image).expect("prediction conversion fixture"),
        );
        let learning = Arc::new(LearningService::memory());
        let runtime = crate::prediction::PredictionRuntime::start(Arc::clone(&conversion))
            .expect("prediction runtime");
        let mut dispatcher = Dispatcher::new_with_runtime_configuration(
            conversion,
            learning,
            runtime.service(),
            Preferences {
                keymap_preset: Preset::Atok,
                ..Preferences::default()
            },
        )
        .expect("shipped defaults");

        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "atok-predicting-muhenkan.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Predicting,
            "setup must reach State::Predicting with a focused suggestion"
        );
        let mode_before = dispatcher.sessions.get(session).expect("session").mode;

        let mut out = OutputBuf::new();
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Muhenkan),
            },
            &mut out,
        );

        assert!(
            out.consumed,
            "issue #16 finding G-1: ATOK muhenkan leaked to the host from State::Predicting"
        );
        assert_eq!(
            out.commit_text(),
            None,
            "a temporary transform out of Predicting must not commit anything"
        );
        assert_eq!(
            out.preedit_text(),
            "カナ",
            "muhenkan's first cycle step must render the predicted reading as full-width katakana"
        );

        let session_ref = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            session_ref.mode, mode_before,
            "a temporary transform out of Predicting must not persist into the session's input mode"
        );
    }

    /// `Home`/`End` while converting jump straight to the first/last
    /// segment (issue #16 finding E). A 1-segment fixture would let a
    /// no-op implementation pass this test trivially, so this fixture is
    /// picked to yield 4 segments, and focus is moved off both edges
    /// before each assertion so neither could pass merely because focus
    /// never left the edge.
    #[test]
    fn ms_ime_converting_home_and_end_move_focus_across_three_or_more_segments() {
        let (mut dispatcher, session) =
            converting_state_dispatcher("ishaniittaowari", "segment-home-end.exe");
        let segment_count = dispatcher
            .sessions
            .get(session)
            .expect("session")
            .segment_count();
        assert!(
            segment_count >= 3,
            "fixture must produce at least 3 segments to prove real movement, got {segment_count}"
        );
        assert_eq!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .focused_segment(),
            0,
            "conversion must start with the first segment focused"
        );

        let mut out = OutputBuf::new();
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Right),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .focused_segment(),
            1,
            "segment_next must move focus off the first segment before End is tested"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::End),
            },
            &mut out,
        );
        assert!(out.consumed);
        assert_eq!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .focused_segment(),
            segment_count - 1,
            "End must move focus to the last segment"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );
        assert!(out.consumed);
        assert_eq!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .focused_segment(),
            0,
            "Home must move focus back to the first segment"
        );
    }

    /// Ctrl+Space is deliberately absent from every shipped preset (see the
    /// `ms-ime` preset's header comment): it is IntelliSense in every major
    /// IDE. This guards against a future fix for issue #16 finding E
    /// accidentally widening into claiming it too.
    #[test]
    fn ctrl_space_is_not_bound_and_reaches_the_application() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "code.exe");
        type_word(&mut dispatcher, session, "sakura", &mut out);
        let before = out.preedit_text().to_owned();

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Space, Modifiers::CTRL),
            },
            &mut out,
        );

        assert!(!out.consumed, "Ctrl+Space was swallowed");
        assert_eq!(out.commit_text(), None);
        assert_eq!(
            out.preedit_text(),
            before,
            "Ctrl+Space changed the composition"
        );
    }

    /// The issue #16 finding E keymap fix bound specific named keys in
    /// `[composing]`, `[converting]` and `[predicting]` -- it must not have
    /// widened into "always consume named keys". `[idle]` never binds
    /// PageUp, so it must still reach the host application when there is no
    /// composition to protect.
    #[test]
    fn an_unbound_named_key_reaches_the_application_while_idle() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Idle
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::PageUp),
            },
            &mut out,
        );

        assert!(!out.consumed, "PageUp was swallowed while idle");
        assert_eq!(out.commit_text(), None);
    }

    #[test]
    fn a_test_only_key_reports_what_the_real_dispatch_would_without_mutating_the_session() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        // "z" has no romaji reading and passes through raw, immediately and
        // deterministically, with no waiting -- ideal for proving a probe
        // did or did not leave a mark.
        let probe_reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: test_only_char_key('z'),
            },
            &mut out,
        );
        assert_eq!(probe_reply, Reply::Output);
        let probe_consumed = out.consumed;

        // If the probe above had mutated the session, this real "z" would
        // land after a leftover "z" and produce "zz" instead of "z".
        let real_reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('z'),
            },
            &mut out,
        );
        assert_eq!(real_reply, Reply::Output);
        assert_eq!(
            out.consumed, probe_consumed,
            "test_only must report the same consumed as the real dispatch"
        );
        assert_eq!(out.preedit_text(), "z");
    }

    #[test]
    fn test_only_stale_learning_cache_is_invalidated_ephemerally_for_probe_parity() {
        let (mut dispatcher, runtime) = prediction_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert!(
            dispatcher
                .prediction_cache
                .candidates(
                    session,
                    dispatcher
                        .sessions
                        .get(session)
                        .expect("session")
                        .prediction_generation,
                )
                .is_some(),
            "the test must start with a populated live cache"
        );

        let learning = Arc::clone(dispatcher.learning.as_ref().expect("learning service"));
        let live_cache = (*dispatcher.prediction_cache).clone();
        let session_state = dispatcher.sessions.get(session).expect("session").clone();
        let observed_generation = dispatcher.observed_learning_generation;
        learning.learn("かな", "generation bump", 0, 0);

        let history_prefix = session_state.preedit.as_str();
        let learning_generation = learning.generation();
        let mut learning_history = Vec::new();
        learning.visit_prediction_history(history_prefix, |reading, surface, right, score| {
            learning_history.push((reading.to_owned(), surface.to_owned(), right, score));
            true
        });

        let probe_reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: KeyInput {
                    test_only: true,
                    ..modified_named_key(KeyCode::Delete, Modifiers::CTRL)
                },
            },
            &mut out,
        );
        assert_eq!(probe_reply, Reply::Output);
        let probe_output = out.to_output();
        assert!(!probe_output.consumed);
        assert!(!probe_output.beep);
        assert_eq!(
            dispatcher.sessions.get(session).expect("session"),
            &session_state,
            "Probe must not mutate the live session while clearing stale focus in its view"
        );
        assert_eq!(learning.generation(), learning_generation);
        let mut after_probe_history = Vec::new();
        learning.visit_prediction_history(history_prefix, |reading, surface, right, score| {
            after_probe_history.push((reading.to_owned(), surface.to_owned(), right, score));
            true
        });
        assert_eq!(after_probe_history, learning_history);
        assert_eq!(
            *dispatcher.prediction_cache, live_cache,
            "Probe must not mutate or clear the live stale cache"
        );

        // Restore the same session/cache snapshot and ask the real dispatcher
        // the same key. Learning remains at the newer generation, so Apply's
        // normal invalidation is the equivalent initial-state transition.
        dispatcher
            .sessions
            .get_mut(session)
            .expect("session")
            .clone_from(&session_state);
        dispatcher.prediction_cache.as_mut().clone_from(&live_cache);
        dispatcher.observed_learning_generation = observed_generation;
        let apply_reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
            },
            &mut out,
        );
        assert_eq!(apply_reply, Reply::Output);
        let apply_output = out.to_output();
        assert_eq!(
            apply_output.consumed, probe_output.consumed,
            "Probe and Apply must agree on host key consumption"
        );
        assert!(!apply_output.consumed);
        assert!(!apply_output.beep);
        assert_eq!(apply_output.delete_before, probe_output.delete_before);
        assert_eq!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .preedit
                .as_str(),
            "かな",
            "cache refresh must not change the composition itself"
        );
        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn commit_undo_render_overflow_rejects_before_tsf_and_restores_post_commit_state() {
        let mut dispatcher = Dispatcher::with_parts(
            Table::builtin().expect("romaji table"),
            KeyMap::preset(Preset::MsIme).expect("key map"),
            Normalizer {
                width: WidthPolicy {
                    alnum: Width::FollowMode,
                    number: Width::FollowMode,
                    symbol: Width::FollowMode,
                },
                punctuation: PunctuationStyle::default(),
            },
        );
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        {
            let session_state = dispatcher.sessions.get_mut(session).expect("session");
            let reading = "a".repeat(MAX_PREEDIT_BYTES);
            session_state
                .preedit
                .push_str(&reading)
                .expect("bounded reading");
            session_state
                .raw_input
                .push_str(&reading)
                .expect("bounded raw input");
            session_state.record_current_commit("x", 42, 0, 1);
            session_state.reset();
            session_state.mode = Mode::FullAlnum;
        }

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));
        assert_eq!(out.delete_before(), "", "a local failure never reaches TSF");
        let session_state = dispatcher.sessions.get_mut(session).expect("session");
        assert!(!session_state.undo_pending());
        assert!(!session_state.is_composing());
        assert_eq!(session_state.carry_right_id(), 42);
        assert!(
            session_state.undo_commit().is_some(),
            "a definite pre-mutation rejection keeps the bounded undo retryable"
        );
        assert!(session_state.reject_undo_commit());
    }

    #[test]
    fn an_unknown_session_id_is_rejected() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session: 9999,
                key: char_key('a'),
            },
            &mut out,
        );

        assert_eq!(
            reply,
            Reply::Message(Response::Error(ErrorCode::UnknownSession))
        );
    }

    #[test]
    fn session_ids_are_never_reused_after_deletion() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let first = create_session(&mut dispatcher, &mut out, "a.exe");

        let deleted = dispatcher.dispatch(&Request::DeleteSession { session: first }, &mut out);
        assert_eq!(deleted, Reply::Message(Response::Ok));

        let second = create_session(&mut dispatcher, &mut out, "b.exe");
        assert_ne!(first, second);
        assert!(second > first);
    }

    #[test]
    fn the_session_table_reports_busy_once_full() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        for _ in 0..crate::session::MAX_SESSIONS {
            create_session(&mut dispatcher, &mut out, "app.exe");
        }

        let reply = dispatcher.dispatch(
            &Request::CreateSession {
                process_name: "one-too-many.exe".to_string(),
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::Busy)));
    }

    #[test]
    fn hello_with_the_previous_v13_version_is_rejected() {
        assert_eq!(
            PROTOCOL_VERSION, 14,
            "history deletion capability adds v14 wire data"
        );
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();

        let reply = dispatcher.dispatch(&Request::Hello { client_version: 13 }, &mut out);

        assert_eq!(
            reply,
            Reply::Message(Response::Error(ErrorCode::UnsupportedVersion))
        );
    }

    #[test]
    fn hello_with_v14_version_is_accepted() {
        assert_eq!(
            PROTOCOL_VERSION, 14,
            "history deletion capability adds v14 wire data"
        );
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();

        let reply = dispatcher.dispatch(
            &Request::Hello {
                client_version: PROTOCOL_VERSION,
            },
            &mut out,
        );

        assert_eq!(
            reply,
            Reply::Message(Response::Hello {
                server_version: PROTOCOL_VERSION,
                engine_version: ENGINE_VERSION
            })
        );
    }

    #[test]
    fn half_width_full_width_can_restore_hiragana_from_direct_mode() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        let off = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::HankakuZenkaku),
            },
            &mut out,
        );
        assert_eq!(off, Reply::Output);
        assert!(out.consumed);
        assert_eq!(out.to_output().mode, Some(Mode::Direct));
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Direct
        );

        let ordinary_key = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            &mut out,
        );
        assert_eq!(ordinary_key, Reply::Output);
        assert!(!out.consumed, "Direct mode must pass normal typing through");

        let on = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::HankakuZenkaku),
            },
            &mut out,
        );
        assert_eq!(on, Reply::Output);
        assert!(out.consumed);
        assert_eq!(out.to_output().mode, Some(Mode::Hiragana));
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Hiragana
        );
    }

    #[test]
    fn shifted_ascii_letters_build_an_english_composition_without_committing() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        for character in ['K', 'A'] {
            assert_eq!(
                dispatcher.dispatch(
                    &Request::SendKey {
                        session,
                        key: shifted_char_key(character),
                    },
                    &mut out,
                ),
                Reply::Output
            );
            assert!(out.consumed);
        }

        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), "KA");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.raw_input.as_str(), "KA");
        assert_eq!(live.mode(), Mode::Hiragana);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "K");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.raw_input.as_str(), "K");
        assert!(live.shifted_ascii);
    }

    #[test]
    fn shifted_first_ascii_letter_latches_english_composition_for_unshifted_ascii() {
        let mut dispatcher = shifted_ascii_english_conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        for (index, character) in "Claude".chars().enumerate() {
            let key = if index == 0 {
                shifted_char_key(character)
            } else {
                char_key(character)
            };
            assert_eq!(
                dispatcher.dispatch(&Request::SendKey { session, key }, &mut out),
                Reply::Output,
                "character {character:?} at index {index}"
            );
            assert!(out.consumed);
        }

        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), "Claude");
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.raw_input.as_str(), "Claude");
        assert!(live.shifted_ascii, "initial Shift must stay latched");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert!(out.consumed);
        assert_eq!(out.preedit_text(), "Claude");
        assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
        let candidates = (0..CANDIDATE_PAGE_SIZE)
            .filter_map(|index| out.candidate(index).map(|(surface, _)| surface))
            .collect::<Vec<_>>();
        assert!(candidates.contains(&"Claude"));
        assert!(candidates.contains(&"Claude Code"));

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("Claude"));
        let live = dispatcher.sessions.get(session).expect("session");
        assert_eq!(live.mode(), Mode::Hiragana);
        assert!(!live.shifted_ascii, "commit must end the temporary mode");
        assert_eq!(live.state(), State::Idle);
    }

    #[test]
    fn shift_started_ascii_without_dictionary_hit_never_falls_back_to_kana() {
        let mut dispatcher = shifted_ascii_english_conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        for (index, character) in "Aiamu".chars().enumerate() {
            let key = if index == 0 {
                shifted_char_key(character)
            } else {
                char_key(character)
            };
            dispatcher.dispatch(&Request::SendKey { session, key }, &mut out);
        }
        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), "Aiamu");

        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Space),
                },
                &mut out,
            ),
            Reply::Output
        );
        assert!(out.consumed);
        assert!(out.beep);
        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), "Aiamu");
        assert_eq!(out.candidate_kind(), None);
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").state(),
            State::Composing
        );

        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Enter),
                },
                &mut out,
            ),
            Reply::Output
        );
        assert_eq!(out.commit_text(), Some("Aiamu"));
    }

    #[test]
    fn shifted_ascii_terms_use_dictionary_case_and_phrase_candidates() {
        let mut dispatcher = shifted_ascii_english_conversion_dispatcher();
        let cases: &[(&str, &str, &[&str])] = &[
            ("CLAUDE", "Claude", &["Claude", "Claude Code"]),
            ("OPENAI", "OpenAI", &["OpenAI"]),
            ("GITLAB", "GitLab", &["GitLab"]),
            ("PYTORCH", "PyTorch", &["PyTorch"]),
            ("MICROSOFTTEAMS", "Microsoft Teams", &["Microsoft Teams"]),
            ("SAKURAINPUT", "Sakura Input", &["Sakura Input"]),
        ];

        for &(typed, expected, expected_candidates) in cases {
            let mut out = OutputBuf::new();
            let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

            for character in typed.chars() {
                assert_eq!(
                    dispatcher.dispatch(
                        &Request::SendKey {
                            session,
                            key: shifted_char_key(character),
                        },
                        &mut out,
                    ),
                    Reply::Output,
                    "Shift+{character} in {typed}"
                );
            }
            assert_eq!(out.preedit_text(), typed);

            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Space),
                },
                &mut out,
            );
            assert!(out.consumed, "Space after Shift+{typed}");
            assert_eq!(out.preedit_text(), expected, "conversion for {typed}");
            assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
            let candidates = (0..CANDIDATE_PAGE_SIZE)
                .filter_map(|index| out.candidate(index).map(|(surface, _)| surface))
                .collect::<Vec<_>>();
            for expected_candidate in expected_candidates {
                assert!(
                    candidates.contains(expected_candidate),
                    "missing {expected_candidate} for {typed}: {candidates:?}"
                );
            }

            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Enter),
                },
                &mut out,
            );
            assert_eq!(out.commit_text(), Some(expected), "commit for {typed}");
            assert_eq!(
                dispatcher.sessions.get(session).expect("session").mode(),
                Mode::Hiragana,
                "Shift conversion must not persist a mode change"
            );
        }
    }

    #[test]
    fn microsoft_nonconvert_temporarily_transforms_composition_without_changing_mode() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "ka", &mut out);
        assert_eq!(out.preedit_text(), "\u{304b}");
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Hiragana
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Muhenkan),
            },
            &mut out,
        );
        assert_eq!(out.to_output().mode, None);
        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), "\u{30ab}"); // カ
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Hiragana
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("カ"));
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Hiragana
        );

        type_word(&mut dispatcher, session, "ka", &mut out);
        assert_eq!(out.preedit_text(), "\u{304b}");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Muhenkan),
            },
            &mut out,
        );
        assert_eq!(out.to_output().mode, None);
        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), "\u{30ab}"); // カ
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Hiragana
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Muhenkan),
            },
            &mut out,
        );
        assert_eq!(out.to_output().mode, None);
        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), "\u{ff76}"); // ｶ
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Hiragana
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("ｶ"));
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Hiragana
        );

        type_word(&mut dispatcher, session, "ka", &mut out);
        assert_eq!(out.preedit_text(), "\u{304b}"); // 次の入力はひらがな
    }

    #[test]
    fn a_temporary_kana_transform_commits_without_teaching_the_reading_its_katakana_form() {
        let learning = Arc::new(LearningService::memory());
        let mut dispatcher =
            Dispatcher::new_with_services(conversion_fixture(), Arc::clone(&learning))
                .expect("dispatcher");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        let generation_before = learning.generation();
        type_word(&mut dispatcher, session, "ka", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Muhenkan),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "\u{30ab}"); // カ
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );

        assert_eq!(
            out.commit_text(),
            Some("\u{30ab}"),
            "the temporary transform still commits what the user saw"
        );
        assert_eq!(
            learning.generation(),
            generation_before,
            "a mechanical kana transform must never bias the reading towards katakana"
        );
        let preference = learning.preference("\u{304b}", 0, [("\u{30ab}", 0)]);
        assert_eq!(
            (preference.exact, preference.general),
            (None, None),
            "the transformed surface must be absent from the learning store"
        );

        // An ordinary commit of the same reading still teaches the store, so the
        // gate is specific to transforms rather than a blanket suppression.
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(learning.generation(), generation_before + 1);
    }

    #[test]
    fn microsoft_nonconvert_cycles_persistent_mode_when_idle() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        for expected in [Mode::Katakana, Mode::HalfKatakana, Mode::Hiragana] {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Muhenkan),
                },
                &mut out,
            );
            assert_eq!(out.to_output().mode, Some(expected));
            assert_eq!(
                dispatcher.sessions.get(session).expect("session").mode(),
                expected
            );
            assert_eq!(out.preedit_text(), "");
        }
    }

    #[test]
    fn set_input_scope_password_forces_direct_mode_and_builds_no_preedit() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "login.exe");
        type_word(&mut dispatcher, session, "ka", &mut out);
        assert_eq!(
            out.preedit_text(),
            "か",
            "sanity check: normally composes before the scope change"
        );

        let reply = dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Password,
            },
            &mut out,
        );
        assert_eq!(reply, Reply::Message(Response::Ok));

        let key_reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            &mut out,
        );
        assert_eq!(key_reply, Reply::Output);
        assert!(!out.consumed, "Direct mode must not intercept keys");
        assert_eq!(out.preedit_text(), "");
        assert_eq!(out.commit_text(), None);

        let toggle_reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::HankakuZenkaku),
            },
            &mut out,
        );
        assert_eq!(toggle_reply, Reply::Output);
        assert!(!out.consumed, "password fields must not inspect mode keys");
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Direct
        );
    }

    #[test]
    fn half_alnum_types_ascii_straight_through_and_full_alnum_widens_it() {
        // The shipped MS-IME preset binds no key to a direct mode-switch
        // action, so reaching HalfAlnum/FullAlnum needs a key map that does;
        // and the default normalizer never widens anything in any mode (by
        // design -- see `sakura_core::width`'s docs), so reaching a widened
        // FullAlnum result needs a normalizer told to follow the mode.
        let table = Table::builtin().expect("builtin table compiles");
        let keymap =
            KeyMap::parse("[global]\nf2 = \"mode_half_alnum\"\nf3 = \"mode_full_alnum\"\n")
                .expect("small keymap compiles");
        let normalizer = Normalizer {
            width: WidthPolicy {
                alnum: Width::FollowMode,
                number: Width::FollowMode,
                symbol: Width::FollowMode,
            },
            punctuation: PunctuationStyle::default(),
        };
        let mut dispatcher = Dispatcher::with_parts(table, keymap, normalizer);
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "cmd.exe");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F3),
            },
            &mut out,
        );
        let mut widened = String::new();
        for c in "docker".chars() {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(c),
                },
                &mut out,
            );
            assert_eq!(out.preedit_text(), "", "alnum modes never build a preedit");
            widened.push_str(out.commit_text().unwrap_or_default());
        }
        assert_eq!(widened, "ｄｏｃｋｅｒ");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F2),
            },
            &mut out,
        );
        let mut plain = String::new();
        for c in "docker".chars() {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(c),
                },
                &mut out,
            );
            plain.push_str(out.commit_text().unwrap_or_default());
        }
        assert_eq!(plain, "docker");
    }

    #[test]
    fn an_oversized_preedit_answers_too_large_and_leaves_the_session_usable() {
        // A single entry whose output alone exceeds MAX_PREEDIT_BYTES, so
        // one keystroke overflows deterministically. This table maps
        // nothing but "a", so "usable afterward" is checked with a
        // character this same minimal table can actually resolve: "k" has
        // no entry, so it passes through raw immediately without reaching
        // the intentionally oversized "a" mapping again.
        let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
        let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
        let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
        let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "app.exe");

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));

        // The session must still be usable afterward, on the same table.
        type_word(&mut dispatcher, session, "k", &mut out);
        assert_eq!(out.preedit_text(), "k");
    }

    #[test]
    fn a_preexisting_preedit_survives_an_overflowing_keystroke_instead_of_being_reset() {
        // Unlike the previous test, this one has real prior composition
        // content when the overflow happens. Before this fix, `Err(Overflow)`
        // reaching `send_key`'s catch site called `session.reset()`,
        // silently discarding everything the user had already typed just
        // because one more keystroke would not fit alongside it.
        let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
        let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
        let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
        let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "app.exe");

        // "k" has no entry in this minimal table, so it passes through raw.
        type_word(&mut dispatcher, session, "kkk", &mut out);
        assert_eq!(out.preedit_text(), "kkk");

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            &mut out,
        );
        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));

        assert_eq!(
            dispatcher.sessions.get(session).unwrap().preedit.as_str(),
            "kkk",
            "an overflowing keystroke must never erase the composition already typed"
        );

        // The session must still be usable afterward.
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('k'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "kkkk");
    }

    #[test]
    fn preedit_overflow_rejects_the_input_without_destroying_the_composition() {
        // Broader than `a_preexisting_preedit_survives_an_overflowing_
        // keystroke_instead_of_being_reset`: this snapshots the *entire*
        // `Session` (every field, via its derived `PartialEq`) rather than
        // only `preedit`, so a regression that partially advances
        // `raw_input`, `romaji`, `cursor`, segments, or any other field on
        // the way to the failing write cannot slip past.
        let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
        let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
        let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
        let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "app.exe");

        // "k" has no entry in this minimal table, so it passes through raw,
        // giving real prior composition content -- preedit, raw_input, and
        // romaji all have something to lose if the overflowing keystroke
        // that follows is not fully atomic.
        type_word(&mut dispatcher, session, "kkk", &mut out);
        assert_eq!(out.preedit_text(), "kkk");

        let before = dispatcher.sessions.get(session).expect("session").clone();

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));
        // `Reply::Message` promises an empty `OutputBuf`: no preedit, no
        // commit, nothing for the client to render or forward. The
        // offending key never leaves the engine's own rejection path.
        assert_eq!(out.preedit_text(), "");
        assert_eq!(out.commit_text(), None);

        let after = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            after, &before,
            "an overflowing keystroke must leave every session field \
             exactly as it was -- no commit, no partial write, no lost \
             composition"
        );

        // The session must still be usable afterward: Backspace still
        // works on the untouched composition.
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "kk",
            "Backspace must still work after a rejected overflow"
        );
    }

    #[test]
    fn preedit_overflow_rejects_non_ascii_key_without_losing_or_leaking_it() {
        // Connects the asymmetry proven in
        // `romaji::tests::non_ascii_overflow_never_loses_the_current_character`:
        // the romaji FSM itself has nowhere to hold a non-ASCII character
        // that could not be written to its sink. If `feed_character` ever
        // wrote `session.romaji` or `session.preedit` back *before* every
        // fallible step had already succeeded, a non-ASCII keystroke
        // landing exactly at the preedit capacity boundary would vanish
        // for good -- not merely get rejected.
        // Same minimal table as the sibling overflow tests: "k" has no
        // entry, so it passes through raw, one byte per keystroke, with no
        // sokuon/consonant-doubling complexity to reason about while
        // filling `preedit` to an exact byte boundary.
        let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
        let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
        let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
        let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "app.exe");

        // Fill preedit to exactly MAX_PREEDIT_BYTES with a plain ASCII
        // passthrough character, leaving zero bytes of room. Any further
        // character -- ASCII or not -- must now be rejected. Read the fill
        // back from the session itself, not from `out`: if the very last
        // fill keystroke happened to be the one that overflowed, Apply's
        // catch site would have cleared `out` already, which would say
        // nothing about how much actually landed in `session.preedit`.
        let filler = "k".repeat(MAX_PREEDIT_BYTES);
        type_word(&mut dispatcher, session, &filler, &mut out);
        assert_eq!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .preedit
                .len(),
            MAX_PREEDIT_BYTES,
            "the filler must land byte-for-byte with no overflow of its own"
        );

        let before = dispatcher.sessions.get(session).expect("session").clone();

        // The offending key is a non-ASCII character. `table.feed` resolves
        // it cleanly into a scratch buffer (there is no pending romaji to
        // flush, and the scratch buffer itself is nowhere near capacity) --
        // the overflow only happens one step later, when `feed_character`
        // tries to splice that resolved text into a `preedit` that has no
        // room left.
        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('字'),
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));
        assert_eq!(out.preedit_text(), "");
        assert_eq!(out.commit_text(), None);

        let after = dispatcher.sessions.get(session).expect("session");
        assert_eq!(
            after, &before,
            "a non-ASCII overflow must leave the session untouched -- if \
             any field advanced past the rejected preedit write, the \
             character that could not be written would be gone from both \
             the document and the engine, with no way to retype it"
        );

        // The session must still be usable: Backspace frees room (three
        // presses -- one byte each -- to fit '字''s three UTF-8 bytes), and
        // a retry of the exact same non-ASCII key now succeeds, per
        // `table.feed`'s own documented retry contract.
        for _ in 0..3 {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Backspace),
                },
                &mut out,
            );
        }
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('字'),
            },
            &mut out,
        );
        assert!(
            out.preedit_text().ends_with('字'),
            "retrying the exact same non-ASCII key after freeing room must \
             recover it, not lose it a second time"
        );
    }

    #[test]
    fn probe_overflow_and_apply_overflow_agree_without_mutating_session() {
        // Mirrors `a_test_only_key_reports_what_the_real_dispatch_would_
        // without_mutating_the_session`, but for the overflow path. Probe
        // (`probe_session`) discards the `Result` of `apply_key` and always
        // answers `Reply::Output`, unlike Apply's explicit `out.clear()` +
        // `Reply::Message(Error(TooLarge))`. Full `Output` equality between
        // the two paths is not required here -- only that Probe's
        // `consumed` verdict agrees with Apply's decision to reject rather
        // than forward the key, so `OnTestKeyDown` never tells the host
        // "not mine" for a key the real path is about to swallow.
        let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
        let table_source = format!("[kana]\na = \"{huge}\"\n");

        let mut probe_dispatcher = Dispatcher::with_parts(
            Table::parse(&table_source).expect("table compiles"),
            KeyMap::preset(Preset::MsIme).expect("preset compiles"),
            Normalizer::default(),
        );
        let mut apply_dispatcher = Dispatcher::with_parts(
            Table::parse(&table_source).expect("table compiles"),
            KeyMap::preset(Preset::MsIme).expect("preset compiles"),
            Normalizer::default(),
        );
        let mut probe_out = OutputBuf::new();
        let mut apply_out = OutputBuf::new();
        let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "probe.exe");
        let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "apply.exe");

        type_word(&mut probe_dispatcher, probe_session, "kkk", &mut probe_out);
        type_word(&mut apply_dispatcher, apply_session, "kkk", &mut apply_out);

        let probe_before = probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("probe session")
            .clone();
        let apply_before = apply_dispatcher
            .sessions
            .get(apply_session)
            .expect("apply session")
            .clone();

        let probe_reply = probe_dispatcher.dispatch(
            &Request::SendKey {
                session: probe_session,
                key: test_only_char_key('a'),
            },
            &mut probe_out,
        );
        assert_eq!(probe_reply, Reply::Output);
        let probe_consumed = probe_out.consumed;
        assert_eq!(
            probe_dispatcher
                .sessions
                .get(probe_session)
                .expect("probe session"),
            &probe_before,
            "Probe must never mutate the session, even on an overflowing key"
        );

        let apply_reply = apply_dispatcher.dispatch(
            &Request::SendKey {
                session: apply_session,
                key: char_key('a'),
            },
            &mut apply_out,
        );
        assert_eq!(
            apply_reply,
            Reply::Message(Response::Error(ErrorCode::TooLarge))
        );
        assert_eq!(
            apply_dispatcher
                .sessions
                .get(apply_session)
                .expect("apply session"),
            &apply_before,
            "Apply must leave the session untouched on Overflow"
        );

        assert!(
            probe_consumed,
            "Probe must report an overflowing key as consumed, agreeing \
             with Apply's decision to reject rather than forward it to the \
             host -- otherwise OnTestKeyDown tells TSF the key is free for \
             the host while the real key press is about to be swallowed by \
             a TooLarge rejection"
        );
    }

    #[test]
    fn a_maximal_suggestion_commit_fits_exactly_at_the_preedit_boundary() {
        // `commit_suggestion_at` stages the candidate's normalized surface
        // into `scratch` (and the reading into a fresh `preedit`) *before*
        // clearing/mutating `session` at all, precisely so a fallible
        // normalization can never leave `romaji`/`raw_input`/`preedit`
        // cleared with nothing committed. That ordering is already in place
        // (see the comment at its `scratch.clear()` call).
        //
        // A prediction candidate's surface is typed as
        // `FixedStr<MAX_PREDICTION_SURFACE_BYTES>` (512 bytes) regardless of
        // source (system, user dictionary, or learned history) -- there is
        // no path that can hand `commit_suggestion_at` a larger one. The
        // widest expansion `normalize_into` can apply is 3x, ASCII (1 byte)
        // to fullwidth (3 bytes) alnum. 512 * 3 == 1536 == MAX_PREEDIT_BYTES
        // exactly, and `FixedStr::push_str` accepts a write that lands
        // exactly on capacity (`new_len > N` is the only rejection, not
        // `>=`). So the maximal legitimate suggestion commit always lands
        // precisely on the boundary and always succeeds: this specific
        // `Overflow` path is provably unreachable through any real
        // prediction candidate given today's constants, not merely untested.
        // This test locks in that exact-fit boundary instead of asserting an
        // overflow that cannot occur, so a future change narrowing either
        // constant (or widening the expansion ratio) will fail loudly here
        // rather than silently reopening the corruption `commit_suggestion_at`
        // was fixed to prevent.
        let full_surface = "x".repeat(crate::prediction::MAX_PREDICTION_SURFACE_BYTES);
        let conversion = prediction_conversion_from_source(
            "maximal-suggestion.tsv",
            &format!(
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nあ\t{full_surface}\t0\t0\t100\t100\tpredict\ttest\n"
            ),
        );
        let learning = Arc::new(LearningService::memory());
        let runtime = crate::prediction::PredictionRuntime::start_with_learning(
            Arc::clone(&conversion),
            Arc::clone(&learning),
        )
        .expect("prediction runtime");
        let profile = AppProfile {
            process_name: "notepad.exe".to_owned(),
            default_mode: Mode::Hiragana,
            normalizer: Normalizer {
                width: WidthPolicy {
                    alnum: Width::Full,
                    number: Width::Half,
                    symbol: Width::Half,
                },
                punctuation: PunctuationStyle::default(),
            },
            prediction_enabled: true,
            suggest_accept: SuggestAccept::Tab,
        };
        let mut dispatcher = Dispatcher::new_with_runtime_configuration_and_profiles(
            conversion,
            learning,
            runtime.service(),
            Preferences::default(),
            Arc::from(vec![profile]),
        )
        .expect("shipped defaults");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "a", &mut out);
        assert_eq!(out.preedit_text(), "あ");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Predicting,
            "the 512-byte entry must be indexed and offered as a focusable suggestion"
        );

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_ne!(
            reply,
            Reply::Message(Response::Error(ErrorCode::TooLarge)),
            "a maximal (512-byte) suggestion must fit exactly in the 1536-byte preedit, not overflow it"
        );
        let expected_commit = "\u{FF58}".repeat(crate::prediction::MAX_PREDICTION_SURFACE_BYTES);
        assert_eq!(out.commit_text(), Some(expected_commit.as_str()));
        assert_eq!(
            out.commit_text().unwrap().len(),
            crate::prediction::MAX_PREDICTION_SURFACE_BYTES * 3,
            "every one of the 512 ASCII bytes must have widened to a 3-byte fullwidth character"
        );

        assert_eq!(
            dispatcher.sessions.get(session).unwrap().state(),
            State::Idle
        );

        // The session must still be usable afterward.
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('k'),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "か");

        runtime.stop().expect("prediction worker joins");
    }

    #[test]
    fn a_probed_overflowing_keystroke_never_mutates_the_live_session() {
        // The legacy `test_only` SendKey path (and `ProbeKey`, `probe_key`)
        // always runs against a throwaway clone (`probe_session`) and never
        // touches `self.sessions`, so this must hold with no change from
        // this fix -- unlike the general SendKey/Commit paths above, which
        // needed one.
        let huge = "あ".repeat(600);
        let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
        let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
        let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "app.exe");
        type_word(&mut dispatcher, session, "kkk", &mut out);
        assert_eq!(out.preedit_text(), "kkk");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: test_only_char_key('a'),
            },
            &mut out,
        );

        assert_eq!(
            dispatcher.sessions.get(session).unwrap().preedit.as_str(),
            "kkk",
            "a probed keystroke must never mutate the live session, overflowing or not"
        );
    }

    fn oversized_numbered_candidate_dispatcher() -> Dispatcher {
        // きょう has one small candidate. です has a small default (index 0)
        // and a larger alternative (index 1) that, stitched together with
        // きょう's own committed surface, exceeds MAX_PREEDIT_BYTES -- but
        // is small enough on its own to render fine in です's own candidate
        // window before it is picked.
        let kyou_surface = "あ".repeat(250); // 750 bytes
        let desu_alt = "い".repeat(350); // 1050 bytes; 750 + 1050 > 1536
        let source = format!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nきょう\t{kyou_surface}\t0\t0\t100\t100\t\tsmall\nです\tです\t0\t0\t100\t100\t\tsmall-default\nです\t{desu_alt}\t0\t0\t200\t200\t\tlarge-alt\n"
        );
        let entries =
            dictc::parse_entries("numbered-candidate-overflow.tsv", &source).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        let conversion = Arc::new(
            ConversionService::from_static_bytes(image).expect("conversion service fixture"),
        );
        Dispatcher::new_with_conversion(conversion).expect("shipped defaults")
    }

    #[test]
    fn an_overflowing_numbered_candidate_pick_restores_the_cleared_segment_transform() {
        // `commit_numbered_candidate` has to clear the focused segment's
        // transform *before* calling `commit_converted_segments`, or the
        // transform branch there shadows the numbered override entirely.
        // That clear used to be unconditional: if the subsequent commit
        // failed with `Overflow` (a different segment overflowing the
        // shared stitching buffer, in this case), the clear stuck around
        // even though nothing was actually committed -- silently discarding
        // an F6-F10 transform the user was still looking at.
        let mut dispatcher = oversized_numbered_candidate_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");

        type_word(&mut dispatcher, session, "kyoudesu", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        let converted = out.to_output().preedit.expect("converted preedit");
        assert_eq!(converted.segments.len(), 2);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Right),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F6),
            },
            &mut out,
        );
        let before = dispatcher
            .sessions
            .get(session)
            .unwrap()
            .segment_transform(1);
        assert_ne!(
            before.0,
            SegmentTransform::None,
            "F6 must have armed a transform on the focused (です) segment"
        );
        let preedit_before = dispatcher
            .sessions
            .get(session)
            .unwrap()
            .preedit
            .as_str()
            .to_string();

        // "2" picks です's second (larger) candidate. Combined with きょう's
        // own surface, the stitched commit overflows MAX_PREEDIT_BYTES.
        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('2'),
            },
            &mut out,
        );
        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));

        let after = dispatcher.sessions.get(session).unwrap();
        assert!(
            after.converting,
            "a failed numbered pick must not cancel the conversion"
        );
        assert_eq!(after.segment_count(), 2);
        assert_eq!(
            after.segment_transform(1),
            before,
            "the transform cleared to let the override apply must be restored \
             when the override's commit never went through"
        );
        assert_eq!(
            after.preedit.as_str(),
            preedit_before,
            "a failed numbered pick must not touch the composition being edited"
        );

        // The session must still be usable afterward: picking です's small
        // default candidate now commits normally.
        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('1'),
            },
            &mut out,
        );
        assert!(!matches!(
            reply,
            Reply::Message(Response::Error(ErrorCode::TooLarge))
        ));
    }

    fn oversized_render_segment_dispatcher() -> Dispatcher {
        // きょう has four small candidates, so its selection index is
        // meaningfully normalized (`rem_euclid`) by a page-down jump that is
        // not a multiple of four. です's sole candidate is 600 bytes of
        // plain ASCII -- comfortably within dictc's own 1536-byte-per-field
        // cap, and small enough that the initial whole-string conversion at
        // Space (which stitches raw, unnormalized dictionary bytes) never
        // comes close to MAX_PREEDIT_BYTES either. It only overflows once
        // `render_converted_segments` normalizes it through this profile's
        // full-width alnum policy -- 600 ASCII bytes at up to 3 bytes each
        // fullwidth is 1800 bytes, past the 1536-byte scratch buffer -- so
        // rendering です always overflows on its own, regardless of きょう's
        // own state.
        let huge_ascii = "x".repeat(600);
        let source = format!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nきょう\tA\t0\t0\t100\t100\t\tc0\nきょう\tB\t0\t0\t200\t200\t\tc1\nきょう\tC\t0\t0\t300\t300\t\tc2\nきょう\tD\t0\t0\t400\t400\t\tc3\nです\t{huge_ascii}\t0\t0\t100\t100\t\thuge-only\n"
        );
        let entries =
            dictc::parse_entries("render-segment-overflow.tsv", &source).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        let conversion = Arc::new(
            ConversionService::from_static_bytes(image).expect("conversion service fixture"),
        );
        let profile = AppProfile {
            process_name: "editor.exe".to_owned(),
            default_mode: Mode::Hiragana,
            normalizer: Normalizer {
                width: WidthPolicy {
                    alnum: Width::Full,
                    number: Width::Half,
                    symbol: Width::Half,
                },
                punctuation: PunctuationStyle::default(),
            },
            prediction_enabled: false,
            suggest_accept: SuggestAccept::Disabled,
        };
        Dispatcher::new_with_configuration_and_profiles(
            conversion,
            Arc::new(LearningService::memory()),
            Preferences::default(),
            Arc::from(vec![profile]),
        )
        .expect("shipped defaults")
    }

    #[test]
    fn an_overflowing_render_leaves_an_earlier_segments_selection_untouched() {
        // `render_converted_segments` used to normalize (`rem_euclid`) and
        // immediately persist each segment's selection as soon as *that*
        // segment rendered successfully, one segment at a time. If a *later*
        // segment then overflowed the shared stitching buffer, the earlier
        // segment's persisted (normalized) selection stuck around even
        // though the render as a whole never completed and nothing new was
        // ever shown to the user.
        let mut dispatcher = oversized_render_segment_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");

        type_word(&mut dispatcher, session, "kyoudesu", &mut out);
        // です's sole candidate alone overflows, so conversion starts in an
        // already-overflowing state; that is fine here; nothing has changed
        // きょう's selection away from its initial 0 yet.
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        let started = dispatcher.sessions.get(session).unwrap();
        assert!(started.converting);
        assert_eq!(started.segment_count(), 2);
        assert_eq!(started.segment_selection(0), 0);

        // Page-down directly writes きょう's raw (unfolded) selection via
        // its own action handler, independent of rendering: 0 + 9 = 9.
        // Rendering that selection folds it to 9 % 4 = 1 -- a different
        // value, so persisting the fold is observable.
        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::PageDown),
            },
            &mut out,
        );
        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));

        let after = dispatcher.sessions.get(session).unwrap();
        assert!(
            after.converting,
            "a failed render must not cancel the conversion"
        );
        assert_eq!(after.segment_count(), 2);
        assert_eq!(
            after.segment_selection(0),
            CANDIDATE_PAGE_SIZE as i16,
            "the raw selection the page-down action wrote directly must survive \
             a render that failed on a later segment -- the render's own \
             normalized (rem_euclid) value must never have been persisted"
        );
    }

    #[test]
    fn commit_request_flushes_pending_romaji_instead_of_dropping_it() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        // A lone trailing "n" never resolves through `feed` alone -- it
        // needs a flush, which is exactly what a top-level Commit must do
        // rather than silently drop it.
        type_word(&mut dispatcher, session, "kon", &mut out);
        assert_eq!(out.preedit_text(), "こn");

        let reply = dispatcher.dispatch(&Request::Commit { session }, &mut out);

        assert_eq!(reply, Reply::Output);
        assert_eq!(out.commit_text(), Some("こん"));
        assert_eq!(out.preedit_text(), "");
    }

    #[test]
    fn conversion_exposes_candidates_moves_selection_and_commits_the_focus() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        let first = out.to_output().candidates.expect("candidate list");
        assert_eq!(first.presentation, CandidatePresentation::Compact);
        assert_eq!(first.selected, 0);
        assert!(first.items.len() > CANDIDATE_PAGE_SIZE);
        assert_eq!(first.page_size, CANDIDATE_PAGE_SIZE as u16);
        assert_eq!(first.items[0].text, "仮名");
        assert_eq!(first.items[0].annotation, "IT用語");
        assert_eq!(out.preedit_text(), "仮名");
        assert_eq!(
            dispatcher.sessions.get(session).map(Session::state),
            Some(sakura_core::keymap::State::Converting)
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        let second = out.to_output().candidates.expect("candidate list");
        assert_eq!(second.presentation, CandidatePresentation::Expanded);
        assert_eq!(second.selected, 1);
        assert_eq!(out.preedit_text(), "加奈");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("加奈"));
        assert_eq!(out.preedit_text(), "");
        assert_eq!(out.candidate_count(), 0);
        assert_eq!(
            dispatcher.sessions.get(session).map(Session::state),
            Some(sakura_core::keymap::State::Idle)
        );
    }

    #[test]
    fn candidate_pages_and_number_shortcuts_use_the_visible_page() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::PageDown),
            },
            &mut out,
        );
        let second_page = out.to_output().candidates.expect("second page");
        assert_eq!(usize::from(second_page.selected), CANDIDATE_PAGE_SIZE);
        assert_eq!(second_page.current_page(), 1);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('9'),
            },
            &mut out,
        );
        assert!(out.beep, "a missing slot on the short final page must beep");
        assert!(
            out.has_candidates(),
            "an invalid shortcut keeps the list open"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('2'),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("候補11"));
        assert_eq!(out.candidate_count(), 0);
        assert_eq!(
            dispatcher.sessions.get(session).map(Session::state),
            Some(sakura_core::keymap::State::Idle)
        );
    }

    #[test]
    fn cancel_from_candidates_returns_to_raw_preedit_then_clears_it() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Escape),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "かな");
        assert_eq!(out.candidate_count(), 0);
        assert_eq!(
            dispatcher.sessions.get(session).map(Session::state),
            Some(sakura_core::keymap::State::Composing)
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Escape),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "");
        assert_eq!(
            dispatcher.sessions.get(session).map(Session::state),
            Some(sakura_core::keymap::State::Idle)
        );
    }

    #[test]
    fn a_character_after_conversion_commits_then_starts_a_new_composition() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('n'),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("仮名"));
        assert_eq!(out.preedit_text(), "n");
        assert_eq!(out.candidate_count(), 0);
        assert_eq!(
            dispatcher.sessions.get(session).map(Session::state),
            Some(sakura_core::keymap::State::Composing)
        );
    }

    #[test]
    fn caret_insertion_and_forward_delete_edit_at_the_visible_cursor() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        assert_eq!(out.preedit_text(), "かな");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Left),
            },
            &mut out,
        );
        assert_eq!(
            out.to_output().preedit.expect("preedit").cursor,
            1,
            "caret must move by characters, not UTF-8 bytes"
        );

        type_word(&mut dispatcher, session, "na", &mut out);
        assert_eq!(out.preedit_text(), "かなな");
        assert_eq!(out.to_output().preedit.expect("preedit").cursor, 2);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Delete),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "かな");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Home),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Delete),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "な");
        assert_eq!(out.to_output().preedit.expect("preedit").cursor, 0);
    }

    #[test]
    fn f6_f10_transform_raw_composition_and_cycle_case_without_a_dictionary() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "gattsu", &mut out);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F7),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "ガッツ");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F8),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "ｶﾞｯﾂ");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("ｶﾞｯﾂ"));

        type_word(&mut dispatcher, session, "docker", &mut out);
        for expected in ["docker", "DOCKER", "Docker"] {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::F10),
                },
                &mut out,
            );
            assert_eq!(out.preedit_text(), expected);
        }
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F9),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "ｄｏｃｋｅｒ");
    }

    #[test]
    fn segment_focus_keeps_candidate_selections_independent() {
        let mut dispatcher = segmented_conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "kyoudesu", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        let initial = out.to_output().preedit.expect("converted preedit");
        assert_eq!(initial.segments.len(), 2);
        assert_eq!(initial.segments[0].text, "今日");
        assert_eq!(initial.segments[0].underline, UnderlineKind::Focused);
        assert_eq!(initial.segments[1].text, "です");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Right),
            },
            &mut out,
        );
        let second_candidates = out
            .to_output()
            .candidates
            .expect("second segment candidates");
        let second_alternative = second_candidates.items[1].text.clone();
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Down),
            },
            &mut out,
        );
        let after_second = out.to_output().preedit.expect("converted preedit");
        assert_eq!(after_second.segments[0].text, "今日");
        assert_eq!(after_second.segments[1].text, second_alternative);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Left),
            },
            &mut out,
        );
        let first_candidates = out
            .to_output()
            .candidates
            .expect("first segment candidates");
        let first_alternative = first_candidates.items[1].text.clone();
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Down),
            },
            &mut out,
        );
        let after_first = out.to_output().preedit.expect("converted preedit");
        assert_eq!(after_first.segments[0].text, first_alternative);
        assert_eq!(after_first.segments[1].text, second_alternative);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Right, Modifiers::SHIFT),
            },
            &mut out,
        );
        assert_eq!(
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .segment_count(),
            2,
            "local resize must not merge or drop a neighbouring segment"
        );
    }

    #[test]
    fn an_existing_dispatcher_observes_an_atomically_reloaded_user_dictionary() {
        let conversion = conversion_fixture();
        let mut dispatcher =
            Dispatcher::new_with_conversion(Arc::clone(&conversion)).expect("shipped defaults");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        conversion.replace_user_dictionary(
            sakura_core::UserDictionary::parse_tsv(
                "reading\tsurface\tpos\tcomment\nさくら\tSakura Input\tproper-noun\tproject\n",
            )
            .expect("user dictionary"),
        );

        type_word(&mut dispatcher, session, "sakura", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );

        assert_eq!(out.preedit_text(), "Sakura Input");
        assert_eq!(
            out.to_output().candidates.expect("candidate list").items[0].annotation,
            "project"
        );
    }

    #[test]
    fn commit_cache_reselects_a_homophone_within_the_session() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Down),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "加奈");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );

        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "加奈");
        assert_eq!(out.selected_candidate(), Some(1));
    }

    #[test]
    fn process_shared_learning_reselects_a_homophone_in_a_new_session() {
        let conversion = conversion_fixture();
        let learning = Arc::new(LearningService::memory());
        let mut first =
            Dispatcher::new_with_services(Arc::clone(&conversion), Arc::clone(&learning))
                .expect("shipped defaults");
        let mut out = OutputBuf::new();
        let first_session = create_session(&mut first, &mut out, "first.exe");
        type_word(&mut first, first_session, "kana", &mut out);
        for code in [KeyCode::Space, KeyCode::Down, KeyCode::Enter] {
            first.dispatch(
                &Request::SendKey {
                    session: first_session,
                    key: named_key(code),
                },
                &mut out,
            );
        }
        assert_eq!(out.commit_text(), Some("加奈"));

        let mut second = Dispatcher::new_with_services(conversion, Arc::clone(&learning))
            .expect("shipped defaults");
        let second_session = create_session(&mut second, &mut out, "second.exe");
        type_word(&mut second, second_session, "kana", &mut out);
        second.dispatch(
            &Request::SendKey {
                session: second_session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );

        assert_eq!(out.preedit_text(), "加奈");
        assert_eq!(out.selected_candidate(), Some(1));
        assert_eq!(
            learning
                .preference("かな", 0, [("仮名", 0), ("加奈", 0)])
                .exact,
            Some(1)
        );
    }

    #[test]
    fn one_off_far_learning_does_not_override_base_conversion_but_repetition_does() {
        let conversion = conversion_fixture();
        let learning = Arc::new(LearningService::memory());
        let mut dispatcher = Dispatcher::new_with_services(conversion, Arc::clone(&learning))
            .expect("shipped defaults");
        let mut out = OutputBuf::new();

        // Candidate 6 is intentionally far below the base winner. A single
        // exact-context confirmation stays in the learning store but must not
        // make the next conversion surprising.
        learning.learn("かな", "候補07", 0, 0);
        let first = create_session(&mut dispatcher, &mut out, "first.exe");
        type_word(&mut dispatcher, first, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session: first,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "仮名");
        assert_eq!(out.selected_candidate(), Some(0));

        // Repeated explicit evidence in the same grammatical context becomes
        // strong enough to select the user's genuine preference.
        learning.learn("かな", "候補07", 0, 0);
        learning.learn("かな", "候補07", 0, 0);
        let second = create_session(&mut dispatcher, &mut out, "second.exe");
        type_word(&mut dispatcher, second, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session: second,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "候補07");
        assert_eq!(out.selected_candidate(), Some(6));
    }

    #[test]
    fn explicit_learning_beats_conflicting_commit_cache_and_domain_coherence() {
        let conversion = conversion_fixture();
        let learning = Arc::new(LearningService::memory());
        let mut dispatcher = Dispatcher::new_with_services(conversion, Arc::clone(&learning))
            .expect("shipped defaults");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "engineering-editor.exe");

        // Establish both lower layers in conflict with the eventual learned
        // choice: the session's domain ratio and commit cache prefer the IT
        // candidate `仮名`.
        for _ in 0..4 {
            type_word(&mut dispatcher, session, "kana", &mut out);
            for code in [KeyCode::Space, KeyCode::Enter] {
                dispatcher.dispatch(
                    &Request::SendKey {
                        session,
                        key: named_key(code),
                    },
                    &mut out,
                );
            }
            assert_eq!(out.commit_text(), Some("仮名"));
        }

        learning.clear().expect("replace prior learning atomically");
        learning.learn("かな", "加奈", 0, 0);
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );

        assert_eq!(out.preedit_text(), "加奈");
        assert_eq!(out.selected_candidate(), Some(1));
    }

    fn capture_replay(label: &str, out: &OutputBuf, snapshot: &mut String) {
        let output = out.to_output();
        write!(
            snapshot,
            "{label}: consumed={} beep={} delete=",
            output.consumed, output.beep
        )
        .expect("snapshot write");
        if output.delete_before.is_empty() {
            snapshot.push_str("<none>");
        } else {
            write!(snapshot, "{:?}", output.delete_before).expect("snapshot write");
        }
        write!(snapshot, " mode={:?}", output.mode).expect("snapshot write");
        if let Some(preedit) = output.preedit {
            snapshot.push_str(" preedit=[");
            for (index, segment) in preedit.segments.iter().enumerate() {
                if index > 0 {
                    snapshot.push(',');
                }
                write!(snapshot, "{}:{:?}", segment.text, segment.underline)
                    .expect("snapshot write");
            }
            write!(snapshot, "]@{}", preedit.cursor).expect("snapshot write");
        }
        if let Some(commit) = output.commit {
            write!(snapshot, " commit={commit}").expect("snapshot write");
        }
        if let Some(candidates) = output.candidates {
            let selected = usize::from(candidates.selected);
            write!(
                snapshot,
                " candidates={}/{}:{}|{}",
                selected,
                candidates.items.len(),
                candidates.items[selected].text,
                candidates.items[selected].annotation
            )
            .expect("snapshot write");
        }
        snapshot.push('\n');
    }

    #[test]
    fn whole_session_editing_replay_matches_the_checked_in_snapshot() {
        let mut snapshot = String::new();
        let mut out = OutputBuf::new();
        let mut dispatcher = conversion_dispatcher();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");

        type_word(&mut dispatcher, session, "kana", &mut out);
        capture_replay("raw-kana", &out, &mut snapshot);
        for (label, key) in [
            ("convert", named_key(KeyCode::Space)),
            ("choose-second", named_key(KeyCode::Down)),
            ("commit-choice", named_key(KeyCode::Enter)),
        ] {
            dispatcher.dispatch(&Request::SendKey { session, key }, &mut out);
            capture_replay(label, &out, &mut snapshot);
        }
        type_word(&mut dispatcher, session, "kana", &mut out);
        capture_replay("raw-kana-again", &out, &mut snapshot);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        capture_replay("cache-reselect", &out, &mut snapshot);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        capture_replay("commit-cached", &out, &mut snapshot);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
            },
            &mut out,
        );
        capture_replay("undo-commit", &out, &mut snapshot);
        assert_eq!(
            dispatcher.dispatch(
                &Request::UndoCommit {
                    session,
                    outcome: UndoCommitOutcome::Applied,
                },
                &mut out,
            ),
            Reply::Message(Response::Ok)
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Escape),
            },
            &mut out,
        );
        capture_replay("cancel-restored-reading", &out, &mut snapshot);

        type_word(&mut dispatcher, session, "docker", &mut out);
        capture_replay("raw-identifier", &out, &mut snapshot);
        for (label, code) in [
            ("lower", KeyCode::F10),
            ("upper", KeyCode::F10),
            ("title", KeyCode::F10),
            ("full-width", KeyCode::F9),
        ] {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(code),
                },
                &mut out,
            );
            capture_replay(label, &out, &mut snapshot);
        }

        let mut segmented = segmented_conversion_dispatcher();
        let segmented_session = create_session(&mut segmented, &mut out, "segments.exe");
        type_word(&mut segmented, segmented_session, "kyoudesu", &mut out);
        segmented.dispatch(
            &Request::SendKey {
                session: segmented_session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        capture_replay("segments", &out, &mut snapshot);
        segmented.dispatch(
            &Request::SendKey {
                session: segmented_session,
                key: modified_named_key(KeyCode::Left, Modifiers::SHIFT),
            },
            &mut out,
        );
        capture_replay("shrink-focused", &out, &mut snapshot);
        segmented.dispatch(
            &Request::SendKey {
                session: segmented_session,
                key: modified_named_key(KeyCode::Right, Modifiers::SHIFT),
            },
            &mut out,
        );
        capture_replay("restore-boundary", &out, &mut snapshot);
        segmented.dispatch(
            &Request::SendKey {
                session: segmented_session,
                key: named_key(KeyCode::Right),
            },
            &mut out,
        );
        capture_replay("focus-next", &out, &mut snapshot);

        assert_eq!(
            snapshot.trim_end(),
            include_str!("../../../corpus/session-replay/phase3-editing.snap").trim_end()
        );
    }

    #[test]
    fn commit_undo_context_selects_the_contextual_homophone_and_rolls_it_back() {
        let mut dispatcher = contextual_conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "context.exe");
        type_word(&mut dispatcher, session, "ishani", &mut out);
        for code in [KeyCode::Space, KeyCode::Enter] {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(code),
                },
                &mut out,
            );
        }
        assert_eq!(out.commit_text(), Some("医者に"));

        type_word(&mut dispatcher, session, "itta", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "行った");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
            },
            &mut out,
        );
        assert_eq!(out.delete_before(), "行った");
        assert_eq!(out.preedit_text(), "いった");

        let boundary_session = create_session(&mut dispatcher, &mut out, "boundary.exe");
        type_word(&mut dispatcher, boundary_session, "owari", &mut out);
        for code in [KeyCode::Space, KeyCode::Enter] {
            dispatcher.dispatch(
                &Request::SendKey {
                    session: boundary_session,
                    key: named_key(code),
                },
                &mut out,
            );
        }
        assert_eq!(out.commit_text(), Some("終わり。"));
        type_word(&mut dispatcher, boundary_session, "itta", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session: boundary_session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "言った");
    }

    #[test]
    fn password_scope_cannot_reach_persistent_learning() {
        let learning = LearningService::memory();
        let mut session = Session::new("password.exe");
        session.scope = InputScope::Password;
        session.preedit.push_str("かな").expect("bounded reading");

        record_learning(
            &session,
            Some(&learning),
            None,
            ExecutionPolicy::Apply,
            "加奈",
            0,
        );

        assert_eq!(
            learning.preference("かな", 0, [("仮名", 0), ("加奈", 0)]),
            LearningPreference {
                exact: None,
                general: None,
            }
        );
    }

    #[test]
    fn ctrl_backspace_restores_the_reading_and_expires_after_another_key() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Down),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some("加奈"));

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
            },
            &mut out,
        );
        assert!(out.consumed);
        assert_eq!(out.delete_before(), "加奈");
        assert_eq!(out.preedit_text(), "かな");

        assert_eq!(
            dispatcher.dispatch(
                &Request::UndoCommit {
                    session,
                    outcome: UndoCommitOutcome::Applied,
                },
                &mut out,
            ),
            Reply::Message(Response::Ok)
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "仮名",
            "undo must evict the retracted commit-cache choice"
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Left),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
            },
            &mut out,
        );
        assert!(
            !out.consumed,
            "expired undo must reach the host application"
        );
        assert_eq!(out.delete_before(), "");
    }

    #[test]
    fn commit_undo_pending_blocks_other_session_mutations_until_explicit_outcome() {
        let mut dispatcher = conversion_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "editor.exe");
        type_word(&mut dispatcher, session, "kana", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
            },
            &mut out,
        );
        assert_eq!(out.delete_before(), "仮名");
        let before = dispatcher.sessions.get(session).expect("session");
        assert!(before.undo_pending());
        assert_eq!(before.preedit.as_str(), "かな");
        assert_eq!(before.scope(), InputScope::Normal);

        // Simulate an external learning update while the host-side exact-text
        // deletion is pending. The incompatible requests below must be
        // fenced before dispatch can invalidate this live cache or advance
        // the observed epoch.
        let learning = Arc::new(LearningService::memory());
        dispatcher.observed_learning_generation = learning.generation();
        dispatcher.learning = Some(Arc::clone(&learning));
        dispatcher.prediction_cache.attempted = true;
        dispatcher.prediction_cache.session = session;
        dispatcher.prediction_cache.generation = 7;
        dispatcher.prediction_cache.has_result = true;
        let cache_before = (*dispatcher.prediction_cache).clone();
        let observed_before = dispatcher.observed_learning_generation;
        learning.learn("かな", "pending external update", 0, 0);

        let blocked = [
            Request::Commit { session },
            Request::Reconvert {
                session,
                text: "かな".to_owned(),
                preview: false,
            },
            Request::Revert { session },
            Request::SetInputScope {
                session,
                scope: InputScope::Password,
            },
            Request::SetMode {
                session,
                mode: Mode::Katakana,
            },
            Request::DeleteSession { session },
        ];
        for request in blocked {
            assert_eq!(
                dispatcher.dispatch(&request, &mut out),
                Reply::Message(Response::Error(ErrorCode::Busy)),
                "{request:?} must not escape a pending exact undo"
            );
            let current = dispatcher
                .sessions
                .get(session)
                .expect("session remains live");
            assert!(current.undo_pending());
            assert_eq!(current.preedit.as_str(), "かな");
            assert_eq!(current.scope(), InputScope::Normal);
            assert_eq!(
                *dispatcher.prediction_cache, cache_before,
                "a Busy request must not invalidate the live cache"
            );
            assert_eq!(
                dispatcher.observed_learning_generation, observed_before,
                "a Busy request must not advance the observed learning epoch"
            );
        }

        assert_eq!(
            dispatcher.dispatch(
                &Request::UndoCommit {
                    session,
                    outcome: UndoCommitOutcome::Rejected,
                },
                &mut out,
            ),
            Reply::Message(Response::Ok)
        );
        assert!(!dispatcher
            .sessions
            .get(session)
            .expect("session")
            .undo_pending());
    }

    #[test]
    fn revert_request_discards_the_composition_without_committing() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "ka", &mut out);

        let reply = dispatcher.dispatch(&Request::Revert { session }, &mut out);

        assert_eq!(reply, Reply::Message(Response::Ok));
        // Revert answers Ok directly (no Output), so a following SendKey
        // starting fresh is what proves the composition is actually gone.
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('k'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "k");
    }

    #[test]
    fn language_bar_mode_change_is_idle_scope_checked_and_never_writes_text() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        // A fresh TSF connection has not yet classified its focused field.
        // The language bar must not guess that it is ordinary text.
        assert_eq!(
            dispatcher.dispatch(
                &Request::SetMode {
                    session,
                    mode: Mode::Katakana,
                },
                &mut out,
            ),
            Reply::Message(Response::Error(ErrorCode::Busy))
        );
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Hiragana
        );

        assert_eq!(
            dispatcher.dispatch(
                &Request::SetInputScope {
                    session,
                    scope: InputScope::Normal,
                },
                &mut out,
            ),
            Reply::Message(Response::Ok)
        );
        assert_eq!(
            dispatcher.dispatch(
                &Request::SetMode {
                    session,
                    mode: Mode::Katakana,
                },
                &mut out,
            ),
            Reply::Message(Response::InputMode {
                mode: Mode::Katakana
            })
        );
        assert_eq!(out.preedit_text(), "");
        assert_eq!(out.commit_text(), None);
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Katakana
        );

        // Once the user has preedit, the request is rejected rather than
        // committing, cancelling, or reinterpreting the document text.
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('k'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "k");
        assert_eq!(
            dispatcher.dispatch(
                &Request::SetMode {
                    session,
                    mode: Mode::Hiragana,
                },
                &mut out,
            ),
            Reply::Message(Response::Error(ErrorCode::Busy))
        );
        let current = dispatcher.sessions.get(session).expect("session");
        assert_eq!(current.mode(), Mode::Katakana);
        assert!(current.is_composing());
    }

    #[test]
    fn ping_is_answered_with_pong() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        assert_eq!(
            dispatcher.dispatch(&Request::Ping, &mut out),
            Reply::Message(Response::Pong)
        );
    }

    #[test]
    fn shared_learning_epoch_invalidates_every_dispatchers_prediction_cache() {
        let learning = Arc::new(LearningService::memory());
        let mut administrator = builtin_dispatcher();
        administrator.observed_learning_generation = learning.generation();
        administrator.learning = Some(Arc::clone(&learning));
        let mut other_connection = builtin_dispatcher();
        other_connection.observed_learning_generation = learning.generation();
        other_connection.learning = Some(Arc::clone(&learning));
        other_connection.prediction_cache.attempted = true;
        other_connection.prediction_cache.session = 7;
        other_connection.prediction_cache.generation = 9;

        let mut out = OutputBuf::new();
        assert_eq!(
            administrator.dispatch(&Request::ClearLearning, &mut out),
            Reply::Message(Response::Ok)
        );
        assert!(other_connection.prediction_cache.attempted);

        assert_eq!(
            other_connection.dispatch(&Request::Ping, &mut out),
            Reply::Message(Response::Pong)
        );
        assert!(!other_connection.prediction_cache.attempted);
        assert_eq!(
            other_connection.observed_learning_generation,
            learning.generation()
        );
    }

    #[test]
    fn shutdown_is_answered_but_left_for_the_caller_to_act_on() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        assert_eq!(
            dispatcher.dispatch(&Request::Shutdown, &mut out),
            Reply::Shutdown(Response::Ok)
        );
    }

    #[test]
    fn reset_drops_every_session_but_keeps_configuration() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let first = create_session(&mut dispatcher, &mut out, "a.exe");

        dispatcher.reset();

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session: first,
                key: char_key('a'),
            },
            &mut out,
        );
        assert_eq!(
            reply,
            Reply::Message(Response::Error(ErrorCode::UnknownSession))
        );

        // Configuration survives: a new session still composes normally.
        let second = create_session(&mut dispatcher, &mut out, "b.exe");
        dispatcher.dispatch(
            &Request::SendKey {
                session: second,
                key: char_key('k'),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session: second,
                key: char_key('a'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "か");
    }
}
