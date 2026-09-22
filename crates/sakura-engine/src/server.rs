//! The accept loop: pipe instances in, dispatched replies out.
//!
//! # Threading
//!
//! One thread per pipe instance, each blocked in `ConnectNamedPipe` until a
//! host application arrives and then in `ReadFile` until it types. A thread
//! that picks up a client spawns a replacement acceptor if it was the last
//! free one, so there is normally always an instance waiting; the pool
//! grows to the high-water mark of simultaneously active applications and
//! stops at [`MAX_INSTANCES`].
//!
//! Nothing is shared between connections. A [`Dispatcher`] and its buffers
//! belong to one thread for that thread's whole life, so the keystroke path
//! takes no lock at all — not an uncontended one, none. That matters
//! because the budget is 50 ms end to end for a keystroke (DESIGN 10) and
//! because a lock held across a blocking pipe read is how one wedged host
//! application freezes typing in every other one.
//!
//! The cost of share-nothing is that cross-session state cannot live here
//! casually. Explicit process-wide components own their own lock discipline:
//! [`crate::ui`]'s board, input history, AI text capacity, and the
//! [`crate::composition_fence::CompositionFence`] that absorbs idle Space
//! while a peer connection of the same host process is converting.
//!
//! # Shutdown
//!
//! `Request::Shutdown` is answered first and acted on second, so the client
//! that asked gets its acknowledgement. The reply is flushed, the renderer
//! is told this was deliberate rather than a crash (see
//! [`crate::ui::UiBoard::stop`]), then the process exits — threads blocked
//! in `ConnectNamedPipe` cannot be polled awake, and inventing a wakeup
//! channel to unblock them would buy nothing:
//! there is nothing to persist in this phase, and when there is (the
//! learning store, DESIGN 4.3) it will be flushed on its own schedule
//! rather than at exit, precisely so that a crash loses no more than a
//! clean exit would.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use sakura_core::{default_app_profiles, AppProfile, AppearanceTheme, Preferences};
use sakura_proto::{
    encode_response, peek_header, EngineTimingSite, ErrorCode, FaultPoint, OutputBuf, Request,
    RequestId, Response, MAX_FRAME,
};
#[cfg(all(test, feature = "dev-fixtures"))]
use sakura_proto::{AiTextOperation, AiTextStatus, SessionId};

use sakura_ipc::debug_trace;
use sakura_ipc::{
    security, Accept, ClientTrust, Descriptor, Endpoint, Fault, PipeInstance, MAX_INSTANCES,
};

use crate::ai_text::AiTextService;
use crate::composition_fence::CompositionFence;
use crate::dictionary::ConversionService;
use crate::dispatch::{Dispatcher, Reply};
use crate::fault_injection;
use crate::history_runtime::HistoryRuntime;
use crate::input_history::InputHistoryService;
use crate::learning::{ForgetPredictionOutcome, LearningService};
use crate::long_conversion::{LongConversionRuntime, LongConversionService};
use crate::prediction::{PredictionRuntime, PredictionService};
use crate::timing;
use crate::ui::UiBoard;

#[derive(Debug, Clone)]
struct RuntimeConfiguration {
    preferences: Preferences,
    profiles: Arc<[AppProfile]>,
}

/// Optional workers are process-wide, but their policy is user-configurable.
/// The startup path may already own a runtime (the normal fast path); the
/// dynamic slots cover an engine that started with the feature disabled and is
/// later enabled from Settings. A worker is created at most once per enabled
/// configuration and is joined when the slot is dropped.
#[derive(Debug, Default)]
struct DynamicRuntimes {
    prediction: Option<PredictionRuntime>,
    long_conversion: Option<LongConversionRuntime>,
    prediction_failed: bool,
    long_conversion_failed: bool,
}

#[derive(Debug, Default)]
struct RuntimeServiceSnapshot {
    prediction: Option<Arc<PredictionService>>,
    long_conversion: Option<Arc<LongConversionService>>,
    input_history: Option<Arc<InputHistoryService>>,
}

/// Why the engine stopped waiting for work.
///
/// The two are not interchangeable to a watcher. A requested stop is
/// announced to the renderer (`UiState::stopping`), which is what stops its
/// watchdog from restarting an engine the user or the uninstaller just
/// stopped. An engine that ran out of instances must get the opposite
/// treatment: it is broken, a restart is the fix, and announcing a
/// deliberate stop would be the one thing that prevents one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopReason {
    /// A client sent [`Request::Shutdown`].
    Requested,
    /// The last pipe instance was released, so nothing is left to accept on
    /// and nothing can create a replacement.
    LastInstanceGone,
}

/// Per-process admission accounting for accepted pipe connections.
///
/// A single AppContainer process can otherwise open every data instance and
/// leave legitimate TSF hosts waiting. The PID comes from the kernel pipe
/// handle; it is never read from the protocol. Endpoint separation still
/// provides the stronger renderer/control boundary, while this quota bounds a
/// single host's data-plane footprint.
#[derive(Debug, Default)]
struct Admission {
    counts: Mutex<HashMap<(Endpoint, u32), u32>>,
}

const MAX_CONNECTIONS_PER_PID: u32 = 8;

struct AdmissionPermit {
    admission: Arc<Admission>,
    key: (Endpoint, u32),
}

impl Admission {
    fn try_acquire(
        self: &Arc<Self>,
        endpoint: Endpoint,
        process_id: u32,
    ) -> Option<AdmissionPermit> {
        let key = (endpoint, process_id);
        let mut counts = match self.counts.lock() {
            Ok(counts) => counts,
            Err(poisoned) => poisoned.into_inner(),
        };
        let count = counts.entry(key).or_default();
        if *count >= MAX_CONNECTIONS_PER_PID {
            return None;
        }
        *count += 1;
        Some(AdmissionPermit {
            admission: Arc::clone(self),
            key,
        })
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        let mut counts = match self.admission.counts.lock() {
            Ok(counts) => counts,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(count) = counts.get_mut(&self.key) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            counts.remove(&self.key);
        }
    }
}

/// Everything a worker thread needs that is not its own pipe instance.
#[derive(Debug)]
struct Shared {
    /// The data-plane name/descriptor are kept under their historic field
    /// names because test fixtures and diagnostics inspect them directly.
    name: String,
    sddl: String,
    renderer_name: String,
    renderer_sddl: String,
    control_name: String,
    control_sddl: String,
    /// Explicit `--test-pipe` fixtures intentionally collapse all roles onto
    /// one private object. Production always uses the three names above.
    test_pipe: bool,
    /// Instances that exist right now, capped at [`MAX_INSTANCES`].
    ///
    /// This is a live count, not a total: [`InstanceSlot`] gives the slot
    /// back when a worker ends, however it ends. It used to be incremented
    /// only, which made every worker that returned early — a failed accept,
    /// unusable engine data — a permanent loss of capacity, until
    /// [`ensure_spare_instance`] could no longer keep an acceptor waiting
    /// and arriving clients blocked in `CreateFileW` against a server that
    /// still looked healthy.
    created: AtomicU32,
    /// Acceptors currently blocked waiting for a client.
    idle: AtomicU32,
    renderer_created: AtomicU32,
    renderer_idle: AtomicU32,
    control_created: AtomicU32,
    control_idle: AtomicU32,
    total_created: AtomicU32,
    admission: Arc<Admission>,
    /// Ends [`Server::run`]. Sent when a client asks the engine to stop, and
    /// when the last instance is released — see [`StopReason`].
    shutdown: Sender<StopReason>,
    /// What the renderer draws. The one thing every connection shares.
    ui: UiBoard,
    /// Idle Space absorb while another connection of the same host is converting.
    composition_fence: Arc<CompositionFence>,
    /// Read-only dictionary plus the bounded process-wide conversion pool.
    conversion: Option<Arc<ConversionService>>,
    /// Process-wide synchronized personalization index and durable log.
    learning: Option<Arc<LearningService>>,
    /// Sole lifecycle owner for developer interaction history. Opening and
    /// stopping never execute on a request thread or under dynamic_runtimes.
    history_runtime: HistoryRuntime,
    /// One process-wide AI job owner. Its fixed capacity of one prevents
    /// separate TSF pipe connections from multiplying outbound requests.
    ai_text: Arc<AiTextService>,
    /// Request side of the one process-wide prediction worker.
    prediction: Option<Arc<PredictionService>>,
    /// Optional isolated ONNX reranker; its child process remains lazy.
    long_conversion: Option<Arc<LongConversionService>>,
    /// Owners for optional runtimes started after the process booted. The
    /// startup owners remain in `main` for their existing shutdown ordering.
    dynamic_runtimes: Mutex<DynamicRuntimes>,
    /// Last complete configuration snapshot accepted by the watcher. Workers
    /// copy this at connection/request boundaries; no lock is held in the
    /// dispatcher while it handles a keystroke.
    configuration: RwLock<RuntimeConfiguration>,
    verbose: bool,
}

