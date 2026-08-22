//! Real-process UI Automation coverage for selected-candidate details.
//!
//! This fixture intentionally does not launch the product engine or open its
//! production pipe. The test process owns a uniquely named pipe and serves a
//! bounded sequence of `UiState` snapshots itself; the renderer is pointed at
//! that pipe with its explicit test-only command-line switch. This keeps the
//! test independent of the installed IME, a production dictionary, and the
//! user's LOCALAPPDATA while still exercising the built renderer process.

use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, sleep, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_ipc::{security::Descriptor, Client, PipeInstance};
use sakura_proto::types::CandidatePresentation;
use sakura_proto::{
    decode_request, encode_response, AppearanceTheme, Candidate, CandidateDetail, CandidateKind,
    CandidateList, Mode, Request, Response, ScreenRect, UiState, PROTOCOL_VERSION,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    COLORREF, HWND, LPARAM, LRESULT, POINT, RPC_E_CHANGED_MODE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomationElement};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowExW, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetCursorPos,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, GWL_EXSTYLE, GWL_STYLE, HTCLIENT, HWND_TOPMOST, MA_NOACTIVATE, MSG,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE, WM_CLOSE, WM_DESTROY,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const PATIENT: Duration = Duration::from_secs(5);
const TEST_PIPE_PREFIX: &str = r"\\.\pipe\SakuraInputRendererTest-";
const CANDIDATE_CLASS: PCWSTR = windows::core::w!("SakuraInputCandidates");
const INDICATOR_CLASS: PCWSTR = windows::core::w!("SakuraInputIndicator");
const DELETE_TARGETS_CLASS: PCWSTR = windows::core::w!("SakuraInputCandidateDeleteTargets");
const FOREIGN_TARGET_CLASS: PCWSTR = windows::core::w!("SakuraInputRendererRoutingTarget");

const INPUT_MOUSE: u32 = 0;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;

const FOREIGN_LEFT_DOWN_INCREMENT: usize = 1;
const FOREIGN_LEFT_UP_INCREMENT: usize = 1 << 8;
const FOREIGN_RIGHT_DOWN_INCREMENT: usize = 1 << 16;
const FOREIGN_RIGHT_UP_INCREMENT: usize = 1 << 24;
const INPUT_SETTLING: Duration = Duration::from_millis(250);
const GW_HWNDNEXT: u32 = 2;
const GW_HWNDPREV: u32 = 3;

