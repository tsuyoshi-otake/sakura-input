//! Experimental crypto worker. Not installed or connected to the Pad UI yet.
use std::io::{self, Write};
use std::time::Duration;

use sakura_pad_worker::protocol::{self, Operation, Response, Status};
use sakura_pad_worker::{open, seal, EnvelopeError, Scope};
use zeroize::Zeroizing;

fn run() -> io::Result<()> {
    // Even an abandoned/truncated stdin cannot leave this process waiting
    // forever. The parent must separately enforce cancellation and reap it.
    std::thread::Builder::new()
        .name("pad-deadline".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(30));
            std::process::exit(124);
        })?;
    let response = match protocol::read_request(io::stdin().lock()) {
        Ok(request) => {
            let scope = if request.memo_id == 0 {
                Scope::pad(request.vault_id)
            } else {
                Scope::memo(request.vault_id, request.memo_id)
            };
            let result = match request.operation {
                Operation::Seal => {
                    seal(scope, &request.password, &request.payload).map(Zeroizing::new)
                }
                Operation::Open => open(scope, &request.password, &request.payload),
            };
            match result {
                Ok(payload) => Response {
                    id: request.id,
                    status: Status::Success,
                    payload,
                },
                Err(error) => Response {
                    id: request.id,
                    status: match error {
                        EnvelopeError::EntropyUnavailable
                        | EnvelopeError::KdfFailed
                        | EnvelopeError::CryptoFailed => Status::Unavailable,
                        EnvelopeError::InvalidInput
                        | EnvelopeError::InvalidEnvelope
                        | EnvelopeError::AuthenticationFailed => Status::Rejected,
                    },
                    payload: Zeroizing::new(Vec::new()),
                },
            }
        }
        Err(_) => Response {
            id: 0,
            status: Status::InvalidRequest,
            payload: Zeroizing::new(Vec::new()),
        },
    };
    let mut output = io::stdout().lock();
    protocol::write_response(&mut output, &response)?;
    output.flush()
}

fn main() {
    // No public CLI and no credential arguments. Content-free diagnostics only.
    if std::env::args_os().len() != 1 || run().is_err() {
        eprintln!("Pad worker could not complete its request");
        std::process::exit(1);
    }
}