impl Shared {
    fn configuration_snapshot(&self) -> RuntimeConfiguration {
        match self.configuration.read() {
            Ok(configuration) => configuration.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn publish_configuration(&self, preferences: Preferences, profiles: Vec<AppProfile>) {
        let configuration = RuntimeConfiguration {
            preferences,
            profiles: Arc::from(profiles),
        };
        match self.configuration.write() {
            Ok(mut current) => {
                self.history_runtime.configure(preferences.developer_mode);
                *current = configuration;
            }
            Err(poisoned) => {
                self.history_runtime.configure(preferences.developer_mode);
                *poisoned.into_inner() = configuration;
            }
        }
        // Appearance is carried in the same atomic snapshot, but the renderer
        // board still gets its narrow notification so an open popup repaints
        // without waiting for a key request.
        self.ui.set_appearance_theme_and_pad_shortcut(
            preferences.appearance_theme,
            preferences.pad_shortcut,
        );
    }

    fn runtime_services(
        &self,
        preferences: &Preferences,
        profiles: &[AppProfile],
    ) -> RuntimeServiceSnapshot {
        let prediction_requested = preferences.prediction_enabled
            || profiles.iter().any(|profile| profile.prediction_enabled);
        let long_requested =
            preferences.neural_reranker_scope != sakura_core::NeuralRerankerScope::Off;
        let history_requested = preferences.developer_mode;
        // Timed separately from the body below because they answer different
        // questions. Time spent here is time this request lost to whatever
        // another connection was doing while holding the process-wide mutex —
        // first-time prediction start-up, an input-history store being opened,
        // a reranker child process being spawned. Time spent after it is this
        // request's own. Only the first kind explains one connection stalling
        // because of another's work, so #148 cannot merge them.
        let lock_started = Instant::now();
        let mut dynamic = match self.dynamic_runtimes.lock() {
            Ok(dynamic) => dynamic,
            Err(poisoned) => poisoned.into_inner(),
        };
        timing::observe(
            EngineTimingSite::RuntimeServicesLockWait,
            lock_started.elapsed(),
        );

        if !prediction_requested {
            // Dropping the owner stops the worker; a dispatcher that still
            // holds the old Arc is detached below before its next request.
            dynamic.prediction = None;
            dynamic.prediction_failed = false;
        } else if self.prediction.is_none()
            && dynamic.prediction.is_none()
            && !dynamic.prediction_failed
        {
            if let (Some(conversion), Some(learning)) =
                (self.conversion.as_ref(), self.learning.as_ref())
            {
                match PredictionRuntime::start_with_learning(
                    Arc::clone(conversion),
                    Arc::clone(learning),
                ) {
                    Ok(runtime) => dynamic.prediction = Some(runtime),
                    Err(error) => {
                        dynamic.prediction_failed = true;
                        report(
                            self,
                            format_args!(
                                "prediction worker could not be enabled from settings: {error}"
                            ),
                        );
                    }
                }
            }
        }

        if !long_requested {
            dynamic.long_conversion = None;
            dynamic.long_conversion_failed = false;
        } else if self.long_conversion.is_none()
            && dynamic.long_conversion.is_none()
            && !dynamic.long_conversion_failed
        {
            if let Some(conversion) = self.conversion.as_ref() {
                match LongConversionRuntime::discover(Arc::clone(conversion)) {
                    Ok(Some(runtime)) => dynamic.long_conversion = Some(runtime),
                    Ok(None) => dynamic.long_conversion_failed = true,
                    Err(error) => {
                        dynamic.long_conversion_failed = true;
                        report(
                            self,
                            format_args!(
                                "long-conversion reranker could not be enabled from settings: {error}"
                            ),
                        );
                    }
                }
            }
        }

        RuntimeServiceSnapshot {
            prediction: if prediction_requested {
                self.prediction
                    .clone()
                    .or_else(|| dynamic.prediction.as_ref().map(PredictionRuntime::service))
            } else {
                None
            },
            long_conversion: if long_requested {
                self.long_conversion.clone().or_else(|| {
                    dynamic
                        .long_conversion
                        .as_ref()
                        .map(LongConversionRuntime::service)
                })
            } else {
                None
            },
            input_history: if history_requested {
                self.history_runtime.service()
            } else {
                None
            },
        }
    }
}

/// The engine's pipe server.
#[derive(Debug)]
pub struct Server {
    shared: Arc<Shared>,
    stopped: Receiver<StopReason>,
    reserved_instances: Option<Vec<(Endpoint, PipeInstance)>>,
}

impl Server {
    /// Resolves the pipe name and security descriptor for this logon
    /// session.
    ///
    /// `verbose` sends connection faults to stderr. The engine normally runs
    /// from a logon task with no console, where that goes nowhere and is
    /// meant to; it is for running the engine by hand.
    pub fn new(verbose: bool) -> windows::core::Result<Self> {
        let preferences = Preferences::default();
        Self::build(
            verbose,
            None,
            None,
            None,
            None,
            preferences,
            Arc::from(default_app_profiles(preferences)),
        )
    }

    /// Builds the production server with dictionary conversion enabled.
    pub fn with_conversion(
        verbose: bool,
        conversion: Arc<ConversionService>,
    ) -> windows::core::Result<Self> {
        let preferences = Preferences::default();
        Self::build(
            verbose,
            Some(conversion),
            None,
            None,
            None,
            preferences,
            Arc::from(default_app_profiles(preferences)),
        )
    }

    /// Builds the production server with dictionary conversion and a shared
    /// personalization store. Every pipe worker receives the same service.
    pub fn with_services(
        verbose: bool,
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
    ) -> windows::core::Result<Self> {
        let preferences = Preferences::default();
        Self::build(
            verbose,
            Some(conversion),
            Some(learning),
            None,
            None,
            preferences,
            Arc::from(default_app_profiles(preferences)),
        )
    }

    /// Builds the production server with all process-wide services and the
    /// validated user configuration captured at startup.
    pub fn with_configuration(
        verbose: bool,
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        preferences: Preferences,
    ) -> windows::core::Result<Self> {
        let profiles = Arc::from(default_app_profiles(preferences));
        Self::with_configuration_and_profiles(verbose, conversion, learning, preferences, profiles)
    }

    pub fn with_configuration_and_profiles(
        verbose: bool,
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        preferences: Preferences,
        profiles: Arc<[AppProfile]>,
    ) -> windows::core::Result<Self> {
        Self::build(
            verbose,
            Some(conversion),
            Some(learning),
            None,
            None,
            preferences,
            profiles,
        )
    }

    pub fn with_configuration_and_profiles_and_history(
        verbose: bool,
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        preferences: Preferences,
        profiles: Arc<[AppProfile]>,
        input_history: Arc<InputHistoryService>,
    ) -> windows::core::Result<Self> {
        Self::build(
            verbose,
            Some(conversion),
            Some(learning),
            None,
            Some(input_history),
            preferences,
            profiles,
        )
    }

    /// Builds the production server with the persistent prediction worker.
    pub fn with_runtime_configuration(
        verbose: bool,
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        prediction: Arc<PredictionService>,
        preferences: Preferences,
    ) -> windows::core::Result<Self> {
        let profiles = Arc::from(default_app_profiles(preferences));
        Self::with_runtime_configuration_and_profiles(
            verbose,
            conversion,
            learning,
            prediction,
            preferences,
            profiles,
        )
    }

    pub fn with_runtime_configuration_and_profiles(
        verbose: bool,
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        prediction: Arc<PredictionService>,
        preferences: Preferences,
        profiles: Arc<[AppProfile]>,
    ) -> windows::core::Result<Self> {
        Self::build(
            verbose,
            Some(conversion),
            Some(learning),
            Some(prediction),
            None,
            preferences,
            profiles,
        )
    }

    pub fn with_runtime_configuration_and_profiles_and_history(
        verbose: bool,
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        prediction: Arc<PredictionService>,
        preferences: Preferences,
        profiles: Arc<[AppProfile]>,
        input_history: Arc<InputHistoryService>,
    ) -> windows::core::Result<Self> {
        Self::build(
            verbose,
            Some(conversion),
            Some(learning),
            Some(prediction),
            Some(input_history),
            preferences,
            profiles,
        )
    }

    fn build(
        verbose: bool,
        conversion: Option<Arc<ConversionService>>,
        learning: Option<Arc<LearningService>>,
        prediction: Option<Arc<PredictionService>>,
        input_history: Option<Arc<InputHistoryService>>,
        preferences: Preferences,
        profiles: Arc<[AppProfile]>,
    ) -> windows::core::Result<Self> {
        let (shutdown, stopped) = mpsc::channel();
        let name = security::pipe_name()?;
        let sddl = security::sddl()?;
        let history_runtime =
            HistoryRuntime::new(input_history, preferences.developer_mode, verbose).map_err(
                |error| {
                    windows::core::Error::new(
                        windows::Win32::Foundation::E_FAIL,
                        format!("start developer input history lifecycle: {error}"),
                    )
                },
            )?;
        Ok(Server {
            shared: Arc::new(Shared {
                renderer_name: security::pipe_name_for(Endpoint::Renderer)?,
                renderer_sddl: security::sddl_for(Endpoint::Renderer)?,
                control_name: security::pipe_name_for(Endpoint::Control)?,
                control_sddl: security::sddl_for(Endpoint::Control)?,
                name,
                sddl,
                test_pipe: false,
                created: AtomicU32::new(0),
                idle: AtomicU32::new(0),
                renderer_created: AtomicU32::new(0),
                renderer_idle: AtomicU32::new(0),
                control_created: AtomicU32::new(0),
                control_idle: AtomicU32::new(0),
                total_created: AtomicU32::new(0),
                admission: Arc::new(Admission::default()),
                shutdown,
                ui: UiBoard::with_appearance_theme_and_pad_shortcut(
                    preferences.appearance_theme,
                    preferences.pad_shortcut,
                ),
                composition_fence: Arc::new(CompositionFence::new()),
                conversion,
                learning,
                history_runtime,
                ai_text: Arc::new(AiTextService::default()),
                prediction,
                long_conversion: None,
                dynamic_runtimes: Mutex::new(DynamicRuntimes::default()),
                configuration: RwLock::new(RuntimeConfiguration {
                    preferences,
                    profiles,
                }),
                verbose,
            }),
            stopped,
            reserved_instances: None,
        })
    }

    /// Reserves the existing secured endpoint names without starting workers.
    /// Dropping this value on any later initialization failure releases every
    /// claim. Ownership is not readiness: requests are served only by run.
    pub fn reserve(mut self) -> windows::core::Result<Self> {
        if self.reserved_instances.is_none() {
            self.reserved_instances = Some(create_initial_instances(&self.shared)?);
        }
        Ok(self)
    }

    /// Installs runtime dependencies after endpoint ownership is secured and
    /// before any callback or acceptor can observe the shared state.
    #[allow(clippy::too_many_arguments)]
    pub fn with_startup_services(
        mut self,
        conversion: Arc<ConversionService>,
        learning: Arc<LearningService>,
        prediction: Option<Arc<PredictionService>>,
        input_history: Option<Arc<InputHistoryService>>,
        preferences: Preferences,
        profiles: Arc<[AppProfile]>,
    ) -> windows::core::Result<Self> {
        let history_runtime = HistoryRuntime::new(
            input_history,
            preferences.developer_mode,
            self.shared.verbose,
        )
        .map_err(|error| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                format!("start developer input history lifecycle: {error}"),
            )
        })?;
        let shared = Arc::get_mut(&mut self.shared)
            .expect("startup services precede shared callbacks and workers");
        shared.conversion = Some(conversion);
        shared.learning = Some(learning);
        shared.prediction = prediction;
        shared.history_runtime = history_runtime;
        shared.ui = UiBoard::with_appearance_theme_and_pad_shortcut(
            preferences.appearance_theme,
            preferences.pad_shortcut,
        );
        shared.configuration = RwLock::new(RuntimeConfiguration {
            preferences,
            profiles,
        });
        Ok(self)
    }

