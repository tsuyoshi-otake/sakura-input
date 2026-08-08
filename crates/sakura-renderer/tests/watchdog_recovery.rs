//! Does the watchdog actually bring a killed engine back?
//!
//! `watch.rs` has unit tests for the *rule* — [`decide`] returns Launch,
//! Wait or GiveUp for the right reasons — and they deliberately stop there,
//! because a unit test that really started an engine would seize the pipe a
//! developer's live IME is using. That leaves the interesting half unproven:
//! nothing has ever checked that a running renderer, watching a real engine
//! over a real pipe, notices the engine dying and starts another one.
//!
//! That gap matters more than most. PLAN.md's Phase 1 crash-resilience
//! criterion says the watchdog restarts the engine, and if it does not, the
//! failure mode is not a stutter — the IME is gone until the user signs out
//! and back in, and nothing on screen explains why.
//!
//! # Why this test runs a control first
//!
//! "Kill the engine, wait, find the pipe answering again" proves the pipe
//! came back. It does not prove the *renderer* is why. A stray engine from
//! an earlier run, a logon task firing, or a developer's own IME starting
//! would all produce the same green tick.
//!
//! So the test does it twice. First with no renderer at all, where the pipe
//! must stay dead — that establishes that nothing ambient on this machine
//! resurrects engines. Then with the renderer running, where it must come
//! back. The second result only means something because the first one
//! happened.
//!
//! # Why it is ignored by default
//!
//! It starts and kills the per-logon engine singleton, and it puts a real
//! renderer on the desktop for a few seconds, tray icon and all. Both are
//! fine when someone asks for them and rude when they happen during an
//! ordinary `cargo test`. It also refuses to run at all if an engine it did
//! not start is already serving, rather than killing someone's IME to make
//! room for itself.
//!
//! ```text
//! cargo build --workspace
//! set SAKURA_PHASE1_DICTIONARY=C:\path\to\system.dic
//! cargo test -p sakura-renderer --test watchdog_recovery -- --ignored --nocapture
//! ```
//!
//! `cargo build --workspace` first because the watchdog launches
//! `sakura_engine.exe` from beside its own executable, and cargo will not
//! build another package's binary just because this test wants it there.
//!
//! Not in CI: the renderer creates windows and a notification-area icon, and
//! a hosted runner is the wrong place to find out what that does.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::thread::sleep;
use std::time::{Duration, Instant};

use sakura_ipc::Client;
use sakura_proto::{KeyCode, KeyInput, Modifiers, Request, Response, SessionId, PROTOCOL_VERSION};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// How long to allow for the watchdog to notice and relaunch.
///
/// The watchdog's own timings are much tighter — the first relaunch is not
/// delayed at all, and the reconnect backoff starts at 250 ms — so this is
/// mostly headroom for a cold process start on a machine that is busy doing
/// something else. It is a ceiling on "did it happen", not a measurement of
/// how long it took.
const RECOVERY_BUDGET: Duration = Duration::from_secs(30);

/// How long the control phase watches a pipe that must stay dead.
///
/// Long enough that a relaunch would comfortably have happened by now —
/// several times the watchdog's own reconnect floor — and short enough that
/// the test does not spend most of its life proving a negative.
const CONTROL_WATCH: Duration = Duration::from_secs(5);

/// A connect attempt used to ask "is anything serving?", not to do work.
const PROBE: Duration = Duration::from_millis(200);

/// Long enough to cover a cold start under load.
const PATIENT: Duration = Duration::from_secs(5);

