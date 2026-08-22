//! Interactive User32 coverage for the settings topic rail.
//!
//! This is deliberately an ignored integration test: it launches the built
//! payload in a separate process and injects a real mouse down/up pair through
//! User32 `SendInput`.  `WM_COMMAND` is used only to inspect the native list
//! state after input; it is never used to simulate the click under test.

#![cfg(windows)]

use std::ffi::c_void;
use std::mem::size_of;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::ptr::null_mut;
use std::sync::{Mutex, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_core::{
    AppearanceTheme, BracketStyle, ConversionMethod, InputMethod, PunctuationStyle,
    ShiftSpaceBehavior, SpaceWidth, Width,
};
use sakura_proto::Mode;
use sakura_settings::configuration::ConfigurationDocument;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::UI::Controls::{
    HTREEITEM, TVGN_CARET, TVGN_CHILD, TVGN_NEXT, TVGN_ROOT, TVM_ENSUREVISIBLE, TVM_GETITEMRECT,
    TVM_GETNEXTITEM,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageW, BM_GETCHECK, CB_GETCURSEL, LB_GETCOUNT, LB_GETCURSEL, LB_GETITEMRECT, LB_GETTEXT,
    LB_GETTEXTLEN, WM_DPICHANGED,
};

const READY_TIMEOUT: Duration = Duration::from_secs(5);
const INPUT_SETTLING: Duration = Duration::from_millis(25);
const INPUT_MOUSE: u32 = 0;
const INPUT_KEYBOARD: u32 = 1;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
const MOUSEEVENTF_WHEEL: u32 = 0x0800;
const WHEEL_DELTA: i32 = 120;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const VK_ESCAPE: u16 = 0x1b;
const VK_TAB: u16 = 0x09;
const LB_GETITEMHEIGHT_SCALAR: u32 = 0x01a1;
const TVM_GETBKCOLOR: u32 = 0x111f;
const TVM_GETTEXTCOLOR: u32 = 0x1120;
const DARK_TREE_BACKGROUND: isize = 0x0025_2525;
const DARK_TREE_TEXT: isize = 0x00f1_f3f5;
const LIGHT_TREE_BACKGROUND: isize = 0x00e2_e5e8;
const LIGHT_TREE_TEXT: isize = 0x002f_2f2f;
const PROCESS_VM_OPERATION: u32 = 0x0008;
const PROCESS_VM_READ: u32 = 0x0010;
const PROCESS_VM_WRITE: u32 = 0x0020;
const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_RELEASE: u32 = 0x8000;
const PAGE_READWRITE: u32 = 0x04;

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
    mouse: TestMouseInput,
    keyboard: TestKeyboardInput,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TestInput {
    input_type: u32,
    payload: TestInputPayload,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn ClientToScreen(window: HWND, point: *mut POINT) -> i32;
    fn FindWindowExW(
        parent: HWND,
        child_after: HWND,
        class_name: *const u16,
        window_name: *const u16,
    ) -> HWND;
    fn GetCursorPos(point: *mut POINT) -> i32;
    fn GetFocus() -> HWND;
    fn GetParent(window: HWND) -> HWND;
    fn GetForegroundWindow() -> HWND;
    fn GetWindowRect(window: HWND, rect: *mut RECT) -> i32;
    fn GetWindowThreadProcessId(window: HWND, process_id: *mut u32) -> u32;
    fn IsWindowVisible(window: HWND) -> i32;
    fn SendInput(input_count: u32, inputs: *const TestInput, input_size: i32) -> u32;
    fn SetCursorPos(x: i32, y: i32) -> i32;
    fn SetForegroundWindow(window: HWND) -> i32;
    fn SetWindowPos(
        window: HWND,
        insert_after: HWND,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        flags: u32,
    ) -> i32;
    fn WindowFromPoint(point: POINT) -> HWND;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn AttachThreadInput(source_thread: u32, target_thread: u32, attach: i32) -> i32;
    fn BringWindowToTop(window: HWND) -> i32;
    fn SetFocus(window: HWND) -> HWND;
    fn ShowWindow(window: HWND, command: i32) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetCurrentThreadId() -> u32;
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
    fn ReadProcessMemory(
        process: *mut c_void,
        address: *const c_void,
        buffer: *mut c_void,
        size: usize,
        bytes_read: *mut usize,
    ) -> i32;
    fn VirtualAllocEx(
        process: *mut c_void,
        address: *const c_void,
        size: usize,
        allocation_type: u32,
        protect: u32,
    ) -> *mut c_void;
    fn VirtualFreeEx(
        process: *mut c_void,
        address: *mut c_void,
        size: usize,
        free_type: u32,
    ) -> i32;
    fn WriteProcessMemory(
        process: *mut c_void,
        address: *mut c_void,
        buffer: *const c_void,
        size: usize,
        bytes_written: *mut usize,
    ) -> i32;
}

const SW_SHOW: i32 = 5;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_SHOWWINDOW: u32 = 0x0040;
const HWND_TOPMOST: HWND = HWND(-1isize as *mut c_void);

// These scenarios drive one interactive desktop cursor and foreground window.
// Serialize only the desktop-driving tests so a normal `cargo test` run cannot
// make two fixtures steal the same pointer or Z-order from each other.
static DESKTOP_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn desktop_test_guard() -> std::sync::MutexGuard<'static, ()> {
    DESKTOP_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Clicks the `アプリ別の設定` leaf exactly as a desktop user would.
///
/// Run only on an interactive Windows desktop:
/// `rtk cargo test -p sakura-settings --test settings_topic_user32 -- --ignored --exact --nocapture`
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn profile_topic_click_shows_only_profile_controls_and_keeps_status_out_of_actions() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let initial_foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();

    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    assert!(is_visible(input_tree), "input topic TreeView is visible");

    let (basic_panel, profile_panel) = input_topic_panels(root);
    let normalizer_panel = input_topic_panel_with_heading(root, "文字幅・句読点");
    assert!(is_visible(basic_panel), "basic topic is initially visible");
    assert!(
        !is_visible(profile_panel),
        "profile topic is initially hidden"
    );

    // The test owns the newly launched process.  Foregrounding only that window
    // makes the following physical input deterministic and avoids interacting
    // with an unrelated desktop target.
    raise_fixture_for_input(root);
    wait_until("settings foreground", || {
        // SAFETY: this scalar query has no caller-provided pointers.
        unsafe { GetForegroundWindow() == root }
    });

    // The ATOK-like rail now contains the additional conversion/support
    // groups, so the app-profile leaf can be just below the first viewport.
    // Scroll it into view with real wheel input before scanning visible rows.
    let tree_rect = window_rect(input_tree);
    cursor.wheel(
        POINT {
            x: (tree_rect.left + tree_rect.right) / 2,
            y: tree_rect.bottom - 10,
        },
        -2,
    );
    sleep(INPUT_SETTLING);
    click_tree_row_until("アプリ別の設定", &cursor, input_tree, || {
        !is_visible(basic_panel) && is_visible(profile_panel)
    });
    assert!(
        !is_visible(basic_panel) && is_visible(profile_panel),
        "only the selected topic panel is visible"
    );
    wait_until("settings remains foreground after topic click", || {
        // SAFETY: this scalar query has no caller-provided pointers.
        unsafe { GetForegroundWindow() == root }
    });

    // Return to the top of the rail before selecting the first row again.
    cursor.wheel(
        POINT {
            x: (tree_rect.left + tree_rect.right) / 2,
            y: tree_rect.top + 10,
        },
        2,
    );
    sleep(INPUT_SETTLING);
    click_tree_row_until("基本", &cursor, input_tree, || {
        is_visible(basic_panel) && !is_visible(profile_panel)
    });
    wait_until("User32 click returns to the basic topic", || {
        is_visible(basic_panel) && !is_visible(profile_panel)
    });
    assert!(
        is_visible(basic_panel) && !is_visible(profile_panel),
        "returning to the first topic hides the profile controls again"
    );
    wait_until(
        "settings remains foreground after returning to basic",
        || {
            // SAFETY: this scalar query has no caller-provided pointers.
            unsafe { GetForegroundWindow() == root }
        },
    );

    // The normalizer leaf owns its reset action.  Select it and press the
    // button through a real desktop click; this is deliberately not a
    // synthetic WM_COMMAND.
    click_tree_row_until("文字幅・句読点 (reset)", &cursor, input_tree, || {
        is_visible(normalizer_panel) && !is_visible(basic_panel) && !is_visible(profile_panel)
    });
    let reset = find_direct_child_with_text(normalizer_panel, "初期値に戻す")
        .expect("文字幅・句読点 topic reset button");
    assert!(
        is_visible(reset),
        "normalizer reset belongs to the selected topic"
    );
    let (status, _) = bottom_status_and_apply(root);
    let reset_rect = window_rect(reset);
    cursor.left_click(POINT {
        x: (reset_rect.left + reset_rect.right) / 2,
        y: (reset_rect.top + reset_rect.bottom) / 2,
    });
    wait_until("input/transform reset status", || {
        status_text(status).starts_with("文字幅・句読点の設定を初期値")
    });

    let (status, apply) = bottom_status_and_apply(root);
    let mut status_rect = window_rect(status);
    let apply_rect = window_rect(apply);
    assert!(
        status_rect.right <= apply_rect.left,
        "one-line status slot must end before the persistent action row"
    );
    assert!(
        status_rect.bottom <= apply_rect.bottom,
        "status stays inside the bottom row"
    );
    // A physical Apply click proves that an operation cannot expand the status
    // slot over the adjacent controls.  The content is intentionally read only
    // after the click; it is not injected through a window message.
    let apply_point = POINT {
        x: (apply_rect.left + apply_rect.right) / 2,
        y: (apply_rect.top + apply_rect.bottom) / 2,
    };
    cursor.left_click(apply_point);
    wait_until("apply status update", || {
        status_text(status) == "既定の設定を保存しました。"
    });
    status_rect = window_rect(status);
    assert!(
        status_rect.right <= apply_rect.left && status_rect.bottom <= apply_rect.bottom,
        "saved-status control still cannot overlap Apply"
    );

    drop(cursor);
    drop(initial_foreground);
}

/// The dark/light preference is a real appearance setting, not just a painted
/// root surface.  Exercise the native ComboBox with User32 input and query the
/// TreeView's documented colors through the same HWND boundary.  This catches
/// the common regression where TreeView keeps a white item background while the
/// surrounding property sheet has already switched to the dark palette.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn theme_combo_updates_input_tree_colors_through_user32() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let cursor = CursorRestore::capture();
    raise_fixture_for_input(root);

    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let display_panel = input_topic_panel_with_heading(root, "表示");
    click_tree_row_until("表示", &cursor, input_tree, || is_visible(display_panel));

    let appearance = direct_children(display_panel)
        .into_iter()
        .find(|window| class_name(*window) == "ComboBox" && is_visible(*window))
        .expect("visible appearance ComboBox in the selected display topic");

    select_combo_item_with_mouse(&cursor, appearance, 2);
    wait_until("dark TreeView background and text colors", || {
        tree_color(input_tree, TVM_GETBKCOLOR) == DARK_TREE_BACKGROUND
            && tree_color(input_tree, TVM_GETTEXTCOLOR) == DARK_TREE_TEXT
    });
    assert_eq!(
        tree_color(input_tree, TVM_GETBKCOLOR),
        DARK_TREE_BACKGROUND,
        "dark appearance applies the dark input surface to the native TreeView"
    );
    assert_eq!(
        tree_color(input_tree, TVM_GETTEXTCOLOR),
        DARK_TREE_TEXT,
        "dark appearance applies readable light text to the native TreeView"
    );

    select_combo_item_with_mouse(&cursor, appearance, 1);
    wait_until("light TreeView background and text colors", || {
        tree_color(input_tree, TVM_GETBKCOLOR) == LIGHT_TREE_BACKGROUND
            && tree_color(input_tree, TVM_GETTEXTCOLOR) == LIGHT_TREE_TEXT
    });
    assert_eq!(
        tree_color(input_tree, TVM_GETBKCOLOR),
        LIGHT_TREE_BACKGROUND,
        "light appearance applies the neutral input surface to the native TreeView"
    );
    assert_eq!(
        tree_color(input_tree, TVM_GETTEXTCOLOR),
        LIGHT_TREE_TEXT,
        "light appearance applies neutral #2F2F2F text to the native TreeView"
    );
}

