//! Renderer-owned candidate popup.
//!
//! This is a topmost, non-activating tool window: it follows the TSF range
//! supplied by the DLL but never takes focus from the application receiving
//! keystrokes. All colors come from system roles, including selection and
//! high-contrast roles, and all dimensions scale from 96-DPI logical pixels.

use sakura_proto::{CandidateList, ScreenRect, UiState, CANDIDATE_PAGE_SIZE};
use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    FrameRect, GetMonitorInfoW, GetSysColor, InvalidateRect, MonitorFromRect, SelectObject,
    SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_3DSHADOW,
    COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT,
    DEFAULT_CHARSET, DEFAULT_PITCH, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, FF_DONTCARE, FW_NORMAL, HBRUSH, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    OUT_TT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW,
    GWLP_USERDATA, HTTRANSPARENT, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_GETOBJECT, WM_NCHITTEST,
    WM_PAINT, WNDCLASSW, WS_BORDER, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::accessibility::CandidateAccessibility;

const CLASS: PCWSTR = windows::core::w!("SakuraInputCandidates");
const WIDTH_96: i32 = 440;
const ROW_HEIGHT_96: i32 = 32;
const FOOTER_HEIGHT_96: i32 = 24;
const PADDING_96: i32 = 10;
const NUMBER_WIDTH_96: i32 = 28;
const GAP_96: i32 = 4;

#[derive(Debug)]
struct PaintState {
    candidates: Option<CandidateList>,
    accessibility: CandidateAccessibility,
}

/// One candidate popup, created once and reused for every conversion.
#[derive(Debug)]
pub struct CandidateWindow {
    window: HWND,
    state: Box<PaintState>,
}

impl CandidateWindow {
    pub fn new() -> Result<Self> {
        // SAFETY: the class name and procedure are static. Duplicate class
        // registration is harmless when tests create more than one object.
        unsafe {
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(procedure),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
        }
        // SAFETY: the class was registered above; the window starts hidden.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                CLASS,
                PCWSTR::null(),
                WS_POPUP | WS_BORDER,
                0,
                0,
                WIDTH_96,
                ROW_HEIGHT_96 + FOOTER_HEIGHT_96,
                None,
                None,
                None,
                None,
            )?
        };
        let mut state = Box::new(PaintState {
            candidates: None,
            accessibility: CandidateAccessibility::new(window),
        });
        // SAFETY: `Box` keeps this address stable until `Drop`, where the
        // pointer is cleared before the box is released.
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, (&raw mut *state) as isize);
        }
        Ok(Self { window, state })
    }

    /// Applies the latest coalesced engine state.
    pub fn update(&mut self, ui: &UiState) {
        let (Some(candidates), Some(anchor)) = (&ui.candidates, ui.anchor) else {
            self.hide();
            return;
        };
        if !ui.renderer_visible || candidates.items.is_empty() || !anchor.is_valid() {
            self.hide();
            return;
        }
        let visible_count = candidates.visible_range().len();
        if visible_count == 0 {
            self.hide();
            return;
        }
        self.state.candidates = Some(candidates.clone());
        self.state.accessibility.update(candidates);

        let dpi = dpi(self.window);
        let width = scaled(WIDTH_96, dpi);
        let height =
            scaled(ROW_HEIGHT_96, dpi) * visible_count as i32 + scaled(FOOTER_HEIGHT_96, dpi);
        let work = monitor_work_area(anchor);
        let placed = place(anchor, width, height, work, scaled(GAP_96, dpi));
        // SAFETY: the popup is live; `SWP_NOACTIVATE` and
        // `SW_SHOWNOACTIVATE` jointly guarantee focus stays in the host.
        unsafe {
            let _ = SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                placed.left,
                placed.top,
                placed.right - placed.left,
                placed.bottom - placed.top,
                SWP_NOACTIVATE,
            );
            let _ = InvalidateRect(Some(self.window), None, true);
            let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        self.state.accessibility.hide();
        // SAFETY: the popup is live for this object's lifetime.
        unsafe {
            let _ = ShowWindow(self.window, SW_HIDE);
        }
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        // SAFETY: the pointer is no longer observable after the live window
        // is destroyed, and both operations happen exactly once.
        unsafe {
            self.state.accessibility.disconnect();
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.window);
        }
    }
}

