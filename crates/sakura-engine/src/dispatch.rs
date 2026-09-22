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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sakura_core::conversion::{
    CandidateOrigin, ConversionInput, ConversionInputClass, CorrectionMap, CorrectionRun,
    CrossCommitBridge, LiteralPolicy, RawRepairPlan, RepairTier,
};
use sakura_core::dictionary::DetailRelationKind;
use sakura_core::keymap::{Action, KeyMap, KeyMapError, Preset, State};
use sakura_core::romaji::{Table, TableError};
use sakura_core::width::Normalizer;
use sakura_core::{
    contextual_punctuation_swap, default_app_profiles, resolve_context_preferences, transform_into,
    AppProfile, CommitBridgeTail, ConversionCandidate, ConversionDiagnostics, ConversionMethod,
    ConversionOptions, ConversionSegment, EntryFlags, Input, InputMethod, NeuralRerankerScope,
    Preferences, SegmentTransform, ShiftSpaceBehavior, SpaceWidth, SuggestAccept, TextSink,
};
use sakura_ipc::debug_trace;
use sakura_proto::{
    AiTextOperation, AiTextStatus, CandidateDetailInput, ErrorCode, FaultPoint, FixedStr, FixedVec,
    InputScope, KeyCode, KeyInput, Mode, OutputBuf, Overflow, Request, Response, SessionId,
    UnderlineKind, UndoCommitOutcome, CANDIDATE_PAGE_SIZE, MAX_CANDIDATE_DETAIL_DEFINITION_BYTES,
    MAX_CANDIDATE_DETAIL_RELATIONS, MAX_CANDIDATE_DETAIL_RELATION_BYTES, MAX_COMMIT_BYTES,
    MAX_PREEDIT_BYTES, MAX_SEGMENTS, PROTOCOL_VERSION,
};