    /// The pipe this server listens on.
    pub fn pipe_name(&self) -> &str {
        &self.shared.name
    }

    /// Returns the narrow theme-only callback kept for callers that already
    /// have an appearance edge. The complete configuration watcher should use
    /// [`Self::configuration_publisher`] so keymap and conversion policy are
    /// applied at the next input boundary as well.
    pub fn appearance_theme_publisher(&self) -> impl Fn(AppearanceTheme) + Send + 'static {
        let shared = Arc::clone(&self.shared);
        move |appearance_theme| {
            shared.ui.set_appearance_theme(appearance_theme);
        }
    }

    /// Callback used by the complete configuration watcher. The snapshot is
    /// replaced as one unit, then the renderer receives the appearance edge.
    pub fn configuration_publisher(
        &self,
    ) -> impl Fn(Preferences, Vec<AppProfile>) + Send + 'static {
        let shared = Arc::clone(&self.shared);
        move |preferences, profiles| shared.publish_configuration(preferences, profiles)
    }

    /// Replaces the name before [`run`](Self::run) creates any pipe instance.
    ///
    /// The binary invokes this only after validating its narrowly scoped test
    /// command-line option. Production constructors still resolve their normal
    /// name through [`security::pipe_name`] during construction.
    pub fn with_explicit_test_pipe(mut self, pipe_name: String) -> Self {
        assert!(
            self.reserved_instances.is_none(),
            "pipe name cannot change after reservation"
        );
        let shared = Arc::get_mut(&mut self.shared)
            .expect("a newly constructed server has no worker thread or shared clone");
        shared.name = pipe_name.clone();
        shared.renderer_name = pipe_name.clone();
        shared.control_name = pipe_name;
        shared.test_pipe = true;
        self
    }

    pub fn with_long_conversion(mut self, long_conversion: Arc<LongConversionService>) -> Self {
        Arc::get_mut(&mut self.shared)
            .expect("a newly constructed server has no worker thread or shared clone")
            .long_conversion = Some(long_conversion);
        self
    }

    /// Creates the first instance and blocks until the engine is asked to
    /// stop.
    ///
    /// The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`,
    /// so this fails rather than starting if the name is already taken —
    /// by a stale copy of the engine, or by something trying to collect
    /// our clients' keystrokes.
    pub fn run(self) -> windows::core::Result<()> {
        self.run_when_ready(|| {})
    }

    /// Calls `ready` only after all owned endpoint workers have started.
    pub fn run_when_ready(mut self, ready: impl FnOnce()) -> windows::core::Result<()> {
        // Each production endpoint owns its own named-pipe object and
        // admission pool. A data-plane flood therefore cannot starve the
        // renderer watchdog or the control channel used by the installer.
        let instances = match self.reserved_instances.take() {
            Some(instances) => instances,
            None => create_initial_instances(&self.shared)?,
        };
        spawn_initial_workers(&self.shared, instances)?;
        ready();
        // `recv` returns when a client asks for shutdown, or when the last
        // pipe instance is released — see [`InstanceSlot::drop`], which is
        // what makes the second case an explicit send. It cannot come from
        // the channel disconnecting: the sender lives in `shared`, and this
        // function holds `shared` for the whole wait, so a reason is the only
        // thing that ends it.
        let reason = self.stopped.recv().unwrap_or(StopReason::LastInstanceGone);
        if reason == StopReason::Requested {
            // Tell the renderer this was deliberate before the pipe breaks
            // under it, and hold the exit open just long enough for that to
            // reach the wire. Without it the renderer's watchdog sees only a
            // dead engine and restarts the one `--stop` just stopped — which
            // during an uninstall means relaunching the file being deleted.
            self.shared.ui.stop();
            self.shared.ui.settle(SHUTDOWN_GRACE);
        } else {
            // The opposite case, and the announcement above would be exactly
            // wrong for it: nobody asked for this exit, the engine is leaving
            // because it can no longer accept anything, and a restart is the
            // repair. Saying nothing lets the watchdog see a dead engine and
            // start a healthy one, which is the recovery it already has.
            report(
                &self.shared,
                format_args!("no pipe instances left; ending so a fresh engine can take over"),
            );
        }
        // The lifecycle thread may still be validating a large store. It owns
        // every history generation, so process shutdown joins it and any
        // writer it opened before returning control to `main`.
        self.shared.history_runtime.stop();
        Ok(())
    }
}

