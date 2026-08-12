#![cfg(all(windows, feature = "e2e-host"))]

//! Physical-key prediction navigation through a real Win32 TSF host.
//!
//! This test is deliberately ignored in the ordinary workspace run because it
//! moves foreground focus and requires the installed Sakura profile, engine,
//! renderer, and dictionary. Run it explicitly on an interactive desktop with:
//!
//! ```text
//! cargo test -p sakura-tsf --features e2e-host \
//!   --test prediction_navigation_user32 -- --ignored --nocapture
//! ```

use std::mem::size_of;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, RPC_E_CHANGED_MODE, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomationElement};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus, HKL};
use windows::Win32::UI::TextServices::{
    CLSID_TF_InputProcessorProfiles, ITfInputProcessorProfileMgr, GUID_TFCAT_TIP_KEYBOARD,
    TF_INPUTPROCESSORPROFILE, TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE, TF_IPPMF_ENABLEPROFILE,
    TF_IPPMF_FORSESSION, TF_PROFILETYPE_INPUTPROCESSOR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, FindWindowExW, FindWindowW, GetClassNameW, GetForegroundWindow, GetParent,
    GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, SendMessageW,
    SetForegroundWindow, WM_APP, WM_CLOSE,
};

const PATIENT: Duration = Duration::from_secs(8);
const INPUT_SETTLING: Duration = Duration::from_millis(250);
const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const VK_SHIFT: u16 = 0x10;
const VK_RETURN: u16 = 0x0D;
const VK_TAB: u16 = 0x09;
const VK_UP: u16 = 0x26;
const VK_DOWN: u16 = 0x28;
const SNAPSHOT_EDIT_TEXT: u32 = WM_APP + 37;

static PHYSICAL_E2E: Mutex<()> = Mutex::new(());

#[repr(C)]
#[derive(Clone, Copy)]
struct TestKeyboardInput {
    virtual_key: u16,
    scan_code: u16,
    flags: u32,
    time: u32,
    extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
union TestInputPayload {
    keyboard: TestKeyboardInput,
    padding: [usize; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TestInput {
    input_type: u32,
    payload: TestInputPayload,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SendInput(input_count: u32, inputs: *const TestInput, input_size: i32) -> u32;
}

#[test]
#[ignore = "moves foreground focus and requires the installed Sakura TSF/engine/renderer"]
fn physical_tab_and_arrows_move_prediction_selection_and_enter_commits() {
    let _serial = PHYSICAL_E2E
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        !candidate_window_visible(),
        "close any existing Sakura candidate popup before running the physical E2E"
    );

    let _apartment = ComApartment::new();
    let _profile = ActiveProfileGuard::activate_sakura();
    let child = Command::new(env!("CARGO_BIN_EXE_sakura_tsf_test_host"))
        .spawn()
        .expect("launch the dedicated Win32 TSF host");
    let mut host = OwnedHost::new(child);
    let host_window = wait_for_window(
        windows::core::w!("SakuraInputTsfTestHost"),
        windows::core::w!("Sakura Input TSF Test Host"),
    );
    host.set_window(host_window);
    let edit = wait_for_child_edit(host_window);
    force_foreground(host_window, edit);
    wait_for_foreground(host_window);

    // `kana` is a stable shipped-dictionary prefix with multiple prediction
    // candidates. These are real virtual-key events, not WM_KEYDOWN messages.
    for key in [b'K', b'A', b'N', b'A'] {
        press_key(host_window, u16::from(key));
    }

    let candidate_window = wait_for_candidate_window();
    // SAFETY: COM is initialized and this is the system UI Automation class.
    let automation: IUIAutomation = unsafe {
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            .expect("create UI Automation client")
    };
    // SAFETY: the candidate HWND is live and retained by the running renderer.
    let candidate_element = unsafe {
        automation
            .ElementFromHandle(candidate_window)
            .expect("candidate popup UIA element")
    };
    let initial = wait_for_selection(&candidate_element, 1);
    let total = selection_total(&initial);
    assert!(
        total >= 2,
        "test prefix must expose at least two candidates: {initial:?}"
    );
    let first_candidate = candidate_surface(&initial, 1, total);
    assert_edit_focus(host_window, edit);

    // The first Tab transfers logical focus to candidate zero. Because the
    // visible popup already publishes row one as selected, this step can be
    // visually unchanged. The second Tab must move to row two.
    press_key(host_window, VK_TAB);
    wait_for_selection(&candidate_element, 1);
    press_key(host_window, VK_TAB);
    wait_for_selection(&candidate_element, 2);

    let after_down = if total > 2 { 3 } else { 1 };
    press_key(host_window, VK_DOWN);
    wait_for_selection(&candidate_element, after_down);
    press_key(host_window, VK_UP);
    wait_for_selection(&candidate_element, 2);
    press_chord(host_window, VK_SHIFT, VK_TAB);
    wait_for_selection(&candidate_element, 1);

    press_key(host_window, VK_RETURN);
    wait_until_hidden(candidate_window);
    assert_eq!(
        window_text(edit),
        first_candidate,
        "Enter must retain the selected prediction as committed host text"
    );

    host.close(host_window);
}

#[test]
#[ignore = "moves foreground focus and requires the installed Sakura TSF/engine/renderer"]
fn physical_arrows_and_tab_navigate_conversion_and_enter_commits() {
    let _serial = PHYSICAL_E2E
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        !candidate_window_visible(),
        "close any existing Sakura candidate popup before running the physical E2E"
    );

