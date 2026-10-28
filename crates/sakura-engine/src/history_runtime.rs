//! Asynchronous lifecycle for the optional developer-history service.
//!
//! Enabling starts inactive: opening and validating an existing store can take
//! seconds, so callers receive `None` until the worker publishes a completed
//! service. A failed generation is terminal and is not retried by requests;
//! an explicit disable followed by enable creates the next generation. Both
//! publication and retirement are generation checked, so a service which
//! finishes opening after disable is stopped without ever becoming visible.
//!
//! A generation's writer owns the store lock until its thread exits, and
//! retirement runs here after requests have already stopped seeing the
//! service. A caller that works on the store by path while no service is
//! attached therefore calls [`HistoryRuntime::hold_idle`] first (#417). Waiting
//! is not enough on its own: the next generation can start the moment the wait
//! ends, so the hold it returns also keeps the lifecycle from moving until the
//! caller is done.

use std::fmt;
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
    /// Services the worker has taken to stop, or opened too late to publish,
    /// whose writers have not finished releasing the store.
    stopping: usize,
    /// The worker has taken a `Starting` generation and has not yet published
    /// or discarded its result. The opener owns the store for that whole span,
    /// including after `configure(false)` has moved the phase on, so the phase
    /// alone cannot say the store is busy.
    opening: bool,
    /// The worker thread has ended, normally or by panic. Nothing is in flight
    /// and nothing will change, so waiting for idleness could only time out.
    worker_exited: bool,
    /// Outstanding [`LifecycleHold`]s. While any exists the worker takes no
    /// retiring service and starts no open, so the store keeps its owner.
    holds: usize,
    shutdown: bool,
}

impl State {
    /// No generation is changing hands: none is waiting to open, opening, or
    /// still releasing the store. The published service, if any, is `service`.
    fn is_idle(&self) -> bool {
        self.worker_exited
            || (!self.opening
                && self.stopping == 0
                && self.retiring.is_empty()
                && !matches!(self.phase, Phase::Starting(_)))
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    /// Signalled by the worker each time it goes to sleep or exits. Anything
    /// opening or stopping is the worker's own work, so those are the only
    /// moments `State::is_idle` can become true.
    settled: Condvar,
    opener: Arc<OpenHistory>,
    verbose: bool,
    /// Test seam: runs on the worker before each service is stopped, so a test
    /// can hold a retiring writer's store lock for as long as it needs.
    #[cfg(test)]
    stop_gate: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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

    pub(crate) fn with_opener(
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
                stopping: 0,
                opening: false,
                worker_exited: false,
                holds: 0,
                shutdown: false,
            }),
            changed: Condvar::new(),
            settled: Condvar::new(),
            opener: Arc::new(opener),
            verbose,
            #[cfg(test)]
            stop_gate: Mutex::new(None),
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
    /// watcher publication. What requests see changes at once; the opening and
    /// stopping are the worker's, and a [`LifecycleHold`] defers them. This
    /// never waits for a hold.
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

    /// Waits for at most `timeout` until no generation is opening or still
    /// releasing the store, then holds the lifecycle idle. Until the returned
    /// guard is dropped no generation starts opening and no retired service is
    /// stopped, so the store keeps the owner it had when this returned. `None`
    /// means the lifecycle was still changing hands after `timeout`.
    ///
    /// Requests stop seeing a service as soon as it is disabled, but its
    /// writer keeps the store lock until retirement here has joined it. A
    /// caller that works on the store by path while nothing is attached has to
    /// wait first, or it finds the lock still held (#417). Waiting alone would
    /// leave a gap before the work starts, in which [`Self::configure`] could
    /// start the next generation under it, so the work happens under the hold.
    /// What it works through is [`LifecycleHold::service`], read at the same
    /// instant as the idle check.
    pub(crate) fn hold_idle(&self, timeout: Duration) -> Option<LifecycleHold<'_>> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (mut state, _) = self
            .shared
            .settled
            .wait_timeout_while(state, timeout, |state| !state.is_idle())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.is_idle() {
            return None;
        }
        state.holds += 1;
        Some(LifecycleHold {
            shared: &self.shared,
            service: state.service.clone(),
        })
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

/// A lease on the history lifecycle being idle, from
/// [`HistoryRuntime::hold_idle`]. It is meant to span one short store
/// operation: while it lives the worker defers every generation change, so a
/// holder that waits on something slow delays the next developer-mode switch
/// by as long.
///
/// [`HistoryRuntime::configure`] never waits for a hold. It only updates what
/// requests see, and it runs under the configuration lock that every request
/// takes to read its snapshot, so waiting there would stall the holder itself.
pub(crate) struct LifecycleHold<'a> {
    shared: &'a Shared,
    service: Option<Arc<InputHistoryService>>,
}

