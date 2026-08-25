//! The pad's own scroll rails.
//!
//! Windows draws a scroll bar at the width the system says and in the colours
//! the theme says, and neither is settable per window: `SetWindowTheme` only
//! chooses which theme class draws it, and `SM_CXVSCROLL` belongs to the
//! machine. A pad whose two panes stand on two different grounds wants two
//! rails that each stand on their own, thinner than a control borrowed from
//! Explorer, so the pad draws them itself.
//!
//! Each rail is a child window beside the pane it scrolls. It reads the pane's
//! position rather than keeping one, so the two can never disagree: whatever
//! moves the pane — the wheel, a key, a caret leaving the view, a memo added
//! to the list — moves the thumb on the next repaint, with no scroll state
//! stored twice.
//!
//! The geometry is pure and tested without a window; the window procedure only
//! reads the pane, calls it, and paints.

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, EndPaint, GetDC, GetTextMetricsW,
    InvalidateRect, ReleaseDC, RoundRect, ScreenToClient, SelectObject, HDC, PAINTSTRUCT, PS_SOLID,
    TEXTMETRICW,
};
use windows::Win32::UI::Controls::{
    EM_GETFIRSTVISIBLELINE, EM_GETLINECOUNT, EM_LINESCROLL, WM_MOUSELEAVE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, GetClientRect, GetCursorPos, GetPropW,
    GetWindowLongPtrW, LoadCursorW, RegisterClassW, RemovePropW, SendMessageW, SetPropW,
    SetWindowLongPtrW, SystemParametersInfoW, GWLP_USERDATA, GWLP_WNDPROC, HMENU, IDC_ARROW,
    LB_GETCOUNT, LB_GETITEMHEIGHT, LB_GETTOPINDEX, LB_SETTOPINDEX, SPI_GETWHEELSCROLLLINES,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WHEEL_DELTA, WM_DESTROY, WM_GETFONT, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT, WNDCLASSW, WNDPROC,
    WS_CHILD, WS_VISIBLE,
};

use crate::theme::{fill_color, scaled, select_font};

/// The strip a rail occupies beside its pane, at 96 DPI.
///
/// Narrower than the system scroll bar by design: the pad's rail is a reading
/// of how far down the pane is, not a control to be aimed at first — the wheel
/// and the keyboard are how the panes are actually scrolled — and it is still
/// wide enough to grab.
pub(crate) const SCROLL_RAIL_96: i32 = 10;

/// The visible bar inside that strip.
const THUMB_96: i32 = 4;

/// A thumb shorter than this stops reading as a thumb, however long the
/// document behind it is.
const THUMB_MIN_96: i32 = 24;

const RAIL_CLASS: PCWSTR = windows::core::w!("SakuraInputPadRail");

/// Where a watched pane remembers the rail that reads it.
const RAIL_PROP: PCWSTR = windows::core::w!("SakuraInputPadRailOwner");

/// How the pane beside the rail counts what it is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scrolls {
    /// A `LISTBOX`, which scrolls by whole rows of a fixed height.
    Rows,
    /// A multi-line `EDIT`, which scrolls by wrapped lines.
    Lines,
}

/// One rail's state, owned by its window through `GWLP_USERDATA`.
struct Rail {
    target: HWND,
    scrolls: Scrolls,
    track: COLORREF,
    thumb: COLORREF,
    hot: COLORREF,
    hovered: bool,
    /// Where inside the thumb a drag took hold, in pixels from its top.
    grab: Option<i32>,
}

/// Where the thumb sits in a track of `track` pixels, or `None` when the pane
/// shows everything it has and there is nothing to report.
///
/// `total` and `visible` are in the pane's own units — rows or lines — and
/// `first` is the one at the top of the view.
pub(crate) fn thumb(
    track: i32,
    total: i32,
    visible: i32,
    first: i32,
    minimum: i32,
) -> Option<(i32, i32)> {
    if track <= 0 || visible <= 0 || total <= visible {
        return None;
    }
    let height = (i64::from(track) * i64::from(visible) / i64::from(total)) as i32;
    let height = height.clamp(minimum.min(track).max(1), track);
    let steps = total - visible;
    let span = track - height;
    let first = first.clamp(0, steps);
    let top = if span <= 0 || steps <= 0 {
        0
    } else {
        (i64::from(span) * i64::from(first) / i64::from(steps)) as i32
    };
    Some((top, height))
}

