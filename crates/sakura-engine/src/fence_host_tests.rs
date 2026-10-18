//! #360 A9 contracts for the composition-fence host key: the fence key of an
//! accepted connection comes from the kernel-reported client PID's image
//! file name, never from the `CreateSession` process name a client sends.

use super::*;
use crate::composition_fence::CompositionFence;
use crate::server::connection_host_key;
use sakura_ipc::security::client_image_file_name;
use sakura_proto::{KeyCode, KeyInput, Modifiers};
use std::sync::Arc;

fn fence_dispatcher(fence: &Arc<CompositionFence>, host: &str) -> Dispatcher {
    let mut dispatcher = Dispatcher::new().expect("built-in dispatcher");
    dispatcher.set_composition_fence(Arc::clone(fence));
    dispatcher.set_fence_host(Box::from(host));
    dispatcher
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

fn create_session(dispatcher: &mut Dispatcher, out: &mut OutputBuf, name: &str) -> SessionId {
    match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: name.to_string(),
        },
        out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    }
}

fn send_key(dispatcher: &mut Dispatcher, session: SessionId, key: KeyInput, out: &mut OutputBuf) {
    dispatcher.dispatch(&Request::SendKey { session, key }, out);
}

/// K2: connections whose client images share a file name share live fence
/// claims, so a composing claim on one suppresses the other's idle Space —
/// even when their `CreateSession` names differ.
#[test]
fn fence_same_image_hosts_share_the_live_claim() {
    let fence = Arc::new(CompositionFence::new());
    let mut composing = fence_dispatcher(&fence, "host-a.exe");
    let mut idle = fence_dispatcher(&fence, "host-a.exe");
    let mut out = OutputBuf::new();
    let a = create_session(&mut composing, &mut out, "x.exe");
    let b = create_session(&mut idle, &mut out, "y.exe");
    send_key(&mut composing, a, char_key('a'), &mut out);
    send_key(&mut idle, b, named_key(KeyCode::Space), &mut out);
    assert_ne!(
        out.commit_text(),
        Some("\u{3000}"),
        "a same-image peer's idle Space must not commit while a claim is live"
    );
    assert_ne!(out.commit_text(), Some(" "));
}

/// K1: a client that names another host in `CreateSession` can neither
/// suppress that host's idle Space with a live claim nor arm a teardown
/// latch for it — both are keyed by the attacker's own image.
#[test]
fn fence_spoofed_session_name_does_not_fence_the_named_host() {
    let fence = Arc::new(CompositionFence::new());
    let mut attacker = fence_dispatcher(&fence, "attacker.exe");
    let mut victim = fence_dispatcher(&fence, "victim.exe");
    let mut out = OutputBuf::new();
    let a = create_session(&mut attacker, &mut out, "victim.exe");
    let v = create_session(&mut victim, &mut out, "victim.exe");
    send_key(&mut attacker, a, char_key('a'), &mut out);
    send_key(&mut victim, v, named_key(KeyCode::Space), &mut out);
    assert_eq!(
        out.commit_text(),
        Some("\u{3000}"),
        "a spoofed session name must not suppress the named host's idle Space"
    );
    // Dropping the composing connection arms the teardown latch under the
    // attacker's image only; the victim's next idle Space still inserts.
    attacker.reset();
    send_key(&mut victim, v, named_key(KeyCode::Space), &mut out);
    assert_eq!(
        out.commit_text(),
        Some("\u{3000}"),
        "a spoofed session name must not arm the named host's teardown latch"
    );
}

/// K5: `reset()` releases every claim and clears the host key, so a new
/// session on the same dispatcher falls back to its own name and an idle
/// Space is neither suppressed nor absorbed by the pre-reset host key.
#[test]
fn fence_host_key_is_cleared_by_reset() {
    let fence = Arc::new(CompositionFence::new());
    let mut dispatcher = fence_dispatcher(&fence, "host-a.exe");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "x.exe");
    send_key(&mut dispatcher, session, char_key('a'), &mut out);
    // Releasing the live claim arms the teardown latch for host-a.exe.
    dispatcher.reset();
    let session = create_session(&mut dispatcher, &mut out, "y.exe");
    send_key(
        &mut dispatcher,
        session,
        named_key(KeyCode::Space),
        &mut out,
    );
    assert_eq!(
        out.commit_text(),
        Some("\u{3000}"),
        "a retained host key would spend the armed teardown latch on this Space"
    );
}

/// K3: the image lookup reads the kernel-reported image for a PID, fails
/// for an unopenable PID, and `connection_host_key` degrades to `pid:<n>` —
/// a shape a real file name can never collide with.
#[test]
fn fence_host_key_comes_from_the_kernel_image_name() {
    let own_exe = std::env::current_exe().expect("the test process has a path");
    let expected = own_exe
        .file_name()
        .expect("an exe path has a file name")
        .to_string_lossy();
    let actual = client_image_file_name(std::process::id()).expect("our own image");
    assert_eq!(
        actual.to_ascii_lowercase(),
        expected.to_ascii_lowercase(),
        "the helper must report the same file name as current_exe"
    );
    assert!(client_image_file_name(0).is_err(), "PID 0 cannot be opened");

    let image = client_image_file_name(std::process::id());
    assert_eq!(
        connection_host_key(1, image).as_ref(),
        expected.as_ref(),
        "a successful lookup is keyed by the image file name"
    );
    // PID 0 always fails to open, so it is a real error the caller sees.
    let seven = connection_host_key(7, client_image_file_name(0));
    let eight = connection_host_key(8, client_image_file_name(0));
    assert_eq!(seven.as_ref(), "pid:7");
    assert_eq!(eight.as_ref(), "pid:8");
    assert_ne!(seven, eight, "distinct failures must not share a key");
}
