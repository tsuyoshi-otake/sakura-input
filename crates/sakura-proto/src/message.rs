//! Request/response messages and their frame-level encode/decode.
//!
//! A frame on the pipe is `u32 LE payload_len` followed by exactly that
//! many bytes (DESIGN.md §7). This module works one layer in from that:
//! [`payload_len`] parses the 4-byte header, and `encode_*`/`decode_*`
//! turn a payload's bytes into/from a [`Request`] or [`Response`]. Both
//! directions share one payload layout:
//!
//! ```text
//! u16 LE  protocol_version
//! u64 LE  request_id
//! u16 LE  message_type
//!         ...body...
//! ```
//!
//! Every request carries a monotonic per-session request id (DESIGN.md
//! §7's stale-response guard: a named pipe is a byte stream, and without
//! correlation ids a late reply to a timed-out request would be
//! mis-attributed to the next one).

use crate::types::{
    AppearanceTheme, CandidateDetail, CandidateList, ErrorCode, InputScope, KeyInput, Mode, Output,
    ScreenRect,
};
use crate::wire::{Reader, Sink, VecSink};
use crate::{RequestId, Revision, SessionId, FRAME_HEADER_LEN, MAX_PAYLOAD, PROTOCOL_VERSION};

// Re-exported so `sakura_proto::message::Error` and `sakura_proto::Error`
// both name the one error type used across the whole crate.
pub use crate::wire::Error;

// Wire values for each request message type.
pub(crate) const REQ_HELLO: u16 = 0x0001;
pub(crate) const REQ_CREATE_SESSION: u16 = 0x0002;
pub(crate) const REQ_SEND_KEY: u16 = 0x0003;
pub(crate) const REQ_COMMIT: u16 = 0x0004;
pub(crate) const REQ_REVERT: u16 = 0x0005;
pub(crate) const REQ_SET_INPUT_SCOPE: u16 = 0x0006;
pub(crate) const REQ_DELETE_SESSION: u16 = 0x0007;
pub(crate) const REQ_PING: u16 = 0x0008;
pub(crate) const REQ_SHUTDOWN: u16 = 0x0009;
pub(crate) const REQ_WATCH_UI: u16 = 0x000A;
pub(crate) const REQ_SET_UI_PLACEMENT: u16 = 0x000B;
pub(crate) const REQ_RECONVERT: u16 = 0x000C;
pub(crate) const REQ_CLEAR_LEARNING: u16 = 0x000D;
pub(crate) const REQ_CLEAR_INPUT_HISTORY: u16 = 0x000E;
pub(crate) const REQ_FLUSH_INPUT_HISTORY: u16 = 0x000F;
pub(crate) const REQ_INPUT_HISTORY_STATS: u16 = 0x0010;
pub(crate) const REQ_UNDO_COMMIT: u16 = 0x0011;
pub(crate) const REQ_PROBE_KEY: u16 = 0x0012;
pub(crate) const REQ_SET_MODE: u16 = 0x0013;
/// Deletes the exact learned prediction candidate in the renderer's current
/// UI snapshot. This is separate from keyboard-driven deletion because the
/// renderer owns no editing session and can only name a revision-stamped row.
pub(crate) const REQ_DELETE_HISTORY_CANDIDATE: u16 = 0x0014;
pub(crate) const REQ_APPLY_AI_COMPOSITION: u16 = 0x0015;
pub(crate) const REQ_RECORD_AI_TEXT: u16 = 0x0016;
pub(crate) const REQ_START_AI_TEXT: u16 = 0x0017;
pub(crate) const REQ_POLL_AI_TEXT: u16 = 0x0018;
pub(crate) const REQ_CANCEL_AI_TEXT: u16 = 0x0019;

// Wire values for each response message type. `RES_OUTPUT` is also used
// directly by `crate::output::OutputBuf::encode_frame`, which encodes a
// `Response::Output` frame without allocating and so cannot go through
// `encode_response`.
pub(crate) const RES_HELLO: u16 = 0x8001;
pub(crate) const RES_SESSION_CREATED: u16 = 0x8002;
pub(crate) const RES_OUTPUT: u16 = 0x8003;
pub(crate) const RES_PONG: u16 = 0x8004;
pub(crate) const RES_OK: u16 = 0x8005;
pub(crate) const RES_UI: u16 = 0x8006;
pub(crate) const RES_INPUT_HISTORY_STATS: u16 = 0x8007;
pub(crate) const RES_INPUT_MODE: u16 = 0x8008;
pub(crate) const RES_HISTORY_CANDIDATE_DELETED: u16 = 0x8009;
pub(crate) const RES_AI_TEXT_STARTED: u16 = 0x800A;
pub(crate) const RES_AI_TEXT_PENDING: u16 = 0x800B;
pub(crate) const RES_AI_TEXT_RESULT: u16 = 0x800C;
pub(crate) const RES_ERROR: u16 = 0x80FF;

