//! Renderer transport for the isolated Pad session child.
//!
//! `exchange` blocks for at most its caller-supplied deadline and must run off
//! the UI thread. The UI also owns its own generation: after cancellation or
//! editing another memo, it must discard responses for the old UI generation.
//! This transport checks only the worker protocol's request/generation echo.

use std::io::{self, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sakura_pad_session_proto::{self as wire, Operation, Request, Response, Status};

/// Content-free transport failure. No OS path or secret bytes enter diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientError {
    Launch,
    Transport,
    Protocol,
    Timeout,
    Closed,
}

type Reply = Sender<Result<Response, ClientError>>;

struct Exchange {
    request: Request,
    reply: Reply,
}

/// One owned child and one serial pipe actor. Each failure makes it terminal.
#[allow(missing_debug_implementations)]
pub struct PadCryptoClient {
    cancellation: PadCryptoCancellation,
    sender: Option<Sender<Exchange>>,
    actor: Option<JoinHandle<()>>,
    closed: bool,
}

/// Cloneable control for ending precisely this child during a blocked exchange.
#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct PadCryptoCancellation {
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
}

impl PadCryptoCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Ok(mut child) = self.child.lock() {
            kill_and_reap(&mut child);
        }
    }
}

impl PadCryptoClient {
    /// Launch only the sibling session image beside the current renderer.
    pub fn start() -> Result<Self, ClientError> {
        let mut sibling = std::env::current_exe().map_err(|_| ClientError::Launch)?;
        sibling.set_file_name("sakura_pad_session.exe");
        Self::start_image(&sibling)
    }

    /// Explicit image injection for isolated real-process tests. Production
    /// always starts only the fixed sibling returned by `start`.
    #[cfg(test)]
    pub(crate) fn start_at(image: &Path) -> Result<Self, ClientError> {
        Self::start_image(image)
    }

    fn start_image(image: &Path) -> Result<Self, ClientError> {
        if !image.is_absolute() {
            return Err(ClientError::Launch);
        }
        let mut command = Command::new(image);
        command
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = command.spawn().map_err(|_| ClientError::Launch)?;
        Self::from_child(child)
    }

    fn from_child(mut child: Child) -> Result<Self, ClientError> {
        let Some(input) = child.stdin.take() else {
            kill_and_reap(&mut child);
            return Err(ClientError::Launch);
        };
        let Some(output) = child.stdout.take() else {
            kill_and_reap(&mut child);
            return Err(ClientError::Launch);
        };
        let (sender, receiver) = mpsc::channel();
        let actor = match thread::Builder::new()
            .name("pad-crypto-io".into())
            .spawn(move || actor_loop(input, output, receiver))
        {
            Ok(actor) => actor,
            Err(_) => {
                kill_and_reap(&mut child);
                return Err(ClientError::Launch);
            }
        };
        Ok(Self {
            cancellation: PadCryptoCancellation {
                child: Arc::new(Mutex::new(child)),
                cancelled: Arc::new(AtomicBool::new(false)),
            },
            sender: Some(sender),
            actor: Some(actor),
            closed: false,
        })
    }

    pub fn cancellation_handle(&self) -> PadCryptoCancellation {
        self.cancellation.clone()
    }

