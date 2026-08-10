//! The engine feed, and the watchdog that keeps it alive.
//!
//! One thread that stays connected to the engine and long-polls
//! `Request::WatchUi` for as long as the renderer runs. Everything the
//! renderer draws arrives here; the UI thread never talks to the pipe,
//! because a blocked pipe read on the thread that pumps messages is a
//! hung desktop.
//!
//! # Why the renderer is the watchdog
//!
//! The DLL runs inside every host application, sometimes at high integrity
//! and sometimes in an AppContainer, and must never spawn anything (DESIGN
//! 3): a process launched from inside a sandboxed host inherits a token
//! nobody wants, and a process launched from an elevated host runs
//! elevated. The renderer is an ordinary medium-integrity process the user
//! owns, started at logon alongside the engine, so it is the one part of
//! the system that can restart the engine with the right token. That is
//! the only reason this module starts processes at all.
//!
//! The hard part is not restarting a crashed engine — the pipe breaking
//! says that plainly. It is *not* restarting one that stopped on purpose,
//! which looks identical from here. Two things distinguish them: the
//! engine announces its own shutdown before the pipe breaks
//! ([`UiState::stopping`]), and a missing engine binary means the product
//! is being uninstalled. Either one ends this thread instead of relaunching
//! anything.

use std::ffi::c_void;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, sleep, JoinHandle};
use std::time::{Duration, Instant};

use sakura_ipc::{Client, Fault, PATIENT_CONNECT};
use sakura_proto::{Request, Response, UiState, PROTOCOL_VERSION};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// The file the watchdog restarts, beside this executable.
const ENGINE_EXE: &str = "sakura_engine.exe";

/// How long a `WatchUi` call may take before it is treated as a fault.
///
/// Must exceed the engine's heartbeat, which is what bounds the call in
/// normal operation: the engine answers an unchanged state after five
/// seconds precisely so that a wedged engine is noticed. A budget shorter
/// than that heartbeat would make every idle poll look like a failure and
/// put the watchdog in a restart loop against a perfectly healthy engine.
const WATCH_BUDGET: Duration = Duration::from_secs(15);

/// How long to wait after a failed connection before trying again, and the
/// ceiling that backoff climbs to.
///
/// The floor is short because the common case for a failed connection is a
/// restart already in flight; the ceiling is what keeps a permanently
/// broken engine from costing a wake-up every second forever.
const RETRY_FLOOR: Duration = Duration::from_millis(250);
const RETRY_CEILING: Duration = Duration::from_secs(30);

/// The minimum gap between two launches of the engine.
///
/// An engine that crashes during startup would otherwise be relaunched as
/// fast as this thread can loop, which turns one bug into a fork bomb that
/// makes the machine unusable — and does it at logon, where the user has no
/// obvious way to intervene.
const RELAUNCH_GAP: Duration = Duration::from_secs(5);
const HISTORY_DELETE_BUDGET: Duration = Duration::from_secs(2);
const HISTORY_DELETE_QUEUE_CAPACITY: usize = 8;

/// A deletion request carries only the opaque UI snapshot coordinates. The
/// engine, not the renderer, resolves those coordinates to learned history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryDeleteRequest {
    pub revision: u64,
    pub candidate_index: u16,
}

/// Completion proves only that the engine answered the deletion request. It
/// never authorizes a renderer-side candidate mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryDeleteCompletion(pub HistoryDeleteRequest);