    let _apartment = ComApartment::new();
    let _profile = ActiveProfileGuard::activate_sakura();
    let child = Command::new(env!("CARGO_BIN_EXE_sakura_tsf_test_host"))
        .spawn()
        .expect("launch the dedicated Win32 TSF host");
    let mut host = OwnedHost::new(child);
    let host_window = wait_for_window(
        windows::core::w!("SakuraInputTsfTestHost"),
        windows::core::w!("Sakura Input TSF Test Host"),
    );
    host.set_window(host_window);
    let edit = wait_for_child_edit(host_window);
    force_foreground(host_window, edit);
    wait_for_foreground(host_window);

    for key in [b'K', b'A', b'N', b'A'] {
        press_key(host_window, u16::from(key));
    }
    let suggestion_window = wait_for_candidate_window();
    press_key(host_window, 0x20); // VK_SPACE: enter ordinary dictionary conversion.

    let conversion_window = wait_for_candidate_window();
    assert_eq!(
        conversion_window, suggestion_window,
        "the renderer must update its owned popup instead of leaking another HWND"
    );
    // SAFETY: COM is initialized and this is the system UI Automation class.
    let automation: IUIAutomation = unsafe {
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            .expect("create UI Automation client")
    };
    // SAFETY: the candidate HWND is live and retained by the running renderer.
    let candidate_element = unsafe {
        automation
            .ElementFromHandle(conversion_window)
            .expect("conversion popup UIA element")
    };
    let initial = wait_for_candidate_kind(&candidate_element, "conversion candidates");
    let (initial_selected, total) = selection_metadata(&initial);
    assert!(
        total >= 2,
        "conversion fixture needs at least two candidates: {initial:?}"
    );

    let next_selected = initial_selected % total + 1;
    press_key(host_window, VK_DOWN);
    wait_for_selection(&candidate_element, next_selected);
    press_key(host_window, VK_UP);
    wait_for_selection(&candidate_element, initial_selected);

    // Under the installed MS-IME preset Tab expands the compact conversion
    // list. The ATOK preset maps Tab to CandidateNext; this acceptance host is
    // intentionally run against the active installed preset reported with the
    // test evidence.
    press_key(host_window, VK_TAB);
    let expansion_probe = if initial_selected == 1 { 2 } else { 1 };
    let expanded = wait_for_candidate_row(&candidate_element, expansion_probe, total);
    press_key(host_window, VK_DOWN);
    let selected = wait_for_selection(&candidate_element, next_selected);
    let committed = candidate_surface(
        if selected.contains(&format!("Candidate {next_selected} of {total}")) {
            &selected
        } else {
            &expanded
        },
        next_selected,
        total,
    );

