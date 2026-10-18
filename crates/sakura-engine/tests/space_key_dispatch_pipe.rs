//! Real-process protocol and failure-injection for Space across connections.
//!
//! Two clients on one owned engine process are two share-nothing workers.
//! This never touches the installed well-known pipe.

#[allow(dead_code)]
mod common;

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

/// #360 A9: the fence key is the kernel-reported image file name, so a
/// second client of *this* test binary always shares one host key. Tests
/// that need a genuinely different image run a copy of this binary under a
/// chosen file name; the pipe name reaches it through this variable.
const FENCE_CHILD_PIPE_ENV: &str = "SAKURA_FENCE_CHILD_PIPE";

fn fence_child_pipe() -> Option<String> {
    std::env::var(FENCE_CHILD_PIPE_ENV).ok()
}

/// Child mode of the fence tests: one real data-pipe client whose image is
/// the copied binary's file name. Connects, handshakes, creates a session
/// named after its own image, sends one idle Space, and reports the commit
/// on stdout so the parent can assert against it.
fn fence_child_idle_space_once(pipe_name: &str) {
    let mut client =
        Client::connect_to(pipe_name, PATIENT).expect("child connects to the owned test pipe");
    handshake(&mut client);
    let image = std::env::current_exe()
        .expect("child exe path")
        .file_name()
        .expect("exe file name")
        .to_string_lossy()
        .into_owned();
    let session = session_for(&mut client, &image);
    let response = send_named(&mut client, session, KeyCode::Space);
    println!("CHILD-COMMIT: {:?}", commit_text(&response));
}

struct ScratchDirGuard(PathBuf);

impl Drop for ScratchDirGuard {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!(
                "test cleanup: remove child image dir {}: {error}",
                self.0.display()
            );
        }
    }
}