/// The line or row a thumb dragged to `top` is asking for.
///
/// The inverse of [`thumb`], rounded to the nearest step so that letting go
/// anywhere inside a step lands on it rather than always on the one above.
pub(crate) fn first_from_thumb(track: i32, total: i32, visible: i32, height: i32, top: i32) -> i32 {
    let steps = (total - visible).max(0);
    let span = track - height;
    if steps <= 0 || span <= 0 {
        return 0;
    }
    let top = top.clamp(0, span);
    ((i64::from(steps) * i64::from(top) + i64::from(span) / 2) / i64::from(span)) as i32
}

/// Registers the rail class. Harmless to call more than once.
pub(crate) fn register_class() {
    // SAFETY: the class name and procedure are static, and re-registering an
    // existing class simply fails.
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(rail_procedure),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: RAIL_CLASS,
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
    }
}

/// Creates a rail for `target` inside `parent`.
///
/// The rail is not a tab stop and never takes focus: it is a reading of the
/// pane beside it, and a keyboard that stopped on it would be stopping on the
/// same pane twice.
pub(crate) fn create(parent: HWND, target: HWND, scrolls: Scrolls, id: u16) -> Option<HWND> {
    register_class();
    // SAFETY: the class is registered above and every pointer is static or
    // lives through this synchronous call.
    let window = unsafe {
        CreateWindowExW(
            Default::default(),
            RAIL_CLASS,
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(id as *mut c_void)),
            None,
            None,
        )
    }
    .ok()?;
    let state = Box::new(Rail {
        target,
        scrolls,
        track: COLORREF(0),
        thumb: COLORREF(0),
        hot: COLORREF(0),
        hovered: false,
        grab: None,
    });
    // SAFETY: the window owns the box from here; `WM_NCDESTROY` takes it back.
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(state) as isize);
    }
    watch(target, scrolls, window);
    Some(window)
}

/// Gives the rail the ground it stands on and the two colours its thumb has.
pub(crate) fn set_colors(rail: HWND, track: COLORREF, thumb: COLORREF, hot: COLORREF) {
    // SAFETY: the caller passes the rail `create` returned, whose state box
    // lives until the window is destroyed, and the borrow ends here.
    let Some(state) = (unsafe { state_of(rail) }) else {
        return;
    };
    if state.track == track && state.thumb == thumb && state.hot == hot {
        return;
    }
    state.track = track;
    state.thumb = thumb;
    state.hot = hot;
    // SAFETY: the window is live.
    unsafe {
        let _ = InvalidateRect(Some(rail), None, false);
    }
}

/// The rail state behind a window handle.
///
/// # Safety
///
/// `rail` must be a live window of this module's class, and the returned
/// reference must not outlive the message being handled.
unsafe fn state_of<'a>(rail: HWND) -> Option<&'a mut Rail> {
    if rail.is_invalid() {
        return None;
    }
    // SAFETY: the caller promises a live rail window, whose user data is the
    // box `create` put there and `WM_NCDESTROY` has not yet taken back.
    let pointer = unsafe { GetWindowLongPtrW(rail, GWLP_USERDATA) } as *mut Rail;
    // SAFETY: as above.
    unsafe { pointer.as_mut() }
}

/// The height of the client area of a window, in pixels.
fn height_of(window: HWND) -> i32 {
    let mut client = RECT::default();
    // SAFETY: the window and the output rectangle are live.
    if unsafe { GetClientRect(window, &mut client) }.is_err() {
        return 0;
    }
    client.bottom.saturating_sub(client.top)
}