/// Starts one bounded command worker separate from the long-poll watcher.
///
/// A `WatchUi` call may block for its heartbeat, so sharing that client would
/// make a click wait seconds. This worker owns short-lived independent pipe
/// clients; it never changes the popup locally and engine UI updates remain the
/// sole confirmation path.
pub fn spawn_history_deleter(
    notify_target: isize,
    notify_message: u32,
    test_pipe: Option<String>,
) -> (
    SyncSender<HistoryDeleteRequest>,
    Receiver<HistoryDeleteCompletion>,
) {
    let (sender, receiver) =
        mpsc::sync_channel::<HistoryDeleteRequest>(HISTORY_DELETE_QUEUE_CAPACITY);
    let (completed_sender, completed_receiver) = mpsc::channel();
    let binding = match test_pipe {
        Some(pipe_name) => PipeBinding::Test(pipe_name),
        None => PipeBinding::Production,
    };
    thread::Builder::new()
        .name("sakura-history-delete".to_owned())
        .spawn(move || {
            while let Ok(request) = receiver.recv() {
                let Ok(mut client) = binding.connect() else {
                    continue;
                };
                if !matches!(
                    client.call(
                        &Request::Hello {
                            client_version: PROTOCOL_VERSION,
                        },
                        HISTORY_DELETE_BUDGET,
                    ),
                    Ok(Response::Hello { server_version, .. }) if server_version == PROTOCOL_VERSION
                ) {
                    continue;
                }
                // `removed: false` is a terminal no-op. Either response only
                // clears click de-duplication; the current popup remains until
                // WatchUi sends an engine-authored snapshot.
                if matches!(
                    client.call(
                        &Request::DeleteHistoryCandidate {
                            revision: request.revision,
                            candidate_index: request.candidate_index,
                        },
                        HISTORY_DELETE_BUDGET,
                    ),
                    Ok(Response::HistoryCandidateDeleted { .. })
                ) {
                    let _ = completed_sender.send(HistoryDeleteCompletion(request));
                    // SAFETY: this numeric HWND is the main thread's live host
                    // until process teardown. A failed post only means it has
                    // already gone away and the completion can be dropped.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(notify_target as *mut c_void)),
                            notify_message,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            }
        })
        .expect("the renderer cannot create its bounded history-delete worker");
    (sender, completed_receiver)
}

/// What the watcher tells the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// New state to draw.
    Ui(Box<UiState>),
    /// The feed has ended for good and the renderer should exit: the engine
    /// said it was stopping, or it is gone and not coming back.
    Ended,
}

/// Starts the watcher thread, reporting every state change to `sink`.
///
/// `sink` is called on the watcher thread, not the UI thread, so it must
/// not touch a window directly — window handles belong to the thread that
/// created them. It is meant to be a `PostMessageW`, which is the
/// documented way to cross that boundary.
pub fn spawn(sink: impl Fn(Signal) + Send + 'static) -> JoinHandle<()> {
    spawn_on(PipeBinding::Production, sink)
}

/// Starts a fixture-only watcher on a caller-provided private pipe. Unlike the
/// production watcher, any disconnect is terminal: a test must never launch
/// the adjacent product engine as a recovery action.
pub fn spawn_test_pipe(
    pipe_name: String,
    sink: impl Fn(Signal) + Send + 'static,
) -> JoinHandle<()> {
    spawn_on(PipeBinding::Test(pipe_name), sink)
}

fn spawn_on(binding: PipeBinding, sink: impl Fn(Signal) + Send + 'static) -> JoinHandle<()> {
    thread::Builder::new()
        .name("sakura-watch".to_owned())
        .spawn(move || run(&sink, &binding))
        .expect("the renderer cannot work without its watcher thread")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PipeBinding {
    Production,
    Test(String),
}

impl PipeBinding {
    fn connect(&self) -> Result<Client, Fault> {
        match self {
            Self::Production => Client::connect(PATIENT_CONNECT),
            Self::Test(pipe_name) => Client::connect_to(pipe_name, PATIENT_CONNECT),
        }
    }

    fn is_test(&self) -> bool {
        matches!(self, Self::Test(_))
    }
}

fn run(sink: &impl Fn(Signal), binding: &PipeBinding) {
    let mut backoff = RETRY_FLOOR;
    // Starts in the past so the first launch is never delayed, which
    // matters at logon: the renderer and the engine race to start, and the
    // renderer frequently wins.
    let mut launched: Option<Instant> = None;

    loop {
        match binding.connect() {
            Ok(client) => {
                let ending = follow(client, sink);
                if ending == Ending::Deliberate || binding.is_test() {
                    sink(Signal::Ended);
                    return;
                }
                // A connection that reached a valid Hello was healthy before
                // it dropped, so restart at the floor. A peer that repeatedly
                // rejects the protocol is still reachable, however: retaining
                // the accumulated delay prevents an immediate reconnect storm
                // while an installer finishes replacing mixed versions.
                let (delay, next) = retry_schedule(ending, backoff)
                    .expect("a non-deliberate ending has a retry schedule");
                sleep(delay);
                backoff = next;
            }
            Err(_) => {
                if binding.is_test() {
                    sink(Signal::Ended);
                    return;
                }
                if !relaunch(&mut launched) {
                    sink(Signal::Ended);
                    return;
                }
                sleep(backoff);
                backoff = grow_backoff(backoff);
            }
        }
    }
}

/// Why a connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// The engine said it was stopping.
    Deliberate,
    /// A valid protocol session existed, then its pipe broke or stopped making
    /// sense. The next retry may start from the floor.
    ConnectionLost,
    /// The peer never accepted this build's protocol. Repeated attempts keep
    /// their accumulated backoff until a valid handshake is observed.
    ProtocolRejected,
}

