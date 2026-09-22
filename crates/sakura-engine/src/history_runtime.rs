//! Asynchronous lifecycle for the optional developer-history service.
//!
//! Enabling starts inactive: opening and validating an existing store can take
//! seconds, so callers receive `None` until the worker publishes a completed
//! service. A failed generation is terminal and is not retried by requests;
//! an explicit disable followed by enable creates the next generation. Both
//! publication and retirement are generation checked, so a service which
//! finishes opening after disable is stopped without ever becoming visible.

use std::fmt;
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use crate::input_history::InputHistoryService;

type OpenHistory = dyn Fn() -> Result<Arc<InputHistoryService>, String> + Send + Sync;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Disabled,
    Starting(u64),
    Active(u64),
    Failed { generation: u64, error: String },
    Shutdown,
}

struct State {
    desired_enabled: bool,
    generation: u64,
    phase: Phase,
    service: Option<Arc<InputHistoryService>>,
    retiring: Vec<Arc<InputHistoryService>>,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    opener: Arc<OpenHistory>,
    verbose: bool,
}

/// The sole owner of history initialization, publication, retirement, and
/// shutdown. Configuration publication updates desired state; request threads
/// only snapshot its small state mutex.
pub(crate) struct HistoryRuntime {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for HistoryRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        formatter
            .debug_struct("HistoryRuntime")
            .field("generation", &state.generation)
            .field("phase", &state.phase)
            .finish_non_exhaustive()
    }
}

impl HistoryRuntime {
    pub(crate) fn new(
        initial_service: Option<Arc<InputHistoryService>>,
        enabled: bool,
        verbose: bool,
    ) -> io::Result<Self> {
        Self::with_opener(initial_service, enabled, verbose, || {
            let path = crate::input_history::default_path().map_err(|error| error.to_string())?;
            InputHistoryService::open(&path).map_err(|error| error.to_string())
        })
    }

    fn with_opener(
        initial_service: Option<Arc<InputHistoryService>>,
        enabled: bool,
        verbose: bool,
        opener: impl Fn() -> Result<Arc<InputHistoryService>, String> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let active = enabled && initial_service.is_some();
        let phase = if active {
            Phase::Active(1)
        } else if enabled {
            Phase::Starting(1)
        } else {
            Phase::Disabled
        };
        let mut retiring = Vec::new();
        let service = if active {
            initial_service
        } else {
            if let Some(service) = initial_service {
                retiring.push(service);
            }
            None
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                desired_enabled: enabled,
                generation: u64::from(enabled),
                phase,
                service,
                retiring,
                shutdown: false,
            }),
            changed: Condvar::new(),
            opener: Arc::new(opener),
            verbose,
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("sakura-history-runtime".to_owned())
            .spawn(move || worker_loop(worker_shared))?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Applies a newly published configuration generation. Requests must use
    /// [`Self::service`] so an older request snapshot cannot undo a newer
    /// watcher publication.
    pub(crate) fn configure(&self, enabled: bool) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.shutdown {
            return;
        }
        if state.desired_enabled != enabled {
            state.desired_enabled = enabled;
            state.generation = state.generation.saturating_add(1);
            let generation = state.generation;
            if enabled {
                state.phase = Phase::Starting(generation);
            } else {
                sakura_ipc::debug_trace::set_enabled(false);
                if let Some(service) = state.service.take() {
                    state.retiring.push(service);
                }
                state.phase = Phase::Disabled;
            }
            self.shared.changed.notify_one();
        }
    }

    /// Returns the active generation without changing lifecycle state.
    /// Starting, disabled, failed, and shutdown states are inactive.
    pub(crate) fn service(&self) -> Option<Arc<InputHistoryService>> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .service
            .clone()
    }

    /// Stops every published or in-progress generation and joins its owner.
    pub(crate) fn stop(&self) {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state.shutdown {
                state.shutdown = true;
                // Only an enabled owner can have published the engine trace.
                // Dropping the disabled startup placeholder must not clear a
                // replacement runtime's already-published trace.
                if state.desired_enabled {
                    sakura_ipc::debug_trace::set_enabled(false);
                }
                state.desired_enabled = false;
                state.generation = state.generation.saturating_add(1);
                if let Some(service) = state.service.take() {
                    state.retiring.push(service);
                }
                state.phase = Phase::Shutdown;
                self.shared.changed.notify_one();
            }
        }
        // Keep this lock through join: every concurrent stop caller observes
        // the same completed terminal state rather than returning while the
        // first caller still owns an in-progress join.
        let mut worker = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(handle) = worker.take() {
            if handle.join().is_err() && self.shared.verbose {
                eprintln!("sakura-engine: developer input history lifecycle worker panicked");
            }
        }
    }

    #[cfg(test)]
    fn phase(&self) -> Phase {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .phase
            .clone()
    }

    #[cfg(test)]
    fn active_service(&self) -> Option<Arc<InputHistoryService>> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .service
            .clone()
    }
}