/// What the pane is showing: how much it has, how much fits, and where it is.
fn reading(rail: &Rail) -> (i32, i32, i32) {
    if rail.target.is_invalid() {
        return (0, 0, 0);
    }
    // SAFETY: the target is a live sibling control and these messages only
    // read its scroll position and extent.
    let ask = |message: u32, w: usize| unsafe {
        SendMessageW(rail.target, message, Some(WPARAM(w)), Some(LPARAM(0))).0 as i32
    };
    let height = height_of(rail.target);
    match rail.scrolls {
        Scrolls::Rows => {
            let row = ask(LB_GETITEMHEIGHT, 0).max(1);
            (ask(LB_GETCOUNT, 0), height / row, ask(LB_GETTOPINDEX, 0))
        }
        Scrolls::Lines => {
            let line = line_height(rail.target).max(1);
            (
                ask(EM_GETLINECOUNT, 0),
                height / line,
                ask(EM_GETFIRSTVISIBLELINE, 0),
            )
        }
    }
}

/// Puts the pane's view at `first`, in the pane's own units.
fn scroll_to(rail: &Rail, first: i32) {
    let (total, visible, current) = reading(rail);
    let first = first.clamp(0, (total - visible).max(0));
    if first == current || rail.target.is_invalid() {
        return;
    }
    // SAFETY: the target is a live sibling control.
    unsafe {
        match rail.scrolls {
            Scrolls::Rows => {
                let _ = SendMessageW(
                    rail.target,
                    LB_SETTOPINDEX,
                    Some(WPARAM(first as usize)),
                    Some(LPARAM(0)),
                );
            }
            Scrolls::Lines => {
                let _ = SendMessageW(
                    rail.target,
                    EM_LINESCROLL,
                    Some(WPARAM(0)),
                    Some(LPARAM((first - current) as isize)),
                );
            }
        }
    }
}

/// One line of the font a text pane is drawing with, in pixels.
///
/// A multi-line `EDIT` scrolls by lines, and how many of them fit is the
/// client height over this. Falling back to one pixel would claim a pane holds
/// its whole document, so a font that cannot be measured reports nothing to
/// scroll at all.
fn line_height(target: HWND) -> i32 {
    // SAFETY: the target is a live control; the DC is released on both paths.
    let dc = unsafe { GetDC(Some(target)) };
    if dc.is_invalid() {
        return 0;
    }
    // SAFETY: the control is live and `WM_GETFONT` only reads its font.
    let font = unsafe { SendMessageW(target, WM_GETFONT, None, None) };
    let restore = select_font(
        dc,
        windows::Win32::Graphics::Gdi::HFONT(font.0 as *mut c_void),
    );
    let mut metrics = TEXTMETRICW::default();
    // SAFETY: the DC is live and the structure is ours.
    let measured = unsafe { GetTextMetricsW(dc, &mut metrics) }.as_bool();
    if let Some(restore) = restore {
        // SAFETY: `restore` is the object the DC held before `select_font`.
        unsafe {
            SelectObject(dc, restore);
        }
    }
    // SAFETY: pairs with the `GetDC` above.
    unsafe {
        ReleaseDC(Some(target), dc);
    }
    if measured {
        (metrics.tmHeight + metrics.tmExternalLeading).max(1)
    } else {
        0
    }
}

/// Where the pointer is inside the rail, in rail client pixels.
fn pointer_y(rail: HWND) -> Option<i32> {
    let mut point = POINT::default();
    // SAFETY: the output point is live.
    unsafe { GetCursorPos(&mut point) }.ok()?;
    // SAFETY: the window is live and the point is in screen coordinates.
    unsafe { ScreenToClient(rail, &mut point) }.ok().ok()?;
    Some(point.y)
}

