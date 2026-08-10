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
//! The cost of share-nothing is that cross-session state cannot live here.
//! That is deliberate: such state arrives as an explicitly synchronized
//! component with its own lock discipline, rather than as a shared mutable
//! engine that every connection happens to reach through. [`crate::ui`]'s
//! board is the first — the renderer has to be told about a mode change
//! that happened on somebody else's connection — and the input history that
//! later phases use to bias conversion (DESIGN 5.4) will be the next.
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

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use sakura_core::{default_app_profiles, AppProfile, Preferences};
use sakura_proto::{
    encode_response, peek_header, ErrorCode, OutputBuf, Request, RequestId, Response, MAX_FRAME,
};

use sakura_ipc::{security, Descriptor, Fault, PipeInstance, MAX_INSTANCES};

use crate::dictionary::ConversionService;
use crate::dispatch::{Dispatcher, Reply};
use crate::input_history::InputHistoryService;
use crate::learning::LearningService;
use crate::long_conversion::LongConversionService;
use crate::prediction::PredictionService;
use crate::ui::UiBoard;

/// Everything a worker thread needs that is not its own pipe instance.
#[derive(Debug)]
struct Shared {
    name: String,
    sddl: String,
    /// Instances created so far, capped at [`MAX_INSTANCES`].
    created: AtomicU32,
    /// Acceptors currently blocked waiting for a client.
    idle: AtomicU32,
    /// Set once, when a client asks the engine to stop.
    shutdown: Sender<()>,
    /// What the renderer draws. The one thing every connection shares.
    ui: UiBoard,
    /// Read-only dictionary plus the bounded process-wide conversion pool.
    conversion: Option<Arc<ConversionService>>,
    /// Process-wide synchronized personalization index and durable log.
    learning: Option<Arc<LearningService>>,
    /// Explicitly enabled developer interaction history.
    input_history: Option<Arc<InputHistoryService>>,
    /// Request side of the one process-wide prediction worker.
    prediction: Option<Arc<PredictionService>>,
    /// Optional isolated ONNX reranker; its child process remains lazy.
    long_conversion: Option<Arc<LongConversionService>>,
    preferences: Preferences,
    profiles: Arc<[AppProfile]>,
    verbose: bool,
}

