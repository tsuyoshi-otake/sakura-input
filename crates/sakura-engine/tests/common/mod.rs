//! Shared ownership-safe harness for integration tests that launch the real engine.
//!
//! A real user engine is a singleton on the production pipe. These tests must
//! never discover, probe, or stop it: every ordinary test creates one private
//! pipe, one private LOCALAPPDATA tree, and one private dictionary fixture,
//! passes them to a child it owns, and connects only to that exact private
//! pipe.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_ipc::{Client, Endpoint, ServerTrustPolicy};
use sakura_proto::{KeyCode, KeyInput, Modifiers, Request, Response, SessionId};

/// Long enough to cover a cold process start on a loaded machine. Nothing
/// here is measuring latency — that is `ipc_latency.rs`'s job, and it uses
/// its own budget.
pub const PATIENT: Duration = Duration::from_secs(5);

const TEST_PIPE_PREFIX: &str = r"\\.\pipe\SakuraInputEngineTest-";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static ENGINE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Windows occasionally aborts this test binary while several owned engine
/// children start and stop concurrently, even though every pipe and profile
/// is unique. Serialize real child lifetimes within one integration binary;
/// separate test binaries still retain their own process-level isolation.
fn acquire_engine_test_lock() -> MutexGuard<'static, ()> {
    ENGINE_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        // The mutex protects no data; it only serializes child lifetimes. A
        // prior assertion can poison the guard without leaving shared state,
        // so later tests must still acquire it and report their own result.
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A process and profile that this test created and therefore may clean up.
#[derive(Debug)]
pub struct Engine {
    _test_lock: MutexGuard<'static, ()>,
    child: Option<Child>,
    pipe_name: String,
    control_pipe_name: String,
    local_app_data: Option<PathBuf>,
}

/// Evidence returned by an explicit normal cleanup.
#[derive(Debug)]
pub struct Cleanup {
    pub pid: u32,
    pub status: ExitStatus,
}

impl Engine {
    /// Starts an engine on a fresh private pipe and profile.
    ///
    /// Unlike the old `running` helper, this never calls `Client::connect()`;
    /// it cannot see or reuse a user's well-known engine.
    pub fn spawn_isolated() -> Engine {
        Self::spawn_isolated_with_setup(|_| {})
    }

    /// Prepares only the owned synthetic profile before the child exists.
    pub fn spawn_isolated_with_setup(setup: impl FnOnce(&Path)) -> Engine {
        let test_lock = acquire_engine_test_lock();
        let identity = TestIdentity::new("ordinary");
        Self::spawn(identity, PipeBinding::PrivateTest, test_lock, setup)
    }

    /// Starts the sole intentional well-known-pipe test owner.
    ///
    /// This is reserved for the AppContainer test, whose child must derive the
    /// normal production name independently. It refuses to proceed if that
    /// pipe has an owner, and sends no protocol request during the check.
    pub fn spawn_well_known_for_appcontainer() -> Engine {
        let test_lock = acquire_engine_test_lock();
        ensure_well_known_pipe_is_unoccupied();
        let identity = TestIdentity::new("appcontainer");
        let pipe_name = sakura_ipc::pipe_name()
            .expect("the AppContainer test parent can resolve the production pipe name");
        Self::spawn(
            identity,
            PipeBinding::WellKnown(pipe_name),
            test_lock,
            |_| {},
        )
    }