unsafe extern "system" fn rail_procedure(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
) -> LRESULT {
    // SAFETY: the state box lives from `create` until `WM_NCDESTROY` below.
    let Some(rail) = (unsafe { state_of(window) }) else {
        // SAFETY: the window is live for the whole message.
        return unsafe { DefWindowProcW(window, message, w, l) };
    };
    let track = height_of(window);
    let dpi = crate::pad::dpi_of(window);
    let minimum = scaled(THUMB_MIN_96, dpi);
    let (total, visible, first) = reading(rail);
    match message {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: every valid paint DC is paired with `EndPaint`.
            let dc = unsafe { BeginPaint(window, &mut ps) };
            if !dc.is_invalid() {
                let mut client = RECT::default();
                // SAFETY: the window and the rectangle are live.
                if unsafe { GetClientRect(window, &mut client) }.is_ok() {
                    paint(
                        dc,
                        client,
                        dpi,
                        rail,
                        thumb(track, total, visible, first, minimum),
                    );
                }
                // SAFETY: pairs with `BeginPaint`.
                unsafe {
                    let _ = EndPaint(window, &ps);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let y = (l.0 >> 16) as i16 as i32;
            if let Some((top, height)) = thumb(track, total, visible, first, minimum) {
                if y >= top && y < top + height {
                    rail.grab = Some(y - top);
                } else {
                    // Outside the thumb the rail behaves as a scroll bar's
                    // track does: one view towards where the pointer is.
                    let page = visible.max(1);
                    scroll_to(rail, if y < top { first - page } else { first + page });
                }
                // SAFETY: the window is live and capture is released on
                // `WM_LBUTTONUP`.
                unsafe {
                    SetCapture(window);
                }
                // SAFETY: the window is live.
                unsafe {
                    let _ = InvalidateRect(Some(window), None, false);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let (Some(grab), Some((_, height))) =
                (rail.grab, thumb(track, total, visible, first, minimum))
            {
                let y = (l.0 >> 16) as i16 as i32;
                scroll_to(
                    rail,
                    first_from_thumb(track, total, visible, height, y - grab),
                );
                // SAFETY: the window is live.
                unsafe {
                    let _ = InvalidateRect(Some(window), None, false);
                }
            } else if !rail.hovered {
                rail.hovered = true;
                let mut leaving = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                // SAFETY: the structure is filled in above and the window is
                // live.
                unsafe {
                    let _ = TrackMouseEvent(&mut leaving);
                }
                // SAFETY: the window is live.
                unsafe {
                    let _ = InvalidateRect(Some(window), None, false);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            rail.hovered = false;
            // SAFETY: the window is live.
            unsafe {
                let _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            rail.grab = None;
            // SAFETY: capture was taken on the button going down.
            unsafe {
                let _ = ReleaseCapture();
            }
            // The pointer may have left while the button was held, and no
            // `WM_MOUSELEAVE` arrives while the rail has capture.
            rail.hovered = pointer_y(window).is_some_and(|y| y >= 0 && y < track);
            // SAFETY: the window is live.
            unsafe {
                let _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        }
        // The wheel belongs to the pane, not to the strip beside it.
        WM_MOUSEWHEEL if !rail.target.is_invalid() => {
            // SAFETY: the target is a live sibling control.
            unsafe { SendMessageW(rail.target, message, Some(w), Some(l)) }
        }
        WM_DESTROY => {
            rail.grab = None;
            // SAFETY: the window is live for the whole message.
            unsafe { DefWindowProcW(window, message, w, l) }
        }
        WM_NCDESTROY => {
            // SAFETY: the pointer is the box `create` handed the window, and
            // taking it back here is the one place it is freed.
            unsafe {
                let pointer = SetWindowLongPtrW(window, GWLP_USERDATA, 0) as *mut Rail;
                if !pointer.is_null() {
                    drop(Box::from_raw(pointer));
                }
            }
            // SAFETY: the window is live for the whole message.
            unsafe { DefWindowProcW(window, message, w, l) }
        }
        // SAFETY: the window is live for the whole message.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

fn paint(dc: HDC, client: RECT, dpi: u32, rail: &Rail, thumb: Option<(i32, i32)>) {
    // The track is the pane's own ground, so a pane with nothing to scroll
    // has no rail on it at all — only a slightly wider margin.
    fill_color(dc, &client, rail.track);
    let Some((top, height)) = thumb else {
        return;
    };
    let width = scaled(THUMB_96, dpi);
    let inset = ((client.right - client.left - width) / 2).max(0);
    let bar = RECT {
        left: client.left + inset,
        top: client.top + top,
        right: client.right - inset,
        bottom: client.top + top + height,
    };
    let color = if rail.hovered || rail.grab.is_some() {
        rail.hot
    } else {
        rail.thumb
    };
    let radius = (bar.right - bar.left).max(1);
    // SAFETY: both objects are created, selected, restored and deleted here,
    // so the DC leaves holding what it arrived with.
    unsafe {
        let pen = CreatePen(PS_SOLID, 1, color);
        let brush = CreateSolidBrush(color);
        if pen.is_invalid() || brush.is_invalid() {
            if !pen.is_invalid() {
                let _ = DeleteObject(pen.into());
            }
            if !brush.is_invalid() {
                let _ = DeleteObject(brush.into());
            }
            // A square thumb is the wrong shape; no thumb is not a shape.
            fill_color(dc, &bar, color);
            return;
        }
        let previous_pen = SelectObject(dc, pen.into());
        let previous_brush = SelectObject(dc, brush.into());
        let _ = RoundRect(dc, bar.left, bar.top, bar.right, bar.bottom, radius, radius);
        SelectObject(dc, previous_brush);
        SelectObject(dc, previous_pen);
        let _ = DeleteObject(brush.into());
        let _ = DeleteObject(pen.into());
    }
}

/// The class procedure of each watched pane, one slot per pane.
///
/// Two slots rather than one shared: the list and the body are different
/// classes, and a single slot would make each pane's hook depend on which of
/// the two happened to be created first.
static ROWS_PROC: AtomicIsize = AtomicIsize::new(0);
static LINES_PROC: AtomicIsize = AtomicIsize::new(0);

fn slot(scrolls: Scrolls) -> &'static AtomicIsize {
    match scrolls {
        Scrolls::Rows => &ROWS_PROC,
        Scrolls::Lines => &LINES_PROC,
    }
}

/// Subclasses a pane so its rail repaints whenever the pane moves.
///
/// Nothing tells a window that the control beside it scrolled: the wheel, the
/// arrow keys, a caret pushed past the last visible line and a memo added to
/// the list all move a pane without a single notification. Watching the pane
/// itself is the one place that sees all of them.
fn watch(target: HWND, scrolls: Scrolls, rail: HWND) {
    if target.is_invalid() {
        return;
    }
    // SAFETY: both windows are live children created on this thread, and the
    // property is removed when the pane is destroyed.
    unsafe {
        let _ = SetPropW(target, RAIL_PROP, Some(HANDLE(rail.0)));
    }
    let ours = match scrolls {
        Scrolls::Rows => rows_proc as *const () as isize,
        Scrolls::Lines => lines_proc as *const () as isize,
    };
    // SAFETY: the target is a live child created on this thread.
    let previous = unsafe { SetWindowLongPtrW(target, GWLP_WNDPROC, ours) };
    // Never remember our own procedure: `CallWindowProcW` would recurse until
    // the stack ended.
    if previous != 0 && previous != ours {
        let _ = slot(scrolls).compare_exchange(0, previous, Ordering::Relaxed, Ordering::Relaxed);
    }
}

unsafe extern "system" fn rows_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: the window is live for the whole message.
    unsafe { watched(Scrolls::Rows, window, message, w, l) }
}

unsafe extern "system" fn lines_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: the window is live for the whole message.
    unsafe { watched(Scrolls::Lines, window, message, w, l) }
}

/// How many of the pane's own units one notch of the wheel moves.
///
/// `0` means the user asked for no wheel scrolling at all, and the whole
/// visible height means they asked for a page at a time.
fn wheel_lines(visible: i32) -> i32 {
    let mut lines: u32 = 3;
    // SAFETY: the buffer is the `u32` the call is being asked to fill, and
    // nothing is being written back to the system.
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some(std::ptr::from_mut(&mut lines).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if read.is_err() {
        return 3;
    }
    // `WHEEL_PAGESCROLL`, the settable value that means a screenful.
    if lines == u32::MAX {
        return visible.max(1);
    }
    lines as i32
}

/// Whole notches in what the wheel has turned, plus what was carried from
/// the last message, and what this one leaves to carry in turn.
///
/// Both `LISTBOX` and `EDIT` keep their wheel handling in the scroll bar they
/// were given, so a pane with no `WS_VSCROLL` is a pane the wheel does
/// nothing to. The rails took the bars away and so owe the panes the wheel,
/// at the number of lines the user set for every other window. What did not
/// amount to a whole notch is kept for the next message, which is what makes
/// a high-resolution wheel move smoothly instead of in jumps.
fn notches(turned: i32, carried: i32) -> (i32, i32) {
    let delta = turned.saturating_add(carried);
    (delta / WHEEL_DELTA as i32, delta % WHEEL_DELTA as i32)
}

/// Scrolls a pane by one turn of the wheel.
fn wheeled(scrolls: Scrolls, window: HWND, w: WPARAM) {
    let (total, visible, first) = view(scrolls, window);
    let lines = wheel_lines(visible);
    if lines == 0 || total <= visible {
        return;
    }
    let key = window.0 as isize;
    let carried = CARRY.get();
    let turned = (w.0 >> 16) as u16 as i16 as i32;
    let (notches, carry) = notches(turned, if carried.0 == key { carried.1 } else { 0 });
    CARRY.set((key, carry));
    if notches == 0 {
        return;
    }
    scroll_to(&probe_of(scrolls, window), first - notches * lines);
}

/// Runs the pane's own procedure, then repaints its rail if the view moved.
///
/// # Safety
///
/// `window` must be a live pane this module subclassed.
unsafe fn watched(scrolls: Scrolls, window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // Asking a pane where it is scrolled is itself a message, and it arrives
    // back here: probing the probe would recurse until the stack ended, and
    // the probe asks several questions — the position, the extent, the row
    // height, the font — so listing them would be a list to keep in step with
    // `reading`. The flag covers whatever the probe happens to ask.
    // `WM_NCDESTROY` is left out for the opposite reason: afterwards the pane
    // is gone and cannot have scrolled on the way out.
    // The wheel is answered here rather than passed on, because the pane
    // has nothing to pass it to.
    if message == WM_MOUSEWHEEL && !PROBING.get() {
        wheeled(scrolls, window, w);
        return LRESULT(0);
    }
    let quiet = PROBING.get() || message == WM_NCDESTROY;
    let before = (!quiet).then(|| view(scrolls, window));
    let captured = slot(scrolls).load(Ordering::Relaxed);
    let result = if captured == 0 {
        // SAFETY: the class procedure was never captured, so the default
        // handler is the only destination left.
        unsafe { DefWindowProcW(window, message, w, l) }
    } else {
        // SAFETY: `captured` came from `SetWindowLongPtrW(GWLP_WNDPROC)`, so
        // it is the control's class procedure and has this signature.
        let previous: WNDPROC = Some(unsafe {
            std::mem::transmute::<
                isize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(captured)
        });
        // SAFETY: the control is live for the whole message.
        unsafe { CallWindowProcW(previous, window, message, w, l) }
    };
    if before.is_some_and(|before| view(scrolls, window) != before) {
        // SAFETY: the property is the rail handle `watch` stored, and both
        // windows outlive the pane's own message.
        unsafe {
            let rail = HWND(GetPropW(window, RAIL_PROP).0);
            if !rail.is_invalid() {
                let _ = InvalidateRect(Some(rail), None, false);
            }
        }
    }
    if message == WM_NCDESTROY {
        // SAFETY: the pane is still addressable during `WM_NCDESTROY`, and
        // this is where what `watch` stored is taken back.
        unsafe {
            let _ = RemovePropW(window, RAIL_PROP);
        }
    }
    result
}

// Set while a pane is being asked what it is showing. Every question the
// reading asks is a message to the same pane, and the subclass below sees them
// all. One thread owns every pad window, so a plain thread-local flag is the
// whole of the mutual exclusion needed.
thread_local! {
    static PROBING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The three numbers a rail draws from, read straight off the pane.
fn view(scrolls: Scrolls, window: HWND) -> (i32, i32, i32) {
    let rail = probe_of(scrolls, window);
    PROBING.set(true);
    let reading = reading(&rail);
    PROBING.set(false);
    reading
}

/// A stand-in for reading and scrolling a pane without its rail: the colours
/// and the drag belong to a window, and asking a pane where it is does not.
fn probe_of(scrolls: Scrolls, window: HWND) -> Rail {
    Rail {
        target: window,
        scrolls,
        track: COLORREF(0),
        thumb: COLORREF(0),
        hot: COLORREF(0),
        hovered: false,
        grab: None,
    }
}

// What one wheel message turned but did not amount to a whole notch, and the
// pane it was turned over. One thread owns every pad window, and the wheel
// goes to one pane at a time, so a single carried remainder is enough.
thread_local! {
    static CARRY: std::cell::Cell<(isize, i32)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_notch_of_the_wheel_is_one_step_and_leaves_nothing_behind() {
        assert_eq!(notches(120, 0), (1, 0));
        assert_eq!(notches(-120, 0), (-1, 0));
    }

    #[test]
    fn a_wheel_that_reports_fractions_of_a_notch_keeps_the_remainder() {
        // A high-resolution wheel sends a third of a notch at a time: three
        // of them are one step, and none of them is lost on the way.
        let (first, carried) = notches(40, 0);
        assert_eq!((first, carried), (0, 40));
        let (second, carried) = notches(40, carried);
        assert_eq!((second, carried), (0, 80));
        let (third, carried) = notches(40, carried);
        assert_eq!((third, carried), (1, 0));
    }

    #[test]
    fn a_pane_showing_everything_has_no_thumb() {
        assert_eq!(thumb(100, 5, 5, 0, 20), None, "exactly full");
        assert_eq!(thumb(100, 3, 5, 0, 20), None, "less than full");
        assert_eq!(thumb(0, 50, 5, 0, 20), None, "no track to draw in");
        assert_eq!(thumb(100, 50, 0, 0, 20), None, "no room to show anything");
    }

    #[test]
    fn the_thumb_stays_inside_its_track_and_keeps_a_readable_length() {
        for track in [24, 100, 431, 1080] {
            for total in [2, 7, 500, 65_536] {
                for visible in [1, 3, 40] {
                    for first in [-5, 0, 1, total / 2, total, total * 2] {
                        let Some((top, height)) = thumb(track, total, visible, first, 24) else {
                            continue;
                        };
                        assert!(height >= 1, "{track} {total} {visible}");
                        assert!(
                            height <= track,
                            "a thumb never outgrows its track: {track} {total} {visible}"
                        );
                        assert!(
                            height >= 24.min(track),
                            "a long document still gets a grabbable thumb: \
                             {track} {total} {visible}"
                        );
                        assert!(top >= 0, "{track} {total} {visible} {first}");
                        assert!(
                            top + height <= track,
                            "the bottom of the document is the bottom of the track: \
                             {track} {total} {visible} {first}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_ends_of_the_document_are_the_ends_of_the_track() {
        let (top, height) = thumb(200, 100, 10, 0, 24).expect("a scrollable pane");
        assert_eq!(top, 0, "the first row puts the thumb at the top");
        let (bottom, _) = thumb(200, 100, 10, 90, 24).expect("a scrollable pane");
        assert_eq!(bottom + height, 200, "the last row puts it at the bottom");
    }

    #[test]
    fn dragging_the_thumb_asks_for_the_row_it_was_dropped_on() {
        for first in [0, 1, 17, 89, 90] {
            let (top, height) = thumb(200, 100, 10, first, 24).expect("a scrollable pane");
            assert_eq!(
                first_from_thumb(200, 100, 10, height, top),
                first,
                "a thumb put where a row places it asks for that row"
            );
        }
    }

    #[test]
    fn a_drag_past_either_end_stops_at_the_end() {
        let (_, height) = thumb(200, 100, 10, 0, 24).expect("a scrollable pane");
        assert_eq!(first_from_thumb(200, 100, 10, height, -400), 0);
        assert_eq!(first_from_thumb(200, 100, 10, height, 4_000), 90);
    }

    #[test]
    fn a_pane_with_nothing_to_scroll_answers_a_drag_with_its_first_row() {
        assert_eq!(first_from_thumb(200, 5, 10, 200, 40), 0);
        assert_eq!(first_from_thumb(0, 100, 10, 0, 40), 0);
    }
}
