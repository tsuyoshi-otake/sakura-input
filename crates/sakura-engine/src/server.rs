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
use std::time::Duration;

use sakura_core::{default_app_profiles, AppProfile, AppearanceTheme, Preferences};
use sakura_proto::{
    encode_response, peek_header, ErrorCode, OutputBuf, Request, RequestId, Response, MAX_FRAME,
};
#[cfg(test)]
use sakura_proto::{AiTextOperation, AiTextStatus, SessionId};

use sakura_ipc::debug_trace;
use sakura_ipc::{
    security, Accept, ClientTrust, Descriptor, Endpoint, Fault, PipeInstance, MAX_INSTANCES,
};

use crate::ai_text::AiTextService;
use crate::composition_fence::CompositionFence;
use crate::dictionary::ConversionService;
use crate::dispatch::{Dispatcher, Reply};
use crate::input_history::InputHistoryService;
use crate::learning::{ForgetPredictionOutcome, LearningService};
use crate::long_conversion::{LongConversionRuntime, LongConversionService};
use crate::prediction::{PredictionRuntime, PredictionService};
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
    input_history: Option<Arc<InputHistoryService>>,
    prediction_failed: bool,
    long_conversion_failed: bool,
    input_history_failed: bool,
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
    /// Explicitly enabled developer interaction history.
    input_history: Option<Arc<InputHistoryService>>,
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
            Ok(mut current) => *current = configuration,
            Err(poisoned) => *poisoned.into_inner() = configuration,
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
        let mut dynamic = match self.dynamic_runtimes.lock() {
            Ok(dynamic) => dynamic,
            Err(poisoned) => poisoned.into_inner(),
        };

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

        if !history_requested {
            // Dropping the dynamic owner stops its writer. A cold-start
            // Shared owner is left in place for shutdown ordering; the
            // dispatcher still receives None below so new keys are not
            // recorded while developer-mode is off.
            dynamic.input_history = None;
            dynamic.input_history_failed = false;
        } else if self.input_history.is_none()
            && dynamic.input_history.is_none()
            && !dynamic.input_history_failed
        {
            match crate::input_history::default_path()
                .and_then(|path| InputHistoryService::open(&path))
            {
                Ok(service) => {
                    sakura_ipc::debug_trace::set_enabled(true);
                    dynamic.input_history = Some(service);
                }
                Err(error) => {
                    dynamic.input_history_failed = true;
                    report(
                        self,
                        format_args!(
                            "developer input history could not be enabled from settings: {error}"
                        ),
                    );
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
                self.input_history
                    .clone()
                    .or_else(|| dynamic.input_history.clone())
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
                input_history,
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
    ) -> Self {
        let shared = Arc::get_mut(&mut self.shared)
            .expect("startup services precede shared callbacks and workers");
        shared.conversion = Some(conversion);
        shared.learning = Some(learning);
        shared.prediction = prediction;
        shared.input_history = input_history;
        shared.ui = UiBoard::with_appearance_theme_and_pad_shortcut(
            preferences.appearance_theme,
            preferences.pad_shortcut,
        );
        shared.configuration = RwLock::new(RuntimeConfiguration {
            preferences,
            profiles,
        });
        self
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
    #[cfg(test)]
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
    #[cfg(test)]
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
#[cfg(test)]
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

        match serve(
            &shared,
            &instance,
            endpoint,
            client_trust,
            &mut dispatcher,
            &mut connection,
        ) {
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
#[cfg(test)]
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

        let configuration = shared.configuration_snapshot();
        let runtime_services =
            shared.runtime_services(&configuration.preferences, &configuration.profiles);
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

        match dispatcher.dispatch(&request, &mut bufs.out) {
            Reply::Output => {
                // The diagnostic helper has a non-trivial call frame. Keep it
                // entirely off the ordinary 160 KiB worker-stack path.
                if debug_trace::is_enabled() {
                    trace_key_result(&request, &bufs.out);
                }
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
                let written = match bufs.out.encode_frame(id, &mut bufs.frame) {
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
                if let Err(fault) = instance.write_all(&bufs.frame[..written]) {
                    return end(fault);
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

#[cfg(test)]
mod tests {
    thread_local! {
        pub(super) static FAIL_PIPE_SPAWN_AFTER: std::cell::Cell<Option<usize>> =
            const { std::cell::Cell::new(None) };
    }
    use super::*;
    use sakura_ipc::Client;
    use std::fs;
    use std::sync::mpsc::TryRecvError;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn test_shared(learning: Arc<LearningService>) -> Shared {
        let (shutdown, _stopped) = mpsc::channel();
        Shared {
            name: "sakura-engine-history-delete-test".to_owned(),
            sddl: String::new(),
            renderer_name: String::new(),
            renderer_sddl: String::new(),
            control_name: String::new(),
            control_sddl: String::new(),
            test_pipe: true,
            created: AtomicU32::new(0),
            idle: AtomicU32::new(0),
            renderer_created: AtomicU32::new(0),
            renderer_idle: AtomicU32::new(0),
            control_created: AtomicU32::new(0),
            control_idle: AtomicU32::new(0),
            total_created: AtomicU32::new(0),
            admission: Arc::new(Admission::default()),
            shutdown,
            ui: UiBoard::new(),
            composition_fence: Arc::new(CompositionFence::new()),
            conversion: None,
            learning: Some(learning),
            input_history: None,
            ai_text: Arc::new(AiTextService::default()),
            prediction: None,
            long_conversion: None,
            dynamic_runtimes: Mutex::new(DynamicRuntimes::default()),
            configuration: RwLock::new(RuntimeConfiguration {
                preferences: Preferences::default(),
                profiles: Arc::from([]),
            }),
            verbose: false,
        }
    }

    #[test]
    fn configured_dark_appearance_reaches_the_ui_state() {
        let preferences = Preferences {
            appearance_theme: sakura_core::AppearanceTheme::Dark,
            ..Preferences::default()
        };
        let server = Server::build(false, None, None, None, None, preferences, Arc::from([]))
            .expect("server");

        assert_eq!(
            look(&server.shared.ui, 0).appearance_theme,
            sakura_core::AppearanceTheme::Dark
        );
    }

    #[test]
    fn configuration_publisher_replaces_input_snapshot_and_repaints_theme() {
        let server = Server::build(
            false,
            None,
            None,
            None,
            None,
            Preferences::default(),
            Arc::from([]),
        )
        .expect("server");
        let publish = server.configuration_publisher();
        let preferences = Preferences {
            appearance_theme: sakura_core::AppearanceTheme::Dark,
            pad_shortcut: sakura_core::PadShortcut::DoubleCtrl,
            association_enabled: false,
            prediction_enabled: false,
            ..Preferences::default()
        };
        publish(preferences, Vec::new());

        let snapshot = server.shared.configuration_snapshot();
        assert_eq!(snapshot.preferences, preferences);
        assert!(snapshot.profiles.is_empty());
        assert_eq!(
            look(&server.shared.ui, 0).appearance_theme,
            preferences.appearance_theme
        );
        assert_eq!(
            look(&server.shared.ui, 0).pad_shortcut,
            preferences.pad_shortcut
        );
    }

    fn prediction_conversion_fixture() -> Arc<ConversionService> {
        let entries = dictc::parse_entries(
            "server-prediction.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t0\t100\t100\tit\tIT用語\n",
        )
        .expect("prediction entries");
        let matrix = dictc::parse_connection(
            "server-prediction-matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("prediction matrix");
        let image = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("prediction dictionary")
                .into_boxed_slice(),
        );
        Arc::new(
            ConversionService::from_static_bytes(image).expect("prediction conversion service"),
        )
    }

    #[test]
    fn optional_prediction_worker_follows_a_live_configuration_change() {
        let conversion = prediction_conversion_fixture();
        let learning = Arc::new(LearningService::memory());
        let server = Server::with_configuration_and_profiles(
            false,
            conversion,
            Arc::clone(&learning),
            Preferences::default(),
            Arc::from([]),
        )
        .expect("server");

        let enabled = Preferences {
            prediction_enabled: true,
            ..Preferences::default()
        };
        let services = server.shared.runtime_services(&enabled, &[]);
        assert!(
            services.prediction.is_some(),
            "enabling prediction must start the optional worker on demand"
        );

        let disabled = Preferences {
            prediction_enabled: false,
            ..Preferences::default()
        };
        let services = server.shared.runtime_services(&disabled, &[]);
        assert!(
            services.prediction.is_none(),
            "disabling prediction must detach the service before the next key"
        );
        let dynamic = server
            .shared
            .dynamic_runtimes
            .lock()
            .expect("dynamic runtime lock");
        assert!(
            dynamic.prediction.is_none(),
            "disabled worker must be joined"
        );
    }

    #[test]
    fn optional_input_history_follows_a_live_developer_mode_change() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "sakura_input_history_hot_{}_{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(root.join("SakuraInput").join("history")).expect("history dir");
        let previous = std::env::var_os("LOCALAPPDATA");
        std::env::set_var("LOCALAPPDATA", &root);

        let learning = Arc::new(LearningService::memory());
        let server = Server::with_configuration_and_profiles(
            false,
            prediction_conversion_fixture(),
            learning,
            Preferences::default(),
            Arc::from([]),
        )
        .expect("server");

        let enabled = Preferences {
            developer_mode: true,
            ..Preferences::default()
        };
        let services = server.shared.runtime_services(&enabled, &[]);
        assert!(
            services.input_history.is_some(),
            "enabling developer-mode must open history without an engine restart"
        );

        let disabled = Preferences {
            developer_mode: false,
            ..Preferences::default()
        };
        let services = server.shared.runtime_services(&disabled, &[]);
        assert!(
            services.input_history.is_none(),
            "disabling developer-mode must detach history at the next boundary"
        );
        let dynamic = server
            .shared
            .dynamic_runtimes
            .lock()
            .expect("dynamic runtime lock");
        assert!(
            dynamic.input_history.is_none(),
            "disabled dynamic history owner must be dropped"
        );

        match previous {
            Some(value) => std::env::set_var("LOCALAPPDATA", value),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    fn test_learning_path() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after the Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "sakura_input_history_delete_{}_{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&directory).expect("test learning directory");
        directory.join("learning.bin")
    }

    fn history_suggestion_output() -> OutputBuf {
        let mut output = OutputBuf::new();
        output.begin_suggestions(0, 9).expect("suggestion list");
        output
            .push_history_candidate("表示名", "履歴", "よみ", "永続化された表記")
            .expect("history candidate");
        output
    }

    fn look(board: &UiBoard, since: u64) -> sakura_proto::UiState {
        let (state, delivery) = board.wait_past(since);
        drop(delivery);
        state
    }

    fn history_contains(learning: &LearningService, reading: &str, surface: &str) -> bool {
        let mut found = false;
        learning.visit_prediction_history(reading, |candidate_reading, candidate_surface, _, _| {
            found |= candidate_reading == reading && candidate_surface == surface;
            true
        });
        found
    }

    #[test]
    fn history_delete_is_snapshot_bound_durable_and_idempotent() {
        let path = test_learning_path();
        let learning = Arc::new(LearningService::open(&path).expect("open learning"));
        learning.learn("よみ", "永続化された表記", 1, 2);
        let published_generation = learning.generation();
        let shared = test_shared(Arc::clone(&learning));
        let output = history_suggestion_output();
        shared.ui.publish_output(9, &output, published_generation);
        let published = look(&shared.ui, 0);

        assert!(history_contains(&learning, "よみ", "永続化された表記"));
        assert!(delete_history_candidate(&shared, published.revision, 0));
        assert!(
            !history_contains(&learning, "よみ", "永続化された表記"),
            "the UI must not be invalidated until this durable store no longer contains the row"
        );
        let invalidated = look(&shared.ui, published.revision);
        assert!(invalidated.candidates.is_none());
        assert!(
            !delete_history_candidate(&shared, published.revision, 0),
            "a duplicate click must fail closed after the first durable removal"
        );

        drop(shared);
        drop(learning);
        let reopened = LearningService::open(&path).expect("reopen durable learning store");
        assert!(
            !history_contains(&reopened, "よみ", "永続化された表記"),
            "the deletion must survive reopening the durable learning store"
        );
        drop(reopened);
        fs::remove_dir_all(path.parent().expect("test directory")).expect("remove test directory");
    }

    #[test]
    fn history_delete_rejects_stale_and_annotation_only_candidates() {
        let path = test_learning_path();
        let learning = Arc::new(LearningService::open(&path).expect("open learning"));
        learning.learn("よみ", "永続化された表記", 1, 2);
        let shared = test_shared(Arc::clone(&learning));

        let history = history_suggestion_output();
        shared.ui.publish_output(9, &history, learning.generation());
        let stale = look(&shared.ui, 0);

        let mut annotation_only = OutputBuf::new();
        annotation_only
            .begin_suggestions(0, 9)
            .expect("suggestion list");
        annotation_only
            .push_candidate("同じ見た目でも履歴ではない", "履歴")
            .expect("ordinary candidate");
        shared
            .ui
            .publish_output(9, &annotation_only, learning.generation());
        let current = look(&shared.ui, stale.revision);

        assert!(
            !delete_history_candidate(&shared, stale.revision, 0),
            "an old revision must never target the replacement row"
        );
        assert!(
            !delete_history_candidate(&shared, current.revision, 0),
            "a display annotation is not a deletion capability"
        );
        assert!(history_contains(&learning, "よみ", "永続化された表記"));

        drop(shared);
        drop(learning);
        fs::remove_dir_all(path.parent().expect("test directory")).expect("remove test directory");
    }

    /// The two values [`worker`] builds on its own stack have to fit in the
    /// stack it was given, with room left for the frames underneath them.
    ///
    /// This is the test that would have caught the startup crash described on
    /// [`WORKER_STACK_BYTES`]: a `Dispatcher` grew to ~109 KB while the
    /// reservation stayed at 128 KB, and nothing said so until the first
    /// worker thread died. A quarter of the reservation is the ceiling
    /// because these are not the only frames on that stack — `worker` calls
    /// into `Dispatcher::dispatch` and the pipe I/O below it, and
    /// `Dispatcher::new` returns its value through a temporary before the
    /// local even exists.
    #[test]
    fn worker_locals_fit_the_reserved_stack() {
        let dispatcher = core::mem::size_of::<crate::dispatch::Dispatcher>();
        let buffers = core::mem::size_of::<Buffers>();
        let budget = WORKER_STACK_BYTES / 4;
        assert!(
            dispatcher + buffers < budget,
            "worker's locals need {dispatcher} + {buffers} bytes of the \
             {WORKER_STACK_BYTES}-byte pipe-thread stack; keep them under \
             {budget} by boxing whatever just grew, not by raising the \
             reservation (that multiplies by {MAX_INSTANCES} threads)"
        );
    }

    /// `Buffers::new` runs on the pipe worker's 160 KiB stack.  Keep this
    /// runtime guard in addition to the size budget above: `Box::new(T::new())`
    /// first constructs all of `T` on that stack, which can overflow even
    /// when `Buffers` itself contains only a pointer to `T`.
    #[test]
    fn worker_buffers_are_initialized_directly_on_the_reserved_stack() {
        std::thread::Builder::new()
            .name("sakura-engine-buffer-stack-test".to_owned())
            .stack_size(WORKER_STACK_BYTES)
            .spawn(|| {
                let mut buffers = Buffers::new();
                buffers.out.consumed = true;
                buffers.out.clear();
                assert!(!buffers.out.consumed);
            })
            .expect("create bounded-stack worker")
            .join()
            .expect("bounded-stack worker must not overflow while constructing Buffers");
    }

    #[test]
    fn the_server_resolves_a_pipe_name_for_this_session() {
        let server = Server::new(false).expect("this process has a token");
        assert!(server.pipe_name().starts_with(r"\\.\pipe\sakura_input_"));
    }

    #[test]
    fn explicit_test_pipe_does_not_change_production_name_resolution() {
        let private_pipe = r"\\.\pipe\SakuraInputEngineTest-server-unit".to_owned();
        let private = Server::new(false)
            .expect("this process has a production token")
            .with_explicit_test_pipe(private_pipe.clone());
        assert_eq!(private.pipe_name(), private_pipe);

        let production = Server::new(false).expect("this process has a production token");
        assert!(production
            .pipe_name()
            .starts_with(r"\\.\pipe\sakura_input_"));
    }

    #[test]
    fn startup_gate_aborts_waiting_workers_and_opens_only_after_commit() {
        let gate = Arc::new(StartupGate::new());
        let waiting = Arc::clone(&gate);
        let worker = std::thread::spawn(move || waiting.wait());

        // A pending gate cannot let a worker enter the accept loop. The
        // abort path must wake it so startup failures have a terminal state.
        gate.abort();
        assert!(!worker.join().expect("startup worker joined"));

        let gate = StartupGate::new();
        gate.abort();
        assert!(!gate.wait(), "an aborted gate must not become open");
    }

    #[test]
    fn startup_gate_opens_waiting_workers() {
        let gate = Arc::new(StartupGate::new());
        let waiting = Arc::clone(&gate);
        let worker = std::thread::spawn(move || waiting.wait());
        gate.open();
        assert!(worker.join().expect("startup worker joined"));
    }

    #[test]
    fn endpoint_request_allowlist_keeps_privileged_operations_off_data() {
        let data_only = Request::CreateSession {
            process_name: "tsf.exe".to_owned(),
        };
        let renderer_only = Request::WatchUi { since: 0 };
        let control_only = Request::Shutdown;
        let history_delete = Request::DeleteHistoryCandidate {
            revision: 1,
            candidate_index: 0,
        };

        assert!(request_allowed(
            Endpoint::Data,
            &data_only,
            ClientTrust::MediumOrHigher
        ));
        assert!(!request_allowed(
            Endpoint::Data,
            &renderer_only,
            ClientTrust::MediumOrHigher
        ));
        assert!(!request_allowed(
            Endpoint::Data,
            &control_only,
            ClientTrust::MediumOrHigher
        ));
        assert!(!request_allowed(
            Endpoint::Data,
            &history_delete,
            ClientTrust::MediumOrHigher
        ));

        assert!(request_allowed(
            Endpoint::Renderer,
            &renderer_only,
            ClientTrust::MediumOrHigher
        ));
        assert!(!request_allowed(
            Endpoint::Renderer,
            &data_only,
            ClientTrust::MediumOrHigher
        ));
        assert!(!request_allowed(
            Endpoint::Renderer,
            &control_only,
            ClientTrust::MediumOrHigher
        ));

        assert!(request_allowed(
            Endpoint::Control,
            &control_only,
            ClientTrust::MediumOrHigher
        ));
        assert!(!request_allowed(
            Endpoint::Control,
            &data_only,
            ClientTrust::MediumOrHigher
        ));
        assert!(!request_allowed(
            Endpoint::Control,
            &renderer_only,
            ClientTrust::MediumOrHigher
        ));
    }

    #[test]
    fn low_and_sandbox_clients_cannot_use_ai_requests() {
        let requests = [
            Request::StartAiText {
                session: 1 as SessionId,
                operation: AiTextOperation::Transform,
                text: "hello".to_owned(),
            },
            Request::PollAiText {
                session: 1 as SessionId,
                job: 1,
            },
            Request::CancelAiText {
                session: 1 as SessionId,
                job: 1,
            },
            Request::ApplyAiComposition {
                session: 1 as SessionId,
                result: "hello".to_owned(),
            },
            Request::RecordAiText {
                session: 1 as SessionId,
                operation: AiTextOperation::Transform,
                status: AiTextStatus::Applied,
                source: "hello".to_owned(),
                result: "hello".to_owned(),
                model: "model".to_owned(),
                provider: "provider".to_owned(),
                style: "style".to_owned(),
                error_code: String::new(),
                latency_ms: 1,
                attempts: 1,
                input_tokens: 1,
                output_tokens: 1,
                cached_tokens: 0,
                test_only: false,
            },
        ];
        for request in &requests {
            assert!(!request_allowed(
                Endpoint::Data,
                request,
                ClientTrust::LowIntegrity
            ));
            assert!(!request_allowed(
                Endpoint::Data,
                request,
                ClientTrust::AppContainer
            ));
            assert!(!request_allowed(
                Endpoint::Data,
                request,
                ClientTrust::Unknown
            ));
            assert!(request_allowed(
                Endpoint::Data,
                request,
                ClientTrust::MediumOrHigher
            ));
        }
    }

    #[test]
    fn admission_is_raii_bounded_per_endpoint_and_process() {
        let admission = Arc::new(Admission::default());
        let mut data = Vec::new();
        for _ in 0..MAX_CONNECTIONS_PER_PID {
            data.push(
                admission
                    .try_acquire(Endpoint::Data, 4242)
                    .expect("the quota admits a normal process's connections"),
            );
        }
        assert!(
            admission.try_acquire(Endpoint::Data, 4242).is_none(),
            "one process must not pin every data endpoint instance"
        );
        assert!(
            admission.try_acquire(Endpoint::Renderer, 4242).is_some(),
            "the quota is scoped to the server-owned endpoint"
        );

        data.pop();
        assert!(
            admission.try_acquire(Endpoint::Data, 4242).is_some(),
            "dropping a connection permit must release its slot"
        );
    }

    #[test]
    fn first_handshake_deadline_closes_an_idle_connection() {
        let name = unique_test_pipe("handshake-timeout");
        let security = Descriptor::for_pipe().expect("descriptor");
        let instance = PipeInstance::create(&name, &security, true).expect("create");
        let client = Client::connect_to(&name, Duration::from_secs(5)).expect("connect");
        instance.wait_for_client().expect("accept");

        let started = Instant::now();
        let result = read_first_frame_with_deadline(&instance);
        assert!(matches!(result, Err(Fault::Timeout)));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the handshake deadline must not turn into an unbounded read"
        );
        drop(client);
    }

    /// The pool must not be able to outgrow the instance limit the pipe
    /// was created with. Past it `CreateNamedPipeW` fails, and the failure
    /// would land on a user opening one more application — the worst
    /// possible moment for an IME to become unavailable.
    #[test]
    fn the_pool_stops_growing_at_the_instance_cap() {
        let server = Server::new(false).expect("this process has a token");
        assert_eq!(
            server.shared.created.load(Ordering::Relaxed),
            0,
            "a server that has not run yet owns no instances"
        );

        // Claim to be full and to have nobody waiting, which is the state
        // that would otherwise make a worker add an instance.
        server
            .shared
            .created
            .store(MAX_INSTANCES, Ordering::Relaxed);
        server.shared.idle.store(0, Ordering::Relaxed);
        ensure_spare_instance(&server.shared);

        assert_eq!(
            server.shared.created.load(Ordering::Relaxed),
            MAX_INSTANCES,
            "the pool grew past the cap the pipe was created with"
        );
    }

    /// The cap only means anything if a slot comes back. `created` used to
    /// be incremented only, so every worker that ended took a slot with it
    /// and the engine crept towards refusing new clients while looking
    /// perfectly healthy.
    #[test]
    fn an_instance_slot_is_released_when_it_is_dropped() {
        let server = Server::new(false).expect("this process has a token");
        {
            let _slot = InstanceSlot::claim(&server.shared);
            assert_eq!(server.shared.created.load(Ordering::Relaxed), 1);
        }
        assert_eq!(
            server.shared.created.load(Ordering::Relaxed),
            0,
            "an instance that no longer exists still counts against the cap"
        );
    }

    /// Releasing the last slot has to end the engine, because nothing can
    /// create an instance once there is no worker to call
    /// `ensure_spare_instance`. Without this the process stayed alive with
    /// no acceptor: arriving clients blocked in `CreateFileW` and the
    /// renderer's watchdog, which only restarts an engine that is gone,
    /// left it exactly where it was.
    ///
    /// The reason matters as much as the wakeup. `run` announces a stop to
    /// the renderer only for [`StopReason::Requested`]; announcing this one
    /// would tell the watchdog the exit was deliberate and leave the user
    /// with no engine at all.
    #[test]
    fn the_last_instance_leaving_ends_the_engine() {
        let server = Server::new(false).expect("this process has a token");
        drop(InstanceSlot::claim(&server.shared));

        assert_eq!(
            server.stopped.try_recv(),
            Ok(StopReason::LastInstanceGone),
            "the engine kept waiting with nothing left to accept on"
        );
    }

    /// The other half: an instance leaving a pool that still has one is
    /// ordinary — every worker that finishes does it — and must not take
    /// the engine down with it.
    #[test]
    fn an_instance_leaving_a_pool_that_still_has_one_does_not() {
        let server = Server::new(false).expect("this process has a token");
        let _kept = InstanceSlot::claim(&server.shared);
        drop(InstanceSlot::claim(&server.shared));

        assert_eq!(
            server.stopped.try_recv(),
            Err(TryRecvError::Empty),
            "one worker ending stopped an engine that was still accepting"
        );
    }

    /// The same rule through the real worker: a served connection that ends
    /// the worker must give the slot back, not merely stop using it.
    #[test]
    fn a_worker_that_ends_gives_its_instance_slot_back() {
        let name = unique_test_pipe("slot-release");
        let server = Server::new(false)
            .expect("this process has a token")
            .with_explicit_test_pipe(name.clone());
        let shared = Arc::clone(&server.shared);
        // Claim an acceptor is already waiting, so serving the connection
        // below does not make this worker add a second instance: the
        // assertion is about the one slot this worker owns.
        shared.idle.store(1, Ordering::Relaxed);

        let descriptor = Descriptor::from_sddl(&shared.sddl).expect("a valid descriptor");
        let instance =
            PipeInstance::create(&shared.name, &descriptor, true).expect("the first instance");
        let slot = InstanceSlot::claim(&shared);
        assert_eq!(shared.created.load(Ordering::Relaxed), 1);

        let owned = Arc::clone(&shared);
        let serving = std::thread::spawn(move || worker(owned, instance, slot, Endpoint::Data));

        let mut client = Client::connect_to(&name, Duration::from_secs(5)).expect("a connection");
        let reply = client
            .call(&Request::Shutdown, Duration::from_secs(5))
            .expect("an answer to the shutdown request");
        assert!(matches!(reply, Response::Ok));
        drop(client);
        serving.join().expect("the worker thread ended");

        assert_eq!(
            shared.created.load(Ordering::Relaxed),
            0,
            "a worker that ended kept its instance slot"
        );
        // Both stops are sent here — the requested one from the worker, then
        // the slot's own as it is released — and the order decides what the
        // renderer is told. The requested one must arrive first, or a
        // user-asked shutdown would look like a fault and be restarted.
        assert_eq!(server.stopped.try_recv(), Ok(StopReason::Requested));
        assert_eq!(server.stopped.try_recv(), Ok(StopReason::LastInstanceGone));
    }

    /// A client that connects and is gone before it can be served is
    /// ordinary — a host process exiting at the wrong moment — and must
    /// cost one wasted accept, not the acceptor. The bound is what keeps
    /// the same answer from an unusable instance from becoming a spin.
    #[test]
    fn one_departed_client_keeps_the_acceptor_and_a_stuck_instance_does_not() {
        assert!(
            !empty_accept_is_fatal(1),
            "an ordinary client race ended the acceptor"
        );
        assert!(
            !empty_accept_is_fatal(MAX_CONSECUTIVE_EMPTY_ACCEPTS - 1),
            "the bound must be a ceiling, not a hair trigger"
        );
        assert!(
            empty_accept_is_fatal(MAX_CONSECUTIVE_EMPTY_ACCEPTS),
            "an instance that never produces a client would be retried forever"
        );
    }

    fn unique_test_pipe(purpose: &str) -> String {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        format!(
            r"\\.\pipe\SakuraInputEngineTest-{purpose}-{}-{stamp}",
            std::process::id()
        )
    }

    fn private_endpoint_server(name: &str) -> Server {
        let mut server = Server::new(false).unwrap();
        let shared = Arc::get_mut(&mut server.shared).unwrap();
        shared.name = format!("{name}-data");
        shared.renderer_name = format!("{name}-renderer");
        shared.control_name = format!("{name}-control");
        server
    }

    #[test]
    fn startup_reservation_blocks_duplicates_before_workers_and_releases_on_drop() {
        let name = unique_test_pipe("reserve");
        let owner = private_endpoint_server(&name).reserve().unwrap();
        assert_eq!(owner.shared.total_created.load(Ordering::Relaxed), 0);
        assert!(private_endpoint_server(&name).reserve().is_err());
        drop(owner);
        let replacement = private_endpoint_server(&name).reserve().unwrap();
        assert_eq!(replacement.reserved_instances.as_ref().unwrap().len(), 3);
        drop(replacement);
    }

    #[test]
    fn startup_reservation_partial_failure_releases_earlier_endpoints_without_ready() {
        let name = unique_test_pipe("partial-reserve");
        let blocker = private_endpoint_server(&name);
        let occupied_renderer = create_instance(&blocker.shared, Endpoint::Renderer, true).unwrap();
        assert!(private_endpoint_server(&name).reserve().is_err());
        // A first-instance creation succeeds only if the failed acquisition
        // dropped its earlier data handle, despite the renderer collision.
        drop(create_instance(&blocker.shared, Endpoint::Data, true).unwrap());
        let ready = std::cell::Cell::new(false);
        assert!(private_endpoint_server(&name)
            .run_when_ready(|| ready.set(true))
            .is_err());
        assert!(!ready.get());
        drop(occupied_renderer);
        drop(private_endpoint_server(&name).reserve().unwrap());
    }

    #[test]
    fn startup_spawn_failure_joins_workers_releases_slots_and_suppresses_ready() {
        for preceding_workers in 0..3 {
            let name = unique_test_pipe("spawn-failure");
            let server = private_endpoint_server(&name).reserve().unwrap();
            let shared = Arc::clone(&server.shared);
            let ready = std::cell::Cell::new(false);
            FAIL_PIPE_SPAWN_AFTER.with(|remaining| remaining.set(Some(preceding_workers)));
            let result = server.run_when_ready(|| ready.set(true));
            FAIL_PIPE_SPAWN_AFTER.with(|remaining| remaining.set(None));
            assert!(result.is_err());
            assert!(!ready.get());
            assert_eq!(shared.total_created.load(Ordering::Relaxed), 0);
            assert_eq!(shared.created.load(Ordering::Relaxed), 0);
            assert_eq!(shared.renderer_created.load(Ordering::Relaxed), 0);
            assert_eq!(shared.control_created.load(Ordering::Relaxed), 0);
            // No worker-held Shared reference remains after the joined abort.
            assert_eq!(Arc::strong_count(&shared), 1);
            drop(private_endpoint_server(&name).reserve().unwrap());
        }
    }

    /// The other half of the same rule: a worker only adds an instance
    /// when no acceptor is left waiting. Otherwise every connection would
    /// add one and the pool would reach the cap under ordinary use.
    #[test]
    fn an_idle_acceptor_means_no_new_instance() {
        let server = Server::new(false).expect("this process has a token");
        server.shared.idle.store(1, Ordering::Relaxed);
        ensure_spare_instance(&server.shared);
        assert_eq!(server.shared.created.load(Ordering::Relaxed), 0);
    }

    /// A `Response` `send` cannot encode. Its single candidate annotation
    /// sits over `MAX_STRING_BYTES`, so `write_str` rejects it before
    /// `encode_response` gets anywhere near `MAX_PAYLOAD` — no connected
    /// peer is needed to observe the failure, because `send` never reaches
    /// `write_all` when encoding fails first.
    ///
    /// This does not claim the failure is reachable through any live
    /// request-handling path today: every `Response` `serve` actually
    /// builds is sourced from `OutputBuf`'s fixed-capacity buffers, bounded
    /// well under `MAX_PAYLOAD`. `CandidateList.items: Vec<Candidate>` is
    /// unbounded at the type level, though, so nothing here is exercising
    /// dead code — only a currently-unused-in-practice path.
    fn oversized_ui_response() -> Response {
        use sakura_proto::types::CandidatePresentation;
        use sakura_proto::{Candidate, CandidateKind, CandidateList, UiState, MAX_STRING_BYTES};

        Response::Ui(UiState {
            revision: 1,
            appearance_theme: sakura_proto::AppearanceTheme::Auto,
            pad_shortcut: sakura_proto::PadShortcut::Disabled,
            mode: None,
            candidates: Some(CandidateList {
                kind: CandidateKind::Conversion,
                presentation: CandidatePresentation::Expanded,
                items: vec![Candidate {
                    text: "a".repeat(MAX_STRING_BYTES + 1),
                    annotation: String::new(),
                    deletable_history: false,
                }],
                selected: 0,
                page_size: 9,
            }),
            candidate_detail: None,
            anchor: None,
            document: None,
            renderer_visible: true,
            stopping: false,
        })
    }

    fn unconnected_instance(tag: &str) -> PipeInstance {
        let name = format!(
            r"\\.\pipe\sakura_engine_test_send_{tag}_{}",
            std::process::id()
        );
        let security = Descriptor::for_pipe().expect("descriptor");
        PipeInstance::create(&name, &security, true).expect("create")
    }

    /// [`sakura_ipc::Fault::Protocol`]'s own doc comment says "the client
    /// sent something the protocol forbids" — a reply this side fails to
    /// encode is not that. The client sent nothing malformed; this process
    /// failed to serialize its own answer.
    #[test]
    fn local_response_encode_failure_is_not_reported_as_peer_protocol_fault() {
        let instance = unconnected_instance("encode_failure_fault");
        let mut scratch = Vec::new();
        let outcome = send(&instance, &oversized_ui_response(), 1, &mut scratch);

        assert!(
            matches!(outcome, Err(Fault::Encode(sakura_proto::Error::TooLarge))),
            "a local encode failure must surface as Fault::Encode, not \
             Fault::Protocol, since the peer did nothing wrong: got {outcome:?}"
        );
    }

    /// Unlike the client's `Fault::Encode` (the peer never saw the failed
    /// request, so the link stays usable), a server that cannot encode its
    /// reply has already dispatched the client's request and left it
    /// waiting for an answer it can never receive on this connection. The
    /// only safe move is to end the connection so the client resynchronizes
    /// instead of hanging on a reply that will never arrive.
    #[test]
    fn local_response_encode_failure_still_ends_the_connection() {
        let instance = unconnected_instance("encode_failure_ends");
        let mut scratch = Vec::new();
        let fault = send(&instance, &oversized_ui_response(), 1, &mut scratch)
            .expect_err("an oversized response must fail to encode");

        assert!(
            matches!(end(fault), Outcome::Failed(Fault::Encode(_))),
            "a response this side could not encode must still end the \
             connection: the peer is left waiting for a reply it will \
             never receive on this connection"
        );
    }
}
