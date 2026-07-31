//! The floating あ / A that appears when the mode changes (DESIGN 8).
//!
//! A borderless, layered, topmost popup that shows the new mode beside the
//! caret for a moment and then hides itself. It is deliberately *not* a
//! permanent overlay: the tray icon is what tells you the mode at rest, and
//! a window that follows the caret forever is a window that sooner or later
//! covers the thing you are typing.
//!
//! # Why it must never take focus
//!
//! This window appears while the user is typing into somebody else's
//! window. `WS_EX_NOACTIVATE` plus `SW_SHOWNOACTIVATE` is what stops it
//! from stealing the caret; without both, changing mode would deactivate
//! the application being typed into, which — for a text service — means
//! the composition it was in the middle of is torn down. `WS_EX_TOOLWINDOW`
//! keeps it out of Alt+Tab for the same reason a tooltip is not in Alt+Tab.
//!
//! # Placement
//!
//! Beside the caret when the foreground thread reports one, and near the
//! foreground window otherwise. The caret rectangle comes from
//! `GetGUIThreadInfo`, which reports it for any thread — that is the point
//! of it — where `GetCaretPos` only ever answers for the calling thread and
//! would report nothing useful from this process.

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetSysColor,
    InvalidateRect, COLOR_WINDOW, COLOR_WINDOWTEXT, HBRUSH, PAINTSTRUCT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetGUIThreadInfo,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, KillTimer,
    RegisterClassW, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, CS_HREDRAW, CS_VREDRAW, GUITHREADINFO, GWLP_USERDATA, HWND_TOPMOST, LWA_ALPHA,
    SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, WM_DESTROY, WM_PAINT,
    WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

use sakura_proto::Mode;

use crate::glyph;

/// The window class the popup registers.
const CLASS: PCWSTR = windows::core::w!("SakuraInputIndicator");

/// How long the indicator stays up, in milliseconds.
///
/// Long enough to read after a deliberate key press, short enough that it
/// is gone before it can be in the way of the next word. Windows' own IME
/// indicator sits in the same range.
const LINGER_MS: u32 = 1_200;

/// The timer that hides it. Any non-zero id; the window has only one.
const HIDE_TIMER: usize = 1;

/// The popup's size at 96 DPI, in pixels. Scaled per monitor on show.
const EDGE_AT_96_DPI: i32 = 56;

/// How far from the caret the popup sits, at 96 DPI.
const CARET_GAP_AT_96_DPI: i32 = 8;

/// Uniform opacity. Not fully opaque, so a glyph that lands on top of text
/// obscures it less; not so faint that it is hard to read on a busy
/// background.
const OPACITY: u8 = 235;

/// The floating mode indicator.
#[derive(Debug)]
pub struct Indicator {
    window: HWND,
}

impl Indicator {
    /// Registers the class and creates the popup, hidden.
    ///
    /// Created once at startup rather than per mode change: creating a
    /// window is the slow part, and a mode change should show something
    /// immediately.
    pub fn new() -> Result<Self> {
        // SAFETY: the class name is a static wide literal and the proc is a
        // real `extern "system"` function. A duplicate registration fails
        // harmlessly and is ignored, which is what makes this safe to call
        // more than once.
        unsafe {
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(procedure),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
        }

        // SAFETY: the class was just registered; every other argument is a
        // plain value or `None`.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                CLASS,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                EDGE_AT_96_DPI,
                EDGE_AT_96_DPI,
                None,
                None,
                None,
                None,
            )?
        };

        // SAFETY: `window` is live and layered, which is what this call
        // requires.
        unsafe {
            SetLayeredWindowAttributes(window, COLORREF(0), OPACITY, LWA_ALPHA)?;
        }

        Ok(Indicator { window })
    }

    /// Shows the glyph for `mode` beside the caret, restarting the timer.
    ///
    /// Restarting rather than ignoring a change while one is already up:
    /// the last mode pressed is the one the user is in, and it is the one
    /// they need to see for the full interval.
    pub fn show(&self, mode: Mode) {
        // SAFETY: `window` is live for this type's lifetime. The stored
        // value is read back only by `painted_mode`, which validates it.
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, glyph::code(mode));
        }

        let edge = scaled(self.window, EDGE_AT_96_DPI);
        let (x, y) = self.placement(edge);
        // SAFETY: every argument is a plain value; `HWND_TOPMOST` is the
        // documented ordering handle.
        unsafe {
            let _ = SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                x,
                y,
                edge,
                edge,
                SWP_NOACTIVATE,
            );
            let _ = InvalidateRect(Some(self.window), None, true);
            // `SW_SHOWNOACTIVATE`, not `SW_SHOW`: see the module docs.
            let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
            SetTimer(Some(self.window), HIDE_TIMER, LINGER_MS, None);
        }
    }

    /// Where an `edge`-sized popup should sit: beside the caret if there is
    /// one, near the foreground window if not, and on screen either way.
    fn placement(&self, edge: i32) -> (i32, i32) {
        let gap = scaled(self.window, CARET_GAP_AT_96_DPI);
        let anchor = caret_point().or_else(foreground_point);
        // SAFETY: no arguments; these are the primary monitor's dimensions.
        let (screen_w, screen_h) =
            unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };

        let (x, y) = match anchor {
            Some(point) => (point.x + gap, point.y + gap),
            // Bottom centre, which is where a user looks for a mode
            // indicator when there is no caret to attach it to.
            None => ((screen_w - edge) / 2, screen_h - edge * 3),
        };

        // Clamped rather than flipped: a popup that jumps to the other side
        // of the caret near a screen edge reads as a glitch, and the only
        // thing that actually matters is that all of it is visible.
        (
            x.clamp(0, (screen_w - edge).max(0)),
            y.clamp(0, (screen_h - edge).max(0)),
        )
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        // SAFETY: `window` was created by this type and is destroyed once.
        unsafe {
            let _ = DestroyWindow(self.window);
        }
    }
}

