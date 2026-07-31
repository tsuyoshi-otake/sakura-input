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

use sakura_ipc::{Client, Fault};
use sakura_proto::{ErrorCode, KeyInput, Output, Request, Response, SessionId, PROTOCOL_VERSION};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

/// DESIGN 4.3's per-keystroke budget. Exceeding it is a dropped keystroke;
/// not having it at all is a frozen application.
const KEY_BUDGET: Duration = Duration::from_millis(50);

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
pub enum Answer {
    /// The engine answered inside the budget. Whether it wants the key is
    /// [`Output::consumed`].
    Ready(Output),
    /// No engine, no answer in time, or an answer that made no sense. The
    /// key belongs to the application, and any composition already on
    /// screen has to be finalized rather than left hanging — see the
    /// crash-resilience criterion in PLAN.md Phase 1.
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
    fn attached_to(name: &str) -> Self {
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

    fn request(&mut self, request: &Request) -> Answer {
        let Some(link) = self.link.as_mut() else {
            return Answer::Unavailable;
        };

        match link.client.call(request, KEY_BUDGET) {
            Ok(Response::Output(output)) => Answer::Ready(output),

            // The engine forgot this session: it restarted behind a
            // connection that outlived it, or its table was reset. A new
            // link is the only way forward, but not on this keystroke —
            // the user gets the key back and the next one reconnects.
            Ok(Response::Error(ErrorCode::UnknownSession)) => {
                self.drop_link();
                Answer::Unavailable
            }

            // `Ok` and `Error` are legitimate answers to `Commit` and to
            // requests this milestone does not make; neither carries text,
            // so there is nothing to show and nothing to correct.
            Ok(_) => Answer::Unavailable,

            // Kept, not dropped: see the module docs. The flag is what
            // stops the next successful call from building on a
            // composition the engine may have moved on from.
            Err(Fault::Timeout) => {
                link.desynchronized = true;
                Answer::Unavailable
            }

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
    let mut client = match name {
        Some(name) => Client::connect_to(name, left(deadline)),
        None => Client::connect(left(deadline)),
    }
    .ok()?;

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
        _ => return None,
    }

    let created = client.call(
        &Request::CreateSession {
            process_name: host_process_name(),
        },
        left(deadline),
    );
    match created {
        Ok(Response::SessionCreated { session }) => Some(Link {
            client,
            session,
            desynchronized: false,
        }),
        _ => None,
    }
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
    use sakura_proto::{decode_request, encode_response, KeyCode, Modifiers, Preedit, Segment};

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
                &Response::SessionCreated { session: 1 },
            );
            then(&server, &mut buffer);
        });
        (name, handle)
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
        // Generous, because this asserts the deadline was honoured at all,
        // not scheduler precision on a loaded machine.
        assert!(
            waited < Duration::from_millis(300),
            "a keystroke waited {waited:?}, well past its budget"
        );

        drop(engine);
        server.join().expect("the server thread");
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
