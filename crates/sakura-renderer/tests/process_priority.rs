//! The renderer's own priority class, on a real process (#322).
//!
//! The logon task starts Sakura Input below normal, and a child created
//! without a priority flag inherits its parent's class. The renderer moves
//! itself to normal before it does anything else. This starts a renderer
//! below normal, the way the logon launcher starts it, and reads the class
//! back from outside the process.
//!
//! Isolation follows `pad_ui.rs`: the test owns a uniquely named pipe and
//! `LOCALAPPDATA` points at the target temp directory, so the installed IME
//! and the production pipe are never touched. Nothing is drawn: the pipe is
//! never answered, so the renderer never receives a state to show. Its
//! connection is the observation point instead of a sleep or a poll. The
//! class changes before the first window exists, so once the renderer has
//! reached its pipe, the class it reports is final. Left alone, it would
//! give up on the unanswered `Hello` after `PATIENT_CONNECT` and exit.

use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sakura_ipc::{security::Descriptor, Accept, PipeInstance};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    GetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
};

const TEST_PIPE_PREFIX: &str = r"\\.\pipe\SakuraInputRendererTest-";
/// A cold process start on a busy machine, not a measurement.
const STARTUP_BUDGET: Duration = Duration::from_secs(30);

#[test]
fn a_renderer_started_below_normal_runs_at_normal() {
    let app_data = IsolatedAppData::new("renderer-priority");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let pipe_name = format!("{TEST_PIPE_PREFIX}{}-{nonce}", std::process::id());
    // Claimed before the renderer starts, so its watcher finds this pipe and
    // nothing else.
    let security = Descriptor::for_pipe().expect("fixture pipe security descriptor");
    let pipe = PipeInstance::create(&pipe_name, &security, true).expect("create fixture pipe");

    let child = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_sakura_renderer")))
        .arg("--test-pipe")
        .arg(&pipe_name)
        .env("LOCALAPPDATA", app_data.path())
        .creation_flags(BELOW_NORMAL_PRIORITY_CLASS.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn test-owned renderer");
    let renderer = OwnedChild(child);

    // `wait_for_client` has no deadline of its own; a renderer that never
    // connects must fail this test rather than hang it.
    let (accepted, accept) = mpsc::channel();
    thread::spawn(move || {
        let outcome = pipe.wait_for_client().map(|accept| match accept {
            Accept::Connected => pipe.client_process_id().ok(),
            Accept::ClientGone => None,
        });
        let _ = accepted.send(outcome);
        // Held until the renderer has been read, so it cannot see a broken
        // pipe and exit before then.
        thread::park();
    });
    let client = accept
        .recv_timeout(STARTUP_BUDGET)
        .expect("the renderer connects to its test pipe")
        .expect("accept the renderer's connection");
    assert_eq!(
        client,
        Some(renderer.0.id()),
        "the test pipe is not held by the renderer this test started"
    );

    let class = renderer.priority_class();
    assert_eq!(
        class, NORMAL_PRIORITY_CLASS.0,
        "the renderer still runs at the class its launcher gave it (0x{class:X}); \
         below normal is 0x{:X}",
        BELOW_NORMAL_PRIORITY_CLASS.0
    );
}

struct OwnedChild(Child);

impl OwnedChild {
    fn priority_class(&self) -> u32 {
        // SAFETY: `self.0` owns this process handle, opened with full access
        // by `CreateProcess`, and keeps it open for the duration of the call.
        unsafe { GetPriorityClass(HANDLE(self.0.as_raw_handle())) }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct IsolatedAppData(PathBuf);

impl IsolatedAppData {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create isolated app data");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for IsolatedAppData {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
