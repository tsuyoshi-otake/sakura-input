//! The engine answers a real client over the real pipe.
//!
//! Every other test in this workspace stops one layer short of this. The
//! dispatcher's unit tests call `dispatch` directly; `sakura-ipc`'s tests put
//! a server and a client on a scratch pipe name; the text service's tests
//! talk to a scripted fake engine. What none of them touch is the arrangement
//! that actually ships: the built `sakura_engine.exe`, serving the well-known
//! name that [`Client::connect`] resolves on its own, spoken to by a separate
//! process.
//!
//! That gap is where the startup crash of the boxed-session-table fix lived —
//! the engine failed before it accepted its first connection, and a fully
//! green `cargo test` said nothing about it, because nothing in the suite had
//! ever started the binary.
//!
//! The sequence below is deliberately the DLL's, in the DLL's order (see
//! `sakura-tsf`'s `engine::open`): connect, `Hello`, `CreateSession`, then
//! keystrokes. If this passes and the text service still cannot type, the
//! fault is in the text service, not in the protocol or the transport.

use std::process::{Child, Command};
use std::thread::sleep;
use std::time::{Duration, Instant};

use sakura_ipc::{pipe_name, Client};
use sakura_proto::{KeyCode, KeyInput, Modifiers, Request, Response, PROTOCOL_VERSION};

/// Long enough to cover a cold process start on a loaded machine. Nothing
/// here is measuring latency — the 50 ms keystroke budget is the DLL's
/// concern and is tested there.
const PATIENT: Duration = Duration::from_secs(5);

/// A running engine to talk to, and whether this test is the one that
/// started it.
///
/// The pipe name belongs to the logon session, so at most one engine serves
/// it at a time (DESIGN 4.1). On a developer's machine that engine may
/// already be running and may be mid-conversation with a real text service in
/// a real editor; killing it to run a test would take the user's IME away
/// under them. So: use whoever is already there, and only clean up a process
/// this test is responsible for.
struct Engine {
    spawned: Option<Child>,
}

impl Engine {
    fn running() -> Engine {
        if Client::connect(Duration::from_millis(200)).is_ok() {
            return Engine { spawned: None };
        }

        let child = Command::new(env!("CARGO_BIN_EXE_sakura_engine"))
            .spawn()
            .expect("the engine binary is built as a dependency of this test");
        Engine {
            spawned: Some(child),
        }
    }