#[test]
#[ignore = "starts, kills and restarts the engine singleton and puts a renderer on the desktop"]
fn a_killed_engine_comes_back_only_when_the_renderer_is_watching() {
    refuse_if_anything_is_already_running();
    let dictionary = required_dictionary();

    // ---- Control: no watchdog, so nothing should resurrect the engine ----
    {
        let mut engine = Spawned::engine(&dictionary);
        wait_until_serving("the engine this test started");
        engine.kill_now();
        wait_until_silent();

        let deadline = Instant::now() + CONTROL_WATCH;
        while Instant::now() < deadline {
            assert!(
                Client::connect(PROBE).is_err(),
                "an engine came back with no renderer running, so something \
                 else on this machine restarts engines and the second half of \
                 this test would prove nothing about the watchdog"
            );
            sleep(Duration::from_millis(200));
        }
        println!("control: pipe stayed dead for {CONTROL_WATCH:?} with no renderer");
    }

    // ---- The real thing: the renderer is watching ----
    let mut engine = Spawned::engine(&dictionary);
    wait_until_serving("the engine this test started");

    let mut renderer = Spawned::renderer(&dictionary);
    // The renderer needs to have connected before the kill for this to be
    // the case we care about. It is not observable from outside the process,
    // so this settles rather than polls — and if it has *not* connected yet,
    // the watchdog's very first connect fails and it relaunches anyway, which
    // reaches the same place by a slightly different route.
    sleep(Duration::from_secs(1));

    engine.kill_now();
    wait_until_silent();
    let killed_at = Instant::now();

    let deadline = killed_at + RECOVERY_BUDGET;
    loop {
        if Client::connect(PROBE).is_ok() {
            println!("recovered after {:?}", killed_at.elapsed());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the engine was still gone {RECOVERY_BUDGET:?} after being killed, \
             with the renderer running: the watchdog did not restart it, and a \
             user hitting this would lose their IME until the next logon"
        );
        sleep(Duration::from_millis(100));
    }

    // Reachable is not the same as working. What the criterion promises is
    // that typing resumes, so type.
    let mut client = Client::connect(PATIENT).expect("the pipe just answered");
    let session = handshake_and_open(&mut client);
    let mut composed = String::new();
    for c in "sa".chars() {
        if let Response::Output(output) = send(&mut client, session, char_key(c)) {
            let text: String = output
                .preedit
                .map(|p| p.segments.into_iter().map(|s| s.text).collect())
                .unwrap_or_default();
            if !text.is_empty() {
                composed = text;
            }
        }
    }
    assert_eq!(
        composed, "さ",
        "the restarted engine answers the pipe but does not compose, so the \
         watchdog brought back something that is not a working IME"
    );

    // Teardown, in this order for a reason. The renderer goes first, because
    // while it is alive it is doing its job: stop the engine with a watchdog
    // still watching and it starts another one, and the test leaks the very
    // thing whose leak corrupts the next run.
    renderer.kill_now();
    // Then ask the engine to stop rather than killing it, so the last engine
    // standing is one that shut down the way a real one does.
    let _ = client.call(&Request::Shutdown, PATIENT);
    drop(client);

    let deadline = Instant::now() + PATIENT;
    loop {
        let left = running_processes();
        if left.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "this test left {left:?} running, which would make the next run's \
             control phase lie"
        );
        sleep(Duration::from_millis(100));
    }
}

/// Stops before touching anything if any part of Sakura Input this test did
/// not start is already running.
///
/// Two separate reasons, and the second one was found the hard way.
///
/// The engine is a per-logon singleton, so there is no way to test killing
/// one without killing *the* one. On a developer's machine that is very
/// likely their real IME, mid-sentence. Refusing with instructions is the
/// only honest option; silently taking it over is not.
///
/// The renderer has to be checked too, and probing the pipe does not find
/// it: a leftover renderer with no engine to talk to answers nothing and
/// looks exactly like a clean machine. It is also the single worst thing
/// that can be running during this test, because it is *another watchdog* —
/// it will restart the engine during the control phase and make the control
/// prove the opposite of what it is for. That is not hypothetical. An early
/// run of this test saw the engine come back 12.75 s after being killed with
/// no renderer of its own started, which is about one `WATCH_BUDGET` after
/// the kill: a renderer left over from the previous run, still sitting in
/// its long poll, noticed and relaunched. The control's five-second window
/// missed it, so the test would have passed for entirely the wrong reason.
fn refuse_if_anything_is_already_running() {
    assert!(
        Client::connect(PROBE).is_err(),
        "an engine is already serving this logon session, and this test kills \
         the engine on purpose. Stop it first (sakura_regtool --stop, or close \
         the renderer) and run this again."
    );
    let running = running_processes();
    assert!(
        running.is_empty(),
        "these Sakura Input processes are already running: {running:?}. A \
         leftover renderer is another watchdog, and it would restart the \
         engine during the control phase — the test would then pass without \
         proving anything. Stop them and run this again."
    );
}