#[test]
#[ignore = "requires an interactive User32 desktop"]
fn dpi_change_reflows_atok_property_sheet_grid_without_clipping() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let _cursor = CursorRestore::capture();
    raise_fixture_for_input(root);

    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let input_outer = input_topic_outer(root);
    let apply = find_direct_child_with_text(root, "適用").expect("bottom Apply button");
    let old_root = window_rect(root);
    let old_tree = window_rect(input_tree);
    let old_outer = window_rect(input_outer);
    let old_apply = window_rect(apply);
    // SAFETY: `root` is the live top-level HWND returned by the fixture.
    let old_dpi = unsafe { GetDpiForWindow(root) }.max(96);
    let new_dpi = old_dpi.saturating_add(old_dpi / 2).max(old_dpi + 1);
    let target_width = scale_metric(old_root.right - old_root.left, old_dpi, new_dpi);
    let target_height = scale_metric(old_root.bottom - old_root.top, old_dpi, new_dpi);
    let mut suggested = RECT {
        left: old_root.left,
        top: old_root.top,
        right: old_root.left + target_width,
        bottom: old_root.top + target_height,
    };

    // This is the same raw User32 system notification Windows sends when the
    // dialog crosses monitors; the payload contains a caller-owned suggested
    // rectangle only for the synchronous duration of SendMessageW.
    // SAFETY: the live root HWND receives a scalar DPI and a valid suggested
    // rectangle that remains allocated for the synchronous SendMessageW call.
    unsafe {
        let _ = SendMessageW(
            root,
            WM_DPICHANGED,
            Some(WPARAM((new_dpi as usize) | ((new_dpi as usize) << 16))),
            Some(LPARAM(
                (&mut suggested as *mut RECT).cast::<c_void>() as isize
            )),
        );
    }
    wait_until("DPI-scaled settings root", || {
        let rect = window_rect(root);
        rect.right - rect.left == target_width && rect.bottom - rect.top == target_height
    });

    let new_tree = window_rect(input_tree);
    let new_outer = window_rect(input_outer);
    let new_apply = window_rect(apply);
    assert_eq!(
        new_tree.right - new_tree.left,
        scale_metric(old_tree.right - old_tree.left, old_dpi, new_dpi)
    );
    assert_eq!(
        new_tree.bottom - new_tree.top,
        scale_metric(old_tree.bottom - old_tree.top, old_dpi, new_dpi)
    );
    assert_eq!(
        new_outer.right - new_outer.left,
        scale_metric(old_outer.right - old_outer.left, old_dpi, new_dpi)
    );
    assert_eq!(
        new_apply.right - new_apply.left,
        scale_metric(old_apply.right - old_apply.left, old_dpi, new_dpi)
    );
    assert!(
        new_tree.right < new_outer.left,
        "scaled left rail must remain before the right pane"
    );
    assert!(
        new_apply.bottom < window_rect(root).bottom,
        "scaled action row must remain inside the dialog"
    );
    assert!(
        is_visible(input_tree) && is_visible(apply),
        "visible controls remain visible after DPI change"
    );

    let mut restore = RECT {
        left: old_root.left,
        top: old_root.top,
        right: old_root.right,
        bottom: old_root.bottom,
    };
    // SAFETY: the live root HWND receives a scalar DPI and a valid restore
    // rectangle that remains allocated for the synchronous SendMessageW call.
    unsafe {
        let _ = SendMessageW(
            root,
            WM_DPICHANGED,
            Some(WPARAM((old_dpi as usize) | ((old_dpi as usize) << 16))),
            Some(LPARAM((&mut restore as *mut RECT).cast::<c_void>() as isize)),
        );
    }
    wait_until("DPI-restored settings root", || {
        let rect = window_rect(root);
        rect.right - rect.left == old_root.right - old_root.left
            && rect.bottom - rect.top == old_root.bottom - old_root.top
    });
}

/// The compact property-sheet shell must still expose a predictable keyboard
/// order.  This test asks User32 for the payload thread's real focus HWND after
/// every physical Tab; it rejects hidden topic controls appearing in the order
/// and requires all five navigation buttons before the input TreeView and the
/// persistent OK/Cancel/Apply row.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn tab_focus_order_skips_hidden_topics_and_ends_at_actions() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    raise_fixture_for_input(root);

    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let outer = input_topic_outer(root);
    let hidden_profile = input_topic_panel_with_heading_from_outer(outer, "アプリ別の設定");
    let ok = find_direct_child_with_text(root, "OK").expect("OK button");
    let cancel = find_direct_child_with_text(root, "キャンセル").expect("Cancel button");
    let apply = find_direct_child_with_text(root, "適用").expect("Apply button");
    let navigation: Vec<_> = direct_children(root)
        .into_iter()
        .filter(|window| {
            class_name(*window) == "Button"
                && ["入力・変換", "辞書", "学習", "診断", "更新"]
                    .contains(&status_text(*window).as_str())
        })
        .collect();
    assert_eq!(
        navigation.len(),
        5,
        "all property-sheet navigation buttons exist"
    );

    let thread_id = window_thread_id(root);
    let mut seen_navigation = Vec::new();
    for _ in 0..16 {
        cursor.key_press(VK_TAB);
        let focused = focused_window(thread_id);
        if navigation.contains(&focused) && !seen_navigation.contains(&focused) {
            seen_navigation.push(focused);
        }
        assert!(
            !is_descendant_or_self(focused, hidden_profile),
            "Tab must skip the hidden app-profile topic controls"
        );
        if focused == input_tree {
            break;
        }
    }
    assert_eq!(
        seen_navigation.len(),
        navigation.len(),
        "Tab reaches every navigation button before the TreeView"
    );
    assert_eq!(
        focused_window(thread_id),
        input_tree,
        "Tab leaves the navigation strip on the visible input TreeView"
    );

    let mut action_order = Vec::new();
    for _ in 0..48 {
        cursor.key_press(VK_TAB);
        let focused = focused_window(thread_id);
        assert!(
            !is_descendant_or_self(focused, hidden_profile),
            "hidden app-profile controls never receive Tab focus"
        );
        if [ok, cancel, apply].contains(&focused) && !action_order.contains(&focused) {
            action_order.push(focused);
        }
        if action_order.len() == 3 {
            break;
        }
    }
    assert_eq!(
        action_order,
        vec![ok, cancel, apply],
        "visible action row follows the pane in OK, Cancel, Apply order"
    );
}

/// Escape is the property-sheet Cancel path.  A theme preview is intentionally
/// immediate for visual feedback, but it must not become durable until Apply or
/// OK.  This test drives both operations through real User32 input and checks
/// the isolated configuration file after the payload has exited.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn escape_cancel_discards_unapplied_preferences() {
    let _desktop = desktop_test_guard();
    let mut fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    raise_fixture_for_input(root);

    let config_path = fixture
        .sandbox
        .join("SakuraInput")
        .join("config")
        .join("config.toml");
    let before = ConfigurationDocument::load(&config_path).expect("load initial isolated config");
    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let display_panel = input_topic_panel_with_heading(root, "表示");
    click_tree_row_until("表示", &cursor, input_tree, || is_visible(display_panel));
    let appearance = direct_children(display_panel)
        .into_iter()
        .find(|window| class_name(*window) == "ComboBox" && is_visible(*window))
        .expect("visible appearance ComboBox");
    select_combo_item_with_mouse(&cursor, appearance, 2);
    wait_until("dark preview before Escape", || {
        combo_selection(appearance) == 2
            && tree_color(input_tree, TVM_GETBKCOLOR) == DARK_TREE_BACKGROUND
    });
    wait_until("appearance popup closes before Escape", || {
        combo_popup(appearance).is_none()
    });

    // This is a physical keyboard path.  The root window owns an explicit
    // WM_KEYDOWN Escape handler so custom panel focus cannot bypass Cancel.
    cursor.key_press(VK_ESCAPE);
    fixture.wait_for_exit();
    let after = ConfigurationDocument::load(&config_path).expect("load config after Escape");
    assert_eq!(
        after.preferences.appearance_theme, before.preferences.appearance_theme,
        "Escape must discard the unapplied appearance preview"
    );
}

/// Apply is the durable property-sheet path.  Close the first process through
/// a real OK click, relaunch against the same isolated LOCALAPPDATA, and query
/// the native appearance ComboBox to prove the saved value is read back.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn apply_persists_preferences_across_a_user32_relaunch() {
    let _desktop = desktop_test_guard();
    let mut fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    raise_fixture_for_input(root);

    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let display_panel = input_topic_panel_with_heading(root, "表示");
    click_tree_row_until("表示", &cursor, input_tree, || is_visible(display_panel));
    let appearance = direct_children(display_panel)
        .into_iter()
        .find(|window| class_name(*window) == "ComboBox" && is_visible(*window))
        .expect("visible appearance ComboBox");
    select_combo_item_with_mouse(&cursor, appearance, 2);

    let (_, apply) = bottom_status_and_apply(root);
    let apply_rect = window_rect(apply);
    cursor.left_click(POINT {
        x: (apply_rect.left + apply_rect.right) / 2,
        y: (apply_rect.top + apply_rect.bottom) / 2,
    });
    let config_path = fixture
        .sandbox
        .join("SakuraInput")
        .join("config")
        .join("config.toml");
    wait_until("physical Apply persists dark appearance", || {
        ConfigurationDocument::load(&config_path)
            .map(|document| document.preferences.appearance_theme == AppearanceTheme::Dark)
            .unwrap_or(false)
    });

    let ok = find_direct_child_with_text(root, "OK").expect("OK button");
    let ok_rect = window_rect(ok);
    cursor.left_click(POINT {
        x: (ok_rect.left + ok_rect.right) / 2,
        y: (ok_rect.top + ok_rect.bottom) / 2,
    });
    fixture.wait_for_exit();

    let mut relaunched = fixture.relaunch();
    let relaunched_root = SettingsFixture::wait_for_process_window(relaunched.id());
    let relaunched_cursor = CursorRestore::capture();
    raise_fixture_for_input(relaunched_root);
    let relaunched_tree =
        find_direct_child(relaunched_root, "SysTreeView32").expect("relaunched input TreeView");
    let relaunched_display = input_topic_panel_with_heading(relaunched_root, "表示");
    click_tree_row_until("表示", &relaunched_cursor, relaunched_tree, || {
        is_visible(relaunched_display)
    });
    let relaunched_appearance = direct_children(relaunched_display)
        .into_iter()
        .find(|window| class_name(*window) == "ComboBox" && is_visible(*window))
        .expect("relaunched appearance ComboBox");
    assert_eq!(
        combo_selection(relaunched_appearance),
        2,
        "the relaunch must read the dark appearance saved by Apply"
    );
    relaunched_cursor.key_press(VK_ESCAPE);
    wait_for_process_exit(&mut relaunched);
}