    /// Connects once the engine is serving, or fails saying what it saw.
    ///
    /// Polls rather than sleeping a fixed amount: the pipe appears when the
    /// engine's first worker calls `CreateNamedPipeW`, which is early, and a
    /// fixed sleep would be both slower in the common case and flaky in the
    /// rare one.
    fn client(&mut self) -> Client {
        let deadline = Instant::now() + PATIENT;
        loop {
            match Client::connect(Duration::from_millis(100)) {
                Ok(client) => return client,
                Err(fault) if Instant::now() >= deadline => {
                    let name = pipe_name().unwrap_or_else(|_| "<unresolvable>".to_owned());
                    panic!("no engine on {name} after {PATIENT:?}: {fault:?}");
                }
                Err(_) => sleep(Duration::from_millis(20)),
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Only what this test started. `kill` rather than `Request::Shutdown`
        // because this runs on the failure path too, where the engine may be
        // exactly the thing that has stopped answering — and a test that
        // leaves a process behind is a test that poisons every run after it.
        if let Some(child) = self.spawned.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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

fn named_key(code: KeyCode) -> KeyInput {
    KeyInput {
        code,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

/// The whole M0 story across a real pipe: the engine starts, accepts the
/// handshake, opens a session, turns romaji into kana, commits it, and is
/// still serving the connection after that.
///
/// One test function rather than several, because the engine is a
/// per-logon-session singleton and [`Engine`] is a guard that stops it: two
/// tests running in parallel would each hold their own guard, and whichever
/// finished first would kill the engine out from under the other. One guard,
/// one lifetime, no race.
#[test]
fn a_real_engine_serves_a_real_client_over_the_well_known_pipe() {
    let mut engine = Engine::running();
    let mut client = engine.client();

    match client.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        PATIENT,
    ) {
        Ok(Response::Hello { server_version, .. }) => {
            assert_eq!(server_version, PROTOCOL_VERSION);
        }
        other => panic!("handshake: expected Hello, got {other:?}"),
    }

    let session = session_for(&mut client, "pipe_round_trip.exe");

    let mut preedit = String::new();
    for c in "sakura".chars() {
        match client.call(
            &Request::SendKey {
                session,
                key: char_key(c),
            },
            PATIENT,
        ) {
            Ok(Response::Output(output)) => {
                assert!(output.consumed, "the engine must claim a composing key");
                preedit = visible(output.preedit);
            }
            other => panic!("SendKey {c:?}: expected Output, got {other:?}"),
        }
    }
    assert_eq!(preedit, "さくら", "romaji did not survive the round trip");

    match client.call(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert_eq!(output.commit.as_deref(), Some("さくら"));
            assert!(
                output.preedit.is_none_or(|p| p.segments.is_empty()),
                "committing must leave nothing composing"
            );
        }
        other => panic!("Enter: expected Output, got {other:?}"),
    }

    // A second connection, which is what focusing a second application
    // does. It has to be served at all — the pool must grow past the
    // instance the engine started with — and the first client hanging up
    // must not have taken the engine with it.
    drop(client);
    let mut next = engine.client();
    match next.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        PATIENT,
    ) {
        Ok(Response::Hello { .. }) => {}
        other => panic!("second connection: expected Hello, got {other:?}"),
    }
    let next_session = session_for(&mut next, "second.exe");

    // Deliberately not asserting this id differs from the first. Sessions
    // are numbered by the `Dispatcher` that owns them, and per the
    // share-nothing design in `server`'s module docs each connection gets
    // its own — so id 1 on two connections is two unrelated sessions, and
    // that is fine precisely because a `SessionId` is only ever interpreted
    // against the connection it arrived on.
    match next.call(
        &Request::SendKey {
            session: next_session,
            key: char_key('a'),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert_eq!(visible(output.preedit), "あ");
        }
        other => panic!("expected Output on the second connection, got {other:?}"),
    }

    // What the renderer does. `since: 0` is nobody's revision, so this is
    // answered from the engine's current state without blocking.
    let mut renderer = engine.client();
    let seen = match renderer.call(&Request::WatchUi { since: 0 }, PATIENT) {
        Ok(Response::Ui(state)) => state.revision,
        other => panic!("WatchUi: expected Ui, got {other:?}"),
    };

    // A mode key on the typing connection has to reach the watcher on the
    // renderer's connection — which is the entire reason the UI board is
    // the one piece of engine state that is not per-connection.
    match next.call(
        &Request::SendKey {
            session: next_session,
            key: named_key(KeyCode::Muhenkan),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert!(
                output.mode.is_some(),
                "無変換 is bound to mode_kana_toggle and must report the new mode"
            );
        }
        other => panic!("Muhenkan: expected Output, got {other:?}"),
    }

    match renderer.call(&Request::WatchUi { since: seen }, PATIENT) {
        Ok(Response::Ui(state)) => {
            assert_ne!(state.revision, seen, "the mode change did not reach the UI");
            assert!(state.mode.is_some(), "a mode change must name a mode");
        }
        other => panic!("WatchUi after a mode change: expected Ui, got {other:?}"),
    }
}

/// What a text service would draw: every segment's text, in order.
fn visible(preedit: Option<sakura_proto::Preedit>) -> String {
    preedit
        .map(|p| p.segments.into_iter().map(|s| s.text).collect())
        .unwrap_or_default()
}

fn session_for(client: &mut Client, process_name: &str) -> sakura_proto::SessionId {
    match client.call(
        &Request::CreateSession {
            process_name: process_name.to_owned(),
        },
        PATIENT,
    ) {
        Ok(Response::SessionCreated { session }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    }
}