/// How long the engine holds its own exit open for watchers to collect the
/// shutdown announcement.
///
/// Long enough for a scheduler round trip and a pipe write on a loaded
/// machine, short enough that a renderer which has stopped reading cannot
/// make an uninstall look hung. It is a ceiling, not a delay: with nothing
/// outstanding — the usual case, since the renderer is normally the only
/// watcher and there is often none — the wait ends immediately.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

/// A client must identify the protocol endpoint within this bound. The
/// timeout is enforced before any request is dispatched, so a connection that
/// only occupies a pipe instance cannot hold admission indefinitely.
const FIRST_HANDSHAKE_BUDGET: Duration = Duration::from_millis(750);

/// Renderer/control are not host-count pools. They need only a small number
/// of independent clients and deliberately have caps separate from the data
/// endpoint's 64 host connections.
const RENDERER_MAX_INSTANCES: u32 = 4;
const CONTROL_MAX_INSTANCES: u32 = 4;

/// How much stack each pipe-instance thread reserves.
///
/// A thread parked in `ReadFile` needs almost no stack, and the engine's
/// whole budget is 15 MB (DESIGN 10); the default 1 MB reservation per thread
/// would dominate it at the [`MAX_INSTANCES`] cap.
///
/// That reasoning is only sound while [`worker`]'s locals stay small, and
/// once they did not: `SessionTable` used to hold its sixty-four sessions
/// inline, making a `Dispatcher` ~109 KB, and the engine died at startup with
/// "thread 'sakura-pipe' has overflowed its stack" before it accepted a single
/// connection. The table is boxed now, and
/// `worker_locals_fit_the_reserved_stack` is what keeps the next large local
/// from rediscovering this at runtime.
///
/// 160 KiB reserves 10 MiB at the [`MAX_INSTANCES`] cap, leaving the rest of
/// the 15 MiB engine budget for the process and its bounded auxiliary workers.
const WORKER_STACK_BYTES: usize = 160 * 1024;

/// One instance's claim on the [`MAX_INSTANCES`] cap.
///
/// The claim is released in `Drop` rather than at each exit, because
/// [`worker`] has several of those and the next one added would otherwise
/// silently leak a slot. A leaked slot has no symptom until the cap is
/// reached, at which point the engine stops accepting new clients while
/// every existing connection keeps working — the hardest kind of failure to
/// attribute after the fact.
struct InstanceSlot {
    shared: Arc<Shared>,
    endpoint: Endpoint,
}

impl InstanceSlot {
    /// Claims a slot. Paired with the instance the caller just created, so
    /// the count and the live instances move together.
    #[cfg(all(test, feature = "dev-fixtures"))]
    fn claim(shared: &Arc<Shared>) -> Self {
        Self::claim_for(shared, Endpoint::Data)
    }

    fn claim_for(shared: &Arc<Shared>, endpoint: Endpoint) -> Self {
        endpoint_created(shared, endpoint).fetch_add(1, Ordering::Relaxed);
        shared.total_created.fetch_add(1, Ordering::Relaxed);
        InstanceSlot {
            shared: Arc::clone(shared),
            endpoint,
        }
    }
}

impl Drop for InstanceSlot {
    /// Releases the slot, and ends the engine when it was the last one.
    ///
    /// Nothing can bring an instance back once the count reaches zero:
    /// [`ensure_spare_instance`] runs only inside a worker, after an accept
    /// has succeeded, so with no worker left there is no caller. A process
    /// in that state accepts nothing while looking perfectly healthy — a
    /// client blocks in `CreateFileW`, and the renderer's watchdog sees a
    /// live engine and leaves it alone.
    ///
    /// Ending the process instead hands recovery to that watchdog, which
    /// already restarts an engine that is gone and is tested for it. It also
    /// avoids the failure a retry here would have: the exits that reach this
    /// point are `engine data is unusable`, a wedged instance, and an OS
    /// accept fault, and re-creating an instance for the first two produces
    /// a worker that fails the same way immediately — a spawn loop at 100%
    /// of a core rather than a restart.
    ///
    /// This lives in `Drop` rather than at [`worker`]'s exits so that a
    /// worker which panics reaches it too: unwinding drops the slot, and a
    /// panicked acceptor leaves exactly the same hole as a returned one.
    fn drop(&mut self) {
        // `fetch_sub` returns the value from before this release, so `1`
        // means this was the last instance. `AcqRel` pairs the releases so
        // exactly one dropped slot can observe that.
        endpoint_created(&self.shared, self.endpoint).fetch_sub(1, Ordering::AcqRel);
        if self.shared.total_created.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.shared.shutdown.send(StopReason::LastInstanceGone);
        }
    }
}

fn endpoint_name(shared: &Shared, endpoint: Endpoint) -> &str {
    match endpoint {
        Endpoint::Data => &shared.name,
        Endpoint::Renderer => &shared.renderer_name,
        Endpoint::Control => &shared.control_name,
    }
}

fn endpoint_sddl(shared: &Shared, endpoint: Endpoint) -> &str {
    match endpoint {
        Endpoint::Data => &shared.sddl,
        Endpoint::Renderer => &shared.renderer_sddl,
        Endpoint::Control => &shared.control_sddl,
    }
}

fn endpoint_created(shared: &Shared, endpoint: Endpoint) -> &AtomicU32 {
    match endpoint {
        Endpoint::Data => &shared.created,
        Endpoint::Renderer => &shared.renderer_created,
        Endpoint::Control => &shared.control_created,
    }
}

fn endpoint_idle(shared: &Shared, endpoint: Endpoint) -> &AtomicU32 {
    match endpoint {
        Endpoint::Data => &shared.idle,
        Endpoint::Renderer => &shared.renderer_idle,
        Endpoint::Control => &shared.control_idle,
    }
}

const fn endpoint_capacity(endpoint: Endpoint) -> u32 {
    match endpoint {
        Endpoint::Data => MAX_INSTANCES,
        Endpoint::Renderer => RENDERER_MAX_INSTANCES,
        Endpoint::Control => CONTROL_MAX_INSTANCES,
    }
}

/// Creates one pipe instance and the thread that serves it.
fn spawn_worker(
    shared: &Arc<Shared>,
    endpoint: Endpoint,
    first: bool,
) -> windows::core::Result<()> {
    let instance = create_instance(shared, endpoint, first)?;
    spawn_worker_with_instance(shared, endpoint, instance, None).map(|_| ())
}

fn create_instance(
    shared: &Arc<Shared>,
    endpoint: Endpoint,
    first: bool,
) -> windows::core::Result<PipeInstance> {
    let descriptor = Descriptor::from_sddl(endpoint_sddl(shared, endpoint))?;
    PipeInstance::create_with_capacity(
        endpoint_name(shared, endpoint),
        &descriptor,
        first,
        endpoint_capacity(endpoint),
    )
}

/// Creates all required first instances before starting any worker thread. This
/// prevents a partial startup from leaving a data acceptor alive when the
/// renderer/control security descriptor or pipe creation fails. Workers also
/// wait behind a startup gate until all three thread spawns have succeeded;
/// if a later spawn fails, the gate aborts and the already-created threads are
/// joined before the error escapes. That makes startup a transaction rather
/// than relying on process exit to clean up detached acceptors.
fn create_initial_instances(
    shared: &Arc<Shared>,
) -> windows::core::Result<Vec<(Endpoint, PipeInstance)>> {
    let mut instances = Vec::with_capacity(3);
    let endpoints: &[Endpoint] = if shared.test_pipe {
        &[Endpoint::Data]
    } else {
        &[Endpoint::Data, Endpoint::Renderer, Endpoint::Control]
    };
    for &endpoint in endpoints {
        instances.push((endpoint, create_instance(shared, endpoint, true)?));
    }
    Ok(instances)
}

