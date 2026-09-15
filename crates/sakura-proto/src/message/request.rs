//! Client-to-engine messages: the [`Request`](super::Request) enum and its body codec.

use sakura_values::{AiTextOperation, AiTextStatus};

use crate::types::{InputScope, KeyInput, Mode, ScreenRect};
use crate::wire::{Reader, Sink};
use crate::wire_types::Wire;
use crate::{RequestId, Revision, SessionId, PROTOCOL_VERSION};

use super::header::encode_frame;
use super::tags::*;
use super::Error;

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
    /// Clears context that is meaningful only while the caret remains directly
    /// after the last committed run. The frontend sends this before the next
    /// real key when exact TSF range validation can no longer prove adjacency.
    ResetDocumentContext {
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
    /// Reads the engine's per-stage timing accumulators. The reply carries
    /// counts and durations only, never text, so it can be collected from a
    /// real session without recording what was typed.
    EngineTiming,
    /// Reads which deterministic key-path delays this engine has armed, and
    /// what they have actually done. Content-free: points, durations and
    /// counts only. A shipping engine cannot arm any of them, so this reply is
    /// how a stress run proves the delay it configured was really applied, and
    /// how anyone can prove a user's engine has none.
    FaultStatus,
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
    /// Queues a passive renderer row click against the exact displayed UI
    /// revision. The renderer owns no editing session, so this request only
    /// creates a bounded intent for the candidate-owning TSF connection.
    QueueCandidateCommit {
        revision: Revision,
        candidate_index: u16,
    },
    /// Reads a queued renderer click only when this exact session owns the
    /// current candidate snapshot. This request never mutates session state.
    PollCandidateCommit {
        session: SessionId,
    },
    /// Commits the exact revision-stamped row previously queued by the
    /// renderer. The engine rejects stale, foreign, or already-consumed
    /// intents before advancing the editing session.
    CommitCandidate {
        session: SessionId,
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
    /// the reply until [`UiState::revision`](super::UiState::revision) moves past `since`, or until a
    /// heartbeat interval passes, and answers [`Response::Ui`](super::Response::Ui) either way.
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
        /// reports one. See [`UiState::document`](super::UiState::document).
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

fn request_msg_type(req: &Request) -> u16 {
    match req {
        Request::Hello { .. } => REQ_HELLO,
        Request::CreateSession { .. } => REQ_CREATE_SESSION,
        Request::SendKey { .. } => REQ_SEND_KEY,
        Request::ProbeKey { .. } => REQ_PROBE_KEY,
        Request::Commit { .. } => REQ_COMMIT,
        Request::Revert { .. } => REQ_REVERT,
        Request::ResetDocumentContext { .. } => REQ_RESET_DOCUMENT_CONTEXT,
        Request::UndoCommit { .. } => REQ_UNDO_COMMIT,
        Request::SetInputScope { .. } => REQ_SET_INPUT_SCOPE,
        Request::SetMode { .. } => REQ_SET_MODE,
        Request::ApplyAiComposition { .. } => REQ_APPLY_AI_COMPOSITION,
        Request::RecordAiText { .. } => REQ_RECORD_AI_TEXT,
        Request::StartAiText { .. } => REQ_START_AI_TEXT,
        Request::PollAiText { .. } => REQ_POLL_AI_TEXT,
        Request::CancelAiText { .. } => REQ_CANCEL_AI_TEXT,
        Request::DeleteHistoryCandidate { .. } => REQ_DELETE_HISTORY_CANDIDATE,
        Request::QueueCandidateCommit { .. } => REQ_QUEUE_CANDIDATE_COMMIT,
        Request::PollCandidateCommit { .. } => REQ_POLL_CANDIDATE_COMMIT,
        Request::CommitCandidate { .. } => REQ_COMMIT_CANDIDATE,
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
        Request::EngineTiming => REQ_ENGINE_TIMING,
        Request::FaultStatus => REQ_FAULT_STATUS,
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
        Request::ResetDocumentContext { session } => w.write_u64(*session),
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
        }
        | Request::QueueCandidateCommit {
            revision,
            candidate_index,
        } => {
            w.write_u64(*revision)?;
            w.write_u16(*candidate_index)
        }
        Request::PollCandidateCommit { session } => w.write_u64(*session),
        Request::CommitCandidate {
            session,
            revision,
            candidate_index,
        } => {
            w.write_u64(*session)?;
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
        Request::EngineTiming => Ok(()),
        Request::FaultStatus => Ok(()),
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

/// Encodes a complete `Request` frame (4-byte length prefix included) into
/// `dst`. See [`encode_frame`] for the allocation contract.
pub fn encode_request(req: &Request, id: RequestId, dst: &mut Vec<u8>) -> Result<(), Error> {
    let msg_type = request_msg_type(req);
    encode_frame(id, msg_type, dst, |w| encode_request_body(req, w))
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
        REQ_RESET_DOCUMENT_CONTEXT => Request::ResetDocumentContext {
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
        REQ_QUEUE_CANDIDATE_COMMIT => Request::QueueCandidateCommit {
            revision: r.read_u64()?,
            candidate_index: r.read_u16()?,
        },
        REQ_POLL_CANDIDATE_COMMIT => Request::PollCandidateCommit {
            session: r.read_u64()?,
        },
        REQ_COMMIT_CANDIDATE => Request::CommitCandidate {
            session: r.read_u64()?,
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
        REQ_ENGINE_TIMING => Request::EngineTiming,
        REQ_FAULT_STATUS => Request::FaultStatus,
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
