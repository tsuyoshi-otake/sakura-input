//! Real-process candidate popup and UI Automation gate.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_ipc::Client;
use sakura_proto::{
    CandidateList, KeyCode, KeyInput, Modifiers, Output, Request, Response, ScreenRect,
    CANDIDATE_PAGE_SIZE,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, RECT, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, UIA_ListControlTypeId,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetForegroundWindow, GetWindowRect, IsWindowVisible, SendMessageW, WM_DPICHANGED,
};

const PATIENT: Duration = Duration::from_secs(5);
const MIN_WIDTH_96: i32 = 260;
const MAX_WIDTH_96: i32 = 480;
const ROW_HEIGHT_96: i32 = 28;
const FOOTER_HEIGHT_96: i32 = 22;
// The observable HWND bounds can include a DPI-scaled non-client border. Keep
// this allowance deliberately small so it cannot mask a logical layout drift.
const NON_CLIENT_ALLOWANCE_96: i32 = 8;

#[test]
#[ignore = "real release engine/renderer; set SAKURA_PHASE2_DICTIONARY"]
fn popup_follows_caret_pages_selects_by_digit_and_exposes_uia() {
    assert!(
        Client::connect(Duration::from_millis(200)).is_err(),
        "a logon-session engine is already running; stop it before this isolated test"
    );
    let dictionary = required_path("SAKURA_PHASE2_DICTIONARY");
    let app_data = IsolatedAppData::new("candidate-uia");
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

    let session = create_session(&mut client);
    for character in "kannji".chars() {
        send_key(&mut client, session, char_key(character));
    }
    let first = send_key(&mut client, session, named_key(KeyCode::Space));
    let first_candidates = first.candidates.expect("candidate list");
    assert!(
        first_candidates.items.len() > CANDIDATE_PAGE_SIZE,
        "the integration reading must exercise a second page"
    );
    assert_eq!(first_candidates.page_size, CANDIDATE_PAGE_SIZE as u16);

    set_placement(
        &mut client,
        session,
        ScreenRect {
            left: 100,
            top: 100,
            right: 120,
            bottom: 124,
        },
    );
    let candidate_window = wait_for_candidate_window();
    let first_rect = window_rect(candidate_window);
    assert_popup_geometry(
        candidate_window,
        first_rect,
        first_candidates.visible_range().len(),
    );

    let _apartment = ComApartment::new();
    // SAFETY: COM is initialized on this test thread and the requested class
    // is the system-provided in-process UI Automation client.
    let automation: IUIAutomation = unsafe {
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            .expect("create UI Automation client")
    };
    // SAFETY: the candidate HWND was discovered as a live visible window and
    // remains owned by the retained renderer process.
    let element = unsafe {
        automation
            .ElementFromHandle(candidate_window)
            .expect("candidate window UIA element")
    };
    let first_name = wait_for_name(&element, "page 1 of");
    assert!(first_name.contains("selected 1 of"));
    assert_visible_annotation_is_exposed(&first_name, &first_candidates);
    assert_eq!(
        // SAFETY: `element` is a live UI Automation element proxy.
        unsafe { element.CurrentAutomationId().expect("AutomationId") }.to_string(),
        "SakuraInputCandidates"
    );
    assert_eq!(
        // SAFETY: `element` is a live UI Automation element proxy.
        unsafe { element.CurrentClassName().expect("ClassName") }.to_string(),
        "SakuraInputCandidates"
    );
    assert_eq!(
        // SAFETY: `element` is a live UI Automation element proxy.
        unsafe { element.CurrentControlType().expect("ControlType") },
        UIA_ListControlTypeId
    );
    assert!(
        // SAFETY: `element` is a live UI Automation element proxy.
        !unsafe {
            element
                .CurrentIsKeyboardFocusable()
                .expect("IsKeyboardFocusable")
        }
        .as_bool(),
        "the no-activate popup must not claim keyboard focus"
    );

    // Exercise the exact message Windows sends while a live composition is
    // dragged between mixed-DPI monitors. The test-supplied suggested rectangle
    // doubles the 96-DPI dimensions; the popup must accept it without hiding,
    // changing its candidate state, or stealing host focus.
    // SAFETY: GetForegroundWindow has no pointer arguments or caller-owned
    // lifetime preconditions; it returns either the current HWND or null.
    let foreground_before_dpi = unsafe { GetForegroundWindow() };
    let dpi_rect = RECT {
        left: first_rect.left + 40,
        top: first_rect.top + 40,
        right: first_rect.left + 40 + (first_rect.right - first_rect.left) * 2,
        bottom: first_rect.top + 40 + (first_rect.bottom - first_rect.top) * 2,
    };
    let dpi_word = usize::from(192u16) | (usize::from(192u16) << 16);
    // SAFETY: the discovered HWND is live, and SendMessageW is synchronous, so
    // `dpi_rect` remains readable for the full WM_DPICHANGED handler.
    unsafe {
        SendMessageW(
            candidate_window,
            WM_DPICHANGED,
            Some(windows::Win32::Foundation::WPARAM(dpi_word)),
            Some(windows::Win32::Foundation::LPARAM(
                (&raw const dpi_rect) as isize,
            )),
        );
    }
    assert_eq!(window_rect(candidate_window), dpi_rect);
    // SAFETY: `candidate_window` was discovered above and is still owned by the
    // live renderer child process.
    assert!(unsafe { IsWindowVisible(candidate_window) }.as_bool());
    // SAFETY: GetForegroundWindow has no pointer arguments or caller-owned
    // lifetime preconditions.
    assert_eq!(unsafe { GetForegroundWindow() }, foreground_before_dpi);
    assert_eq!(wait_for_name(&element, "page 1 of"), first_name);
    assert_eq!(
        // SAFETY: `element` is a live UI Automation element proxy.
        unsafe {
            element
                .CurrentBoundingRectangle()
                .expect("mixed-DPI UIA rectangle")
        },
        dpi_rect,
        "UIA must publish the resized popup rectangle immediately"
    );

    set_placement(
        &mut client,
        session,
        ScreenRect {
            left: 400,
            top: 300,
            right: 420,
            bottom: 324,
        },
    );
    // WM_DPICHANGED already moved the HWND away from `first_rect`; waiting
    // relative to that stale rectangle would return before this placement is
    // processed and race the UIA property refresh.
    let second_rect = wait_for_moved_window(candidate_window, dpi_rect);
    // SAFETY: `element` is a live UI Automation element proxy.
    let uia_rect = unsafe {
        element
            .CurrentBoundingRectangle()
            .expect("UIA bounding rectangle")
    };
    assert_eq!(uia_rect, second_rect, "UIA and HWND rectangles must agree");

    let second = send_key(&mut client, session, named_key(KeyCode::PageDown));
    let second_candidates = second.candidates.expect("paged candidate list");
    assert_eq!(second_candidates.current_page(), 1);
    assert_eq!(usize::from(second_candidates.selected), CANDIDATE_PAGE_SIZE);
    let expected_commit = second_candidates.items[CANDIDATE_PAGE_SIZE + 1]
        .text
        .clone();
    let second_name = wait_for_name(&element, "page 2 of");
    assert!(second_name.contains("selected 10 of"));
    assert!(second_name.contains(&second_candidates.items[CANDIDATE_PAGE_SIZE].text));
    let second_page_rect =
        wait_for_popup_geometry(candidate_window, second_candidates.visible_range().len());
    assert_popup_geometry(
        candidate_window,
        second_page_rect,
        second_candidates.visible_range().len(),
    );
    assert_visible_annotation_is_exposed(&second_name, &second_candidates);
    assert_name_stable(&element, &second_name);

    let committed = send_key(&mut client, session, char_key('2'));
    assert_eq!(committed.commit.as_deref(), Some(expected_commit.as_str()));
    wait_until_hidden(candidate_window);
    assert!(
        // SAFETY: `element` remains a valid proxy after its provider window
        // becomes hidden; off-screen state is part of that provider contract.
        unsafe { element.CurrentIsOffscreen().expect("IsOffscreen") }.as_bool(),
        "hidden popup must be off-screen to UIA"
    );

    if let Some(report) = std::env::var_os("SAKURA_CANDIDATE_UIA_REPORT") {
        write_report(
            Path::new(&report),
            first_rect,
            dpi_rect,
            second_rect,
            &first_name,
            &second_name,
            &expected_commit,
        );
    }

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

fn create_session(client: &mut Client) -> u64 {
    match client.call(
        &Request::CreateSession {
            process_name: "phase2-candidate-uia.exe".to_owned(),
        },
        PATIENT,
    ) {
        Ok(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    }
}

fn send_key(client: &mut Client, session: u64, key: KeyInput) -> Output {
    match client.call(&Request::SendKey { session, key }, PATIENT) {
        Ok(Response::Output(output)) => output,
        other => panic!("expected Output, got {other:?}"),
    }
}

fn char_key(character: char) -> KeyInput {
    KeyInput {
        code: KeyCode::Char,
        ch: Some(character),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn named_key(code: KeyCode) -> KeyInput {
    KeyInput {
        code,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn set_placement(client: &mut Client, session: u64, anchor: ScreenRect) {
    assert!(matches!(
        client.call(
            &Request::SetUiPlacement {
                session,
                anchor: Some(anchor),
                renderer_visible: true,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));
}

fn wait_for_candidate_window() -> HWND {
    let deadline = Instant::now() + PATIENT;
    loop {
        // SAFETY: both class-name and optional title pointers are valid for
        // the duration of this synchronous lookup.
        let found =
            unsafe { FindWindowW(windows::core::w!("SakuraInputCandidates"), PCWSTR::null()) };
        if let Ok(window) = found {
            // SAFETY: `FindWindowW` returned this HWND in the immediately
            // preceding call; visibility querying does not retain it.
            if unsafe { IsWindowVisible(window) }.as_bool() {
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

fn window_rect(window: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: callers supply the live renderer HWND and the output pointer is
    // a valid local for the duration of the synchronous call.
    unsafe { GetWindowRect(window, &mut rect).expect("candidate window rectangle") };
    rect
}

fn wait_for_popup_geometry(window: HWND, visible_rows: usize) -> RECT {
    let deadline = Instant::now() + PATIENT;
    loop {
        let rect = window_rect(window);
        if popup_geometry_matches(window, rect, visible_rows) {
            return rect;
        }
        assert!(
            Instant::now() < deadline,
            "candidate popup geometry did not reach the redesigned logical layout; last rectangle was {rect:?}"
        );
        sleep(Duration::from_millis(20));
    }
}

fn assert_popup_geometry(window: HWND, rect: RECT, visible_rows: usize) {
    assert!(
        popup_geometry_matches(window, rect, visible_rows),
        "candidate popup geometry must use a 260–480 logical px content-aware width and {ROW_HEIGHT_96} logical px rows plus a {FOOTER_HEIGHT_96} logical px footer; got {rect:?} for {visible_rows} visible rows at {} DPI",
        window_dpi(window),
    );
}

fn popup_geometry_matches(window: HWND, rect: RECT, visible_rows: usize) -> bool {
    let dpi = window_dpi(window);
    let allowance = scaled(NON_CLIENT_ALLOWANCE_96, dpi);
    let width = rect.right.saturating_sub(rect.left);
    let minimum_width = scaled(MIN_WIDTH_96, dpi).saturating_sub(allowance);
    let maximum_width = scaled(MAX_WIDTH_96, dpi).saturating_add(allowance);
    let expected_height = scaled(ROW_HEIGHT_96, dpi)
        .saturating_mul(visible_rows as i32)
        .saturating_add(scaled(FOOTER_HEIGHT_96, dpi));
    let height = rect.bottom.saturating_sub(rect.top);

    (minimum_width..=maximum_width).contains(&width)
        && height.abs_diff(expected_height) <= allowance as u32
}

fn window_dpi(window: HWND) -> u32 {
    // SAFETY: callers provide the live candidate HWND and the function neither
    // retains it nor dereferences caller-owned memory.
    match unsafe { GetDpiForWindow(window) } {
        0 => 96,
        dpi => dpi,
    }
}

fn scaled(logical: i32, dpi: u32) -> i32 {
    logical.saturating_mul(dpi as i32) / 96
}

fn wait_for_moved_window(window: HWND, previous: RECT) -> RECT {
    let deadline = Instant::now() + PATIENT;
    loop {
        let current = window_rect(window);
        if current.left != previous.left || current.top != previous.top {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "candidate window did not follow caret"
        );
        sleep(Duration::from_millis(20));
    }
}

fn wait_for_name(element: &IUIAutomationElement, needle: &str) -> String {
    let deadline = Instant::now() + PATIENT;
    let mut last = String::new();
    loop {
        // SAFETY: `element` is retained by the caller and remains a live UI
        // Automation proxy throughout this bounded polling loop.
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

fn assert_visible_annotation_is_exposed(name: &str, candidates: &CandidateList) {
    let annotation = candidates.visible_range().find_map(|index| {
        let annotation = candidates.items[index].annotation.trim();
        (!annotation.is_empty()).then_some(annotation)
    });
    if let Some(annotation) = annotation {
        assert!(
            name.contains(annotation),
            "UIA name must expose the non-empty annotation of the visible fixture candidate: {annotation:?}; name was {name:?}"
        );
    }
}

fn assert_name_stable(element: &IUIAutomationElement, expected: &str) {
    let deadline = Instant::now() + Duration::from_millis(150);
    loop {
        // SAFETY: `element` is retained by the caller and remains a live UIA
        // Automation proxy during this short bounded stability observation.
        let actual = unsafe { element.CurrentName().expect("stable UIA name") }.to_string();
        assert_eq!(
            actual, expected,
            "the second-page UIA state changed without a corresponding engine update"
        );
        if Instant::now() >= deadline {
            return;
        }
        sleep(Duration::from_millis(20));
    }
}

fn wait_until_hidden(window: HWND) {
    let deadline = Instant::now() + PATIENT;
    // SAFETY: the renderer process remains retained by the caller while this
    // bounded loop queries the discovered candidate HWND.
    while unsafe { IsWindowVisible(window) }.as_bool() {
        assert!(Instant::now() < deadline, "candidate window did not hide");
        sleep(Duration::from_millis(20));
    }
}

struct ComApartment {
    owns_initialization: bool,
}

impl ComApartment {
    fn new() -> Self {
        // SAFETY: no reserved pointer is provided and the result is checked
        // before any COM interface is created on this thread.
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
            // SAFETY: balances exactly one successful `CoInitializeEx` made
            // by this guard on this test thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn write_report(
    path: &Path,
    before: RECT,
    mixed_dpi: RECT,
    after: RECT,
    first_name: &str,
    second_name: &str,
    committed: &str,
) {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).expect("create candidate UIA report directory");
    }
    let measured_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_secs();
    let report = format!(
        "{{\n  \"schema_version\": 2,\n  \"measured_at_unix\": {measured_at},\n  \
         \"before\": [{}, {}, {}, {}],\n  \"after\": [{}, {}, {}, {}],\n  \
         \"mixed_dpi\": [{}, {}, {}, {}],\n  \
         \"first_name\": {},\n  \"second_name\": {},\n  \"committed\": {},\n  \
         \"caret_followed\": true,\n  \"paging_passed\": true,\n  \
         \"digit_selection_passed\": true,\n  \"mixed_dpi_passed\": true,\n  \
         \"uia_passed\": true,\n  \"passed\": true\n}}\n",
        before.left,
        before.top,
        before.right,
        before.bottom,
        after.left,
        after.top,
        after.right,
        after.bottom,
        mixed_dpi.left,
        mixed_dpi.top,
        mixed_dpi.right,
        mixed_dpi.bottom,
        json_string(first_name),
        json_string(second_name),
        json_string(committed),
    );
    std::fs::write(path, report).expect("write candidate UIA report");
}

fn json_string(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() + 2);
    encoded.push('"');
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            control if control.is_control() => {
                use std::fmt::Write as _;
                write!(encoded, "\\u{:04x}", control as u32).expect("write JSON escape");
            }
            other => encoded.push(other),
        }
    }
    encoded.push('"');
    encoded
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