/// The stable root launcher can be clicked repeatedly, but all versioned
/// payloads must converge on the already-visible property sheet. The second
/// process should activate the first window and exit without publishing a
/// second top-level settings HWND.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn second_settings_launch_activates_the_existing_window() {
    let _desktop = desktop_test_guard();
    let mut fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let first_pid = fixture.child.id();
    let _foreground = ForegroundRestore::capture();
    raise_fixture_for_input(root);

    let mut duplicate = fixture.relaunch();
    let duplicate_pid = duplicate.id();
    wait_for_process_exit(&mut duplicate);

    assert!(
        is_visible(root),
        "the first settings window remains visible after the duplicate launch"
    );
    assert_eq!(
        window_process_id(root),
        first_pid,
        "the original settings process still owns the visible window"
    );
    assert!(
        !top_level_windows().into_iter().any(|window| {
            class_name(window) == "SakuraInputSettingsWindow"
                && window_process_id(window) == duplicate_pid
        }),
        "the duplicate process must not publish another settings window"
    );
    wait_until(
        "second launch foregrounds the existing settings window",
        || {
            // SAFETY: this scalar query has no caller-provided pointers.
            unsafe { GetForegroundWindow() == root }
        },
    );

    // Close only the first process through its normal Escape path. The fixture
    // owns it; the duplicate has already reached its successful terminal exit.
    let cursor = CursorRestore::capture();
    cursor.key_press(VK_ESCAPE);
    fixture.wait_for_exit();
}