/// The names of any Sakura Input processes currently running.
///
/// Enumerated rather than inferred from the pipe, for the reason in
/// [`refuse_if_anything_is_already_running`]: the process that matters most
/// here is the one that holds no pipe of its own.
fn running_processes() -> Vec<String> {
    const WATCHED: [&str; 2] = ["sakura_engine.exe", "sakura_renderer.exe"];

    // SAFETY: `TH32CS_SNAPPROCESS` with pid 0 snapshots every process and is
    // the documented call; the returned handle is closed below.
    let snapshot = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        Ok(handle) => handle,
        // Not being able to look is not the same as nothing being there, but
        // failing the test over it would be worse than proceeding: the pipe
        // probe above still covers the case that matters most.
        Err(_) => return Vec::new(),
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = Vec::new();
    // SAFETY: `snapshot` is live for this whole block and `entry` has its
    // `dwSize` set, which is what both calls require.
    unsafe {
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = String::from_utf16_lossy(
                    &entry.szExeFile[..entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len())],
                );
                if WATCHED.iter().any(|w| w.eq_ignore_ascii_case(&name)) {
                    found.push(name);
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    found
}

/// A child process this test is responsible for, killed on drop.
#[derive(Debug)]
struct Spawned {
    what: &'static str,
    child: Option<Child>,
}

impl Spawned {
    fn engine(dictionary: &std::path::Path) -> Spawned {
        Spawned::start("sakura_engine.exe", &engine_exe(), dictionary)
    }

    fn renderer(dictionary: &std::path::Path) -> Spawned {
        Spawned::start(
            "sakura_renderer.exe",
            &PathBuf::from(RENDERER_EXE),
            dictionary,
        )
    }

    fn start(what: &'static str, path: &std::path::Path, dictionary: &std::path::Path) -> Spawned {
        assert!(
            path.exists(),
            "{what} is not at {}. Run `cargo build --workspace` first: the \
             watchdog launches the engine from beside its own executable, and \
             cargo does not build another package's binary for this test.",
            path.display()
        );
        let child = Command::new(path)
            .env("SAKURA_DICTIONARY", dictionary)
            .spawn()
            .unwrap_or_else(|error| panic!("could not start {what}: {error}"));
        Spawned {
            what,
            child: Some(child),
        }
    }

    /// Kills it now, and waits, so that "the process is gone" is true by the
    /// time this returns rather than eventually.
    fn kill_now(&mut self) {
        if let Some(mut child) = self.child.take() {
            child
                .kill()
                .unwrap_or_else(|error| panic!("could not kill {}: {error}", self.what));
            let _ = child.wait();
        }
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// The engine beside the renderer, which is where the watchdog looks for it.
fn engine_exe() -> PathBuf {
    let mut path = PathBuf::from(RENDERER_EXE);
    path.pop();
    path.push("sakura_engine.exe");
    path
}

const RENDERER_EXE: &str = env!("CARGO_BIN_EXE_sakura_renderer");

fn required_dictionary() -> PathBuf {
    let value = std::env::var_os("SAKURA_PHASE1_DICTIONARY")
        .expect("SAKURA_PHASE1_DICTIONARY must name a real release dictionary");
    let path = PathBuf::from(value);
    assert!(
        path.is_file(),
        "dictionary is not a file: {}",
        path.display()
    );
    path
}

fn wait_until_serving(who: &str) {
    let deadline = Instant::now() + PATIENT;
    while Instant::now() < deadline {
        if Client::connect(PROBE).is_ok() {
            return;
        }
        sleep(Duration::from_millis(20));
    }
    panic!("{who} never started serving the pipe within {PATIENT:?}");
}

/// Waits for the pipe to stop answering after a kill.
///
/// Not instantaneous: a pipe instance already created can be connected to
/// for a moment after the process holding it dies. Treating that window as
/// "still alive" would make the recovery measurement start too early.
fn wait_until_silent() {
    let deadline = Instant::now() + PATIENT;
    while Instant::now() < deadline {
        if Client::connect(PROBE).is_err() {
            return;
        }
        sleep(Duration::from_millis(20));
    }
    panic!("the pipe was still answering {PATIENT:?} after the engine was killed");
}

fn handshake_and_open(client: &mut Client) -> SessionId {
    match client.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        PATIENT,
    ) {
        Ok(Response::Hello { .. }) => {}
        other => panic!("the restarted engine did not handshake: {other:?}"),
    }
    match client.call(
        &Request::CreateSession {
            process_name: "watchdog_recovery.exe".to_owned(),
        },
        PATIENT,
    ) {
        Ok(Response::SessionCreated { session }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    }
}

fn send(client: &mut Client, session: SessionId, key: KeyInput) -> Response {
    client
        .call(&Request::SendKey { session, key }, PATIENT)
        .unwrap_or_else(|fault| panic!("SendKey failed against the restarted engine: {fault:?}"))
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
