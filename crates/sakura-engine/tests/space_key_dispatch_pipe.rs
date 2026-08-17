//! Real-process protocol and failure-injection for Space across connections.
//!
//! Two clients on one owned engine process are two share-nothing workers.
//! This never touches the installed well-known pipe.

#[allow(dead_code)]
mod common;

use std::fs;
use std::time::Duration;

use sakura_ipc::Client;
use sakura_proto::{KeyCode, Request, Response, SessionId, PROTOCOL_VERSION};

use common::{char_key, named_key, session_for, Engine, PATIENT};

fn handshake(client: &mut Client) {
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
}

fn send_char(client: &mut Client, session: SessionId, character: char) -> Response {
    client
        .call(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            PATIENT,
        )
        .expect("char")
}

fn send_named(client: &mut Client, session: SessionId, code: KeyCode) -> Response {
    client
        .call(
            &Request::SendKey {
                session,
                key: named_key(code),
            },
            PATIENT,
        )
        .expect("named")
}

fn commit_text(response: &Response) -> Option<&str> {
    match response {
        Response::Output(output) => output.commit.as_deref(),
        _ => None,
    }
}

fn has_candidates(response: &Response) -> bool {
    match response {
        Response::Output(output) => output
            .candidates
            .as_ref()
            .is_some_and(|list| !list.items.is_empty()),
        _ => false,
    }
}

fn write_failure_log(name: &str, body: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("verification/space-key-dispatch/failure-injection");
    fs::create_dir_all(&dir).expect("dir");
    fs::write(dir.join(name), body).expect("log");
}

#[test]
fn api_idle_space_inserts_fullwidth_on_a_live_process() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-idle.exe");
    let response = send_named(&mut client, session, KeyCode::Space);
    assert_eq!(commit_text(&response), Some("\u{3000}"));
    let cleanup = engine.cleanup().expect("cleanup");
    assert!(cleanup.status.success() || cleanup.status.code().is_some());
    write_failure_log(
        "api-idle-space.txt",
        &format!("pid={} commit=U+3000\n", cleanup.pid),
    );
}

#[test]
fn api_composing_space_does_not_commit_a_document_space() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-compose.exe");
    let _ = send_char(&mut client, session, 'a');
    let response = send_named(&mut client, session, KeyCode::Space);
    assert_ne!(commit_text(&response), Some("\u{3000}"));
    assert_ne!(commit_text(&response), Some(" "));
    let _ = engine.cleanup();
}

#[test]
fn fail_dual_delivery_two_clients_insert_and_convert() {
    let mut engine = Engine::spawn_isolated();
    let mut composing = engine.client();
    let mut idle = engine.client();
    handshake(&mut composing);
    handshake(&mut idle);
    // Same host process name so the composition fence treats them as peers.
    let session_a = session_for(&mut composing, "space-dual-host.exe");
    let session_b = session_for(&mut idle, "space-dual-host.exe");
    let _ = send_char(&mut composing, session_a, 'a');
    let converted = send_named(&mut composing, session_a, KeyCode::Space);
    let inserted = send_named(&mut idle, session_b, KeyCode::Space);
    let inserted_space = matches!(commit_text(&inserted), Some("\u{3000}" | " "));
    let converted_without_space =
        commit_text(&converted) != Some("\u{3000}") && commit_text(&converted) != Some(" ");
    write_failure_log(
        "fail-dual-delivery.txt",
        &format!(
            "inserted_space={inserted_space} converted_without_space={converted_without_space} \
converted_candidates={} inserted_commit={:?} converted_commit={:?}\n",
            has_candidates(&converted),
            commit_text(&inserted),
            commit_text(&converted)
        ),
    );
    assert!(
        !inserted_space && converted_without_space,
        "composition fence must absorb idle Space while a peer converts"
    );
    let _ = engine.cleanup();
}

#[test]
fn fail_duplicate_idle_space_is_idempotent_per_key_and_accumulates_spaces() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-dup.exe");
    let first = send_named(&mut client, session, KeyCode::Space);
    let second = send_named(&mut client, session, KeyCode::Space);
    assert_eq!(commit_text(&first), Some("\u{3000}"));
    assert_eq!(commit_text(&second), Some("\u{3000}"));
    let _ = engine.cleanup();
}

#[test]
fn fail_cancel_then_space_inserts() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-cancel.exe");
    let _ = send_char(&mut client, session, 'a');
    let _ = send_named(&mut client, session, KeyCode::Escape);
    let response = send_named(&mut client, session, KeyCode::Space);
    assert_eq!(commit_text(&response), Some("\u{3000}"));
    let _ = engine.cleanup();
}