/// The input rail is a real native TreeView. This scenario uses only physical
/// User32 mouse input to reach the `連想変換` leaf and proves that its page
/// replaces the basic/profile pages rather than being painted on top of them.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn input_tree_click_shows_only_selected_conversion_controls() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let (basic_panel, profile_panel) = input_topic_panels(root);
    let input_assist_panel = input_topic_panel_with_heading(root, "入力補助");
    let segment_panel = input_topic_panel_with_heading(root, "文節変換");
    let normalizer_panel = input_topic_panel_with_heading(root, "文字幅・句読点");
    let prediction_panel = input_topic_panel_with_heading(root, "推測変換");
    let association_panel = input_topic_panel_with_heading(root, "連想変換");
    let display_panel = input_topic_panel_with_heading(root, "表示");
    assert!(
        is_visible(input_tree),
        "ATOK-like input tree starts visible"
    );
    assert!(is_visible(basic_panel), "basic topic starts visible");
    assert!(
        !is_visible(association_panel),
        "association topic starts hidden"
    );
    assert!(!is_visible(segment_panel), "segment topic starts hidden");
    assert!(
        !is_visible(normalizer_panel),
        "normalizer topic starts hidden"
    );
    assert!(
        !is_visible(prediction_panel),
        "prediction topic starts hidden"
    );
    assert!(!is_visible(display_panel), "display topic starts hidden");
    assert!(
        !is_visible(input_assist_panel),
        "input-assist topic starts hidden"
    );

    raise_fixture_for_input(root);
    click_tree_row_until("入力補助", &cursor, input_tree, || {
        is_visible(input_assist_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(prediction_panel)
            && !is_visible(association_panel)
            && !is_visible(display_panel)
    });
    assert!(
        is_visible(input_assist_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(prediction_panel)
            && !is_visible(association_panel)
            && !is_visible(display_panel),
        "only the selected 入力補助 page remains visible"
    );
    assert!(
        find_direct_child_with_text(input_assist_panel, "スペースキー").is_some(),
        "input-assist page exposes its real Space-key control"
    );
    assert!(
        find_direct_child_with_text(input_assist_panel, "キー設定").is_none(),
        "basic keymap control must not be duplicated by input-assist"
    );

    click_tree_row_until("文節変換", &cursor, input_tree, || {
        is_visible(segment_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(prediction_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(association_panel)
            && !is_visible(display_panel)
    });
    assert!(
        is_visible(segment_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(prediction_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(association_panel)
            && !is_visible(display_panel),
        "only the selected 文節変換 page remains visible"
    );
    assert!(
        find_direct_child_with_text(segment_panel, "sakura-rerank の適用範囲").is_some(),
        "sakura-rerank scope belongs to the selected 文節変換 page"
    );

    click_tree_row_until("文字幅・句読点", &cursor, input_tree, || {
        is_visible(normalizer_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(prediction_panel)
            && !is_visible(association_panel)
            && !is_visible(display_panel)
    });
    assert!(
        is_visible(normalizer_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(prediction_panel)
            && !is_visible(association_panel)
            && !is_visible(display_panel),
        "only the selected 文字幅・句読点 page remains visible"
    );
    assert!(
        find_direct_child_with_text(normalizer_panel, "sakura-rerank の適用範囲").is_none(),
        "normalizer page must not duplicate the sakura-rerank scope control"
    );
    assert!(
        find_direct_child_with_text(normalizer_panel, "初期値に戻す").is_some(),
        "normalizer reset belongs to the selected 文字幅・句読点 page"
    );

    click_tree_row_until("推測変換", &cursor, input_tree, || {
        is_visible(prediction_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(association_panel)
            && !is_visible(display_panel)
    });
    assert!(
        is_visible(prediction_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(association_panel)
            && !is_visible(display_panel),
        "only the selected 推測変換 page remains visible"
    );
    let prediction_description = find_direct_child_with_text(
        prediction_panel,
        "入力中に候補を自動表示し、確定方法を選べます。",
    )
    .expect("prediction page description");
    let prediction_group =
        find_direct_child_with_text(prediction_panel, "推測候補").expect("prediction group box");
    let description_rect = window_rect(prediction_description);
    let group_rect = window_rect(prediction_group);
    assert!(
        group_rect.top >= description_rect.bottom + 8,
        "prediction group must leave an 8 px visual gap below its description: description={description_rect:?}, group={group_rect:?}"
    );

    click_tree_row_until("連想変換", &cursor, input_tree, || {
        is_visible(association_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(prediction_panel)
            && !is_visible(display_panel)
    });
    assert!(
        is_visible(association_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(prediction_panel)
            && !is_visible(display_panel),
        "only the selected 連想変換 page remains visible"
    );
    assert!(
        find_direct_child_with_text(association_panel, "sakura-rerank の適用範囲").is_none(),
        "association page must not duplicate the sakura-rerank scope control"
    );
    let association_toggle = find_direct_child_with_text(association_panel, "連想変換を使う")
        .expect("association enable checkbox");
    assert!(
        is_visible(association_toggle),
        "association controls are visible in the selected page"
    );

    let tree_rect = window_rect(input_tree);
    cursor.wheel(
        POINT {
            x: (tree_rect.left + tree_rect.right) / 2,
            y: tree_rect.bottom - 10,
        },
        -1,
    );
    sleep(INPUT_SETTLING);
    click_tree_row_until("表示", &cursor, input_tree, || {
        is_visible(display_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(prediction_panel)
            && !is_visible(association_panel)
    });
    assert!(
        is_visible(display_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(segment_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(prediction_panel)
            && !is_visible(association_panel),
        "only the selected 表示 page remains visible"
    );
}

/// `入力支援` is a category, not a duplicate settings page.  A physical click
/// must normalize its TreeView selection to the first real child
/// (`入力誤りの自動修復`) so the highlighted left item and the visible
/// right-hand heading cannot disagree.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn input_support_topic_click_shows_prediction_assistance_controls() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let (basic_panel, profile_panel) = input_topic_panels(root);
    let input_assist_panel = input_topic_panel_with_heading(root, "入力補助");
    let repair_panel = input_topic_panel_with_heading(root, "入力誤りの自動修復");
    raise_fixture_for_input(root);

    click_tree_row_until("入力支援", &cursor, input_tree, || {
        is_visible(repair_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(input_assist_panel)
    });
    let expected_repair = tree_item_relative(
        input_tree,
        TVGN_CHILD as usize,
        input_tree_item(input_tree, "入力支援"),
    );
    assert_eq!(
        selected_input_tree_item(input_tree),
        expected_repair,
        "a category click must leave the first real child highlighted"
    );
    assert!(
        is_visible(repair_panel)
            && !is_visible(basic_panel)
            && !is_visible(profile_panel)
            && !is_visible(input_assist_panel),
        "入力支援 category click selects only its input-repair child page"
    );
    assert!(
        find_direct_child_with_text(repair_panel, "入力支援を有効にする").is_some(),
        "input-repair page exposes its master preference control"
    );
}

/// `入力補助` owns only the two physical Space-key rules. The basic input
/// method, character type, and conversion method must not be duplicated here.
/// This drives both native ComboBox popups with User32 pointer input and proves
/// Apply changes only the two input-assist preferences.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn input_assist_topic_click_shows_only_input_assist_controls() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let outer = input_topic_outer(root);
    let basic_panel = input_topic_panel_with_heading_from_outer(outer, "基本設定");
    let input_assist_panel = input_topic_panel_with_heading_from_outer(outer, "入力補助");
    let segment_panel = input_topic_panel_with_heading_from_outer(outer, "文節変換");
    let profile_panel = input_topic_panel_with_heading_from_outer(outer, "アプリ別の設定");
    raise_fixture_for_input(root);
    click_tree_row_until("入力補助", &cursor, input_tree, || {
        is_visible(input_assist_panel)
            && !is_visible(basic_panel)
            && !is_visible(segment_panel)
            && !is_visible(profile_panel)
    });

    let mut combos: Vec<_> = direct_children(input_assist_panel)
        .into_iter()
        .filter(|window| class_name(*window) == "ComboBox" && is_visible(*window))
        .collect();
    combos.sort_by_key(|window| {
        let rect = window_rect(*window);
        (rect.top, rect.left)
    });
    assert_eq!(
        combos.len(),
        2,
        "input-assist exposes only the Space and Shift+Space controls"
    );
    for basic_label in ["キー設定", "入力方法", "文字種", "変換方法"] {
        assert!(
            find_direct_child_with_text(input_assist_panel, basic_label).is_none(),
            "input-assist must not duplicate the basic `{basic_label}` setting"
        );
    }
    let close_popup = || {
        if combos
            .iter()
            .any(|candidate| combo_popup(*candidate).is_some())
        {
            cursor.key_press(VK_ESCAPE);
            wait_until("input-assist ComboBox popup closes", || {
                combos
                    .iter()
                    .all(|candidate| combo_popup(*candidate).is_none())
            });
        }
    };
    close_popup();
    select_combo_item_with_mouse(&cursor, combos[0], 2);
    close_popup();
    select_combo_item_with_mouse(&cursor, combos[1], 0);
    assert_eq!(
        combo_selection(combos[0]),
        2,
        "Space order selects 常に半角"
    );
    assert_eq!(
        combo_selection(combos[1]),
        0,
        "Shift+Space order selects スペースの逆"
    );

    let (_, apply) = bottom_status_and_apply(root);
    let apply_rect = window_rect(apply);
    cursor.left_click(POINT {
        x: (apply_rect.left + apply_rect.right) / 2,
        y: (apply_rect.top + apply_rect.bottom) / 2,
    });
    let config_path = fixture
        .sandbox
        .join("SakuraInput")
        .join("config")
        .join("config.toml");
    wait_until(
        "input-assist Apply persists only the two input-assist values",
        || {
            ConfigurationDocument::load(&config_path)
                .map(|document| {
                    document.preferences.keymap_preset.name() == "ms-ime"
                        && document.preferences.default_mode == Mode::Hiragana
                        && document.preferences.conversion_method == ConversionMethod::MultiSegment
                        && document.preferences.space_width == SpaceWidth::Half
                        && document.preferences.shift_space_behavior == ShiftSpaceBehavior::Opposite
                })
                .unwrap_or(false)
        },
    );
    let saved = ConfigurationDocument::load(&config_path).expect("load input-assist Apply output");
    assert_eq!(saved.preferences.keymap_preset.name(), "ms-ime");
    assert_eq!(saved.preferences.default_mode, Mode::Hiragana);
    assert_eq!(
        saved.preferences.conversion_method,
        ConversionMethod::MultiSegment
    );
    assert_eq!(saved.preferences.space_width, SpaceWidth::Half);
    assert_eq!(
        saved.preferences.shift_space_behavior,
        ShiftSpaceBehavior::Opposite
    );
}

/// `変換補助` is a category, not a second page with the same conversion
/// controls.  A physical click normalizes to its first real child (`文節変換`),
/// where the conversion method is owned exactly once.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn conversion_category_click_normalizes_to_segment_controls() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let outer = input_topic_outer(root);
    let basic_panel = input_topic_panel_with_heading_from_outer(outer, "基本設定");
    let segment_panel = input_topic_panel_with_heading_from_outer(outer, "文節変換");
    let normalizer_panel = input_topic_panel_with_heading_from_outer(outer, "文字幅・句読点");
    let input_assist_panel = input_topic_panel_with_heading_from_outer(outer, "入力補助");
    raise_fixture_for_input(root);
    click_tree_row_until("変換補助", &cursor, input_tree, || {
        is_visible(segment_panel)
            && !is_visible(basic_panel)
            && !is_visible(normalizer_panel)
            && !is_visible(input_assist_panel)
    });
    let expected_segment = tree_item_relative(
        input_tree,
        TVGN_CHILD as usize,
        input_tree_item(input_tree, "変換補助"),
    );
    assert_eq!(
        selected_input_tree_item(input_tree),
        expected_segment,
        "a category click must leave the first real child highlighted"
    );
    let method = direct_children(segment_panel)
        .into_iter()
        .find(|window| class_name(*window) == "ComboBox" && is_visible(*window))
        .expect("segment conversion-method ComboBox");
    select_combo_item_with_mouse(&cursor, method, 1);
    assert_eq!(
        combo_selection(method),
        1,
        "conversion method selection is physical"
    );
    let (_, apply) = bottom_status_and_apply(root);
    let apply_rect = window_rect(apply);
    cursor.left_click(POINT {
        x: (apply_rect.left + apply_rect.right) / 2,
        y: (apply_rect.top + apply_rect.bottom) / 2,
    });
    let config_path = fixture
        .sandbox
        .join("SakuraInput")
        .join("config")
        .join("config.toml");
    wait_until("conversion category Apply persists the method", || {
        ConfigurationDocument::load(&config_path)
            .map(|document| {
                document.preferences.conversion_method == ConversionMethod::SingleSegment
            })
            .unwrap_or(false)
    });
}

/// The `文字幅・句読点` reset action is a real settings transaction, not a
/// cosmetic button.  Its group frame must end before the action row begins,
/// and its physical click must restore only normalizer settings instead of
/// silently resetting a sibling page.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn normalizer_reset_is_separate_from_its_group_and_restores_only_normalizer() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let outer = input_topic_outer(root);
    let normalizer_panel = input_topic_panel_with_heading_from_outer(outer, "文字幅・句読点");
    let association_panel = input_topic_panel_with_heading_from_outer(outer, "連想変換");
    raise_fixture_for_input(root);

    click_tree_row_until("連想変換", &cursor, input_tree, || {
        is_visible(association_panel) && !is_visible(normalizer_panel)
    });
    let association_toggle = find_direct_child_with_text(association_panel, "連想変換を使う")
        .expect("association checkbox");
    assert_eq!(
        button_checked(association_toggle),
        1,
        "association starts enabled"
    );
    let association_rect = window_rect(association_toggle);
    cursor.left_click(POINT {
        x: (association_rect.left + association_rect.right) / 2,
        y: (association_rect.top + association_rect.bottom) / 2,
    });
    wait_until("association disabled through User32", || {
        button_checked(association_toggle) == 0
    });

    click_tree_row_until("文字幅・句読点", &cursor, input_tree, || {
        is_visible(normalizer_panel) && !is_visible(association_panel)
    });
    let reset = find_direct_child_with_text(normalizer_panel, "初期値に戻す")
        .expect("normalizer reset button");
    let group = find_direct_child_with_text(normalizer_panel, "入力・変換")
        .expect("normalizer visual group box");
    let group_rect = window_rect(group);
    let reset_rect = window_rect(reset);
    assert!(
        reset_rect.top >= group_rect.bottom + 8,
        "normalizer reset must be below the group border with a clear gap: group={group_rect:?}, reset={reset_rect:?}"
    );
    let mut combos: Vec<_> = direct_children(normalizer_panel)
        .into_iter()
        .filter(|window| class_name(*window) == "ComboBox" && is_visible(*window))
        .collect();
    combos.sort_by_key(|window| {
        let rect = window_rect(*window);
        (rect.top, rect.left)
    });
    assert_eq!(
        combos.len(),
        6,
        "normalizer page exposes its six owned ComboBoxes"
    );
    select_combo_item_with_mouse(&cursor, combos[0], 1);
    assert_eq!(
        combo_selection(combos[0]),
        1,
        "physical width change is visible before reset"
    );
    let reset_point = POINT {
        x: (reset_rect.left + reset_rect.right) / 2,
        y: (reset_rect.top + reset_rect.bottom) / 2,
    };
    // SAFETY: `reset_point` is inside the live native reset button rectangle.
    assert_eq!(unsafe { WindowFromPoint(reset_point) }, reset);
    cursor.left_click(POINT {
        x: reset_point.x,
        y: reset_point.y,
    });
    wait_until("normalizer reset restores the alnum default", || {
        combo_selection(combos[0]) == 0
    });

    let (_, apply) = bottom_status_and_apply(root);
    let apply_rect = window_rect(apply);
    cursor.left_click(POINT {
        x: (apply_rect.left + apply_rect.right) / 2,
        y: (apply_rect.top + apply_rect.bottom) / 2,
    });
    let config_path = fixture
        .sandbox
        .join("SakuraInput")
        .join("config")
        .join("config.toml");
    wait_until(
        "physical normalizer reset persists only its defaults",
        || {
            ConfigurationDocument::load(&config_path)
                .map(|document| {
                    !document.preferences.association_enabled
                        && document.preferences.normalizer.width.alnum == Width::Half
                })
                .unwrap_or(false)
        },
    );
}

/// This is the control-level counterpart to the topic visibility checks.  It
/// discovers all live ComboBox HWNDs in the selected `文字幅・句読点` page,
/// clicks the changed controls through the desktop, selects a different native
/// popup row, and then clicks Apply.  The assertion is made against the isolated
/// configuration file rather than a status string, so a visually convincing
/// but disconnected control cannot pass.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn normalizer_controls_are_discoverable_clickable_and_apply_persists_values() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    let input_tree = find_direct_child(root, "SysTreeView32").expect("input topic TreeView");
    let outer = input_topic_outer(root);
    let basic_panel = input_topic_panel_with_heading_from_outer(outer, "基本設定");
    let profile_panel = input_topic_panel_with_heading_from_outer(outer, "アプリ別の設定");
    let normalizer_panel = input_topic_panel_with_heading_from_outer(outer, "文字幅・句読点");

    raise_fixture_for_input(root);
    click_tree_row_until("文字幅・句読点", &cursor, input_tree, || {
        is_visible(normalizer_panel) && !is_visible(basic_panel) && !is_visible(profile_panel)
    });
    assert!(
        is_visible(normalizer_panel) && !is_visible(basic_panel) && !is_visible(profile_panel),
        "only the selected normalizer page is visible before editing"
    );

    let mut combos: Vec<_> = direct_children(normalizer_panel)
        .into_iter()
        .filter(|window| class_name(*window) == "ComboBox" && is_visible(*window))
        .collect();
    combos.sort_by_key(|window| {
        let rect = window_rect(*window);
        (rect.top, rect.left)
    });
    assert_eq!(
        combos.len(),
        6,
        "the selected normalizer page exposes width, punctuation, and bracket ComboBox controls"
    );
    let (status, _) = bottom_status_and_apply(root);

    for (index, combo) in combos.iter().enumerate() {
        assert!(is_visible(*combo), "ComboBox {index} remains visible");
        assert_eq!(class_name(*combo), "ComboBox");
        if index == 0 || index == 2 {
            // Select the second item with a physical popup-row click.  Leave
            // the comma at its Japanese default so this first Apply proves
            // the two punctuation controls are independent.
            if combos
                .iter()
                .any(|candidate| combo_popup(*candidate).is_some())
            {
                cursor.key_press(VK_ESCAPE);
                wait_until("previous ComboBox popup closes", || {
                    combos
                        .iter()
                        .all(|candidate| combo_popup(*candidate).is_none())
                });
            }
            let status_rect = window_rect(status);
            cursor.left_click(POINT {
                x: (status_rect.left + status_rect.right) / 2,
                y: (status_rect.top + status_rect.bottom) / 2,
            });
            select_combo_item_with_mouse(&cursor, *combo, 1);
        }
    }
    assert_eq!(
        combo_selection(combos[0]),
        1,
        "physical ComboLBox row selection changes the alnum ComboBox"
    );
    assert_eq!(
        combo_selection(combos[2]),
        1,
        "physical ComboBox row selection changes the period ComboBox"
    );
    assert_eq!(
        combo_selection(combos[3]),
        0,
        "period selection leaves the comma ComboBox at its Japanese default"
    );
    assert_eq!(
        combo_selection(combos[5]),
        0,
        "the bracket ComboBox starts at Sakura's corner-bracket default"
    );

    let (_, apply) = bottom_status_and_apply(root);
    let apply_rect = window_rect(apply);
    let apply_point = POINT {
        x: (apply_rect.left + apply_rect.right) / 2,
        y: (apply_rect.top + apply_rect.bottom) / 2,
    };
    // SAFETY: the point is the live bottom Apply button's screen rectangle.
    assert_eq!(unsafe { WindowFromPoint(apply_point) }, apply);
    cursor.left_click(apply_point);

    let config_path = fixture
        .sandbox
        .join("SakuraInput")
        .join("config")
        .join("config.toml");
    wait_until("physical Apply persists transform values", || {
        ConfigurationDocument::load(&config_path)
            .map(|document| {
                document.preferences.normalizer.width.alnum == Width::Full
                    && document.preferences.normalizer.punctuation == PunctuationStyle::Mixed
            })
            .unwrap_or(false)
    });
    select_combo_item_with_mouse(&cursor, combos[3], 1);
    assert_eq!(
        combo_selection(combos[3]),
        1,
        "physical ComboBox row selection changes the comma ComboBox"
    );
    select_combo_item_with_mouse(&cursor, combos[5], 1);
    assert_eq!(
        combo_selection(combos[5]),
        1,
        "physical ComboBox row selection changes the bracket style"
    );
    cursor.left_click(apply_point);
    wait_until(
        "second physical Apply persists the independent comma value",
        || {
            ConfigurationDocument::load(&config_path)
                .map(|document| {
                    document.preferences.normalizer.punctuation == PunctuationStyle::CommaPeriod
                        && document.preferences.normalizer.brackets == BracketStyle::Square
                })
                .unwrap_or(false)
        },
    );
    let saved = ConfigurationDocument::load(&config_path).expect("load isolated Apply output");
    assert_eq!(
        saved.preferences.normalizer.width.alnum,
        Width::Full,
        "physical ComboBox selection reaches the saved alnum setting"
    );
    assert_eq!(
        saved.preferences.normalizer.punctuation,
        PunctuationStyle::CommaPeriod,
        "physical independent punctuation selections reach the saved setting"
    );
    assert_eq!(
        saved.preferences.normalizer.brackets,
        BracketStyle::Square,
        "physical bracket selection reaches the saved setting"
    );
}

/// The ATOK-like basic page exposes input-method choice as real native radio
/// controls.  This test proves that the controls are discoverable by HWND,
/// that a physical click changes the mutually-exclusive selection, and that
/// Apply reaches the same isolated TOML boundary consumed by the engine.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn input_method_radio_controls_are_physical_and_persist_to_the_engine_config() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    raise_fixture_for_input(root);

    let outer = input_topic_outer(root);
    let basic_panel = input_topic_panel_with_heading_from_outer(outer, "基本設定");
    let kana =
        find_direct_child_with_text(basic_panel, "カナ入力").expect("native カナ入力 radio button");
    let romaji = find_direct_child_with_text(basic_panel, "ローマ字入力")
        .expect("native ローマ字入力 radio button");
    let (_, apply) = bottom_status_and_apply(root);

    let kana_rect = window_rect(kana);
    let kana_point = POINT {
        x: (kana_rect.left + kana_rect.right) / 2,
        y: (kana_rect.top + kana_rect.bottom) / 2,
    };
    // SAFETY: the point is inside the live native radio button rectangle.
    assert_eq!(unsafe { WindowFromPoint(kana_point) }, kana);
    cursor.left_click(kana_point);
    wait_until("physical Kana radio selection", || {
        button_checked(kana) == 1 && button_checked(romaji) == 0
    });

    let apply_rect = window_rect(apply);
    cursor.left_click(POINT {
        x: (apply_rect.left + apply_rect.right) / 2,
        y: (apply_rect.top + apply_rect.bottom) / 2,
    });
    let config_path = fixture
        .sandbox
        .join("SakuraInput")
        .join("config")
        .join("config.toml");
    wait_until("physical Apply persists Kana input method", || {
        ConfigurationDocument::load(&config_path)
            .map(|document| document.preferences.input_method == InputMethod::Kana)
            .unwrap_or(false)
    });

    let mut basic_combos: Vec<_> = direct_children(basic_panel)
        .into_iter()
        .filter(|window| class_name(*window) == "ComboBox" && is_visible(*window))
        .collect();
    basic_combos.sort_by_key(|window| {
        let rect = window_rect(*window);
        (rect.top, rect.left)
    });
    assert_eq!(
        basic_combos.len(),
        2,
        "basic page exposes only keymap and character-type ComboBoxes"
    );
    select_combo_item_with_mouse(&cursor, basic_combos[1], 2);
    assert_eq!(
        combo_selection(basic_combos[1]),
        2,
        "physical character-type selection changes the native ComboBox"
    );
    cursor.left_click(POINT {
        x: (apply_rect.left + apply_rect.right) / 2,
        y: (apply_rect.top + apply_rect.bottom) / 2,
    });
    wait_until(
        "physical Apply persists Katakana default character type",
        || {
            ConfigurationDocument::load(&config_path)
                .map(|document| document.preferences.default_mode == Mode::Katakana)
                .unwrap_or(false)
        },
    );

    select_combo_item_with_mouse(&cursor, basic_combos[1], 1);
    cursor.left_click(POINT {
        x: (apply_rect.left + apply_rect.right) / 2,
        y: (apply_rect.top + apply_rect.bottom) / 2,
    });
    wait_until(
        "physical Apply restores Hiragana default character type",
        || {
            ConfigurationDocument::load(&config_path)
                .map(|document| document.preferences.default_mode == Mode::Hiragana)
                .unwrap_or(false)
        },
    );
}

/// The dictionary page follows the same property-sheet rule: the selected
/// left topic owns the only visible right-hand group.  This catches regressions
/// where the import/export controls remain painted over the word editor.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn dictionary_topic_click_shows_only_the_selected_dictionary_group() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    raise_fixture_for_input(root);
    wait_until("dictionary navigation foreground", || unsafe {
        // SAFETY: this scalar query has no caller-provided pointers.
        GetForegroundWindow() == root
    });

    let dictionary_tab =
        find_direct_child_with_text(root, "辞書").expect("dictionary navigation button");
    let tab_rect = window_rect(dictionary_tab);
    let tab_point = POINT {
        x: (tab_rect.left + tab_rect.right) / 2,
        y: (tab_rect.top + tab_rect.bottom) / 2,
    };
    // SAFETY: `tab_point` is inside the live navigation button rectangle.
    let tab_hit = unsafe { WindowFromPoint(tab_point) };
    assert_eq!(
        tab_hit,
        dictionary_tab,
        "the dictionary navigation point must hit the fixture button (point {:?}, tab {:?}, root {:?}, hit {:?})",
        tab_point,
        tab_rect,
        window_rect(root),
        tab_hit,
    );
    assert_eq!(
        class_name(tab_hit),
        "Button",
        "the dictionary navigation point must be covered by the settings button (point {:?}, tab {:?}, root {:?}, foreground {:?}, hit {:?})",
        tab_point,
        tab_rect,
        window_rect(root),
        // SAFETY: this scalar query has no caller-provided pointers.
        unsafe { GetForegroundWindow() },
        tab_hit,
    );
    cursor.left_click(tab_point);
    wait_until("dictionary page foreground", || unsafe {
        // SAFETY: this scalar query has no caller-provided pointers.
        GetForegroundWindow() == root
    });
    let dictionary_ready_started = Instant::now();
    while !find_direct_child(root, "ListBox")
        .map(|list| list_value(list, LB_GETCOUNT) == 2 && list_text(list, 0) == "登録単語")
        .unwrap_or(false)
    {
        assert!(
            dictionary_ready_started.elapsed() < READY_TIMEOUT,
            "timed out waiting for dictionary page; children={:?}",
            direct_children(root)
                .into_iter()
                .map(|child| (class_name(child), status_text(child), is_visible(child)))
                .collect::<Vec<_>>()
        );
        sleep(INPUT_SETTLING);
    }

    let topic_list = find_direct_child(root, "ListBox").expect("dictionary topic ListBox");
    assert_eq!(
        list_text(topic_list, 0),
        "登録単語",
        "dictionary page topic 0"
    );
    assert_eq!(
        list_text(topic_list, 1),
        "辞書ファイルの入出力",
        "dictionary page topic 1"
    );
    let outer = direct_children(root)
        .into_iter()
        .find(|window| {
            class_name(*window) == "SakuraInputSettingsPanel"
                && is_visible(*window)
                && direct_children(*window)
                    .iter()
                    .filter(|child| class_name(**child) == "SakuraInputSettingsPanel")
                    .count()
                    >= 2
        })
        .expect("visible dictionary page panel");
    let topics: Vec<_> = direct_children(outer)
        .into_iter()
        .filter(|window| class_name(*window) == "SakuraInputSettingsPanel")
        .collect();
    assert_eq!(
        topics.len(),
        2,
        "dictionary page has two nested topic panels"
    );
    assert!(
        is_visible(topics[0]),
        "word registration topic starts visible"
    );
    assert!(!is_visible(topics[1]), "file I/O topic starts hidden");

    let item_rect = list_item_rect(topic_list, 1);
    let mut click = POINT {
        x: (item_rect.left + item_rect.right) / 2,
        y: (item_rect.top + item_rect.bottom) / 2,
    };
    // SAFETY: the point is in the live list box client coordinate space.
    assert_ne!(unsafe { ClientToScreen(topic_list, &mut click) }, 0);
    // SAFETY: `click` is a desktop point returned by User32 for this fixture.
    let hit = unsafe { WindowFromPoint(click) };
    assert_eq!(
        class_name(hit),
        "ListBox",
        "the physical second-topic point must hit the topic ListBox (got {:?}, rect {:?}, list {:?})",
        hit,
        window_rect(hit),
        window_rect(topic_list),
    );
    cursor.left_click(click);
    wait_until("dictionary file topic selection", || {
        list_value(topic_list, LB_GETCURSEL) == 1
    });
    wait_until("dictionary file topic visibility", || {
        !is_visible(topics[0]) && is_visible(topics[1])
    });
    assert!(
        !is_visible(topics[0]),
        "word panel hides after file topic click (topic0={:?}, topic1={:?})",
        is_visible(topics[0]),
        is_visible(topics[1])
    );
    assert!(
        is_visible(topics[1]),
        "file panel shows after file topic click (topic0={:?}, topic1={:?})",
        is_visible(topics[0]),
        is_visible(topics[1])
    );

    let item_rect = list_item_rect(topic_list, 0);
    let mut click = POINT {
        x: (item_rect.left + item_rect.right) / 2,
        y: (item_rect.top + item_rect.bottom) / 2,
    };
    // SAFETY: the point is in the live list box client coordinate space.
    assert_ne!(unsafe { ClientToScreen(topic_list, &mut click) }, 0);
    cursor.left_click(click);
    wait_until("dictionary word topic selection", || {
        list_value(topic_list, LB_GETCURSEL) == 0
    });
    wait_until("dictionary word topic visibility", || {
        is_visible(topics[0]) && !is_visible(topics[1])
    });
    assert!(
        is_visible(topics[0]),
        "word panel shows after word topic click"
    );
    assert!(
        !is_visible(topics[1]),
        "file panel hides after word topic click"
    );
}

/// Flat pages use the same property-sheet rule as the ATOK-style input rail:
/// the selected topic owns the only visible nested panel.  This scenario keeps
/// the learning and update tabs in the User32 coverage rather than treating
/// their ListBox rows as decorative labels.
#[test]
#[ignore = "requires an interactive User32 desktop"]
fn learning_and_update_topics_are_discoverable_and_clickable() {
    let _desktop = desktop_test_guard();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    raise_fixture_for_input(root);

    let learning_tab =
        find_direct_child_with_text(root, "学習").expect("learning navigation button");
    let learning_tab_rect = window_rect(learning_tab);
    let learning_point = POINT {
        x: (learning_tab_rect.left + learning_tab_rect.right) / 2,
        y: (learning_tab_rect.top + learning_tab_rect.bottom) / 2,
    };
    // SAFETY: `learning_point` is inside the live navigation button rectangle.
    assert_eq!(unsafe { WindowFromPoint(learning_point) }, learning_tab);
    cursor.left_click(learning_point);
    wait_until("learning page topics", || {
        find_direct_child(root, "ListBox")
            .map(|list| list_value(list, LB_GETCOUNT) == 2 && list_text(list, 0) == "学習履歴")
            .unwrap_or(false)
    });
    let learning_topics = find_direct_child(root, "ListBox").expect("learning topic ListBox");
    assert_eq!(list_text(learning_topics, 1), "操作");
    let learning_outer = page_outer_with_topic(root, "学習履歴");
    let learning_history = page_topic_panel(learning_outer, "学習履歴");
    let learning_operations = page_topic_panel(learning_outer, "操作");
    assert!(is_visible(learning_history));
    assert!(!is_visible(learning_operations));
    click_topic_item(&cursor, learning_topics, 1);
    wait_until("learning operations topic", || {
        !is_visible(learning_history) && is_visible(learning_operations)
    });

    let updates_tab = find_direct_child_with_text(root, "更新").expect("updates navigation button");
    let updates_tab_rect = window_rect(updates_tab);
    let updates_point = POINT {
        x: (updates_tab_rect.left + updates_tab_rect.right) / 2,
        y: (updates_tab_rect.top + updates_tab_rect.bottom) / 2,
    };
    // SAFETY: `updates_point` is inside the live navigation button rectangle.
    assert_eq!(unsafe { WindowFromPoint(updates_point) }, updates_tab);
    cursor.left_click(updates_point);
    wait_until("updates page topics", || {
        find_direct_child(root, "ListBox")
            .map(|list| list_value(list, LB_GETCOUNT) == 3 && list_text(list, 0) == "更新の設定")
            .unwrap_or(false)
    });
    let update_topics = find_direct_child(root, "ListBox").expect("updates topic ListBox");
    assert_eq!(list_text(update_topics, 1), "利用可能な更新");
    assert_eq!(list_text(update_topics, 2), "更新の状態");
    let updates_outer = page_outer_with_topic(root, "更新の確認");
    let update_settings = page_topic_panel(updates_outer, "更新の確認");
    let update_available = page_topic_panel(updates_outer, "利用可能な更新");
    let update_status = page_topic_panel(updates_outer, "更新の状態");
    assert!(is_visible(update_settings));
    assert!(!is_visible(update_available));
    assert!(!is_visible(update_status));
    click_topic_item(&cursor, update_topics, 2);
    wait_until("updates status topic", || {
        !is_visible(update_settings) && !is_visible(update_available) && is_visible(update_status)
    });
}

struct SettingsFixture {
    child: Child,
    sandbox: PathBuf,
}

impl SettingsFixture {
    fn launch() -> Self {
        let user_profile = std::env::var_os("USERPROFILE").expect("USERPROFILE is set on Windows");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_nanos();
        let sandbox = PathBuf::from(user_profile).join("tmp").join(format!(
            "sakura-settings-user32-{unique}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&sandbox).expect("create isolated LOCALAPPDATA");
        let child = spawn_settings_payload(&sandbox);
        Self { child, sandbox }
    }

    fn wait_for_window(&self) -> HWND {
        Self::wait_for_process_window(self.child.id())
    }

    fn wait_for_process_window(process_id: u32) -> HWND {
        let start = Instant::now();
        loop {
            if let Some(window) = top_level_windows()
                .into_iter()
                .find(|window| window_process_id(*window) == process_id && is_visible(*window))
            {
                return window;
            }
            assert!(
                start.elapsed() < READY_TIMEOUT,
                "settings payload did not publish a visible top-level HWND within {READY_TIMEOUT:?}"
            );
            sleep(INPUT_SETTLING);
        }
    }

    fn relaunch(&self) -> Child {
        spawn_settings_payload(&self.sandbox)
    }

    fn wait_for_exit(&mut self) {
        wait_for_process_exit(&mut self.child);
    }
}

fn settings_payload_executable() -> PathBuf {
    std::env::var_os("SAKURA_SETTINGS_E2E_EXE")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_sakura_settings_payload"))
        .map(PathBuf::from)
        .expect("set SAKURA_SETTINGS_E2E_EXE or let Cargo supply the settings payload path")
}

fn spawn_settings_payload(sandbox: &std::path::Path) -> Child {
    Command::new(settings_payload_executable())
        .env("LOCALAPPDATA", sandbox)
        .spawn()
        .expect("launch settings payload")
}

fn wait_for_process_exit(child: &mut Child) {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("read settings payload state") {
            assert!(
                status.success(),
                "settings payload exited unsuccessfully: {status}"
            );
            return;
        }
        assert!(
            start.elapsed() < READY_TIMEOUT,
            "settings payload did not exit within {READY_TIMEOUT:?}"
        );
        sleep(INPUT_SETTLING);
    }
}

impl Drop for SettingsFixture {
    fn drop(&mut self) {
        if self
            .child
            .try_wait()
            .expect("read settings payload state")
            .is_none()
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.sandbox);
    }
}

struct CursorRestore {
    original: POINT,
}

impl CursorRestore {
    fn capture() -> Self {
        let mut original = POINT::default();
        // SAFETY: `original` is valid writable storage for User32's cursor coordinate.
        let captured = unsafe { GetCursorPos(&mut original) };
        assert_ne!(captured, 0, "capture cursor position");
        Self { original }
    }

    fn left_click(&self, point: POINT) {
        // SAFETY: the point was derived from the fixture window's screen rectangle.
        let positioned = unsafe { SetCursorPos(point.x, point.y) };
        assert_ne!(positioned, 0, "position input cursor");
        let mut actual = POINT::default();
        // SAFETY: `actual` is valid writable storage for User32's cursor coordinate.
        let read_back = unsafe { GetCursorPos(&mut actual) };
        assert_ne!(read_back, 0, "read positioned cursor");
        assert_eq!(
            actual, point,
            "SendInput must use the intended screen point"
        );
        let inputs = [
            TestInput {
                input_type: INPUT_MOUSE,
                payload: TestInputPayload {
                    mouse: TestMouseInput {
                        dx: 0,
                        dy: 0,
                        mouse_data: 0,
                        flags: MOUSEEVENTF_LEFTDOWN,
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
                        flags: MOUSEEVENTF_LEFTUP,
                        time: 0,
                        extra_info: 0,
                    },
                },
            },
        ];
        // SAFETY: `inputs` remains live for this synchronous call and has INPUT's C layout.
        let inserted = unsafe { SendInput(2, inputs.as_ptr(), size_of::<TestInput>() as i32) };
        assert_eq!(
            inserted, 2,
            "User32 SendInput must insert down and up records"
        );
    }

    fn key_press(&self, virtual_key: u16) {
        let inputs = [
            TestInput {
                input_type: INPUT_KEYBOARD,
                payload: TestInputPayload {
                    keyboard: TestKeyboardInput {
                        virtual_key,
                        scan_code: 0,
                        flags: 0,
                        time: 0,
                        extra_info: 0,
                    },
                },
            },
            TestInput {
                input_type: INPUT_KEYBOARD,
                payload: TestInputPayload {
                    keyboard: TestKeyboardInput {
                        virtual_key,
                        scan_code: 0,
                        flags: KEYEVENTF_KEYUP,
                        time: 0,
                        extra_info: 0,
                    },
                },
            },
        ];
        // SAFETY: `inputs` remains live for this synchronous User32 call and
        // follows the x64 INPUT layout for keyboard records.
        let inserted = unsafe { SendInput(2, inputs.as_ptr(), size_of::<TestInput>() as i32) };
        assert_eq!(
            inserted, 2,
            "User32 SendInput must insert keyboard down/up records for VK {virtual_key:#x}"
        );
        sleep(INPUT_SETTLING);
    }

    fn wheel(&self, point: POINT, notches: i32) {
        // SAFETY: the point is derived from the live TreeView rectangle and is
        // only used to place the real desktop pointer before wheel input.
        let positioned = unsafe { SetCursorPos(point.x, point.y) };
        assert_ne!(positioned, 0, "position input cursor for wheel");
        let mut actual = POINT::default();
        // SAFETY: `actual` is valid writable storage for User32's cursor query.
        let read_back = unsafe { GetCursorPos(&mut actual) };
        assert_ne!(read_back, 0, "read wheel cursor position");
        assert_eq!(actual, point, "wheel must use the intended screen point");
        let input = TestInput {
            input_type: INPUT_MOUSE,
            payload: TestInputPayload {
                mouse: TestMouseInput {
                    dx: 0,
                    dy: 0,
                    mouse_data: notches.saturating_mul(WHEEL_DELTA) as u32,
                    flags: MOUSEEVENTF_WHEEL,
                    time: 0,
                    extra_info: 0,
                },
            },
        };
        // SAFETY: `input` remains live for this synchronous User32 call.
        let inserted = unsafe { SendInput(1, &input, size_of::<TestInput>() as i32) };
        assert_eq!(inserted, 1, "User32 SendInput must insert the wheel record");
    }
}

impl Drop for CursorRestore {
    fn drop(&mut self) {
        // SAFETY: this restores the previously captured physical screen coordinate.
        let _ = unsafe { SetCursorPos(self.original.x, self.original.y) };
    }
}

struct ForegroundRestore {
    original: HWND,
}

impl ForegroundRestore {
    fn capture() -> Self {
        Self {
            // SAFETY: this scalar query has no caller-provided pointers.
            original: unsafe { GetForegroundWindow() },
        }
    }
}

impl Drop for ForegroundRestore {
    fn drop(&mut self) {
        if !self.original.is_invalid() {
            // SAFETY: the saved handle came from User32 before this fixture was foregrounded.
            let _ = unsafe { SetForegroundWindow(self.original) };
        }
    }
}

fn focus_window(window: HWND) -> bool {
    // SAFETY: all calls below operate on live HWND/thread identifiers returned
    // by User32. The temporary input-queue attachment is always detached before
    // this function returns.
    let current_thread = unsafe { GetCurrentThreadId() };
    // SAFETY: this scalar query has no caller-provided pointers.
    let foreground = unsafe { GetForegroundWindow() };
    let foreground_thread = if foreground.is_invalid() {
        0
    } else {
        // SAFETY: `foreground` is a live HWND returned by User32.
        unsafe { GetWindowThreadProcessId(foreground, null_mut()) }
    };
    let attached = foreground_thread != 0
        && foreground_thread != current_thread
        // SAFETY: both thread IDs came from User32 and the attachment is balanced below.
        && unsafe { AttachThreadInput(current_thread, foreground_thread, 1) != 0 };

    let focused = (0..20).any(|_| {
        // SAFETY: `window` is the live top-level HWND created by this fixture.
        let is_foreground = unsafe {
            let _ = ShowWindow(window, SW_SHOW);
            let _ = BringWindowToTop(window);
            let _ = SetForegroundWindow(window);
            let _ = SetFocus(window);
            GetForegroundWindow() == window
        };
        if is_foreground {
            true
        } else {
            sleep(Duration::from_millis(10));
            false
        }
    });

    if attached {
        // SAFETY: balances the exact successful AttachThreadInput call above.
        let _ = unsafe { AttachThreadInput(current_thread, foreground_thread, 0) };
    }
    focused
}

fn raise_fixture_for_input(root: HWND) {
    // The interactive desktop used by these ignored tests may have a topmost
    // browser surface over the normal z-order. Keep this live fixture HWND on
    // top so WindowFromPoint and SendInput prove the intended controls.
    // The z-order change is scoped to this test process and the fixture exits
    // during cleanup.
    assert_ne!(
        // SAFETY: `root` is the live fixture HWND and all flags are scalar.
        unsafe {
            SetWindowPos(
                root,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            )
        },
        0,
        "raise settings fixture above unrelated topmost desktop surfaces"
    );
    assert!(focus_window(root), "foreground settings fixture");
    wait_until("settings fixture foreground", || unsafe {
        // SAFETY: this scalar query has no caller-provided pointers.
        GetForegroundWindow() == root
    });
}

fn wait_until(label: &str, predicate: impl Fn() -> bool) {
    let start = Instant::now();
    loop {
        if predicate() {
            return;
        }
        assert!(
            start.elapsed() < READY_TIMEOUT,
            "timed out waiting for {label} after {READY_TIMEOUT:?}"
        );
        sleep(INPUT_SETTLING);
    }
}

fn input_topic_panels(root: HWND) -> (HWND, HWND) {
    let outer = input_topic_outer(root);
    (
        input_topic_panel_with_heading_from_outer(outer, "基本設定"),
        input_topic_panel_with_heading_from_outer(outer, "アプリ別の設定"),
    )
}

fn input_topic_outer(root: HWND) -> HWND {
    direct_children(root)
        .into_iter()
        .find(|window| {
            class_name(*window) == "SakuraInputSettingsPanel"
                && direct_children(*window).iter().any(|child| {
                    class_name(*child) == "SakuraInputSettingsPanel"
                        && direct_children(*child)
                            .iter()
                            .any(|grandchild| status_text(*grandchild) == "アプリ別の設定")
                })
        })
        .expect("outer Input panel with topic children")
}

fn input_topic_panel_with_heading(root: HWND, heading: &str) -> HWND {
    input_topic_panel_with_heading_from_outer(input_topic_outer(root), heading)
}

fn input_topic_panel_with_heading_from_outer(outer: HWND, heading: &str) -> HWND {
    direct_children(outer)
        .into_iter()
        .find(|window| {
            class_name(*window) == "SakuraInputSettingsPanel"
                && direct_children(*window)
                    .iter()
                    .any(|child| status_text(*child) == heading)
        })
        .unwrap_or_else(|| panic!("input topic panel heading {heading:?}"))
}

fn page_outer_with_topic(root: HWND, heading: &str) -> HWND {
    direct_children(root)
        .into_iter()
        .find(|window| {
            class_name(*window) == "SakuraInputSettingsPanel"
                && direct_children(*window).iter().any(|child| {
                    class_name(*child) == "SakuraInputSettingsPanel"
                        && direct_children(*child)
                            .iter()
                            .any(|grandchild| status_text(*grandchild) == heading)
                })
        })
        .unwrap_or_else(|| panic!("page outer with topic {heading:?}"))
}

fn page_topic_panel(outer: HWND, heading: &str) -> HWND {
    direct_children(outer)
        .into_iter()
        .find(|window| {
            class_name(*window) == "SakuraInputSettingsPanel"
                && direct_children(*window)
                    .iter()
                    .any(|child| status_text(*child) == heading)
        })
        .unwrap_or_else(|| panic!("page topic panel {heading:?}"))
}

fn click_topic_item(cursor: &CursorRestore, list: HWND, index: usize) {
    let item = list_item_rect(list, index);
    let mut point = POINT {
        x: (item.left + item.right) / 2,
        y: (item.top + item.bottom) / 2,
    };
    // SAFETY: `point` starts in the live list client rectangle and remains live
    // for the synchronous coordinate conversion.
    assert_ne!(unsafe { ClientToScreen(list, &mut point) }, 0);
    // SAFETY: `point` was converted from the live ListBox item rectangle.
    assert_eq!(class_name(unsafe { WindowFromPoint(point) }), "ListBox");
    cursor.left_click(point);
}

fn bottom_status_and_apply(root: HWND) -> (HWND, HWND) {
    let root_rect = window_rect(root);
    let mut static_controls = direct_children(root)
        .into_iter()
        .filter(|window| class_name(*window) == "Static" && is_visible(*window));
    let status = static_controls
        .find(|window| window_rect(*window).top >= root_rect.bottom - 100)
        .expect("visible bottom status control");
    let mut bottom_buttons: Vec<_> = direct_children(root)
        .into_iter()
        .filter(|window| class_name(*window) == "Button" && is_visible(*window))
        .filter(|window| window_rect(*window).top >= root_rect.bottom - 100)
        .collect();
    bottom_buttons.sort_by_key(|window| window_rect(*window).left);
    let apply = *bottom_buttons
        .last()
        .expect("persistent bottom Apply button");
    (status, apply)
}

/// RAII ownership for the small remote buffer required by `TVM_GETITEMRECT`.
/// TreeView messages are `WM_USER`-range messages, so User32 does not marshal a
/// caller-owned `RECT` buffer into the fixture process for us.
struct RemoteTreeRect {
    process: *mut c_void,
    address: *mut c_void,
}

impl RemoteTreeRect {
    fn allocate(process_id: u32) -> Self {
        // SAFETY: the fixture was started by this test under the same user. The
        // requested rights are limited to the one temporary `RECT` buffer.
        let process = unsafe {
            OpenProcess(
                PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE,
                0,
                process_id,
            )
        };
        assert!(
            !process.is_null(),
            "open isolated settings payload for TreeView rectangle query"
        );
        // SAFETY: `process` is a live process handle and the allocation is only
        // `RECT`-sized, read/write memory that is released by Drop.
        let address = unsafe {
            VirtualAllocEx(
                process,
                std::ptr::null(),
                size_of::<RECT>(),
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if address.is_null() {
            // SAFETY: balances the successfully opened process handle above.
            let _ = unsafe { CloseHandle(process) };
            panic!("allocate remote TreeView RECT buffer");
        }
        Self { process, address }
    }

    fn write(&self, value: &RECT) {
        let mut bytes_written = 0;
        // SAFETY: `address` is this instance's `RECT`-sized remote allocation;
        // `value` remains live for the synchronous copy.
        let wrote = unsafe {
            WriteProcessMemory(
                self.process,
                self.address,
                (value as *const RECT).cast::<c_void>(),
                size_of::<RECT>(),
                &mut bytes_written,
            )
        };
        assert_ne!(wrote, 0, "seed remote TreeView RECT query buffer");
        assert_eq!(bytes_written, size_of::<RECT>());
    }

    fn read(&self) -> RECT {
        let mut value = RECT::default();
        let mut bytes_read = 0;
        // SAFETY: `address` is this instance's `RECT`-sized remote allocation;
        // `value` is writable local storage for the synchronous copy.
        let read = unsafe {
            ReadProcessMemory(
                self.process,
                self.address,
                (&mut value as *mut RECT).cast::<c_void>(),
                size_of::<RECT>(),
                &mut bytes_read,
            )
        };
        assert_ne!(read, 0, "read remote TreeView item rectangle");
        assert_eq!(bytes_read, size_of::<RECT>());
        value
    }
}

impl Drop for RemoteTreeRect {
    fn drop(&mut self) {
        if !self.address.is_null() {
            // SAFETY: `address` is the exact allocation made by `allocate`; the
            // fixture process remains alive while this query helper is scoped.
            let _ = unsafe { VirtualFreeEx(self.process, self.address, 0, MEM_RELEASE) };
        }
        if !self.process.is_null() {
            // SAFETY: balances the exact `OpenProcess` call made by `allocate`.
            let _ = unsafe { CloseHandle(self.process) };
        }
    }
}

fn tree_item_relative(tree: HWND, relationship: usize, item: HTREEITEM) -> HTREEITEM {
    // SAFETY: this is a scalar TreeView query. Item handles belong to the live
    // fixture TreeView; no caller-owned pointer crosses the process boundary.
    let result = unsafe {
        SendMessageW(
            tree,
            TVM_GETNEXTITEM,
            Some(WPARAM(relationship)),
            Some(LPARAM(item.0)),
        )
    };
    HTREEITEM(result.0)
}

fn require_tree_item(item: HTREEITEM, description: &str) -> HTREEITEM {
    assert_ne!(item.0, 0, "live input TreeView {description}");
    item
}

fn selected_input_tree_item(tree: HWND) -> HTREEITEM {
    require_tree_item(
        tree_item_relative(tree, TVGN_CARET as usize, HTREEITEM::default()),
        "selected item",
    )
}

fn input_tree_item(tree: HWND, label: &str) -> HTREEITEM {
    let basic = require_tree_item(
        tree_item_relative(tree, TVGN_ROOT as usize, HTREEITEM::default()),
        "root 基本 item",
    );
    let input_assist = require_tree_item(
        tree_item_relative(tree, TVGN_NEXT as usize, basic),
        "root 入力補助 item",
    );
    let conversion = require_tree_item(
        tree_item_relative(tree, TVGN_NEXT as usize, input_assist),
        "root 変換補助 item",
    );
    let segment = require_tree_item(
        tree_item_relative(tree, TVGN_CHILD as usize, conversion),
        "文節変換 child item",
    );
    let normalizer = require_tree_item(
        tree_item_relative(tree, TVGN_NEXT as usize, segment),
        "文字幅・句読点 child item",
    );
    let display = require_tree_item(
        tree_item_relative(tree, TVGN_NEXT as usize, conversion),
        "root 表示 item",
    );
    let input_support = require_tree_item(
        tree_item_relative(tree, TVGN_NEXT as usize, display),
        "root 入力支援 item",
    );
    let prediction = require_tree_item(
        tree_item_relative(tree, TVGN_CHILD as usize, input_support),
        "推測変換 child item",
    );
    let association = require_tree_item(
        tree_item_relative(tree, TVGN_NEXT as usize, prediction),
        "連想変換 child item",
    );
    let profile = require_tree_item(
        tree_item_relative(tree, TVGN_NEXT as usize, input_support),
        "root アプリ別の設定 item",
    );

    match label {
        "基本" => basic,
        "入力補助" => input_assist,
        "変換補助" => conversion,
        "文節変換" => segment,
        "文字幅・句読点" | "文字幅・句読点 (reset)" => normalizer,
        "表示" => display,
        "入力支援" => input_support,
        "推測変換" => prediction,
        "連想変換" => association,
        "アプリ別の設定" => profile,
        _ => panic!("unknown input TreeView label {label:?}"),
    }
}

fn tree_item_screen_rect(tree: HWND, item: HTREEITEM) -> RECT {
    let remote = RemoteTreeRect::allocate(window_process_id(tree));
    let mut query = RECT::default();
    // SAFETY: TreeView's `TVM_GETITEMRECT` contract aliases the first pointer-
    // sized bytes of the RECT input as an HTREEITEM. The full 16-byte RECT is
    // allocated in the target process before this `WM_USER`-range message.
    unsafe {
        std::ptr::write((&mut query as *mut RECT).cast::<HTREEITEM>(), item);
    }
    remote.write(&query);
    // SAFETY: the LPARAM points at the remote buffer above, which remains live
    // until after the target TreeView synchronously writes the rectangle.
    let obtained = unsafe {
        SendMessageW(
            tree,
            TVM_GETITEMRECT,
            // Request the text rectangle rather than the whole row: TreeView
            // does not use TVS_FULLROWSELECT, so the center of an otherwise
            // valid full-row rectangle can be inert whitespace.
            Some(WPARAM(1)),
            Some(LPARAM(remote.address as isize)),
        )
    };
    assert_ne!(obtained.0, 0, "get live TreeView item rectangle");
    let client = remote.read();
    assert!(
        client.right > client.left && client.bottom > client.top,
        "TreeView returned a nonempty item rectangle: {client:?}"
    );
    let mut top_left = POINT {
        x: client.left,
        y: client.top,
    };
    let mut bottom_right = POINT {
        x: client.right,
        y: client.bottom,
    };
    // SAFETY: both points are valid client coordinates for the live TreeView.
    assert_ne!(unsafe { ClientToScreen(tree, &mut top_left) }, 0);
    // SAFETY: both points are valid client coordinates for the live TreeView.
    assert_ne!(unsafe { ClientToScreen(tree, &mut bottom_right) }, 0);
    RECT {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    }
}

fn ensure_tree_item_visible(tree: HWND, item: HTREEITEM) {
    // SAFETY: `item` is the live TreeView-owned opaque handle obtained above.
    // TVM_ENSUREVISIBLE only scrolls the native control; it does not change its
    // selection or synthesize the user action under test.
    unsafe {
        let _ = SendMessageW(
            tree,
            TVM_ENSUREVISIBLE,
            Some(WPARAM(0)),
            Some(LPARAM(item.0)),
        );
    }
}

fn click_tree_row_until(
    label: &str,
    cursor: &CursorRestore,
    tree: HWND,
    predicate: impl Fn() -> bool,
) {
    let item = input_tree_item(tree, label);
    ensure_tree_item_visible(tree, item);
    let rect = tree_item_screen_rect(tree, item);
    let point = POINT {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
    };
    // SAFETY: `point` is the center of the live target item's text rectangle.
    // This proves the following pointer input is not row scanning and lands on
    // an interactive TreeView label rather than inert row whitespace.
    assert_eq!(unsafe { WindowFromPoint(point) }, tree);
    cursor.left_click(point);
    let goal = format!("physical TreeView click reaches {label}");
    wait_until(&goal, predicate);
}

fn list_item_rect(list: HWND, index: usize) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: `rect` is valid writable storage for the synchronous ListBox query.
    let result = unsafe {
        SendMessageW(
            list,
            LB_GETITEMRECT,
            Some(WPARAM(index)),
            Some(LPARAM((&mut rect as *mut RECT).cast::<c_void>() as isize)),
        )
    };
    assert_ne!(result.0, -1, "get ListBox item rectangle");
    rect
}

fn list_value(list: HWND, message: u32) -> usize {
    // SAFETY: both supported messages are scalar ListBox queries with no pointer payload.
    let result = unsafe { SendMessageW(list, message, Some(WPARAM(0)), Some(LPARAM(0))).0 };
    usize::try_from(result).expect("ListBox scalar result")
}

fn combo_selection(combo: HWND) -> isize {
    // CB_GETCURSEL is a scalar query; unlike a pointer-bearing list message it
    // does not cross a process boundary with caller-owned storage.
    // SAFETY: `combo` is a live native ComboBox HWND discovered from the
    // isolated settings payload; the message carries no caller-owned pointer.
    unsafe { SendMessageW(combo, CB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 }
}

fn tree_color(tree: HWND, message: u32) -> isize {
    // TVM_GETBKCOLOR and TVM_GETTEXTCOLOR are scalar TreeView queries; no
    // caller-owned buffer crosses the isolated settings-process boundary.
    // SAFETY: `tree` is a live native TreeView discovered from the fixture.
    unsafe { SendMessageW(tree, message, Some(WPARAM(0)), Some(LPARAM(0))).0 }
}

fn button_checked(button: HWND) -> isize {
    // BM_GETCHECK is a scalar query; the native radio HWND remains in the
    // isolated settings process for the duration of this test.
    // SAFETY: the message carries no caller-owned pointer.
    unsafe { SendMessageW(button, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0 }
}

fn combo_popup(combo: HWND) -> Option<HWND> {
    let owner_rect = window_rect(combo);
    let owner_pid = window_process_id(combo);
    top_level_windows().into_iter().find(|window| {
        class_name(*window) == "ComboLBox"
            && is_visible(*window)
            && window_process_id(*window) == owner_pid
            && {
                let popup = window_rect(*window);
                popup.left == owner_rect.left
                    && popup.right == owner_rect.right
                    && popup.bottom > popup.top
            }
    })
}

fn select_combo_item_with_mouse(cursor: &CursorRestore, combo: HWND, index: usize) {
    let rect = window_rect(combo);
    let open_point = POINT {
        // The native arrow area opens the list through the actual ComboBox
        // control; no CB_SHOWDROPDOWN or other synthetic message is used.
        x: rect.right - 8,
        y: (rect.top + rect.bottom) / 2,
    };
    // SAFETY: `open_point` is derived from the live ComboBox screen rectangle.
    let hit = unsafe { WindowFromPoint(open_point) };
    assert!(
        is_descendant_or_self(hit, combo),
        "ComboBox opening point must hit its native control (point {:?}, hit {:?} class={}, combo {:?})",
        open_point,
        hit,
        class_name(hit),
        combo
    );
    cursor.left_click(open_point);
    let start = Instant::now();
    let popup = loop {
        if let Some(popup) = combo_popup(combo) {
            break popup;
        }
        assert!(
            start.elapsed() < READY_TIMEOUT,
            "ComboBox {:?} did not publish a native ComboLBox popup",
            combo
        );
        sleep(INPUT_SETTLING);
    };
    let count = list_value(popup, LB_GETCOUNT);
    assert!(
        index < count,
        "ComboBox popup has {count} items, requested {index}"
    );
    // SAFETY: the popup HWND is a live native ComboLBox from the same payload;
    // LB_GETITEMHEIGHT is a scalar query with no caller-owned buffer.
    let row_height = unsafe {
        SendMessageW(
            popup,
            LB_GETITEMHEIGHT_SCALAR,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        )
        .0
    };
    assert!(
        row_height > 0,
        "native ComboLBox reports a positive row height"
    );
    let popup_rect = window_rect(popup);
    let row_point = POINT {
        x: (popup_rect.left + popup_rect.right) / 2,
        y: popup_rect.top + 2 + index as i32 * row_height as i32 + row_height as i32 / 2,
    };
    // SAFETY: `row_point` is derived from the live popup rectangle and row
    // height returned by User32.
    let row_hit = unsafe { WindowFromPoint(row_point) };
    assert_eq!(
        class_name(row_hit),
        "ComboLBox",
        "physical row point must hit the native ComboLBox"
    );
    cursor.left_click(row_point);
    wait_until("native ComboBox selection", || {
        combo_selection(combo) == index as isize
    });
}

fn list_text(list: HWND, index: usize) -> String {
    // SAFETY: `LB_GETTEXTLEN` is a scalar query for this live ListBox.
    let length =
        unsafe { SendMessageW(list, LB_GETTEXTLEN, Some(WPARAM(index)), Some(LPARAM(0))).0 };
    assert!(length >= 0, "ListBox text length is valid");
    let mut text = vec![0u16; length as usize + 1];
    // SAFETY: the vector has room for the synchronous UTF-16 result.
    let copied = unsafe {
        SendMessageW(
            list,
            LB_GETTEXT,
            Some(WPARAM(index)),
            Some(LPARAM(text.as_mut_ptr().cast::<c_void>() as isize)),
        )
        .0
    };
    assert!(copied >= 0, "ListBox text query succeeds");
    String::from_utf16_lossy(&text[..copied as usize])
}

fn top_level_windows() -> Vec<HWND> {
    direct_children(HWND::default())
}

fn direct_children(parent: HWND) -> Vec<HWND> {
    let mut windows = Vec::new();
    let mut previous = HWND::default();
    loop {
        // SAFETY: null class/title pointers enumerate the next direct child below `parent`.
        let next = unsafe { FindWindowExW(parent, previous, core::ptr::null(), core::ptr::null()) };
        if next.is_invalid() {
            return windows;
        }
        windows.push(next);
        previous = next;
    }
}

fn find_direct_child(parent: HWND, expected_class: &str) -> Option<HWND> {
    direct_children(parent)
        .into_iter()
        .find(|window| class_name(*window) == expected_class)
}

fn find_direct_child_with_text(parent: HWND, expected: &str) -> Option<HWND> {
    direct_children(parent)
        .into_iter()
        .find(|window| status_text(*window) == expected)
}

fn window_process_id(window: HWND) -> u32 {
    let mut process_id = 0;
    // SAFETY: `process_id` is valid writable storage for the synchronous scalar query.
    unsafe {
        let _ = GetWindowThreadProcessId(window, &mut process_id);
    }
    process_id
}

fn window_thread_id(window: HWND) -> u32 {
    // SAFETY: the target window is a live top-level HWND; the process output is
    // intentionally null because only the owning UI thread is needed here.
    unsafe { GetWindowThreadProcessId(window, null_mut()) }
}

fn focused_window(thread_id: u32) -> HWND {
    // Querying another GUI thread's focus is reliable while this test thread
    // is temporarily attached to its input queue.  The attachment is always
    // balanced before returning.
    // SAFETY: this scalar query reads the calling test thread identifier.
    let current_thread = unsafe { GetCurrentThreadId() };
    let attached = current_thread != thread_id
        // SAFETY: both IDs came from User32 and the successful attachment is
        // balanced below before this helper returns.
        && unsafe { AttachThreadInput(current_thread, thread_id, 1) != 0 };
    // SAFETY: after a successful attachment, GetFocus reads the shared input
    // queue without crossing a caller-owned pointer boundary.
    let focused = unsafe { GetFocus() };
    if attached {
        // SAFETY: balances the exact successful attachment above.
        let _ = unsafe { AttachThreadInput(current_thread, thread_id, 0) };
    }
    assert!(
        !focused.is_invalid(),
        "GetFocus must return a live settings HWND"
    );
    focused
}

fn class_name(window: HWND) -> String {
    let mut buffer = [0u16; 128];
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetClassNameW(window: HWND, buffer: *mut u16, max_count: i32) -> i32;
    }
    // SAFETY: `buffer` is a valid UTF-16 output buffer for the synchronous class-name query.
    let length =
        unsafe { GetClassNameW(window, buffer.as_mut_ptr(), buffer.len() as i32) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..length])
}

fn is_descendant_or_self(window: HWND, ancestor: HWND) -> bool {
    let mut current = window;
    for _ in 0..16 {
        if current == ancestor {
            return true;
        }
        if current.is_invalid() {
            return false;
        }
        // SAFETY: `current` is a live HWND returned by WindowFromPoint or a
        // parent query, and GetParent has no caller-owned output buffer.
        current = unsafe { GetParent(current) };
    }
    false
}

fn status_text(window: HWND) -> String {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowTextLengthW(window: HWND) -> i32;
        fn GetWindowTextW(window: HWND, buffer: *mut u16, max_count: i32) -> i32;
    }
    // SAFETY: this scalar query reads only the live control's text length.
    let length = unsafe { GetWindowTextLengthW(window) }.max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    // SAFETY: `buffer` has space for the length plus its NUL terminator throughout the copy.
    let copied =
        unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

fn is_visible(window: HWND) -> bool {
    // SAFETY: this scalar query reads visibility for a handle enumerated from this fixture tree.
    unsafe { IsWindowVisible(window) != 0 }
}

fn window_rect(window: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: `rect` is valid writable storage for the synchronous rectangle query.
    let read = unsafe { GetWindowRect(window, &mut rect) };
    assert_ne!(read, 0, "read HWND rectangle");
    rect
}

fn scale_metric(value: i32, from: u32, to: u32) -> i32 {
    let from = i64::from(from.max(1));
    let to = i64::from(to.max(1));
    (i64::from(value)
        .saturating_mul(to)
        .saturating_add(from / 2)
        .checked_div(from)
        .unwrap_or(i64::from(value))) as i32
}