impl LifecycleHold<'_> {
    /// The service that was published when the lifecycle was found idle, or
    /// `None` when there was none. [`HistoryRuntime::service`] stops returning
    /// it as soon as developer mode goes off, but it keeps running until the
    /// hold is dropped, so work dispatched to it cannot meet a stopped writer.
    pub(crate) fn service(&self) -> Option<Arc<InputHistoryService>> {
        self.service.clone()
    }
}

impl Drop for LifecycleHold<'_> {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.holds = state.holds.saturating_sub(1);
        if state.holds == 0 {
            // The worker sleeps while a hold is outstanding, however much work
            // `configure` has left for it.
            self.shared.changed.notify_one();
        }
    }
}

/// Marks the worker as gone however it ends. A panic would otherwise leave
/// `State::opening` or `State::stopping` set with nobody left to clear them,
/// and [`HistoryRuntime::hold_idle`] could only ever time out.
struct WorkerExit<'a>(&'a Shared);

impl Drop for WorkerExit<'_> {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.worker_exited = true;
        self.0.settled.notify_all();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    let _exit = WorkerExit(&shared);
    loop {
        let (generation, retiring, shutdown) = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while state.holds > 0
                || (state.retiring.is_empty()
                    && !state.shutdown
                    && !matches!(state.phase, Phase::Starting(_)))
            {
                // Nothing is opening or stopping once the worker has nothing
                // to take, so this is the moment waiters can be released. Work
                // left waiting behind a hold keeps `State::is_idle` false.
                shared.settled.notify_all();
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
            // Taking the services and counting them is one step, so a waiter
            // never sees them in neither place.
            state.stopping += retiring.len();
            state.opening = generation.is_some();
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
                Ok(service) => {
                    state.stopping += 1;
                    stale_service = Some(service);
                }
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
            // Publishing, failing, and handing a stale service over to be
            // stopped all happen in this one critical section, so the store is
            // never reported idle between them.
            state.opening = false;
        }
        if let Some(service) = stale_service {
            stop_service(&shared, &service);
        }
    }
}