fn spawn_initial_workers(
    shared: &Arc<Shared>,
    mut instances: Vec<(Endpoint, PipeInstance)>,
) -> windows::core::Result<()> {
    let gate = Arc::new(StartupGate::new());
    let mut workers = Vec::with_capacity(instances.len());
    while let Some((endpoint, instance)) = instances.pop() {
        match spawn_worker_with_instance(shared, endpoint, instance, Some(Arc::clone(&gate))) {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                // Workers that did start are still parked before their first
                // accept. Abort wakes them, and joining them proves that no
                // pipe handle or slot survives this failed transaction.
                gate.abort();
                drop(instances);
                for worker in workers {
                    let _ = worker.join();
                }
                return Err(error);
            }
        }
    }

    // All handles are intentionally detached only after the gate opens. The
    // workers now own their instances for the remainder of the server run.
    gate.open();
    drop(workers);
    Ok(())
}

fn spawn_worker_with_instance(
    shared: &Arc<Shared>,
    endpoint: Endpoint,
    instance: PipeInstance,
    startup_gate: Option<Arc<StartupGate>>,
) -> windows::core::Result<std::thread::JoinHandle<()>> {
    // Claimed before the thread exists so the cap can never be exceeded by
    // a spawn that is still in flight; released by `Drop` on either path
    // below — the failed spawn here, or the worker ending later.
    let slot = InstanceSlot::claim_for(shared, endpoint);
    let owned = Arc::clone(shared);
    let job = move || worker_with_gate(owned, instance, slot, endpoint, startup_gate);
    #[cfg(all(test, feature = "dev-fixtures"))]
    if tests::FAIL_PIPE_SPAWN_AFTER.with(|remaining| match remaining.get() {
        Some(0) => {
            remaining.set(None);
            true
        }
        Some(count) => {
            remaining.set(Some(count - 1));
            false
        }
        None => false,
    }) {
        // Match Builder::spawn failure ownership: the unstarted closure is
        // dropped, releasing its instance and slot before the error escapes.
        drop(job);
        return Err(thread_failure(&std::io::Error::other(
            "injected pipe spawn failure",
        )));
    }
    let spawned = std::thread::Builder::new()
        .name("sakura-pipe".to_owned())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(job);

    match spawned {
        Ok(worker) => Ok(worker),
        // The instance has no thread to accept on it, so it must not count
        // towards the cap. Both it and the slot are already released: the
        // closure that owned them was dropped when the spawn failed.
        Err(error) => Err(thread_failure(&error)),
    }
}

/// Coordinates the all-or-nothing production endpoint startup. A worker that
/// has been spawned but not yet admitted to this gate owns its slot and pipe
/// handle, so aborting and joining it is sufficient cleanup on every failure
/// path.
struct StartupGate {
    state: Mutex<StartupState>,
    wake: Condvar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupState {
    Pending,
    Open,
    Aborted,
}

impl StartupGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(StartupState::Pending),
            wake: Condvar::new(),
        }
    }

    fn open(&self) {
        self.set(StartupState::Open);
    }

    fn abort(&self) {
        self.set(StartupState::Aborted);
    }

    fn wait(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *state == StartupState::Pending {
            state = self
                .wake
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *state == StartupState::Open
    }

    fn set(&self, next: StartupState) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = next;
        self.wake.notify_all();
    }
}

/// The OS reason a thread could not start, in the engine's error currency.
/// `E_FAIL` only stands in for the case where the failure carried no OS
/// code at all — losing the real one would turn "out of memory" into an
/// unexplained startup failure at logon, which is where this is hardest to
/// investigate.
fn thread_failure(error: &std::io::Error) -> windows::core::Error {
    use windows::core::HRESULT;
    use windows::Win32::Foundation::E_FAIL;

    match error.raw_os_error() {
        Some(code) => windows::core::Error::from_hresult(HRESULT::from_win32(code as u32)),
        None => windows::core::Error::from_hresult(E_FAIL),
    }
}

/// How many accepts in a row may end with no client on the other end
/// before the instance is treated as unusable.
///
/// A client that connects and exits immediately is ordinary and must cost
/// nothing but one wasted accept. An instance that reports it without ever
/// producing a client is not ordinary, and retrying it forever would be a
/// spin at 100% of a core — the failure mode that makes an IME feel like a
/// hardware fault. The count resets on every served connection, so a busy
/// engine cannot reach it by accumulating unrelated races.
const MAX_CONSECUTIVE_EMPTY_ACCEPTS: u32 = 16;

/// Whether an accept that produced no client should end the worker.
///
/// The count is of *consecutive* empty accepts and is reset by every served
/// connection, so this asks "is this instance producing clients at all",
/// not "has this engine seen many client races".
const fn empty_accept_is_fatal(consecutive: u32) -> bool {
    consecutive >= MAX_CONSECUTIVE_EMPTY_ACCEPTS
}

/// One instance's whole life: accept, serve, disconnect, repeat.
///
/// `slot` is this instance's claim on the [`MAX_INSTANCES`] cap. It is
/// owned here and nowhere else, so every way out of this function — a
/// return below, a panic, the loop ending — gives the slot back.
#[cfg(all(test, feature = "dev-fixtures"))]
fn worker(shared: Arc<Shared>, instance: PipeInstance, slot: InstanceSlot, endpoint: Endpoint) {
    worker_with_gate(shared, instance, slot, endpoint, None);
}

fn worker_with_gate(
    shared: Arc<Shared>,
    instance: PipeInstance,
    slot: InstanceSlot,
    endpoint: Endpoint,
    startup_gate: Option<Arc<StartupGate>>,
) {
    let _slot = slot;
    if let Some(gate) = startup_gate {
        if !gate.wait() {
            return;
        }
    }
    let configuration = shared.configuration_snapshot();
    let dispatcher = match (
        shared.conversion.as_ref(),
        shared.learning.as_ref(),
        shared.prediction.as_ref(),
    ) {
        (Some(conversion), Some(learning), Some(prediction)) => {
            Dispatcher::new_with_runtime_configuration_and_profiles(
                Arc::clone(conversion),
                Arc::clone(learning),
                Arc::clone(prediction),
                configuration.preferences,
                Arc::clone(&configuration.profiles),
            )
        }
        (Some(conversion), Some(learning), None) => {
            Dispatcher::new_with_configuration_and_profiles(
                Arc::clone(conversion),
                Arc::clone(learning),
                configuration.preferences,
                Arc::clone(&configuration.profiles),
            )
        }
        (Some(conversion), None, _) => Dispatcher::new_with_conversion(Arc::clone(conversion)),
        (None, _, _) => Dispatcher::new(),
    };
    let mut dispatcher = match dispatcher {
        Ok(dispatcher) => dispatcher,
        Err(error) => {
            report(&shared, format_args!("engine data is unusable: {error}"));
            return;
        }
    };
    {
        let configuration = shared.configuration_snapshot();
        let runtime_services =
            shared.runtime_services(&configuration.preferences, &configuration.profiles);
        dispatcher.set_input_history(runtime_services.input_history);
    }
    dispatcher.set_ai_text(Arc::clone(&shared.ai_text));
    dispatcher.set_composition_fence(Arc::clone(&shared.composition_fence));
    if let Some(long_conversion) = shared.long_conversion.as_ref() {
        dispatcher.set_long_conversion(Some(Arc::clone(long_conversion)));
    }
    let mut connection = Buffers::new();

    let mut empty_accepts = 0u32;
    loop {
        endpoint_idle(&shared, endpoint).fetch_add(1, Ordering::Relaxed);
        let accepted = instance.wait_for_client();
        endpoint_idle(&shared, endpoint).fetch_sub(1, Ordering::Relaxed);
        match accepted {
            Ok(Accept::Connected) => empty_accepts = 0,
            // The client was gone before it could be served. The instance
            // is still healthy, but it is holding a connection to nobody
            // and `ConnectNamedPipe` would report the same thing again
            // until it is released — so disconnect first, then accept
            // again. Serving nothing here would read from a dead peer and
            // reach the same place by a longer route.
            Ok(Accept::ClientGone) => {
                empty_accepts += 1;
                if empty_accept_is_fatal(empty_accepts) {
                    report(
                        &shared,
                        format_args!("accept produced no client {empty_accepts} times in a row"),
                    );
                    return;
                }
                instance.disconnect();
                continue;
            }
            Err(error) => {
                report(&shared, format_args!("accept failed: {error}"));
                return;
            }
        }

        let process_id = match instance.client_process_id() {
            Ok(process_id) => process_id,
            Err(error) => {
                report(
                    &shared,
                    format_args!("could not identify the connected client: {error}"),
                );
                instance.disconnect();
                continue;
            }
        };
        let Some(_permit) = shared.admission.try_acquire(endpoint, process_id) else {
            report(
                &shared,
                format_args!(
                    "client pid {process_id} exceeded the {MAX_CONNECTIONS_PER_PID}-connection {} quota",
                    endpoint_name(&shared, endpoint)
                ),
            );
            instance.disconnect();
            continue;
        };

        // The PID is kernel-reported from this accepted handle. A malformed
        // or inaccessible token is deliberately `Unknown`: ordinary data
        // requests remain available for a degraded host, while AI requests
        // are denied by the request matrix below.
        let client_trust = match security::classify_client_process(process_id) {
            Ok(trust) => trust,
            Err(error) => {
                report(
                    &shared,
                    format_args!(
                        "could not classify client pid {process_id}; sensitive requests are denied: {error}"
                    ),
                );
                ClientTrust::Unknown
            }
        };

        ensure_spare_instance_for(&shared, endpoint);

        let outcome = serve(
            &shared,
            &instance,
            endpoint,
            client_trust,
            &mut dispatcher,
            &mut connection,
        );
        // A disconnected/crashed host cannot send Revert or DeleteSession.
        // Finalize its published UI before this worker accepts another client;
        // the renderer's separate, healthy watch pipe may still be displaying it.
        shared.ui.clear_connection(dispatcher.ui_owner());
        match outcome {
            Outcome::Closed => {}
            Outcome::Failed(fault) => report(&shared, format_args!("{fault}")),
            Outcome::Shutdown => {
                instance.disconnect();
                // Sent before this worker returns, so it is the reason `run`
                // sees: the slot this worker gives up on the way out sends
                // one of its own when it happens to be the last instance,
                // and a requested stop must not be mistaken for a failure.
                let _ = shared.shutdown.send(StopReason::Requested);
                return;
            }
        }
        instance.disconnect();
        // A new client gets a session table that starts empty. Sessions
        // belong to the connection that created them: the host process that
        // owned them is gone, and a session id from a dead connection must
        // never resolve against a live one.
        dispatcher.reset();
    }
}