    press_key(host_window, VK_RETURN);
    wait_until_hidden(conversion_window);
    assert_eq!(
        window_text(edit),
        committed,
        "Enter must retain the selected conversion as committed host text"
    );

    host.close(host_window);
}

fn press_key(host: HWND, virtual_key: u16) {
    send_keyboard(host, &[(virtual_key, false), (virtual_key, true)]);
}

fn press_chord(host: HWND, modifier: u16, key: u16) {
    send_keyboard(
        host,
        &[
            (modifier, false),
            (key, false),
            (key, true),
            (modifier, true),
        ],
    );
}

fn send_keyboard(host: HWND, events: &[(u16, bool)]) {
    // SAFETY: GetForegroundWindow has no pointer arguments. The explicit check
    // turns concurrent user interaction into a clear E2E precondition failure
    // instead of misattributing a key delivered to another application.
    assert_eq!(
        unsafe { GetForegroundWindow() },
        host,
        "foreground focus left the dedicated host before physical input"
    );
    let inputs: Vec<_> = events
        .iter()
        .map(|&(virtual_key, key_up)| TestInput {
            input_type: INPUT_KEYBOARD,
            payload: TestInputPayload {
                keyboard: TestKeyboardInput {
                    virtual_key,
                    scan_code: 0,
                    flags: if key_up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    extra_info: 0,
                },
            },
        })
        .collect();
    // SAFETY: `inputs` remains live for the synchronous call and follows the
    // platform INPUT/KEYBDINPUT C layout used by User32.
    let inserted = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<TestInput>() as i32,
        )
    };
    assert_eq!(
        inserted as usize,
        inputs.len(),
        "User32 must insert every physical keyboard record"
    );
    sleep(INPUT_SETTLING);
}

fn wait_for_window(class: PCWSTR, title: PCWSTR) -> HWND {
    let deadline = Instant::now() + PATIENT;
    loop {
        // SAFETY: both pointers have static lifetime.
        if let Ok(window) = unsafe { FindWindowW(class, title) } {
            return window;
        }
        assert!(
            Instant::now() < deadline,
            "dedicated TSF host did not start"
        );
        sleep(Duration::from_millis(20));
    }
}

fn wait_for_child_edit(parent: HWND) -> HWND {
    let deadline = Instant::now() + PATIENT;
    loop {
        // SAFETY: `parent` is live and the class pointer has static lifetime.
        if let Ok(edit) =
            unsafe { FindWindowExW(Some(parent), None, windows::core::w!("EDIT"), None) }
        {
            return edit;
        }
        assert!(Instant::now() < deadline, "host EDIT control did not start");
        sleep(Duration::from_millis(20));
    }
}

fn wait_for_foreground(window: HWND) {
    let deadline = Instant::now() + PATIENT;
    loop {
        // SAFETY: GetForegroundWindow has no caller-owned pointer arguments.
        if unsafe { GetForegroundWindow() } == window {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "dedicated host did not acquire foreground focus"
        );
        sleep(Duration::from_millis(20));
    }
}