/// Stops a service the worker has counted in `State::stopping` and uncounts it
/// once its writer has released the store: `stop` returns only after joining
/// the writer thread, which owns the store lock, whether or not the stop
/// itself reported an error.
fn stop_service(shared: &Shared, service: &InputHistoryService) {
    #[cfg(test)]
    {
        let gate = shared
            .stop_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(gate) = gate {
            gate();
        }
    }
    if let Err(error) = service.stop() {
        if shared.verbose {
            eprintln!("sakura-engine: developer input history stop failed: {error}");
        }
    }
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.stopping = state.stopping.saturating_sub(1);
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::{HistoryRuntime, Phase, State};
    use crate::input_history::{clear_path, InputHistoryService};

    /// Blocks each call until the test releases it. It gates the opener and,
    /// through `hold_stops`, the worker's stop of a retiring service.
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

    /// A scratch directory for one test, removed when the guard drops, also
    /// when an assertion fails, which is when a cleanup written at the end of
    /// the test never runs. Declare it before the runtime or service that has
    /// the store open, so those drop first and release their files.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
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
            Self(directory)
        }

        /// The history store inside the directory.
        fn store(&self) -> PathBuf {
            self.0.join("input.bin")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_test_directory_goes_with_its_contents_when_the_guard_drops() {
        let path = {
            let dir = TestDir::new("guard-drop");
            std::fs::write(dir.store(), b"history").expect("store file");
            dir.0.clone()
        };
        assert!(!path.exists(), "{path:?} outlived its guard");
    }

    #[test]
    fn a_test_directory_goes_when_its_test_fails() {
        let path = std::cell::RefCell::new(None);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let dir = TestDir::new("guard-panic");
            std::fs::write(dir.store(), b"history").expect("store file");
            *path.borrow_mut() = Some(dir.0.clone());
            panic!("a failing assertion");
        }));
        assert!(outcome.is_err());
        let path = path.into_inner().expect("the directory was created");
        assert!(!path.exists(), "{path:?} outlived a failing test");
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

    /// Opens its gate for good when dropped, so a failing assertion cannot
    /// leave the worker blocked inside the gate while the runtime's own drop
    /// joins that worker.
    struct OpenOnDrop(Arc<OpenControl>);

    impl Drop for OpenOnDrop {
        fn drop(&mut self) {
            self.0.release(usize::MAX);
        }
    }

    /// Holds the worker inside `stop_service`, so a retiring writer keeps its
    /// store lock until the test releases `gate` or drops the returned guard.
    fn hold_stops(runtime: &HistoryRuntime, gate: &Arc<OpenControl>) -> OpenOnDrop {
        let held = Arc::clone(gate);
        *runtime.shared.stop_gate.lock().expect("stop gate") =
            Some(Arc::new(move || held.enter_and_wait()));
        OpenOnDrop(Arc::clone(gate))
    }

    /// Whether the lifecycle settles within `timeout`. The hold it takes is
    /// released at once, so asking never keeps the lifecycle from moving.
    fn settles(runtime: &HistoryRuntime, timeout: Duration) -> bool {
        runtime.hold_idle(timeout).is_some()
    }

    /// Waits as the engine does, and also requires the wake-up to follow soon
    /// after the lifecycle settles: a missed notification only costs the whole
    /// timeout, which the boolean alone would not show.
    fn assert_settles_promptly(runtime: &HistoryRuntime) {
        let started = Instant::now();
        assert!(settles(runtime, Duration::from_secs(5)));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the lifecycle settled but its waiter was not woken: {:?}",
            started.elapsed()
        );
    }

    fn quiet_state(phase: Phase) -> State {
        State {
            desired_enabled: false,
            generation: 0,
            phase,
            service: None,
            retiring: Vec::new(),
            stopping: 0,
            opening: false,
            worker_exited: false,
            holds: 0,
            shutdown: false,
        }
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
        let dir = TestDir::new("stalled");
        let runtime = controlled_runtime(dir.store(), Arc::clone(&control), opens);
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
        let dir = TestDir::new("disable-during-open");
        let runtime = controlled_runtime(dir.store(), Arc::clone(&control), opens);
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
        let dir = TestDir::new("toggles");
        let runtime = controlled_runtime(dir.store(), Arc::clone(&control), Arc::clone(&opens));
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
        let dir = TestDir::new("shutdown");
        let runtime = Arc::new(controlled_runtime(dir.store(), Arc::clone(&control), opens));
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

    #[test]
    fn idle_means_no_generation_is_changing_hands() {
        for phase in [
            Phase::Disabled,
            Phase::Active(1),
            Phase::Failed {
                generation: 1,
                error: "injected open failure".to_owned(),
            },
            Phase::Shutdown,
        ] {
            assert!(quiet_state(phase.clone()).is_idle(), "{phase:?}");
        }
        assert!(
            !quiet_state(Phase::Starting(1)).is_idle(),
            "a generation about to open owns the store before its opener starts"
        );

        let mut opening = quiet_state(Phase::Disabled);
        opening.opening = true;
        assert!(
            !opening.is_idle(),
            "a disabled generation's opener still owns the store"
        );

        let mut stopping = quiet_state(Phase::Disabled);
        stopping.stopping = 1;
        assert!(
            !stopping.is_idle(),
            "a stopping writer still owns the store lock"
        );

        let dir = TestDir::new("retiring-state");
        let mut retiring = quiet_state(Phase::Disabled);
        retiring
            .retiring
            .push(InputHistoryService::open(&dir.store()).expect("open history"));
        assert!(
            !retiring.is_idle(),
            "a service waiting to be stopped still owns the store lock"
        );
        drop(retiring);

        let mut exited = quiet_state(Phase::Starting(1));
        exited.opening = true;
        exited.stopping = 1;
        exited.worker_exited = true;
        assert!(
            exited.is_idle(),
            "nothing can change once the worker is gone"
        );
    }

    #[test]
    fn hold_idle_outlasts_a_retiring_writers_store_lock() {
        let dir = TestDir::new("retiring-writer");
        let path = dir.store();
        let service = InputHistoryService::open(&path).expect("open history");
        let runtime = HistoryRuntime::with_opener(Some(service), true, false, || {
            Err("no replacement generation".to_owned())
        })
        .expect("start test history lifecycle");
        let gate = Arc::new(OpenControl::default());
        let _stops = hold_stops(&runtime, &gate);

        runtime.configure(false);
        gate.wait_for_calls(1);
        assert!(
            runtime.service().is_none(),
            "requests stop seeing the service before its writer releases the store"
        );
        assert_eq!(
            clear_path(&path)
                .expect_err("the retiring writer owns the store lock")
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(!settles(&runtime, Duration::from_millis(50)));

        gate.release(usize::MAX);
        assert_settles_promptly(&runtime);
        clear_path(&path).expect("the store is free once the lifecycle is idle");
        runtime.stop();
    }

    #[test]
    fn hold_idle_covers_a_disabled_open_and_the_stop_of_its_stale_service() {
        let dir = TestDir::new("stale-open");
        let path = dir.store();
        let control = Arc::new(OpenControl::default());
        let runtime = controlled_runtime(
            path.clone(),
            Arc::clone(&control),
            Arc::new(AtomicUsize::new(0)),
        );
        let _opener = OpenOnDrop(Arc::clone(&control));
        let gate = Arc::new(OpenControl::default());
        let _stops = hold_stops(&runtime, &gate);
        control.wait_for_calls(1);

        runtime.configure(false);
        assert!(runtime.service().is_none());
        assert!(
            !settles(&runtime, Duration::from_millis(50)),
            "the disabled generation's opener still owns the store"
        );

        control.release(1);
        gate.wait_for_calls(1);
        assert_eq!(
            clear_path(&path)
                .expect_err("the stale service's writer owns the store lock")
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(!settles(&runtime, Duration::from_millis(50)));

        gate.release(usize::MAX);
        assert_settles_promptly(&runtime);
        clear_path(&path).expect("the store is free once the lifecycle is idle");
        runtime.stop();
    }

    #[test]
    fn hold_idle_returns_with_the_opened_service_already_published() {
        let dir = TestDir::new("publish-before-idle");
        let control = Arc::new(OpenControl::default());
        let runtime = controlled_runtime(
            dir.store(),
            Arc::clone(&control),
            Arc::new(AtomicUsize::new(0)),
        );
        let _opener = OpenOnDrop(Arc::clone(&control));
        control.wait_for_calls(1);
        assert!(
            !settles(&runtime, Duration::from_millis(50)),
            "a generation that is still opening is not idle"
        );

        control.release(1);
        assert_settles_promptly(&runtime);
        assert!(
            runtime.service().is_some(),
            "the lifecycle must not be idle before the opened service is visible"
        );
        runtime.stop();
    }

    #[test]
    fn hold_idle_settles_after_a_failed_open_and_after_shutdown() {
        let runtime = HistoryRuntime::with_opener(None, true, false, || {
            Err("injected open failure".to_owned())
        })
        .expect("start test history lifecycle");
        assert_settles_promptly(&runtime);
        assert!(matches!(
            runtime.phase(),
            Phase::Failed { generation: 1, .. }
        ));

        runtime.stop();
        assert!(settles(&runtime, Duration::ZERO));
    }

    #[test]
    fn a_panicked_worker_cannot_leave_the_lifecycle_busy() {
        let control = Arc::new(OpenControl::default());
        let gate = Arc::clone(&control);
        let runtime = HistoryRuntime::with_opener(None, true, false, move || {
            gate.enter_and_wait();
            panic!("injected opener panic")
        })
        .expect("start test history lifecycle");
        let _open = OpenOnDrop(Arc::clone(&control));
        control.wait_for_calls(1);

        // The waiter is parked before the worker dies, so only the exit guard
        // can wake it. A dead worker would otherwise keep every waiter busy
        // until its timeout, because nothing would ever clear what it left set.
        let (idle, waited) = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let started = Instant::now();
                let idle = settles(&runtime, Duration::from_secs(5));
                (idle, started.elapsed())
            });
            std::thread::sleep(Duration::from_millis(100));
            control.release(1);
            waiter.join().expect("waiter thread")
        });
        assert!(idle, "a dead worker has nothing in flight");
        assert!(
            waited < Duration::from_secs(3),
            "the worker died but its waiter was not woken: {waited:?}"
        );
        runtime.stop();
    }

    #[test]
    fn a_hold_defers_the_next_generation_until_every_hold_is_released() {
        let opens = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&opens);
        let runtime = HistoryRuntime::with_opener(None, false, false, move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Err("test opener".to_owned())
        })
        .expect("start test history lifecycle");

        let first = runtime
            .hold_idle(Duration::from_secs(5))
            .expect("a disabled lifecycle with nothing in flight is idle");
        let second = runtime
            .hold_idle(Duration::from_secs(5))
            .expect("holds are counted, not exclusive");
        // Developer mode goes on after the holds were taken.
        runtime.configure(true);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            opens.load(Ordering::SeqCst),
            0,
            "the next generation opened the store under a hold"
        );
        assert!(
            !settles(&runtime, Duration::ZERO),
            "an open that is waiting behind a hold is not idle"
        );

        drop(first);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            opens.load(Ordering::SeqCst),
            0,
            "releasing one hold must not free the other"
        );

        drop(second);
        assert_settles_promptly(&runtime);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        runtime.stop();
    }

    #[test]
    fn a_hold_keeps_a_disabled_service_running_until_it_is_released() {
        let dir = TestDir::new("held-service");
        let path = dir.store();
        let service = InputHistoryService::open(&path).expect("open history");
        let runtime = HistoryRuntime::with_opener(Some(Arc::clone(&service)), true, false, || {
            Err("no replacement generation".to_owned())
        })
        .expect("start test history lifecycle");

        let hold = runtime
            .hold_idle(Duration::from_secs(5))
            .expect("a published service with nothing in flight is idle");
        let held = hold.service().expect("the service published when idle");
        assert!(Arc::ptr_eq(&held, &service));

        // Developer mode goes off after the hold was taken.
        runtime.configure(false);
        assert!(
            runtime.service().is_none(),
            "requests stop seeing the service at once"
        );
        std::thread::sleep(Duration::from_millis(150));
        held.clear()
            .expect("the writer of a service retired under a hold is still running");
        assert_eq!(
            clear_path(&path)
                .expect_err("the held service's writer still owns the store lock")
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(
            !settles(&runtime, Duration::ZERO),
            "the retirement is waiting behind the hold"
        );

        drop(hold);
        assert_settles_promptly(&runtime);
        clear_path(&path).expect("the store is free once the hold is gone and the service retired");
        runtime.stop();
    }

    #[test]
    fn configure_and_service_never_wait_for_a_hold() {
        let runtime =
            HistoryRuntime::with_opener(None, false, false, || Err("test opener".to_owned()))
                .expect("start test history lifecycle");
        let (done, finished) = mpsc::channel();

        // `configure` runs under the configuration lock that every request
        // takes to read its snapshot, the holder's among them. The hold is
        // taken inside the scope so a failure releases it before the scope
        // joins the thread it spawned.
        std::thread::scope(|scope| {
            let hold = runtime
                .hold_idle(Duration::from_secs(5))
                .expect("a disabled lifecycle with nothing in flight is idle");
            scope.spawn(|| {
                runtime.configure(true);
                let _ = runtime.service();
                runtime.configure(false);
                done.send(()).expect("report that configure returned");
            });
            finished
                .recv_timeout(Duration::from_secs(5))
                .expect("configure waited for a hold");
            drop(hold);
        });
        assert_settles_promptly(&runtime);
        runtime.stop();
    }

    #[test]
    fn stop_waits_for_a_hold_and_then_completes() {
        let runtime =
            HistoryRuntime::with_opener(None, false, false, || Err("test opener".to_owned()))
                .expect("start test history lifecycle");
        let (done, stopped) = mpsc::channel();

        std::thread::scope(|scope| {
            let hold = runtime
                .hold_idle(Duration::from_secs(5))
                .expect("a disabled lifecycle with nothing in flight is idle");
            scope.spawn(|| {
                runtime.stop();
                done.send(()).expect("report that stop returned");
            });
            assert!(
                stopped.recv_timeout(Duration::from_millis(150)).is_err(),
                "stop returned while a hold was outstanding"
            );

            drop(hold);
            if stopped.recv_timeout(Duration::from_secs(5)).is_err() {
                // Wake the worker so a failing run reports instead of hanging
                // in the join that `stop` is stuck in.
                runtime.shared.changed.notify_all();
                panic!("stop did not finish after the hold was released");
            }
        });
    }
}