    fn spawn(
        identity: TestIdentity,
        binding: PipeBinding,
        test_lock: MutexGuard<'static, ()>,
        setup: impl FnOnce(&Path),
    ) -> Engine {
        let dictionary = test_dictionary(&identity.local_app_data);
        setup(&identity.local_app_data);
        let pipe_name = binding.name(&identity.pipe_name);
        let control_pipe_name = match &binding {
            PipeBinding::PrivateTest => pipe_name.clone(),
            PipeBinding::WellKnown(_) => sakura_ipc::pipe_name_for(Endpoint::Control)
                .expect("the AppContainer test parent can resolve the control pipe name"),
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_sakura_engine"));
        command
            .env("SAKURA_DICTIONARY", &dictionary)
            .env("LOCALAPPDATA", &identity.local_app_data);
        if binding.uses_explicit_test_pipe() {
            command.arg("--test-pipe").arg(&pipe_name);
        }
        let child = command
            .spawn()
            .expect("the engine binary is built as a dependency of this test");
        println!(
            "test engine: pipe={} child_pid={} LOCALAPPDATA={}",
            pipe_name,
            child.id(),
            identity.local_app_data.display()
        );
        Engine {
            _test_lock: test_lock,
            child: Some(child),
            pipe_name,
            control_pipe_name,
            local_app_data: Some(identity.local_app_data),
        }
    }

    /// Connects only to this guard's explicitly-owned pipe.
    pub fn client(&mut self) -> Client {
        let deadline = Instant::now() + PATIENT;
        loop {
            self.owned_child_pid_while_running("waiting for named-pipe readiness")
                .unwrap_or_else(|detail| panic!("{detail}"));
            let policy =
                ServerTrustPolicy::Exact(PathBuf::from(env!("CARGO_BIN_EXE_sakura_engine")));
            match Client::connect_verified_to(&self.pipe_name, &policy, Duration::from_millis(100))
            {
                Ok(client) => {
                    return self
                        .require_owned_server(client, "opening a test client")
                        .unwrap_or_else(|detail| panic!("{detail}"));
                }
                Err(fault) if Instant::now() >= deadline => {
                    let status = self
                        .child
                        .as_mut()
                        .and_then(|child| child.try_wait().ok().flatten());
                    panic!(
                        "owned engine pid {} did not open {} after {PATIENT:?}: {fault:?}; child status: {status:?}",
                        self.child_pid(),
                        self.pipe_name
                    );
                }
                Err(_) => sleep(Duration::from_millis(20)),
            }
        }
    }

    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    pub fn local_app_data(&self) -> &Path {
        self.local_app_data
            .as_deref()
            .expect("the owned profile is retained until cleanup")
    }

    pub fn child_pid(&self) -> u32 {
        self.child
            .as_ref()
            .expect("the owned engine has not been cleaned up")
            .id()
    }

    /// Requests a clean shutdown on this owned pipe, waits for the exact child
    /// PID, then removes only the profile directory this guard created.
    pub fn cleanup(&mut self) -> Result<Cleanup, String> {
        let pid = self.child_pid();
        let shutdown = match Client::connect_to(&self.control_pipe_name, PATIENT) {
            Ok(client) => match self.require_owned_server(client, "opening the cleanup client") {
                Ok(mut client) => {
                    let handshake = client.call(
                        &Request::Hello {
                            client_version: sakura_proto::PROTOCOL_VERSION,
                        },
                        PATIENT,
                    );
                    match handshake {
                        Ok(Response::Hello { server_version, .. })
                            if server_version == sakura_proto::PROTOCOL_VERSION =>
                        {
                            client.call(&Request::Shutdown, PATIENT)
                        }
                        Ok(_) => Err(sakura_ipc::Fault::Protocol(sakura_proto::Error::BadEnum)),
                        Err(error) => Err(error),
                    }
                }
                Err(detail) => {
                    self.kill_and_wait_after_failure();
                    return Err(detail);
                }
            },
            Err(fault) => Err(fault),
        };
        if !matches!(shutdown, Ok(Response::Ok)) {
            let detail = format!("Shutdown for owned engine pid {pid}: {shutdown:?}");
            self.kill_and_wait_after_failure();
            return Err(detail);
        }

        let deadline = Instant::now() + PATIENT;
        loop {
            let child = self.child.as_mut().expect("owned child remains present");
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.child = None;
                    self.remove_owned_profile()?;
                    return Ok(Cleanup { pid, status });
                }
                Ok(None) if Instant::now() < deadline => sleep(Duration::from_millis(20)),
                Ok(None) => {
                    self.kill_and_wait_after_failure();
                    return Err(format!(
                        "owned engine pid {pid} did not exit after Shutdown within {PATIENT:?}"
                    ));
                }
                Err(error) => {
                    self.kill_and_wait_after_failure();
                    return Err(format!("could not query owned engine pid {pid}: {error}"));
                }
            }
        }
    }

    fn kill_and_wait_after_failure(&mut self) {
        if let Some(mut child) = self.child.take() {
            let pid = child.id();
            if let Err(error) = child.kill() {
                eprintln!("test cleanup: kill owned engine pid {pid} failed: {error}");
            }
            if let Err(error) = child.wait() {
                eprintln!("test cleanup: wait for owned engine pid {pid} failed: {error}");
                // Do not remove an isolated profile while the owned child may
                // still be using it.
                self.child = Some(child);
                return;
            }
        }
        if let Err(error) = self.remove_owned_profile() {
            eprintln!("test cleanup: remove isolated LOCALAPPDATA failed: {error}");
        }
    }

    fn remove_owned_profile(&mut self) -> Result<(), String> {
        let Some(path) = self.local_app_data.take() else {
            return Ok(());
        };
        std::fs::remove_dir_all(&path)
            .map_err(|error| format!("remove owned LOCALAPPDATA {}: {error}", path.display()))
    }

    /// Proves that the child we own has not already reached its terminal state.
    fn owned_child_pid_while_running(&mut self, purpose: &str) -> Result<u32, String> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| format!("no owned engine remains while {purpose}"))?;
        let pid = child.id();
        match child.try_wait() {
            Ok(None) => Ok(pid),
            Ok(Some(status)) => Err(format!(
                "owned engine pid {pid} exited with {status} while {purpose}; no protocol request was sent"
            )),
            Err(error) => Err(format!(
                "could not inspect owned engine pid {pid} while {purpose}: {error}; no protocol request was sent"
            )),
        }
    }

    /// Rejects a named-pipe connection unless its kernel-reported server PID is
    /// the child this guard spawned. This query targets the exact client handle,
    /// rather than a second connection that could observe a different server.
    fn require_owned_server(&mut self, client: Client, purpose: &str) -> Result<Client, String> {
        let expected = self.owned_child_pid_while_running(purpose)?;
        let actual = client.server_process_id().map_err(|fault| {
            format!(
                "could not identify the server on the exact pipe connection while {purpose}: {fault:?}; no protocol request was sent"
            )
        })?;
        if actual != expected {
            return Err(format!(
                "refusing {purpose}: exact pipe connection is served by pid {actual}, not owned engine pid {expected}; no protocol request was sent"
            ));
        }
        Ok(client)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.child.is_some() {
            eprintln!(
                "test cleanup: unwinding with owned engine pid {}; killing only that child",
                self.child_pid()
            );
            self.kill_and_wait_after_failure();
        } else if let Err(error) = self.remove_owned_profile() {
            eprintln!("test cleanup: remove isolated LOCALAPPDATA failed: {error}");
        }
    }
}