impl Drop for HistoryRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let (generation, retiring, shutdown) = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while state.retiring.is_empty()
                && !state.shutdown
                && !matches!(state.phase, Phase::Starting(_))
            {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            let generation = match state.phase {
                Phase::Starting(generation) => Some(generation),
                _ => None,
            };
            let retiring = std::mem::take(&mut state.retiring);
            (generation, retiring, state.shutdown)
        };

        for service in retiring {
            stop_service(&shared, &service);
        }
        if shutdown && generation.is_none() {
            return;
        }
        let Some(generation) = generation else {
            continue;
        };

        let opened = (shared.opener)();
        let mut stale_service = None;
        {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let publish = !state.shutdown
                && state.desired_enabled
                && state.generation == generation
                && state.phase == Phase::Starting(generation);
            match opened {
                Ok(service) if publish => {
                    sakura_ipc::debug_trace::set_enabled(true);
                    state.service = Some(service);
                    state.phase = Phase::Active(generation);
                }
                Ok(service) => stale_service = Some(service),
                Err(error) if publish => {
                    if shared.verbose {
                        eprintln!(
                            "sakura-engine: developer input history unavailable; continuing without it: {error}"
                        );
                    }
                    state.phase = Phase::Failed { generation, error };
                }
                Err(_) => {}
            }
        }
        if let Some(service) = stale_service {
            stop_service(&shared, &service);
        }
    }
}

