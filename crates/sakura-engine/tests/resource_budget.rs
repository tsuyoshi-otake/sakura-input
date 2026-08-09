//! Phase 2's real-process dictionary and private-working-set release gates.
//!
//! Run deliberately and alone in release mode:
//!
//! ```text
//! SAKURA_PHASE2_DICTIONARY=... cargo test -p sakura-engine --release \
//!   --test resource_budget -- --ignored --nocapture
//! ```

use std::mem::size_of;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_ipc::Client;
use sakura_proto::{KeyCode, KeyInput, Modifiers, Request, Response};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
};

const IMAGE_BUDGET: u64 = 128 * 1024 * 1024;
const PRIVATE_WORKING_SET_BUDGET: usize = 15 * 1024 * 1024;
const PATIENT: Duration = Duration::from_secs(5);

#[test]
#[ignore = "real release engine and full dictionary; set SAKURA_PHASE2_DICTIONARY"]
fn full_engine_stays_within_the_phase2_resource_budgets() {
    assert!(
        Client::connect(Duration::from_millis(200)).is_err(),
        "a logon-session engine is already running; stop it before this isolated measurement"
    );
    let dictionary = required_path("SAKURA_PHASE2_DICTIONARY");
    let app_data = IsolatedAppData::new("engine-resource-budget");
    let image_bytes = std::fs::metadata(&dictionary)
        .expect("full dictionary metadata")
        .len();
    assert!(
        image_bytes <= IMAGE_BUDGET,
        "dictionary is {image_bytes} bytes (budget {IMAGE_BUDGET})"
    );

    let child = Command::new(env!("CARGO_BIN_EXE_sakura_engine"))
        .env("SAKURA_DICTIONARY", &dictionary)
        .env("LOCALAPPDATA", app_data.path())
        .spawn()
        .expect("spawn release engine");
    let mut engine = OwnedChild(Some(child));
    let mut client = connect();
    let session = match client.call(
        &Request::CreateSession {
            process_name: "phase2-resource-budget.exe".to_owned(),
        },
        PATIENT,
    ) {
        Ok(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    };
    for character in "kyoukaigidesetteihenkounokekkawokuwashikusetsumeisuru".chars() {
        send_key(&mut client, session, character);
    }
    let response = client.call(
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
    assert!(matches!(response, Ok(Response::Output(_))));

    // Let the worker return to its pipe read so the sample describes the
    // steady state after real dictionary conversion, not a transient frame.
    sleep(Duration::from_millis(100));
    let private_working_set = private_working_set(engine.child());
    println!("image {image_bytes} bytes; engine private working set {private_working_set} bytes");
    if let Some(report) = std::env::var_os("SAKURA_RESOURCE_REPORT") {
        write_report(Path::new(&report), image_bytes, private_working_set);
    }
    assert!(
        private_working_set <= PRIVATE_WORKING_SET_BUDGET,
        "private working set is {private_working_set} bytes (budget {PRIVATE_WORKING_SET_BUDGET})"
    );

    let _ = client.call(&Request::Shutdown, PATIENT);
    engine.wait_for_exit();
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
    // SAFETY: the child handle remains live for this call. EX2 starts with the
    // exact PROCESS_MEMORY_COUNTERS prefix the API accepts, and `cb` names the
    // complete writable structure.
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

fn write_report(path: &Path, image_bytes: u64, private_working_set: usize) {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).expect("create resource report directory");
    }
    let measured_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_secs();
    let report = format!(
        "{{\n  \"schema_version\": 1,\n  \"measured_at_unix\": {measured_at},\n  \
         \"dictionary_bytes\": {image_bytes},\n  \"dictionary_budget_bytes\": {IMAGE_BUDGET},\n  \
         \"engine_private_working_set_bytes\": {private_working_set},\n  \
         \"engine_private_working_set_budget_bytes\": {PRIVATE_WORKING_SET_BUDGET},\n  \
         \"passed\": true\n}}\n"
    );
    std::fs::write(path, report).expect("write resource report");
}

struct OwnedChild(Option<Child>);

impl OwnedChild {
    fn child(&self) -> &Child {
        self.0.as_ref().expect("child remains owned")
    }

    fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + PATIENT;
        loop {
            let child = self.0.as_mut().expect("child remains owned");
            match child.try_wait().expect("query engine exit") {
                Some(status) => {
                    assert!(status.success(), "engine exited with {status}");
                    self.0 = None;
                    return;
                }
                None if Instant::now() < deadline => sleep(Duration::from_millis(20)),
                None => panic!("engine did not exit after Shutdown within {PATIENT:?}"),
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
