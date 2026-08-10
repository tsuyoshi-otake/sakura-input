//! Real-process UI Automation coverage for selected-candidate details.
//!
//! This fixture intentionally does not launch the product engine or open its
//! production pipe. The test process owns a uniquely named pipe and serves a
//! bounded sequence of `UiState` snapshots itself; the renderer is pointed at
//! that pipe with its explicit test-only command-line switch. This keeps the
//! test independent of the installed IME, a production dictionary, and the
//! user's LOCALAPPDATA while still exercising the built renderer process.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, sleep, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_ipc::{security::Descriptor, Client, PipeInstance};
use sakura_proto::types::CandidatePresentation;
use sakura_proto::{
    decode_request, encode_response, Candidate, CandidateDetail, CandidateKind, CandidateList,
    Request, Response, ScreenRect, UiState, PROTOCOL_VERSION,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RPC_E_CHANGED_MODE, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomationElement};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, SendMessageW, GWL_EXSTYLE, HTTRANSPARENT, WM_NCHITTEST, WS_EX_NOACTIVATE,
    WS_EX_TRANSPARENT,
};

const PATIENT: Duration = Duration::from_secs(5);
const TEST_PIPE_PREFIX: &str = r"\\.\pipe\SakuraInputRendererTest-";

/// Runs only when the built renderer supports its explicit `--test-pipe`
/// switch. Invocation requires Windows and a normal desktop session:
///
/// `cargo test -p sakura-renderer --test candidate_detail_uia -- --ignored`
///
/// The test prints the exact owned renderer PID, pipe, and LOCALAPPDATA path
/// to leave inspectable process-ownership evidence in the test log.
#[test]
#[ignore = "real renderer process; requires an interactive Windows desktop"]
fn selected_detail_is_fresh_complete_and_noninteractive_over_an_owned_pipe() {
    let app_data = IsolatedAppData::new("candidate-detail-uia");
    let initial = state(1, 0, None, anchor(120, 120));
    let mut engine = FixtureEngine::new(initial);
    let renderer_path = PathBuf::from(env!("CARGO_BIN_EXE_sakura_renderer"));
    let renderer = Command::new(&renderer_path)
        .arg("--test-pipe")
        .arg(engine.pipe_name())
        .env("LOCALAPPDATA", app_data.path())
        .spawn()
        .expect("spawn test-owned renderer");
    let mut renderer = OwnedChild::new(renderer, "renderer");
    println!(
        "candidate detail UIA fixture: pipe={} renderer_pid={} LOCALAPPDATA={}",
        engine.pipe_name(),
        renderer.pid(),
        app_data.path().display()
    );

    let popup = wait_for_candidate_window(renderer.pid());
    let _apartment = ComApartment::new();
    // SAFETY: the visible popup HWND is owned by the exact renderer child the
    // test spawned and remains live until the guard below completes cleanup.
    let automation: IUIAutomation = unsafe {
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            .expect("create UI Automation client")
    };
    // SAFETY: `popup` was just found as the visible candidate HWND.
    let element = unsafe {
        automation
            .ElementFromHandle(popup)
            .expect("candidate popup UIA element")
    };
    assert_noninteractive_popup(popup, &element);

    // Complete source-backed preview: UIA must include the complete text and
    // must not claim there is more source text when the flag is false.
    let full = detail("complete-definition", false, 0);
    engine.publish(state(2, 0, Some(full), anchor(120, 120)));
    let full_name = wait_for_name(&element, "complete-definition");
    assert!(!full_name.contains("Definition continues."));
    let short_detail_rect = window_rect(popup);

    // Changing only definition length must not alter the candidate/detail
    // horizontal rhythm. The fixed-width detail grows vertically to expose the
    // complete preview instead of making the popup jitter sideways.
    let long_definition = format!("long-complete-definition-{}", "x".repeat(880));
    engine.publish(state(
        3,
        0,
        Some(detail(&long_definition, false, 0)),
        anchor(120, 120),
    ));
    let long_name = wait_for_name(&element, "long-complete-definition");
    assert!(long_name.contains(&long_definition));
    let long_detail_rect = window_rect(popup);
    assert_eq!(
        long_detail_rect.right - long_detail_rect.left,
        short_detail_rect.right - short_detail_rect.left,
        "definition length must not change popup width"
    );
    assert!(
        long_detail_rect.bottom - long_detail_rect.top
            >= short_detail_rect.bottom - short_detail_rect.top
    );

    // An update for a different selected candidate with no detail must clear
    // the prior detail rather than leave the old text associated with B.
    engine.publish(state(4, 1, None, anchor(120, 120)));
    let cleared_name = wait_for_name(&element, "selected 2 of 18");
    assert!(
        !cleared_name.contains("Detail for selected candidate"),
        "selection change without a detail must clear stale detail: {cleared_name:?}"
    );
    assert!(!cleared_name.contains("complete-definition"));

    // A truncated wire preview must keep the explicit continuation marker.
    let truncated = detail("preview-definition", true, 0b1111);
    engine.publish(state(5, 1, Some(truncated), anchor(120, 120)));
    let truncated_name = wait_for_name(&element, "preview-definition");
    assert!(truncated_name.contains("Definition continues."));

    // Bounded exhaustive detail combinations: every direct relation group is
    // announced iff it is non-empty, for both complete and truncated previews.
    // This is deliberately a finite generated corpus, not a dictionary lookup.
    for truncated in [false, true] {
        for mask in 0_u8..16 {
            let token = format!("generated-definition-{truncated}-{mask}");
            let revision = 10 + u64::from(truncated as u8) * 16 + u64::from(mask);
            engine.publish(state(
                revision,
                usize::from(mask % 2),
                Some(detail(&token, truncated, mask)),
                anchor(120, 120),
            ));
            let name = wait_for_name(&element, &token);
            assert_eq!(name.contains("Definition continues."), truncated, "{token}");
            assert_relation_groups(&name, mask);
        }
    }

    // Move both selection and caret to page two. The popup has one HWND for
    // candidates and detail, so its bounds must follow the new anchor while
    // UIA immediately reports the selected second-page detail.
    let before = window_rect(popup);
    let moved_anchor = anchor(620, 420);
    engine.publish(state(
        50,
        9,
        Some(detail("page-two-definition", false, 0b0101)),
        moved_anchor,
    ));
    let page_two = wait_for_name(&element, "page 2 of 2");
    assert!(page_two.contains("selected 10 of 18"));
    assert!(page_two.contains("page-two-definition"));
    assert_relation_groups(&page_two, 0b0101);
    let after = wait_for_moved_window(popup, before);
    assert!(after.left >= moved_anchor.left || after.top >= moved_anchor.bottom);

    engine.stop();
    renderer.wait_for_exit();
}