fn force_foreground(window: HWND, edit: HWND) {
    // SAFETY: both HWNDs are live. The calls return thread identifiers and do
    // not retain either handle.
    let foreground = unsafe { GetForegroundWindow() };
    let foreground_thread = unsafe { GetWindowThreadProcessId(foreground, None) };
    let host_thread = unsafe { GetWindowThreadProcessId(window, None) };
    // SAFETY: this is the current test thread identifier.
    let current_thread = unsafe { GetCurrentThreadId() };
    assert_ne!(host_thread, 0, "dedicated host must have a GUI thread");

    let attach_foreground = foreground_thread != 0 && foreground_thread != current_thread;
    let attach_host = host_thread != current_thread;
    // SAFETY: attachments are paired below on every non-panicking path. No
    // caller-owned memory crosses the calls.
    unsafe {
        if attach_foreground {
            assert!(
                AttachThreadInput(current_thread, foreground_thread, true).as_bool(),
                "attach current input queue to the foreground thread"
            );
        }
        if attach_host {
            assert!(
                AttachThreadInput(current_thread, host_thread, true).as_bool(),
                "attach current input queue to the dedicated host"
            );
        }
        let _ = BringWindowToTop(window);
        let _ = SetForegroundWindow(window);
        let _ = SetFocus(Some(edit));
        let focused = GetFocus();
        if attach_host {
            let _ = AttachThreadInput(current_thread, host_thread, false);
        }
        if attach_foreground {
            let _ = AttachThreadInput(current_thread, foreground_thread, false);
        }
        assert_eq!(focused, edit, "dedicated EDIT must receive keyboard focus");
    }
}

fn assert_edit_focus(window: HWND, edit: HWND) {
    // Read the host thread's focus without changing it. Attaching input queues
    // lets GetFocus observe the other GUI thread; the attachment is always
    // released before the assertion.
    let host_thread = unsafe { GetWindowThreadProcessId(window, None) };
    let current_thread = unsafe { GetCurrentThreadId() };
    assert_ne!(host_thread, 0, "dedicated host must have a GUI thread");
    let attached = host_thread != current_thread;
    let focused = unsafe {
        if attached {
            assert!(
                AttachThreadInput(current_thread, host_thread, true).as_bool(),
                "attach current input queue to inspect host focus"
            );
        }
        let focused = GetFocus();
        if attached {
            let _ = AttachThreadInput(current_thread, host_thread, false);
        }
        focused
    };
    assert_eq!(
        focused,
        edit,
        "dedicated EDIT lost keyboard focus to {}",
        describe_window(focused)
    );
}

fn describe_window(window: HWND) -> String {
    let mut class = [0u16; 128];
    let mut title = [0u16; 256];
    let mut process_id = 0u32;
    let class_len = unsafe { GetClassNameW(window, &mut class) }.max(0) as usize;
    let title_len = unsafe { GetWindowTextW(window, &mut title) }.max(0) as usize;
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    format!(
        "HWND={window:?} class={:?} title={:?} pid={process_id}",
        String::from_utf16_lossy(&class[..class_len]),
        String::from_utf16_lossy(&title[..title_len])
    )
}

fn candidate_window_visible() -> bool {
    // SAFETY: the class pointer has static lifetime and no title is required.
    unsafe {
        FindWindowW(windows::core::w!("SakuraInputCandidates"), PCWSTR::null())
            .ok()
            .is_some_and(|window| IsWindowVisible(window).as_bool())
    }
}

fn wait_for_candidate_window() -> HWND {
    let deadline = Instant::now() + PATIENT;
    loop {
        // SAFETY: the class pointer has static lifetime and no title is required.
        if let Ok(window) =
            unsafe { FindWindowW(windows::core::w!("SakuraInputCandidates"), PCWSTR::null()) }
        {
            // SAFETY: `window` came from the immediately preceding lookup.
            if unsafe { IsWindowVisible(window) }.as_bool() {
                return window;
            }
        }
        assert!(
            Instant::now() < deadline,
            "prediction popup did not become visible; verify that Sakura Input, prediction, engine, renderer, and a dictionary are active"
        );
        sleep(Duration::from_millis(20));
    }
}

fn wait_for_selection(element: &IUIAutomationElement, selected: usize) -> String {
    let needle = format!("selected {selected} of");
    let deadline = Instant::now() + PATIENT;
    let mut last = String::new();
    loop {
        // SAFETY: the caller retains the live UIA element for this bounded poll.
        if let Ok(name) = unsafe { element.CurrentName() } {
            last = name.to_string();
            if last.contains(&needle) {
                return last;
            }
        }
        assert!(
            Instant::now() < deadline,
            "candidate selection never reached {selected}; last UIA name was {last:?}"
        );
        sleep(Duration::from_millis(20));
    }
}

