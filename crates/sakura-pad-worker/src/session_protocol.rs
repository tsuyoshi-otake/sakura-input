//! Authenticated state machine for the persistent Pad crypto worker.

use std::io::{self, Read, Write};

pub use sakura_pad_session_proto::{
    encode_created_recovery, parse_created_recovery, read_request, read_response, write_request,
    write_response, Operation, Request, Response, Status,
};
use zeroize::Zeroizing;

use crate::envelope::{
    seal_with_recovery, unlock_with_password_v2, unlock_with_recovery_v2, RecoveryKey,
    UnlockedRecoveryEnvelope,
};
use crate::{seal, unlock, EnvelopeError, Scope, UnlockedEnvelope};

const _: () = assert!(sakura_pad_session_proto::MAX_PLAINTEXT_BYTES == crate::MAX_PLAINTEXT_BYTES);

#[allow(missing_debug_implementations)]
struct Session {
    unlocked: Option<UnlockedSession>,
    generation: u64,
    last_request_id: u64,
    shutdown: bool,
}

enum UnlockedSession {
    V1(UnlockedEnvelope),
    V2(UnlockedRecoveryEnvelope),
}

impl UnlockedSession {
    fn reseal(&self, plaintext: &[u8]) -> Result<Vec<u8>, EnvelopeError> {
        match self {
            Self::V1(session) => session.reseal(plaintext),
            Self::V2(session) => session.reseal(plaintext),
        }
    }

    fn open_authenticated(&self, envelope: &[u8]) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
        match self {
            Self::V1(session) => session.open_authenticated(envelope),
            Self::V2(session) => session.open_authenticated(envelope),
        }
    }
}

fn scope(request: &Request) -> Scope {
    if request.memo_id == 0 {
        Scope::pad(request.vault_id)
    } else {
        Scope::memo(request.vault_id, request.memo_id)
    }
}

impl Session {
    const fn new() -> Self {
        Self {
            unlocked: None,
            generation: 0,
            last_request_id: 0,
            shutdown: false,
        }
    }

    const fn is_shutdown(&self) -> bool {
        self.shutdown
    }