#[derive(Debug)]
struct TestIdentity {
    pipe_name: String,
    local_app_data: PathBuf,
}

/// Which name the child engine must actually bind. The AppContainer probe is
/// the one exception that intentionally owns the production well-known name.
#[derive(Debug)]
enum PipeBinding {
    PrivateTest,
    WellKnown(String),
}

impl PipeBinding {
    fn name(&self, private_name: &str) -> String {
        match self {
            Self::PrivateTest => private_name.to_owned(),
            Self::WellKnown(name) => name.clone(),
        }
    }

    fn uses_explicit_test_pipe(&self) -> bool {
        matches!(self, Self::PrivateTest)
    }
}

impl TestIdentity {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let token = format!("{}-{:x}-{:x}", std::process::id(), nonce, sequence);
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sakura-engine-real-process");
        std::fs::create_dir_all(&root).expect("create shared target test root");
        let profile = root.join(format!("{label}-{token}"));
        // Never replace a pre-existing path: a collision is ownership failure.
        std::fs::create_dir(&profile).unwrap_or_else(|error| {
            panic!(
                "create unique isolated LOCALAPPDATA {} (collision is fatal): {error}",
                profile.display()
            )
        });
        TestIdentity {
            pipe_name: format!("{TEST_PIPE_PREFIX}{label}-{token}"),
            local_app_data: profile,
        }
    }
}