fn wait_for_candidate_kind(element: &IUIAutomationElement, kind: &str) -> String {
    wait_for_uia_name(element, |name| name.contains(kind), kind)
}

fn wait_for_candidate_row(element: &IUIAutomationElement, index: usize, total: usize) -> String {
    let marker = format!("Candidate {index} of {total}");
    wait_for_uia_name(element, |name| name.contains(&marker), &marker)
}

fn wait_for_uia_name(
    element: &IUIAutomationElement,
    accept: impl Fn(&str) -> bool,
    expected: &str,
) -> String {
    let deadline = Instant::now() + PATIENT;
    let mut last = String::new();
    loop {
        // SAFETY: the caller retains the live UIA element for this bounded poll.
        if let Ok(name) = unsafe { element.CurrentName() } {
            last = name.to_string();
            if accept(&last) {
                return last;
            }
        }
        assert!(
            Instant::now() < deadline,
            "candidate UIA name never exposed {expected:?}; last name was {last:?}"
        );
        sleep(Duration::from_millis(20));
    }
}

fn selection_total(name: &str) -> usize {
    selection_metadata(name).1
}

fn selection_metadata(name: &str) -> (usize, usize) {
    let tail = name
        .split_once("selected ")
        .unwrap_or_else(|| panic!("UIA name lacks selection metadata: {name:?}"))
        .1;
    let (selected, tail) = tail
        .split_once(" of ")
        .unwrap_or_else(|| panic!("UIA selection metadata is malformed: {name:?}"));
    let selected = selected
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("UIA selected index is malformed: {name:?}"));
    let total = tail
        .split(|character: char| !character.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("UIA name has malformed selection total: {name:?}"));
    (selected, total)
}

fn candidate_surface(name: &str, index: usize, total: usize) -> String {
    let plain = format!("Candidate {index} of {total}: ");
    let selected = format!("Candidate {index} of {total} (selected): ");
    let tail = name
        .split_once(&plain)
        .or_else(|| name.split_once(&selected))
        .unwrap_or_else(|| panic!("UIA name lacks candidate {index}: {name:?}"))
        .1;
    let item = tail
        .split_once(". Candidate ")
        .map_or(tail, |(candidate, _)| candidate)
        .trim_end_matches('.');
    item.split_once(" — ")
        .map_or(item, |(surface, _)| surface)
        .to_owned()
}

#[test]
fn candidate_surface_reads_selected_and_annotated_uia_rows() {
    let name = "Sakura Input candidates, suggestion candidates, selected 1 of 2. \
        Candidate 1 of 2 (selected): かな — 履歴; removable. Candidate 2 of 2: かなり.";
    assert_eq!(candidate_surface(name, 1, 2), "かな");
    assert_eq!(candidate_surface(name, 2, 2), "かなり");
}

