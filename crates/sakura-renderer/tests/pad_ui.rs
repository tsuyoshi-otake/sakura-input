//! Real-process coverage for the two Sakura Pad shapes.
//!
//! The unit tests in `pad.rs` grade the arrangement function. They cannot
//! prove that the function is the thing the window actually obeys, because
//! `sakura-renderer` is a binary and an integration test links none of it.
//! This fixture therefore measures the live child windows of a real renderer
//! process and checks the promises the shapes make, independently of the
//! constants that produced them.
//!
//! Isolation follows the candidate fixtures: the test owns a uniquely named
//! pipe and serves the renderer's `UiState` itself, and `LOCALAPPDATA` points
//! at the target temp directory. The installed IME, the production pipe, and
//! the user's own memos are never touched.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, sleep, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_ipc::{security::Descriptor, Client, PipeInstance};
use sakura_proto::{
    decode_request, encode_response, AppearanceTheme, PadShortcut, Request, Response, UiState,
    PROTOCOL_VERSION,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    DrawTextW, GetDC, GetDIBits, GetWindowDC, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DT_CALCRECT, DT_NOPREFIX, DT_SINGLELINE, HFONT,
    SRCCOPY,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetClientRect, GetDlgItem, GetSystemMetrics, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, SendMessageW,
    SetForegroundWindow, SetWindowPos, BN_CLICKED, EN_CHANGE, GWL_EXSTYLE, GW_OWNER, SM_CXVSCROLL,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, WM_APP, WM_COMMAND, WM_GETFONT, WM_GETTEXT,
    WM_GETTEXTLENGTH, WM_SETTEXT, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

const PATIENT: Duration = Duration::from_secs(5);
const TEST_PIPE_PREFIX: &str = r"\\.\pipe\SakuraInputRendererTest-";
const HOST_CLASS: PCWSTR = windows::core::w!("SakuraInputRenderer");
const PAD_CLASS: PCWSTR = windows::core::w!("SakuraInputPad");
/// `WM_PAD_TRIGGER` in `main.rs`. The gesture that normally sends it is a
/// Ctrl double tap, which a test cannot deliver without typing into whatever
/// the user has in front.
const WM_PAD_TRIGGER: u32 = WM_APP + 6;

/// Control identifiers, mirrored from `pad.rs`.
///
/// An integration test links only the binary's command line, so these are
/// deliberately duplicated: they are part of what the window exposes, and a
/// copy here fails loudly if the window ever stops offering them.
const MENU_ID: i32 = 101;
const COUNT_ID: i32 = 102;
const SEARCH_ID: i32 = 104;
const LIST_ID: i32 = 105;
const STATUS_ID: i32 = 106;
const HEADER_TITLE_ID: i32 = 107;
const TITLE_ID: i32 = 108;
const BODY_ID: i32 = 109;
const NEW_ID: i32 = 110;
const SORT_ID: i32 = 111;
const SYNC_ID: i32 = 112;
const SHARE_ID: i32 = 113;
const DELETE_ID: i32 = 114;
const LIST_RAIL_ID: i32 = 115;
const BODY_RAIL_ID: i32 = 116;

const ALL_CONTROLS: [(&str, i32); 15] = [
    ("menu", MENU_ID),
    ("count", COUNT_ID),
    ("search", SEARCH_ID),
    ("list", LIST_ID),
    ("status", STATUS_ID),
    ("header-title", HEADER_TITLE_ID),
    ("title", TITLE_ID),
    ("body", BODY_ID),
    ("new", NEW_ID),
    ("sort", SORT_ID),
    ("sync", SYNC_ID),
    ("share", SHARE_ID),
    ("delete", DELETE_ID),
    ("list-rail", LIST_RAIL_ID),
    ("body-rail", BODY_RAIL_ID),
];

/// Comfortably above the 520 logical px breakpoint.
const WIDE_LOGICAL: i32 = 760;
/// Below the breakpoint and above the 480 logical px minimum.
const NARROW_LOGICAL: i32 = 500;
const TALL_LOGICAL: i32 = 600;
/// Comfortably inside the renderer's fifteen-second watch budget.
const HEARTBEAT: Duration = Duration::from_secs(3);

#[test]
#[ignore = "real renderer process; requires an interactive Windows desktop"]
fn the_pad_splits_above_the_breakpoint_and_folds_below_it() {
    let app_data = IsolatedAppData::new("pad-ui");
    let mut engine = FixtureEngine::new(initial_state());
    let renderer = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_sakura_renderer")))
        .arg("--test-pipe")
        .arg(engine.pipe_name())
        .env("LOCALAPPDATA", app_data.path())
        .spawn()
        .expect("spawn test-owned renderer");
    let mut renderer = OwnedChild::new(renderer, "renderer");

    let host = wait_for_renderer_window(renderer.pid(), HOST_CLASS, false);
    // SAFETY: `host` belongs to the child this test owns and is live until
    // that child exits, which happens below.
    unsafe {
        PostMessageW(Some(host), WM_PAD_TRIGGER, WPARAM(0), LPARAM(0))
            .expect("post the pad trigger");
    }
    let pad = wait_for_renderer_window(renderer.pid(), PAD_CLASS, true);
    // SAFETY: `pad` is a live top-level window of the owned child.
    unsafe {
        let _ = SetForegroundWindow(pad);
    }

    // The pad is summoned by a gesture and dismissed with a key, and it has no
    // business in the taskbar. Windows gives a top-level window a button when
    // it asks with `WS_EX_APPWINDOW` or when it is nobody's, so the rule is
    // asserted as Windows states it rather than by looking at the taskbar.
    // `WS_EX_TOOLWINDOW` is the other way to be left out and is checked too:
    // it would shrink the caption of a window that shows a title.
    // SAFETY: `pad` is a live top-level window of the owned child.
    let (ex_style, owner) = unsafe {
        (
            GetWindowLongPtrW(pad, GWL_EXSTYLE) as u32,
            GetWindow(pad, GW_OWNER).ok(),
        )
    };
    assert_eq!(
        ex_style & WS_EX_APPWINDOW.0,
        0,
        "the pad must not ask for a taskbar button"
    );
    assert_eq!(
        ex_style & WS_EX_TOOLWINDOW.0,
        0,
        "and must not pay for it with the small tool caption"
    );
    assert!(
        owner.is_some_and(|owner| !owner.is_invalid()),
        "an unowned window gets a taskbar button whatever its styles say"
    );

    seed_memos(pad);

    // --- Two panes -------------------------------------------------------
    resize_client(pad, WIDE_LOGICAL, TALL_LOGICAL);
    let wide = wait_for_shape(pad, "wide", |shape| {
        shape.visible("list") && shape.visible("body")
    });
    capture(pad, "wide");

    assert!(
        !wide.visible("menu"),
        "the two-pane shape shows both panes at once, so nothing needs a pane toggle: {wide:?}"
    );
    for name in ["list", "search", "title", "body", "share", "delete"] {
        assert!(wide.visible(name), "the two-pane shape must show {name}");
    }
    let list = wide.rect("list");
    let body = wide.rect("body");
    assert!(
        list.right <= body.left,
        "the list is a column beside the editor, not behind it: {list:?} then {body:?}"
    );
    assert!(
        wide.rect("search").right <= body.left,
        "the search box belongs to the list column"
    );
    // The column is the list and the rail that reads it, so the edge every
    // control below is measured against is the rail's.
    let column = wide.rect("list-rail").right;
    for name in ["new", "sort", "sync"] {
        assert!(
            wide.rect(name).right <= column,
            "in the two-pane shape the bottom bar is the list column's own bar, so {name} may not \
             reach under the editor"
        );
    }
    for name in ["share", "delete"] {
        assert!(
            wide.rect(name).left >= column,
            "{name} acts on the open memo, so in the two-pane shape it belongs to the editor"
        );
        assert!(
            wide.rect(name).bottom <= body.top,
            "{name} sits in the editor's meta row, above the body"
        );
    }
    // The search box says what it is for while it is empty. That hint has to
    // be paint: were it the field's text, the filter would read it and the
    // resting pad would list no memo at all.
    // SAFETY: `pad` is live and the identifier is a plain integer.
    let search = unsafe { GetDlgItem(Some(pad), SEARCH_ID) }.expect("the search field exists");
    assert_eq!(
        text_of(search),
        "",
        "the search field must rest empty, whatever it draws in itself"
    );
    // Both panes scroll, and in the two-pane shape their scroll bars stand
    // side by side a few hundred pixels apart. A LISTBOX takes its bar away
    // when the rows fit and a multi-line EDIT keeps its own on screen either
    // way, so left alone the two panes disagree about whether a pane has a
    // gutter at all — which is what the owner saw. Measured rather than
    // asserted from the styles: what matters is that the two panes reserve the
    // same width, in the resting state where nothing has to scroll.
    // SAFETY: `pad` is live and the identifiers are plain integers.
    let (list_control, body_control) = unsafe {
        (
            GetDlgItem(Some(pad), LIST_ID).expect("the list exists"),
            GetDlgItem(Some(pad), BODY_ID).expect("the body exists"),
        )
    };
    // Neither pane carries a system scroll bar any more: the pad draws its
    // own rail beside each, so a pane's window is its client area.
    assert_eq!(
        (scroll_gutter(list_control), scroll_gutter(body_control)),
        (0, 0),
        "a system scroll bar in either pane is the one the rails replaced"
    );
    // SAFETY: `pad` is live.
    let dpi = unsafe { GetDpiForWindow(pad) } as i32;
    for (pane, rail) in [("list", "list-rail"), ("body", "body-rail")] {
        let pane_rect = wide.rect(pane);
        let rail_rect = wide.rect(rail);
        assert_eq!(
            rail_rect.left, pane_rect.right,
            "{rail} stands against {pane} with nothing between them"
        );
        assert_eq!(
            (rail_rect.top, rail_rect.bottom),
            (pane_rect.top, pane_rect.bottom),
            "{rail} runs the height of {pane}"
        );
        let width = rail_rect.right - rail_rect.left;
        // SAFETY: the metric takes no pointer and is valid on any thread.
        let system_bar = unsafe { GetSystemMetrics(SM_CXVSCROLL) };
        assert_eq!(
            width,
            10 * dpi / 96,
            "{rail} is the pad's own width rather than the system's"
        );
        assert!(
            width < system_bar,
            "{rail} is thinner than the scroll bar it replaced"
        );
    }
    // A LISTBOX rounds its own height down to a whole number of rows unless it
    // is told not to, and hands the remainder back as bare pad surface. That
    // is what the empty band between the last memo and the bar was. Read from
    // the real control, because the placement was always right and the style
    // was not: the bar's own top is where the list has to reach.
    let bar_top = wide.rect("new").top - 8 * dpi / 96;
    assert!(
        wide.rect("list").bottom >= bar_top,
        "the list must reach the bar rather than stop a part-row short of it: \
         {:?} against a bar starting at {bar_top}",
        wide.rect("list")
    );
    // A notice the slot cannot hold is not a notice: `Markdown をコピーしました`
    // arrived as `Markdown をコピ`. The sync notice lands in the same slot and
    // is used here because, unlike the copy, running this test must not take
    // the desktop's clipboard away from whoever is at it.
    // SAFETY: `pad` is live and the identifier is a plain integer.
    let status_control = unsafe { GetDlgItem(Some(pad), STATUS_ID) }.expect("the status exists");
    click(pad, SYNC_ID);
    let notice = wait_for_text(status_control, "a notice", |value| value.contains("GitHub"));
    let slot = client_rect(status_control);
    assert!(
        slot.right - slot.left >= text_extent(status_control),
        "the slot must hold the whole notice rather than an ellipsis: {notice:?}          in {slot:?}"
    );
    // And it has to give the row back: a notice has no successor to replace
    // it, so without an expiry it sits in the row for the rest of the session.
    // The resting row reports nothing at all — saving is the ordinary state
    // and the memo's time is already in its list row — so what the notice
    // gives back is the whole slot.
    let returned = wait_for_text(status_control, "a row with nothing to report", |value| {
        value.is_empty()
    });
    assert!(returned.is_empty(), "the notice cleared");
    assert!(
        !visible(status_control),
        "a slot held open for a reading that is not there is room the memo's          name could have used"
    );

    wide.assert_disjoint_and_inside();

    // --- One pane ---------------------------------------------------------
    // Seeding left the pad on the memo it created last, and narrowing does not
    // change which pane the user is in: it changes how many of them fit. So
    // the folded window arrives showing the editor, and the toggle is what
    // takes the list back.
    resize_client(pad, NARROW_LOGICAL, TALL_LOGICAL);
    let folded = wait_for_shape(pad, "one pane", |shape| {
        shape.visible("list") != shape.visible("body")
    });
    assert!(
        folded.visible("body"),
        "creating a memo opens it, and folding the window must not silently \
         abandon the memo the user is writing: {folded:?}"
    );

    // --- One pane, list ---------------------------------------------------
    click(pad, MENU_ID);
    let list_pane = wait_for_shape(pad, "narrow list", |shape| {
        shape.visible("list") && !shape.visible("body")
    });
    capture(pad, "narrow-list");

    assert!(
        list_pane.visible("menu"),
        "one pane at a time needs a way back to the other one"
    );
    for name in ["title", "body"] {
        assert!(
            !list_pane.visible(name),
            "the list pane must not leave the editor's {name} on screen"
        );
    }
    for name in ["share", "delete", "new", "sort", "sync"] {
        assert!(
            list_pane.visible(name),
            "the one-pane bottom bar carries every action, including {name}"
        );
        assert!(
            list_pane.rect(name).top >= list_pane.rect("list").bottom,
            "{name} belongs to the bottom bar, below the list"
        );
    }
    assert!(
        list_pane.rect("delete").right >= list_pane.rect("share").right,
        "delete is the destructive action and sits at the far end of the bar"
    );
    list_pane.assert_disjoint_and_inside();

    // --- One pane, editor ------------------------------------------------
    click(pad, MENU_ID);
    let editor_pane = wait_for_shape(pad, "narrow editor", |shape| {
        shape.visible("body") && !shape.visible("list")
    });
    capture(pad, "narrow-editor");

    assert!(
        editor_pane.visible("menu"),
        "the toggle must survive the trip so the list is reachable again"
    );
    for name in ["list", "search"] {
        assert!(
            !editor_pane.visible(name),
            "the editor pane must not leave {name} on screen"
        );
    }
    assert!(
        editor_pane.visible("title") && editor_pane.visible("body"),
        "the editor pane shows the memo it opened"
    );
    editor_pane.assert_disjoint_and_inside();

    // And back again, so the toggle is proven to be a toggle rather than a
    // one-way trip into the editor.
    click(pad, MENU_ID);
    wait_for_shape(pad, "narrow list again", |shape| {
        shape.visible("list") && !shape.visible("body")
    });

    // Widening restores both panes without needing the toggle at all.
    resize_client(pad, WIDE_LOGICAL, TALL_LOGICAL);
    wait_for_shape(pad, "wide again", |shape| {
        shape.visible("list") && shape.visible("body") && !shape.visible("menu")
    });

    engine.stop();
    renderer.wait_for_exit();
}

/// Opens a real pad, seeds it, and leaves it on screen to be looked at.
///
/// Not an assertion — a way to review the design against the wireframe by
/// hand. It runs only when named explicitly, and it uses the same isolation
/// as the test above, so the memos it creates are the fixture's own and the
/// installed IME is untouched. Closing the pad window ends it; so does the
/// deadline, which keeps a forgotten run from holding a process all day.
#[test]
#[ignore = "design review harness; hold this open by hand, not in CI"]
fn the_pad_stays_open_for_a_look() {
    let hold = std::env::var("SAKURA_PAD_PREVIEW_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(Duration::from_secs(900), Duration::from_secs);

    let app_data = IsolatedAppData::new("pad-preview");
    let mut engine = FixtureEngine::new(initial_state());
    let renderer = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_sakura_renderer")))
        .arg("--test-pipe")
        .arg(engine.pipe_name())
        .env("LOCALAPPDATA", app_data.path())
        .spawn()
        .expect("spawn test-owned renderer");
    let mut renderer = OwnedChild::new(renderer, "renderer");

    let host = wait_for_renderer_window(renderer.pid(), HOST_CLASS, false);
    // SAFETY: `host` belongs to the child this test owns and is live until
    // that child exits, which happens below.
    unsafe {
        PostMessageW(Some(host), WM_PAD_TRIGGER, WPARAM(0), LPARAM(0))
            .expect("post the pad trigger");
    }
    let pad = wait_for_renderer_window(renderer.pid(), PAD_CLASS, true);
    // SAFETY: `pad` is a live top-level window of the owned child.
    unsafe {
        let _ = SetForegroundWindow(pad);
    }
    seed_memos(pad);
    // SAFETY: as above.
    unsafe {
        let _ = SetForegroundWindow(pad);
    }

    println!(
        "Sakura Pad is open. Resize it across 520 logical px to see both shapes.          Close the window when you are done; otherwise this ends in {} seconds.",
        hold.as_secs()
    );

    // Closing the pad hides it rather than destroying it, so visibility is
    // what says the reviewer is finished.
    let deadline = Instant::now() + hold;
    let mut beat = Instant::now();
    while Instant::now() < deadline {
        // SAFETY: the window belongs to the child this test owns.
        if !unsafe { IsWindowVisible(pad) }.as_bool() {
            break;
        }
        if beat.elapsed() >= HEARTBEAT {
            engine.heartbeat();
            beat = Instant::now();
        }
        sleep(Duration::from_millis(200));
    }

    engine.stop();
    renderer.wait_for_exit();
}

/// Every control's rectangle in one arrangement, in client coordinates.
#[derive(Debug)]
struct Shape {
    client: RECT,
    controls: Vec<(&'static str, Option<RECT>)>,
}

impl Shape {
    fn read(pad: HWND) -> Self {
        let origin = client_origin(pad);
        let controls = ALL_CONTROLS
            .iter()
            .map(|&(name, id)| {
                // SAFETY: `pad` is live and the identifier is a plain integer.
                let child = unsafe { GetDlgItem(Some(pad), id) };
                let rect = child.ok().filter(|&child| visible(child)).map(|child| {
                    let screen = window_rect(child);
                    RECT {
                        left: screen.left - origin.0,
                        top: screen.top - origin.1,
                        right: screen.right - origin.0,
                        bottom: screen.bottom - origin.1,
                    }
                });
                (name, rect)
            })
            .collect();
        Self {
            client: client_rect(pad),
            controls,
        }
    }

    fn visible(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    fn find(&self, name: &str) -> Option<RECT> {
        self.controls
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .and_then(|(_, rect)| *rect)
    }

    fn rect(&self, name: &str) -> RECT {
        self.find(name)
            .unwrap_or_else(|| panic!("{name} is not on screen in this shape: {self:?}"))
    }

    /// No control may cover another, and none may hang off the window.
    ///
    /// This is the property a user notices when it breaks — a clipped button,
    /// a label painted over an edit field — and it is checked here on the real
    /// windows rather than on the numbers that placed them.
    fn assert_disjoint_and_inside(&self) {
        let placed: Vec<(&str, RECT)> = self
            .controls
            .iter()
            .filter_map(|&(name, rect)| rect.map(|rect| (name, rect)))
            .collect();
        for &(name, rect) in &placed {
            assert!(
                rect.left >= self.client.left
                    && rect.top >= self.client.top
                    && rect.right <= self.client.right
                    && rect.bottom <= self.client.bottom,
                "{name} at {rect:?} hangs outside the client area {:?}",
                self.client
            );
            assert!(
                rect.right > rect.left && rect.bottom > rect.top,
                "{name} at {rect:?} is visible but has no area"
            );
        }
        for (index, &(name, rect)) in placed.iter().enumerate() {
            for &(other_name, other) in &placed[index + 1..] {
                let overlaps = rect.left < other.right
                    && other.left < rect.right
                    && rect.top < other.bottom
                    && other.top < rect.bottom;
                assert!(
                    !overlaps,
                    "{name} at {rect:?} covers {other_name} at {other:?}"
                );
            }
        }
    }
}

fn wait_for_shape(pad: HWND, label: &str, settled: impl Fn(&Shape) -> bool) -> Shape {
    let deadline = Instant::now() + PATIENT;
    loop {
        let shape = Shape::read(pad);
        if settled(&shape) {
            return shape;
        }
        assert!(
            Instant::now() < deadline,
            "the pad never reached its {label} shape: {shape:?}"
        );
        sleep(Duration::from_millis(20));
    }
}

/// Fills the pad with a few memos through its own controls.
///
/// Seeding through the window rather than the storage file keeps the fixture
/// honest: it exercises the same create-and-capture path the user drives, and
/// it needs no knowledge of the protected document format.
fn seed_memos(pad: HWND) {
    const MEMOS: [(&str, &str); 6] = [
        (
            "ようこそ！",
            "Sakura Pad へようこそ。ダブル Ctrl で開きます。",
        ),
        (
            "1番目のメモ",
            "cargo test --workspace が通ることを確認する。",
        ),
        ("買い物リスト", "コーヒー豆\r\n牛乳\r\nキーキャップ"),
        (
            "2番目のメモ",
            "TSF の edit session は同期要求が拒否されたら非同期へ落ちる。",
        ),
        (
            "3番目のメモ",
            "GitHub 同期は Device Flow を使う。client_secret を同梱しない。",
        ),
        // One memo long enough to need the editor's scroll bar, so that a
        // look at the pad shows both scroll bars in the same state and any
        // difference between them is a difference in design.
        (
            "長いメモ",
            "1 行目。このメモは縦スクロールを起こす長さがある。
2 行目。
             3 行目。
4 行目。
5 行目。
6 行目。
7 行目。
             8 行目。
9 行目。
10 行目。
11 行目。
12 行目。
             13 行目。
14 行目。
15 行目。
16 行目。
17 行目。
             18 行目。
19 行目。
20 行目。
21 行目。
22 行目。
             23 行目。
24 行目。
25 行目。",
        ),
    ];
    for (title, body) in MEMOS {
        click(pad, NEW_ID);
        sleep(Duration::from_millis(120));
        set_text(pad, TITLE_ID, title);
        set_text(pad, BODY_ID, body);
        notify(pad, TITLE_ID, EN_CHANGE as u16);
        // The pad batches edits behind a short timer before it writes.
        sleep(Duration::from_millis(250));
    }
}

fn click(pad: HWND, id: i32) {
    notify(pad, id, BN_CLICKED as u16);
}

fn notify(pad: HWND, id: i32, code: u16) {
    let packed = (id as u32 & 0xffff) | ((code as u32) << 16);
    // SAFETY: `pad` is a live window owned by the child process.
    unsafe {
        PostMessageW(Some(pad), WM_COMMAND, WPARAM(packed as usize), LPARAM(0))
            .expect("post a pad command");
    }
}

/// Types into one of the pad's edit controls.
///
/// `SetWindowTextW` is deliberately not used: it does not cross a process
/// boundary for a control, and it reports success anyway, which is how an
/// earlier version of this fixture managed to seed five memos that were all
/// empty. `WM_SETTEXT` is marshalled by `SendMessageW` and does arrive.
fn set_text(pad: HWND, id: i32, value: &str) {
    // SAFETY: `pad` is live and the identifier is a plain integer.
    let child = unsafe { GetDlgItem(Some(pad), id) }.expect("the control exists");
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    wide.push(0);
    // SAFETY: the buffer is NUL-terminated and outlives this synchronous send.
    let set = unsafe {
        SendMessageW(
            child,
            WM_SETTEXT,
            Some(WPARAM(0)),
            Some(LPARAM(wide.as_ptr() as isize)),
        )
    };
    assert_eq!(set.0, 1, "the control accepted the text");
    assert_eq!(text_of(child), value, "the control kept the text");
}

/// Reads one of the pad's edit controls.
///
/// `GetWindowTextW` is avoided for the same reason as `SetWindowTextW`: across
/// a process boundary it returns an empty string for a control rather than
/// asking the owning thread. `WM_GETTEXT` is marshalled and does ask.
fn text_of(control: HWND) -> String {
    // SAFETY: `control` is live and the length query takes no buffer.
    let length = unsafe { SendMessageW(control, WM_GETTEXTLENGTH, None, None) }.0;
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0_u16; length as usize + 1];
    // SAFETY: the buffer holds the requested count and outlives this
    // synchronous send.
    let copied = unsafe {
        SendMessageW(
            control,
            WM_GETTEXT,
            Some(WPARAM(buffer.len())),
            Some(LPARAM(buffer.as_mut_ptr() as isize)),
        )
    }
    .0;
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

/// Waits for `control`'s text to satisfy `settled`, and returns it.
fn wait_for_text(control: HWND, label: &str, settled: impl Fn(&str) -> bool) -> String {
    // A notice outlives its own expiry, so this waits longer than the rest of
    // the fixture does.
    let deadline = Instant::now() + PATIENT + PATIENT;
    loop {
        let value = text_of(control);
        if settled(&value) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label}; the status reads {value:?}"
        );
        sleep(Duration::from_millis(50));
    }
}

/// How wide `control`'s own text is, drawn in `control`'s own font.
///
/// Measured rather than assumed: what makes a reading unreadable is the
/// relationship between the two, and neither number is interesting alone.
fn text_extent(control: HWND) -> i32 {
    let value = text_of(control);
    let Some(mut wide) = (!value.is_empty()).then(|| value.encode_utf16().collect::<Vec<u16>>())
    else {
        return 0;
    };
    // SAFETY: the control is live, the DC is released below, and the font is
    // borrowed from the control rather than owned here.
    unsafe {
        let dc = GetDC(Some(control));
        let font = HFONT(SendMessageW(control, WM_GETFONT, None, None).0 as *mut std::ffi::c_void);
        let restore = SelectObject(dc, font.into());
        let mut rect = RECT::default();
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, restore);
        ReleaseDC(Some(control), dc);
        rect.right - rect.left
    }
}

/// Resizes the pad so its *client* area is the requested logical size.
fn resize_client(pad: HWND, logical_width: i32, logical_height: i32) {
    // SAFETY: `pad` is a live window.
    let dpi = unsafe { GetDpiForWindow(pad) };
    let target_width = logical_width * dpi as i32 / 96;
    let target_height = logical_height * dpi as i32 / 96;
    let deadline = Instant::now() + PATIENT;
    loop {
        let outer = window_rect(pad);
        let client = client_rect(pad);
        let width = client.right - client.left;
        let height = client.bottom - client.top;
        if width == target_width && height == target_height {
            return;
        }
        let chrome_width = (outer.right - outer.left) - width;
        let chrome_height = (outer.bottom - outer.top) - height;
        // SAFETY: `pad` is live; the call only moves and sizes it.
        unsafe {
            SetWindowPos(
                pad,
                None,
                0,
                0,
                target_width + chrome_width,
                target_height + chrome_height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .expect("resize the pad");
        }
        assert!(
            Instant::now() < deadline,
            "the pad never accepted a {logical_width}x{logical_height} logical client"
        );
        sleep(Duration::from_millis(20));
    }
}

fn visible(window: HWND) -> bool {
    // SAFETY: the handle was just produced by `GetDlgItem` on a live parent.
    unsafe { IsWindowVisible(window) }.as_bool()
}

fn window_rect(window: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: `window` is live and `rect` is a valid out-pointer.
    unsafe {
        GetWindowRect(window, &mut rect).expect("window rectangle");
    }
    rect
}

/// How much of `window`'s width its vertical scroll bar takes.
///
/// Zero when there is no bar. The pad's scrolling controls carry no border
/// styles, so the difference between the window and its client area is the
/// bar and nothing else.
fn scroll_gutter(window: HWND) -> i32 {
    let outside = window_rect(window);
    let inside = client_rect(window);
    (outside.right - outside.left) - (inside.right - inside.left)
}

fn client_rect(window: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: `window` is live and `rect` is a valid out-pointer.
    unsafe {
        GetClientRect(window, &mut rect).expect("client rectangle");
    }
    rect
}

fn client_origin(window: HWND) -> (i32, i32) {
    let mut point = windows::Win32::Foundation::POINT { x: 0, y: 0 };
    // SAFETY: `window` is live and `point` is a valid in-out pointer.
    unsafe {
        let _ = ClientToScreen(window, &mut point);
    }
    (point.x, point.y)
}

/// Saves a screenshot of the pad when `SAKURA_PAD_UI_SHOTS` names a directory.
///
/// Off by default: the assertions above are the test, and a run that writes
/// nothing outside its own temp directory is the one that belongs in CI. The
/// variable exists so a human reviewing a layout change can see it.
fn capture(pad: HWND, name: &str) {
    let Some(directory) = std::env::var_os("SAKURA_PAD_UI_SHOTS") else {
        return;
    };
    let directory = PathBuf::from(directory);
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    sleep(Duration::from_millis(250));
    let rect = window_rect(pad);
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return;
    }
    let mut pixels = vec![0_u8; (width as usize) * (height as usize) * 4];
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Positive: bottom-up, which is the order a .bmp file wants.
            biHeight: height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: every handle below is created and released on this thread, each
    // call receives handles the immediately preceding call returned, and the
    // pixel buffer is large enough for the requested 32-bit rectangle.
    let copied = unsafe {
        let source = GetWindowDC(Some(pad));
        let memory = CreateCompatibleDC(Some(source));
        let bitmap = CreateCompatibleBitmap(source, width, height);
        let previous = SelectObject(memory, bitmap.into());
        let blitted = BitBlt(memory, 0, 0, width, height, Some(source), 0, 0, SRCCOPY).is_ok();
        let rows = GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(memory, previous);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(Some(pad), source);
        blitted && rows == height
    };
    if !copied {
        return;
    }
    let mut file = Vec::with_capacity(54 + pixels.len());
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
    file.extend_from_slice(&0_u32.to_le_bytes());
    file.extend_from_slice(&54_u32.to_le_bytes());
    file.extend_from_slice(&info.bmiHeader.biSize.to_le_bytes());
    file.extend_from_slice(&width.to_le_bytes());
    file.extend_from_slice(&height.to_le_bytes());
    file.extend_from_slice(&1_u16.to_le_bytes());
    file.extend_from_slice(&32_u16.to_le_bytes());
    file.extend_from_slice(&0_u32.to_le_bytes());
    file.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
    file.extend_from_slice(&[0_u8; 16]);
    file.extend_from_slice(&pixels);
    let _ = std::fs::write(directory.join(format!("{name}.bmp")), file);
}

fn initial_state() -> UiState {
    UiState {
        revision: 1,
        appearance_theme: AppearanceTheme::Light,
        // The gesture is not what opens the pad here; `WM_PAD_TRIGGER` is.
        // Leaving it disabled keeps this fixture from registering for raw
        // keyboard input on the desktop running the test.
        pad_shortcut: PadShortcut::Disabled,
        mode: None,
        candidates: None,
        candidate_detail: None,
        anchor: None,
        document: None,
        renderer_visible: false,
        stopping: false,
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
        // Claim the pipe before the renderer starts: a renderer that races a
        // not-yet-created fixture would take its production watchdog path and
        // launch the adjacent engine binary.
        let security = Descriptor::for_pipe().expect("fixture pipe security descriptor");
        let watch =
            PipeInstance::create(&pipe_name, &security, true).expect("create first fixture pipe");
        let history = PipeInstance::create(&pipe_name, &security, false)
            .expect("create history fixture pipe");
        let commit = PipeInstance::create(&pipe_name, &security, false)
            .expect("create candidate commit fixture pipe");
        let served = Arc::clone(&state);
        let thread = thread::spawn(move || {
            let watch_state = Arc::clone(&served);
            let watcher = thread::spawn(move || serve_connection(watch, watch_state));
            let history_state = Arc::clone(&served);
            let historian = thread::spawn(move || serve_connection(history, history_state));
            serve_connection(commit, served);
            historian.join().expect("history fixture must finish");
            watcher.join().expect("watch fixture must finish");
        });
        Self {
            pipe_name,
            state,
            thread: Some(thread),
        }
    }

    fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// A real engine publishes state every few seconds. The renderer's watch
    /// call gives up after fifteen and reads the silence as an engine that has
    /// gone, which hides the pad and ends the process — so an idle fixture has
    /// to keep beating or the window closes under whoever is looking at it.
    fn heartbeat(&self) {
        let (state, changed) = &*self.state;
        let mut current = state.lock().expect("fixture state lock");
        current.revision = current.revision.saturating_add(1);
        changed.notify_all();
    }

    fn stop(&mut self) {
        let (state, changed) = &*self.state;
        let mut current = state.lock().expect("fixture state lock");
        current.revision = current.revision.saturating_add(1);
        current.stopping = true;
        changed.notify_all();
        drop(current);
        // An early assertion failure can leave a fixture connection blocked in
        // `wait_for_client`. These bounded connections wake only this test's
        // own pipe and are dropped immediately.
        for _ in 0..3 {
            let _ = Client::connect_to(&self.pipe_name, Duration::from_millis(100));
        }
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

fn serve_connection(pipe: PipeInstance, state: Arc<(Mutex<UiState>, Condvar)>) {
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
            Request::WatchUi { since } => Response::Ui(wait_for_state(&state, since)),
            Request::DeleteHistoryCandidate { .. } => {
                Response::HistoryCandidateDeleted { removed: false }
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
            // Assertion unwinding can end the owned renderer before the final
            // response reaches it. That is cleanup, not a second failure.
            Err(sakura_ipc::Fault::Disconnected) => return,
            Err(error) => panic!("write fixture response: {error:?}"),
        }
        if matches!(response, Response::Ui(UiState { stopping: true, .. })) {
            return;
        }
    }
}

fn wait_for_state(state: &Arc<(Mutex<UiState>, Condvar)>, since: u64) -> UiState {
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

fn wait_for_renderer_window(renderer_pid: u32, class: PCWSTR, require_visible: bool) -> HWND {
    let deadline = Instant::now() + PATIENT;
    loop {
        if let Some(window) = find_renderer_window(renderer_pid, class, require_visible) {
            return window;
        }
        assert!(
            Instant::now() < deadline,
            "a renderer window of the expected class never appeared"
        );
        sleep(Duration::from_millis(20));
    }
}

fn find_renderer_window(renderer_pid: u32, class: PCWSTR, require_visible: bool) -> Option<HWND> {
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
        // SAFETY: `window` was returned by the preceding enumeration call and
        // `owner_pid` is a valid out-pointer.
        unsafe { GetWindowThreadProcessId(window, Some(&mut owner_pid)) };
        if owner_pid == renderer_pid && (!require_visible || visible(window)) {
            return Some(window);
        }
    }
}

struct OwnedChild {
    child: Child,
    label: &'static str,
}

impl OwnedChild {
    fn new(child: Child, label: &'static str) -> Self {
        Self { child, label }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + PATIENT;
        loop {
            match self.child.try_wait().expect("poll owned child") {
                Some(_) => return,
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!("{} did not exit after its fixture stopped", self.label);
                }
                None => sleep(Duration::from_millis(20)),
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