/// Keeps one acceptor waiting, so an arriving client rarely has to queue.
///
/// The counters are read without synchronization and can disagree with
/// reality for a moment. The failure that costs is spawning one instance
/// too many or too few, and neither loses a connection: a client that finds
/// no free instance blocks in `CreateFileW` until one frees, which is what
/// a named pipe does by design.
#[cfg(all(test, feature = "dev-fixtures"))]
fn ensure_spare_instance(shared: &Arc<Shared>) {
    ensure_spare_instance_for(shared, Endpoint::Data);
}

fn ensure_spare_instance_for(shared: &Arc<Shared>, endpoint: Endpoint) {
    if endpoint_idle(shared, endpoint).load(Ordering::Relaxed) > 0 {
        return;
    }
    if endpoint_created(shared, endpoint).load(Ordering::Relaxed) >= endpoint_capacity(endpoint) {
        return;
    }
    if let Err(error) = spawn_worker(shared, endpoint, false) {
        report(shared, format_args!("could not add an instance: {error}"));
    }
}

/// Why one connection ended.
enum Outcome {
    /// The client went away. The ordinary case.
    Closed,
    Failed(Fault),
    /// The client asked the engine to stop.
    Shutdown,
}

/// Per-connection buffers, allocated once and reused for every frame.
struct Buffers {
    /// The payload just read. Grows to what this client actually sends.
    read: Vec<u8>,
    /// Scratch for the allocation-free `OutputBuf::encode_frame` path.
    frame: Vec<u8>,
    /// Scratch for the responses that are not `Output`; these are rare and
    /// small, so `encode_response`'s owned form is fine for them.
    reply: Vec<u8>,
    out: Box<OutputBuf>,
}

impl Buffers {
    fn new() -> Self {
        Buffers {
            read: Vec::new(),
            // Sized for the largest frame the protocol allows, once, so the
            // keystroke path never resizes it. `OutputBuf::encode_frame`
            // writes into a slice and needs the space to already exist.
            frame: vec![0; MAX_FRAME],
            reply: Vec::new(),
            // Candidate buffers are intentionally large and bounded. Keep
            // them off the 160 KiB pipe-worker stack; this allocation happens
            // once per worker, never on a keystroke.
            out: OutputBuf::new_boxed(),
        }
    }
}

/// Reads and validates the first frame before a connection can reach the
/// ordinary request loop. Production clients must begin with `Hello`; a
/// private test pipe retains the historical ability to send a request first
/// because a number of fixture cleanup paths intentionally do so.
fn first_request(
    shared: &Shared,
    instance: &PipeInstance,
    endpoint: Endpoint,
    dispatcher: &mut Dispatcher,
    bufs: &mut Buffers,
) -> Result<Option<(RequestId, Request)>, Outcome> {
    let payload = match read_first_frame_with_deadline(instance) {
        Ok(payload) => payload,
        Err(Fault::Disconnected) => return Err(Outcome::Closed),
        Err(fault) => return Err(Outcome::Failed(fault)),
    };
    let (id, request) = match sakura_proto::decode_request(&payload) {
        Ok(decoded) => decoded,
        Err(error) => {
            let code = match error {
                sakura_proto::Error::UnsupportedVersion(_) => ErrorCode::UnsupportedVersion,
                sakura_proto::Error::TooLarge => ErrorCode::TooLarge,
                _ => ErrorCode::Malformed,
            };
            if let Ok(header) = peek_header(&payload) {
                let _ = send(
                    instance,
                    &Response::Error(code),
                    header.request_id,
                    &mut bufs.reply,
                );
            }
            return Err(Outcome::Failed(Fault::Protocol(error)));
        }
    };

    if !matches!(request, Request::Hello { .. }) {
        if shared.test_pipe {
            // Explicitly private test fixtures may exercise the server with a
            // single request and omit protocol negotiation.
            return Ok(Some((id, request)));
        }
        let _ = send(
            instance,
            &Response::Error(ErrorCode::Malformed),
            id,
            &mut bufs.reply,
        );
        return Err(Outcome::Failed(Fault::Protocol(
            sakura_proto::Error::BadEnum,
        )));
    }

    let reply = dispatcher.dispatch(&request, &mut bufs.out);
    let response = match reply {
        Reply::Message(response @ Response::Hello { .. }) => response,
        Reply::Message(response) => {
            // A version-mismatched Hello is answered once so the peer can
            // diagnose the mixed install, but it is not a successful
            // admission. Do not let that connection continue into the
            // ordinary request loop without a negotiated protocol.
            let _ = send(instance, &response, id, &mut bufs.reply);
            return Err(Outcome::Failed(Fault::Protocol(
                sakura_proto::Error::BadEnum,
            )));
        }
        // `Hello` is a pure negotiation request. Any other branch would mean
        // the dispatcher contract changed without updating this boundary.
        Reply::Output | Reply::Shutdown(_) => {
            let _ = send(
                instance,
                &Response::Error(ErrorCode::Internal),
                id,
                &mut bufs.reply,
            );
            return Err(Outcome::Failed(Fault::Protocol(
                sakura_proto::Error::BadEnum,
            )));
        }
    };
    if let Err(fault) = send(instance, &response, id, &mut bufs.reply) {
        return Err(end(fault));
    }

    // The endpoint is checked again here so this invariant remains explicit
    // at the handshake boundary even if a future dispatcher starts handling a
    // second handshake message itself.
    debug_assert!(matches!(
        endpoint,
        Endpoint::Data | Endpoint::Renderer | Endpoint::Control
    ));
    Ok(None)
}

/// Performs one bounded read for the initial frame. The named pipe is
/// intentionally byte-mode and the steady-state path uses blocking reads;
/// the transport's byte-availability polling is used only here so a client
/// that connects and then goes idle cannot hold an acceptor indefinitely.
fn read_first_frame_with_deadline(instance: &PipeInstance) -> Result<Vec<u8>, Fault> {
    let mut buffer = Vec::new();
    instance
        .read_frame_with_deadline(&mut buffer, FIRST_HANDSHAKE_BUDGET)
        .map(|payload| payload.to_vec())
}