    /// Serialized request/response exchange. A timeout kills and reaps the child.
    pub fn exchange(
        &mut self,
        request: Request,
        timeout: Duration,
    ) -> Result<Response, ClientError> {
        if self.closed || self.cancellation.cancelled.load(Ordering::Acquire) {
            self.cancel();
            return Err(ClientError::Closed);
        }
        if timeout.is_zero() {
            self.cancel();
            return Err(ClientError::Timeout);
        }
        let (reply, received) = mpsc::channel();
        let Some(sender) = &self.sender else {
            return Err(ClientError::Closed);
        };
        if sender.send(Exchange { request, reply }).is_err() {
            self.cancel();
            return Err(ClientError::Transport);
        }
        match received.recv_timeout(timeout) {
            Ok(Ok(response)) => {
                if self.cancellation.cancelled.load(Ordering::Acquire) {
                    self.cancel();
                    Err(ClientError::Closed)
                } else {
                    Ok(response)
                }
            }
            Ok(Err(error)) => {
                let cancelled = self.cancellation.cancelled.load(Ordering::Acquire);
                self.cancel();
                Err(if cancelled {
                    ClientError::Closed
                } else {
                    error
                })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.cancel();
                Err(ClientError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let cancelled = self.cancellation.cancelled.load(Ordering::Acquire);
                self.cancel();
                Err(if cancelled {
                    ClientError::Closed
                } else {
                    ClientError::Transport
                })
            }
        }
    }

    /// Cancel an in-flight exchange from its owning caller and reap this child.
    pub fn cancel(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.cancellation.cancel();
        self.sender.take();
        if let Some(actor) = self.actor.take() {
            let _ = actor.join();
        }
    }
}

impl Drop for PadCryptoClient {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn actor_loop(mut input: ChildStdin, mut output: ChildStdout, receiver: Receiver<Exchange>) {
    while let Ok(exchange) = receiver.recv() {
        let shutdown = exchange.request.operation == Operation::Shutdown;
        let result = write_and_read(&mut input, &mut output, &exchange.request)
            .and_then(|response| correlate(&exchange.request, response));
        let failed = result.is_err();
        let _ = exchange.reply.send(result);
        if failed || shutdown {
            break;
        }
    }
}

fn correlate(request: &Request, response: Response) -> Result<Response, ClientError> {
    if response.id != request.id
        || response.generation != request.generation
        || response.status == Status::InvalidRequest
        || (matches!(request.operation, Operation::Lock | Operation::Shutdown)
            && !response.payload.is_empty())
    {
        Err(ClientError::Protocol)
    } else {
        Ok(response)
    }
}

fn write_and_read(
    input: &mut ChildStdin,
    output: &mut ChildStdout,
    request: &Request,
) -> Result<Response, ClientError> {
    wire::write_request(&mut *input, request).map_err(classify_io)?;
    input.flush().map_err(|_| ClientError::Transport)?;
    wire::read_response(output).map_err(classify_io)
}

fn classify_io(error: io::Error) -> ClientError {
    if error.kind() == io::ErrorKind::InvalidData {
        ClientError::Protocol
    } else {
        ClientError::Transport
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_pad_session_proto::Operation;
    use zeroize::Zeroizing;

    fn request() -> Request {
        Request {
            id: 7,
            generation: 9,
            operation: Operation::Lock,
            vault_id: [0; 16],
            memo_id: 0,
            password: Zeroizing::new(Vec::new()),
            payload: Zeroizing::new(Vec::new()),
        }
    }

    fn child(script: &str) -> Child {
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn reaped(client: &mut PadCryptoClient) -> bool {
        client
            .cancellation
            .child
            .lock()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_some()
    }

    #[test]
    fn mismatched_response_identity_is_rejected() {
        for (id, generation) in [(8, 9), (7, 10)] {
            let response = Response {
                id,
                generation,
                status: Status::Success,
                payload: Zeroizing::new(Vec::new()),
            };
            assert_eq!(
                correlate(&request(), response).err(),
                Some(ClientError::Protocol)
            );
        }
    }

    #[test]
    fn malformed_response_is_terminal_and_child_is_reaped() {
        let mut client = PadCryptoClient::from_child(child(
            "[Console]::OpenStandardOutput().Write([byte[]]@(1,0,0,0),0,4)",
        ))
        .unwrap();
        assert_eq!(
            client.exchange(request(), Duration::from_secs(5)).err(),
            Some(ClientError::Protocol)
        );
        assert!(reaped(&mut client));
        assert_eq!(
            client.exchange(request(), Duration::from_secs(1)).err(),
            Some(ClientError::Closed)
        );
    }

    #[test]
    fn unresponsive_child_times_out_and_is_reaped() {
        let mut client = PadCryptoClient::from_child(child("Start-Sleep -Seconds 30")).unwrap();
        assert_eq!(
            client.exchange(request(), Duration::from_millis(100)).err(),
            Some(ClientError::Timeout)
        );
        assert!(reaped(&mut client));
    }

    #[test]
    fn cancellation_and_drop_reap_an_idle_child() {
        let mut client = PadCryptoClient::from_child(child("Start-Sleep -Seconds 30")).unwrap();
        let cancellation = client.cancellation_handle();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancellation.cancel();
        });
        assert_eq!(
            client.exchange(request(), Duration::from_secs(5)).err(),
            Some(ClientError::Closed)
        );
        canceller.join().unwrap();
        assert!(reaped(&mut client));
        drop(client);
    }
}