#[test]
#[ignore = "real renderer process; requires an interactive Windows desktop"]
fn mode_indicator_is_horizontal_nonactivating_and_repaints_light_and_dark() {
    let app_data = IsolatedAppData::new("mode-indicator-ui");
    let mut initial = state_with_theme(1, AppearanceTheme::Light, 0, None, anchor(120, 120));
    initial.mode = Some(Mode::Hiragana);
    initial.candidates = None;
    initial.renderer_visible = false;
    let mut engine = FixtureEngine::new(initial);
    let renderer_path = PathBuf::from(env!("CARGO_BIN_EXE_sakura_renderer"));
    let renderer = Command::new(&renderer_path)
        .arg("--test-pipe")
        .arg(engine.pipe_name())
        .env("LOCALAPPDATA", app_data.path())
        .spawn()
        .expect("spawn test-owned renderer");
    let mut renderer = OwnedChild::new(renderer, "renderer");
    let indicator = wait_for_renderer_window(renderer.pid(), INDICATOR_CLASS);

    // SAFETY: `indicator` is a live HWND owned by the renderer child.
    let dpi = unsafe { GetDpiForWindow(indicator) };
    let rect = window_rect(indicator);
    assert_eq!(rect.right - rect.left, scaled_logical_px(220, dpi));
    assert_eq!(rect.bottom - rect.top, scaled_logical_px(28, dpi));
    // SAFETY: `indicator` remains live for this immediate style query.
    let ex_style = unsafe { GetWindowLongPtrW(indicator, GWL_EXSTYLE) } as u32;
    for required in [WS_EX_NOACTIVATE.0, WS_EX_TOOLWINDOW.0, WS_EX_TOPMOST.0] {
        assert_ne!(ex_style & required, 0, "missing ex-style 0x{required:08x}");
    }
    let hiragana_code = Mode::Hiragana as isize + 1;
    wait_for_indicator_state(indicator, hiragana_code | (2 << 8));
    assert_eq!(
        wait_for_surface_color(indicator, COLORREF(0x00F4F6F7)),
        COLORREF(0x00F4F6F7)
    );

    let mut dark = state_with_theme(2, AppearanceTheme::Dark, 0, None, anchor(120, 120));
    dark.mode = Some(Mode::Hiragana);
    dark.candidates = None;
    dark.renderer_visible = false;
    engine.publish(dark);
    wait_for_indicator_state(indicator, hiragana_code | (3 << 8));
    assert_eq!(
        wait_for_surface_color(indicator, COLORREF(0x00353535)),
        COLORREF(0x00353535)
    );

    engine.stop();
    renderer.wait_for_exit();
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TestMouseInput {
    dx: i32,
    dy: i32,
    mouse_data: u32,
    flags: u32,
    time: u32,
    extra_info: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
union TestInputPayload {
    mouse: TestMouseInput,
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
    #[link_name = "IsWindowEnabled"]
    fn test_is_window_enabled(window: HWND) -> i32;
    #[link_name = "GetFocus"]
    fn test_get_focus() -> HWND;
    #[link_name = "GetCapture"]
    fn test_get_capture() -> HWND;
    #[link_name = "WindowFromPoint"]
    fn test_window_from_point(point: POINT) -> HWND;
    #[link_name = "GetWindow"]
    fn test_get_window(window: HWND, command: u32) -> HWND;
}

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

/// A visible candidate popup must repaint from the newest UiState theme; it
/// must not require a hide/show cycle or a new candidate list to replace an
/// already painted dark frame.
///
/// This is intentionally a real renderer-process test because the regression
/// boundary is the watch-pipe update, main-thread invalidation, and GDI paint
/// sequence together. Invocation requires an interactive Windows desktop:
///
/// `cargo test -p sakura-renderer --test candidate_detail_uia appearance_switch_repaints_a_visible_candidate_popup -- --ignored`
#[test]
#[ignore = "real renderer process; requires an interactive Windows desktop"]
fn appearance_switch_repaints_a_visible_candidate_popup() {
    let app_data = IsolatedAppData::new("candidate-appearance-switch");
    let anchor = anchor(120, 120);
    let mut engine =
        FixtureEngine::new(state_with_theme(1, AppearanceTheme::Dark, 0, None, anchor));
    let renderer_path = PathBuf::from(env!("CARGO_BIN_EXE_sakura_renderer"));
    let renderer = Command::new(&renderer_path)
        .arg("--test-pipe")
        .arg(engine.pipe_name())
        .env("LOCALAPPDATA", app_data.path())
        .spawn()
        .expect("spawn test-owned renderer");
    let mut renderer = OwnedChild::new(renderer, "renderer");
    println!(
        "candidate appearance switch fixture: pipe={} renderer_pid={} LOCALAPPDATA={}",
        engine.pipe_name(),
        renderer.pid(),
        app_data.path().display()
    );

    let popup = wait_for_candidate_window(renderer.pid());
    assert_eq!(
        wait_for_surface_color(popup, COLORREF(0x0025_2525)),
        COLORREF(0x0025_2525),
        "the initial dark candidate frame must be painted before the switch"
    );

    engine.publish(state_with_theme(2, AppearanceTheme::Light, 0, None, anchor));
    assert_eq!(
        wait_for_surface_color(popup, COLORREF(0x00E2_E5E8)),
        COLORREF(0x00E2_E5E8),
        "the existing popup must repaint with the light selected-row surface"
    );
    assert!(
        // SAFETY: `popup` is still owned by the test renderer process.
        unsafe { IsWindowVisible(popup) }.as_bool(),
        "an appearance-only update must keep the candidate popup visible"
    );

    engine.stop();
    renderer.wait_for_exit();
}

#[test]
#[ignore = "real renderer process; requires an interactive Windows desktop"]
fn history_delete_uses_typed_index_keeps_popup_until_next_ui_state_and_never_activates() {
    let app_data = IsolatedAppData::new("candidate-history-delete-uia");
    let mut engine = FixtureEngine::new(history_state(41, anchor(120, 120)));
    let renderer_path = PathBuf::from(env!("CARGO_BIN_EXE_sakura_renderer"));
    let renderer = Command::new(&renderer_path)
        .arg("--test-pipe")
        .arg(engine.pipe_name())
        .env("LOCALAPPDATA", app_data.path())
        .spawn()
        .expect("spawn test-owned renderer");
    let mut renderer = OwnedChild::new(renderer, "renderer");
    let windows = wait_for_history_delete_windows(renderer.pid());
    let popup = windows.base;
    let overlay = windows.overlay;
    let _apartment = ComApartment::new();
    // SAFETY: `popup` is the visible candidate HWND belonging to our child.
    let automation: IUIAutomation = unsafe {
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            .expect("create UI Automation client")
    };
    // SAFETY: the live popup can be represented by UI Automation.
    let element = unsafe {
        automation
            .ElementFromHandle(popup)
            .expect("candidate popup UIA element")
    };
    let name = wait_for_name(&element, "learned-history deletion is available");
    assert!(name.contains("fixture-history"));
    assert_noninteractive_popup(popup, &element);
    assert_delete_overlay(popup, overlay);

    let rect = window_rect(popup);
    // The 12 logical-px trash glyph is centered inside its 24 logical-px hit
    // target at the right edge of the 28 logical-px row. Derive the target center
    // from the live popup rather than relying on the former left-gutter point.
    // SAFETY: `popup` is live and belongs to the renderer child spawned above.
    let dpi = unsafe { GetDpiForWindow(popup) };
    let right_padding = scaled_logical_px(8, dpi);
    let hit_target_half_width = scaled_logical_px(24, dpi) / 2;
    let first_row_center = scaled_logical_px(28, dpi) / 2;
    let delete_target = POINT {
        x: (rect.right - rect.left).saturating_sub(right_padding + hit_target_half_width),
        y: first_row_center,
    };
    let screen_delete_target = POINT {
        x: rect.left + delete_target.x,
        y: rect.top + delete_target.y,
    };
    assert_eq!(
        // SAFETY: the LPARAM packs the screen position for WM_NCHITTEST.
        unsafe {
            SendMessageW(
                overlay,
                WM_NCHITTEST,
                Some(WPARAM(0)),
                Some(pack_point(screen_delete_target)),
            )
        },
        LRESULT(HTCLIENT as isize),
        "the right-edge delete overlay target becomes a client hit"
    );
    assert_eq!(
        // SAFETY: the row text coordinate is not a button hit target.
        unsafe {
            SendMessageW(
                popup,
                WM_NCHITTEST,
                Some(WPARAM(0)),
                Some(pack_point(POINT {
                    x: rect.left + scaled_logical_px(80, dpi),
                    y: rect.top + first_row_center,
                })),
            )
        },
        LRESULT(HTCLIENT as isize),
        "the disabled base retains its default client hit result; native disabled-window routing is proven below"
    );
    let old_left_gutter = POINT {
        // The same 24 logical px hit target would have been centered against
        // the left padding before the control moved to the row's right edge.
        x: scaled_logical_px(8, dpi) + hit_target_half_width,
        y: first_row_center,
    };
    assert_eq!(
        // SAFETY: the LPARAM packs the screen position for WM_NCHITTEST.
        unsafe {
            SendMessageW(
                popup,
                WM_NCHITTEST,
                Some(WPARAM(0)),
                Some(pack_point(POINT {
                    x: rect.left + old_left_gutter.x,
                    y: rect.top + old_left_gutter.y,
                })),
            )
        },
        LRESULT(HTCLIENT as isize),
        "the former left-gutter delete location must remain an ordinary disabled-base client point"
    );
    assert_eq!(
        // SAFETY: popup messages use no borrowed pointers.
        unsafe { SendMessageW(overlay, WM_MOUSEACTIVATE, Some(WPARAM(0)), Some(LPARAM(0))) },
        LRESULT(MA_NOACTIVATE as isize),
        "the delete overlay must not activate the popup"
    );

    // The target uses another test-owned UI thread and a PID distinct from the
    // renderer. Its counters prove that the visible disabled base swallows
    // non-target physical User32 input rather than accidentally forwarding it.
    let foreign_target = ForeignClickTarget::new(rect);
    assert_distinct_renderer_and_foreign_owners(popup, renderer.pid(), &foreign_target);
    place_target_beneath_renderer_windows(foreign_target.window(), popup, overlay);
    let _foreground = ForegroundRestore::new();
    let cursor = CursorRestore::new();
    assert_renderer_noninteractive(popup, overlay);

    let row_text = POINT {
        x: rect.left + scaled_logical_px(80, dpi),
        y: rect.top + first_row_center,
    };
    let non_deletable_row = POINT {
        x: rect.left + delete_target.x,
        y: rect.top + scaled_logical_px(28, dpi) + first_row_center,
    };
    let screen_old_left_gutter = POINT {
        x: rect.left + old_left_gutter.x,
        y: rect.top + old_left_gutter.y,
    };
    assert_base_uia_at_point(&automation, row_text);
    assert_base_uia_at_point(&automation, screen_delete_target);
    assert_base_uia_at_point(&automation, non_deletable_row);

    let _ = cursor.click_at(row_text);
    assert_swallowed_non_target_input(&foreign_target, &engine, popup, overlay);

    let _ = cursor.right_click_at(row_text);
    assert_swallowed_non_target_input(&foreign_target, &engine, popup, overlay);

    let _ = cursor.click_at(screen_old_left_gutter);
    assert_swallowed_non_target_input(&foreign_target, &engine, popup, overlay);

    let _ = cursor.click_at(non_deletable_row);
    assert_swallowed_non_target_input(&foreign_target, &engine, popup, overlay);

    let target_input = cursor.click_at(screen_delete_target);
    assert_eq!(
        engine.wait_for_delete_with_diagnostics(|| {
            pointer_routing_diagnostics(
                popup,
                overlay,
                foreign_target.window(),
                screen_delete_target,
                target_input,
            )
        }),
        (41, 0)
    );
    foreign_target.assert_no_mouse_messages_for(INPUT_SETTLING);
    engine.assert_exactly_one_delete_for((41, 0), INPUT_SETTLING);
    assert_renderer_noninteractive(popup, overlay);
    assert!(
        // SAFETY: `popup` is the live window owned by this test process.
        unsafe { IsWindowVisible(popup) }.as_bool(),
        "a delete response must not optimistically hide the candidate"
    );
    assert!(
        // SAFETY: `element` is the live UIA object obtained for `popup`.
        unsafe { element.CurrentName() }
            .expect("read UIA name after delete response")
            .to_string()
            .contains("fixture-history"),
        "renderer must wait for a newer UiState before changing the popup"
    );

    engine.publish(UiState {
        revision: 42,
        appearance_theme: AppearanceTheme::Auto,
        mode: None,
        candidates: None,
        candidate_detail: None,
        anchor: None,
        document: None,
        renderer_visible: false,
        stopping: false,
    });
    wait_for_hidden_window(popup);
    wait_for_hidden_window(overlay);
    drop(cursor);
    drop(foreign_target);
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
    assert_eq!(
        style & WS_EX_TRANSPARENT.0 as isize,
        0,
        "disabled base popup must not depend on WS_EX_TRANSPARENT"
    );
    assert!(
        // SAFETY: the caller supplies a live candidate base HWND.
        unsafe { test_is_window_enabled(window) } == 0,
        "base popup must be natively disabled for OS pointer pass-through"
    );
    assert!(
        // SAFETY: querying the UIA proxy has no caller-owned pointer arguments.
        unsafe {
            element
                .CurrentIsEnabled()
                .expect("UIA enabled property for disabled native base")
        }
        .as_bool(),
        "base UIA provider must remain enabled despite native WS_DISABLED"
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

fn assert_base_uia_at_point(automation: &IUIAutomation, point: POINT) {
    // SAFETY: point is in the test-owned candidate popup and the automation
    // client remains live for this bounded query.
    let element = unsafe {
        automation
            .ElementFromPoint(point)
            .expect("UIA element at candidate pointer target")
    };
    // SAFETY: querying the UIA proxy has no caller-owned pointer arguments.
    let name = unsafe { element.CurrentName() }
        .expect("UIA name at candidate pointer target")
        .to_string();
    assert!(
        name.contains("fixture-history") && name.contains("learned-history deletion is available"),
        "pointer target must expose the base candidate provider, not an unlabeled overlay: {name:?}"
    );
    assert!(
        // SAFETY: querying the UIA proxy has no caller-owned pointer arguments.
        unsafe {
            element
                .CurrentIsEnabled()
                .expect("UIA enabled property at candidate pointer target")
        }
        .as_bool(),
        "base UIA provider remains enabled at both candidate pointer targets"
    );
    assert!(
        // SAFETY: querying the UIA proxy has no caller-owned pointer arguments.
        !unsafe {
            element
                .CurrentIsKeyboardFocusable()
                .expect("UIA focusable property at candidate pointer target")
        }
        .as_bool(),
        "delete overlay must not surface as a focusable UIA control"
    );
}

fn assert_delete_overlay(base: HWND, overlay: HWND) {
    // SAFETY: both HWNDs were enumerated as visible children of the renderer
    // process and remain live for these immediate style and geometry reads.
    let style = unsafe { GetWindowLongPtrW(overlay, GWL_EXSTYLE) };
    assert_ne!(
        style & WS_EX_NOACTIVATE.0 as isize,
        0,
        "delete overlay must not activate"
    );
    assert_eq!(
        style & WS_EX_TRANSPARENT.0 as isize,
        0,
        "delete overlay must receive pointer input inside its region"
    );
    assert!(
        // SAFETY: the caller supplies a live delete overlay HWND.
        unsafe { test_is_window_enabled(overlay) } != 0,
        "delete overlay must be natively enabled for pointer delivery"
    );
    assert_eq!(
        window_rect(overlay),
        window_rect(base),
        "delete overlay must share the base popup's screen coordinates"
    );
}

fn assert_renderer_not_foreground(base: HWND, overlay: HWND) {
    // SAFETY: foreground querying has no caller-owned pointer arguments.
    let foreground = unsafe { GetForegroundWindow() };
    assert_ne!(foreground, base, "base popup must not become foreground");
    assert_ne!(
        foreground, overlay,
        "delete overlay must not become foreground"
    );
}

fn assert_renderer_noninteractive(base: HWND, overlay: HWND) {
    assert_renderer_not_foreground(base, overlay);
    // SAFETY: focus and capture queries have no caller-owned pointer arguments.
    let focus = unsafe { test_get_focus() };
    // SAFETY: focus and capture queries have no caller-owned pointer arguments.
    let capture = unsafe { test_get_capture() };
    assert_ne!(focus, base, "base popup must not receive keyboard focus");
    assert_ne!(
        focus, overlay,
        "delete overlay must not receive keyboard focus"
    );
    assert_ne!(capture, base, "base popup must not capture the pointer");
    assert_ne!(
        capture, overlay,
        "delete overlay must not capture the pointer"
    );
}

fn assert_swallowed_non_target_input(
    foreign_target: &ForeignClickTarget,
    engine: &FixtureEngine,
    base: HWND,
    overlay: HWND,
) {
    foreign_target.assert_no_mouse_messages_for(INPUT_SETTLING);
    engine.assert_no_delete_for(INPUT_SETTLING);
    assert_renderer_noninteractive(base, overlay);
}

fn assert_distinct_renderer_and_foreign_owners(
    renderer_window: HWND,
    renderer_pid: u32,
    foreign_target: &ForeignClickTarget,
) {
    let mut foreign_pid = 0;
    // SAFETY: both HWNDs are live and these calls write only the supplied
    // process-ID out-pointers.
    let foreign_thread =
        unsafe { GetWindowThreadProcessId(foreign_target.window(), Some(&mut foreign_pid)) };
    let mut actual_renderer_pid = 0;
    // SAFETY: `renderer_window` is a live renderer HWND and the out-pointer is valid.
    let renderer_thread =
        unsafe { GetWindowThreadProcessId(renderer_window, Some(&mut actual_renderer_pid)) };
    assert_eq!(
        actual_renderer_pid, renderer_pid,
        "base must belong to the spawned renderer"
    );
    assert_eq!(
        foreign_pid,
        std::process::id(),
        "foreign target must belong to the test process rather than the renderer"
    );
    assert_ne!(
        foreign_pid, renderer_pid,
        "foreign target must use a distinct PID"
    );
    assert_ne!(
        foreign_thread, renderer_thread,
        "foreign target must use a distinct UI thread"
    );
}

fn pointer_routing_diagnostics(
    base: HWND,
    overlay: HWND,
    foreign: HWND,
    target: POINT,
    input: InputDelivery,
) -> String {
    let mut cursor = POINT::default();
    // SAFETY: `cursor` is a valid output pointer for the desktop cursor state.
    let cursor_result = unsafe { GetCursorPos(&mut cursor) };
    // SAFETY: these User32 queries take no caller-owned pointers and all three
    // windows are still owned by the bounded test at failure reporting time.
    let foreground = unsafe { GetForegroundWindow() };
    // SAFETY: focus and capture queries take no caller-owned pointer arguments.
    let focus = unsafe { test_get_focus() };
    // SAFETY: focus and capture queries take no caller-owned pointer arguments.
    let capture = unsafe { test_get_capture() };
    // SAFETY: POINT is passed by value to the User32 hit-test lookup.
    let point_window = unsafe { test_window_from_point(target) };
    // SAFETY: GetWindow only reads the current z-order links for live HWNDs.
    let overlay_previous = unsafe { test_get_window(overlay, GW_HWNDPREV) };
    // SAFETY: GetWindow only reads the current z-order links for live HWNDs.
    let overlay_next = unsafe { test_get_window(overlay, GW_HWNDNEXT) };
    format!(
        "target=({}, {}); input=[point=({}, {}),cursor_before=({}, {}),cursor_after=({}, {}),inserted={}]; cursor_result={cursor_result:?}; \
         cursor_after_wait=({}, {}); foreground={foreground:?}; focus={focus:?}; capture={capture:?}; \
         WindowFromPoint={point_window:?}; overlay_hit={:?}; base_hit={:?}; \
         overlay_prev={overlay_previous:?}; overlay_next={overlay_next:?}; {}; {}; {}",
        target.x,
        target.y,
        input.point.x,
        input.point.y,
        input.cursor_before_input.x,
        input.cursor_before_input.y,
        input.cursor_after_input.x,
        input.cursor_after_input.y,
        input.inserted,
        cursor.x,
        cursor.y,
        // SAFETY: the LPARAM packs the immediate diagnostic screen position.
        unsafe {
            SendMessageW(
                overlay,
                WM_NCHITTEST,
                Some(WPARAM(0)),
                Some(pack_point(target)),
            )
        },
        // SAFETY: the LPARAM packs the immediate diagnostic screen position.
        unsafe {
            SendMessageW(
                base,
                WM_NCHITTEST,
                Some(WPARAM(0)),
                Some(pack_point(target)),
            )
        },
        hwnd_diagnostics("base", base),
        hwnd_diagnostics("overlay", overlay),
        hwnd_diagnostics("foreign", foreign),
    )
}

fn hwnd_diagnostics(label: &str, window: HWND) -> String {
    let mut pid = 0;
    // SAFETY: `window` is live and `pid` is a valid out-pointer.
    let thread_id = unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    // SAFETY: these immediate style and visibility queries only read the live HWND.
    let style = unsafe { GetWindowLongPtrW(window, GWL_STYLE) };
    // SAFETY: these immediate style and visibility queries only read the live HWND.
    let ex_style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
    // SAFETY: these immediate style and visibility queries only read the live HWND.
    let visible = unsafe { IsWindowVisible(window) }.as_bool();
    // SAFETY: this immediate state query only reads the live HWND.
    let enabled = unsafe { test_is_window_enabled(window) } != 0;
    let rect = window_rect(window);
    format!(
        "{label}={window:?}[pid={pid},tid={thread_id},visible={visible},enabled={enabled}, \
         style=0x{style:08x},ex=0x{ex_style:08x},rect=({}, {}, {}, {})]",
        rect.left, rect.top, rect.right, rect.bottom
    )
}

fn scaled_logical_px(value: i32, dpi: u32) -> i32 {
    value.saturating_mul(dpi as i32).saturating_add(48) / 96
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
    state_with_theme(
        revision,
        AppearanceTheme::Dark,
        selected,
        candidate_detail,
        anchor,
    )
}

fn state_with_theme(
    revision: u64,
    appearance_theme: AppearanceTheme,
    selected: usize,
    candidate_detail: Option<CandidateDetail>,
    anchor: ScreenRect,
) -> UiState {
    UiState {
        revision,
        appearance_theme,
        mode: None,
        candidates: Some(CandidateList {
            kind: CandidateKind::Suggestion,
            presentation: CandidatePresentation::Expanded,
            items: (0..18)
                .map(|index| Candidate {
                    text: format!("fixture-candidate-{index}"),
                    annotation: String::new(),
                    deletable_history: false,
                })
                .collect(),
            selected: u16::try_from(selected).expect("fixture selected fits u16"),
            page_size: 9,
        }),
        candidate_detail,
        anchor: Some(anchor),
        document: None,
        renderer_visible: true,
        stopping: false,
    }
}

fn history_state(revision: u64, anchor: ScreenRect) -> UiState {
    UiState {
        revision,
        appearance_theme: AppearanceTheme::Dark,
        mode: None,
        candidates: Some(CandidateList {
            kind: CandidateKind::Suggestion,
            presentation: CandidatePresentation::Expanded,
            items: vec![
                Candidate {
                    text: "fixture-history".to_owned(),
                    annotation: "not-a-marker".to_owned(),
                    deletable_history: true,
                },
                Candidate {
                    text: "fixture-non-deletable".to_owned(),
                    annotation: "not-a-marker".to_owned(),
                    deletable_history: false,
                },
            ],
            selected: 0,
            page_size: 9,
        }),
        candidate_detail: None,
        anchor: Some(anchor),
        document: None,
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

type DeletedRequests = Arc<(Mutex<Vec<(u64, u16)>>, Condvar)>;

struct FixtureEngine {
    pipe_name: String,
    state: Arc<(Mutex<UiState>, Condvar)>,
    deleted: DeletedRequests,
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
        let pipe =
            PipeInstance::create(&pipe_name, &security, true).expect("create first fixture pipe");
        let delete_pipe =
            PipeInstance::create(&pipe_name, &security, false).expect("create delete fixture pipe");
        let candidate_pipe = PipeInstance::create(&pipe_name, &security, false)
            .expect("create candidate commit fixture pipe");
        let deleted = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let server_deleted = Arc::clone(&deleted);
        let thread = thread::spawn(move || {
            serve_fixture(
                pipe,
                delete_pipe,
                candidate_pipe,
                server_state,
                server_deleted,
            )
        });
        Self {
            pipe_name,
            state,
            deleted,
            thread: Some(thread),
        }
    }

    fn wait_for_delete_with_diagnostics<F>(&self, diagnostics: F) -> (u64, u16)
    where
        F: FnOnce() -> String,
    {
        let (deleted, changed) = &*self.deleted;
        let deadline = Instant::now() + PATIENT;
        let mut requests = deleted.lock().expect("fixture delete lock");
        while requests.is_empty() {
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                panic!(
                    "renderer never issued history delete request; {}",
                    diagnostics()
                );
            }
            let (next, _) = changed
                .wait_timeout(requests, wait)
                .expect("fixture delete lock after wake");
            requests = next;
        }
        assert_eq!(
            requests.len(),
            1,
            "renderer must not duplicate one delete click"
        );
        requests[0]
    }

    fn assert_no_delete_for(&self, duration: Duration) {
        let (deleted, changed) = &*self.deleted;
        let requests = deleted.lock().expect("fixture delete lock");
        let (requests, _) = changed
            .wait_timeout_while(requests, duration, |requests| requests.is_empty())
            .expect("fixture delete condition wait");
        assert!(
            requests.is_empty(),
            "a non-target pointer click must not issue a history delete request"
        );
    }

    fn assert_exactly_one_delete_for(&self, expected: (u64, u16), duration: Duration) {
        let (deleted, changed) = &*self.deleted;
        let requests = deleted.lock().expect("fixture delete lock");
        let (requests, _) = changed
            .wait_timeout_while(requests, duration, |requests| requests.len() == 1)
            .expect("fixture delete condition wait");
        assert_eq!(
            requests.as_slice(),
            [expected],
            "target click must produce exactly one typed history delete request"
        );
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
        let _ = Client::connect_to(&self.pipe_name, Duration::from_millis(100));
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

fn serve_fixture(
    pipe: PipeInstance,
    delete_pipe: PipeInstance,
    candidate_pipe: PipeInstance,
    state: Arc<(Mutex<UiState>, Condvar)>,
    deleted: DeletedRequests,
) {
    let watch_state = Arc::clone(&state);
    let watch_deleted = Arc::clone(&deleted);
    let watch = thread::spawn(move || serve_fixture_connection(pipe, watch_state, watch_deleted));
    let command_state = Arc::clone(&state);
    let command_deleted = Arc::clone(&deleted);
    let command = thread::spawn(move || {
        serve_fixture_connection(delete_pipe, command_state, command_deleted)
    });
    serve_fixture_connection(candidate_pipe, state, deleted);
    command.join().expect("first command fixture must finish");
    watch.join().expect("watch fixture connection must finish");
}

fn serve_fixture_connection(
    pipe: PipeInstance,
    state: Arc<(Mutex<UiState>, Condvar)>,
    deleted: DeletedRequests,
) {
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
            Request::DeleteHistoryCandidate {
                revision,
                candidate_index,
            } => {
                let (requests, changed) = &*deleted;
                requests
                    .lock()
                    .expect("fixture delete lock")
                    .push((revision, candidate_index));
                changed.notify_all();
                Response::HistoryCandidateDeleted { removed: true }
            }
            Request::QueueCandidateCommit { .. } => {
                Response::CandidateCommitQueued { queued: true }
            }
            other => panic!("fixture renderer sent unexpected request: {other:?}"),
        };
        frame.clear();
        encode_response(&response, id, &mut frame).expect("encode fixture response");
        match pipe.write_all(&frame) {
            Ok(()) => {}
            // Assertion unwinding can terminate the test-owned renderer before
            // the fixture's final response reaches it; this is a normal owned
            // cleanup terminal state, not a second fixture failure.
            Err(sakura_ipc::Fault::Disconnected) => return,
            Err(error) => panic!("write fixture response: {error:?}"),
        }
        if matches!(response, Response::Ui(UiState { stopping: true, .. })) {
            return;
        }
    }
}

fn pack_point(point: POINT) -> LPARAM {
    let x = point.x as u16 as u32;
    let y = point.y as u16 as u32;
    LPARAM((x | (y << 16)) as isize)
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
    wait_for_renderer_window(renderer_pid, CANDIDATE_CLASS)
}

#[derive(Clone, Copy)]
struct HistoryDeleteWindows {
    base: HWND,
    overlay: HWND,
}

fn wait_for_history_delete_windows(renderer_pid: u32) -> HistoryDeleteWindows {
    let deadline = Instant::now() + PATIENT;
    loop {
        let base = find_visible_renderer_window(renderer_pid, CANDIDATE_CLASS);
        let overlay = find_visible_renderer_window(renderer_pid, DELETE_TARGETS_CLASS);
        if let (Some(base), Some(overlay)) = (base, overlay) {
            return HistoryDeleteWindows { base, overlay };
        }
        assert!(
            Instant::now() < deadline,
            "candidate base and delete overlay did not become visible"
        );
        sleep(Duration::from_millis(20));
    }
}

fn wait_for_renderer_window(renderer_pid: u32, class: PCWSTR) -> HWND {
    let deadline = Instant::now() + PATIENT;
    loop {
        if let Some(window) = find_visible_renderer_window(renderer_pid, class) {
            return window;
        }
        assert!(
            Instant::now() < deadline,
            "renderer window did not become visible"
        );
        sleep(Duration::from_millis(20));
    }
}

fn find_visible_renderer_window(renderer_pid: u32, class: PCWSTR) -> Option<HWND> {
    let mut after = None;
    loop {
        // SAFETY: the supplied class name and null title are valid for this
        // synchronous top-level window enumeration step.
        let found = unsafe { FindWindowExW(None, after, class, PCWSTR::null()) };
        let Ok(window) = found else {
            return None;
        };
        after = Some(window);
        let mut owner_pid = 0;
        // SAFETY: `window` was returned by the immediately preceding
        // enumeration call and `owner_pid` is a valid out-pointer.
        unsafe { GetWindowThreadProcessId(window, Some(&mut owner_pid)) };
        // SAFETY: the discovered HWND remains valid for this immediate query.
        if owner_pid == renderer_pid && unsafe { IsWindowVisible(window) }.as_bool() {
            return Some(window);
        }
    }
}

struct ForeignClickTarget {
    window: HWND,
    clicks: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl ForeignClickTarget {
    fn new(rect: windows::Win32::Foundation::RECT) -> Self {
        let clicks = Arc::new(AtomicUsize::new(0));
        let thread_clicks = Arc::clone(&clicks);
        let (ready, result) = sync_channel::<Result<isize, &'static str>>(1);
        let thread = thread::spawn(move || {
            // SAFETY: the class procedure and class name are static. If a
            // prior registration remains in this test process it is compatible.
            unsafe {
                let class = WNDCLASSW {
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(foreign_target_procedure),
                    lpszClassName: FOREIGN_TARGET_CLASS,
                    ..Default::default()
                };
                RegisterClassW(&class);
            }
            // SAFETY: the class is registered above; the test owns both the
            // target HWND and the stable `Arc` backing its user-data pointer.
            let window = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                    FOREIGN_TARGET_CLASS,
                    PCWSTR::null(),
                    WS_POPUP,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    None,
                    None,
                    None,
                    None,
                )
            };
            let Ok(window) = window else {
                let _ = ready.send(Err("create foreign routing target"));
                return;
            };
            // SAFETY: `thread_clicks` remains retained by `ForeignClickTarget`
            // until this thread is joined; the HWND belongs to this thread.
            unsafe {
                SetWindowLongPtrW(window, GWLP_USERDATA, Arc::as_ptr(&thread_clicks) as isize);
                let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
            }
            if ready.send(Ok(window.0 as isize)).is_err() {
                // SAFETY: this branch retains exclusive ownership of the HWND.
                unsafe {
                    let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
                };
            }
            let mut message = MSG::default();
            loop {
                // SAFETY: `message` is a valid out-pointer for this thread's
                // private message loop.
                let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
                if result.0 <= 0 {
                    return;
                }
                // SAFETY: `message` was returned by GetMessageW above.
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        });
        let window = result
            .recv_timeout(PATIENT)
            .expect("foreign routing target thread must report readiness")
            .expect("create foreign routing target");
        Self {
            window: HWND(window as *mut _),
            clicks,
            thread: Some(thread),
        }
    }

    const fn window(&self) -> HWND {
        self.window
    }

    fn assert_no_mouse_messages_for(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        loop {
            let messages = self.clicks.load(Ordering::SeqCst);
            assert_eq!(
                messages,
                0,
                "foreign routing target received physical mouse input: {}",
                foreign_mouse_message_summary(messages)
            );
            if Instant::now() >= deadline {
                return;
            }
            sleep(Duration::from_millis(10));
        }
    }

    fn stop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        // SAFETY: this test owns the target HWND and WM_CLOSE triggers its
        // WM_DESTROY/PostQuitMessage shutdown path on the foreign thread.
        let _ = unsafe { PostMessageW(Some(self.window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        let deadline = Instant::now() + PATIENT;
        while !thread.is_finished() {
            assert!(
                Instant::now() < deadline,
                "foreign routing target thread did not stop within {PATIENT:?}"
            );
            sleep(Duration::from_millis(20));
        }
        thread
            .join()
            .expect("foreign routing target thread must stop");
    }
}

impl Drop for ForeignClickTarget {
    fn drop(&mut self) {
        self.stop();
    }
}

fn foreign_mouse_message_summary(messages: usize) -> String {
    format!(
        "left-down={}, left-up={}, right-down={}, right-up={}",
        messages & 0xff,
        (messages >> 8) & 0xff,
        (messages >> 16) & 0xff,
        (messages >> 24) & 0xff,
    )
}

extern "system" fn foreign_target_procedure(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP => {
            // SAFETY: the owner installs this stable AtomicUsize pointer before
            // showing the window and joins the UI thread before releasing it.
            let clicks = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const AtomicUsize;
            if !clicks.is_null() {
                // SAFETY: pointer lifetime is owned by ForeignClickTarget.
                let increment = match message {
                    WM_LBUTTONDOWN => FOREIGN_LEFT_DOWN_INCREMENT,
                    WM_LBUTTONUP => FOREIGN_LEFT_UP_INCREMENT,
                    WM_RBUTTONDOWN => FOREIGN_RIGHT_DOWN_INCREMENT,
                    WM_RBUTTONUP => FOREIGN_RIGHT_UP_INCREMENT,
                    _ => unreachable!("outer match restricts mouse message"),
                };
                // SAFETY: pointer lifetime is owned by ForeignClickTarget.
                unsafe { (*clicks).fetch_add(increment, Ordering::SeqCst) };
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: this is the sole message loop owned by the target thread.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: all unhandled messages use the system default procedure.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

fn place_target_beneath_renderer_windows(target: HWND, base: HWND, overlay: HWND) {
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    // SAFETY: all three HWNDs are live, test-owned windows. With this API's
    // hWndInsertAfter ordering, request overlay, then base, then the foreign
    // target; the actual SendInput result remains the routing authority.
    unsafe {
        SetWindowPos(overlay, Some(HWND_TOPMOST), 0, 0, 0, 0, flags)
            .expect("make delete overlay topmost");
        SetWindowPos(base, Some(overlay), 0, 0, 0, 0, flags)
            .expect("place base popup beneath delete overlay");
        SetWindowPos(target, Some(base), 0, 0, 0, 0, flags)
            .expect("place foreign routing target beneath base popup");
    }
}

struct CursorRestore {
    original: POINT,
}

#[derive(Clone, Copy, Debug)]
struct InputDelivery {
    point: POINT,
    cursor_before_input: POINT,
    cursor_after_input: POINT,
    inserted: u32,
}

impl CursorRestore {
    fn new() -> Self {
        let mut original = POINT::default();
        // SAFETY: `original` is a valid out-pointer for the cursor location.
        unsafe { GetCursorPos(&mut original).expect("read user cursor position") };
        Self { original }
    }

    fn click_at(&self, point: POINT) -> InputDelivery {
        self.send_click(point, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, "left")
    }

    fn right_click_at(&self, point: POINT) -> InputDelivery {
        self.send_click(point, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, "right")
    }

    fn send_click(
        &self,
        point: POINT,
        down_flag: u32,
        up_flag: u32,
        button: &str,
    ) -> InputDelivery {
        // SAFETY: the absolute screen point is valid; Drop restores the prior
        // cursor location on every normal or unwinding terminal path.
        unsafe { SetCursorPos(point.x, point.y).expect("position test cursor") };
        let mut cursor_before_input = POINT::default();
        // SAFETY: `cursor_before_input` is a valid out-pointer and this test
        // must prove that SendInput uses the intended physical screen point.
        unsafe { GetCursorPos(&mut cursor_before_input).expect("read positioned test cursor") };
        assert_eq!(
            cursor_before_input, point,
            "SetCursorPos must reach the intended SendInput screen point"
        );
        let inputs = [
            TestInput {
                input_type: INPUT_MOUSE,
                payload: TestInputPayload {
                    mouse: TestMouseInput {
                        dx: 0,
                        dy: 0,
                        mouse_data: 0,
                        flags: down_flag,
                        time: 0,
                        extra_info: 0,
                    },
                },
            },
            TestInput {
                input_type: INPUT_MOUSE,
                payload: TestInputPayload {
                    mouse: TestMouseInput {
                        dx: 0,
                        dy: 0,
                        mouse_data: 0,
                        flags: up_flag,
                        time: 0,
                        extra_info: 0,
                    },
                },
            },
        ];
        // SAFETY: `inputs` has the exact C layout for a pair of mouse INPUT
        // records and remains live throughout the synchronous SendInput call.
        let inserted = unsafe { SendInput(2, inputs.as_ptr(), size_of::<TestInput>() as i32) };
        assert_eq!(
            inserted, 2,
            "SendInput must deliver both {button}-button mouse records"
        );
        let mut cursor_after_input = POINT::default();
        // SAFETY: `cursor_after_input` is a valid out-pointer for the current
        // desktop pointer position after the bounded SendInput call.
        unsafe { GetCursorPos(&mut cursor_after_input).expect("read cursor after SendInput") };
        InputDelivery {
            point,
            cursor_before_input,
            cursor_after_input,
            inserted,
        }
    }
}

impl Drop for CursorRestore {
    fn drop(&mut self) {
        // SAFETY: restore only the position captured from the interactive
        // desktop; no button or keyboard input is synthesized during cleanup.
        let _ = unsafe { SetCursorPos(self.original.x, self.original.y) };
    }
}

struct ForegroundRestore {
    original: HWND,
}

impl ForegroundRestore {
    fn new() -> Self {
        // SAFETY: foreground querying has no caller-owned pointer arguments.
        let original = unsafe { GetForegroundWindow() };
        Self { original }
    }
}

impl Drop for ForegroundRestore {
    fn drop(&mut self) {
        // SAFETY: if an external window changed foreground during this ignored
        // interactive test, request restoration without synthesizing input.
        // SAFETY: foreground querying has no caller-owned pointer arguments.
        let current = unsafe { GetForegroundWindow() };
        if !self.original.0.is_null() && current != self.original {
            // SAFETY: `original` was obtained from GetForegroundWindow and
            // restoration does not synthesize keyboard or pointer input.
            let _ = unsafe { SetForegroundWindow(self.original) };
        }
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

fn wait_for_indicator_state(window: HWND, expected: isize) {
    let deadline = Instant::now() + PATIENT;
    loop {
        // SAFETY: the test owns the renderer child and this is an immediate
        // read of the indicator's documented scalar window word.
        let observed = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) };
        if observed == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "indicator state never became {expected}; last value was {observed}"
        );
        sleep(Duration::from_millis(20));
    }
}

fn wait_for_surface_color(window: HWND, expected: COLORREF) -> COLORREF {
    let deadline = Instant::now() + PATIENT;
    loop {
        let observed = candidate_surface_color(window);
        if observed == expected {
            return observed;
        }
        if observed == COLORREF(CLR_INVALID) {
            assert!(
                Instant::now() < deadline,
                "candidate popup paint was never available"
            );
            sleep(Duration::from_millis(20));
            continue;
        }
        assert!(
            Instant::now() < deadline,
            "candidate surface never repainted to {expected:?}; last color was {observed:?}"
        );
        sleep(Duration::from_millis(20));
    }
}

fn candidate_surface_color(window: HWND) -> COLORREF {
    // SAFETY: `window` is live and owned by the test renderer process. The
    // client point (4, 1) is inside the selected row but outside text and rail.
    let dc = unsafe { GetDC(Some(window)) };
    assert!(!dc.is_invalid(), "acquire candidate popup paint DC");
    // SAFETY: `dc` is live until the paired ReleaseDC immediately below.
    let color = unsafe { GetPixel(dc, 4, 1) };
    // SAFETY: balances the successful GetDC above for this exact HWND/DC pair.
    let released = unsafe { ReleaseDC(Some(window), dc) };
    assert_ne!(released, 0, "release candidate popup paint DC");
    color
}

fn wait_for_hidden_window(window: HWND) {
    let deadline = Instant::now() + PATIENT;
    loop {
        // SAFETY: the caller owns the renderer process and this immediate query.
        if !unsafe { IsWindowVisible(window) }.as_bool() {
            return;
        }
        assert!(Instant::now() < deadline, "candidate window did not hide");
        sleep(Duration::from_millis(20));
    }
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
