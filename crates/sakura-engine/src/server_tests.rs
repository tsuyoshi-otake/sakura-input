thread_local! {
    pub(super) static BEFORE_OUTPUT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
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
fn disconnected_queued_key_is_not_dispatched() {
    use sakura_proto::{KeyCode, KeyInput, Modifiers};
    let key = |ch| KeyInput {
        code: KeyCode::Char,
        ch: Some(ch),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    };
    let shared = test_shared(Arc::new(LearningService::memory()));
    let name = unique_test_pipe("abandoned-queued-key");
    let security = Descriptor::for_pipe().expect("descriptor");
    let pipe = PipeInstance::create(&name, &security, true).expect("pipe");
    let mut client = Client::connect_to(&name, Duration::from_secs(2)).expect("client");
    pipe.wait_for_client().expect("accept");
    let mut dispatcher = Dispatcher::new().expect("dispatcher");
    let mut bufs = Buffers::new();
    let Reply::Message(Response::SessionCreated { session, .. }) = dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "synthetic-queued.exe".to_owned(),
        },
        &mut bufs.out,
    ) else {
        panic!("session");
    };
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: key('k'),
        },
        &mut bufs.out,
    );
    let (release_tx, release_rx) = mpsc::channel();
    let serving = std::thread::spawn(move || {
        release_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("release request reader");
        let outcome = serve(
            &shared,
            &pipe,
            Endpoint::Data,
            ClientTrust::MediumOrHigher,
            &mut dispatcher,
            &mut bufs,
        );
        // Inspect the synthetic session before worker reset. No host write
        // occurs here; this commit is only a probe of what was dispatched.
        dispatcher.dispatch(&Request::Commit { session }, &mut bufs.out);
        (outcome, bufs.out.commit_text().map(str::to_owned))
    });
    let answer = client.call(
        &Request::SendKey {
            session,
            key: key('a'),
        },
        Duration::from_millis(100),
    );
    drop(client);
    release_tx.send(()).expect("release server");
    let (outcome, text) = serving.join().expect("server ended");
    assert!(matches!(answer, Err(Fault::Timeout)));
    assert!(matches!(outcome, Outcome::Closed | Outcome::Failed(_)));
    assert_eq!(
        text.as_deref(),
        Some("k"),
        "an abandoned queued key still mutated session state"
    );
}

#[test]
fn disconnected_busy_worker_cannot_swallow_replacement_connection_letters() {
    use sakura_proto::{KeyCode, KeyInput, Modifiers};
    let key = |code, ch| KeyInput {
        code,
        ch,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    };
    let shared = Arc::new(test_shared(Arc::new(LearningService::memory())));
    let security = Descriptor::for_pipe().expect("descriptor");
    let old_name = unique_test_pipe("old-busy-composition");
    let old_pipe = PipeInstance::create(&old_name, &security, true).expect("old pipe");
    let mut old_client = Client::connect_to(&old_name, Duration::from_secs(5)).expect("old client");
    old_pipe.wait_for_client().expect("old accept");
    let mut old_dispatcher = Dispatcher::new().expect("dispatcher");
    old_dispatcher.set_composition_fence(Arc::clone(&shared.composition_fence));
    let mut old_bufs = Buffers::new();
    let Reply::Message(Response::SessionCreated {
        session: old_session,
        ..
    }) = old_dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "synthetic-recovery.exe".to_owned(),
        },
        &mut old_bufs.out,
    )
    else {
        panic!("old session");
    };
    old_dispatcher.dispatch(
        &Request::SendKey {
            session: old_session,
            key: key(KeyCode::Char, Some('k')),
        },
        &mut old_bufs.out,
    );
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let old_shared = Arc::clone(&shared);
    let old_server = std::thread::spawn(move || {
        BEFORE_OUTPUT.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                ready_tx.send(()).expect("ready");
                release_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("release old dispatch");
            }))
        });
        let outcome = serve(
            &old_shared,
            &old_pipe,
            Endpoint::Data,
            ClientTrust::MediumOrHigher,
            &mut old_dispatcher,
            &mut old_bufs,
        );
        old_dispatcher.reset();
        outcome
    });
    let old_reply = old_client.call(
        &Request::SendKey {
            session: old_session,
            key: key(KeyCode::Char, Some('a')),
        },
        Duration::from_millis(200),
    );
    let reached = ready_rx.recv_timeout(Duration::from_secs(5));
    drop(old_client);

    let new_name = unique_test_pipe("replacement-composition");
    let new_pipe = PipeInstance::create(&new_name, &security, true).expect("new pipe");
    let mut new_client = Client::connect_to(&new_name, Duration::from_secs(5)).expect("new client");
    new_pipe.wait_for_client().expect("new accept");
    let new_shared = Arc::clone(&shared);
    let new_server = std::thread::spawn(move || {
        let mut dispatcher = Dispatcher::new().expect("new dispatcher");
        dispatcher.set_composition_fence(Arc::clone(&new_shared.composition_fence));
        let outcome = serve(
            &new_shared,
            &new_pipe,
            Endpoint::Data,
            ClientTrust::MediumOrHigher,
            &mut dispatcher,
            &mut Buffers::new(),
        );
        dispatcher.reset();
        outcome
    });
    let Response::SessionCreated { session, .. } = new_client
        .call(
            &Request::CreateSession {
                process_name: "synthetic-recovery.exe".to_owned(),
            },
            Duration::from_secs(2),
        )
        .expect("new session reply")
    else {
        panic!("new session");
    };
    let replacement = new_client.call(
        &Request::SendKey {
            session,
            key: key(KeyCode::Char, Some('b')),
        },
        Duration::from_secs(2),
    );
    release_tx.send(()).expect("release old server");
    let old_outcome = old_server.join().expect("old server ended");
    let committed = new_client.call(
        &Request::SendKey {
            session,
            key: key(KeyCode::Enter, None),
        },
        Duration::from_secs(2),
    );
    let idle_space = new_client.call(
        &Request::SendKey {
            session,
            key: key(KeyCode::Space, None),
        },
        Duration::from_secs(2),
    );
    drop(new_client);
    new_server.join().expect("new server ended");
    assert!(matches!(old_reply, Err(Fault::Timeout)) && reached.is_ok());
    assert!(matches!(old_outcome, Outcome::Closed | Outcome::Failed(_)));
    assert!(
        matches!(replacement, Ok(Response::Output(ref output))
            if output.preedit.as_ref().is_some_and(|preedit| preedit.segments.first().is_some_and(|segment| segment.text == "b"))),
        "a disconnected old worker swallowed the new connection's letter: {replacement:?}"
    );
    assert!(
        matches!(committed, Ok(Response::Output(ref output)) if output.commit.as_deref() == Some("b"))
    );
    assert!(
        matches!(idle_space, Ok(Response::Output(ref output)) if output.commit.as_deref() == Some(" ")),
        "late old teardown armed another absorption after recovery: {idle_space:?}"
    );
}

