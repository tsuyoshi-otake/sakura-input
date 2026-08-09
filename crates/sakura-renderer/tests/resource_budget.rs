//! Real-process renderer footprint gate after receiving candidate data.

use std::mem::size_of;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_ipc::Client;
use sakura_proto::{KeyCode, KeyInput, Modifiers, Request, Response, ScreenRect};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
};

const RENDERER_PRIVATE_WORKING_SET_BUDGET: usize = 10 * 1024 * 1024;
const PATIENT: Duration = Duration::from_secs(5);

#[test]
#[ignore = "real release engine/renderer; set SAKURA_PHASE2_DICTIONARY"]
fn renderer_with_candidates_stays_within_its_footprint_budget() {
    assert!(
        Client::connect(Duration::from_millis(200)).is_err(),
        "a logon-session engine is already running; stop it before this isolated measurement"
    );
    let dictionary = required_path("SAKURA_PHASE2_DICTIONARY");
    let app_data = IsolatedAppData::new("renderer-resource-budget");
    let renderer_path = PathBuf::from(env!("CARGO_BIN_EXE_sakura_renderer"));
    let engine_path = renderer_path.with_file_name("sakura_engine.exe");
    assert!(
        engine_path.is_file(),
        "build the release workspace first; missing {}",
        engine_path.display()
    );

    let engine = Command::new(&engine_path)
        .env("SAKURA_DICTIONARY", &dictionary)
        .env("LOCALAPPDATA", app_data.path())
        .spawn()
        .expect("spawn release engine");
    let mut engine = OwnedChild::new(engine, "engine");
    let mut client = connect();
    let renderer = Command::new(&renderer_path)
        .env("LOCALAPPDATA", app_data.path())
        .spawn()
        .expect("spawn release renderer");
    let mut renderer = OwnedChild::new(renderer, "renderer");

    let session = match client.call(
        &Request::CreateSession {
            process_name: "phase2-renderer-budget.exe".to_owned(),
        },
        PATIENT,
    ) {
        Ok(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    };
    for character in "kannsuu".chars() {
        send_key(&mut client, session, character);
    }
    let converted = client.call(
        &Request::SendKey {
            session,
            key: KeyInput {
                code: KeyCode::Space,
                ch: None,
                modifiers: Modifiers::NONE,
                repeat: false,
                test_only: false,
            },
        },
        PATIENT,
    );
    assert!(matches!(converted, Ok(Response::Output(_))));
    assert!(matches!(
        client.call(
            &Request::SetUiPlacement {
                session,
                anchor: Some(ScreenRect {
                    left: 100,
                    top: 100,
                    right: 120,
                    bottom: 124,
                }),
                renderer_visible: true,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));

    sleep(Duration::from_millis(250));
    assert!(
        renderer
            .child_mut()
            .try_wait()
            .expect("query renderer")
            .is_none(),
        "renderer exited before the measurement (another instance may own its mutex)"
    );
    let private_working_set = private_working_set(renderer.child());
    println!("renderer private working set {private_working_set} bytes");
    if let Some(report) = std::env::var_os("SAKURA_RENDERER_RESOURCE_REPORT") {
        write_report(Path::new(&report), private_working_set);
    }
    assert!(
        private_working_set <= RENDERER_PRIVATE_WORKING_SET_BUDGET,
        "renderer private working set is {private_working_set} bytes (budget {RENDERER_PRIVATE_WORKING_SET_BUDGET})"
    );

    let _ = client.call(&Request::Shutdown, PATIENT);
    engine.wait_for_exit();
    renderer.wait_for_exit();
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

fn required_path(name: &str) -> PathBuf {
    let value = std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required"));
    let path = PathBuf::from(value);
    assert!(path.is_file(), "{} is not a file", path.display());
    path
}

fn connect() -> Client {
    let deadline = Instant::now() + PATIENT;
    loop {
        match Client::connect(Duration::from_millis(100)) {
            Ok(client) => return client,
            Err(fault) if Instant::now() >= deadline => {
                panic!("engine did not open its pipe after {PATIENT:?}: {fault:?}")
            }
            Err(_) => sleep(Duration::from_millis(20)),
        }
    }
}

fn send_key(client: &mut Client, session: u64, character: char) {
    let response = client.call(
        &Request::SendKey {
            session,
            key: KeyInput {
                code: KeyCode::Char,
                ch: Some(character),
                modifiers: Modifiers::NONE,
                repeat: false,
                test_only: false,
            },
        },
        PATIENT,
    );
    assert!(matches!(response, Ok(Response::Output(_))));
}

fn private_working_set(child: &Child) -> usize {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX2 {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
        ..Default::default()
    };
    // SAFETY: the retained child handle is live and EX2 has the accepted
    // PROCESS_MEMORY_COUNTERS prefix followed by writable extension fields.
    let queried = unsafe {
        K32GetProcessMemoryInfo(
            HANDLE(child.as_raw_handle()),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX2).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    assert!(queried.as_bool(), "K32GetProcessMemoryInfo failed");
    counters.PrivateWorkingSetSize
}

fn write_report(path: &Path, private_working_set: usize) {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).expect("create renderer report directory");
    }
    let measured_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_secs();
    let report = format!(
        "{{\n  \"schema_version\": 1,\n  \"measured_at_unix\": {measured_at},\n  \
         \"renderer_private_working_set_bytes\": {private_working_set},\n  \
         \"renderer_private_working_set_budget_bytes\": {RENDERER_PRIVATE_WORKING_SET_BUDGET},\n  \
         \"graphics_backend\": \"GDI\",\n  \"passed\": true\n}}\n"
    );
    std::fs::write(path, report).expect("write renderer resource report");
}

struct OwnedChild {
    child: Option<Child>,
    name: &'static str,
}

impl OwnedChild {
    const fn new(child: Child, name: &'static str) -> Self {
        Self {
            child: Some(child),
            name,
        }
    }

    fn child(&self) -> &Child {
        self.child.as_ref().expect("child remains owned")
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("child remains owned")
    }

    fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + PATIENT;
        loop {
            match self.child_mut().try_wait().expect("query process exit") {
                Some(status) => {
                    assert!(status.success(), "{} exited with {status}", self.name);
                    self.child = None;
                    return;
                }
                None if Instant::now() < deadline => sleep(Duration::from_millis(20)),
                None => panic!("{} did not exit within {PATIENT:?}", self.name),
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