    fn handle(&mut self, request: Request) -> Response {
        let mut response = Response {
            id: request.id,
            generation: request.generation,
            status: Status::Rejected,
            payload: Zeroizing::new(Vec::new()),
        };
        if self.shutdown || request.id <= self.last_request_id {
            response.status = Status::Stale;
            return response;
        }
        self.last_request_id = request.id;
        match request.operation {
            Operation::Unlock | Operation::UnlockPasswordV2 | Operation::UnlockRecoveryV2 => {
                if self.unlocked.is_some() || request.generation <= self.generation {
                    response.status = Status::Stale;
                } else {
                    self.generation = request.generation;
                    let scope = scope(&request);
                    let result = match request.operation {
                        Operation::Unlock => unlock(scope, &request.password, &request.payload)
                            .map(|(plaintext, session)| (plaintext, UnlockedSession::V1(session))),
                        Operation::UnlockPasswordV2 => {
                            unlock_with_password_v2(scope, &request.password, &request.payload).map(
                                |(plaintext, session)| (plaintext, UnlockedSession::V2(session)),
                            )
                        }
                        Operation::UnlockRecoveryV2 => std::str::from_utf8(&request.password)
                            .map_err(|_| EnvelopeError::InvalidInput)
                            .and_then(RecoveryKey::decode)
                            .and_then(|key| unlock_with_recovery_v2(scope, &key, &request.payload))
                            .map(|(plaintext, session)| (plaintext, UnlockedSession::V2(session))),
                        _ => unreachable!(),
                    };
                    match result {
                        Ok((plaintext, unlocked)) => {
                            self.unlocked = Some(unlocked);
                            response.status = Status::Success;
                            response.payload = plaintext;
                        }
                        Err(error) => response.status = status_for(error),
                    }
                }
            }
            Operation::Create | Operation::CreateRecoverable => {
                if self.unlocked.is_some() || request.generation <= self.generation {
                    response.status = Status::Stale;
                } else {
                    self.generation = request.generation;
                    let scope = scope(&request);
                    if request.operation == Operation::Create {
                        match seal(scope, &request.password, &request.payload) {
                            Ok(envelope) => match unlock(scope, &request.password, &envelope) {
                                Ok((_plaintext, unlocked)) => {
                                    self.unlocked = Some(UnlockedSession::V1(unlocked));
                                    response.status = Status::Success;
                                    response.payload = Zeroizing::new(envelope);
                                }
                                Err(error) => response.status = status_for(error),
                            },
                            Err(error) => response.status = status_for(error),
                        }
                    } else {
                        match seal_with_recovery(scope, &request.password, &request.payload) {
                            Ok((envelope, key)) => {
                                let envelope = Zeroizing::new(envelope);
                                match unlock_with_password_v2(scope, &request.password, &envelope) {
                                    Ok((_plaintext, unlocked)) => {
                                        let display = key.encode();
                                        match encode_created_recovery(&envelope, &display) {
                                            Ok(payload) => {
                                                self.unlocked = Some(UnlockedSession::V2(unlocked));
                                                response.status = Status::Success;
                                                response.payload = payload;
                                            }
                                            Err(_) => response.status = Status::Unavailable,
                                        }
                                    }
                                    Err(error) => response.status = status_for(error),
                                }
                            }
                            Err(error) => response.status = status_for(error),
                        }
                    }
                }
            }
            Operation::Reseal => {
                if request.generation != self.generation {
                    response.status = Status::Stale;
                } else if let Some(unlocked) = &self.unlocked {
                    match unlocked.reseal(&request.payload) {
                        Ok(envelope) => {
                            response.status = Status::Success;
                            response.payload = Zeroizing::new(envelope);
                        }
                        Err(error) => {
                            self.unlocked = None;
                            response.status = status_for(error);
                        }
                    }
                } else {
                    response.status = Status::Locked;
                }
            }
            Operation::Verify => {
                if request.generation != self.generation {
                    response.status = Status::Stale;
                } else if let Some(unlocked) = &self.unlocked {
                    match unlocked.open_authenticated(&request.payload) {
                        Ok(plaintext) => {
                            response.status = Status::Success;
                            response.payload = plaintext;
                        }
                        Err(error) => response.status = status_for(error),
                    }
                } else {
                    response.status = Status::Locked;
                }
            }
            Operation::Lock => {
                if request.generation != self.generation {
                    response.status = Status::Stale;
                } else {
                    self.unlocked = None;
                    response.status = Status::Success;
                }
            }
            Operation::Shutdown => {
                if request.generation != self.generation && self.generation != 0 {
                    response.status = Status::Stale;
                } else {
                    self.unlocked = None;
                    self.shutdown = true;
                    response.status = Status::Success;
                }
            }
        }
        response
    }
}

fn status_for(error: EnvelopeError) -> Status {
    match error {
        EnvelopeError::EntropyUnavailable
        | EnvelopeError::KdfFailed
        | EnvelopeError::CryptoFailed => Status::Unavailable,
        EnvelopeError::InvalidInput
        | EnvelopeError::InvalidEnvelope
        | EnvelopeError::AuthenticationFailed => Status::Rejected,
    }
}

pub fn run(
    mut reader: impl Read,
    mut writer: impl Write,
    mut completed: impl FnMut(),
) -> io::Result<()> {
    let mut session = Session::new();
    loop {
        let request = match read_request(&mut reader) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(_) => {
                let response = Response {
                    id: 0,
                    generation: 0,
                    status: Status::InvalidRequest,
                    payload: Zeroizing::new(Vec::new()),
                };
                write_response(&mut writer, &response)?;
                writer.flush()?;
                return Ok(());
            }
        };
        let response = session.handle(request);
        write_response(&mut writer, &response)?;
        writer.flush()?;
        completed();
        if session.is_shutdown() {
            return Ok(());
        }
    }
}