#[test]
fn disconnected_output_cannot_replace_newer_ui_state() {
    use sakura_proto::{KeyCode, KeyInput, Mode, Modifiers};

    let shared = Arc::new(test_shared(Arc::new(LearningService::memory())));
    let name = unique_test_pipe("abandoned-output");
    let security = Descriptor::for_pipe().expect("descriptor");
    let instance = PipeInstance::create(&name, &security, true).expect("create");
    let mut client = Client::connect_to(&name, Duration::from_secs(5)).expect("connect");
    instance.wait_for_client().expect("accept");
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (session_tx, session_rx) = mpsc::channel();
    let owned = Arc::clone(&shared);
    let serving = std::thread::spawn(move || {
        let mut dispatcher = Dispatcher::new().expect("dispatcher");
        let mut bufs = Buffers::new();
        let Reply::Message(Response::SessionCreated { session, .. }) = dispatcher.dispatch(
            &Request::CreateSession {
                process_name: "synthetic.exe".to_owned(),
            },
            &mut bufs.out,
        ) else {
            panic!("session creation");
        };
        session_tx.send(session).expect("session receiver");
        BEFORE_OUTPUT.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                ready_tx.send(()).expect("ready receiver");
                release_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("release output");
            }))
        });
        serve(
            &owned,
            &instance,
            Endpoint::Data,
            ClientTrust::MediumOrHigher,
            &mut dispatcher,
            &mut bufs,
        )
    });
    let session = session_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("session");
    let reply = client.call(
        &Request::SendKey {
            session,
            key: KeyInput {
                code: KeyCode::HankakuZenkaku,
                ch: None,
                modifiers: Modifiers::NONE,
                repeat: false,
                test_only: false,
            },
        },
        Duration::from_millis(200),
    );
    let reached = ready_rx.recv_timeout(Duration::from_secs(5));
    drop(client);
    shared.ui.publish(Mode::Katakana);
    let newer = look(&shared.ui, 0);
    release_tx.send(()).expect("release server");
    let outcome = serving.join().expect("server terminated");
    assert!(matches!(reply, Err(Fault::Timeout)), "{reply:?}");
    assert!(reached.is_ok(), "request must reach the output boundary");
    assert!(matches!(outcome, Outcome::Closed | Outcome::Failed(_)));
    let after = look(&shared.ui, 0);
    assert_eq!(
        after.revision, newer.revision,
        "undeliverable output changed the shared UI"
    );
    assert_eq!(after.mode, newer.mode);
}

#[test]
fn configured_dark_appearance_reaches_the_ui_state() {
    let preferences = Preferences {
        appearance_theme: sakura_core::AppearanceTheme::Dark,
        ..Preferences::default()
    };
    let server =
        Server::build(false, None, None, None, None, preferences, Arc::from([])).expect("server");

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
    let entries = dictc_core::parse_entries(
            "server-prediction.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t0\t100\t100\tit\tIT用語\n",
        )
        .expect("prediction entries");
    let matrix = dictc_core::parse_connection(
        "server-prediction-matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("prediction matrix");
    let image = Box::leak(
        dictc_core::compile(&entries, &matrix)
            .expect("prediction dictionary")
            .into_boxed_slice(),
    );
    Arc::new(ConversionService::from_static_bytes(image).expect("prediction conversion service"))
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