fn grow_backoff(delay: Duration) -> Duration {
    (delay * 2).min(RETRY_CEILING)
}

fn retry_schedule(ending: Ending, current: Duration) -> Option<(Duration, Duration)> {
    match ending {
        Ending::Deliberate => None,
        Ending::ConnectionLost => Some((RETRY_FLOOR, grow_backoff(RETRY_FLOOR))),
        Ending::ProtocolRejected => Some((current, grow_backoff(current))),
    }
}

/// Handshakes, then reports every state the engine publishes until the
/// connection ends.
fn follow(mut client: Client, sink: &impl Fn(Signal)) -> Ending {
    match client.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        PATIENT_CONNECT,
    ) {
        Ok(Response::Hello { .. }) => {}
        // Anything else means this build and the engine's do not agree on
        // the protocol, which a retry cannot fix. Treating it as a lost
        // connection rather than a deliberate stop is deliberate: the
        // backoff will slow the retries down, and if the mismatch is
        // because an upgrade replaced the engine mid-session, the next
        // connection is to the new one and simply works.
        _ => return Ending::ProtocolRejected,
    }

    // Nobody's revision, so the first call is answered immediately with
    // whatever is true now instead of blocking until the user happens to
    // change mode.
    let mut since = 0;
    loop {
        match client.call(&Request::WatchUi { since }, WATCH_BUDGET) {
            Ok(Response::Ui(state)) => {
                if state.stopping {
                    return Ending::Deliberate;
                }
                // Heartbeat replies deliberately repeat a revision. They
                // prove liveness but are not redraw requests: forwarding one
                // would make the transient mode indicator flash every five
                // seconds while the user is idle.
                if state.revision != since {
                    since = state.revision;
                    sink(Signal::Ui(Box::new(state)));
                }
            }
            Ok(_) => return Ending::ConnectionLost,
            Err(Fault::Disconnected) => return Ending::ConnectionLost,
            Err(_) => return Ending::ConnectionLost,
        }
    }
}

/// What the watchdog should do about an engine it cannot reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// Start it.
    Launch,
    /// One was started too recently to start another.
    Wait,
    /// There is no engine to start, now or later.
    GiveUp,
}

/// Separated from [`relaunch`] so the rule can be tested without this
/// process actually starting an engine — which a test on a developer's
/// machine must not do, since the engine is a per-logon singleton and would
/// take over the pipe the user's real IME is using.
fn decide(engine: Option<&std::path::Path>, launched: Option<Instant>) -> Decision {
    match engine {
        // Missing binary is the state an uninstall leaves behind: nothing
        // left to watch, and a renderer that keeps trying is a process the
        // uninstaller cannot get rid of.
        None => Decision::GiveUp,
        Some(path) if !path.exists() => Decision::GiveUp,
        // Too soon. Still worth staying alive — the engine started a moment
        // ago and may be a moment away from serving.
        Some(_) if launched.is_some_and(|at| at.elapsed() < RELAUNCH_GAP) => Decision::Wait,
        Some(_) => Decision::Launch,
    }
}

/// Starts the engine if it is time to, reporting whether the watchdog
/// should keep going at all.
fn relaunch(launched: &mut Option<Instant>) -> bool {
    let engine = engine_path();
    match decide(engine.as_deref(), *launched) {
        Decision::GiveUp => false,
        Decision::Wait => true,
        Decision::Launch => {
            *launched = Some(Instant::now());
            // A failed spawn is not fatal. The engine may be locked by an
            // installer that is mid-replace, in which case the next attempt
            // succeeds.
            let _ = Command::new(engine.expect("decided to launch it")).spawn();
            true
        }
    }
}