/// Server-owned request matrix. This is deliberately exhaustive over the
/// protocol enum so a newly added request cannot silently inherit access to a
/// privileged endpoint.
fn request_allowed(endpoint: Endpoint, request: &Request, client_trust: ClientTrust) -> bool {
    let endpoint_allowed = match endpoint {
        Endpoint::Data => matches!(
            request,
            Request::CreateSession { .. }
                | Request::SendKey { .. }
                | Request::ProbeKey { .. }
                | Request::Commit { .. }
                | Request::Revert { .. }
                | Request::ResetDocumentContext { .. }
                | Request::UndoCommit { .. }
                | Request::Reconvert { .. }
                | Request::SetInputScope { .. }
                | Request::SetMode { .. }
                | Request::ApplyAiComposition { .. }
                | Request::RecordAiText { .. }
                | Request::StartAiText { .. }
                | Request::PollAiText { .. }
                | Request::CancelAiText { .. }
                | Request::PollCandidateCommit { .. }
                | Request::CommitCandidate { .. }
                | Request::DeleteSession { .. }
                | Request::SetUiPlacement { .. }
                | Request::Ping
        ),
        Endpoint::Renderer => matches!(
            request,
            Request::WatchUi { .. }
                | Request::DeleteHistoryCandidate { .. }
                | Request::QueueCandidateCommit { .. }
        ),
        Endpoint::Control => matches!(
            request,
            Request::ClearLearning
                | Request::ClearInputHistory
                | Request::FlushInputHistory
                | Request::InputHistoryStats
                | Request::EngineTiming
                | Request::FaultStatus
                | Request::Shutdown
                | Request::Ping
        ),
    };
    endpoint_allowed && (!is_ai_request(request) || client_trust == ClientTrust::MediumOrHigher)
}

fn is_ai_request(request: &Request) -> bool {
    matches!(
        request,
        Request::StartAiText { .. }
            | Request::PollAiText { .. }
            | Request::CancelAiText { .. }
            | Request::ApplyAiComposition { .. }
            | Request::RecordAiText { .. }
    )
}

/// Serves one connected client until it disconnects or misbehaves.
fn serve(
    shared: &Shared,
    instance: &PipeInstance,
    endpoint: Endpoint,
    client_trust: ClientTrust,
    dispatcher: &mut Dispatcher,
    bufs: &mut Buffers,
) -> Outcome {
    let connection_probe = instance.connection_probe();
    dispatcher.set_connection_probe(connection_probe.clone());
    let mut initial = match first_request(shared, instance, endpoint, dispatcher, bufs) {
        Ok(initial) => initial,
        Err(outcome) => return outcome,
    };
    loop {
        let (id, request) = if let Some(initial) = initial.take() {
            initial
        } else {
            let payload = match instance.read_frame(&mut bufs.read) {
                Ok(payload) => payload,
                Err(Fault::Disconnected) => return Outcome::Closed,
                Err(fault) => return Outcome::Failed(fault),
            };

            match sakura_proto::decode_request(payload) {
                Ok(decoded) => decoded,
                Err(error) => {
                    let code = match error {
                        sakura_proto::Error::UnsupportedVersion(_) => ErrorCode::UnsupportedVersion,
                        sakura_proto::Error::TooLarge => ErrorCode::TooLarge,
                        _ => ErrorCode::Malformed,
                    };
                    // `peek_header` reads the id without validating the
                    // version or the body, so a frame the decoder rejected
                    // can still be answered by id. A client waiting on a
                    // request needs to be told which one failed.
                    if let Ok(header) = peek_header(payload) {
                        let _ = send(
                            instance,
                            &Response::Error(code),
                            header.request_id,
                            &mut bufs.reply,
                        );
                    }
                    // A byte stream that has lost frame alignment cannot be
                    // resynchronized safely.
                    return Outcome::Failed(Fault::Protocol(error));
                }
            }
        };

        // Closing a client does not remove its unread request bytes. Reject
        // that queue before any session, history, or shared UI side effect.
        if connection_probe
            .as_ref()
            .is_some_and(sakura_ipc::ConnectionProbe::is_disconnected)
        {
            return Outcome::Closed;
        }

        // The pipe name selected by the server is the authority for this
        // allowlist. No client-supplied role or Hello field can widen it.
        // Test fixtures use one private pipe for all protocol roles and are
        // deliberately outside the production boundary.
        if !shared.test_pipe && !request_allowed(endpoint, &request, client_trust) {
            let _ = send(
                instance,
                &Response::Error(ErrorCode::Malformed),
                id,
                &mut bufs.reply,
            );
            return Outcome::Failed(Fault::Protocol(sakura_proto::Error::BadEnum));
        }

        // `WatchUi` never reaches the dispatcher. It is answered from state
        // shared by every connection, and it *blocks* — two things the
        // dispatcher's module docs rule out on purpose (no clocks, nothing
        // shared), and both of which belong to this layer.
        if let Request::WatchUi { since } = request {
            // `delivery` is held across the write and dropped straight
            // after, which is what lets a shutdown wait for the farewell to
            // reach the wire instead of racing the process teardown.
            let (state, delivery) = shared.ui.wait_past(since);
            let written = send(instance, &Response::Ui(state), id, &mut bufs.reply);
            drop(delivery);
            if let Err(fault) = written {
                return end(fault);
            }
            continue;
        }

        // Layout callbacks arrive independently of keystrokes and update
        // only the shared renderer snapshot. The session id prevents a late
        // callback from an old focus owner moving a newer popup.
        if let Request::SetUiPlacement {
            session,
            anchor,
            document,
            renderer_visible,
        } = request
        {
            let _ = shared.ui.publish_placement_from(
                dispatcher.ui_owner(),
                session,
                anchor,
                document,
                renderer_visible,
            );
            if let Err(fault) = send(instance, &Response::Ok, id, &mut bufs.reply) {
                return end(fault);
            }
            continue;
        }

        // The renderer is a separate pipe client and therefore has no
        // dispatcher-owned editing session. It can only ask the shared board
        // to resolve a row from the exact revision it displayed; the board
        // supplies the private learning identity after validating it. Do not
        // route this through a fresh Dispatcher, which would have neither the
        // source candidate nor the selected session cache needed to make the
        // operation safe.
        if let Request::DeleteHistoryCandidate {
            revision,
            candidate_index,
        } = request
        {
            let removed = delete_history_candidate(shared, revision, candidate_index);
            if let Err(fault) = send(
                instance,
                &Response::HistoryCandidateDeleted { removed },
                id,
                &mut bufs.reply,
            ) {
                return end(fault);
            }
            continue;
        }

        if let Request::QueueCandidateCommit {
            revision,
            candidate_index,
        } = request
        {
            let queued = shared.ui.queue_candidate_commit(revision, candidate_index);
            if let Err(fault) = send(
                instance,
                &Response::CandidateCommitQueued { queued },
                id,
                &mut bufs.reply,
            ) {
                return end(fault);
            }
            continue;
        }

        if let Request::PollCandidateCommit { session } = request {
            let pending = shared
                .ui
                .pending_candidate_commit(dispatcher.ui_owner(), session);
            if let Err(fault) = send(
                instance,
                &Response::CandidateCommitPending { request: pending },
                id,
                &mut bufs.reply,
            ) {
                return end(fault);
            }
            continue;
        }

        // From here to the end of this iteration is the engine's service time
        // for one request, and it is what a client's expired budget has to be
        // compared against. It starts below the branches above rather than at
        // the decoded frame because `WatchUi` blocks on purpose until the UI
        // moves; folding a long poll into this total would bury every key
        // request under it.
        let _request_span = timing::Span::start(EngineTimingSite::RequestTotal);

        let consumed_candidate_commit = if let Request::CommitCandidate {
            session,
            revision,
            candidate_index,
        } = &request
        {
            if !shared.ui.take_candidate_commit(
                dispatcher.ui_owner(),
                *session,
                *revision,
                *candidate_index,
            ) {
                if let Err(fault) = send(
                    instance,
                    &Response::Error(ErrorCode::Malformed),
                    id,
                    &mut bufs.reply,
                ) {
                    return end(fault);
                }
                continue;
            }
            Some((*session, *revision))
        } else {
            None
        };

        let output_session = match &request {
            Request::SendKey { session, key } if !key.test_only => Some(*session),
            Request::Commit { session } => Some(*session),
            Request::Reconvert {
                session,
                preview: false,
                ..
            } => Some(*session),
            Request::ApplyAiComposition { session, .. } => Some(*session),
            Request::CommitCandidate { session, .. } => Some(*session),
            _ => None,
        };
        let clears_candidates = match &request {
            Request::Revert { session } | Request::DeleteSession { session } => Some(*session),
            Request::SetInputScope {
                session,
                scope:
                    sakura_proto::InputScope::Password
                    | sakura_proto::InputScope::Url
                    | sakura_proto::InputScope::Email
                    | sakura_proto::InputScope::Digits,
            } => Some(*session),
            _ => None,
        };

        let configuration = {
            let _span = timing::Span::start(EngineTimingSite::ConfigurationSnapshot);
            shared.configuration_snapshot()
        };
        let runtime_services = {
            let _span = timing::Span::start(EngineTimingSite::RuntimeServicesTotal);
            shared.runtime_services(&configuration.preferences, &configuration.profiles)
        };
        dispatcher.set_prediction(runtime_services.prediction);
        dispatcher.set_long_conversion(runtime_services.long_conversion);
        dispatcher.set_input_history(runtime_services.input_history);
        if let Err(error) = dispatcher
            .apply_runtime_configuration(configuration.preferences, configuration.profiles)
        {
            report(
                shared,
                format_args!("configuration update rejected: {error}"),
            );
        }

        // Configuration/runtime locks may have delayed this request since
        // the first check. Do not dispatch a client that closed while waiting.
        if connection_probe
            .as_ref()
            .is_some_and(sakura_ipc::ConnectionProbe::is_disconnected)
        {
            return Outcome::Closed;
        }
        // Nothing has been touched yet: the frame is decoded, no session has
        // moved and no reply exists. A key delayed here is the mildest of the
        // four stories, and the one a correct client should always survive.
        fault_injection::delay(FaultPoint::BeforeDispatch);
        let reply = {
            let _span = timing::Span::start(EngineTimingSite::Dispatch);
            dispatcher.dispatch(&request, &mut bufs.out)
        };
        match reply {
            Reply::Output => {
                #[cfg(all(test, feature = "dev-fixtures"))]
                tests::BEFORE_OUTPUT.with(|hook| {
                    if let Some(hook) = hook.borrow_mut().take() {
                        hook();
                    }
                });
                // The diagnostic helper has a non-trivial call frame. Keep it
                // entirely off the ordinary 160 KiB worker-stack path.
                if debug_trace::is_enabled() {
                    // Timed because it is a file write on the reply path, and
                    // it is enabled by exactly the developer mode used to
                    // collect the reports #148 is working from. If this cost
                    // is material then those reports measured the instrument
                    // as well as the engine, and the two configurations have
                    // to be compared rather than pooled.
                    let _span = timing::Span::start(EngineTimingSite::DebugTraceEmit);
                    trace_key_result(&request, &bufs.out);
                }
                let encoded = {
                    let _span = timing::Span::start(EngineTimingSite::Encoding);
                    bufs.out.encode_frame(id, &mut bufs.frame)
                };
                let written = match encoded {
                    Ok(written) => written,
                    Err(error) => {
                        // The engine built something that will not fit on
                        // the wire. That is our bug, not the client's, so
                        // it gets a diagnosable answer rather than silence.
                        let _ = send(
                            instance,
                            &Response::Error(ErrorCode::Internal),
                            id,
                            &mut bufs.reply,
                        );
                        return Outcome::Failed(Fault::Protocol(error));
                    }
                };
                // The answer exists and is correct; only its delivery is late.
                // A client that gives up here has abandoned a key the engine
                // has already applied.
                fault_injection::delay(FaultPoint::DuringReply);
                let transported = {
                    let _span = timing::Span::start(EngineTimingSite::ReplyWrite);
                    instance.write_all(&bufs.frame[..written])
                };
                if let Err(fault) = transported {
                    return end(fault);
                }
                // An abandoned client's output must not replace another
                // connection's newer UI when its response cannot be sent.
                // Successful transport is not a host document-apply receipt.
                if let Some(session) = output_session {
                    let learning_generation = shared
                        .learning
                        .as_ref()
                        .map_or(0, |learning| learning.generation());
                    shared.ui.publish_output_from(
                        dispatcher.ui_owner(),
                        session,
                        &bufs.out,
                        learning_generation,
                    );
                } else if let Some(mode) = bufs.out.mode {
                    shared.ui.publish(mode);
                }
            }
            Reply::Message(response) => {
                if let Some((session, revision)) = consumed_candidate_commit {
                    shared
                        .ui
                        .reject_candidate_commit(dispatcher.ui_owner(), session, revision);
                }
                // The TSF input-mode menu changes an idle session without a
                // document-edit `Output`. Publish its exact mode separately
                // so the renderer's transient indicator remains in sync with
                // the language-bar item that initiated the change.
                if let Response::InputMode { mode } = &response {
                    shared.ui.publish(*mode);
                }
                if matches!(response, Response::Ok) {
                    if let Some(session) = clears_candidates {
                        shared.ui.clear_session_from(dispatcher.ui_owner(), session);
                    }
                }
                if let Err(fault) = send(instance, &response, id, &mut bufs.reply) {
                    return end(fault);
                }
            }
            Reply::Shutdown(response) => {
                // Answered before acting on it, so the client that asked is
                // not left waiting on a process that has already gone.
                let _ = send(instance, &response, id, &mut bufs.reply);
                return Outcome::Shutdown;
            }
        }
    }
}