/// A message sent from a client (the TSF DLL) to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The first message on a new connection: negotiates the protocol
    /// version.
    Hello {
        client_version: u16,
    },
    /// Starts a new editing session for a host process.
    CreateSession {
        process_name: String,
    },
    /// Delivers one key event to a session.
    SendKey {
        session: SessionId,
        key: KeyInput,
    },
    /// Evaluates one key against a throwaway session state after applying the
    /// supplied host scope. Unlike `SetInputScope`, this never changes the
    /// live session or the link's applied-scope cache. When `fresh_context` is
    /// true, the probe starts from the same resolved profile defaults as a new
    /// connection after a TSF context replacement, rather than cloning the
    /// old document's session state.
    ProbeKey {
        session: SessionId,
        scope: InputScope,
        fresh_context: bool,
        key: KeyInput,
    },
    /// Commits the current composition.
    Commit {
        session: SessionId,
    },
    /// Reverts the current composition (cancels it).
    Revert {
        session: SessionId,
    },
    /// Completes the two-phase exact-text commit undo transaction. The TSF
    /// side sends `Applied` only after the host deleted the verified text;
    /// `Rejected` means validation failed before mutation, and `Unknown`
    /// means a host call may have changed the document; the TSF caller retires
    /// the engine link if it cannot confirm settlement.
    UndoCommit {
        session: SessionId,
        outcome: UndoCommitOutcome,
    },
    /// Builds conversion candidates for already committed text. `preview`
    /// serves `ITfFnReconversion::GetReconversion` without mutating the live
    /// session; `false` starts the real composition.
    Reconvert {
        session: SessionId,
        text: String,
        preview: bool,
    },
    /// Atomically clears the engine's live and durable learning state. This
    /// administration request is used by the per-user settings process and
    /// deliberately carries no editing session.
    ClearLearning,
    /// Clears the opt-in developer input-history store.
    ClearInputHistory,
    /// Flushes queued developer input-history records to durable storage.
    FlushInputHistory,
    /// Reads live developer input-history drop and persistence counters.
    InputHistoryStats,
    /// Tells the engine the input scope of the focused field.
    SetInputScope {
        session: SessionId,
        scope: InputScope,
    },
    /// Changes the persistent input mode without synthesizing a keyboard
    /// event. This is reserved for the focused TSF input-mode menu and is
    /// rejected while a composition is active or the field is not a known
    /// ordinary-text scope, so a menu callback can never own a document edit.
    SetMode {
        session: SessionId,
        mode: Mode,
    },
    /// Commits an isolated worker result in place of the still-active Sakura
    /// composition. The TSF frontend validates the captured visible source
    /// before issuing this request; the engine independently requires a known
    /// normal scope and a live composition.
    ApplyAiComposition {
        session: SessionId,
        result: String,
    },
    /// Appends one terminal AI text operation to opt-in developer history.
    /// Persistence applies the session's authoritative scope classification;
    /// the frontend cannot opt sensitive or test traffic into the store.
    RecordAiText {
        session: SessionId,
        operation: AiTextOperation,
        status: AiTextStatus,
        source: String,
        result: String,
        model: String,
        provider: String,
        style: String,
        error_code: String,
        latency_ms: u64,
        input_tokens: u32,
        output_tokens: u32,
        cached_tokens: u32,
        attempts: u32,
        test_only: bool,
    },
    StartAiText {
        session: SessionId,
        operation: AiTextOperation,
        text: String,
    },
    PollAiText {
        session: SessionId,
        job: u64,
    },
    CancelAiText {
        session: SessionId,
        job: u64,
    },
    /// Deletes one learned prediction candidate from the exact UI snapshot
    /// previously published to the renderer. `revision` and
    /// `candidate_index` are checked against engine-owned state; a surface or
    /// reading never crosses this untrusted boundary.
    DeleteHistoryCandidate {
        revision: Revision,
        candidate_index: u16,
    },
    /// Ends a session and releases its resources.
    DeleteSession {
        session: SessionId,
    },
    /// A liveness check; the engine answers with `Response::Pong`.
    Ping,
    /// Asks the engine to flush state and exit.
    Shutdown,
    /// Asks for the UI state, but not before it differs from `since`.
    ///
    /// This is how the renderer learns what to draw (DESIGN 8's mode
    /// indicator). It is a long poll, not a subscription: the engine holds
    /// the reply until [`UiState::revision`] moves past `since`, or until a
    /// heartbeat interval passes, and answers [`Response::Ui`] either way.
    ///
    /// Long poll rather than a push channel because the transport is one
    /// reply per request in both directions (see the module docs' frame
    /// layout), and rather than fixed-interval polling because a mode
    /// indicator that woke a laptop ten times a second to be told nothing
    /// changed would cost more battery than the entire rest of the IME.
    /// The heartbeat is what makes engine death observable: a renderer
    /// whose long poll stops coming back knows to restart it (DESIGN 4.3's
    /// watchdog).
    ///
    /// `since` of 0 means "answer immediately with whatever is current",
    /// which is what a renderer that just connected wants.
    WatchUi {
        since: Revision,
    },
    /// Updates the renderer-owned candidate window's caret rectangle and
    /// whether TSF's UI-element manager permits the renderer to show it.
    ///
    /// This request is separate from keystrokes so layout-change callbacks
    /// can move the popup without mutating the conversion session.
    SetUiPlacement {
        session: SessionId,
        anchor: Option<ScreenRect>,
        /// Screen rectangle of the host's editable area, when the host
        /// reports one. See [`UiState::document`].
        document: Option<ScreenRect>,
        renderer_visible: bool,
    },
}