fn assert_noninteractive_popup(window: HWND, element: &IUIAutomationElement) {
    // SAFETY: the caller supplies a live candidate popup HWND and this query
    // only reads its extended window style.
    let style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
    assert_ne!(
        style & WS_EX_NOACTIVATE.0 as isize,
        0,
        "popup must not activate"
    );
    assert_ne!(
        style & WS_EX_TRANSPARENT.0 as isize,
        0,
        "popup must be click-through"
    );
    assert_eq!(
        // SAFETY: synchronous message with no borrowed pointer arguments.
        unsafe { SendMessageW(window, WM_NCHITTEST, Some(WPARAM(0)), Some(LPARAM(0))) },
        LRESULT(HTTRANSPARENT as isize),
        "candidate popup must return HTTRANSPARENT"
    );
    assert!(
        // SAFETY: querying the UIA proxy has no caller-owned pointer arguments.
        !unsafe {
            element
                .CurrentIsKeyboardFocusable()
                .expect("UIA keyboard-focusable property")
        }
        .as_bool(),
        "UIA must expose the popup as non-focusable"
    );
    // SAFETY: foreground querying has no caller-owned pointer arguments.
    let foreground = unsafe { GetForegroundWindow() };
    assert_ne!(foreground, window, "popup must not become foreground");
}

fn assert_relation_groups(name: &str, mask: u8) {
    for (bit, label, term) in [
        (0_u8, "Aliases", "alias-term"),
        (1_u8, "Related", "related-term"),
        (2_u8, "Similar", "similar-term"),
        (3_u8, "Antonyms", "antonym-term"),
    ] {
        let expected = mask & (1_u8 << bit) != 0;
        assert_eq!(name.contains(label), expected, "{label}: {name:?}");
        assert_eq!(name.contains(term), expected, "{term}: {name:?}");
    }
}