fn dpi(window: HWND) -> u32 {
    // SAFETY: `window` is live. Zero is documented failure and falls back to
    // the logical baseline.
    let value = unsafe { GetDpiForWindow(window) };
    if value == 0 {
        96
    } else {
        value
    }
}

fn scaled(value: i32, dpi: u32) -> i32 {
    value.saturating_mul(dpi as i32) / 96
}

fn monitor_work_area(anchor: ScreenRect) -> RECT {
    let rect = RECT {
        left: anchor.left,
        top: anchor.top,
        right: anchor.right,
        bottom: anchor.bottom,
    };
    // SAFETY: `rect` and `info` outlive their calls. `NEAREST` guarantees a
    // monitor for rectangles just outside a work area.
    unsafe {
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: core::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            return info.rcWork;
        }
    }
    rect
}

/// Places below the composition when possible, flips above when needed, and
/// clamps to the selected monitor's (possibly negative) work coordinates.
fn place(anchor: ScreenRect, width: i32, height: i32, work: RECT, gap: i32) -> RECT {
    let max_x = (work.right - width).max(work.left);
    let x = anchor.left.clamp(work.left, max_x);
    let below = anchor.bottom.saturating_add(gap);
    let above = anchor.top.saturating_sub(gap).saturating_sub(height);
    let max_y = (work.bottom - height).max(work.top);
    let y = if below.saturating_add(height) <= work.bottom {
        below
    } else if above >= work.top {
        above
    } else {
        below.clamp(work.top, max_y)
    };
    RECT {
        left: x,
        top: y,
        right: x.saturating_add(width),
        bottom: y.saturating_add(height),
    }
}

extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(window);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_GETOBJECT => {
            // SAFETY: this only reads the per-window pointer installed by
            // `CandidateWindow`; its validity is checked before dereference.
            let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const PaintState;
            if state.is_null() {
                // SAFETY: every unhandled object request belongs to the
                // default procedure.
                unsafe { DefWindowProcW(window, message, w, l) }
            } else {
                // SAFETY: CandidateWindow owns this stable box for the live
                // HWND and clears the pointer before destruction.
                unsafe { &*state }
                    .accessibility
                    .return_provider(window, w, l)
            }
        }
        WM_DPICHANGED => {
            let suggested = l.0 as *const RECT;
            if !suggested.is_null() {
                // SAFETY: Windows supplies a readable suggested rectangle for
                // this message; the popup remains non-activating.
                unsafe {
                    let rect = *suggested;
                    let _ = SetWindowPos(
                        window,
                        None,
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                    let _ = InvalidateRect(Some(window), None, true);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        // SAFETY: every unhandled message belongs to the default procedure.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

fn paint(window: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: paired with `EndPaint` below.
    let dc = unsafe { BeginPaint(window, &mut ps) };
    if dc.is_invalid() {
        return;
    }
    let mut client = RECT::default();
    // SAFETY: the window and output rectangle are live.
    let sized = unsafe { GetClientRect(window, &mut client) }.is_ok();
    // SAFETY: this only reads the stable pointer owned by `CandidateWindow`;
    // null and lifetime are checked before it is dereferenced below.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const PaintState;
    if sized && !state.is_null() {
        // SAFETY: `CandidateWindow` owns the stable box for the lifetime of
        // this window and clears the pointer before destroying it.
        let state = unsafe { &*state };
        if let Some(candidates) = &state.candidates {
            draw(dc, client, candidates, dpi(window));
        }
    }
    // SAFETY: pairs with the successful `BeginPaint` above.
    unsafe {
        let _ = EndPaint(window, &ps);
    }
}

fn draw(
    dc: windows::Win32::Graphics::Gdi::HDC,
    client: RECT,
    candidates: &CandidateList,
    dpi: u32,
) {
    let row_height = scaled(ROW_HEIGHT_96, dpi);
    let footer_height = scaled(FOOTER_HEIGHT_96, dpi);
    let padding = scaled(PADDING_96, dpi);
    let number_width = scaled(NUMBER_WIDTH_96, dpi);
    let font = font(scaled(17, dpi));
    // SAFETY: GDI objects are restored/deleted before returning.
    let previous = unsafe { SelectObject(dc, font.into()) };
    // SAFETY: the paint DC is valid for this frame and the mode value is a
    // documented GDI constant.
    unsafe {
        let _ = SetBkMode(dc, TRANSPARENT);
    }

    let visible = candidates.visible_range();
    // Keep the numeric shortcuts and footer tied to the complete current
    // page. Compact conversion merely hides sibling rows; it must not make a
    // second-page selection appear to be shortcut 1 or page 1.
    let page = candidates.current_page_range();
    let page_start = page.start;
    let page_end = page.end;
    for (page_index, global_index) in visible.enumerate() {
        let candidate = &candidates.items[global_index];
        let row = RECT {
            left: client.left,
            top: client.top + row_height * page_index as i32,
            right: client.right,
            bottom: client.top + row_height * (page_index as i32 + 1),
        };
        let selected = global_index == usize::from(candidates.selected);
        fill(
            dc,
            &row,
            if selected {
                COLOR_HIGHLIGHT
            } else {
                COLOR_WINDOW
            },
        );
        let ink = system_color(if selected {
            COLOR_HIGHLIGHTTEXT
        } else {
            COLOR_WINDOWTEXT
        });
        let number = number_label(global_index.saturating_sub(page_start));
        let number_rect = RECT {
            left: row.left + padding,
            right: row.left + padding + number_width,
            ..row
        };
        text(dc, number, number_rect, ink, DT_RIGHT);

        let candidate_rect = candidate_text_rect(row, number_rect.right, padding);
        text(
            dc,
            &candidate.text,
            candidate_rect,
            ink,
            DT_LEFT | DT_END_ELLIPSIS,
        );
    }

    let footer = RECT {
        left: client.left,
        top: client.bottom - footer_height,
        right: client.right,
        bottom: client.bottom,
    };
    fill(dc, &footer, COLOR_WINDOW);
    // SAFETY: the system color value is valid and the returned brush is
    // deleted before this frame returns.
    let border = unsafe { CreateSolidBrush(system_color(COLOR_3DSHADOW)) };
    // SAFETY: the brush is live for the frame and then deleted.
    unsafe {
        FrameRect(dc, &footer, border);
        let _ = DeleteObject(border.into());
    }
    let summary = page_summary(page_start, page_end, candidates.items.len());
    let summary_rect = RECT {
        left: footer.left + padding,
        right: footer.right - padding,
        ..footer
    };
    text(
        dc,
        &summary,
        summary_rect,
        system_color(COLOR_GRAYTEXT),
        DT_RIGHT,
    );

    // SAFETY: restores the selected object before deleting the font.
    unsafe {
        SelectObject(dc, previous);
        let _ = DeleteObject(font.into());
    }
}

fn candidate_text_rect(row: RECT, number_right: i32, padding: i32) -> RECT {
    RECT {
        left: number_right.saturating_add(padding),
        right: row.right.saturating_sub(padding),
        ..row
    }
}

fn system_color(role: windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX) -> COLORREF {
    // SAFETY: callers pass documented system color roles.
    COLORREF(unsafe { GetSysColor(role) })
}

fn fill(
    dc: windows::Win32::Graphics::Gdi::HDC,
    rect: &RECT,
    role: windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX,
) {
    // SAFETY: the brush and rectangle live through the fill; the brush is
    // deleted immediately afterwards.
    unsafe {
        let brush: HBRUSH = CreateSolidBrush(system_color(role));
        FillRect(dc, rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

fn text(
    dc: windows::Win32::Graphics::Gdi::HDC,
    value: &str,
    mut rect: RECT,
    color: COLORREF,
    alignment: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    let Some(mut wide) = drawable_utf16(value) else {
        return;
    };
    // SAFETY: the UTF-16 buffer and rectangle outlive the call.
    unsafe {
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            alignment | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    }
}

/// `DrawTextW` may still inspect its buffer pointer when its length is zero.
/// Rust's empty `Vec` uses a dangling non-null sentinel, so do not cross the
/// FFI boundary for an empty candidate text.
fn drawable_utf16(value: &str) -> Option<Vec<u16>> {
    (!value.is_empty()).then(|| value.encode_utf16().collect())
}

fn font(height: i32) -> windows::Win32::Graphics::Gdi::HFONT {
    // SAFETY: all arguments are plain values; an empty face asks Windows for
    // the locale-appropriate UI font and fallback chain.
    unsafe {
        CreateFontW(
            -height,
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
            PCWSTR::null(),
        )
    }
}

fn number_label(index: usize) -> &'static str {
    const LABELS: [&str; CANDIDATE_PAGE_SIZE] =
        ["1.", "2.", "3.", "4.", "5.", "6.", "7.", "8.", "9."];
    LABELS.get(index).copied().unwrap_or("")
}

fn page_summary(start: usize, end: usize, total: usize) -> String {
    format!("{}–{} / {total}", start.saturating_add(1), end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_labels_cover_exactly_the_protocol_page() {
        for index in 0..CANDIDATE_PAGE_SIZE {
            assert_eq!(number_label(index), format!("{}.", index + 1));
        }
        assert_eq!(number_label(CANDIDATE_PAGE_SIZE), "");
    }

    #[test]
    fn second_page_summary_uses_global_candidate_indices() {
        assert_eq!(page_summary(9, 14, 14), "10–14 / 14");
    }

    #[test]
    fn empty_text_is_not_passed_to_draw_text() {
        assert_eq!(drawable_utf16(""), None);
        assert_eq!(
            drawable_utf16("候補"),
            Some("候補".encode_utf16().collect())
        );
    }

    #[test]
    fn candidate_text_uses_the_width_freed_by_annotations() {
        let row = RECT {
            left: 0,
            top: 0,
            right: 440,
            bottom: 32,
        };
        let text_rect = candidate_text_rect(row, 48, 10);
        assert_eq!(text_rect.left, 58);
        assert_eq!(text_rect.right, 430);
        assert_eq!(text_rect.top, row.top);
        assert_eq!(text_rect.bottom, row.bottom);
    }

    #[test]
    fn placement_supports_negative_virtual_desktop_coordinates() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        let anchor = ScreenRect {
            left: -1800,
            top: 100,
            right: -1700,
            bottom: 124,
        };
        let popup = place(anchor, 440, 200, work, 4);
        assert_eq!(popup.left, -1800);
        assert_eq!(popup.top, 128);
        assert!(popup.right <= work.right);
    }

    #[test]
    fn placement_flips_above_at_the_bottom_edge() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let anchor = ScreenRect {
            left: 100,
            top: 1000,
            right: 150,
            bottom: 1024,
        };
        let popup = place(anchor, 440, 300, work, 4);
        assert_eq!(popup.bottom, 996);
        assert!(popup.top >= work.top);
    }

    #[test]
    fn oversized_popup_is_clamped_to_the_work_area_origin() {
        let work = RECT {
            left: 200,
            top: 100,
            right: 500,
            bottom: 400,
        };
        let anchor = ScreenRect {
            left: 450,
            top: 350,
            right: 470,
            bottom: 370,
        };
        let popup = place(anchor, 600, 600, work, 4);
        assert_eq!((popup.left, popup.top), (work.left, work.top));
    }

    #[test]
    fn every_logical_dimension_scales_when_dpi_changes_mid_composition() {
        for logical in [
            GAP_96,
            PADDING_96,
            FOOTER_HEIGHT_96,
            ROW_HEIGHT_96,
            WIDTH_96,
        ] {
            assert_eq!(scaled(logical, 96), logical);
            assert_eq!(scaled(logical, 144), logical * 3 / 2);
            assert_eq!(scaled(logical, 192), logical * 2);
        }
    }
}