use crate::ai_text::{AiTextService, Poll as AiPoll, StartError as AiStartError};
use crate::candidate_projection::{CandidateProjection, ProjectionError};
use crate::composition_fence::CompositionFence;
use crate::dictionary::{ConversionService, ConvertFailure};
use crate::input_history::{clear_path, default_path, InputHistoryService, ScopeClass};
use crate::learning::{ForgetPredictionOutcome, LearningPreference, LearningService};
use crate::long_conversion::LongConversionService;
use crate::prediction::{PredictionResult, PredictionService, PredictionSource};
use crate::session::{
    scope_is_sensitive, text_hash, RawProvenanceState, Session, SessionCrossCommitBridge,
    SessionTable,
};
use crate::shift_ascii_space::{
    decide_shift_ascii_convert, ConversionTrigger, ShiftAsciiConvertDecision,
    ShiftAsciiConvertFacts,
};
use crate::ui;

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
    /// The preset currently backing `keymap`, when this dispatcher was built
    /// from a production configuration. Test-only custom maps intentionally
    /// keep this as `None` so a live preference refresh cannot replace them.
    keymap_preset: Option<Preset>,
    normalizer: Normalizer,
    input_method: InputMethod,
    default_mode: Mode,
    conversion_method: ConversionMethod,
    space_width: SpaceWidth,
    shift_space_behavior: ShiftSpaceBehavior,
    conversion: Option<Arc<ConversionService>>,
    learning: Option<Arc<LearningService>>,
    input_history: Option<Arc<InputHistoryService>>,
    ai_text: Arc<AiTextService>,
    ai_text_owner: u64,
    prediction: Option<Arc<PredictionService>>,
    long_conversion: Option<Arc<LongConversionService>>,
    long_conversion_owner: u64,
    neural_reranker_scope: NeuralRerankerScope,
    prediction_enabled: bool,
    suggest_accept: SuggestAccept,
    association_enabled: bool,
    app_profiles: Arc<[AppProfile]>,
    /// Last process-wide learning epoch observed by this pipe worker.
    observed_learning_generation: u64,
    /// One cache per connection, boxed so nested dispatcher constructors do
    /// not copy its fixed suggestion buffers through the 160 KiB pipe stack.
    prediction_cache: Box<PredictionCache>,
    sessions: SessionTable,
    /// Process-wide composing/converting claims shared with sibling pipe
    /// workers. Absent in isolated unit tests that do not model dual delivery.
    composition_fence: Option<Arc<CompositionFence>>,
    /// Local mirror of fence claims for sessions this worker owns.
    fence_claims: HashMap<SessionId, (Box<str>, bool)>,
    connection_probe: Option<sakura_ipc::ConnectionProbe>,
    /// Process-wide candidate-board identity. Independent of AI-text owners:
    /// those restart at 1 on a private `AiTextService`, which Dual TSF workers
    /// would collide on.
    ui_connection: u64,
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
        let mut dispatcher = Self::with_parts(table, keymap, Normalizer::default());
        dispatcher.keymap_preset = Some(Preset::MsIme);
        Ok(dispatcher)
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
        dispatcher.keymap_preset = Some(preferences.keymap_preset);
        dispatcher.input_method = preferences.input_method;
        dispatcher.default_mode = preferences.default_mode;
        dispatcher.conversion_method = preferences.conversion_method;
        dispatcher.space_width = preferences.space_width;
        dispatcher.shift_space_behavior = preferences.shift_space_behavior;
        dispatcher.conversion = Some(conversion);
        dispatcher.observed_learning_generation = learning.generation();
        dispatcher.learning = Some(learning);
        dispatcher.neural_reranker_scope = preferences.neural_reranker_scope;
        dispatcher.prediction_enabled = preferences.prediction_enabled;
        dispatcher.suggest_accept = preferences.suggest_accept;
        dispatcher.association_enabled = preferences.association_enabled;
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

    /// Attaches or detaches the process-wide developer history service at a
    /// request boundary. A configuration reload can enable history after this
    /// dispatcher was created without an engine restart. Clearing the service
    /// stops new durable records; existing sessions keep their allocated
    /// history session ids until the next attach reallocates them.
    pub(crate) fn set_input_history(&mut self, input_history: Option<Arc<InputHistoryService>>) {
        let changed =
            self.input_history.as_ref().map(Arc::as_ptr) != input_history.as_ref().map(Arc::as_ptr);
        if !changed {
            return;
        }
        let attaching = self.input_history.is_none() && input_history.is_some();
        self.input_history = input_history;
        if attaching {
            if let Some(history) = self.input_history.as_ref() {
                self.sessions
                    .reallocate_history_session_ids(|| history.allocate_session_id().unwrap_or(0));
            }
        }
    }

    pub(crate) fn set_ai_text(&mut self, ai_text: Arc<AiTextService>) {
        self.ai_text.cancel_owner(self.ai_text_owner);
        self.ai_text_owner = ai_text.allocate_owner();
        self.ai_text = ai_text;
    }

    /// Replaces the process-wide prediction service at a request boundary.
    ///
    /// A configuration reload can enable or disable prediction after this
    /// dispatcher was created. Clearing the service also clears any result
    /// cache so a value produced under the old policy cannot leak into the
    /// newly configured session.
    pub(crate) fn set_prediction(&mut self, prediction: Option<Arc<PredictionService>>) {
        if self.prediction.as_ref().map(Arc::as_ptr) != prediction.as_ref().map(Arc::as_ptr) {
            self.prediction = prediction;
            self.prediction_cache.clear();
        }
    }

    pub(crate) fn set_long_conversion(
        &mut self,
        long_conversion: Option<Arc<LongConversionService>>,
    ) {
        let changed = self.long_conversion.as_ref().map(Arc::as_ptr)
            != long_conversion.as_ref().map(Arc::as_ptr);
        if !changed {
            return;
        }
        self.long_conversion_owner = long_conversion
            .as_ref()
            .map_or(0, |service| service.allocate_owner());
        self.long_conversion = long_conversion;
    }

    /// Builds a dispatcher from already-built parts. Used by tests that need
    /// a romaji table, key map, or normalizer the shipped defaults cannot
    /// reach (for example the MS-IME preset binds no key to a direct
    /// mode-switch action, so exercising `Mode::FullAlnum` needs a key map
    /// that does).
    pub fn with_parts(table: Table, keymap: KeyMap, normalizer: Normalizer) -> Self {
        let ai_text = Arc::new(AiTextService::default());
        let ai_text_owner = ai_text.allocate_owner();
        Dispatcher {
            table,
            keymap,
            keymap_preset: None,
            normalizer,
            input_method: InputMethod::Romaji,
            default_mode: Mode::Hiragana,
            conversion_method: ConversionMethod::MultiSegment,
            space_width: SpaceWidth::SameAsInput,
            shift_space_behavior: ShiftSpaceBehavior::Opposite,
            conversion: None,
            learning: None,
            input_history: None,
            ai_text,
            ai_text_owner,
            prediction: None,
            long_conversion: None,
            long_conversion_owner: 0,
            neural_reranker_scope: NeuralRerankerScope::Off,
            prediction_enabled: false,
            suggest_accept: SuggestAccept::Disabled,
            association_enabled: true,
            app_profiles: Arc::from([]),
            observed_learning_generation: 0,
            prediction_cache: Box::new(PredictionCache::new()),
            sessions: SessionTable::new(),
            composition_fence: None,
            fence_claims: HashMap::new(),
            connection_probe: None,
            ui_connection: ui::allocate_board_connection(),
            scratch: FixedStr::new(),
        }
    }

    /// Numeric identity of this pipe worker on the shared candidate board.
    /// Protocol session ids restart at 1 on every worker; this does not.
    pub(crate) fn ui_owner(&self) -> u64 {
        self.ui_connection
    }

    pub(crate) fn set_connection_probe(&mut self, probe: Option<sakura_ipc::ConnectionProbe>) {
        self.connection_probe = probe;
        if let Some(fence) = self.composition_fence.as_ref() {
            for (&session, (name, claimed)) in &self.fence_claims {
                if *claimed {
                    fence.acquire_owned(
                        name,
                        self.ui_connection,
                        session,
                        self.connection_probe.clone(),
                    );
                }
            }
        }
    }

    /// Attaches the process-wide composition fence used to absorb idle Space
    /// while a peer connection of the same host process is converting.
    pub(crate) fn set_composition_fence(&mut self, fence: Arc<CompositionFence>) {
        self.release_all_composition_fence_claims();
        self.composition_fence = Some(fence);
    }

    fn suppress_idle_space_for(&self, id: SessionId, process_name: &str) -> bool {
        self.composition_fence
            .as_ref()
            .is_some_and(|fence| fence.any_active(process_name))
            || self.sessions.peer_is_composing(id, process_name)
    }

    /// Spends the one absorption a torn-down composition is owed, when this
    /// key is the idle Space that would otherwise commit U+3000 (#102).
    ///
    /// A *live* peer claim suppresses every idle key, because that idle
    /// connection is a duplicate of a composing one. A *torn-down* claim
    /// cannot: the connection that replaces a link the DLL dropped after its
    /// 50 ms key budget is the real input path, and eating its letters would
    /// lose them. So this narrows to exactly the keystroke that produced the
    /// reported symptom -- an unbound Space on an idle session -- and spends
    /// the latch so a second one still inserts.
    fn teardown_absorbs_idle_space(&self, session: &Session, key: &KeyInput) -> bool {
        if key.code != KeyCode::Space
            || key.modifiers.ctrl()
            || key.modifiers.alt()
            || session.state() != State::Idle
            || self.keymap.lookup(State::Idle, key).is_some()
        {
            return false;
        }
        self.composition_fence
            .as_ref()
            .is_some_and(|fence| fence.consume_teardown(session.process_name()))
    }

    fn sync_composition_fence(&mut self, id: SessionId) {
        let Some(fence) = self.composition_fence.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(id) else {
            self.release_composition_fence(id);
            return;
        };
        let process_name = session.process_name().to_owned();
        let want = session.is_composing();
        let entry = self
            .fence_claims
            .entry(id)
            .or_insert_with(|| (Box::from(process_name.as_str()), false));
        if entry.0.as_ref() != process_name {
            if entry.1 {
                fence.release_owned(entry.0.as_ref(), self.ui_connection, id, false);
                entry.1 = false;
            }
            entry.0 = Box::from(process_name.as_str());
        }
        if entry.1 == want {
            return;
        }
        if want {
            fence.acquire_owned(
                process_name.as_str(),
                self.ui_connection,
                id,
                self.connection_probe.clone(),
            );
        } else {
            fence.release_owned(process_name.as_str(), self.ui_connection, id, false);
        }
        entry.1 = want;
    }

    /// Both fence teardowns below release *after a teardown*, because
    /// reaching them with `claimed` still set means the reading was live
    /// when the session went away — a deleted session or a reset connection,
    /// not a committed or cancelled composition. Those end through
    /// `sync_composition_fence` with `want == false`, which releases the
    /// claim outright and never reaches here still claimed. Without the
    /// latch the DLL reconnect after a 50 ms key timeout opens the fence and
    /// the next Space becomes a document U+3000 (#102).
    fn release_composition_fence(&mut self, id: SessionId) {
        let Some((name, claimed)) = self.fence_claims.remove(&id) else {
            return;
        };
        if claimed {
            if let Some(fence) = self.composition_fence.as_ref() {
                fence.release_owned(name.as_ref(), self.ui_connection, id, true);
            }
        }
    }

    fn release_all_composition_fence_claims(&mut self) {
        if let Some(fence) = self.composition_fence.as_ref() {
            for (id, (name, claimed)) in self.fence_claims.drain() {
                if claimed {
                    fence.release_owned(name.as_ref(), self.ui_connection, id, true);
                }
            }
        } else {
            self.fence_claims.clear();
        }
    }

    /// Drops every session, ready for a new connection to start clean.
    ///
    /// Leaves the romaji table, key map, and normalizer untouched — those
    /// are the engine's shipped configuration, not connection state. See
    /// `crate::server`'s `worker`, which calls this between connections on
    /// the same pipe instance.
    pub fn reset(&mut self) {
        self.release_all_composition_fence_claims();
        self.connection_probe = None;
        self.ai_text.cancel_owner(self.ai_text_owner);
        self.ai_text_owner = self.ai_text.allocate_owner();
        self.sessions.clear();
        self.prediction_cache.clear();
    }

    /// Applies a complete, validated settings snapshot at a request boundary.
    /// Existing sessions retain their mode and composition, while profile-
    /// resolved policies (normalization, prediction, suggestion acceptance and
    /// association) are refreshed immediately for the next key.
    pub(crate) fn apply_runtime_configuration(
        &mut self,
        preferences: Preferences,
        profiles: Arc<[AppProfile]>,
    ) -> Result<(), NewError> {
        let keymap_changed = self
            .keymap_preset
            .is_some_and(|current| current != preferences.keymap_preset);
        let prediction_policy_changed = self.prediction_enabled != preferences.prediction_enabled
            || self.suggest_accept != preferences.suggest_accept
            || self.app_profiles.as_ref() != profiles.as_ref();
        if let Some(current) = self.keymap_preset {
            if current != preferences.keymap_preset {
                self.keymap =
                    KeyMap::preset(preferences.keymap_preset).map_err(NewError::KeyMap)?;
                self.keymap_preset = Some(preferences.keymap_preset);
            }
        }
        self.normalizer = preferences.normalizer;
        self.input_method = preferences.input_method;
        self.default_mode = preferences.default_mode;
        self.conversion_method = preferences.conversion_method;
        self.space_width = preferences.space_width;
        self.shift_space_behavior = preferences.shift_space_behavior;
        self.neural_reranker_scope = preferences.neural_reranker_scope;
        self.prediction_enabled = preferences.prediction_enabled;
        self.suggest_accept = preferences.suggest_accept;
        self.association_enabled = preferences.association_enabled;
        self.app_profiles = profiles.clone();
        // The server applies its immutable snapshot before every request,
        // including OnTestKeyDown's ProbeKey and the following real SendKey.
        // Clearing unconditionally here made each prediction navigation key
        // forget its predecessor and re-focus candidate zero forever. Preserve
        // the cache for an identical snapshot; a changed prediction policy (or
        // `set_prediction` replacing the worker) still invalidates it.
        if prediction_policy_changed {
            self.prediction_cache.clear();
        }
        self.sessions
            .apply_runtime_preferences(preferences, &profiles, keymap_changed);
        Ok(())
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
            Request::ResetDocumentContext { session } => self.reset_document_context(*session),
            Request::UndoCommit { session, outcome } => {
                self.undo_commit_outcome(*session, *outcome)
            }
            Request::ClearLearning => self.clear_learning(),
            Request::ClearInputHistory => self.clear_input_history(),
            Request::FlushInputHistory => self.flush_input_history(),
            Request::InputHistoryStats => self.input_history_stats(),
            Request::EngineTiming => Reply::Message(Response::EngineTiming {
                entries: crate::timing::snapshot(),
            }),
            Request::FaultStatus => Reply::Message(Response::FaultStatus {
                entries: crate::fault_injection::status(),
            }),
            Request::SetInputScope { session, scope } => self.set_input_scope(*session, *scope),
            Request::SetMode { session, mode } => self.set_mode(*session, *mode),
            Request::ApplyAiComposition { session, result } => {
                self.apply_ai_composition(*session, result, out)
            }
            Request::RecordAiText {
                session,
                operation,
                status,
                source,
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
                test_only,
            } => self.record_ai_text(
                *session,
                *operation,
                *status,
                source,
                result,
                model,
                provider,
                style,
                error_code,
                *latency_ms,
                *input_tokens,
                *output_tokens,
                *cached_tokens,
                *attempts,
                *test_only,
            ),
            Request::StartAiText {
                session,
                operation,
                text,
            } => self.start_ai_text(*session, *operation, text),
            Request::PollAiText { session, job } => self.poll_ai_text(*session, *job),
            Request::CancelAiText { session, job } => self.cancel_ai_text(*session, *job),
            // The renderer has no dispatcher-owned session. `server` resolves
            // this request through its shared revision-stamped UiBoard before
            // it reaches a worker; a direct dispatcher call must remain a
            // fail-closed no-op rather than deleting by a guessed surface.
            Request::DeleteHistoryCandidate { .. } => {
                Reply::Message(Response::HistoryCandidateDeleted { removed: false })
            }
            Request::QueueCandidateCommit { .. } | Request::PollCandidateCommit { .. } => {
                Reply::Message(Response::Error(ErrorCode::Internal))
            }
            Request::CommitCandidate {
                session,
                candidate_index,
                ..
            } => self.commit_candidate(*session, usize::from(*candidate_index), out),
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
            | Request::ResetDocumentContext { session }
            | Request::SetInputScope { session, .. }
            | Request::SetMode { session, .. }
            | Request::ApplyAiComposition { session, .. }
            | Request::PollCandidateCommit { session }
            | Request::CommitCandidate { session, .. }
            | Request::StartAiText { session, .. }
            | Request::DeleteSession { session } => *session,
            Request::Hello { .. }
            | Request::CreateSession { .. }
            | Request::UndoCommit { .. }
            | Request::ClearLearning
            | Request::ClearInputHistory
            | Request::FlushInputHistory
            | Request::InputHistoryStats
            | Request::EngineTiming
            | Request::FaultStatus
            | Request::RecordAiText { .. }
            | Request::PollAiText { .. }
            | Request::CancelAiText { .. }
            | Request::DeleteHistoryCandidate { .. }
            | Request::QueueCandidateCommit { .. }
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
                    input_method: self.input_method,
                    default_mode: self.default_mode,
                    conversion_method: self.conversion_method,
                    normalizer: self.normalizer,
                    space_width: self.space_width,
                    shift_space_behavior: self.shift_space_behavior,
                    prediction_enabled: self.prediction_enabled,
                    suggest_accept: self.suggest_accept,
                    association_enabled: self.association_enabled,
                    developer_mode: self.input_history.is_some(),
                    ..Preferences::default()
                };
                let resolved =
                    resolve_context_preferences(global, &self.app_profiles, process_name);
                if let Some(created) = self.sessions.get_mut(session) {
                    let history_session_id =
                        self.input_history.as_ref().map_or(session, |history| {
                            history.allocate_session_id().unwrap_or(0)
                        });
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
            self.release_composition_fence(id);
            self.ai_text.cancel_session(self.ai_text_owner, id);
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
            ai_requests: stats.ai_requests,
            ai_attempts: stats.ai_attempts,
            ai_input_tokens: stats.ai_input_tokens,
            ai_output_tokens: stats.ai_output_tokens,
            ai_cached_tokens: stats.ai_cached_tokens,
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
            session.suppress_raw_provenance();
            session.apply_input_scope(scope)
        };
        if clear_cache {
            self.prediction_cache.clear_if_session(id);
        }
        Reply::Message(Response::Ok)
    }

    /// Clears document-relative state after the frontend can no longer prove
    /// exact adjacency to the last committed run. The user's explicit input
    /// mode remains unchanged.
    fn reset_document_context(&mut self, id: SessionId) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if session.undo_pending() || session.is_composing() {
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        session.reset_document_context();
        self.prediction_cache.clear_if_session(id);
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
        session.suppress_raw_provenance();
        session.reset_carryover();
        session.mode = mode;
        Reply::Message(Response::InputMode { mode })
    }

    fn apply_ai_composition(&mut self, id: SessionId, result: &str, out: &mut OutputBuf) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if !session.host_policy().allows_ai_text() {
            // Sakura Pad text is local-only.  Do not let a renderer-owned
            // session turn an externally supplied AI result into a document
            // edit, even when its current InputScope is Normal.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        if session.undo_pending()
            || !session.is_composing()
            || !session.scope_classified()
            || session.scope() != InputScope::Normal
            || result.is_empty()
        {
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        if result.len() > MAX_COMMIT_BYTES || out.set_commit(result).is_err() {
            out.clear();
            return Reply::Message(Response::Error(ErrorCode::TooLarge));
        }
        out.consumed = true;
        session.disarm_commit_undo();
        session.reset();
        if let Some(mode) = session.take_mode_restored() {
            out.mode = Some(mode);
        }
        self.prediction_cache.clear_if_session(id);
        Reply::Output
    }

    #[allow(clippy::too_many_arguments)]
    fn record_ai_text(
        &self,
        id: SessionId,
        operation: AiTextOperation,
        status: AiTextStatus,
        source: &str,
        result: &str,
        model: &str,
        provider: &str,
        style: &str,
        error_code: &str,
        latency_ms: u64,
        input_tokens: u32,
        output_tokens: u32,
        cached_tokens: u32,
        attempts: u32,
        test_only: bool,
    ) -> Reply {
        let Some(session) = self.sessions.get(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if !session.host_policy().allows_ai_text() {
            // Recording an AI terminal result is itself a durable history
            // sink.  Reject before constructing a ScopeClass or touching the
            // history service so Normal scope cannot bypass the host policy.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        if let Some(history) = self.input_history.as_deref() {
            history.record_ai_text(
                session.history_session_id(),
                ScopeClass::from_scope(session.scope(), session.scope_classified()),
                operation,
                status,
                source,
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
                test_only,
            );
        }
        Reply::Message(Response::Ok)
    }

    fn start_ai_text(&self, id: SessionId, operation: AiTextOperation, text: &str) -> Reply {
        let Some(session) = self.sessions.get(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if !session.host_policy().allows_ai_text() {
            // Never pass Pad text to the AI worker.  This is an explicit
            // terminal response rather than a silent no-op, so callers can
            // observe that the privacy policy—not worker capacity—rejected it.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        if session.undo_pending()
            || !session.scope_classified()
            || session.scope() != InputScope::Normal
        {
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        match self.ai_text.start(self.ai_text_owner, id, operation, text) {
            Ok(job) => Reply::Message(Response::AiTextStarted { job }),
            Err(AiStartError::Duplicate | AiStartError::Capacity) => {
                Reply::Message(Response::Error(ErrorCode::Busy))
            }
            Err(AiStartError::Invalid) => Reply::Message(Response::Error(ErrorCode::TooLarge)),
            Err(AiStartError::Spawn) => Reply::Message(Response::Error(ErrorCode::Internal)),
        }
    }

    fn poll_ai_text(&self, id: SessionId, job: u64) -> Reply {
        let Some(session) = self.sessions.get(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if !session.host_policy().allows_ai_text() {
            // A private session cannot own an AI job.  Refuse a stale or
            // forged poll rather than exposing any result to the renderer.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        match self.ai_text.poll(self.ai_text_owner, id, job) {
            AiPoll::Pending => Reply::Message(Response::AiTextPending { job }),
            AiPoll::Complete(result) => Reply::Message(Response::AiTextResult {
                job,
                status: result.status,
                result: result.result,
                model: result.model,
                provider: result.provider,
                style: result.style,
                error_code: result.error_code,
                latency_ms: result.latency_ms,
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
                cached_tokens: result.cached_tokens,
                attempts: result.attempts,
            }),
            AiPoll::Missing => Reply::Message(Response::Error(ErrorCode::Malformed)),
        }
    }

    fn cancel_ai_text(&self, id: SessionId, job: u64) -> Reply {
        let Some(session) = self.sessions.get(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        if !session.host_policy().allows_ai_text() {
            // No private-session job may be started. Keep this terminal and
            // observable instead of allowing a renderer request to mutate
            // the shared AI service's job state.
            return Reply::Message(Response::Error(ErrorCode::Busy));
        }
        if self.ai_text.cancel(self.ai_text_owner, id, job) {
            Reply::Message(Response::Ok)
        } else {
            Reply::Message(Response::Error(ErrorCode::Malformed))
        }
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
        let reply = {
            let (suppress_idle_space, absorb_teardown_space) = {
                let Some(session) = self.sessions.get(id) else {
                    return Reply::Message(Response::Error(ErrorCode::UnknownSession));
                };
                if session.undo_pending() {
                    return Reply::Message(Response::Error(ErrorCode::Busy));
                }
                let suppress = self.suppress_idle_space_for(id, session.process_name());
                // Already suppressed by a live claim: leave the latch armed
                // for the teardown it was armed for.
                let absorb = !suppress && self.teardown_absorbs_idle_space(session, key);
                (suppress, absorb)
            };
            if matches!(key.code, KeyCode::Space | KeyCode::Henkan)
                && !key.modifiers.ctrl()
                && !key.modifiers.alt()
            {
                debug_trace::emit(sakura_ipc::debug_trace::TraceEvent {
                    component: "engine",
                    instance: id,
                    event: "idle_fence",
                    decision: if suppress_idle_space {
                        "absorb"
                    } else if absorb_teardown_space {
                        "absorb_teardown"
                    } else {
                        "open"
                    },
                    k0: key.code as u64,
                    k1: u64::from(key.test_only),
                    k2: u64::from(absorb_teardown_space),
                    k3: 0,
                });
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
                neural_reranker_scope: self.neural_reranker_scope,
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
                    services.learning,
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
            // The dictionary, learning and prediction services are resolved
            // and the session is still untouched. A key delayed here is one
            // the engine could still decline to apply.
            crate::fault_injection::delay(FaultPoint::DuringConversion);
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
                    suppress_idle_space,
                    absorb_teardown_space,
                },
            ) {
                Ok(()) => {
                    // The session has advanced. Nothing has been rendered,
                    // encoded or sent, so a client that times out from here on
                    // has abandoned a key the engine has already applied —
                    // the one ordering in which a lost keystroke and a
                    // duplicated one are both possible.
                    crate::fault_injection::delay(FaultPoint::AfterMutation);
                    if let Some(restored) = session.take_mode_restored() {
                        out.mode.get_or_insert(restored);
                    }
                    schedule_long_conversion(id, session, &services);
                    if let Some(history) = services
                        .input_history
                        .filter(|_| session.host_policy().allows_persistence())
                    {
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
                    if session.undo_pending() {
                        let _ = session.reject_undo_commit();
                    }
                    out.clear();
                    if let Some(history) = services
                        .input_history
                        .filter(|_| session.host_policy().allows_persistence())
                    {
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
        };
        self.sync_composition_fence(id);
        reply
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
                input_method: self.input_method,
                default_mode: self.default_mode,
                conversion_method: self.conversion_method,
                normalizer: self.normalizer,
                space_width: self.space_width,
                shift_space_behavior: self.shift_space_behavior,
                prediction_enabled: self.prediction_enabled,
                suggest_accept: self.suggest_accept,
                developer_mode: self.input_history.is_some(),
                ..Preferences::default()
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
            neural_reranker_scope: NeuralRerankerScope::Off,
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
        let suppress_idle_space = self.suppress_idle_space_for(id, probe.process_name());
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
                suppress_idle_space,
                // Probe answers what a key *would* do without applying it, so
                // it must not spend the one absorption a teardown is owed.
                absorb_teardown_space: false,
            },
        );
        Reply::Output
    }

    fn commit(&mut self, id: SessionId, out: &mut OutputBuf) -> Reply {
        let reply = {
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
        };
        self.sync_composition_fence(id);
        reply
    }

    fn commit_candidate(&mut self, id: SessionId, index: usize, out: &mut OutputBuf) -> Reply {
        let reply = {
            let Some(session) = self.sessions.get_mut(id) else {
                return Reply::Message(Response::Error(ErrorCode::UnknownSession));
            };
            if session.undo_pending()
                || !session.scope_classified()
                || session.scope() != InputScope::Normal
            {
                return Reply::Message(Response::Error(ErrorCode::Busy));
            }
            out.consumed = true;
            let normalizer = session.normalizer;
            let result = if session.suggestions_visible && !session.converting {
                let cache = PredictionCacheWork::Probe {
                    cache: &self.prediction_cache,
                    stale: false,
                };
                commit_suggestion_at(
                    id,
                    session,
                    index,
                    &normalizer,
                    self.learning.as_deref(),
                    self.input_history.as_deref(),
                    ExecutionPolicy::Apply,
                    &cache,
                    &mut self.scratch,
                    out,
                )
                .map(|committed| committed.then_some(()))
            } else if session.converting {
                commit_numbered_candidate(
                    session,
                    &self.table,
                    &normalizer,
                    self.conversion.as_deref(),
                    self.learning.as_deref(),
                    self.input_history.as_deref(),
                    ExecutionPolicy::Apply,
                    &mut self.scratch,
                    index % CANDIDATE_PAGE_SIZE,
                    out,
                )
                .map(|()| (!out.beep).then_some(()))
            } else {
                Ok(None)
            };
            match result {
                Ok(Some(())) => {
                    self.prediction_cache.clear_if_session(id);
                    Reply::Output
                }
                Ok(None) => {
                    out.clear();
                    Reply::Message(Response::Error(ErrorCode::Malformed))
                }
                Err(Overflow) => {
                    out.clear();
                    Reply::Message(Response::Error(ErrorCode::TooLarge))
                }
            }
        };
        self.sync_composition_fence(id);
        reply
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
                // `build_reconversion` stages the pre-request mode before it
                // switches to Hiragana, and `reset` is where a staged mode is
                // put back — so the failure path needs no restore of its own.
                session.reset();
                session.take_mode_restored();
                out.clear();
                Reply::Message(Response::Error(code))
            }
        }
    }

    fn revert(&mut self, id: SessionId) -> Reply {
        let restored = {
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
            session.take_mode_restored()
        };
        self.prediction_cache.clear_if_session(id);
        self.sync_composition_fence(id);
        // Reverting a reconversion is a terminal path like any other, so the
        // reset above has already put the user's mode back. This reply carries
        // no `Output`, so the mode has to ride out on the message itself or
        // the host would keep showing the Hiragana reconversion imposed.
        match restored {
            Some(mode) => Reply::Message(Response::InputMode { mode }),
            None => Reply::Message(Response::Ok),
        }
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
    neural_reranker_scope: NeuralRerankerScope,
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

    /// Probe answers whether the host may see the key. Dictionary conversion
    /// belongs to Apply so OnTestKeyDown cannot spend the 50 ms budget.
    const fn allows_dictionary_conversion(self) -> bool {
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
    /// Absorb idle Space when a peer connection of the same host is converting.
    suppress_idle_space: bool,
    /// Absorb this one idle Space because a live composition was torn down
    /// and its reading must not become a document space (#102).
    absorb_teardown_space: bool,
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

fn conversion_options(
    session: &Session,
    initial_right_id: u16,
    learning: Option<&LearningService>,
) -> ConversionOptions {
    let mut options = ConversionOptions {
        method: session.conversion_method,
        initial_right_id: if session.association_enabled {
            initial_right_id
        } else {
            0
        },
        input_support: session.input_support,
        // Read off the session rather than the dispatcher, so a per-app
        // profile's notation style reaches the converter the same way its
        // width policy already reaches the choke point (Issue #99).
        punctuation: session.normalizer.punctuation,
        skip_input_repair: scope_is_sensitive(session.scope)
            || !session.scope_classified
            || learning
                .is_some_and(|service| service.is_repair_suppressed(session.preedit.as_str())),
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

fn commit_repair_readings_for(
    reading: &str,
    learning: Option<&LearningService>,
    options: ConversionOptions,
) -> Vec<FixedStr<MAX_PREEDIT_BYTES>> {
    if options.skip_input_repair || !options.input_support.commit_based {
        return Vec::new();
    }
    let Some(learning) = learning else {
        return Vec::new();
    };
    learning.collect_commit_repair_readings(reading, options.input_support)
}

#[cfg(test)]
std::thread_local! {
    static TEST_CONVERSION_LOOKUPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_conversion_lookup_for_test() {
    TEST_CONVERSION_LOOKUPS.with(|count| count.set(count.get().saturating_add(1)));
}

/// Returns and resets the current test thread's calls into `ConversionService`.
#[cfg(all(test, feature = "dev-fixtures"))]
pub(crate) fn take_conversion_lookup_count_for_test() -> u64 {
    TEST_CONVERSION_LOOKUPS.with(|count| count.replace(0))
}

fn with_session_conversion_input<R>(
    conversion: &ConversionService,
    learning: Option<&LearningService>,
    input: ConversionInput<'_>,
    options: ConversionOptions,
    bridge: Option<CrossCommitBridge<'_>>,
    consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
) -> Result<R, crate::dictionary::ConvertFailure> {
    let hints = if input.literal_policy == LiteralPolicy::Ranked {
        commit_repair_readings_for(input.lookup_reading, learning, options)
    } else {
        Vec::new()
    };
    let hint_refs: Vec<&str> = hints.iter().map(FixedStr::as_str).collect();
    #[cfg(test)]
    record_conversion_lookup_for_test();
    if input.literal_policy == LiteralPolicy::Ranked {
        conversion.with_conversion_input_bridge_hints(input, options, &hint_refs, bridge, consume)
    } else {
        conversion.with_conversion_input_bridge_hints(input, options, &hint_refs, None, consume)
    }
}

/// Builds a classified conversion input from the session's staged exact
/// surface. The caller must pass the reading slice that the selected segment
/// actually owns; the class/policy remains the composition-level decision.
fn session_conversion_input<'a>(
    session: &'a Session,
    lookup_reading: &'a str,
) -> ConversionInput<'a> {
    let exact_surface = if session.conversion_exact_surface().is_empty() {
        lookup_reading
    } else {
        session.conversion_exact_surface()
    };
    if session.literal_policy() == LiteralPolicy::ExactOnly
        && !session.staged_exact_surface_matches_current()
    {
        // A caret/edit invalidates the staged raw snapshot. Keep an unresolved
        // Latin reading exact-only when it is still valid, but do not feed a
        // stale pre-edit surface into the core validator after the text has
        // changed.
        if lookup_reading
            .bytes()
            .any(|byte| byte.is_ascii_alphabetic())
        {
            return ConversionInput::new(
                lookup_reading,
                lookup_reading,
                ConversionInputClass::MixedUnresolvedLatin,
                LiteralPolicy::ExactOnly,
            );
        }
        return ConversionInput::ordinary(lookup_reading);
    }
    if session.literal_policy() == LiteralPolicy::ExactTop1
        && !session.staged_exact_surface_matches_current()
    {
        if is_opaque_ascii_identifier(lookup_reading) {
            return ConversionInput::new(
                lookup_reading,
                lookup_reading,
                ConversionInputClass::OpaqueAsciiIdentifier,
                LiteralPolicy::ExactTop1,
            );
        }
        return ConversionInput::ordinary(lookup_reading);
    }
    ConversionInput::new(
        lookup_reading,
        exact_surface,
        session.conversion_input_class(),
        session.literal_policy(),
    )
}

/// Input-aware one-slot conversion. The dictionary service keeps the slot
/// through both direct and corrected passes; this helper never recursively
/// calls a second `ConversionService` lookup.
fn with_session_raw_conversion<R>(
    conversion: &ConversionService,
    learning: Option<&LearningService>,
    input: ConversionInput<'_>,
    plans: &[RawRepairPlan],
    options: ConversionOptions,
    bridge: Option<CrossCommitBridge<'_>>,
    consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
) -> Result<R, crate::dictionary::ConvertFailure> {
    // A normal conversion has no corrected pass to run.  Keep it on the
    // ordinary one-slot path instead of entering the raw-repair multipass
    // frame merely to perform its direct lookup.  Besides avoiding needless
    // arena copies, this is important for the fixed-size server worker stack:
    // the raw helper is reserved for requests that actually admitted a plan.
    if plans.is_empty() {
        return with_session_conversion_input(
            conversion, learning, input, options, bridge, consume,
        );
    }
    let hints = if input.literal_policy == LiteralPolicy::Ranked {
        commit_repair_readings_for(input.lookup_reading, learning, options)
    } else {
        Vec::new()
    };
    let hint_refs: Vec<&str> = hints.iter().map(FixedStr::as_str).collect();
    #[cfg(test)]
    record_conversion_lookup_for_test();
    conversion.with_raw_repair_conversion_input_hints(input, plans, options, &hint_refs, consume)
}

fn with_session_candidates_input<R>(
    conversion: &ConversionService,
    learning: Option<&LearningService>,
    input: ConversionInput<'_>,
    options: ConversionOptions,
    consume: impl FnOnce(&[ConversionCandidate]) -> R,
) -> Result<R, crate::dictionary::ConvertFailure> {
    with_session_conversion_input(
        conversion,
        learning,
        input,
        options,
        None,
        |candidates, _| consume(candidates),
    )
}

fn with_session_candidates<R>(
    conversion: &ConversionService,
    learning: Option<&LearningService>,
    reading: &str,
    options: ConversionOptions,
    consume: impl FnOnce(&[ConversionCandidate]) -> R,
) -> Result<R, crate::dictionary::ConvertFailure> {
    with_session_candidates_input(
        conversion,
        learning,
        ConversionInput::ordinary(reading),
        options,
        consume,
    )
}

#[cfg(test)]
mod associative_conversion_setting_tests {
    use super::*;

    #[test]
    fn association_setting_controls_the_previous_connection_class() {
        let mut session = Session::new("association-test.exe");
        session.association_enabled = true;
        assert_eq!(conversion_options(&session, 7, None).initial_right_id, 7);

        session.association_enabled = false;
        assert_eq!(conversion_options(&session, 7, None).initial_right_id, 0);
    }
}

fn schedule_long_conversion(session_id: SessionId, session: &Session, services: &KeyServices<'_>) {
    let Some(long_conversion) = services.long_conversion else {
        return;
    };
    if !session.host_policy().allows_local_worker() {
        // Keep this admission check separate from AI/network policy. The
        // shipped local worker receives only its bounded candidate snapshot;
        // a future host policy may still choose to disable that local path.
        return;
    }
    let allow_short_reading = match services.neural_reranker_scope {
        NeuralRerankerScope::Off => return,
        NeuralRerankerScope::LongTextOnly => false,
        NeuralRerankerScope::AllNormalConversions => true,
    };
    if !session.scope_classified()
        || session.scope() != InputScope::Normal
        || session.converting
        || session.shifted_ascii
        || session.preedit.is_empty()
        || session.cross_commit_bridge().is_some()
        || usize::from(session.cursor) != session.preedit.as_str().chars().count()
    {
        return;
    }
    let options = conversion_options(session, session.carry_right_id(), services.learning);
    let _ = long_conversion.schedule(
        services.long_conversion_owner,
        session_id,
        session.prediction_generation,
        session.preedit.as_str(),
        options,
        allow_short_reading,
    );
}

fn preferred_candidate_index(
    candidates: &[ConversionCandidate],
    reading: &str,
    requested: i16,
    cached: Option<(u64, u16)>,
    learned: LearningPreference,
    direct_limit: usize,
) -> usize {
    if candidates.is_empty() {
        return 0;
    }
    if requested == 0 {
        let direct_limit = direct_limit.min(candidates.len());
        let direct = &candidates[..direct_limit];
        if let Some(index) = learned
            .exact
            .filter(|index| *index < direct.len())
            .filter(|index| candidate_preference_matches_context(direct, *index))
            .filter(|index| direct[*index].text() != reading)
        {
            return index;
        }
        if let Some((surface_hash, surface_len)) = cached {
            if let Some(index) = direct.iter().enumerate().position(|(index, candidate)| {
                u16::try_from(candidate.text().len()).ok() == Some(surface_len)
                    && text_hash(candidate.text()) == surface_hash
                    && candidate.text() != reading
                    && candidate_preference_matches_context(direct, index)
            }) {
                return index;
            }
        }
        if let Some(index) = learned
            .general
            .filter(|index| *index < direct.len())
            .filter(|index| candidate_preference_matches_context(direct, *index))
            .filter(|index| direct[*index].text() != reading)
        {
            return index;
        }
        if let Some(index) = direct
            .iter()
            .position(|candidate| candidate.text() != reading)
        {
            return index;
        }
    }
    requested.rem_euclid(candidates.len() as i16) as usize
}

fn has_authoritative_candidate_preference(
    candidates: &[ConversionCandidate],
    reading: &str,
    requested: i16,
    cached: Option<(u64, u16)>,
    learned: LearningPreference,
    direct_limit: usize,
) -> bool {
    if requested != 0 {
        return true;
    }
    let direct_limit = direct_limit.min(candidates.len());
    let direct = &candidates[..direct_limit];
    if learned.exact.is_some_and(|index| {
        index < direct.len()
            && candidate_preference_matches_context(direct, index)
            && direct[index].text() != reading
    }) || learned.general.is_some_and(|index| {
        index < direct.len()
            && candidate_preference_matches_context(direct, index)
            && direct[index].text() != reading
    }) {
        return true;
    }
    cached.is_some_and(|(surface_hash, surface_len)| {
        direct.iter().enumerate().any(|(index, candidate)| {
            u16::try_from(candidate.text().len()).ok() == Some(surface_len)
                && text_hash(candidate.text()) == surface_hash
                && candidate.text() != reading
                && candidate_preference_matches_context(direct, index)
        })
    })
}

/// A preference learned or cached without the retained lexical tail must not
/// jump from outside a successfully reanalysed cross-commit continuation into
/// the selected position. This is intentionally evidence-based rather than a
/// surface exception: when no direct candidate received bridge evidence, all
/// ordinary preference behavior remains unchanged.
fn candidate_preference_matches_context(
    direct: &[ConversionCandidate],
    candidate_index: usize,
) -> bool {
    !direct
        .iter()
        .any(ConversionCandidate::was_cross_commit_rescored)
        || direct
            .get(candidate_index)
            .is_some_and(ConversionCandidate::was_cross_commit_rescored)
}

fn is_opaque_ascii_identifier(raw: &str) -> bool {
    !raw.is_empty()
        && raw.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && raw.bytes().any(|byte| byte.is_ascii_alphabetic())
        && raw.bytes().any(|byte| byte.is_ascii_digit())
}

fn classify_conversion_input(session: &Session) -> ConversionInputClass {
    if session.shifted_ascii && is_opaque_ascii_identifier(session.raw_input.as_str()) {
        ConversionInputClass::OpaqueAsciiIdentifier
    } else if !session.shifted_ascii
        && session
            .preedit
            .as_str()
            .bytes()
            .any(|byte| byte.is_ascii_alphabetic())
    {
        ConversionInputClass::MixedUnresolvedLatin
    } else {
        ConversionInputClass::Ordinary
    }
}

const fn literal_policy_for(class: ConversionInputClass) -> LiteralPolicy {
    match class {
        ConversionInputClass::Ordinary => LiteralPolicy::Ranked,
        ConversionInputClass::OpaqueAsciiIdentifier => LiteralPolicy::ExactTop1,
        ConversionInputClass::MixedUnresolvedLatin => LiteralPolicy::ExactOnly,
    }
}

/// Converts the table's bounded local completion into a forward-only UTF-8
/// correction map. Longest common prefix/suffix are exact character
/// boundaries; a middle insertion/deletion is rejected because Phase 1 maps
/// only one-to-one replacement runs. The returned vector is transient scratch;
/// no correction trace is retained in [`Session`].
fn build_correction_runs(original: &str, corrected: &str) -> Option<Vec<CorrectionRun>> {
    if original.is_empty() || corrected.is_empty() || original == corrected {
        return None;
    }
    let original_chars: Vec<(usize, char)> = original.char_indices().collect();
    let corrected_chars: Vec<(usize, char)> = corrected.char_indices().collect();
    let mut prefix = 0usize;
    while prefix < original_chars.len()
        && prefix < corrected_chars.len()
        && original_chars[prefix].1 == corrected_chars[prefix].1
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < original_chars.len().saturating_sub(prefix)
        && suffix < corrected_chars.len().saturating_sub(prefix)
        && original_chars[original_chars.len() - 1 - suffix].1
            == corrected_chars[corrected_chars.len() - 1 - suffix].1
    {
        suffix += 1;
    }
    let original_prefix_end = original_chars
        .get(prefix)
        .map_or(original.len(), |(at, _)| *at);
    let corrected_prefix_end = corrected_chars
        .get(prefix)
        .map_or(corrected.len(), |(at, _)| *at);
    let original_suffix_start = if suffix == 0 {
        original.len()
    } else {
        original_chars[original_chars.len() - suffix].0
    };
    let corrected_suffix_start = if suffix == 0 {
        corrected.len()
    } else {
        corrected_chars[corrected_chars.len() - suffix].0
    };
    if original_prefix_end >= original_suffix_start
        || corrected_prefix_end >= corrected_suffix_start
    {
        return None;
    }
    let mut runs = Vec::with_capacity(3);
    if prefix > 0 {
        runs.push(CorrectionRun::equal(
            0,
            u16::try_from(corrected_prefix_end).ok()?,
            0,
            u16::try_from(original_prefix_end).ok()?,
        ));
    }
    runs.push(CorrectionRun::replace(
        u16::try_from(corrected_prefix_end).ok()?,
        u16::try_from(corrected_suffix_start).ok()?,
        u16::try_from(original_prefix_end).ok()?,
        u16::try_from(original_suffix_start).ok()?,
    ));
    if suffix > 0 {
        runs.push(CorrectionRun::equal(
            u16::try_from(corrected_suffix_start).ok()?,
            u16::try_from(corrected.len()).ok()?,
            u16::try_from(original_suffix_start).ok()?,
            u16::try_from(original.len()).ok()?,
        ));
    }
    Some(runs)
}

/// Builds the bounded local-completion plans for the exact raw/preedit
/// snapshot currently in `session`. This is intentionally a pure, transient
/// operation: conversion/render/commit callers regenerate it from the
/// admission stamp instead of keeping user text or correction traces in the
/// long-lived Session clone.
fn build_local_completion_plans(session: &Session, table: &Table) -> Vec<RawRepairPlan> {
    let raw = session.raw_input.as_str();
    let observed = session.preedit.as_str();
    if session.raw_provenance() != RawProvenanceState::AppendOnly
        || !session.raw_repair_candidate_possible()
        || session.scope() != InputScope::Normal
        || !session.scope_classified()
        || session.input_method != InputMethod::Romaji
        || !session.romaji.is_empty()
        || raw.is_empty()
        || observed.is_empty()
    {
        return Vec::new();
    }
    let Ok(completions) = table.plan_local_completions(raw, observed) else {
        return Vec::new();
    };
    let mut plans = Vec::with_capacity(completions.len());
    for (plan_id, completion) in completions.iter().enumerate() {
        let corrected = completion.corrected_reading.as_str();
        let Some(runs) = build_correction_runs(observed, corrected) else {
            continue;
        };
        let Ok(map) = CorrectionMap::new(observed, corrected, &runs) else {
            continue;
        };
        let Ok(plan) = RawRepairPlan::new(
            u8::try_from(plan_id).unwrap_or(u8::MAX),
            corrected,
            map,
            RepairTier::LocalCompletion,
        ) else {
            continue;
        };
        plans.push(plan);
    }
    plans
}

fn project_repair_segment(
    plans: &[RawRepairPlan],
    original: &str,
    candidate: &ConversionCandidate,
    corrected_start: u16,
    corrected_end: u16,
) -> Option<(u16, u16)> {
    let CandidateOrigin::RawRepair { plan_id, .. } = candidate.origin() else {
        return Some((corrected_start, corrected_end));
    };
    let plan = plans.iter().find(|plan| plan.plan_id() == plan_id)?;
    plan.map()
        .validate_for_readings(original, plan.corrected_reading())
        .ok()?;
    plan.map()
        .project_corrected_range(corrected_start, corrected_end)
}

#[derive(Clone, Copy, Default)]
struct CommitSegmentMeta {
    right_id: u16,
    it_words: u8,
    total_words: u8,
    synthetic_exact: bool,
    raw_repair: bool,
    bridge_tail: Option<CommitBridgeTail>,
}

fn candidate_meta(
    conversion: &ConversionService,
    candidate: &ConversionCandidate,
) -> CommitSegmentMeta {
    let segments = candidate.segments();
    // Per-word counters, not segment counts: bunsetsu fusion merges an IT
    // content word and its non-IT ancillary into one OR-flagged segment,
    // which would otherwise shrink the denominator of the domain ratio
    // while keeping its numerator.
    let (it_words, total_words) = segments.iter().fold((0u8, 0u8), |(it, total), segment| {
        (
            it.saturating_add(segment.it_word_count),
            total.saturating_add(segment.word_count),
        )
    });
    CommitSegmentMeta {
        right_id: segments.last().map_or(0, |segment| segment.right_id),
        it_words,
        total_words,
        synthetic_exact: candidate.is_synthetic_exact(),
        raw_repair: candidate.origin() != CandidateOrigin::Direct,
        bridge_tail: conversion.cross_commit_tail(candidate),
    }
}

fn session_cross_commit_bridge(
    reading: &str,
    surface: &str,
    tail: CommitBridgeTail,
) -> Option<SessionCrossCommitBridge> {
    let tail_reading = reading.get(usize::from(tail.reading_start)..)?;
    let tail_surface = surface.get(usize::from(tail.text_start)..)?;
    SessionCrossCommitBridge::new(
        tail_reading,
        tail_surface,
        tail.prefix_right_id.raw(),
        tail.prefix_cost,
    )
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
    if !session.host_policy().allows_persistence() {
        // Host policy is independent of InputScope. A renderer-owned Pad is
        // normally classified as Normal so Japanese conversion remains
        // usable, but neither its commit history nor learning record may be
        // admitted to a durable sink.
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
        suppress_idle_space,
        absorb_teardown_space,
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
    // The bridge describes text immediately to the left of the host caret.
    // An unclaimed idle named key or application shortcut can move that caret
    // or edit the document outside the engine, while idle Space inserts a
    // hard phrase boundary. Character input is the one unclaimed idle path
    // that deliberately starts the adjacent next composition.
    if state == State::Idle
        && action.is_none()
        && (key.ch.is_none()
            || key.code == KeyCode::Space
            || key.modifiers.ctrl()
            || key.modifiers.alt())
    {
        session.clear_cross_commit_bridge();
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
    let idle_space_commit = action.is_none()
        && state == State::Idle
        && key.code == KeyCode::Space
        && !key.modifiers.ctrl()
        && !key.modifiers.alt();
    let prediction_direction = match action {
        Some(Action::PredictNext) => Some(1),
        Some(Action::PredictPrev) => Some(-1),
        _ => None,
    };
    if !matches!(action, Some(Action::UndoCommit)) {
        session.disarm_commit_undo();
    }
    if suppress_idle_space && state == State::Idle && !key.modifiers.ctrl() && !key.modifiers.alt()
    {
        // Dual TSF / Electron: the idle peer is not an IME. Eat every key so a
        // second pipe cannot start a second composition or insert U+3000.
        out.consumed = true;
        return Ok(());
    }
    if idle_space_commit && absorb_teardown_space {
        // The composition this session replaced was torn down with its
        // reading still live, so this Space is the one the user aimed at that
        // reading. Eat it rather than commit U+3000 into the document (#102).
        // Only this keystroke: the latch is already spent, so the next Space
        // inserts normally.
        out.consumed = true;
        return Ok(());
    }
    match action {
        Some(action) => apply_action(
            session_id,
            session,
            action,
            key,
            services,
            policy,
            &mut prediction_cache,
            scratch,
            out,
        )?,

        None if idle_space_commit => {
            let is_full = session.idle_space_is_full(key.modifiers.shift());
            out.set_commit(if is_full { "　" } else { " " })?;
            out.consumed = true;
        }

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
            feed_input_character(
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
                feed_input_character(session, services.table, ch, key.modifiers.shift(), scratch)?;
            }
            None => out.consumed = false,
        },
    }

    // The temporary English composition is a property of a composition, so it
    // ends with one however the composition ended -- committed, cancelled, or
    // simply erased character by character (#51). Restored here, once, rather
    // than in each handler that can empty the buffers, and before prediction
    // and rendering so both see the same session the next key will.
    session.release_shifted_ascii_without_composition();

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
        services.learning,
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
    let fresh_composition =
        session.preedit.is_empty() && session.raw_input.is_empty() && session.romaji.is_empty();
    let append_only_tail = !session.shifted_ascii
        && session.input_method == InputMethod::Romaji
        && character.is_ascii()
        && !shifted
        && usize::from(session.cursor) == session.preedit.as_str().chars().count()
        && next_raw_boundary(table, session) == session.raw_input.len()
        && !(character == '.' && ascii_digit_immediately_before_cursor(session, table));
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
    if session.shifted_ascii && !shifted_ascii {
        session.cursor =
            u16::try_from(session.preedit.as_str().chars().count()).unwrap_or(u16::MAX);
    }

    if shifted_ascii && character.is_ascii() {
        // Visible English text is `raw_input`. Insert at the raw caret, then
        // rebuild kana provenance from the whole buffer so a later convert
        // still has a faithful reading. Using the romaji caret (`next_raw_boundary`)
        // here is what turned "AIUEO, Left, Backspace, O" into AIUOEO: the
        // displayed caret stayed at the end while the kana cursor moved.
        let mut raw_input = session.raw_input.clone();
        let at = raw_input
            .byte_index(usize::from(session.cursor))
            .unwrap_or(raw_input.len());
        let mut buf = [0u8; 4];
        raw_input.insert_str(at, character.encode_utf8(&mut buf))?;
        let cursor = session
            .cursor
            .saturating_add(1)
            .min(u16::try_from(raw_input.as_str().chars().count()).unwrap_or(u16::MAX));
        session.shifted_ascii = true;
        session.raw_input = raw_input;
        session.cursor = cursor;
        resync_shifted_ascii_from_raw(session, table, scratch)?;
        session.suppress_raw_provenance();
        return Ok(());
    }

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
    let mut contextual_swap = false;
    if !scratch.is_empty() {
        let previous = if cursor == 0 {
            None
        } else {
            preedit.as_str().chars().nth(usize::from(cursor) - 1)
        };
        let mut emitted = FixedStr::<MAX_PREEDIT_BYTES>::new();
        for character in scratch.as_str().chars() {
            let swapped = if !scope_is_sensitive(session.scope) && session.scope_classified {
                contextual_punctuation_swap(previous, character, session.input_support)
                    .unwrap_or(character)
            } else {
                character
            };
            contextual_swap |= swapped != character;
            emitted.push(swapped)?;
        }
        let at = preedit
            .byte_index(usize::from(cursor))
            .unwrap_or(preedit.len());
        preedit.insert_str(at, emitted.as_str())?;
        cursor = cursor
            .saturating_add(u16::try_from(emitted.as_str().chars().count()).unwrap_or(u16::MAX));
    }

    let (replay_compatible, raw_repair_candidate_possible) = if append_only_tail && !contextual_swap
    {
        table
            .replay(raw_input.as_str())
            .map_or((false, false), |trace| {
                let compatible = trace.output() == preedit.as_str()
                    && trace.pending() == romaji.pending()
                    && trace.carry_overlap() == romaji.carry_overlap();
                let repair_candidate_possible =
                    compatible && trace.raw_passthrough_count() == 1 && trace.pending().is_empty();
                (compatible, repair_candidate_possible)
            })
    } else {
        (false, false)
    };
    session.shifted_ascii = shifted_ascii;
    session.raw_input = raw_input;
    session.romaji = romaji;
    session.preedit = preedit;
    session.cursor = cursor;
    session.set_raw_repair_candidate_possible(raw_repair_candidate_possible);
    if replay_compatible {
        session.mark_append_only_raw_feed(fresh_composition);
    } else {
        session.suppress_raw_provenance();
    }
    session.invalidate_prediction();
    Ok(())
}

/// Feeds one ordinary character according to the configured input method.
///
/// The TSF translator already supplies the character produced by the active
/// Windows keyboard layout. In Kana mode that character is accepted directly;
/// no synthetic JIS mapping is invented in the engine. If a setting changes
/// while a romaji sequence is still pending, the existing sequence wins until
/// it is committed or cancelled, preserving the raw-input provenance
/// invariants of the romaji path.
fn feed_input_character(
    session: &mut Session,
    table: &Table,
    character: char,
    shifted: bool,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
) -> Result<(), Overflow> {
    let direct = session.input_method == InputMethod::Kana
        && matches!(
            session.mode,
            Mode::Hiragana | Mode::Katakana | Mode::HalfKatakana
        )
        && session.romaji.is_empty()
        && session.raw_input.is_empty()
        && !session.shifted_ascii;
    if direct {
        feed_kana_character(session, character)
    } else {
        feed_character(session, table, character, shifted, scratch)
    }
}

fn feed_kana_character(session: &mut Session, character: char) -> Result<(), Overflow> {
    let previous = if session.cursor == 0 {
        None
    } else {
        session
            .preedit
            .as_str()
            .chars()
            .nth(usize::from(session.cursor) - 1)
    };
    let mapped = if !scope_is_sensitive(session.scope) && session.scope_classified {
        contextual_punctuation_swap(previous, character, session.input_support).unwrap_or(character)
    } else {
        character
    };
    let mut preedit = session.preedit.clone();
    let at = preedit
        .byte_index(usize::from(session.cursor))
        .unwrap_or(preedit.len());
    let mut buf = [0u8; 4];
    preedit.insert_str(at, mapped.encode_utf8(&mut buf))?;
    session.preedit = preedit;
    session.cursor = session.cursor.saturating_add(1);
    session.suppress_raw_provenance();
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
    key: &KeyInput,
    services: &KeyServices<'_>,
    policy: ExecutionPolicy,
    prediction_cache: &mut PredictionCacheWork<'_>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    out.consumed = true;
    if let Some(offset) = action.candidate_offset() {
        return if session.converting {
            if !session.conversion_focused() && key.ch.is_some_and(|ch| ch.is_ascii_digit()) {
                // A compact conversion list is visible, but it has not yet
                // claimed the keyboard. Keep the first 1-9 keystroke as
                // literal input: accept the current conversion and feed the
                // same digit into the next composition, exactly like an
                // unbound character arriving while conversion is active.
                commit_conversion_then_feed_literal(session, services, policy, key, scratch, out)
            } else {
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
            }
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
            if policy.allows_dictionary_conversion() {
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
                    ConversionTrigger {
                        is_space: key.code == KeyCode::Space,
                    },
                    out,
                )?;
            }
        }
        Action::ConvertPrev => {
            if policy.allows_dictionary_conversion() {
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
                    ConversionTrigger {
                        is_space: key.code == KeyCode::Space,
                    },
                    out,
                )?;
            }
        }
        Action::CandidateNext => {
            if session.expand_conversion() {
                let index = session.focused_segment();
                session.clear_segment_transform(index);
                session.clear_selected_raw_repair();
                let next = session.segment_selection(index).saturating_add(1);
                session.set_segment_selection(index, next);
            } else {
                out.beep = true;
            }
        }
        Action::CandidatePrev => {
            if session.expand_conversion() {
                let index = session.focused_segment();
                session.clear_segment_transform(index);
                session.clear_selected_raw_repair();
                let next = session.segment_selection(index).saturating_sub(1);
                session.set_segment_selection(index, next);
            } else {
                out.beep = true;
            }
        }
        Action::CandidatePageDown => {
            if session.expand_conversion() {
                let index = session.focused_segment();
                session.clear_segment_transform(index);
                session.clear_selected_raw_repair();
                let next = session
                    .segment_selection(index)
                    .saturating_add(CANDIDATE_PAGE_SIZE as i16);
                session.set_segment_selection(index, next);
            } else {
                out.beep = true;
            }
        }
        Action::CandidatePageUp => {
            if session.expand_conversion() {
                let index = session.focused_segment();
                session.clear_segment_transform(index);
                session.clear_selected_raw_repair();
                let next = session
                    .segment_selection(index)
                    .saturating_sub(CANDIDATE_PAGE_SIZE as i16);
                session.set_segment_selection(index, next);
            } else {
                out.beep = true;
            }
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
                let candidates = prediction_cache
                    .candidates(session_id, session.prediction_generation)
                    .unwrap_or_default();
                let direction = if action == Action::PredictPrev { -1 } else { 1 };
                let focused = focus_prediction_candidate(session, candidates, direction);
                if !focused {
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
        Action::SegmentPrev => {
            session.suppress_raw_provenance();
            session.focus_previous_segment();
        }
        Action::SegmentNext => {
            session.suppress_raw_provenance();
            session.focus_next_segment();
        }
        Action::SegmentHome => {
            session.suppress_raw_provenance();
            session.focus_first_segment();
        }
        Action::SegmentEnd => {
            session.suppress_raw_provenance();
            session.focus_last_segment();
        }
        Action::SegmentShrink => {
            session.suppress_raw_provenance();
            if !session.resize_focused_segment(false) {
                out.beep = true;
            } else if session.host_policy().allows_persistence() {
                if let Some(learning) = services.learning {
                    learning.suppress_repair_reading(session.preedit.as_str());
                }
            } else {
                // Segment editing remains local, but its repair-suppression
                // write is explicitly rejected for renderer-owned Pad text.
            }
        }
        Action::SegmentGrow => {
            session.suppress_raw_provenance();
            if !session.resize_focused_segment(true) {
                out.beep = true;
            } else if session.host_policy().allows_persistence() {
                if let Some(learning) = services.learning {
                    learning.suppress_repair_reading(session.preedit.as_str());
                }
            } else {
                // See SegmentShrink: local segmentation is retained while the
                // durable learning sink has an explicit rejected outcome.
            }
        }
        Action::CaretLeft => move_caret(session, services.table, scratch, CaretMove::Left)?,
        Action::CaretRight => move_caret(session, services.table, scratch, CaretMove::Right)?,
        Action::CaretHome => move_caret(session, services.table, scratch, CaretMove::Home)?,
        Action::CaretEnd => move_caret(session, services.table, scratch, CaretMove::End)?,
        Action::DeleteBack => apply_backspace(session, services.table, scratch)?,
        Action::DeleteForward => apply_delete_forward(session, services.table, scratch)?,
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
            Some(surface) => {
                if out.set_delete_before(surface.as_str()).is_err() {
                    // The host never received a delete request, which is
                    // exactly a rejection: restore the post-commit idle
                    // state and keep the record so a retry stays possible,
                    // instead of leaving the undo wedged pending forever.
                    session.reject_undo_commit();
                    return Err(Overflow);
                }
            }
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

/// Accepts the current conversion and feeds the triggering digit into a new
/// composition. This is the unfocused conversion-list counterpart to the
/// ordinary character arm in [`apply_key`]: the list is visible, but numeric
/// shortcuts do not own the keyboard until explicit candidate navigation has
/// happened.
#[allow(clippy::too_many_arguments)]
fn commit_conversion_then_feed_literal(
    session: &mut Session,
    services: &KeyServices<'_>,
    policy: ExecutionPolicy,
    key: &KeyInput,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    let Some(character) = key.ch else {
        // A candidate action is normally bound to a character trigger. Keep
        // this malformed/custom-map path terminal and recoverable rather
        // than committing a conversion without a literal continuation.
        out.beep = true;
        return Ok(());
    };
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
    feed_input_character(
        session,
        services.table,
        character,
        key.modifiers.shift(),
        scratch,
    )?;
    // This key accepted the old conversion and started a new composition;
    // the one-key undo window must not describe only the first half.
    session.disarm_commit_undo();
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
    let mut invalid_mapping = false;
    let raw_repair_plans = build_local_completion_plans(session, table);
    let raw_full = !raw_repair_plans.is_empty();
    let options = conversion_options(session, session.carry_right_id(), learning);
    let result = match conversion {
        Some(service) => {
            let reading = if raw_full {
                session.preedit.as_str()
            } else {
                &session.preedit.as_str()[range.clone()]
            };
            let full_span = raw_full || (range.start == 0 && range.end == session.preedit.len());
            let input = if full_span {
                session_conversion_input(session, reading)
            } else {
                ConversionInput::ordinary(reading)
            };
            let mut choose = |candidates: &[ConversionCandidate]| -> Result<Option<i16>, Overflow> {
                if candidates.is_empty() {
                    return Ok(None);
                }
                if raw_full
                    && candidates.iter().any(|candidate| {
                        !raw_candidate_mapping_is_valid(&raw_repair_plans, reading, candidate)
                    })
                {
                    invalid_mapping = true;
                    return Ok(None);
                }
                let projection = match project_conversion_candidates(
                    candidates,
                    "",
                    SegmentTransform::None,
                    0,
                    normalizer,
                    session.mode,
                ) {
                    Ok(projection) => projection,
                    Err(ProjectionError::SurfaceOverflow) => return Err(Overflow),
                    Err(_) => return Ok(None),
                };
                let Some(current) = projection.normalize_selection(selected) else {
                    if !projection.is_complete() {
                        return Err(Overflow);
                    }
                    return Ok(None);
                };
                let page_start = current / CANDIDATE_PAGE_SIZE * CANDIDATE_PAGE_SIZE;
                let target = page_start.saturating_add(offset);
                let Some(raw_target) = projection.raw_index(target) else {
                    if !projection.is_complete() {
                        return Err(Overflow);
                    }
                    return Ok(None);
                };
                let candidate = &candidates[raw_target];
                chosen.push_str(candidate.text())?;
                chosen_meta = candidate_meta(service, candidate);
                Ok(i16::try_from(target).ok())
            };
            let bridge = if focused == 0 {
                session.cross_commit_bridge()
            } else {
                None
            };
            if full_span {
                with_session_raw_conversion(
                    service,
                    learning,
                    input,
                    &raw_repair_plans,
                    options,
                    bridge,
                    |candidates, _| choose(candidates),
                )
            } else {
                with_session_conversion_input(
                    service,
                    learning,
                    input,
                    options,
                    bridge,
                    |candidates, _| choose(candidates),
                )
            }
        }
        None => {
            out.beep = true;
            return Ok(());
        }
    };

    if invalid_mapping {
        session.suppress_raw_provenance();
        if commit_converted_segments(
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
        )? {
            return Ok(());
        }
        out.beep = true;
        return Ok(());
    }

    match result {
        Ok(Ok(Some(_target))) => {
            if raw_full {
                let previous_transform = session.segment_transform(focused);
                session.clear_segment_transform(focused);
                match commit_candidate_surface(
                    session,
                    normalizer,
                    learning,
                    input_history,
                    policy,
                    scratch,
                    out,
                    chosen.as_str(),
                    chosen_meta,
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
                return Ok(());
            }
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
    if !session.host_policy().allows_persistence() {
        // The focused suggestion may be stale or forged. Consume the policy
        // decision here before reading any candidate identity or mutating the
        // learning store; private Pad text never reaches this sink.
        return false;
    }
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

/// Exact synthetic candidates preserve the literal surface byte-for-byte.
/// Raw-repair candidates remain ordinary dictionary surfaces for width policy;
/// explicit F6-F10 transforms still own their output path.
fn append_candidate_surface(
    candidate: &ConversionCandidate,
    raw_input: &str,
    transform: SegmentTransform,
    cycle: u8,
    normalizer: &Normalizer,
    mode: Mode,
    target: &mut FixedStr<MAX_PREEDIT_BYTES>,
) -> Result<(), Overflow> {
    if candidate.is_synthetic_exact() && transform == SegmentTransform::None {
        target.push_str(candidate.text())
    } else {
        append_segment_surface(
            candidate.text(),
            raw_input,
            transform,
            cycle,
            normalizer,
            mode,
            target,
        )
    }
}

/// Builds the one display projection used by every conversion consumer.
///
/// The callback intentionally shares [`append_candidate_surface`] with the
/// selected preedit and commit paths. A candidate is therefore deduplicated
/// on the exact string the user will see and that the host will receive after
/// the current segment transform/normalizer policy, rather than on raw
/// dictionary bytes or a guessed surface comparison.
fn project_conversion_candidates(
    candidates: &[ConversionCandidate],
    raw_input: &str,
    transform: SegmentTransform,
    cycle: u8,
    normalizer: &Normalizer,
    mode: Mode,
) -> Result<CandidateProjection, ProjectionError> {
    CandidateProjection::build_prefix(candidates.len(), |index, surface| {
        append_candidate_surface(
            &candidates[index],
            raw_input,
            transform,
            cycle,
            normalizer,
            mode,
            surface,
        )
    })
}

fn raw_candidate_mapping_is_valid(
    plans: &[RawRepairPlan],
    original: &str,
    candidate: &ConversionCandidate,
) -> bool {
    if candidate.origin() == CandidateOrigin::Direct {
        return true;
    }
    !candidate.segments().is_empty()
        && candidate.segments().iter().all(|segment| {
            project_repair_segment(
                plans,
                original,
                candidate,
                segment.reading_start,
                segment.reading_end,
            )
            .is_some()
        })
}

/// Commits one already-selected candidate surface. This is used for a raw
/// repair candidate whose corrected path may contain several dictionary
/// segments: candidate text is the authoritative full surface, while the
/// transient plan set has already validated every corrected segment boundary.
#[allow(clippy::too_many_arguments)]
fn commit_candidate_surface(
    session: &mut Session,
    normalizer: &Normalizer,
    learning: Option<&LearningService>,
    input_history: Option<&InputHistoryService>,
    policy: ExecutionPolicy,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
    text: &str,
    meta: CommitSegmentMeta,
) -> Result<bool, Overflow> {
    scratch.clear();
    if meta.synthetic_exact {
        scratch.push_str(text)?;
    } else {
        normalizer.normalize_into(text, session.mode, scratch)?;
    }
    out.set_commit(scratch.as_str())?;
    let learnable = if meta.raw_repair || meta.synthetic_exact {
        None
    } else {
        learning
    };
    record_learning(
        session,
        learnable,
        input_history,
        policy,
        scratch.as_str(),
        meta.right_id,
    );
    if meta.raw_repair || meta.synthetic_exact {
        session.record_current_commit_without_cache(
            scratch.as_str(),
            meta.right_id,
            meta.it_words,
            meta.total_words,
        );
    } else {
        session.record_current_commit(
            scratch.as_str(),
            meta.right_id,
            meta.it_words,
            meta.total_words,
        );
    }
    session.reset();
    Ok(true)
}

/// One-shot raw conversion for Enter/Commit. It keeps direct candidates in
/// their normal order, validates all repaired segment maps before exposing the
/// selected surface, and suppresses learning for repaired/exact-literal paths.
#[allow(clippy::too_many_arguments)]
fn commit_staged_raw_conversion(
    session: &mut Session,
    table: &Table,
    plans: &[RawRepairPlan],
    normalizer: &Normalizer,
    conversion: &ConversionService,
    learning: Option<&LearningService>,
    input_history: Option<&InputHistoryService>,
    policy: ExecutionPolicy,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<bool, Overflow> {
    let reading = session.preedit.as_str();
    let input = session_conversion_input(session, reading);
    let options = conversion_options(session, session.carry_right_id(), learning);
    let requested = session.segment_selection(session.focused_segment());
    let mut selected_text = FixedStr::<MAX_PREEDIT_BYTES>::new();
    let mut selected_meta = CommitSegmentMeta::default();
    let mut invalid_mapping = false;
    let mut projection_overflow = false;
    let result = with_session_raw_conversion(
        conversion,
        learning,
        input,
        plans,
        options,
        None,
        |candidates, _| {
            if candidates.is_empty() {
                return false;
            }
            if candidates
                .iter()
                .any(|candidate| !raw_candidate_mapping_is_valid(plans, reading, candidate))
            {
                invalid_mapping = true;
                return false;
            }
            let projection = match project_conversion_candidates(
                candidates,
                "",
                SegmentTransform::None,
                0,
                normalizer,
                session.mode,
            ) {
                Ok(projection) => projection,
                Err(ProjectionError::SurfaceOverflow) => {
                    projection_overflow = true;
                    return false;
                }
                Err(_) => return false,
            };
            let Some(visible_index) = projection.normalize_selection(requested) else {
                if !projection.is_complete() {
                    projection_overflow = true;
                }
                return false;
            };
            let Some(raw_index) = projection.raw_index(visible_index) else {
                if !projection.is_complete() {
                    projection_overflow = true;
                }
                return false;
            };
            let candidate = &candidates[raw_index];
            if selected_text.push_str(candidate.text()).is_err() {
                return false;
            }
            selected_meta = candidate_meta(conversion, candidate);
            true
        },
    )
    .unwrap_or(false);
    if invalid_mapping {
        // A stale or malformed correction map is a repair-only failure. Drop
        // the repair tier and retry through the ordinary direct conversion
        // path so the user's original reading remains usable.
        session.suppress_raw_provenance();
        return commit_converted_segments(
            session,
            table,
            normalizer,
            Some(conversion),
            learning,
            input_history,
            policy,
            scratch,
            out,
            None,
        );
    }
    if projection_overflow {
        return Err(Overflow);
    }
    if !result || selected_text.is_empty() {
        return Ok(false);
    }
    commit_candidate_surface(
        session,
        normalizer,
        learning,
        input_history,
        policy,
        scratch,
        out,
        selected_text.as_str(),
        selected_meta,
    )
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

    let raw_repair_plans = build_local_completion_plans(session, table);
    if candidate_override.is_none() && !raw_repair_plans.is_empty() {
        if let Some(service) = conversion {
            return commit_staged_raw_conversion(
                session,
                table,
                &raw_repair_plans,
                normalizer,
                service,
                learning,
                input_history,
                policy,
                scratch,
                out,
            );
        }
    }

    scratch.clear();
    let count = session.segment_count();
    let mut context_right_id = session.carry_right_id();
    let mut it_words = 0u8;
    let mut total_words = 0u8;
    let mut transformed = false;
    let mut synthetic_exact_committed = false;
    let mut raw_repair_committed = false;
    let mut next_cross_commit_bridge = None;
    let selected_raw_repair_at_start = session.has_selected_raw_repair();
    for index in 0..count {
        let Some(range) = session.segment_range(index) else {
            scratch.clear();
            return Ok(false);
        };
        let reading = &session.preedit.as_str()[range.clone()];
        let segment_output_start = scratch.len();
        // Each segment commits only its own share of `raw_input`, not the
        // whole composition's keystrokes -- see the identical note in
        // `render_converted_segments` (#16 finding D).
        let raw_segment = segment_raw_text(
            table,
            session.preedit.as_str(),
            session.raw_input.as_str(),
            range.clone(),
        );
        let (transform, cycle) = session.segment_transform(index);

        if transform != SegmentTransform::None {
            let transform_reading = if matches!(
                transform,
                SegmentTransform::Hiragana
                    | SegmentTransform::Katakana
                    | SegmentTransform::HalfKatakana
            ) {
                if session.segment_count() == 1 {
                    session.selected_raw_repair_full().unwrap_or(reading)
                } else {
                    session
                        .selected_raw_repair_segment(index)
                        .unwrap_or(reading)
                }
            } else {
                reading
            };
            append_segment_surface(
                transform_reading,
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
            if override_candidate.meta.synthetic_exact && transform == SegmentTransform::None {
                scratch.push_str(override_candidate.text)?;
            } else {
                append_segment_surface(
                    override_candidate.text,
                    raw_segment,
                    transform,
                    cycle,
                    normalizer,
                    session.mode,
                    scratch,
                )?;
            }
            context_right_id = override_candidate.meta.right_id;
            it_words = it_words.saturating_add(override_candidate.meta.it_words);
            total_words = total_words.saturating_add(override_candidate.meta.total_words);
            synthetic_exact_committed |= override_candidate.meta.synthetic_exact;
            raw_repair_committed |= override_candidate.meta.raw_repair;
            if index + 1 == count
                && scratch.as_str()[segment_output_start..] == *override_candidate.text
            {
                next_cross_commit_bridge = override_candidate.meta.bridge_tail.and_then(|tail| {
                    session_cross_commit_bridge(reading, override_candidate.text, tail)
                });
            }
            continue;
        }

        let Some(service) = conversion else {
            scratch.clear();
            return Ok(false);
        };
        let selection = session.segment_selection(index);
        let options = conversion_options(session, context_right_id, learning);
        let full_span = range.start == 0 && range.end == session.preedit.len();
        let input = if full_span {
            session_conversion_input(session, reading)
        } else {
            ConversionInput::ordinary(reading)
        };
        let mut selected_surface = FixedStr::<MAX_PREEDIT_BYTES>::new();
        let mut render_candidate =
            |candidates: &[ConversionCandidate]| -> Result<Option<CommitSegmentMeta>, Overflow> {
                if candidates.is_empty() {
                    return Ok(None);
                }
                let projection = match project_conversion_candidates(
                    candidates,
                    raw_segment,
                    transform,
                    cycle,
                    normalizer,
                    session.mode,
                ) {
                    Ok(projection) => projection,
                    Err(ProjectionError::SurfaceOverflow) => return Err(Overflow),
                    Err(_) => return Ok(None),
                };
                let Some(selected) = projection.normalize_selection(selection) else {
                    if !projection.is_complete() {
                        return Err(Overflow);
                    }
                    return Ok(None);
                };
                let Some(raw_selected) = projection.raw_index(selected) else {
                    if !projection.is_complete() {
                        return Err(Overflow);
                    }
                    return Ok(None);
                };
                append_candidate_surface(
                    &candidates[raw_selected],
                    raw_segment,
                    transform,
                    cycle,
                    normalizer,
                    session.mode,
                    scratch,
                )?;
                selected_surface.clear();
                selected_surface.push_str(candidates[raw_selected].text())?;
                Ok(Some(candidate_meta(service, &candidates[raw_selected])))
            };
        let bridge = if index == 0 {
            session.cross_commit_bridge()
        } else {
            None
        };
        let result = if full_span {
            with_session_raw_conversion(
                service,
                learning,
                input,
                &raw_repair_plans,
                options,
                bridge,
                |candidates, _| render_candidate(candidates),
            )
        } else {
            with_session_conversion_input(
                service,
                learning,
                input,
                options,
                bridge,
                |candidates, _| render_candidate(candidates),
            )
        };
        match result {
            Ok(Ok(Some(meta))) => {
                context_right_id = meta.right_id;
                it_words = it_words.saturating_add(meta.it_words);
                total_words = total_words.saturating_add(meta.total_words);
                synthetic_exact_committed |= meta.synthetic_exact;
                raw_repair_committed |= meta.raw_repair;
                if index + 1 == count
                    && scratch.as_str()[segment_output_start..] == *selected_surface.as_str()
                {
                    next_cross_commit_bridge = meta.bridge_tail.and_then(|tail| {
                        session_cross_commit_bridge(reading, selected_surface.as_str(), tail)
                    });
                }
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
    let learnable = if transformed || synthetic_exact_committed {
        None
    } else {
        learning
    };
    record_learning(
        session,
        learnable,
        input_history,
        policy,
        scratch.as_str(),
        context_right_id,
    );
    if raw_repair_committed
        || synthetic_exact_committed
        || (selected_raw_repair_at_start && transformed)
    {
        session.record_current_commit_without_cache(
            scratch.as_str(),
            context_right_id,
            it_words,
            total_words,
        );
    } else {
        let bridge = (!transformed
            && !synthetic_exact_committed
            && !raw_repair_committed
            && !selected_raw_repair_at_start)
            .then_some(next_cross_commit_bridge)
            .flatten();
        session.record_current_commit_with_bridge(
            scratch.as_str(),
            context_right_id,
            it_words,
            total_words,
            bridge,
        );
    }
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
    trigger: ConversionTrigger,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    let long_conversion = if session.host_policy().allows_local_worker() {
        long_conversion
    } else {
        // Renderer-owned Pad conversion still has the local dictionary
        // fallback if a future host policy disables this bounded worker.
        None
    };
    if session.converting {
        let focused = session.focused_segment();
        session.clear_segment_transform(focused);
        session.clear_selected_raw_repair();
        let next = session
            .segment_selection(focused)
            .saturating_add(initial_selection.signum());
        session.set_segment_selection(focused, next);
        return Ok(());
    }
    // Capture the raw/preedit pair before flush.  A completion is admissible
    // only for a proven append-only Romaji snapshot; pending prefixes are
    // intentionally left without a plan rather than guessed around.
    let raw_repair_plans = build_local_completion_plans(session, table);
    flush_pending(session, table, scratch)?;
    if session.preedit.is_empty() {
        out.beep = true;
        return Ok(());
    }
    // Space in a Shift-started English composition is a word separator, never
    // a conversion trigger. Skip the glossary lookup conversion would need so
    // the live casing in `raw_input` is not rewritten to a lowercase reading.
    let input_class = classify_conversion_input(session);
    let literal_policy = literal_policy_for(input_class);
    // Capture the exact source before the optional Shift-Latin preparation
    // lowercases the visible reading. All later render/commit paths reuse this
    // admission decision and surface snapshot.
    session.stage_conversion_input(input_class, literal_policy);
    let skip_dictionary_lookup = session.shifted_ascii && trigger.is_space;
    let shifted_ascii_dictionary_hit = if skip_dictionary_lookup {
        false
    } else {
        prepare_shifted_ascii_reading(session, conversion, input_class)?
    };
    match decide_shift_ascii_convert(ShiftAsciiConvertFacts {
        shifted_ascii: session.shifted_ascii,
        trigger_is_space: trigger.is_space,
        dictionary_hit: shifted_ascii_dictionary_hit,
    }) {
        ShiftAsciiConvertDecision::InsertLiteralSpace => {
            // The temporary English composition owns this key, so keep the
            // space in the same preedit/raw-input provenance rather than
            // committing around it or passing it underneath to the host.
            // Always ASCII U+0020: idle SpaceWidth must not widen a word gap.
            session.clear_staged_conversion_input();
            feed_character(session, table, ' ', false, scratch)?;
            return Ok(());
        }
        ShiftAsciiConvertDecision::RejectUnknown => {
            // A non-Space conversion request for an unknown Shift-started ASCII
            // sequence must not reinterpret it as kana.
            session.clear_staged_conversion_input();
            out.beep = true;
            return Ok(());
        }
        ShiftAsciiConvertDecision::Convert => {}
    }
    session.begin_conversion();
    session.selected_candidate = initial_selection;
    let mut segments: FixedVec<ConversionSegment, MAX_SEGMENTS> = FixedVec::new();
    let mut selected_surface = FixedStr::<MAX_PREEDIT_BYTES>::new();
    let cached = session.cached_surface_fingerprint(session.preedit.as_str());
    let initial_context = session.carry_right_id();
    let options = conversion_options(session, initial_context, learning);
    let mut chosen_selection = initial_selection;
    let mut invalid_mapping = false;
    let initialized = conversion.and_then(|service| {
        match with_session_raw_conversion(
            service,
            learning,
            session_conversion_input(session, session.preedit.as_str()),
            &raw_repair_plans,
            options,
            session.cross_commit_bridge(),
            |candidates, _diagnostics| {
                if candidates.is_empty() {
                    return false;
                }
                if candidates.iter().any(|candidate| {
                    candidate.origin() != CandidateOrigin::Direct
                        && !raw_candidate_mapping_is_valid(
                            &raw_repair_plans,
                            session.preedit.as_str(),
                            candidate,
                        )
                }) {
                    // A malformed/stale map must never make the whole
                    // conversion disappear. Suppress the repair tier and
                    // retain the direct prefix as the deterministic fallback.
                    invalid_mapping = true;
                }
                let direct_limit = candidates
                    .iter()
                    .position(|candidate| candidate.origin() != CandidateOrigin::Direct)
                    .unwrap_or(candidates.len());
                if direct_limit == 0 {
                    return false;
                }
                // A reading that is itself one punctuation mark converts into
                // its whole family with the configured glyph on top, and that
                // glyph is also the reading. Both rules below would move the
                // selection off it: `preferred_candidate_index` refuses a row
                // whose text equals the reading, and a surface learned while a
                // different mark was configured outranks the top row outright.
                // Either way the settings window says 、 and the document gets
                // ､ or ，. These rows are kept out of learning and the exact
                // cache on the way in, so the setting is their only durable
                // preference on the way out too -- pin them the way an exact
                // literal is pinned, which also keeps the optional reranker off
                // a four-row list it has no context for (Issue #99).
                let punctuation_family = session
                    .normalizer
                    .punctuation
                    .family_reading(session.preedit.as_str())
                    .is_some();
                let preserve_exact_initial = (literal_policy != LiteralPolicy::Ranked
                    || punctuation_family)
                    && initial_selection == 0;
                let learned = if preserve_exact_initial {
                    LearningPreference {
                        exact: None,
                        general: None,
                    }
                } else {
                    learning.map_or(
                        LearningPreference {
                            exact: None,
                            general: None,
                        },
                        |service| {
                            service.preference(
                                session.preedit.as_str(),
                                initial_context,
                                candidates[..direct_limit]
                                    .iter()
                                    .map(candidate_learning_key),
                            )
                        },
                    )
                };
                let authoritative = has_authoritative_candidate_preference(
                    candidates,
                    session.preedit.as_str(),
                    initial_selection,
                    cached,
                    learned,
                    direct_limit,
                );
                let preferred = preferred_candidate_index(
                    candidates,
                    session.preedit.as_str(),
                    initial_selection,
                    cached,
                    learned,
                    direct_limit,
                );
                let mut selected = if preserve_exact_initial {
                    0
                } else if !authoritative {
                    long_conversion
                        .filter(|_| {
                            direct_limit > 0
                                && literal_policy == LiteralPolicy::Ranked
                                && session.cross_commit_bridge().is_none()
                        })
                        .and_then(|service| {
                            service.selection(
                                long_conversion_owner,
                                session_id,
                                session.prediction_generation,
                                session.preedit.as_str(),
                                &candidates[..direct_limit],
                            )
                        })
                        .unwrap_or(preferred)
                } else {
                    preferred
                };
                if selected >= direct_limit {
                    selected = 0;
                }
                let (visible_selected, raw_selected) = match project_conversion_candidates(
                    candidates,
                    "",
                    SegmentTransform::None,
                    0,
                    &session.normalizer,
                    session.mode,
                ) {
                    Ok(projection) => {
                        let Some(visible_selected) = projection.visible_index(selected) else {
                            return false;
                        };
                        let Some(raw_selected) = projection.raw_index(visible_selected) else {
                            return false;
                        };
                        (visible_selected, raw_selected)
                    }
                    Err(ProjectionError::SurfaceOverflow) => {
                        // Keep the raw candidate/segments staged so the
                        // normal renderer reports the same bounded overflow
                        // it would have reported before visible projection was
                        // introduced. This path cannot safely map a duplicate,
                        // so the selected raw row remains authoritative until
                        // rendering fails closed.
                        (selected, selected)
                    }
                    Err(_) => return false,
                };
                chosen_selection = i16::try_from(visible_selected).unwrap_or(i16::MAX);
                let candidate = &candidates[raw_selected];
                if selected_surface.push_str(candidate.text()).is_err() {
                    return false;
                }
                // A registered single-character reading is one selectable
                // unit. Splitting e.g. まいくろりっとる into several segments
                // hides its whole-reading tail from every focused list.
                // Preserve the chosen surface and terminal POS while allowing
                // the usual explicit segment-resize commands afterward.
                // Selection is already clamped to the direct prefix above;
                // the projection retains the first raw representative.
                if raw_repair_plans.is_empty()
                    && service
                        .dictionary()
                        .single_kanji(session.preedit.as_str())
                        .next()
                        .is_some()
                {
                    let Some(first) = candidate.segments().first() else {
                        return false;
                    };
                    let mut whole = *first;
                    for next in candidate.segments().iter().skip(1) {
                        whole.reading_end = next.reading_end;
                        whole.text_end = next.text_end;
                        whole.right_id = next.right_id;
                        whole.flags = whole.flags | next.flags;
                        whole.word_count = whole.word_count.saturating_add(next.word_count);
                        whole.it_word_count =
                            whole.it_word_count.saturating_add(next.it_word_count);
                    }
                    return segments.push(whole).is_ok();
                }
                for segment in candidate.segments() {
                    let mut mapped = *segment;
                    if candidate.origin() != CandidateOrigin::Direct {
                        let Some((start, end)) = project_repair_segment(
                            &raw_repair_plans,
                            session.preedit.as_str(),
                            candidate,
                            segment.reading_start,
                            segment.reading_end,
                        ) else {
                            return false;
                        };
                        mapped.reading_start = start;
                        mapped.reading_end = end;
                    }
                    if segments.push(mapped).is_err() {
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
    if invalid_mapping {
        session.suppress_raw_provenance();
    }
    let mut selections = [0i16; MAX_SEGMENTS];
    selections[0] = chosen_selection;
    let selections =
        if matches!(initialized, Some(true)) && segments.len() > 1 && raw_repair_plans.is_empty() {
            conversion.and_then(|service| {
                initial_segment_selections(
                    session,
                    service,
                    learning,
                    segments.as_slice(),
                    selected_surface.as_str(),
                )
            })
        } else {
            Some(selections)
        };
    if !matches!(initialized, Some(true))
        || selections.is_none()
        || !session.set_segments(segments.as_slice())
    {
        session.cancel_conversion();
        out.beep = true;
    } else if let Some(selections) = selections {
        for (index, selection) in selections.into_iter().enumerate().take(segments.len()) {
            session.set_segment_selection(index, selection);
        }
    }
    Ok(())
}

/// Whole-reading and segment-local candidate indices are different domains.
/// Preserve the selected path's surfaces when initializing each segment, using
/// the same contextual query and visible projection as rendering and commit.
/// Run after the whole-reading callback releases its conversion-pool lease.
fn initial_segment_selections(
    session: &Session,
    service: &ConversionService,
    learning: Option<&LearningService>,
    segments: &[ConversionSegment],
    selected_surface: &str,
) -> Option<[i16; MAX_SEGMENTS]> {
    let mut selections = [0i16; MAX_SEGMENTS];
    let mut right_id = session.carry_right_id();
    for (index, segment) in segments.iter().enumerate() {
        let reading = session
            .preedit
            .as_str()
            .get(usize::from(segment.reading_start)..usize::from(segment.reading_end))?;
        let surface =
            selected_surface.get(usize::from(segment.text_start)..usize::from(segment.text_end))?;
        let options = conversion_options(session, right_id, learning);
        let bridge = (index == 0)
            .then(|| session.cross_commit_bridge())
            .flatten();
        let (selection, next_right_id) = with_session_conversion_input(
            service,
            learning,
            ConversionInput::ordinary(reading),
            options,
            bridge,
            |candidates, _| {
                // Bounded segment search can omit a full-path surface. Its
                // deterministic fallback is the first visible segment row,
                // never a whole-reading index applied to this different list.
                let raw = candidates
                    .iter()
                    .position(|candidate| candidate.text() == surface)
                    .unwrap_or(0);
                let (visible, representative) = match project_conversion_candidates(
                    candidates,
                    "",
                    SegmentTransform::None,
                    0,
                    &session.normalizer,
                    session.mode,
                ) {
                    Ok(projection) => {
                        let visible = projection.visible_index(raw)?;
                        (visible, projection.raw_index(visible)?)
                    }
                    // Rendering owns the overflow response. Keep conversion
                    // active and its selections intact until it reports it.
                    Err(ProjectionError::SurfaceOverflow) => (raw, raw),
                    Err(_) => return None,
                };
                Some((
                    i16::try_from(visible).ok()?,
                    candidate_meta(service, candidates.get(representative)?).right_id,
                ))
            },
        )
        .ok()??;
        selections[index] = selection;
        right_id = next_right_id;
    }
    Some(selections)
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
    input_class: ConversionInputClass,
) -> Result<bool, Overflow> {
    if !session.shifted_ascii || session.raw_input.is_empty() {
        return Ok(false);
    }
    let mut reading = FixedStr::<MAX_PREEDIT_BYTES>::new();
    for character in session.raw_input.as_str().chars() {
        reading.push(character.to_ascii_lowercase())?;
    }
    let can_convert = input_class == ConversionInputClass::OpaqueAsciiIdentifier
        || conversion
            .and_then(|service| {
                let options = conversion_options(session, session.carry_right_id(), None);
                with_session_candidates(service, None, reading.as_str(), options, |candidates| {
                    candidates.iter().any(|candidate| {
                        candidate
                            .segments()
                            .last()
                            .map(|segment| segment.reading_end)
                            == u16::try_from(reading.len()).ok()
                            && candidate
                                .segments()
                                .iter()
                                .all(|segment| segment.flags.contains(EntryFlags::IT))
                    })
                })
                .ok()
            })
            .unwrap_or(false);
    if !can_convert {
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
    // Recover the reading before touching the session at all, so a failed
    // reverse scan leaves the live composition exactly as it was and the
    // caller's error reset never has partially-replaced state to clean up.
    let recovered = conversion
        .reconversion_reading(selected_text, scratch)
        .map_err(|_| ErrorCode::Internal)?;
    if !recovered {
        scratch.clear();
        scratch
            .push_str(selected_text)
            .map_err(|_| ErrorCode::TooLarge)?;
    }

    // Order matters: reset first, stage second. `Session::reset` is the one
    // place a staged mode is put back, so staging before the reset would have
    // that very reset consume the stage and leave the Hiragana below permanent.
    // The mode itself has to change — commit-time normalization reads
    // `session.mode`, and a Katakana session would katakana-ise the okurigana
    // this reading just recovered — so what is staged is the way back out.
    session.reset();
    session.reset_carryover();
    session.take_mode_restored();
    session.stage_reconversion_mode(session.mode);
    session.mode = Mode::Hiragana;
    out.consumed = true;
    out.mode = Some(session.mode);

    session
        .preedit
        .push_str(scratch.as_str())
        .map_err(|_| ErrorCode::TooLarge)?;
    session.cursor =
        u16::try_from(session.preedit.as_str().chars().count()).map_err(|_| ErrorCode::TooLarge)?;
    // Reconversion replaces host text rather than replaying an append-only
    // Romaji tail.  Keep that distinction explicit so later conversion
    // renders can never admit a local raw-repair plan for the recovered text.
    session.suppress_raw_provenance();

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
        ConversionTrigger::default(),
        out,
    )
    .map_err(|_| ErrorCode::TooLarge)?;
    let normalizer = session.normalizer;
    render_preedit(
        session,
        table,
        &normalizer,
        Some(conversion),
        learning,
        scratch,
        out,
    )
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
    let keep_selected_repair = matches!(
        transform,
        SegmentTransform::Hiragana | SegmentTransform::Katakana | SegmentTransform::HalfKatakana
    ) && session.has_selected_raw_repair();
    if keep_selected_repair {
        session.suppress_raw_provenance_preserve_selected();
    } else {
        session.suppress_raw_provenance();
    }
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
fn resync_shifted_ascii_from_raw(
    session: &mut Session,
    table: &Table,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
) -> Result<(), Overflow> {
    let raw_cursor = session.cursor;
    let mut romaji = Input::new();
    let mut preedit = FixedStr::<MAX_PREEDIT_BYTES>::new();
    for character in session.raw_input.as_str().chars() {
        scratch.clear();
        table.feed(&mut romaji, character, scratch)?;
        preedit.push_str(scratch.as_str())?;
    }
    session.romaji = romaji;
    session.preedit = preedit;
    session.cursor = raw_cursor
        .min(u16::try_from(session.raw_input.as_str().chars().count()).unwrap_or(u16::MAX));
    session.invalidate_prediction();
    Ok(())
}

fn apply_backspace(
    session: &mut Session,
    table: &Table,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
) -> Result<(), Overflow> {
    session.suppress_raw_provenance();
    if session.shifted_ascii {
        let cursor = usize::from(session.cursor);
        if cursor == 0 {
            return Ok(());
        }
        if let Some(at) = session.raw_input.byte_index(cursor - 1) {
            let _ = session.raw_input.remove_char_at(at);
            session.cursor = session.cursor.saturating_sub(1);
        }
        resync_shifted_ascii_from_raw(session, table, scratch)?;
        return Ok(());
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
        return Ok(());
    }
    let cursor = usize::from(session.cursor);
    if cursor == 0 {
        return Ok(());
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
    Ok(())
}

/// Mirrors `apply_backspace`'s kana-group-wise removal on the raw side: the
/// character sitting at `cursor` may likewise have taken more than one
/// keystroke to produce, and forward-delete must drop exactly those to keep
/// `raw_input` faithful (#16 finding C). Uses `raw_range_for_next_emitted`
/// rather than a whole-buffer replay so a keystroke still pending ahead of
/// the deleted character -- possible once the caret has moved -- is left
/// untouched instead of being folded into the deleted span (#16 finding
/// B/C).
fn apply_delete_forward(
    session: &mut Session,
    table: &Table,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
) -> Result<(), Overflow> {
    session.suppress_raw_provenance();
    if session.shifted_ascii {
        let cursor = usize::from(session.cursor);
        if let Some(at) = session.raw_input.byte_index(cursor) {
            let _ = session.raw_input.remove_char_at(at);
        }
        resync_shifted_ascii_from_raw(session, table, scratch)?;
        return Ok(());
    }
    let cursor = usize::from(session.cursor);
    if let Some(range) = raw_range_for_next_emitted(table, session) {
        remove_raw_range(&mut session.raw_input, range);
    }
    if let Some(at) = session.preedit.byte_index(cursor) {
        let _ = session.preedit.remove_char_at(at);
        session.invalidate_prediction();
    }
    Ok(())
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
    session.suppress_raw_provenance();
    if session.shifted_ascii {
        let end = u16::try_from(session.raw_input.as_str().chars().count()).unwrap_or(u16::MAX);
        session.cursor = match movement {
            CaretMove::Left => session.cursor.saturating_sub(1),
            CaretMove::Right => session.cursor.saturating_add(1).min(end),
            CaretMove::Home => 0,
            CaretMove::End => end,
        };
        session.hide_suggestions();
        return Ok(());
    }
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
    let preserve_exact = session.literal_policy() != LiteralPolicy::Ranked
        && session.staged_exact_surface_matches_current();
    if preserve_exact {
        scratch.push_str(session.conversion_exact_surface())?;
    } else {
        normalizer.normalize_into(session.preedit.as_str(), session.mode, scratch)?;
    }
    if !scratch.is_empty() {
        out.set_commit(scratch.as_str())?;
        record_learning(
            session,
            if preserve_exact { None } else { learning },
            input_history,
            policy,
            scratch.as_str(),
            0,
        );
        if preserve_exact {
            session.record_current_commit_without_cache(scratch.as_str(), 0, 0, 0);
        } else {
            session.record_current_commit(scratch.as_str(), 0, 0, 0);
        }
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
    session.host_policy().allows_local_worker()
        && services.prediction_enabled
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
            session.input_support,
            scope_is_sensitive(session.scope)
                || !session.scope_classified
                || services.learning.as_ref().is_some_and(|learning| {
                    learning.is_repair_suppressed(session.preedit.as_str())
                }),
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
    if focus_prediction_candidate(session, candidates, direction) {
        // The explicit navigation key was initially evaluated before the
        // bounded retry completed. A successful retry makes that same key a
        // real focus transition rather than a misleading beep.
        out.beep = false;
    }
}

fn focus_prediction_candidate(
    session: &mut Session,
    candidates: &[crate::prediction::PredictionCandidate],
    direction: i16,
) -> bool {
    if direction >= 0 && !session.suggestion_focused {
        let first_distinct = candidates
            .iter()
            .position(|candidate| candidate.surface() != session.preedit.as_str())
            .unwrap_or(0);
        session.focus_suggestion_at(first_distinct, candidates.len())
    } else {
        session.focus_suggestion(direction, candidates.len())
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
    if !session.host_policy().allows_local_worker() {
        session.hide_suggestions();
        return Ok(());
    }
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
    for (index, candidate) in candidates.iter().enumerate() {
        scratch.clear();
        normalizer.normalize_into(candidate.surface(), session.mode, scratch)?;
        let pushed = if candidate.source() == PredictionSource::History {
            out.push_history_candidate(
                scratch.as_str(),
                candidate.annotation(),
                candidate.reading(),
                candidate.surface(),
            )
        } else {
            out.push_candidate(scratch.as_str(), candidate.annotation())
        };
        if let Err(overflow) = pushed {
            // The shared candidate-text arena is sized for `MAX_CANDIDATES`
            // short entries, not `MAX_CANDIDATES` worst-case-length ones, so
            // a long tail can still exhaust it (Issue #95). Once the selected
            // candidate is already in the builder, running out of arena
            // space just ends the list early -- the renderer only shows one
            // page at a time anyway. Losing the selected candidate itself
            // would show the wrong selection, so that case still fails
            // closed instead of silently truncating past it.
            if index <= selected {
                return Err(overflow);
            }
            break;
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
    if !session.host_policy().allows_local_worker() {
        session.hide_suggestions();
        return Ok(());
    }
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
    learning: Option<&LearningService>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.is_composing() {
        return Ok(());
    }

    if session.converting {
        match render_converted_segments(
            session, table, normalizer, conversion, learning, scratch, out,
        )? {
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
        let end = session.raw_input.as_str().chars().count();
        let cursor = usize::from(session.cursor).min(end);
        out.set_cursor(u32::try_from(cursor).unwrap_or(u32::MAX));
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

/// Renders a raw-repair conversion as one authoritative candidate surface.
/// Corrected candidates may have more than one bunsetsu segment, while the
/// engine's ordinary per-segment renderer only has the original-reading
/// boundaries. Re-running the one-slot raw service here preserves the full
/// candidate list and selected surface without dropping a corrected segment.
#[allow(clippy::too_many_arguments)]
fn render_staged_raw_repair(
    session: &mut Session,
    table: &Table,
    plans: &[RawRepairPlan],
    normalizer: &Normalizer,
    conversion: &ConversionService,
    learning: Option<&LearningService>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<bool, Overflow> {
    let reading = session.preedit.as_str();
    let input = session_conversion_input(session, reading);
    let options = conversion_options(session, session.carry_right_id(), learning);
    let focused = session.focused_segment();
    let requested = session.segment_selection(focused);
    let mut selected = 0usize;
    let mut rendered_chars = 0usize;
    let mut invalid_mapping = false;
    let mut projection_overflow = false;
    let mut selected_plan_id = None;
    let mut selected_segment_ends = [0u16; MAX_SEGMENTS];
    let mut selected_segment_count = 0usize;
    let result = with_session_raw_conversion(
        conversion,
        learning,
        input,
        plans,
        options,
        None,
        |candidates, _| {
            if candidates.is_empty() {
                return false;
            }
            if candidates
                .iter()
                .any(|candidate| !raw_candidate_mapping_is_valid(plans, reading, candidate))
            {
                invalid_mapping = true;
                return false;
            }
            let projection = match project_conversion_candidates(
                candidates,
                "",
                SegmentTransform::None,
                0,
                normalizer,
                session.mode,
            ) {
                Ok(projection) => projection,
                Err(ProjectionError::SurfaceOverflow) => {
                    projection_overflow = true;
                    return false;
                }
                Err(_) => return false,
            };
            let Some(visible_index) = projection.normalize_selection(requested) else {
                if !projection.is_complete() {
                    projection_overflow = true;
                }
                return false;
            };
            let Some(raw_index) = projection.raw_index(visible_index) else {
                if !projection.is_complete() {
                    projection_overflow = true;
                }
                return false;
            };
            selected = visible_index;
            scratch.clear();
            let candidate = &candidates[raw_index];
            if append_candidate_surface(
                candidate,
                "",
                SegmentTransform::None,
                0,
                normalizer,
                session.mode,
                scratch,
            )
            .is_err()
            {
                return false;
            }
            rendered_chars = scratch.as_str().chars().count();
            selected_plan_id = match candidate.origin() {
                CandidateOrigin::RawRepair { plan_id, .. } => {
                    selected_segment_count = candidate.segments().len();
                    if selected_segment_count > MAX_SEGMENTS {
                        return false;
                    }
                    for (index, segment) in candidate.segments().iter().enumerate() {
                        selected_segment_ends[index] = segment.reading_end;
                    }
                    Some(plan_id)
                }
                CandidateOrigin::Direct => {
                    selected_segment_count = 0;
                    None
                }
            };
            out.begin_preedit();
            if out
                .push_segment(scratch.as_str(), UnderlineKind::Focused)
                .is_err()
            {
                return false;
            }
            if out
                .begin_conversion_candidates(
                    session.conversion_presentation(),
                    u16::try_from(selected).unwrap_or(u16::MAX),
                    CANDIDATE_PAGE_SIZE as u16,
                )
                .is_err()
            {
                return false;
            }
            for visible_index in projection.visible_indices() {
                let Some(raw_index) = projection.raw_index(visible_index) else {
                    return false;
                };
                let candidate = &candidates[raw_index];
                scratch.clear();
                if append_candidate_surface(
                    candidate,
                    "",
                    SegmentTransform::None,
                    0,
                    normalizer,
                    session.mode,
                    scratch,
                )
                .is_err()
                {
                    return false;
                }
                if out
                    .push_candidate(scratch.as_str(), candidate.annotation())
                    .is_err()
                {
                    // As below in `render_candidates`: an arena overflow past
                    // the already-pushed selection just truncates the list;
                    // one at or before it would show the wrong selection, so
                    // that case is still treated as a full render failure.
                    if visible_index <= selected {
                        return false;
                    }
                    break;
                }
            }
            if let Some(entry_index) = candidate.system_entry_index() {
                publish_system_candidate_detail(conversion, entry_index, reading, out);
            }
            true
        },
    )
    .unwrap_or(false);
    if invalid_mapping {
        // The correction map is repair metadata, not a reason to hide the
        // ordinary direct conversion. Once the repair tier is suppressed,
        // the regular renderer publishes the direct prefix only.
        session.suppress_raw_provenance();
        return render_converted_segments(
            session,
            table,
            normalizer,
            Some(conversion),
            learning,
            scratch,
            out,
        );
    }
    if projection_overflow {
        return Err(Overflow);
    }
    if !result {
        return Ok(false);
    }
    if let Some(plan_id) = selected_plan_id {
        let Some(corrected_snapshot) = plans
            .iter()
            .find(|plan| plan.plan_id() == plan_id)
            .map(|plan| plan.corrected_reading())
        else {
            session.clear_selected_raw_repair();
            return Ok(false);
        };
        let mut corrected = FixedStr::<MAX_PREEDIT_BYTES>::new();
        if corrected.push_str(corrected_snapshot).is_err() {
            session.clear_selected_raw_repair();
            return Ok(false);
        }
        if !session.stage_selected_raw_repair(
            plan_id,
            corrected.as_str(),
            &selected_segment_ends[..selected_segment_count],
        ) {
            session.clear_selected_raw_repair();
            return Ok(false);
        }
    } else {
        session.clear_selected_raw_repair();
    }
    session.set_segment_selection(focused, i16::try_from(selected).unwrap_or(i16::MAX));
    out.set_cursor(u32::try_from(rendered_chars).unwrap_or(u32::MAX));
    Ok(true)
}

/// Renders each pinned segment independently. Only the focused segment owns a
/// candidate table; every other segment keeps its own selection and underline.
/// Returning `false` is a terminal, recoverable conversion-service failure.
fn render_converted_segments(
    session: &mut Session,
    table: &Table,
    normalizer: &Normalizer,
    conversion: Option<&ConversionService>,
    learning: Option<&LearningService>,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<bool, Overflow> {
    if session.segment_count() == 0 {
        return Ok(false);
    }

    let raw_repair_plans = build_local_completion_plans(session, table);
    if !raw_repair_plans.is_empty() {
        let Some(service) = conversion else {
            return Ok(false);
        };
        return render_staged_raw_repair(
            session,
            table,
            &raw_repair_plans,
            normalizer,
            service,
            learning,
            scratch,
            out,
        );
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
            range.clone(),
        );
        let (transform, cycle) = session.segment_transform(index);
        let underline = if index == focused_segment {
            UnderlineKind::Focused
        } else {
            UnderlineKind::Converted
        };

        if transform != SegmentTransform::None {
            let transform_reading = if matches!(
                transform,
                SegmentTransform::Hiragana
                    | SegmentTransform::Katakana
                    | SegmentTransform::HalfKatakana
            ) {
                if session.segment_count() == 1 {
                    session.selected_raw_repair_full().unwrap_or(reading)
                } else {
                    session
                        .selected_raw_repair_segment(index)
                        .unwrap_or(reading)
                }
            } else {
                reading
            };
            scratch.clear();
            append_segment_surface(
                transform_reading,
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
        let options = conversion_options(session, context_right_id, learning);
        let full_span = range.start == 0 && range.end == session.preedit.len();
        let input = if full_span {
            session_conversion_input(session, reading)
        } else {
            ConversionInput::ordinary(reading)
        };
        let mut render_candidates = |candidates: &[ConversionCandidate]| -> Result<
            Option<(i16, usize, CommitSegmentMeta)>,
            Overflow,
        > {
            if candidates.is_empty() {
                return Ok(None);
            }
            let projection = match project_conversion_candidates(
                candidates,
                raw_segment,
                SegmentTransform::None,
                0,
                normalizer,
                session.mode,
            ) {
                Ok(projection) => projection,
                Err(ProjectionError::SurfaceOverflow) => return Err(Overflow),
                Err(_) => return Ok(None),
            };
            let Some(selected) = projection.normalize_selection(requested_selection) else {
                if !projection.is_complete() {
                    return Err(Overflow);
                }
                return Ok(None);
            };
            let Some(raw_selected) = projection.raw_index(selected) else {
                if !projection.is_complete() {
                    return Err(Overflow);
                }
                return Ok(None);
            };
            scratch.clear();
            append_candidate_surface(
                &candidates[raw_selected],
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
                for visible_index in projection.visible_indices() {
                    let Some(raw_index) = projection.raw_index(visible_index) else {
                        return Ok(None);
                    };
                    let candidate = &candidates[raw_index];
                    scratch.clear();
                    append_candidate_surface(
                        candidate,
                        raw_segment,
                        SegmentTransform::None,
                        0,
                        normalizer,
                        session.mode,
                        scratch,
                    )?;
                    if let Err(overflow) =
                        out.push_candidate(scratch.as_str(), candidate.annotation())
                    {
                        // Mirrors `render_suggestions`: the shared arena can
                        // still run dry over a long candidate tail (Issue
                        // #95). Truncating after the selection is already in
                        // the builder is safe; truncating at or before it
                        // would show the wrong selection, so that still
                        // fails the render instead of guessing.
                        if visible_index <= selected {
                            return Err(overflow);
                        }
                        break;
                    }
                }
                if let Some(entry_index) = candidates[raw_selected].system_entry_index() {
                    publish_system_candidate_detail(service, entry_index, reading, out);
                }
            }

            Ok(Some((
                i16::try_from(selected).map_err(|_| Overflow)?,
                rendered_chars,
                candidate_meta(service, &candidates[raw_selected]),
            )))
        };
        let bridge = if index == 0 {
            session.cross_commit_bridge()
        } else {
            None
        };
        let rendered = if full_span {
            with_session_raw_conversion(
                service,
                learning,
                input,
                &raw_repair_plans,
                options,
                bridge,
                |candidates, _| render_candidates(candidates),
            )
        } else {
            with_session_conversion_input(
                service,
                learning,
                input,
                options,
                bridge,
                |candidates, _| render_candidates(candidates),
            )
        };

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

#[cfg(all(test, feature = "dev-fixtures"))]
#[path = "dispatch_tests.rs"]
mod tests;
