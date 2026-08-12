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
    BeginPaint, ClientToScreen, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, InvalidateRect, SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, FF_DONTCARE, FW_NORMAL, HBRUSH, OUT_TT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetGUIThreadInfo,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, KillTimer,
    RegisterClassW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW,
    GUITHREADINFO, GWLP_USERDATA, HWND_TOPMOST, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SW_HIDE,
    SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_DESTROY, WM_PAINT, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use sakura_proto::{AppearanceTheme, Mode};

use crate::{candidate, glyph};

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

/// Compact horizontal mode bar measurements at 96 DPI.
const WIDTH_AT_96_DPI: i32 = 220;

const fn logical_size() -> (i32, i32) {
    (WIDTH_AT_96_DPI, candidate::ROW_HEIGHT_96)
}

/// How far from the caret the popup sits, at 96 DPI.
const CARET_GAP_AT_96_DPI: i32 = 8;

fn popup_ex_style() -> WINDOW_EX_STYLE {
    WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE
}

fn popup_style() -> WINDOW_STYLE {
    WS_POPUP
}

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
                popup_ex_style(),
                CLASS,
                PCWSTR::null(),
                popup_style(),
                0,
                0,
                logical_size().0,
                logical_size().1,
                None,
                None,
                None,
                None,
            )?
        };

        Ok(Indicator { window })
    }

    /// Shows the glyph for `mode` beside the caret, restarting the timer.
    ///
    /// Restarting rather than ignoring a change while one is already up:
    /// the last mode pressed is the one the user is in, and it is the one
    /// they need to see for the full interval.
    pub fn show(&self, mode: Mode, theme: AppearanceTheme) {
        // SAFETY: `window` is live for this type's lifetime. The stored
        // value is read back only by `painted_mode`, which validates it.
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, encode_state(mode, theme));
        }

        let (logical_width, logical_height) = logical_size();
        let width = scaled(self.window, logical_width);
        let height = scaled(self.window, logical_height);
        let (x, y) = self.placement(width, height);
        // SAFETY: every argument is a plain value; `HWND_TOPMOST` is the
        // documented ordering handle.
        unsafe {
            let _ = SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE,
            );
            let _ = InvalidateRect(Some(self.window), None, true);
            // `SW_SHOWNOACTIVATE`, not `SW_SHOW`: see the module docs.
            let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
            SetTimer(Some(self.window), HIDE_TIMER, LINGER_MS, None);
        }
    }

    /// Where the popup should sit: beside the caret if there is
    /// one, near the foreground window if not, and on screen either way.
    fn placement(&self, width: i32, height: i32) -> (i32, i32) {
        let gap = scaled(self.window, CARET_GAP_AT_96_DPI);
        let anchor = caret_point().or_else(foreground_point);
        // SAFETY: no arguments; these are the primary monitor's dimensions.
        let (screen_w, screen_h) =
            unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };

        let (x, y) = match anchor {
            Some(point) => (point.x + gap, point.y + gap),
            // Bottom centre, which is where a user looks for a mode
            // indicator when there is no caret to attach it to.
            None => ((screen_w - width) / 2, screen_h - height * 3),
        };

        // Clamped rather than flipped: a popup that jumps to the other side
        // of the caret near a screen edge reads as a glitch, and the only
        // thing that actually matters is that all of it is visible.
        (
            x.clamp(0, (screen_w - width).max(0)),
            y.clamp(0, (screen_h - height).max(0)),
        )
    }
}

fn encode_state(mode: Mode, theme: AppearanceTheme) -> isize {
    glyph::code(mode) | (theme_code(theme) << 8)
}

fn decode_state(stored: isize) -> Option<(Mode, AppearanceTheme)> {
    let mode = glyph::from_code(stored & 0xff)?;
    let theme = match (stored >> 8) & 0xff {
        1 => AppearanceTheme::Auto,
        2 => AppearanceTheme::Light,
        3 => AppearanceTheme::Dark,
        _ => return None,
    };
    Some((mode, theme))
}

const fn theme_code(theme: AppearanceTheme) -> isize {
    match theme {
        AppearanceTheme::Auto => 1,
        AppearanceTheme::Light => 2,
        AppearanceTheme::Dark => 3,
    }
}