/// Scales a 96-DPI measurement for the monitor `window` is on.
fn scaled(window: HWND, at_96: i32) -> i32 {
    // SAFETY: `window` is live. A zero result means the call failed, which
    // the guard below turns back into the 96-DPI value.
    let dpi = unsafe { GetDpiForWindow(window) };
    if dpi == 0 {
        return at_96;
    }
    (at_96 * dpi as i32) / 96
}

/// The caret's bottom-left corner in screen coordinates, if the foreground
/// thread has one.
fn caret_point() -> Option<POINT> {
    let foreground = glyph::foreground()?;
    // SAFETY: `foreground` is a window handle from the OS; passing `None`
    // asks only for the thread id, which is all this needs.
    let thread = unsafe { GetWindowThreadProcessId(foreground, None) };
    if thread == 0 {
        return None;
    }

    let mut info = GUITHREADINFO {
        cbSize: core::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info.cbSize` is set, which is what the call validates.
    unsafe { GetGUIThreadInfo(thread, &mut info) }.ok()?;

    // An empty rectangle means the thread reported no caret — a browser
    // drawing its own, for instance — and the caller falls back.
    let caret = info.rcCaret;
    if caret.right == caret.left && caret.bottom == caret.top {
        return None;
    }

    let mut point = POINT {
        x: caret.left,
        y: caret.bottom,
    };
    // The rectangle is in the client coordinates of the window that owns
    // the caret, which is not necessarily the foreground window.
    let owner = if info.hwndCaret.is_invalid() {
        foreground
    } else {
        info.hwndCaret
    };
    // SAFETY: `owner` is live and `point` outlives the call.
    if !unsafe { ClientToScreen(owner, &mut point) }.as_bool() {
        // The window died between being reported and being asked about. The
        // caller falls back to the foreground window's corner, which is
        // where the indicator would have gone had there been no caret.
        return None;
    }
    Some(point)
}

/// The foreground window's bottom-left corner, for when there is no caret.
fn foreground_point() -> Option<POINT> {
    let window = glyph::foreground()?;
    let mut rect = RECT::default();
    // SAFETY: `window` is live and `rect` outlives the call.
    unsafe { GetWindowRect(window, &mut rect) }.ok()?;
    Some(POINT {
        x: rect.left,
        y: rect.bottom,
    })
}

/// The popup's message handler.
///
/// Painting and hiding only. It deliberately handles no input at all: the
/// window is `WS_EX_NOACTIVATE` and exists to be looked at, and a click
/// target floating over somebody else's text field is a way to lose a
/// keystroke, not a feature.
extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(window);
            LRESULT(0)
        }
        WM_TIMER if w.0 == HIDE_TIMER => {
            // SAFETY: `window` is the window this proc was called for, and
            // the timer is the one set in `show`.
            unsafe {
                let _ = KillTimer(Some(window), HIDE_TIMER);
                let _ = ShowWindow(window, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: cancelling a timer that was never set is harmless and
            // reported through the ignored result.
            unsafe {
                let _ = KillTimer(Some(window), HIDE_TIMER);
            }
            LRESULT(0)
        }
        // SAFETY: the default handler is what every unhandled message must
        // reach; the arguments are the ones this proc was given.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

/// Fills the background and draws the glyph.
///
/// Colours come from the system rather than being chosen here, so the
/// indicator follows a light or dark theme without knowing which one it is
/// in — and stays legible under a high-contrast theme, where hard-coded
/// colours are exactly what makes an overlay unreadable.
fn paint(window: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `window` is live; `EndPaint` is called with the same struct
    // below on every path.
    let dc = unsafe { BeginPaint(window, &mut ps) };
    if dc.is_invalid() {
        return;
    }

    let mut rect = RECT::default();
    // SAFETY: `window` is live and `rect` outlives the call.
    let sized = unsafe { GetClientRect(window, &mut rect) }.is_ok();

    if sized {
        // SAFETY: `GetSysColor` takes a documented index; the brush is
        // deleted immediately after the fill.
        unsafe {
            let paper: HBRUSH = CreateSolidBrush(COLORREF(GetSysColor(COLOR_WINDOW)));
            FillRect(dc, &rect, paper);
            let _ = DeleteObject(paper.into());
        }
        // SAFETY: reads back only what `show` wrote; `glyph::from_code`
        // rejects anything else, including the zero of a window that has
        // not been shown yet.
        let stored = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) };
        if let Some(mode) = glyph::from_code(stored) {
            // SAFETY: `GetSysColor` takes a documented index.
            let ink = COLORREF(unsafe { GetSysColor(COLOR_WINDOWTEXT) });
            glyph::draw_centered(dc, &rect, glyph::label(mode), ink);
        }
    }

    // SAFETY: pairs with the `BeginPaint` above, with the struct it filled.
    unsafe {
        let _ = EndPaint(window, &ps);
    }
}