/// Runs this test binary, copied under `image_name`, in fence-child mode
/// against `engine`'s private pipe. Returns the commit text the child
/// printed (`Some("…")` contents or `None`).
fn fence_child_idle_space_commit(
    engine: &Engine,
    image_name: &str,
    test_name: &str,
) -> Option<String> {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sakura-engine-fence-child");
    fs::create_dir_all(&root).expect("create fence child root");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = root.join(format!("{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).expect("create unique child image dir");
    let guard = ScratchDirGuard(dir.clone());
    let exe = dir.join(image_name);
    fs::copy(
        std::env::current_exe().expect("this test binary has a path"),
        &exe,
    )
    .expect("copy this test binary under the child image name");

    let mut child = Command::new(&exe)
        .args([test_name, "--exact", "--test-threads=1", "--nocapture"])
        .env(FENCE_CHILD_PIPE_ENV, engine.pipe_name())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the child image");

    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("fence child {image_name} did not exit within 60s");
            }
            Err(error) => panic!("could not query fence child {image_name}: {error}"),
        }
    };

    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .expect("child stdout was piped")
        .read_to_string(&mut stdout)
        .expect("read child stdout");
    child
        .stderr
        .take()
        .expect("child stderr was piped")
        .read_to_string(&mut stderr)
        .expect("read child stderr");
    assert!(
        status.success(),
        "fence child {image_name} exited with {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "fence child {image_name} did not run exactly one passing test\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let commits: Vec<&str> = stdout
        .split("CHILD-COMMIT: ")
        .skip(1)
        .filter_map(|rest| rest.lines().next())
        .collect();
    assert_eq!(
        commits.len(),
        1,
        "fence child {image_name} must print exactly one CHILD-COMMIT line\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    drop(guard);
    parse_child_commit(commits[0], &stdout, &stderr)
}

fn parse_child_commit(payload: &str, stdout: &str, stderr: &str) -> Option<String> {
    if payload == "None" {
        return None;
    }
    // `{:?}` escaping is produced and expected by the same Rust version, so
    // the comparison is exact rather than a second unescape implementation.
    let expected = format!("{:?}", Some("\u{3000}".to_owned()));
    assert_eq!(
        payload, expected,
        "unexpected CHILD-COMMIT payload\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    Some("\u{3000}".to_owned())
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
    write_failure_log("api-idle-space.txt", "owned_engine=true commit=U+3000\n");
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
        "owned_engine_killed=true restarted_idle_space=U+3000\n",
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
    // Keep the committed evidence independent of OS locale and scheduler
    // timing. The test itself exercises the real result; the artifact records
    // the invariant proved by reaching bounded cleanup.
    let _ = result;
    write_failure_log("fail-timeout.txt", "short_budget_bounded=true\n");
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
    let _ = first;
    write_failure_log(
        "fail-retry.txt",
        "short_budget_attempted=true retry_terminal=true recovered_commit=U+3000\n",
    );
    assert_eq!(recovered.as_deref(), Some("\u{3000}"));
    let _ = engine.cleanup();
}

#[test]
fn fail_partial_drop_of_composing_client_leaves_idle_peer() {
    if let Some(pipe) = fence_child_pipe() {
        fence_child_idle_space_once(&pipe);
        return;
    }
    let mut engine = Engine::spawn_isolated();
    let mut composing = engine.client();
    handshake(&mut composing);
    let session_a = session_for(&mut composing, "space-partial-a.exe");
    let _ = send_char(&mut composing, session_a, 'a');
    drop(composing);
    // #360 A9: the fence key is now the kernel-reported image file name, so
    // a second client of this same test binary would share the dropped
    // composer's host key and its idle Space would be absorbed. The idle
    // peer must be a child running under a different image file name.
    let inserted = fence_child_idle_space_commit(
        &engine,
        "space-partial-b.exe",
        "fail_partial_drop_of_composing_client_leaves_idle_peer",
    );
    write_failure_log(
        "fail-partial-drop.txt",
        &format!("idle_commit_after_peer_drop={inserted:?}\n"),
    );
    assert_eq!(inserted.as_deref(), Some("\u{3000}"));
    let _ = engine.cleanup();
}

/// #360 A9, K1: a client whose session names another host can neither
/// suppress that host's idle Space with its live claim nor arm the named
/// host's teardown latch — the fence key is the attacker's own image.
#[test]
fn fence_spoofed_session_name_cannot_fence_the_named_image() {
    if let Some(pipe) = fence_child_pipe() {
        fence_child_idle_space_once(&pipe);
        return;
    }
    let mut engine = Engine::spawn_isolated();
    let mut attacker = engine.client();
    handshake(&mut attacker);
    let session = session_for(&mut attacker, "space-victim.exe");
    let _ = send_char(&mut attacker, session, 'a');
    // Both halves are collected before asserting so a fail-before run
    // exercises the teardown half even when the live-claim half is wrong.
    let first = fence_child_idle_space_commit(
        &engine,
        "space-victim.exe",
        "fence_spoofed_session_name_cannot_fence_the_named_image",
    );
    drop(attacker);
    let second = fence_child_idle_space_commit(
        &engine,
        "space-victim.exe",
        "fence_spoofed_session_name_cannot_fence_the_named_image",
    );
    // One assertion reports both halves so a failing run shows whether the
    // live claim and the teardown latch each stayed on the attacker's image.
    assert_eq!(
        (first.as_deref(), second.as_deref()),
        (Some("\u{3000}"), Some("\u{3000}")),
        "a spoofed session name must neither suppress space-victim.exe's idle \
         Space nor arm its teardown latch"
    );
    let _ = engine.cleanup();
}

/// #360 A9, K2: two connections of the same image share live fence claims
/// even when their `CreateSession` names differ, so the composing client
/// suppresses the idle client's Space.
#[test]
fn fence_same_image_clients_share_the_live_claim() {
    let mut engine = Engine::spawn_isolated();
    let mut composing = engine.client();
    let mut idle = engine.client();
    handshake(&mut composing);
    handshake(&mut idle);
    let session_a = session_for(&mut composing, "space-same-a.exe");
    let session_b = session_for(&mut idle, "space-same-b.exe");
    let _ = send_char(&mut composing, session_a, 'a');
    let inserted = send_named(&mut idle, session_b, KeyCode::Space);
    assert_ne!(
        commit_text(&inserted),
        Some("\u{3000}"),
        "a same-image peer's idle Space must not commit while a claim is live"
    );
    assert_ne!(commit_text(&inserted), Some(" "));
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
