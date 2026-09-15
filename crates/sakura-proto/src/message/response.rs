//! Engine-to-client messages: the [`Response`](super::Response) enum and its body codec.

use sakura_values::AiTextStatus;

use crate::types::{
    AppearanceTheme, CandidateDetail, CandidateList, EngineTimingEntry, EngineTimingSite,
    ErrorCode, FaultInjectionEntry, FaultPoint, Mode, Output, PadShortcut, ScreenRect,
};
use crate::wire::{Reader, Sink};
use crate::wire_types::Wire;
use crate::{RequestId, Revision, SessionId, PROTOCOL_VERSION};

use super::header::encode_frame;
use super::tags::*;
use super::{Error, UiState};

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
    /// The current mode after a successful [`Request::SetMode`](super::Request::SetMode).
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
    /// Answers [`Request::EngineTiming`](super::Request::EngineTiming): one entry per
    /// [`EngineTimingSite`], in declaration order.
    EngineTiming {
        entries: Vec<EngineTimingEntry>,
    },
    /// Answers [`Request::FaultStatus`](super::Request::FaultStatus): one entry per [`FaultPoint`], in
    /// declaration order. A production engine answers with every point
    /// disarmed, which is what makes "no delay was injected" a checkable
    /// claim rather than an assumption.
    FaultStatus {
        entries: Vec<FaultInjectionEntry>,
    },
    /// Answers [`Request::DeleteHistoryCandidate`](super::Request::DeleteHistoryCandidate). `false` is a terminal,
    /// fail-closed no-op for stale UI, a non-history row, disabled learning,
    /// a duplicate click, or a persistence failure. The renderer must wait
    /// for a later [`Response::Ui`](super::Response::Ui) before changing what it draws.
    HistoryCandidateDeleted {
        removed: bool,
    },
    /// Answers [`Request::QueueCandidateCommit`](super::Request::QueueCandidateCommit). A negative result is a
    /// terminal stale/invalid no-op; the renderer may accept a later click.
    CandidateCommitQueued {
        queued: bool,
    },
    /// Answers [`Request::PollCandidateCommit`](super::Request::PollCandidateCommit). `None` means this session
    /// owns no current click intent.
    CandidateCommitPending {
        request: Option<(Revision, u16)>,
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

fn response_msg_type(res: &Response) -> u16 {
    match res {
        Response::Hello { .. } => RES_HELLO,
        Response::SessionCreated { .. } => RES_SESSION_CREATED,
        Response::Output(_) => RES_OUTPUT,
        Response::Pong => RES_PONG,
        Response::Ok => RES_OK,
        Response::InputHistoryStats { .. } => RES_INPUT_HISTORY_STATS,
        Response::EngineTiming { .. } => RES_ENGINE_TIMING,
        Response::FaultStatus { .. } => RES_FAULT_STATUS,
        Response::InputMode { .. } => RES_INPUT_MODE,
        Response::HistoryCandidateDeleted { .. } => RES_HISTORY_CANDIDATE_DELETED,
        Response::CandidateCommitQueued { .. } => RES_CANDIDATE_COMMIT_QUEUED,
        Response::CandidateCommitPending { .. } => RES_CANDIDATE_COMMIT_PENDING,
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
        Response::CandidateCommitQueued { queued } => w.write_bool(*queued),
        Response::CandidateCommitPending { request } => w.write_option(request, |w, request| {
            w.write_u64(request.0)?;
            w.write_u16(request.1)
        }),
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
        Response::EngineTiming { entries } => {
            // The site set is fixed, so a longer list means the sender built
            // something the receiver cannot index. Refuse it here rather than
            // letting the reader decide what to drop.
            if entries.len() > EngineTimingSite::ALL.len() {
                return Err(Error::TooLarge);
            }
            w.write_count(entries.len())?;
            for entry in entries {
                entry.encode(w)?;
            }
            Ok(())
        }
        Response::FaultStatus { entries } => {
            if entries.len() > FaultPoint::ALL.len() {
                return Err(Error::TooLarge);
            }
            w.write_count(entries.len())?;
            for entry in entries {
                entry.encode(w)?;
            }
            Ok(())
        }
        Response::Ui(ui) => {
            if ui.candidate_detail.is_some() && ui.candidates.is_none() {
                return Err(Error::TooLarge);
            }
            w.write_u64(ui.revision)?;
            ui.appearance_theme.encode(w)?;
            ui.pad_shortcut.encode(w)?;
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

/// Encodes a complete `Response` frame (4-byte length prefix included)
/// into `dst`. See [`encode_frame`] for the allocation contract.
pub fn encode_response(res: &Response, id: RequestId, dst: &mut Vec<u8>) -> Result<(), Error> {
    let msg_type = response_msg_type(res);
    encode_frame(id, msg_type, dst, |w| encode_response_body(res, w))
}

/// Decodes a payload (the bytes *after* the 4-byte length prefix) into a
/// `Response`. Same version/trailing-bytes contract as
/// [`decode_request`](super::decode_request).
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
        RES_CANDIDATE_COMMIT_QUEUED => Response::CandidateCommitQueued {
            queued: r.read_bool()?,
        },
        RES_CANDIDATE_COMMIT_PENDING => Response::CandidateCommitPending {
            request: r.read_option(|r| Ok((r.read_u64()?, r.read_u16()?)))?,
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
        RES_ENGINE_TIMING => {
            let count = usize::from(r.read_count()?);
            // A frame claiming more entries than there are sites cannot have
            // come from a build this one can read. Stop before allocating for
            // a length that a peer chose.
            if count > EngineTimingSite::ALL.len() {
                return Err(Error::BadEnum);
            }
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                entries.push(EngineTimingEntry::decode(&mut r)?);
            }
            Response::EngineTiming { entries }
        }
        RES_FAULT_STATUS => {
            let count = usize::from(r.read_count()?);
            // As above: the point set is fixed, so a longer list cannot have
            // come from a build this one can read.
            if count > FaultPoint::ALL.len() {
                return Err(Error::BadEnum);
            }
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                entries.push(FaultInjectionEntry::decode(&mut r)?);
            }
            Response::FaultStatus { entries }
        }
        RES_UI => {
            let revision = r.read_u64()?;
            let appearance_theme = AppearanceTheme::decode(&mut r)?;
            let pad_shortcut = PadShortcut::decode(&mut r)?;
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
                pad_shortcut,
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