fn state(
    revision: u64,
    selected: usize,
    candidate_detail: Option<CandidateDetail>,
    anchor: ScreenRect,
) -> UiState {
    UiState {
        revision,
        mode: None,
        candidates: Some(CandidateList {
            kind: CandidateKind::Suggestion,
            presentation: CandidatePresentation::Expanded,
            items: (0..18)
                .map(|index| Candidate {
                    text: format!("fixture-candidate-{index}"),
                    annotation: String::new(),
                })
                .collect(),
            selected: u16::try_from(selected).expect("fixture selected fits u16"),
            page_size: 9,
        }),
        candidate_detail,
        anchor: Some(anchor),
        renderer_visible: true,
        stopping: false,
    }
}

fn detail(definition: &str, definition_truncated: bool, mask: u8) -> CandidateDetail {
    let group = |bit: u8, term: &str| (mask & (1_u8 << bit) != 0).then(|| vec![term.to_owned()]);
    CandidateDetail {
        reading: "fixture-reading".to_owned(),
        definition: definition.to_owned(),
        definition_truncated,
        aliases: group(0, "alias-term").unwrap_or_default(),
        related: group(1, "related-term").unwrap_or_default(),
        similar: group(2, "similar-term").unwrap_or_default(),
        antonyms: group(3, "antonym-term").unwrap_or_default(),
    }
}

fn anchor(left: i32, top: i32) -> ScreenRect {
    ScreenRect {
        left,
        top,
        right: left + 20,
        bottom: top + 24,
    }
}

struct FixtureEngine {
    pipe_name: String,
    state: Arc<(Mutex<UiState>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl FixtureEngine {
    fn new(initial: UiState) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let pipe_name = format!("{TEST_PIPE_PREFIX}{}-{nonce}", std::process::id());
        let state = Arc::new((Mutex::new(initial), Condvar::new()));
        let server_state = Arc::clone(&state);
        // Claim the unique pipe before the renderer starts. A renderer which
        // races a not-yet-created fixture could otherwise enter its production
        // watchdog path and launch the adjacent engine binary.
        let security = Descriptor::for_pipe().expect("fixture pipe security descriptor");
        let pipe = PipeInstance::create(&pipe_name, &security, true).expect("create fixture pipe");
        let thread = thread::spawn(move || serve_fixture(pipe, server_state));
        Self {
            pipe_name,
            state,
            thread: Some(thread),
        }
    }

    fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    fn publish(&self, next: UiState) {
        let (state, changed) = &*self.state;
        let mut current = state.lock().expect("fixture state lock");
        assert!(
            next.revision > current.revision,
            "fixture revisions are monotonic"
        );
        *current = next;
        changed.notify_all();
    }

    fn stop(&mut self) {
        let (state, changed) = &*self.state;
        let mut current = state.lock().expect("fixture state lock");
        current.revision = current.revision.saturating_add(1);
        current.stopping = true;
        changed.notify_all();
        drop(current);
        // On an early assertion failure the renderer might never have opened
        // the pipe, leaving the owned server blocked in `wait_for_client`.
        // This bounded, exact-pipe connection wakes only our fixture and is
        // dropped immediately; a live renderer connection simply makes it
        // time out while the notified WatchUi request completes normally.
        let _ = Client::connect_to(&self.pipe_name, Duration::from_millis(100));
        self.thread
            .take()
            .expect("fixture server thread remains owned")
            .join()
            .expect("fixture server must finish");
    }
}

impl Drop for FixtureEngine {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop();
        }
    }
}