fn window_text(window: HWND) -> String {
    // SAFETY: `window` is the dedicated host's standard EDIT child.
    let parent = unsafe { GetParent(window) }.expect("dedicated EDIT parent");
    // SAFETY: this private test message carries no pointer. The host copies its
    // own EDIT text into its top-level caption before returning.
    unsafe {
        SendMessageW(parent, SNAPSHOT_EDIT_TEXT, None, None);
    }
    let mut buffer = vec![0u16; 2048];
    // SAFETY: top-level captions are readable across processes and `buffer`
    // remains writable for the call.
    let copied = unsafe { GetWindowTextW(parent, &mut buffer) } as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

fn wait_until_hidden(window: HWND) {
    let deadline = Instant::now() + PATIENT;
    // SAFETY: the renderer retains the HWND while it transitions to hidden.
    while unsafe { IsWindowVisible(window) }.as_bool() {
        assert!(
            Instant::now() < deadline,
            "candidate popup did not hide after Enter"
        );
        sleep(Duration::from_millis(20));
    }
}

struct ComApartment {
    owns_initialization: bool,
}

struct ActiveProfileGuard {
    manager: ITfInputProcessorProfileMgr,
    previous: TF_INPUTPROCESSORPROFILE,
}

impl ActiveProfileGuard {
    fn activate_sakura() -> Self {
        // SAFETY: COM is initialized by the preceding apartment guard and the
        // requested class is the system TSF profile manager.
        let manager: ITfInputProcessorProfileMgr = unsafe {
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)
                .expect("create TSF profile manager")
        };
        let mut previous = TF_INPUTPROCESSORPROFILE::default();
        // SAFETY: `previous` is valid writable storage and the category GUID is
        // a static Windows identifier.
        unsafe {
            manager
                .GetActiveProfile(&GUID_TFCAT_TIP_KEYBOARD, &mut previous)
                .expect("capture active keyboard profile");
            manager
                .ActivateProfile(
                    TF_PROFILETYPE_INPUTPROCESSOR,
                    sakura_reg::LANGID_JA_JP,
                    &sakura_reg::CLSID_SAKURA_TSF,
                    &sakura_reg::GUID_PROFILE_JA_JP,
                    HKL(std::ptr::null_mut()),
                    TF_IPPMF_FORSESSION
                        | TF_IPPMF_ENABLEPROFILE
                        | TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE,
                )
                .expect("activate Sakura profile for the E2E session");
        }
        sleep(Duration::from_millis(150));
        Self { manager, previous }
    }
}

impl Drop for ActiveProfileGuard {
    fn drop(&mut self) {
        // SAFETY: all values were returned by GetActiveProfile and the manager
        // remains alive until this call completes. Restoration is best-effort
        // because Drop cannot report an OS profile-manager failure.
        unsafe {
            let _ = self.manager.ActivateProfile(
                self.previous.dwProfileType,
                self.previous.langid,
                &self.previous.clsid,
                &self.previous.guidProfile,
                self.previous.hkl,
                TF_IPPMF_FORSESSION
                    | TF_IPPMF_ENABLEPROFILE
                    | TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE,
            );
        }
    }
}

impl ComApartment {
    fn new() -> Self {
        // SAFETY: no reserved pointer is supplied and the result is checked.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Self {
                owns_initialization: false,
            };
        }
        result.ok().expect("initialize COM for UI Automation");
        Self {
            owns_initialization: true,
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns_initialization {
            // SAFETY: balances the successful initialization on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

struct OwnedHost {
    child: Child,
    window: Option<HWND>,
    closed: bool,
}

impl OwnedHost {
    fn new(child: Child) -> Self {
        Self {
            child,
            window: None,
            closed: false,
        }
    }

    fn set_window(&mut self, window: HWND) {
        self.window = Some(window);
    }

    fn close(&mut self, window: HWND) {
        // SAFETY: the HWND belongs to the retained child; the asynchronous
        // message contains no borrowed pointers.
        unsafe {
            let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        let deadline = Instant::now() + PATIENT;
        loop {
            match self.child.try_wait().expect("poll dedicated host") {
                Some(status) => {
                    assert!(status.success(), "dedicated host exited with {status}");
                    self.closed = true;
                    return;
                }
                None if Instant::now() < deadline => sleep(Duration::from_millis(20)),
                None => panic!("dedicated host did not exit after WM_CLOSE"),
            }
        }
    }
}

impl Drop for OwnedHost {
    fn drop(&mut self) {
        if !self.closed {
            if let Some(window) = self.window {
                // SAFETY: the HWND was discovered from this retained child and
                // WM_CLOSE contains no borrowed data.
                unsafe {
                    let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    if self.child.try_wait().ok().flatten().is_some() {
                        self.closed = true;
                        return;
                    }
                    sleep(Duration::from_millis(20));
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