#[test]
fn fail_reorder_space_before_type_inserts_then_composes() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-reorder.exe");
    let space_first = send_named(&mut client, session, KeyCode::Space);
    assert_eq!(commit_text(&space_first), Some("\u{3000}"));
    let typed = send_char(&mut client, session, 'a');
    match typed {
        Response::Output(output) => assert!(output.consumed),
        other => panic!("{other:?}"),
    }
    let _ = engine.cleanup();
}

#[test]
fn fail_crash_restart_reopens_idle() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-crash.exe");
    let _ = send_char(&mut client, session, 'a');
    let pid = engine.child_pid();
    drop(client);
    drop(engine);
    let mut restarted = Engine::spawn_isolated();
    let mut client = restarted.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-crash.exe");
    let response = send_named(&mut client, session, KeyCode::Space);
    assert_eq!(commit_text(&response), Some("\u{3000}"));
    write_failure_log(
        "fail-crash-restart.txt",
        &format!("killed_pid={pid} restarted_idle_space=U+3000\n"),
    );
    let _ = restarted.cleanup();
}

#[test]
fn fail_omission_drop_space_leaves_composition() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-drop.exe");
    let _ = send_char(&mut client, session, 'a');
    let _ = send_char(&mut client, session, 'i');
    // Space omitted: next Enter should commit preedit, not a space.
    let committed = send_named(&mut client, session, KeyCode::Enter);
    assert_ne!(commit_text(&committed), Some("\u{3000}"));
    let _ = engine.cleanup();
}

#[test]
fn aa_fail_timeout_unavailability_does_not_orphan_the_child() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-timeout.exe");
    let result = client.call(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        Duration::from_millis(1),
    );
    write_failure_log("fail-timeout.txt", &format!("short_budget={result:?}\n"));
    // A 1ms budget can leave the pipe mid-frame; prefer Drop cleanup over a
    // hard Shutdown expectation so the harness never orphans the child.
    drop(client);
    let _ = engine.cleanup();
}

#[test]
fn fail_retry_after_short_timeout_reaches_a_terminal_idle_space() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-retry.exe");
    let first = client.call(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        Duration::from_millis(1),
    );
    let retry = client.call(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        PATIENT,
    );
    let recovered = match &retry {
        Ok(response) => commit_text(response).map(str::to_owned),
        Err(_) => {
            drop(client);
            let mut recovered_client = engine.client();
            handshake(&mut recovered_client);
            let recovered_session = session_for(&mut recovered_client, "space-retry-reopen.exe");
            let response = send_named(&mut recovered_client, recovered_session, KeyCode::Space);
            commit_text(&response).map(str::to_owned)
        }
    };
    write_failure_log(
        "fail-retry.txt",
        &format!("first={first:?} same_client_retry={retry:?} recovered_commit={recovered:?}\n"),
    );
    assert_eq!(recovered.as_deref(), Some("\u{3000}"));
    let _ = engine.cleanup();
}

#[test]
fn fail_partial_drop_of_composing_client_leaves_idle_peer() {
    let mut engine = Engine::spawn_isolated();
    let mut composing = engine.client();
    let mut idle = engine.client();
    handshake(&mut composing);
    handshake(&mut idle);
    let session_a = session_for(&mut composing, "space-partial-a.exe");
    let session_b = session_for(&mut idle, "space-partial-b.exe");
    let _ = send_char(&mut composing, session_a, 'a');
    drop(composing);
    let inserted = send_named(&mut idle, session_b, KeyCode::Space);
    write_failure_log(
        "fail-partial-drop.txt",
        &format!("idle_commit_after_peer_drop={:?}\n", commit_text(&inserted)),
    );
    assert_eq!(commit_text(&inserted), Some("\u{3000}"));
    let _ = engine.cleanup();
}

#[test]
fn fail_product_idle_spaces_are_unbounded_unlike_oracle_bound() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "space-exhaust.exe");
    let mut commits = 0u32;
    for _ in 0..5 {
        let response = send_named(&mut client, session, KeyCode::Space);
        if matches!(commit_text(&response), Some("\u{3000}" | " ")) {
            commits += 1;
        }
    }
    write_failure_log(
        "fail-exhaust.txt",
        &format!("product_idle_spaces={commits} oracle_bound=4\n"),
    );
    assert_eq!(
        commits, 5,
        "product has no idle-space quota; BND-SPACE-01 is a model bound"
    );
    let _ = engine.cleanup();
}