fn description(mode: Mode) -> &'static str {
    match mode {
        Mode::Hiragana => "ひらがなで入力します",
        Mode::Katakana => "全角カタカナで入力します",
        Mode::HalfKatakana => "半角カタカナで入力します",
        Mode::Direct => "直接入力します",
        Mode::HalfAlnum => "半角英数で入力します",
        Mode::FullAlnum => "全角英数で入力します",
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

/// Paints the same warm neutral Sakura surface as the candidate popup.
/// High contrast and automatic Windows theme resolution remain centralized in
/// `candidate::palette`, so the two renderer-owned surfaces cannot disagree.
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
        // SAFETY: reads back only what `show` wrote; `decode_state` rejects
        // anything else, including an indicator that has never been shown.
        let stored = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) };
        if let Some((mode, theme)) = decode_state(stored) {
            let palette = candidate::palette(theme);
            fill(dc, &rect, palette.surface);

            let accent = RECT {
                right: rect.left + scaled(window, candidate::RAIL_WIDTH_96),
                ..rect
            };
            fill(dc, &accent, palette.rail);
            draw_border(dc, rect, palette.border);

            let glyph_rect = RECT {
                left: accent.right,
                right: accent.right + scaled(window, candidate::NUMBER_WIDTH_96),
                top: rect.top + scaled(window, 2),
                bottom: rect.bottom - scaled(window, 2),
            };
            glyph::draw_centered(dc, &glyph_rect, glyph::label(mode), palette.ink);

            let label_rect = RECT {
                left: glyph_rect.right + scaled(window, candidate::GAP_96),
                right: rect.right - scaled(window, candidate::PADDING_96),
                ..rect
            };
            draw_description(
                dc,
                label_rect,
                description(mode),
                palette.annotation,
                window,
            );
        }
    }

    // SAFETY: pairs with the `BeginPaint` above, with the struct it filled.
    unsafe {
        let _ = EndPaint(window, &ps);
    }
}

fn fill(dc: windows::Win32::Graphics::Gdi::HDC, rect: &RECT, color: COLORREF) {
    // SAFETY: the brush is owned by this call and is deleted after the fill.
    unsafe {
        let brush: HBRUSH = CreateSolidBrush(color);
        if !brush.is_invalid() {
            let _ = FillRect(dc, rect, brush);
            let _ = DeleteObject(brush.into());
        }
    }
}

fn draw_border(dc: windows::Win32::Graphics::Gdi::HDC, rect: RECT, color: COLORREF) {
    let edges = [
        RECT {
            bottom: rect.top + 1,
            ..rect
        },
        RECT {
            top: rect.bottom - 1,
            ..rect
        },
        RECT {
            right: rect.left + 1,
            ..rect
        },
        RECT {
            left: rect.right - 1,
            ..rect
        },
    ];
    for edge in edges {
        fill(dc, &edge, color);
    }
}

fn draw_description(
    dc: windows::Win32::Graphics::Gdi::HDC,
    mut rect: RECT,
    text: &str,
    color: COLORREF,
    window: HWND,
) {
    let face: Vec<u16> = "Yu Gothic UI\0".encode_utf16().collect();
    // SAFETY: the face string is NUL-terminated and all numeric font
    // attributes are fixed UI values scaled for the current monitor.
    let font = unsafe {
        CreateFontW(
            -scaled(window, candidate::SUPPORT_FONT_96),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0).into(),
            PCWSTR(face.as_ptr()),
        )
    };
    if font.is_invalid() {
        return;
    }
    // SAFETY: the font and DC are live. The previous object is restored before
    // deleting the font, and DrawTextW receives a live UTF-16 buffer.
    unsafe {
        let previous = SelectObject(dc, font.into());
        let _ = SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, color);
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, previous);
        let _ = DeleteObject(font.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_and_theme_survives_the_window_word() {
        for mode in Mode::ALL {
            for theme in AppearanceTheme::ALL {
                assert_eq!(decode_state(encode_state(mode, theme)), Some((mode, theme)));
            }
        }
        assert_eq!(decode_state(0), None);
        assert_eq!(decode_state(isize::MAX), None);
    }

    #[test]
    fn every_mode_has_a_specific_nonempty_description() {
        let labels: Vec<_> = Mode::ALL.into_iter().map(description).collect();
        assert!(labels.iter().all(|label| !label.is_empty()));
        for (index, label) in labels.iter().enumerate() {
            assert!(!labels[..index].contains(label), "duplicate label: {label}");
        }
        assert_eq!(description(Mode::Hiragana), "ひらがなで入力します");
    }

    #[test]
    fn popup_is_horizontal_and_remains_nonactivating() {
        let (width, height) = logical_size();
        assert!(width > height * 4);
        let ex = popup_ex_style();
        assert_ne!(ex.0 & WS_EX_NOACTIVATE.0, 0);
        assert_ne!(ex.0 & WS_EX_TOOLWINDOW.0, 0);
        assert_ne!(ex.0 & WS_EX_TOPMOST.0, 0);
        assert_eq!(popup_style(), WS_POPUP);
    }

    #[test]
    fn candidate_palette_is_the_indicator_palette_for_light_and_dark() {
        let light = candidate::palette(AppearanceTheme::Light);
        let dark = candidate::palette(AppearanceTheme::Dark);
        assert_ne!(light.surface, dark.surface);
        assert_ne!(light.ink, dark.ink);
        assert_ne!(light.annotation, dark.annotation);
        assert_ne!(light.rail, dark.rail);
        assert_ne!(light.border, dark.border);
    }
}