fn stop_service(shared: &Shared, service: &InputHistoryService) {
    if let Err(error) = service.stop() {
        if shared.verbose {
            eprintln!("sakura-engine: developer input history stop failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::{HistoryRuntime, Phase};
    use crate::input_history::InputHistoryService;

    #[derive(Default)]
    struct OpenControl {
        state: Mutex<(usize, usize)>,
        changed: Condvar,
    }

    impl OpenControl {
        fn enter_and_wait(&self) {
            let mut state = self.state.lock().expect("open control");
            state.0 += 1;
            let call = state.0;
            self.changed.notify_all();
            while state.1 < call {
                state = self.changed.wait(state).expect("open control wait");
            }
        }

        fn wait_for_calls(&self, calls: usize) {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut state = self.state.lock().expect("open control");
            while state.0 < calls {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "opener call {calls} did not start");
                let (next, result) = self
                    .changed
                    .wait_timeout(state, remaining)
                    .expect("open control wait");
                state = next;
                assert!(!result.timed_out(), "opener call {calls} did not start");
            }
        }

        fn release(&self, calls: usize) {
            let mut state = self.state.lock().expect("open control");
            state.1 = calls;
            self.changed.notify_all();
        }
    }

    fn test_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "sakura_history_runtime_{name}_{}_{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir_all(&directory).expect("history runtime test directory");
        directory.join("input.bin")
    }

    fn controlled_runtime(
        path: PathBuf,
        control: Arc<OpenControl>,
        opens: Arc<AtomicUsize>,
    ) -> HistoryRuntime {
        HistoryRuntime::with_opener(None, true, false, move || {
            opens.fetch_add(1, Ordering::SeqCst);
            control.enter_and_wait();
            InputHistoryService::open(&path).map_err(|error| error.to_string())
        })
        .expect("start test history lifecycle")
    }

    fn wait_for_phase(runtime: &HistoryRuntime, expected: impl Fn(&Phase) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let phase = runtime.phase();
            if expected(&phase) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "unexpected terminal phase: {phase:?}"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn stalled_open_is_not_a_request_path_stall_and_publishes_when_current() {
        let control = Arc::new(OpenControl::default());
        let opens = Arc::new(AtomicUsize::new(0));
        let runtime = controlled_runtime(test_path("stalled"), Arc::clone(&control), opens);
        control.wait_for_calls(1);

        let started = Instant::now();
        assert!(runtime.service().is_none(), "starting is inactive");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "a stalled opener blocked the request path"
        );
        control.release(1);
        wait_for_phase(&runtime, |phase| matches!(phase, Phase::Active(1)));
        assert!(runtime.active_service().is_some());
        println!("lifecycle-evidence history.runtime_current_open published");
        runtime.stop();
    }

    #[test]
    fn disable_during_open_never_publishes_the_stale_generation() {
        let control = Arc::new(OpenControl::default());
        let opens = Arc::new(AtomicUsize::new(0));
        let runtime = controlled_runtime(
            test_path("disable-during-open"),
            Arc::clone(&control),
            opens,
        );
        control.wait_for_calls(1);
        runtime.configure(false);
        assert!(runtime.service().is_none());
        control.release(1);
        wait_for_phase(&runtime, |phase| matches!(phase, Phase::Disabled));
        assert!(runtime.active_service().is_none());
        println!("lifecycle-evidence history.runtime_disabled_open retired");
        runtime.stop();
    }

    #[test]
    fn repeated_toggles_open_once_per_enabled_generation() {
        let control = Arc::new(OpenControl::default());
        let opens = Arc::new(AtomicUsize::new(0));
        let runtime = controlled_runtime(
            test_path("toggles"),
            Arc::clone(&control),
            Arc::clone(&opens),
        );
        control.wait_for_calls(1);
        assert!(runtime.service().is_none());
        runtime.configure(false);
        assert!(runtime.service().is_none());
        runtime.configure(true);
        assert!(runtime.service().is_none());
        control.release(1);
        control.wait_for_calls(2);
        assert!(
            runtime.active_service().is_none(),
            "generation 1 stayed stale"
        );
        println!("lifecycle-evidence history.runtime_stale_generation retired");
        control.release(2);
        wait_for_phase(&runtime, |phase| matches!(phase, Phase::Active(3)));
        assert!(runtime.active_service().is_some());
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert!(runtime.service().is_some());
        assert_eq!(opens.load(Ordering::SeqCst), 2, "requests must not retry");
        println!("lifecycle-evidence history.runtime_new_generation published");
        runtime.stop();
    }

    #[test]
    fn failed_generation_is_terminal_until_an_explicit_toggle() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let opener_attempts = Arc::clone(&attempts);
        let runtime = HistoryRuntime::with_opener(None, true, false, move || {
            opener_attempts.fetch_add(1, Ordering::SeqCst);
            Err("injected open failure".to_owned())
        })
        .expect("start test history lifecycle");
        wait_for_phase(&runtime, |phase| {
            matches!(phase, Phase::Failed { generation: 1, .. })
        });
        for _ in 0..20 {
            assert!(runtime.service().is_none());
        }
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "requests must not retry"
        );
        runtime.configure(false);
        runtime.configure(true);
        wait_for_phase(&runtime, |phase| {
            matches!(phase, Phase::Failed { generation: 3, .. })
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        runtime.stop();
    }

    #[test]
    fn shutdown_waits_for_the_owned_open_and_writer_join() {
        let control = Arc::new(OpenControl::default());
        let opens = Arc::new(AtomicUsize::new(0));
        let runtime = Arc::new(controlled_runtime(
            test_path("shutdown"),
            Arc::clone(&control),
            opens,
        ));
        control.wait_for_calls(1);
        let (done_sender, done_receiver) = mpsc::channel();
        let stopping = Arc::clone(&runtime);
        let joiner = std::thread::spawn(move || {
            stopping.stop();
            done_sender.send(()).expect("shutdown completion");
        });
        assert!(
            done_receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "shutdown returned before its opener"
        );
        control.release(1);
        done_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("joined lifecycle");
        joiner.join().expect("shutdown caller");
        assert!(matches!(runtime.phase(), Phase::Shutdown));
        assert!(runtime.active_service().is_none());
    }
}