/// The engine's pipe server.
#[derive(Debug)]
pub struct Server {
    shared: Arc<Shared>,
    stopped: Receiver<()>,
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
        Ok(Server {
            shared: Arc::new(Shared {
                name: security::pipe_name()?,
                sddl: security::sddl()?,
                created: AtomicU32::new(0),
                idle: AtomicU32::new(0),
                shutdown,
                ui: UiBoard::new(),
                conversion,
                learning,
                input_history,
                prediction,
                long_conversion: None,
                preferences,
                profiles,
                verbose,
            }),
            stopped,
        })
    }

    /// The pipe this server listens on.
    pub fn pipe_name(&self) -> &str {
        &self.shared.name
    }

    /// Replaces the name before [`run`](Self::run) creates any pipe instance.
    ///
    /// The binary invokes this only after validating its narrowly scoped test
    /// command-line option. Production constructors still resolve their normal
    /// name through [`security::pipe_name`] during construction.
    pub fn with_explicit_test_pipe(mut self, pipe_name: String) -> Self {
        Arc::get_mut(&mut self.shared)
            .expect("a newly constructed server has no worker thread or shared clone")
            .name = pipe_name;
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
        spawn_worker(&self.shared, true)?;
        // The only sender lives in `shared`, which the worker threads hold,
        // so `recv` returns either when a client asks for shutdown or when
        // every worker is gone.
        let _ = self.stopped.recv();
        // Tell the renderer this was deliberate before the pipe breaks
        // under it, and hold the exit open just long enough for that to
        // reach the wire. Without it the renderer's watchdog sees only a
        // dead engine and restarts the one `--stop` just stopped — which
        // during an uninstall means relaunching the file being deleted.
        self.shared.ui.stop();
        self.shared.ui.settle(SHUTDOWN_GRACE);
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
const WORKER_STACK_BYTES: usize = 128 * 1024;

/// Creates one pipe instance and the thread that serves it.
fn spawn_worker(shared: &Arc<Shared>, first: bool) -> windows::core::Result<()> {
    let descriptor = Descriptor::from_sddl(&shared.sddl)?;
    let instance = PipeInstance::create(&shared.name, &descriptor, first)?;
    shared.created.fetch_add(1, Ordering::Relaxed);
    let owned = Arc::clone(shared);
    let spawned = std::thread::Builder::new()
        .name("sakura-pipe".to_owned())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || worker(owned, instance));

    match spawned {
        Ok(_) => Ok(()),
        Err(error) => {
            // The instance has no thread to accept on it, so it must not
            // count towards the cap. Its handle is already closed: the
            // closure that owned it was dropped when the spawn failed.
            shared.created.fetch_sub(1, Ordering::Relaxed);
            Err(thread_failure(&error))
        }
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

/// One instance's whole life: accept, serve, disconnect, repeat.
fn worker(shared: Arc<Shared>, instance: PipeInstance) {
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
                shared.preferences,
                Arc::clone(&shared.profiles),
            )
        }
        (Some(conversion), Some(learning), None) => {
            Dispatcher::new_with_configuration_and_profiles(
                Arc::clone(conversion),
                Arc::clone(learning),
                shared.preferences,
                Arc::clone(&shared.profiles),
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
    if let Some(history) = shared.input_history.as_ref() {
        dispatcher.set_input_history(Arc::clone(history));
    }
    if let Some(long_conversion) = shared.long_conversion.as_ref() {
        dispatcher.set_long_conversion(Arc::clone(long_conversion));
    }
    let mut connection = Buffers::new();

    loop {
        shared.idle.fetch_add(1, Ordering::Relaxed);
        let accepted = instance.wait_for_client();
        shared.idle.fetch_sub(1, Ordering::Relaxed);
        if let Err(error) = accepted {
            report(&shared, format_args!("accept failed: {error}"));
            return;
        }

        ensure_spare_instance(&shared);

        match serve(&shared, &instance, &mut dispatcher, &mut connection) {
            Outcome::Closed => {}
            Outcome::Failed(fault) => report(&shared, format_args!("{fault}")),
            Outcome::Shutdown => {
                instance.disconnect();
                let _ = shared.shutdown.send(());
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
fn ensure_spare_instance(shared: &Arc<Shared>) {
    if shared.idle.load(Ordering::Relaxed) > 0 {
        return;
    }
    if shared.created.load(Ordering::Relaxed) >= MAX_INSTANCES {
        return;
    }
    if let Err(error) = spawn_worker(shared, false) {
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
            // them off the 128 KiB pipe-worker stack; this allocation happens
            // once per worker, never on a keystroke.
            out: Box::new(OutputBuf::new()),
        }
    }
}

/// Serves one connected client until it disconnects or misbehaves.
fn serve(
    shared: &Shared,
    instance: &PipeInstance,
    dispatcher: &mut Dispatcher,
    bufs: &mut Buffers,
) -> Outcome {
    loop {
        let payload = match instance.read_frame(&mut bufs.read) {
            Ok(payload) => payload,
            Err(Fault::Disconnected) => return Outcome::Closed,
            Err(fault) => return Outcome::Failed(fault),
        };

        let (id, request) = match sakura_proto::decode_request(payload) {
            Ok(decoded) => decoded,
            Err(error) => {
                let code = match error {
                    sakura_proto::Error::UnsupportedVersion(_) => ErrorCode::UnsupportedVersion,
                    sakura_proto::Error::TooLarge => ErrorCode::TooLarge,
                    _ => ErrorCode::Malformed,
                };
                // `peek_header` reads the id without validating the version
                // or the body, so a frame the decoder rejected can still be
                // answered by id. A client waiting on a request needs to be
                // told *which* one failed; an uncorrelated error reads as a
                // reply to whatever it sends next, which is the stale-reply
                // confusion the request id exists to prevent (DESIGN 7).
                if let Ok(header) = peek_header(payload) {
                    let _ = send(
                        instance,
                        &Response::Error(code),
                        header.request_id,
                        &mut bufs.reply,
                    );
                }
                // Then drop the connection: a byte stream that has lost
                // frame alignment cannot be resynchronized, and guessing
                // where the next frame starts is how one bad frame becomes
                // an endless stream of them.
                return Outcome::Failed(Fault::Protocol(error));
            }
        };

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
            renderer_visible,
        } = request
        {
            let _ = shared
                .ui
                .publish_placement(session, anchor, renderer_visible);
            if let Err(fault) = send(instance, &Response::Ok, id, &mut bufs.reply) {
                return end(fault);
            }
            continue;
        }

        let output_session = match &request {
            Request::SendKey { session, key } if !key.test_only => Some(*session),
            Request::Commit { session } => Some(*session),
            Request::Reconvert {
                session,
                preview: false,
                ..
            } => Some(*session),
            _ => None,
        };
        let clears_candidates = match &request {
            Request::Revert { session } | Request::DeleteSession { session } => Some(*session),
            Request::SetInputScope { session, scope }
                if matches!(
                    scope,
                    sakura_proto::InputScope::Password
                        | sakura_proto::InputScope::Url
                        | sakura_proto::InputScope::Email
                        | sakura_proto::InputScope::Digits
                ) =>
            {
                Some(*session)
            }
            _ => None,
        };

        match dispatcher.dispatch(&request, &mut bufs.out) {
            Reply::Output => {
                if let Some(session) = output_session {
                    shared.ui.publish_output(session, &bufs.out);
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
                // The TSF input-mode menu changes an idle session without a
                // document-edit `Output`. Publish its exact mode separately
                // so the renderer's transient indicator remains in sync with
                // the language-bar item that initiated the change.
                if let Response::InputMode { mode } = &response {
                    shared.ui.publish(*mode);
                }
                if matches!(response, Response::Ok) {
                    if let Some(session) = clears_candidates {
                        shared.ui.clear_session(session);
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
    use super::*;

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
            mode: None,
            candidates: Some(CandidateList {
                kind: CandidateKind::Conversion,
                presentation: CandidatePresentation::Expanded,
                items: vec![Candidate {
                    text: "a".repeat(MAX_STRING_BYTES + 1),
                    annotation: String::new(),
                }],
                selected: 0,
                page_size: 9,
            }),
            candidate_detail: None,
            anchor: None,
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
