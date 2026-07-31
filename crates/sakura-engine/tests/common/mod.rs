//! What the tests that talk to a real engine over the real pipe all need.
//!
//! The engine is a per-logon-session singleton (DESIGN 4.1), so everything
//! about starting it, finding it, and — crucially — not killing somebody
//! else's is shared policy rather than per-test convenience. Getting that
//! wrong once, in one test file, is enough to take a developer's IME away
//! mid-sentence, so it lives in one place.

use std::process::{Child, Command};
use std::thread::sleep;
use std::time::{Duration, Instant};

use sakura_ipc::{pipe_name, Client};
use sakura_proto::{KeyCode, KeyInput, Modifiers, Request, Response, SessionId};

/// Long enough to cover a cold process start on a loaded machine. Nothing
/// here is measuring latency — that is `ipc_latency.rs`'s job, and it uses
/// its own budget.
pub const PATIENT: Duration = Duration::from_secs(5);

/// A running engine to talk to, and whether this test is the one that
/// started it.
///
/// The pipe name belongs to the logon session, so at most one engine serves
/// it at a time (DESIGN 4.1). On a developer's machine that engine may
/// already be running and may be mid-conversation with a real text service in
/// a real editor; killing it to run a test would take the user's IME away
/// under them. So: use whoever is already there, and only clean up a process
/// this test is responsible for.
#[derive(Debug)]
pub struct Engine {
    spawned: Option<Child>,
}

impl Engine {
    pub fn running() -> Engine {
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
    pub fn client(&mut self) -> Client {
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

pub fn char_key(c: char) -> KeyInput {
    KeyInput {
        code: KeyCode::Char,
        ch: Some(c),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

pub fn named_key(code: KeyCode) -> KeyInput {
    KeyInput {
        code,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

/// What a text service would draw: every segment's text, in order.
pub fn visible(preedit: Option<sakura_proto::Preedit>) -> String {
    preedit
        .map(|p| p.segments.into_iter().map(|s| s.text).collect())
        .unwrap_or_default()
}

pub fn session_for(client: &mut Client, process_name: &str) -> SessionId {
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
