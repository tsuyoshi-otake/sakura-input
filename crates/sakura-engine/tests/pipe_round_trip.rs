//! The engine answers a real client over a private test-owned pipe.
//!
//! Every other test in this workspace stops one layer short of this. The
//! dispatcher's unit tests call `dispatch` directly; `sakura-ipc`'s tests put
//! a server and a client on a scratch pipe name; the text service's tests
//! talk to a scripted fake engine. This test exercises the built
//! `sakura_engine.exe` in a separate process, but gives that process a unique
//! explicit test pipe and owned profile. It never resolves or touches the
//! installed engine's well-known pipe, so real-process test binaries may run
//! concurrently.
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

#[allow(dead_code)]
mod common;

use sakura_ipc::Client;
use sakura_proto::{KeyCode, Request, Response, SessionId, PROTOCOL_VERSION};

use common::{char_key, named_key, session_for, shifted_char_key, visible, Engine, PATIENT};

fn assert_shifted_term(client: &mut Client, session: SessionId, typed: &str, expected: &[&str]) {
    let mut shifted_preedit = String::new();
    for character in typed.chars() {
        match client.call(
            &Request::SendKey {
                session,
                key: shifted_char_key(character),
            },
            PATIENT,
        ) {
            Ok(Response::Output(output)) => {
                assert!(
                    output.consumed,
                    "Shift+{character} in {typed} must stay in English composition"
                );
                shifted_preedit = visible(output.preedit);
            }
            other => panic!("Shift+{character} in {typed}: expected Output, got {other:?}"),
        }
    }
    assert_eq!(shifted_preedit, typed);

    let converted = match client.call(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert!(output.consumed);
            let converted = visible(output.preedit);
            assert!(
                expected.contains(&converted.as_str()),
                "Space after Shift+{typed}: expected one of {expected:?}, got {converted:?}"
            );
            converted
        }
        other => panic!("Space after Shift+{typed}: expected Output, got {other:?}"),
    };
    match client.call(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert_eq!(output.commit.as_deref(), Some(converted.as_str()))
        }
        other => panic!("Enter after Shift+{typed}: expected Output, got {other:?}"),
    }
}

/// The whole M0 story across a real pipe: the engine starts, accepts the
/// handshake, opens a session, verifies stateful composition and UI behavior,
/// then verifies isolated durable English learning.
///
/// The harness owns a unique pipe and process, so concurrent integration test
/// binaries cannot interfere with this test or a user's installed engine.
#[test]
fn a_real_engine_serves_a_real_client_over_an_owned_private_pipe() {
    let mut engine = Engine::spawn_isolated();
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

    // 無変換 while composing is deliberately a temporary kana transform and
    // must not publish a persistent mode change. Commit the preedit first so
    // the mode-key assertion below exercises the idle `mode_kana_cycle` path
    // that the renderer's shared UI state is meant to observe.
    match next.call(
        &Request::SendKey {
            session: next_session,
            key: named_key(KeyCode::Enter),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert_eq!(output.commit.as_deref(), Some("あ"));
        }
        other => panic!("Enter on the second connection: expected Output, got {other:?}"),
    }

    // Real-pipe tests intentionally run against the durable learning store.
    // A user who previously selected Claude Code should keep that Microsoft
    // IME-style preference, while a fresh profile still ranks Claude first.
    assert_shifted_term(
        &mut next,
        next_session,
        "CLAUDE",
        &["Claude", "Claude Code"],
    );
    assert_shifted_term(&mut next, next_session, "OPENAI", &["OpenAI"]);
    assert_shifted_term(&mut next, next_session, "GITLAB", &["GitLab"]);
    assert_shifted_term(&mut next, next_session, "PYTORCH", &["PyTorch"]);

    // These and the stateful composition/UI keys above deliberately remain
    // non-test-only: this test verifies retained engine state. Every write is
    // confined to this test's owned profile, never the user's LOCALAPPDATA.
    let learning_path = engine
        .local_app_data()
        .join("SakuraInput")
        .join("learning")
        .join("log.bin");
    let learning = sakura_engine::learning::read_snapshot(&learning_path).unwrap_or_else(|error| {
        panic!(
            "read isolated learning log {}: {error}",
            learning_path.display()
        )
    });
    assert!(
        learning
            .records
            .iter()
            .any(|record| record.surface == "Claude" || record.surface == "Claude Code"),
        "non-test-only English commit did not persist in isolated learning log {}: {learning:?}",
        learning_path.display()
    );

    // What the renderer does. `since: 0` is nobody's revision, so this is
    // answered from the engine's current state without blocking.
    let mut renderer = engine.client();
    let seen = match renderer.call(&Request::WatchUi { since: 0 }, PATIENT) {
        Ok(Response::Ui(state)) => state,
        other => panic!("WatchUi: expected Ui, got {other:?}"),
    };

    // A mode key on the typing connection has to reach the watcher on the
    // renderer's connection. Cycle until the session reports a mode different
    // from the observed board; within three states that must publish a change.
    let mut changed_mode = None;
    for _ in 0..3 {
        match next.call(
            &Request::SendKey {
                session: next_session,
                key: named_key(KeyCode::Muhenkan),
            },
            PATIENT,
        ) {
            Ok(Response::Output(output)) => {
                let mode = output.mode.expect("idle Muhenkan must report the new mode");
                if Some(mode) != seen.mode {
                    changed_mode = Some(mode);
                    break;
                }
            }
            other => panic!("Muhenkan: expected Output, got {other:?}"),
        }
    }
    assert!(
        changed_mode.is_some(),
        "the three-state mode cycle must differ from the observed UI mode"
    );

    match renderer.call(
        &Request::WatchUi {
            since: seen.revision,
        },
        PATIENT,
    ) {
        Ok(Response::Ui(state)) => {
            assert_ne!(
                state.revision, seen.revision,
                "the mode change did not reach the UI"
            );
            assert!(state.mode.is_some(), "a mode change must name a mode");
        }
        other => panic!("WatchUi after a mode change: expected Ui, got {other:?}"),
    }

    drop(renderer);
    drop(next);
    let cleanup = engine.cleanup().expect("owned engine cleanup must succeed");
    assert!(
        cleanup.status.success(),
        "owned engine pid {} exited with {}",
        cleanup.pid,
        cleanup.status
    );
}