fn serve_fixture(pipe: PipeInstance, state: Arc<(Mutex<UiState>, Condvar)>) {
    pipe.wait_for_client().expect("accept renderer connection");
    let mut frame = Vec::new();
    loop {
        let payload = match pipe.read_frame(&mut frame) {
            Ok(payload) => payload,
            Err(sakura_ipc::Fault::Disconnected) => return,
            Err(error) => panic!("fixture read: {error:?}"),
        };
        let (id, request) = decode_request(payload).expect("decode renderer request");
        let response = match request {
            Request::Hello { client_version } => {
                assert_eq!(
                    client_version, PROTOCOL_VERSION,
                    "renderer protocol version"
                );
                Response::Hello {
                    server_version: PROTOCOL_VERSION,
                    engine_version: [0, 0, 0],
                }
            }
            Request::WatchUi { since } => Response::Ui(wait_for_fixture_state(&state, since)),
            other => panic!("fixture renderer sent unexpected request: {other:?}"),
        };
        frame.clear();
        encode_response(&response, id, &mut frame).expect("encode fixture response");
        pipe.write_all(&frame).expect("write fixture response");
        if matches!(response, Response::Ui(UiState { stopping: true, .. })) {
            return;
        }
    }
}

fn wait_for_fixture_state(state: &Arc<(Mutex<UiState>, Condvar)>, since: u64) -> UiState {
    let (state, changed) = &**state;
    let mut current = state.lock().expect("fixture state lock");
    while current.revision == since && !current.stopping {
        current = changed
            .wait(current)
            .expect("fixture state lock after wake");
    }
    current.clone()
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
        std::fs::create_dir_all(&path).expect("create isolated LOCALAPPDATA");
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

fn wait_for_candidate_window(renderer_pid: u32) -> HWND {
    let deadline = Instant::now() + PATIENT;
    loop {
        let mut after = None;
        loop {
            // SAFETY: the class name and null title are valid for this
            // synchronous top-level window enumeration step.
            let found = unsafe {
                FindWindowExW(
                    None,
                    after,
                    windows::core::w!("SakuraInputCandidates"),
                    PCWSTR::null(),
                )
            };
            let Ok(window) = found else {
                break;
            };
            after = Some(window);
            let mut owner_pid = 0;
            // SAFETY: `window` was returned by the immediately preceding
            // enumeration call and `owner_pid` is a valid out-pointer.
            unsafe { GetWindowThreadProcessId(window, Some(&mut owner_pid)) };
            // SAFETY: the discovered HWND remains valid for this immediate query.
            if owner_pid == renderer_pid && unsafe { IsWindowVisible(window) }.as_bool() {
                return window;
            }
        }
        assert!(
            Instant::now() < deadline,
            "candidate window did not become visible"
        );
        sleep(Duration::from_millis(20));
    }
}

fn wait_for_name(element: &IUIAutomationElement, needle: &str) -> String {
    let deadline = Instant::now() + PATIENT;
    let mut last = String::new();
    loop {
        // SAFETY: the retained UIA element proxy remains live during this bounded poll.
        if let Ok(name) = unsafe { element.CurrentName() } {
            let name = name.to_string();
            if name.contains(needle) {
                return name;
            }
            last = name;
        }
        assert!(
            Instant::now() < deadline,
            "UIA name never contained {needle:?}; last value was {last:?}"
        );
        sleep(Duration::from_millis(20));
    }
}

fn window_rect(window: HWND) -> windows::Win32::Foundation::RECT {
    let mut rect = windows::Win32::Foundation::RECT::default();
    // SAFETY: caller provides the live popup HWND and `rect` is a valid out-pointer.
    unsafe { GetWindowRect(window, &mut rect).expect("candidate popup rectangle") };
    rect
}

fn wait_for_moved_window(
    window: HWND,
    previous: windows::Win32::Foundation::RECT,
) -> windows::Win32::Foundation::RECT {
    let deadline = Instant::now() + PATIENT;
    loop {
        let current = window_rect(window);
        if current.left != previous.left || current.top != previous.top {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "candidate popup did not follow caret"
        );
        sleep(Duration::from_millis(20));
    }
}

struct ComApartment {
    owns_initialization: bool,
}

impl ComApartment {
    fn new() -> Self {
        // SAFETY: no pointer argument is supplied and the result is checked.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Self {
                owns_initialization: false,
            };
        }
        result.ok().expect("initialize COM");
        Self {
            owns_initialization: true,
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns_initialization {
            // SAFETY: balances the successful initialization in `new`.
            unsafe { CoUninitialize() };
        }
    }
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

    fn pid(&self) -> u32 {
        self.child.as_ref().expect("owned child remains live").id()
    }

    fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + PATIENT;
        loop {
            let child = self.child.as_mut().expect("owned child remains live");
            match child.try_wait().expect("query renderer exit") {
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