fn trace_key_result(request: &Request, output: &OutputBuf) {
    let Request::SendKey { session, key } = request else {
        return;
    };
    if !debug_trace::is_enabled() {
        return;
    }
    let kind = output
        .candidate_kind()
        .map(|kind| kind as u64)
        .unwrap_or(255);
    let count = output.candidate_count() as u64;
    let identity_top = output
        .candidate(0)
        .is_some_and(|(text, _)| text == output.preedit_text()) as u64;
    let decision = if key.test_only {
        "probe"
    } else if output.commit_text().is_some() {
        "commit"
    } else {
        "apply"
    };
    debug_trace::emit(sakura_ipc::debug_trace::TraceEvent {
        component: "engine",
        instance: *session,
        event: "key_result",
        decision,
        k0: key.code as u64,
        k1: kind,
        k2: count,
        k3: identity_top,
    });
}

/// Resolves and removes one renderer-selected history prediction. The first
/// step is a revision/index capability check in [`UiBoard`]; persistence is
/// the commit point. Only after it succeeds does the advanced learning
/// generation invalidate cached prediction UI. Every failed path is a
/// terminal no-op, including duplicate clicks after the first removal.
fn delete_history_candidate(shared: &Shared, revision: u64, candidate_index: u16) -> bool {
    let Some((reading, surface)) = shared
        .ui
        .history_candidate_identity(revision, candidate_index)
    else {
        return false;
    };
    let Some(learning) = shared.learning.as_deref() else {
        return false;
    };
    match learning.forget_prediction_exact(&reading, &surface) {
        Ok(ForgetPredictionOutcome::Removed) => {
            // Every Dispatcher observes this process-wide generation before
            // serving its next non-probe request. Hide the old shared snapshot
            // now; it was built before the durable removal and must not remain
            // clickable while a worker refreshes its bounded cache.
            shared
                .ui
                .invalidate_stale_prediction_candidates(learning.generation());
            true
        }
        Ok(ForgetPredictionOutcome::NotFound | ForgetPredictionOutcome::Unavailable) | Err(_) => {
            false
        }
    }
}

fn end(fault: Fault) -> Outcome {
    match fault {
        Fault::Disconnected => Outcome::Closed,
        other => Outcome::Failed(other),
    }
}

fn send(
    instance: &PipeInstance,
    response: &Response,
    id: RequestId,
    scratch: &mut Vec<u8>,
) -> Result<(), Fault> {
    // A reply this side cannot encode is `Fault::Encode`, not
    // `Fault::Protocol`: the client sent nothing malformed here, this
    // process failed to serialize its own answer. `end()` still drops the
    // connection for it (see `Fault::Encode`'s doc comment) — the client is
    // left waiting for a reply to a request it already sent, and there is
    // no way to answer that on this connection.
    encode_response(response, id, scratch).map_err(Fault::Encode)?;
    instance.write_all(scratch)
}

fn report(shared: &Shared, args: core::fmt::Arguments<'_>) {
    if shared.verbose {
        eprintln!("sakura-engine: {args}");
    }
}

#[cfg(all(test, feature = "dev-fixtures"))]
#[path = "server_tests.rs"]
mod tests;