fn ensure_well_known_pipe_is_unoccupied() {
    use windows::core::HRESULT;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;

    match Client::connect(Duration::from_millis(100)) {
        Ok(_) | Err(sakura_ipc::Fault::Timeout) => panic!(
            "refusing AppContainer test: the production well-known pipe already has an owner; no protocol request was sent"
        ),
        Err(sakura_ipc::Fault::Os(error))
            if error.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {}
        Err(fault) => panic!(
            "refusing AppContainer test: could not prove the production well-known pipe is unoccupied ({fault:?}); no protocol request was sent"
        ),
    }
}

pub fn test_dictionary(local_app_data: &Path) -> PathBuf {
    let directory = local_app_data.join("engine-fixture");
    std::fs::create_dir(&directory).expect("create owned fixture directory");
    let path = directory.join("system.dic");
    let mut entries = dictc::parse_entries(
        "engine-fixture.tsv",
        concat!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
            "かな\t仮名\t0\t0\t100\t100\tit\tIT用語\n",
            "きょう\t今日\t0\t0\t100\t100\t\tfixture\n",
            "は\tは\t0\t0\t100\t100\t\tfixture\n",
            "きょうは\t今日は\t0\t0\t500\t500\t\tfixture\n",
        ),
    )
    .expect("parse engine fixture entries");
    entries.extend(
        dictc::parse_entries(
            "engine-shifted-english.tsv",
            concat!(
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
                "claude\tClaude\t0\t0\t100\t100\tit\tfixture\n",
                "claude\tClaude Code\t0\t0\t150\t150\tit\tfixture\n",
                "openai\tOpenAI\t0\t0\t100\t100\tit\tfixture\n",
                "gitlab\tGitLab\t0\t0\t100\t100\tit\tfixture\n",
                "pytorch\tPyTorch\t0\t0\t100\t100\tit\tfixture\n",
            ),
        )
        .expect("parse shifted English fixture entries"),
    );
    let matrix = dictc::parse_connection(
        "engine-fixture-matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("parse engine fixture matrix");
    let image = dictc::compile(&entries, &matrix).expect("compile engine fixture dictionary");
    std::fs::write(&path, image).expect("write owned fixture dictionary");
    path
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

pub fn test_char_key(c: char) -> KeyInput {
    KeyInput {
        test_only: true,
        ..char_key(c)
    }
}

pub fn shifted_char_key(c: char) -> KeyInput {
    KeyInput {
        modifiers: Modifiers::SHIFT,
        ..char_key(c)
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

pub fn test_named_key(code: KeyCode) -> KeyInput {
    KeyInput {
        test_only: true,
        ..named_key(code)
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
        Ok(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identities_claim_distinct_new_profiles_without_reusing_paths() {
        let first = TestIdentity::new("identity");
        let second = TestIdentity::new("identity");
        assert_ne!(first.pipe_name, second.pipe_name);
        assert_ne!(first.local_app_data, second.local_app_data);
        assert!(first.local_app_data.is_dir());
        assert!(second.local_app_data.is_dir());
        std::fs::remove_dir_all(&first.local_app_data).expect("remove first owned profile");
        std::fs::remove_dir_all(&second.local_app_data).expect("remove second owned profile");
    }

    #[test]
    fn the_appcontainer_binding_retains_the_actual_well_known_pipe_name() {
        let private = r"\\.\pipe\SakuraInputEngineTest-private";
        let well_known = r"\\.\pipe\sakura_input_well_known".to_owned();
        assert_eq!(PipeBinding::PrivateTest.name(private), private);
        assert!(PipeBinding::PrivateTest.uses_explicit_test_pipe());
        assert_eq!(
            PipeBinding::WellKnown(well_known.clone()).name(private),
            well_known
        );
        assert!(!PipeBinding::WellKnown("ignored".to_owned()).uses_explicit_test_pipe());
    }
}