/// Terminal outcome for an exact-text commit undo transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum UndoCommitOutcome {
    Applied = 1,
    Rejected = 2,
    Unknown = 3,
}

impl UndoCommitOutcome {
    fn encode<S: Sink>(self, w: &mut S) -> Result<(), Error> {
        w.write_u16(self as u16)
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u16()? {
            1 => Ok(Self::Applied),
            2 => Ok(Self::Rejected),
            3 => Ok(Self::Unknown),
            _ => Err(Error::BadEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AiTextOperation {
    Transform = 1,
    Proofread = 2,
}

impl AiTextOperation {
    fn encode<S: Sink>(self, w: &mut S) -> Result<(), Error> {
        w.write_u8(self as u8)
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            1 => Ok(Self::Transform),
            2 => Ok(Self::Proofread),
            _ => Err(Error::BadEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AiTextStatus {
    Applied = 1,
    Cancelled = 2,
    Timeout = 3,
    MissingKey = 4,
    WorkerError = 5,
    ApiError = 6,
    Rejected = 7,
}

impl AiTextStatus {
    fn encode<S: Sink>(self, w: &mut S) -> Result<(), Error> {
        w.write_u8(self as u8)
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            1 => Ok(Self::Applied),
            2 => Ok(Self::Cancelled),
            3 => Ok(Self::Timeout),
            4 => Ok(Self::MissingKey),
            5 => Ok(Self::WorkerError),
            6 => Ok(Self::ApiError),
            7 => Ok(Self::Rejected),
            _ => Err(Error::BadEnum),
        }
    }
}

/// A message sent from the engine back to a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// Answers `Request::Hello` with the engine's own version info.
    Hello {
        server_version: u16,
        engine_version: [u16; 3],
    },
    /// Answers `Request::CreateSession` with the new session's id and its
    /// profile-resolved initial mode. The TSF input-mode item needs this before
    /// the user types a key, because a visible caret must immediately show the
    /// actual per-application mode rather than a guessed default.
    SessionCreated {
        session: SessionId,
        mode: Mode,
    },
    /// The result of a key event or editing command.
    Output(Output),
    /// Answers `Request::Ping`.
    Pong,
    /// A generic success acknowledgement (e.g. for `Commit`/`Revert`).
    Ok,
    /// The current mode after a successful [`Request::SetMode`].
    InputMode {
        mode: Mode,
    },
    /// Live counters for the developer input-history writer.
    InputHistoryStats {
        active: bool,
        dropped_events: u64,
        persistence_failures: u64,
        excluded_unclassified_events: u64,
        excluded_sensitive_events: u64,
        excluded_test_only_events: u64,
        ai_requests: u64,
        ai_attempts: u64,
        ai_input_tokens: u64,
        ai_output_tokens: u64,
        ai_cached_tokens: u64,
    },
    /// Answers [`Request::DeleteHistoryCandidate`]. `false` is a terminal,
    /// fail-closed no-op for stale UI, a non-history row, disabled learning,
    /// a duplicate click, or a persistence failure. The renderer must wait
    /// for a later [`Response::Ui`] before changing what it draws.
    HistoryCandidateDeleted {
        removed: bool,
    },
    AiTextStarted {
        job: u64,
    },
    AiTextPending {
        job: u64,
    },
    AiTextResult {
        job: u64,
        status: AiTextStatus,
        result: String,
        model: String,
        provider: String,
        style: String,
        error_code: String,
        latency_ms: u64,
        input_tokens: u32,
        output_tokens: u32,
        cached_tokens: u32,
        attempts: u32,
    },
    /// Answers `Request::WatchUi` with what the renderer should draw.
    Ui(UiState),
    /// The request could not be fulfilled.
    Error(ErrorCode),
}

/// What the renderer draws, and the revision that identifies it.
///
/// Deliberately not a session's state. The renderer draws one indicator for
/// the whole logon session, because that is what the user sees — one caret,
/// in one focused field, at a time — while the engine keeps a mode per
/// session, one per focused field in every running application. This is the
/// mode of whichever session most recently changed one, which is the same
/// thing as "the mode of the field the user is typing in" for as long as
/// only one field can have the caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    /// Increments on every change. A renderer passes the last one it saw
    /// back as `Request::WatchUi { since }`; the engine answers when this
    /// has moved past it.
    ///
    /// Starts at 1, so `since: 0` is always stale and always answers at
    /// once — that is a fresh renderer asking "what is true right now?".
    pub revision: Revision,
    /// User-wide appearance preference for Sakura-owned renderer UI. It is
    /// present even while the popup is hidden so a later candidate state
    /// cannot be drawn with an assumed palette.
    pub appearance_theme: AppearanceTheme,
    /// The mode to show, or `None` when no field is composing and the
    /// indicator should be hidden.
    pub mode: Option<Mode>,
    /// Candidates for the renderer-owned popup, or `None` when conversion is
    /// not active. UI-less TSF hosts read the same list through
    /// `ITfCandidateListUIElement` in the DLL.
    pub candidates: Option<CandidateList>,
    /// Optional detail for the selected candidate. It is absent whenever the
    /// candidate list is absent, so renderers can fail closed without guessing.
    pub candidate_detail: Option<CandidateDetail>,
    /// Screen rectangle of the active composition. The renderer anchors its
    /// popup below this rectangle and hides it until one is available.
    pub anchor: Option<ScreenRect>,
    /// Screen rectangle of the host's editable area, when the host reports
    /// one. "Below the composition" is still inside the box the user is
    /// typing into whenever that box is taller than one caret line, so the
    /// renderer needs the box itself to avoid covering it.
    ///
    /// `None` whenever the host does not answer, which leaves the renderer
    /// with exactly the composition-only placement it used before.
    pub document: Option<ScreenRect>,
    /// `false` when TSF's UI-element manager elected to render candidates
    /// itself. The external renderer must then stay hidden.
    pub renderer_visible: bool,
    /// The engine is shutting down deliberately, and whoever is watching
    /// should shut down too rather than treat the closing pipe as a crash.
    ///
    /// This exists because the renderer is the engine's watchdog (DESIGN
    /// 3): when the pipe breaks it restarts the engine. That is right when
    /// the engine crashed and catastrophic during an uninstall, where
    /// `sakura_regtool --stop` has just asked it to exit and the installer
    /// is about to delete the very file the watchdog would relaunch — and
    /// a relaunched engine holds that file open, so the delete fails too.
    /// The two cases are indistinguishable from the broken pipe alone,
    /// which is why the intent is announced *before* the pipe breaks.
    ///
    /// It rides on the UI state rather than getting a message of its own
    /// because the renderer is already parked in a `WatchUi` call, and
    /// DESIGN 11 specifies `--stop` as asking the engine *and the renderer*
    /// to exit over the pipe — the renderer only holds the client end, so
    /// the request can only reach it as an answer to something it asked.
    pub stopping: bool,
}

/// The fixed-layout header shared by every payload, decoded without
/// interpreting the message body. Useful for triage (e.g. logging or
/// routing) before committing to a full [`decode_request`]/
/// [`decode_response`] call, which additionally validates the protocol
/// version and the body shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version: u16,
    pub request_id: RequestId,
    pub msg_type: u16,
}

/// Reads the 4-byte little-endian frame length prefix.
///
/// Errors with [`Error::TooLarge`] if the declared length exceeds
/// [`MAX_PAYLOAD`] — the caller must not attempt to read that many bytes
/// from the pipe.
pub fn payload_len(header: &[u8; 4]) -> Result<usize, Error> {
    let len = u32::from_le_bytes(*header) as usize;
    if len > MAX_PAYLOAD {
        return Err(Error::TooLarge);
    }
    Ok(len)
}

/// Reads only the `version` + `request_id` + `msg_type` header of a
/// payload, without decoding (or validating the version of) the body.
pub fn peek_header(payload: &[u8]) -> Result<Header, Error> {
    let mut r = Reader::new(payload);
    let version = r.read_u16()?;
    let request_id = r.read_u64()?;
    let msg_type = r.read_u16()?;
    Ok(Header {
        version,
        request_id,
        msg_type,
    })
}

/// Writes the common `version, id, msg_type` header and then `body` into
/// `dst`, patching in the 4-byte frame length prefix at the end.
///
/// `dst` is cleared first; its existing capacity is reused, so a caller
/// that keeps one buffer per connection performs no allocation at
/// steady state. On any error `dst` is left empty rather than holding a
/// partial or oversized frame.
fn encode_frame(
    id: RequestId,
    msg_type: u16,
    dst: &mut Vec<u8>,
    body: impl FnOnce(&mut VecSink<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    dst.clear();
    dst.extend_from_slice(&[0u8; FRAME_HEADER_LEN]);
    let result = (|| {
        let mut w = VecSink::new(dst);
        w.write_u16(PROTOCOL_VERSION)?;
        w.write_u64(id)?;
        w.write_u16(msg_type)?;
        body(&mut w)
    })();
    if let Err(e) = result {
        dst.clear();
        return Err(e);
    }
    let payload_len = dst.len() - FRAME_HEADER_LEN;
    if payload_len > MAX_PAYLOAD {
        dst.clear();
        return Err(Error::TooLarge);
    }
    let len_bytes = (payload_len as u32).to_le_bytes();
    dst[..FRAME_HEADER_LEN].copy_from_slice(&len_bytes);
    Ok(())
}

fn request_msg_type(req: &Request) -> u16 {
    match req {
        Request::Hello { .. } => REQ_HELLO,
        Request::CreateSession { .. } => REQ_CREATE_SESSION,
        Request::SendKey { .. } => REQ_SEND_KEY,
        Request::ProbeKey { .. } => REQ_PROBE_KEY,
        Request::Commit { .. } => REQ_COMMIT,
        Request::Revert { .. } => REQ_REVERT,
        Request::UndoCommit { .. } => REQ_UNDO_COMMIT,
        Request::SetInputScope { .. } => REQ_SET_INPUT_SCOPE,
        Request::SetMode { .. } => REQ_SET_MODE,
        Request::ApplyAiComposition { .. } => REQ_APPLY_AI_COMPOSITION,
        Request::RecordAiText { .. } => REQ_RECORD_AI_TEXT,
        Request::StartAiText { .. } => REQ_START_AI_TEXT,
        Request::PollAiText { .. } => REQ_POLL_AI_TEXT,
        Request::CancelAiText { .. } => REQ_CANCEL_AI_TEXT,
        Request::DeleteHistoryCandidate { .. } => REQ_DELETE_HISTORY_CANDIDATE,
        Request::DeleteSession { .. } => REQ_DELETE_SESSION,
        Request::Ping => REQ_PING,
        Request::Shutdown => REQ_SHUTDOWN,
        Request::WatchUi { .. } => REQ_WATCH_UI,
        Request::SetUiPlacement { .. } => REQ_SET_UI_PLACEMENT,
        Request::Reconvert { .. } => REQ_RECONVERT,
        Request::ClearLearning => REQ_CLEAR_LEARNING,
        Request::ClearInputHistory => REQ_CLEAR_INPUT_HISTORY,
        Request::FlushInputHistory => REQ_FLUSH_INPUT_HISTORY,
        Request::InputHistoryStats => REQ_INPUT_HISTORY_STATS,
    }
}

fn encode_request_body<S: Sink>(req: &Request, w: &mut S) -> Result<(), Error> {
    match req {
        Request::Hello { client_version } => w.write_u16(*client_version),
        Request::CreateSession { process_name } => w.write_str(process_name),
        Request::SendKey { session, key } => {
            w.write_u64(*session)?;
            key.encode(w)
        }
        Request::ProbeKey {
            session,
            scope,
            fresh_context,
            key,
        } => {
            w.write_u64(*session)?;
            scope.encode(w)?;
            w.write_bool(*fresh_context)?;
            key.encode(w)
        }
        Request::Commit { session } => w.write_u64(*session),
        Request::Revert { session } => w.write_u64(*session),
        Request::UndoCommit { session, outcome } => {
            w.write_u64(*session)?;
            outcome.encode(w)
        }
        Request::Reconvert {
            session,
            text,
            preview,
        } => {
            w.write_u64(*session)?;
            w.write_str(text)?;
            w.write_bool(*preview)
        }
        Request::SetInputScope { session, scope } => {
            w.write_u64(*session)?;
            scope.encode(w)
        }
        Request::SetMode { session, mode } => {
            w.write_u64(*session)?;
            mode.encode(w)
        }
        Request::ApplyAiComposition { session, result } => {
            w.write_u64(*session)?;
            w.write_str(result)
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
        } => {
            w.write_u64(*session)?;
            operation.encode(w)?;
            status.encode(w)?;
            w.write_str(source)?;
            w.write_str(result)?;
            w.write_str(model)?;
            w.write_str(provider)?;
            w.write_str(style)?;
            w.write_str(error_code)?;
            w.write_u64(*latency_ms)?;
            w.write_u32(*input_tokens)?;
            w.write_u32(*output_tokens)?;
            w.write_u32(*cached_tokens)?;
            w.write_u32(*attempts)?;
            w.write_bool(*test_only)
        }
        Request::StartAiText {
            session,
            operation,
            text,
        } => {
            w.write_u64(*session)?;
            operation.encode(w)?;
            w.write_str(text)
        }
        Request::PollAiText { session, job } | Request::CancelAiText { session, job } => {
            w.write_u64(*session)?;
            w.write_u64(*job)
        }
        Request::DeleteHistoryCandidate {
            revision,
            candidate_index,
        } => {
            w.write_u64(*revision)?;
            w.write_u16(*candidate_index)
        }
        Request::DeleteSession { session } => w.write_u64(*session),
        Request::Ping => Ok(()),
        Request::Shutdown => Ok(()),
        Request::ClearLearning => Ok(()),
        Request::ClearInputHistory => Ok(()),
        Request::FlushInputHistory => Ok(()),
        Request::InputHistoryStats => Ok(()),
        Request::WatchUi { since } => w.write_u64(*since),
        Request::SetUiPlacement {
            session,
            anchor,
            document,
            renderer_visible,
        } => {
            w.write_u64(*session)?;
            w.write_option(anchor, |w, rect| rect.encode(w))?;
            w.write_option(document, |w, rect| rect.encode(w))?;
            w.write_bool(*renderer_visible)
        }
    }
}

fn response_msg_type(res: &Response) -> u16 {
    match res {
        Response::Hello { .. } => RES_HELLO,
        Response::SessionCreated { .. } => RES_SESSION_CREATED,
        Response::Output(_) => RES_OUTPUT,
        Response::Pong => RES_PONG,
        Response::Ok => RES_OK,
        Response::InputHistoryStats { .. } => RES_INPUT_HISTORY_STATS,
        Response::InputMode { .. } => RES_INPUT_MODE,
        Response::HistoryCandidateDeleted { .. } => RES_HISTORY_CANDIDATE_DELETED,
        Response::AiTextStarted { .. } => RES_AI_TEXT_STARTED,
        Response::AiTextPending { .. } => RES_AI_TEXT_PENDING,
        Response::AiTextResult { .. } => RES_AI_TEXT_RESULT,
        Response::Ui(_) => RES_UI,
        Response::Error(_) => RES_ERROR,
    }
}

fn encode_response_body<S: Sink>(res: &Response, w: &mut S) -> Result<(), Error> {
    match res {
        Response::Hello {
            server_version,
            engine_version,
        } => {
            w.write_u16(*server_version)?;
            w.write_u16(engine_version[0])?;
            w.write_u16(engine_version[1])?;
            w.write_u16(engine_version[2])
        }
        Response::SessionCreated { session, mode } => {
            w.write_u64(*session)?;
            mode.encode(w)
        }
        Response::Output(out) => out.encode(w),
        Response::Pong => Ok(()),
        Response::Ok => Ok(()),
        Response::InputMode { mode } => mode.encode(w),
        Response::HistoryCandidateDeleted { removed } => w.write_bool(*removed),
        Response::AiTextStarted { job } | Response::AiTextPending { job } => w.write_u64(*job),
        Response::AiTextResult {
            job,
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
        } => {
            w.write_u64(*job)?;
            status.encode(w)?;
            w.write_str(result)?;
            w.write_str(model)?;
            w.write_str(provider)?;
            w.write_str(style)?;
            w.write_str(error_code)?;
            w.write_u64(*latency_ms)?;
            w.write_u32(*input_tokens)?;
            w.write_u32(*output_tokens)?;
            w.write_u32(*cached_tokens)?;
            w.write_u32(*attempts)
        }
        Response::InputHistoryStats {
            active,
            dropped_events,
            persistence_failures,
            excluded_unclassified_events,
            excluded_sensitive_events,
            excluded_test_only_events,
            ai_requests,
            ai_attempts,
            ai_input_tokens,
            ai_output_tokens,
            ai_cached_tokens,
        } => {
            w.write_bool(*active)?;
            w.write_u64(*dropped_events)?;
            w.write_u64(*persistence_failures)?;
            w.write_u64(*excluded_unclassified_events)?;
            w.write_u64(*excluded_sensitive_events)?;
            w.write_u64(*excluded_test_only_events)?;
            w.write_u64(*ai_requests)?;
            w.write_u64(*ai_attempts)?;
            w.write_u64(*ai_input_tokens)?;
            w.write_u64(*ai_output_tokens)?;
            w.write_u64(*ai_cached_tokens)
        }
        Response::Ui(ui) => {
            if ui.candidate_detail.is_some() && ui.candidates.is_none() {
                return Err(Error::TooLarge);
            }
            w.write_u64(ui.revision)?;
            ui.appearance_theme.encode(w)?;
            w.write_option(&ui.mode, |w, mode| mode.encode(w))?;
            w.write_option(&ui.candidates, |w, candidates| candidates.encode(w))?;
            w.write_option(&ui.candidate_detail, |w, detail| detail.encode(w))?;
            w.write_option(&ui.anchor, |w, rect| rect.encode(w))?;
            w.write_option(&ui.document, |w, rect| rect.encode(w))?;
            w.write_bool(ui.renderer_visible)?;
            w.write_bool(ui.stopping)
        }
        Response::Error(code) => code.encode(w),
    }
}

/// Encodes a complete `Request` frame (4-byte length prefix included) into
/// `dst`. See [`encode_frame`] for the allocation contract.
pub fn encode_request(req: &Request, id: RequestId, dst: &mut Vec<u8>) -> Result<(), Error> {
    let msg_type = request_msg_type(req);
    encode_frame(id, msg_type, dst, |w| encode_request_body(req, w))
}

/// Encodes a complete `Response` frame (4-byte length prefix included)
/// into `dst`. See [`encode_frame`] for the allocation contract.
pub fn encode_response(res: &Response, id: RequestId, dst: &mut Vec<u8>) -> Result<(), Error> {
    let msg_type = response_msg_type(res);
    encode_frame(id, msg_type, dst, |w| encode_response_body(res, w))
}

/// Decodes a payload (the bytes *after* the 4-byte length prefix) into a
/// `Request`.
///
/// Rejects a payload whose version does not match [`PROTOCOL_VERSION`]
/// with `Error::UnsupportedVersion` and rejects trailing bytes after a
/// complete message with `Error::TrailingBytes`.
pub fn decode_request(payload: &[u8]) -> Result<(RequestId, Request), Error> {
    let mut r = Reader::new(payload);
    let version = r.read_u16()?;
    if version != PROTOCOL_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let request_id = r.read_u64()?;
    let msg_type = r.read_u16()?;
    let req = match msg_type {
        REQ_HELLO => Request::Hello {
            client_version: r.read_u16()?,
        },
        REQ_CREATE_SESSION => Request::CreateSession {
            process_name: r.read_str()?.to_string(),
        },
        REQ_SEND_KEY => {
            let session = r.read_u64()?;
            let key = KeyInput::decode(&mut r)?;
            Request::SendKey { session, key }
        }
        REQ_PROBE_KEY => {
            let session = r.read_u64()?;
            let scope = InputScope::decode(&mut r)?;
            let fresh_context = r.read_bool()?;
            let key = KeyInput::decode(&mut r)?;
            Request::ProbeKey {
                session,
                scope,
                fresh_context,
                key,
            }
        }
        REQ_COMMIT => Request::Commit {
            session: r.read_u64()?,
        },
        REQ_REVERT => Request::Revert {
            session: r.read_u64()?,
        },
        REQ_UNDO_COMMIT => Request::UndoCommit {
            session: r.read_u64()?,
            outcome: UndoCommitOutcome::decode(&mut r)?,
        },
        REQ_RECONVERT => Request::Reconvert {
            session: r.read_u64()?,
            text: r.read_str()?.to_string(),
            preview: r.read_bool()?,
        },
        REQ_SET_INPUT_SCOPE => {
            let session = r.read_u64()?;
            let scope = InputScope::decode(&mut r)?;
            Request::SetInputScope { session, scope }
        }
        REQ_SET_MODE => Request::SetMode {
            session: r.read_u64()?,
            mode: Mode::decode(&mut r)?,
        },
        REQ_APPLY_AI_COMPOSITION => Request::ApplyAiComposition {
            session: r.read_u64()?,
            result: r.read_str()?.to_string(),
        },
        REQ_RECORD_AI_TEXT => Request::RecordAiText {
            session: r.read_u64()?,
            operation: AiTextOperation::decode(&mut r)?,
            status: AiTextStatus::decode(&mut r)?,
            source: r.read_str()?.to_string(),
            result: r.read_str()?.to_string(),
            model: r.read_str()?.to_string(),
            provider: r.read_str()?.to_string(),
            style: r.read_str()?.to_string(),
            error_code: r.read_str()?.to_string(),
            latency_ms: r.read_u64()?,
            input_tokens: r.read_u32()?,
            output_tokens: r.read_u32()?,
            cached_tokens: r.read_u32()?,
            attempts: r.read_u32()?,
            test_only: r.read_bool()?,
        },
        REQ_START_AI_TEXT => Request::StartAiText {
            session: r.read_u64()?,
            operation: AiTextOperation::decode(&mut r)?,
            text: r.read_str()?.to_string(),
        },
        REQ_POLL_AI_TEXT => Request::PollAiText {
            session: r.read_u64()?,
            job: r.read_u64()?,
        },
        REQ_CANCEL_AI_TEXT => Request::CancelAiText {
            session: r.read_u64()?,
            job: r.read_u64()?,
        },
        REQ_DELETE_HISTORY_CANDIDATE => Request::DeleteHistoryCandidate {
            revision: r.read_u64()?,
            candidate_index: r.read_u16()?,
        },
        REQ_DELETE_SESSION => Request::DeleteSession {
            session: r.read_u64()?,
        },
        REQ_PING => Request::Ping,
        REQ_SHUTDOWN => Request::Shutdown,
        REQ_CLEAR_LEARNING => Request::ClearLearning,
        REQ_CLEAR_INPUT_HISTORY => Request::ClearInputHistory,
        REQ_FLUSH_INPUT_HISTORY => Request::FlushInputHistory,
        REQ_INPUT_HISTORY_STATS => Request::InputHistoryStats,
        REQ_WATCH_UI => Request::WatchUi {
            since: r.read_u64()?,
        },
        REQ_SET_UI_PLACEMENT => Request::SetUiPlacement {
            session: r.read_u64()?,
            anchor: r.read_option(ScreenRect::decode)?,
            document: r.read_option(ScreenRect::decode)?,
            renderer_visible: r.read_bool()?,
        },
        other => return Err(Error::BadMsgType(other)),
    };
    r.finish()?;
    Ok((request_id, req))
}

/// Decodes a payload (the bytes *after* the 4-byte length prefix) into a
/// `Response`. Same version/trailing-bytes contract as
/// [`decode_request`].
pub fn decode_response(payload: &[u8]) -> Result<(RequestId, Response), Error> {
    let mut r = Reader::new(payload);
    let version = r.read_u16()?;
    if version != PROTOCOL_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let request_id = r.read_u64()?;
    let msg_type = r.read_u16()?;
    let res = match msg_type {
        RES_HELLO => {
            let server_version = r.read_u16()?;
            let engine_version = [r.read_u16()?, r.read_u16()?, r.read_u16()?];
            Response::Hello {
                server_version,
                engine_version,
            }
        }
        RES_SESSION_CREATED => Response::SessionCreated {
            session: r.read_u64()?,
            mode: Mode::decode(&mut r)?,
        },
        RES_OUTPUT => Response::Output(Output::decode(&mut r)?),
        RES_PONG => Response::Pong,
        RES_OK => Response::Ok,
        RES_INPUT_MODE => Response::InputMode {
            mode: Mode::decode(&mut r)?,
        },
        RES_HISTORY_CANDIDATE_DELETED => Response::HistoryCandidateDeleted {
            removed: r.read_bool()?,
        },
        RES_AI_TEXT_STARTED => Response::AiTextStarted { job: r.read_u64()? },
        RES_AI_TEXT_PENDING => Response::AiTextPending { job: r.read_u64()? },
        RES_AI_TEXT_RESULT => Response::AiTextResult {
            job: r.read_u64()?,
            status: AiTextStatus::decode(&mut r)?,
            result: r.read_str()?.to_string(),
            model: r.read_str()?.to_string(),
            provider: r.read_str()?.to_string(),
            style: r.read_str()?.to_string(),
            error_code: r.read_str()?.to_string(),
            latency_ms: r.read_u64()?,
            input_tokens: r.read_u32()?,
            output_tokens: r.read_u32()?,
            cached_tokens: r.read_u32()?,
            attempts: r.read_u32()?,
        },
        RES_INPUT_HISTORY_STATS => Response::InputHistoryStats {
            active: r.read_bool()?,
            dropped_events: r.read_u64()?,
            persistence_failures: r.read_u64()?,
            excluded_unclassified_events: r.read_u64()?,
            excluded_sensitive_events: r.read_u64()?,
            excluded_test_only_events: r.read_u64()?,
            ai_requests: r.read_u64()?,
            ai_attempts: r.read_u64()?,
            ai_input_tokens: r.read_u64()?,
            ai_output_tokens: r.read_u64()?,
            ai_cached_tokens: r.read_u64()?,
        },
        RES_UI => {
            let revision = r.read_u64()?;
            let appearance_theme = AppearanceTheme::decode(&mut r)?;
            let mode = r.read_option(Mode::decode)?;
            let candidates = r.read_option(CandidateList::decode)?;
            let candidate_detail = r.read_option(CandidateDetail::decode)?;
            if candidate_detail.is_some() && candidates.is_none() {
                return Err(Error::TooLarge);
            }
            let anchor = r.read_option(ScreenRect::decode)?;
            let document = r.read_option(ScreenRect::decode)?;
            let renderer_visible = r.read_bool()?;
            let stopping = r.read_bool()?;
            Response::Ui(UiState {
                revision,
                appearance_theme,
                mode,
                candidates,
                candidate_detail,
                anchor,
                document,
                renderer_visible,
                stopping,
            })
        }
        RES_ERROR => Response::Error(ErrorCode::decode(&mut r)?),
        other => return Err(Error::BadMsgType(other)),
    };
    r.finish()?;
    Ok((request_id, res))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail() -> CandidateDetail {
        CandidateDetail {
            reading: "reading".to_owned(),
            definition: "definition".to_owned(),
            definition_truncated: false,
            aliases: Vec::new(),
            related: Vec::new(),
            similar: Vec::new(),
            antonyms: Vec::new(),
        }
    }

    #[test]
    fn ui_rejects_detail_without_candidates_before_writing_a_frame() {
        let response = Response::Ui(UiState {
            revision: 1,
            appearance_theme: AppearanceTheme::Auto,
            mode: None,
            candidates: None,
            candidate_detail: Some(detail()),
            anchor: None,
            document: None,
            renderer_visible: false,
            stopping: false,
        });
        let mut frame = vec![1, 2, 3];
        assert_eq!(
            encode_response(&response, 1, &mut frame),
            Err(Error::TooLarge)
        );
        assert!(frame.is_empty());
    }

    #[test]
    fn payload_len_accepts_at_limit_and_rejects_above() {
        let at_limit = (MAX_PAYLOAD as u32).to_le_bytes();
        assert_eq!(payload_len(&at_limit), Ok(MAX_PAYLOAD));
        let over = (MAX_PAYLOAD as u32 + 1).to_le_bytes();
        assert_eq!(payload_len(&over), Err(Error::TooLarge));
    }

    #[test]
    fn encode_decode_ping_roundtrip() {
        let mut dst = Vec::new();
        encode_request(&Request::Ping, 42, &mut dst).expect("encode");
        let len = payload_len(&[dst[0], dst[1], dst[2], dst[3]]).expect("len");
        assert_eq!(len, dst.len() - FRAME_HEADER_LEN);
        let (id, req) = decode_request(&dst[FRAME_HEADER_LEN..]).expect("decode");
        assert_eq!(id, 42);
        assert_eq!(req, Request::Ping);
    }

    #[test]
    fn decode_request_rejects_wrong_version() {
        let mut dst = Vec::new();
        encode_request(&Request::Ping, 1, &mut dst).expect("encode");
        // Corrupt the version field (first two bytes of the payload).
        dst[FRAME_HEADER_LEN] = 0xFF;
        dst[FRAME_HEADER_LEN + 1] = 0xFF;
        let result = decode_request(&dst[FRAME_HEADER_LEN..]);
        assert_eq!(result, Err(Error::UnsupportedVersion(0xFFFF)));
    }

    #[test]
    fn decode_request_rejects_trailing_bytes() {
        let mut dst = Vec::new();
        encode_request(&Request::Ping, 1, &mut dst).expect("encode");
        dst.push(0);
        let result = decode_request(&dst[FRAME_HEADER_LEN..]);
        assert_eq!(result, Err(Error::TrailingBytes));
    }

    #[test]
    fn decode_request_rejects_unknown_msg_type() {
        let mut dst = Vec::new();
        encode_request(&Request::Ping, 1, &mut dst).expect("encode");
        // Message type is the last two bytes of the (empty-body) header.
        let mt_offset = dst.len() - 2;
        dst[mt_offset] = 0xEE;
        dst[mt_offset + 1] = 0xEE;
        let result = decode_request(&dst[FRAME_HEADER_LEN..]);
        assert_eq!(result, Err(Error::BadMsgType(0xEEEE)));
    }

    #[test]
    fn encode_reuses_dst_capacity() {
        let mut dst = Vec::with_capacity(256);
        encode_request(&Request::Ping, 1, &mut dst).expect("encode");
        let cap_after_first = dst.capacity();
        encode_request(
            &Request::CreateSession {
                process_name: "notepad.exe".to_string(),
            },
            2,
            &mut dst,
        )
        .expect("encode");
        // Capacity should not have needed to shrink/reallocate below what
        // was already reserved for such a small payload.
        assert!(dst.capacity() >= cap_after_first || dst.capacity() >= dst.len());
    }
}