/// The engine beside this executable.
///
/// Resolved relative to the renderer rather than looked up on `PATH` or in
/// the registry: both are writable by things that are not us, and this
/// process would be launching whatever they name. The install layout puts
/// both executables in one directory (DESIGN 12), so the sibling is always
/// the right answer and is the only answer an attacker cannot redirect
/// without already being able to replace the renderer itself.
fn engine_path() -> Option<PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.pop();
    path.push(ENGINE_EXE);
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The poll budget has to outlast the engine's heartbeat or every idle
    /// poll times out and the watchdog restarts a healthy engine.
    #[test]
    fn the_watch_budget_outlasts_the_engines_heartbeat() {
        // Not imported from the engine: the renderer must not depend on it
        // (that would make the UI process carry the conversion engine), so
        // the number is restated here and this test is what keeps the two
        // in step.
        let engine_heartbeat = Duration::from_secs(5);
        assert!(
            WATCH_BUDGET > engine_heartbeat * 2,
            "a {WATCH_BUDGET:?} budget leaves no room over a \
             {engine_heartbeat:?} heartbeat plus a slow schedule"
        );
    }

    #[test]
    fn backoff_climbs_but_stops_at_the_ceiling() {
        let mut backoff = RETRY_FLOOR;
        for _ in 0..20 {
            backoff = grow_backoff(backoff);
        }
        assert_eq!(backoff, RETRY_CEILING);
    }

    #[test]
    fn protocol_rejection_keeps_exponential_backoff_instead_of_resetting_it() {
        let current = Duration::from_secs(4);
        assert_eq!(
            retry_schedule(Ending::ProtocolRejected, current),
            Some((current, Duration::from_secs(8)))
        );
    }

    #[test]
    fn a_previously_healthy_connection_restarts_from_the_retry_floor() {
        assert_eq!(
            retry_schedule(Ending::ConnectionLost, RETRY_CEILING),
            Some((RETRY_FLOOR, grow_backoff(RETRY_FLOOR)))
        );
        assert_eq!(retry_schedule(Ending::Deliberate, RETRY_CEILING), None);
    }

    /// The watchdog only ever launches a sibling of itself. Anything else
    /// would mean a writable lookup path decided what this process starts.
    #[test]
    fn the_engine_is_resolved_beside_this_executable() {
        let engine = engine_path().expect("this process has a path");
        let here = std::env::current_exe().expect("this process has a path");
        assert_eq!(engine.parent(), here.parent());
        assert_eq!(engine.file_name().unwrap(), ENGINE_EXE);
    }

    /// A missing engine ends the watchdog rather than starting a retry loop
    /// against a product that is being uninstalled — the case that would
    /// otherwise leave a process the uninstaller cannot remove.
    #[test]
    fn a_missing_engine_ends_the_watchdog() {
        let missing = PathBuf::from(r"C:\nonexistent\sakura_engine.exe");
        assert!(
            !missing.exists(),
            "the test's premise is that this path is absent"
        );
        assert_eq!(decide(Some(&missing), None), Decision::GiveUp);
        assert_eq!(decide(None, None), Decision::GiveUp);
    }

    /// A present engine that has not just been started is launched.
    #[test]
    fn a_present_engine_that_is_not_running_is_launched() {
        // This test's own binary: a file that certainly exists, standing in
        // for the engine so the rule can be checked without one.
        let present = std::env::current_exe().expect("this process has a path");
        assert_eq!(decide(Some(&present), None), Decision::Launch);
    }

    /// An engine that crashes on startup must not be relaunched as fast as
    /// the watcher can loop. That is a fork bomb, and it happens at logon
    /// where the user has no obvious way to intervene.
    #[test]
    fn an_engine_started_a_moment_ago_is_not_started_again() {
        let present = std::env::current_exe().expect("this process has a path");
        assert_eq!(decide(Some(&present), Some(Instant::now())), Decision::Wait);
    }
}
