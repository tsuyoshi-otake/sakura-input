//! Renderer-owned candidate popup.
//!
//! The popup follows the TSF caret without taking focus. Its layout is computed
//! from 96-DPI tokens, while painting uses a restrained Sakura palette unless
//! Windows high-contrast mode requests system colors instead.

use sakura_proto::{
    CandidateDetail, CandidateKind, CandidateList, ScreenRect, UiState, CANDIDATE_PAGE_SIZE,
};
use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    GetMonitorInfoW, GetSysColor, InvalidateRect, MonitorFromRect, SelectObject, SetBkMode,
    SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_3DSHADOW, COLOR_GRAYTEXT,
    COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DEFAULT_CHARSET,
    DEFAULT_PITCH, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FW_NORMAL, HBRUSH, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_TT_PRECIS,
    PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, HTTRANSPARENT, HWND_TOPMOST, SPI_GETHIGHCONTRAST, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_GETOBJECT, WM_NCHITTEST, WM_PAINT, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::accessibility::CandidateAccessibility;

const CLASS: PCWSTR = windows::core::w!("SakuraInputCandidates");

// All measurements are logical pixels at 96 DPI.
const ROW_HEIGHT_96: i32 = 28;
const FOOTER_HEIGHT_96: i32 = 22;
const PADDING_96: i32 = 8;
const GAP_96: i32 = 8;
const NUMBER_WIDTH_96: i32 = 28;
const RAIL_WIDTH_96: i32 = 2;
const RAIL_MARGIN_96: i32 = 4;
const MIN_WIDTH_96: i32 = 260;
const MAX_WIDTH_96: i32 = 480;
const BODY_FONT_96: i32 = 16;
const SUPPORT_FONT_96: i32 = 13;
const DETAIL_MIN_WIDTH_96: i32 = 220;
const DETAIL_MAX_WIDTH_96: i32 = 360;
const DETAIL_PADDING_96: i32 = 12;
const DETAIL_TITLE_HEIGHT_96: i32 = 22;
const DETAIL_LINE_HEIGHT_96: i32 = 18;
const DETAIL_SECTION_GAP_96: i32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Palette {
    surface: COLORREF,
    ink: COLORREF,
    annotation: COLORREF,
    selected: COLORREF,
    selected_ink: COLORREF,
    rail: COLORREF,
    border: COLORREF,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    width: i32,
    height: i32,
    row_height: i32,
    footer_height: i32,
    padding: i32,
    gap: i32,
    number_width: i32,
    annotation_width: i32,
    rail_width: i32,
    rail_margin: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DetailLayout {
    width: i32,
    height: i32,
    padding: i32,
    title_height: i32,
    line_height: i32,
    section_gap: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PopupLayout {
    candidates: RECT,
    detail: Option<RECT>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PopupPlacement {
    window: RECT,
    layout: PopupLayout,
}

#[derive(Debug)]
struct PaintState {
    candidates: Option<CandidateList>,
    detail: Option<CandidateDetail>,
    layout: Option<PopupLayout>,
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
                WS_POPUP,
                0,
                0,
                MIN_WIDTH_96,
                ROW_HEIGHT_96 + FOOTER_HEIGHT_96,
                None,
                None,
                None,
                None,
            )?
        };
        let mut state = Box::new(PaintState {
            candidates: None,
            detail: None,
            layout: None,
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
        if candidates.visible_range().is_empty() {
            self.hide();
            return;
        }

        let current_dpi = dpi(self.window);
        let candidate_layout = layout(candidates, current_dpi);
        // Details are optional and strictly source-backed by the protocol. A
        // malformed value is deliberately omitted rather than partially drawn.
        let detail = ui
            .candidate_detail
            .as_ref()
            .filter(|detail| detail.validate().is_ok());
        let surface = selected_surface(candidates).unwrap_or("");
        let detail_layout = detail.map(|detail| detail_layout(surface, detail, current_dpi));
        let work = monitor_work_area(anchor);
        let placed = popup_placement(anchor, candidate_layout, detail_layout, work);
        self.state.candidates = Some(candidates.clone());
        self.state.detail = detail.cloned();
        self.state.layout = Some(placed.layout);
        self.state.accessibility.update(candidates, detail);
        // SAFETY: the popup is live; `SWP_NOACTIVATE` and
        // `SW_SHOWNOACTIVATE` jointly preserve focus in the host application.
        unsafe {
            let _ = SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                placed.window.left,
                placed.window.top,
                placed.window.right - placed.window.left,
                placed.window.bottom - placed.window.top,
                SWP_NOACTIVATE,
            );
            let _ = InvalidateRect(Some(self.window), None, false);
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
    value.saturating_mul(dpi as i32).saturating_add(48) / 96
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
    let max_x = (work.right.saturating_sub(width)).max(work.left);
    let x = anchor.left.clamp(work.left, max_x);
    let below = anchor.bottom.saturating_add(gap);
    let above = anchor.top.saturating_sub(gap).saturating_sub(height);
    let max_y = (work.bottom.saturating_sub(height)).max(work.top);
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

/// Keeps the established candidate rectangle intact, then attaches the
/// selected-candidate detail to its right, left, or bottom. The detail is
/// omitted when no complete placement fits the current monitor work area.
fn popup_placement(
    anchor: ScreenRect,
    candidate_layout: Layout,
    detail_layout: Option<DetailLayout>,
    work: RECT,
) -> PopupPlacement {
    let candidates = place(
        anchor,
        candidate_layout.width,
        candidate_layout.height,
        work,
        candidate_layout.gap,
    );
    let detail = detail_layout.and_then(|detail_layout| {
        if detail_layout.width > work.right.saturating_sub(work.left)
            || detail_layout.height > work.bottom.saturating_sub(work.top)
        {
            return None;
        }
        let right = RECT {
            left: candidates.right,
            top: candidates.top,
            right: candidates.right.saturating_add(detail_layout.width),
            bottom: candidates.top.saturating_add(detail_layout.height),
        };
        if right.right <= work.right && right.bottom <= work.bottom {
            return Some(right);
        }

        let left = RECT {
            left: candidates.left.saturating_sub(detail_layout.width),
            top: candidates.top,
            right: candidates.left,
            bottom: candidates.top.saturating_add(detail_layout.height),
        };
        if left.left >= work.left && left.bottom <= work.bottom {
            return Some(left);
        }

        let max_x = (work.right.saturating_sub(detail_layout.width)).max(work.left);
        let below_left = candidates.left.clamp(work.left, max_x);
        let below = RECT {
            left: below_left,
            top: candidates.bottom.saturating_add(candidate_layout.gap),
            right: below_left.saturating_add(detail_layout.width),
            bottom: candidates
                .bottom
                .saturating_add(candidate_layout.gap)
                .saturating_add(detail_layout.height),
        };
        (below.bottom <= work.bottom).then_some(below)
    });

    let window = detail.map_or(candidates, |detail| RECT {
        left: candidates.left.min(detail.left),
        top: candidates.top.min(detail.top),
        right: candidates.right.max(detail.right),
        bottom: candidates.bottom.max(detail.bottom),
    });
    let local = |rect: RECT| RECT {
        left: rect.left.saturating_sub(window.left),
        top: rect.top.saturating_sub(window.top),
        right: rect.right.saturating_sub(window.left),
        bottom: rect.bottom.saturating_sub(window.top),
    };
    PopupPlacement {
        window,
        layout: PopupLayout {
            candidates: local(candidates),
            detail: detail.map(local),
        },
    }
}

extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(window);
            LRESULT(0)
        }
        // Painting fills the complete client area, so Windows need not erase
        // it first. This removes the usual popup resize flash.
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
                    let _ = InvalidateRect(Some(window), None, false);
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
    // SAFETY: every non-invalid paint DC is paired with `EndPaint` below.
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
        if let (Some(candidates), Some(layout)) = (&state.candidates, state.layout) {
            draw(
                dc,
                client,
                candidates,
                state.detail.as_ref(),
                layout,
                dpi(window),
            );
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
    detail: Option<&CandidateDetail>,
    popup: PopupLayout,
    dpi: u32,
) {
    let candidate_client = popup.candidates;
    let layout = layout(candidates, dpi);
    let palette = palette();
    fill_color(dc, &client, palette.surface);

    let body_font = font(scaled(BODY_FONT_96, dpi));
    let support_font = font(scaled(SUPPORT_FONT_96, dpi));
    // SAFETY: the paint DC is valid for this frame and the mode value is a
    // documented GDI constant.
    unsafe {
        let _ = SetBkMode(dc, TRANSPARENT);
    }

    // `CreateFontW` can fail under resource pressure. The default selected
    // font remains usable in that case, and valid fonts are always restored
    // before being deleted below.
    let (prior, mut body_font_selected) = if let Some(prior) = select_font(dc, body_font) {
        // SAFETY: `body_font` is a live GDI object until this frame ends.
        (Some(prior), true)
    } else if let Some(prior) = select_font(dc, support_font) {
        // SAFETY: select the fallback font so that this owned object can be
        // restored before deletion even when creating the body font failed.
        (Some(prior), false)
    } else {
        (None, false)
    };

    let visible = candidates.visible_range();
    let page = candidates.current_page_range();
    for (row_index, global_index) in visible.enumerate() {
        let row = RECT {
            left: candidate_client.left,
            top: candidate_client
                .top
                .saturating_add(layout.row_height.saturating_mul(row_index as i32)),
            right: candidate_client.right,
            bottom: candidate_client.top.saturating_add(
                layout
                    .row_height
                    .saturating_mul((row_index as i32).saturating_add(1)),
            ),
        };
        let selected = global_index == usize::from(candidates.selected);
        fill_color(
            dc,
            &row,
            if selected {
                palette.selected
            } else {
                palette.surface
            },
        );
        if selected {
            let rail = selection_rail(row, layout);
            fill_color(dc, &rail, palette.rail);
        }

        let number_rect = RECT {
            left: row.left.saturating_add(layout.padding),
            right: row
                .left
                .saturating_add(layout.padding)
                .saturating_add(layout.number_width),
            ..row
        };
        text(
            dc,
            number_label(global_index.saturating_sub(page.start)),
            number_rect,
            if selected {
                palette.selected_ink
            } else {
                palette.annotation
            },
            DT_RIGHT,
        );

        let (surface_rect, annotation_rect) = candidate_columns(row, layout);
        let candidate = &candidates.items[global_index];
        text(
            dc,
            &candidate.text,
            surface_rect,
            if selected {
                palette.selected_ink
            } else {
                palette.ink
            },
            DT_LEFT | DT_END_ELLIPSIS,
        );

        if !candidate.annotation.is_empty() {
            // The support font is selected only after primary candidate text
            // has been drawn. `prior` restores the original object later.
            let support_font_selected = select_font(dc, support_font).is_some();
            text(
                dc,
                &candidate.annotation,
                annotation_rect,
                if selected {
                    palette.selected_ink
                } else {
                    palette.annotation
                },
                DT_RIGHT | DT_END_ELLIPSIS,
            );
            if support_font_selected && body_font_selected {
                // SAFETY: restores the body font for the next primary row.
                body_font_selected = select_font(dc, body_font).is_some();
            }
        }
    }

    let footer = RECT {
        left: candidate_client.left,
        top: candidate_client.bottom.saturating_sub(layout.footer_height),
        right: candidate_client.right,
        bottom: candidate_client.bottom,
    };
    fill_color(dc, &footer, palette.surface);
    let divider = RECT {
        bottom: footer.top.saturating_add(1),
        ..footer
    };
    fill_color(dc, &divider, palette.border);
    // If selecting the support font fails, the already selected valid font is
    // retained for the footer rather than continuing with an invalid object.
    let _ = select_font(dc, support_font);
    let label = match candidates.kind {
        CandidateKind::Conversion => "変換",
        CandidateKind::Suggestion => "予測",
    };
    let label_rect = RECT {
        left: footer.left.saturating_add(layout.padding),
        right: footer.left.saturating_add(layout.width / 2),
        ..footer
    };
    text(dc, label, label_rect, palette.annotation, DT_LEFT);

    let rail = page_rail(footer, candidates, layout);
    fill_color(dc, &rail.track, palette.border);
    fill_color(dc, &rail.thumb, palette.rail);
    let summary_rect = RECT {
        left: label_rect.right,
        right: rail.track.left.saturating_sub(layout.gap),
        ..footer
    };
    text(
        dc,
        &page_summary(page.start, page.end, candidates.items.len()),
        summary_rect,
        palette.annotation,
        DT_RIGHT | DT_END_ELLIPSIS,
    );

    let border_top = RECT {
        bottom: candidate_client.top.saturating_add(1),
        ..candidate_client
    };
    let border_bottom = RECT {
        top: candidate_client.bottom.saturating_sub(1),
        ..candidate_client
    };
    let border_left = RECT {
        right: candidate_client.left.saturating_add(1),
        ..candidate_client
    };
    let border_right = RECT {
        left: candidate_client.right.saturating_sub(1),
        ..candidate_client
    };
    for border in [border_top, border_bottom, border_left, border_right] {
        fill_color(dc, &border, palette.border);
    }

    // SAFETY: restore the pre-existing object before deleting either owned
    // font. `SelectObject` does not transfer ownership of its returned value.
    unsafe {
        if let Some(prior) = prior {
            let _ = SelectObject(dc, prior);
        }
        if !body_font.is_invalid() {
            let _ = DeleteObject(body_font.into());
        }
        if !support_font.is_invalid() {
            let _ = DeleteObject(support_font.into());
        }
    }

    if let (Some(detail), Some(detail_rect)) = (detail, popup.detail) {
        draw_detail(
            dc,
            detail_rect,
            selected_surface(candidates).unwrap_or(""),
            detail,
            dpi,
            palette,
        );
    }
}

fn layout(candidates: &CandidateList, dpi: u32) -> Layout {
    let visible = candidates.visible_range();
    let annotation_width_96 = visible
        .clone()
        .map(|index| text_width_96(&candidates.items[index].annotation, true))
        .max()
        .unwrap_or(0)
        .min(MAX_WIDTH_96 / 2);
    let surface_width_96 = visible
        .map(|index| text_width_96(&candidates.items[index].text, false))
        .max()
        .unwrap_or(0)
        .min(MAX_WIDTH_96);
    let annotation_gap_96 = i32::from(annotation_width_96 > 0) * GAP_96;
    let content_width_96 = PADDING_96
        .saturating_mul(2)
        .saturating_add(NUMBER_WIDTH_96)
        .saturating_add(GAP_96)
        .saturating_add(surface_width_96)
        .saturating_add(annotation_gap_96)
        .saturating_add(annotation_width_96);
    let width_96 = content_width_96.clamp(MIN_WIDTH_96, MAX_WIDTH_96);
    let rows = i32::try_from(candidates.visible_range().len()).unwrap_or(i32::MAX);
    Layout {
        width: scaled(width_96, dpi),
        // Build the total from the separately rounded rows and footer used by
        // `draw`. Rounding the aggregate can otherwise leave an unpainted gap
        // between the last row and footer at non-integral DPI scales.
        height: scaled(ROW_HEIGHT_96, dpi)
            .saturating_mul(rows)
            .saturating_add(scaled(FOOTER_HEIGHT_96, dpi)),
        row_height: scaled(ROW_HEIGHT_96, dpi),
        footer_height: scaled(FOOTER_HEIGHT_96, dpi),
        padding: scaled(PADDING_96, dpi),
        gap: scaled(GAP_96, dpi),
        number_width: scaled(NUMBER_WIDTH_96, dpi),
        annotation_width: scaled(annotation_width_96, dpi),
        rail_width: scaled(RAIL_WIDTH_96, dpi).max(1),
        rail_margin: scaled(RAIL_MARGIN_96, dpi),
    }
}

fn detail_layout(surface: &str, detail: &CandidateDetail, dpi: u32) -> DetailLayout {
    let widest = std::iter::once(surface)
        .chain(std::iter::once(detail.reading.as_str()))
        .chain(std::iter::once(detail.definition.as_str()))
        .chain(detail.aliases.iter().map(String::as_str))
        .chain(detail.related.iter().map(String::as_str))
        .chain(detail.similar.iter().map(String::as_str))
        .chain(detail.antonyms.iter().map(String::as_str))
        .map(|value| text_width_96(value, true))
        .max()
        .unwrap_or(0);
    let width_96 = widest
        .saturating_add(DETAIL_PADDING_96.saturating_mul(2))
        .clamp(DETAIL_MIN_WIDTH_96, DETAIL_MAX_WIDTH_96);
    let sections = [
        &detail.aliases,
        &detail.related,
        &detail.similar,
        &detail.antonyms,
    ]
    .into_iter()
    .filter(|group| !group.is_empty())
    .count() as i32;
    let reading_height = i32::from(detail.reading != surface).saturating_mul(DETAIL_LINE_HEIGHT_96);
    let height_96 = DETAIL_PADDING_96
        .saturating_mul(2)
        .saturating_add(DETAIL_TITLE_HEIGHT_96)
        .saturating_add(reading_height)
        // Definitions intentionally have a hard two-line visual budget.
        .saturating_add(DETAIL_LINE_HEIGHT_96.saturating_mul(2))
        .saturating_add(
            sections.saturating_mul(DETAIL_LINE_HEIGHT_96.saturating_add(DETAIL_SECTION_GAP_96)),
        );
    DetailLayout {
        width: scaled(width_96, dpi),
        height: scaled(height_96, dpi),
        padding: scaled(DETAIL_PADDING_96, dpi),
        title_height: scaled(DETAIL_TITLE_HEIGHT_96, dpi),
        line_height: scaled(DETAIL_LINE_HEIGHT_96, dpi),
        section_gap: scaled(DETAIL_SECTION_GAP_96, dpi),
    }
}

fn draw_detail(
    dc: windows::Win32::Graphics::Gdi::HDC,
    rect: RECT,
    surface: &str,
    detail: &CandidateDetail,
    dpi: u32,
    palette: Palette,
) {
    let layout = detail_layout(surface, detail, dpi);
    fill_color(dc, &rect, palette.surface);
    let border = [
        RECT {
            bottom: rect.top.saturating_add(1),
            ..rect
        },
        RECT {
            top: rect.bottom.saturating_sub(1),
            ..rect
        },
        RECT {
            right: rect.left.saturating_add(1),
            ..rect
        },
        RECT {
            left: rect.right.saturating_sub(1),
            ..rect
        },
    ];
    for edge in border {
        fill_color(dc, &edge, palette.border);
    }

    let body_font = font(scaled(BODY_FONT_96, dpi));
    let support_font = font(scaled(SUPPORT_FONT_96, dpi));
    let prior = select_font(dc, body_font).or_else(|| select_font(dc, support_font));
    // SAFETY: the paint DC is valid for this frame and the mode value is a
    // documented GDI constant.
    unsafe {
        let _ = SetBkMode(dc, TRANSPARENT);
    }
    let content = RECT {
        left: rect.left.saturating_add(layout.padding),
        top: rect.top.saturating_add(layout.padding),
        right: rect.right.saturating_sub(layout.padding),
        bottom: rect.bottom.saturating_sub(layout.padding),
    };
    let mut cursor = content.top;
    let title = RECT {
        top: cursor,
        bottom: cursor.saturating_add(layout.title_height),
        ..content
    };
    text(dc, surface, title, palette.ink, DT_LEFT | DT_END_ELLIPSIS);
    cursor = title.bottom;
    let _ = select_font(dc, support_font);
    if detail.reading != surface {
        let reading = RECT {
            top: cursor,
            bottom: cursor.saturating_add(layout.line_height),
            ..content
        };
        let value = format!("読み: {}", detail.reading);
        text(
            dc,
            &value,
            reading,
            palette.annotation,
            DT_LEFT | DT_END_ELLIPSIS,
        );
        cursor = reading.bottom;
    }
    for line in definition_lines(
        &detail.definition,
        content.right.saturating_sub(content.left),
        dpi,
    ) {
        let line_rect = RECT {
            top: cursor,
            bottom: cursor.saturating_add(layout.line_height),
            ..content
        };
        text(
            dc,
            &line,
            line_rect,
            palette.annotation,
            DT_LEFT | DT_END_ELLIPSIS,
        );
        cursor = line_rect.bottom;
    }
    for (label, group) in [
        ("別名", &detail.aliases),
        ("関連語", &detail.related),
        ("類似語", &detail.similar),
        ("反対語", &detail.antonyms),
    ] {
        if group.is_empty() {
            continue;
        }
        cursor = cursor.saturating_add(layout.section_gap);
        let section = RECT {
            top: cursor,
            bottom: cursor.saturating_add(layout.line_height),
            ..content
        };
        let value = format!("{label}: {}", relation_text(group));
        text(
            dc,
            &value,
            section,
            palette.annotation,
            DT_LEFT | DT_END_ELLIPSIS,
        );
        cursor = section.bottom;
    }
    // SAFETY: restore the pre-existing object before deleting either owned
    // font. Each created font is owned by this routine only.
    unsafe {
        if let Some(prior) = prior {
            let _ = SelectObject(dc, prior);
        }
        if !body_font.is_invalid() {
            let _ = DeleteObject(body_font.into());
        }
        if !support_font.is_invalid() {
            let _ = DeleteObject(support_font.into());
        }
    }
}

fn relation_text(values: &[String]) -> String {
    values
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join("・")
}

fn definition_lines(definition: &str, available_width: i32, dpi: u32) -> [String; 2] {
    // This intentionally uses the widest expected glyph advance, which keeps
    // CJK and emoji text inside the fixed two visual rows at every DPI.
    let per_line = (available_width / scaled(SUPPORT_FONT_96, dpi).max(1)).max(1) as usize;
    let mut characters = definition.chars();
    let first: String = characters.by_ref().take(per_line).collect();
    let mut second: String = characters.by_ref().take(per_line).collect();
    if characters.next().is_some() {
        second.pop();
        second.push('…');
    }
    [first, second]
}

fn text_width_96(value: &str, supporting: bool) -> i32 {
    let unit = if supporting {
        SUPPORT_FONT_96
    } else {
        BODY_FONT_96
    };
    value.chars().fold(0_i32, |width, character| {
        let advance = if character.is_ascii() { unit / 2 } else { unit };
        width.saturating_add(advance)
    })
}

fn selected_surface(candidates: &CandidateList) -> Option<&str> {
    candidates
        .items
        .get(usize::from(candidates.selected))
        .map(|candidate| candidate.text.as_str())
}

/// Keeps the full logical selection rail visible inside the one-pixel window
/// border that is painted after every row.
fn selection_rail(row: RECT, layout: Layout) -> RECT {
    let left = row.left.saturating_add(1);
    RECT {
        left,
        right: left.saturating_add(layout.rail_width).min(row.right),
        ..row
    }
}

fn candidate_columns(row: RECT, layout: Layout) -> (RECT, RECT) {
    let surface_left = row
        .left
        .saturating_add(layout.padding)
        .saturating_add(layout.number_width)
        .saturating_add(layout.gap);
    let annotation_right = row.right.saturating_sub(layout.padding);
    let annotation_left = annotation_right.saturating_sub(layout.annotation_width);
    let surface_right = if layout.annotation_width == 0 {
        annotation_right
    } else {
        annotation_left.saturating_sub(layout.gap).max(surface_left)
    };
    (
        RECT {
            left: surface_left,
            right: surface_right,
            ..row
        },
        RECT {
            left: annotation_left,
            right: annotation_right,
            ..row
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PageRail {
    track: RECT,
    thumb: RECT,
}

fn page_rail(footer: RECT, candidates: &CandidateList, layout: Layout) -> PageRail {
    let track = RECT {
        left: footer
            .right
            .saturating_sub(layout.padding)
            .saturating_sub(layout.rail_width),
        top: footer.top.saturating_add(layout.rail_margin),
        right: footer.right.saturating_sub(layout.padding),
        bottom: footer.bottom.saturating_sub(layout.rail_margin),
    };
    let track_height = (track.bottom - track.top).max(1);
    let pages = candidates.page_count().max(1);
    let current = candidates.current_page().min(pages.saturating_sub(1));
    let thumb_height = (track_height / i32::try_from(pages).unwrap_or(i32::MAX)).max(2);
    let travel = track_height.saturating_sub(thumb_height);
    let offset = if pages <= 1 {
        0
    } else {
        travel.saturating_mul(i32::try_from(current).unwrap_or(i32::MAX))
            / i32::try_from(pages.saturating_sub(1)).unwrap_or(i32::MAX)
    };
    PageRail {
        track,
        thumb: RECT {
            top: track.top.saturating_add(offset),
            bottom: track
                .top
                .saturating_add(offset)
                .saturating_add(thumb_height),
            ..track
        },
    }
}

fn palette() -> Palette {
    if high_contrast_enabled() {
        return Palette {
            surface: system_color(COLOR_WINDOW),
            ink: system_color(COLOR_WINDOWTEXT),
            annotation: system_color(COLOR_GRAYTEXT),
            selected: system_color(COLOR_HIGHLIGHT),
            selected_ink: system_color(COLOR_HIGHLIGHTTEXT),
            rail: system_color(COLOR_HIGHLIGHT),
            border: system_color(COLOR_3DSHADOW),
        };
    }
    let window = system_color(COLOR_WINDOW);
    if is_dark(window) {
        dark_palette()
    } else {
        light_palette()
    }
}

fn high_contrast_enabled() -> bool {
    let mut high_contrast = HIGHCONTRASTW {
        cbSize: core::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: Windows fills the supplied initialized HIGHCONTRASTW structure.
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            high_contrast.cbSize,
            Some((&mut high_contrast as *mut HIGHCONTRASTW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && (high_contrast.dwFlags.0 & HCF_HIGHCONTRASTON.0) != 0
    }
}

fn light_palette() -> Palette {
    Palette {
        surface: rgb(0xF7, 0xF6, 0xF4),
        ink: rgb(0x30, 0x2E, 0x2D),
        annotation: rgb(0x74, 0x70, 0x6D),
        selected: rgb(0xE8, 0xE5, 0xE2),
        selected_ink: rgb(0x30, 0x2E, 0x2D),
        rail: rgb(0xB2, 0x8D, 0x96),
        border: rgb(0xBD, 0xB9, 0xB5),
    }
}

fn dark_palette() -> Palette {
    Palette {
        surface: rgb(0x24, 0x23, 0x22),
        ink: rgb(0xE4, 0xE1, 0xDE),
        annotation: rgb(0xA9, 0xA3, 0x9F),
        selected: rgb(0x37, 0x33, 0x31),
        selected_ink: rgb(0xE4, 0xE1, 0xDE),
        rail: rgb(0xA7, 0x83, 0x8C),
        border: rgb(0x53, 0x50, 0x4D),
    }
}

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(u32::from_le_bytes([red, green, blue, 0]))
}

fn is_dark(color: COLORREF) -> bool {
    let red = color.0 & 0xff;
    let green = (color.0 >> 8) & 0xff;
    let blue = (color.0 >> 16) & 0xff;
    // Rec. 709 luma calculated with integer weights, avoiding a floating-point
    // dependency in the rendering hot path.
    red * 2126 + green * 7152 + blue * 722 < 128 * 10_000
}

fn system_color(role: windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX) -> COLORREF {
    // SAFETY: callers pass documented system color roles.
    COLORREF(unsafe { GetSysColor(role) })
}

fn fill_color(dc: windows::Win32::Graphics::Gdi::HDC, rect: &RECT, color: COLORREF) {
    // SAFETY: the brush is owned by this call, remains live for `FillRect`, and
    // is deleted on the same path before returning.
    unsafe {
        let brush: HBRUSH = CreateSolidBrush(color);
        if !brush.is_invalid() {
            let _ = FillRect(dc, rect, brush);
            let _ = DeleteObject(brush.into());
        }
    }
}

fn select_font(
    dc: windows::Win32::Graphics::Gdi::HDC,
    font: windows::Win32::Graphics::Gdi::HFONT,
) -> Option<HGDIOBJ> {
    if font.is_invalid() {
        return None;
    }
    // SAFETY: the font is a live object owned by this paint frame. A NULL or
    // HGDI_ERROR result means the DC keeps its previously selected valid font.
    let previous = unsafe { SelectObject(dc, font.into()) };
    (!previous.0.is_null() && (previous.0 as isize) != -1).then_some(previous)
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
    // SAFETY: the non-empty UTF-16 buffer and rectangle outlive the call.
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
/// Rust's empty `Vec` uses a dangling non-null sentinel, so empty strings never
/// cross this FFI boundary.
fn drawable_utf16(value: &str) -> Option<Vec<u16>> {
    (!value.is_empty()).then(|| value.encode_utf16().collect())
}

fn font(height: i32) -> windows::Win32::Graphics::Gdi::HFONT {
    // SAFETY: all arguments are plain values and the face name is static.
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
            windows::core::w!("Yu Gothic UI"),
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
    use sakura_proto::types::CandidatePresentation;
    use sakura_proto::Candidate;

    fn candidates(items: Vec<Candidate>, selected: u16, kind: CandidateKind) -> CandidateList {
        CandidateList {
            kind,
            presentation: CandidatePresentation::Expanded,
            items,
            selected,
            page_size: CANDIDATE_PAGE_SIZE as u16,
        }
    }

    fn item(text: &str, annotation: &str) -> Candidate {
        Candidate {
            text: text.to_owned(),
            annotation: annotation.to_owned(),
        }
    }

    fn detail() -> CandidateDetail {
        CandidateDetail {
            reading: "ようご".to_owned(),
            definition: "絵文字😀と結合文字e\u{301}を含む、画面では二行までの説明です。".repeat(8),
            definition_truncated: true,
            aliases: vec!["別名A".to_owned(), "別名B".to_owned()],
            related: vec!["関連語".to_owned()],
            similar: Vec::new(),
            antonyms: vec!["反対語".to_owned()],
        }
    }

    #[test]
    fn digit_labels_cover_exactly_the_protocol_page() {
        for index in 0..CANDIDATE_PAGE_SIZE {
            assert_eq!(number_label(index), format!("{}.", index + 1));
        }
        assert_eq!(number_label(CANDIDATE_PAGE_SIZE), "");
    }

    #[test]
    fn summaries_use_global_candidate_indices() {
        assert_eq!(page_summary(9, 14, 14), "10–14 / 14");
        assert_eq!(page_summary(0, 9, 19), "1–9 / 19");
    }

    #[test]
    fn empty_text_never_crosses_draw_text() {
        assert_eq!(drawable_utf16(""), None);
        assert_eq!(
            drawable_utf16("候補"),
            Some("候補".encode_utf16().collect())
        );
    }

    #[test]
    fn settled_palettes_are_exact_and_distinct() {
        assert_eq!(light_palette().surface, rgb(0xF7, 0xF6, 0xF4));
        assert_eq!(light_palette().ink, rgb(0x30, 0x2E, 0x2D));
        assert_eq!(light_palette().annotation, rgb(0x74, 0x70, 0x6D));
        assert_eq!(light_palette().selected, rgb(0xE8, 0xE5, 0xE2));
        assert_eq!(light_palette().rail, rgb(0xB2, 0x8D, 0x96));
        assert_eq!(light_palette().border, rgb(0xBD, 0xB9, 0xB5));
        assert_eq!(dark_palette().surface, rgb(0x24, 0x23, 0x22));
        assert_eq!(dark_palette().ink, rgb(0xE4, 0xE1, 0xDE));
        assert_eq!(dark_palette().annotation, rgb(0xA9, 0xA3, 0x9F));
        assert_eq!(dark_palette().selected, rgb(0x37, 0x33, 0x31));
        assert_eq!(dark_palette().rail, rgb(0xA7, 0x83, 0x8C));
        assert_eq!(dark_palette().border, rgb(0x53, 0x50, 0x4D));
        assert_ne!(light_palette(), dark_palette());
    }

    #[test]
    fn observable_window_luminance_selects_the_corresponding_palette() {
        assert!(!is_dark(rgb(0xF7, 0xF6, 0xF4)));
        assert!(is_dark(rgb(0x24, 0x23, 0x22)));
        for channel in [0_u8, 32, 64, 96, 127, 128, 160, 224, 255] {
            let color = rgb(channel, channel, channel);
            assert_eq!(is_dark(color), channel < 128);
        }
    }

    #[test]
    fn layout_is_bounded_and_scales_all_tokens_for_adversarial_text() {
        for length in [0, 1, 2, 16, 128, 4096] {
            let content = "候".repeat(length);
            let list = candidates(vec![item(&content, &content)], 0, CandidateKind::Conversion);
            let at_96 = layout(&list, 96);
            let at_144 = layout(&list, 144);
            let at_192 = layout(&list, 192);
            assert!((MIN_WIDTH_96..=MAX_WIDTH_96).contains(&at_96.width));
            assert_eq!(at_144.width, scaled(at_96.width, 144));
            assert_eq!(at_192.width, scaled(at_96.width, 192));
            assert_eq!(at_144.row_height, scaled(ROW_HEIGHT_96, 144));
            assert_eq!(at_192.footer_height, scaled(FOOTER_HEIGHT_96, 192));
            assert!(at_96.annotation_width <= MAX_WIDTH_96 / 2);
        }
    }

    #[test]
    fn columns_are_stable_and_non_overlapping_for_annotations() {
        for surface_length in 0..80 {
            for annotation_length in 0..40 {
                let list = candidates(
                    vec![item(
                        &"a".repeat(surface_length),
                        &"注".repeat(annotation_length),
                    )],
                    0,
                    CandidateKind::Suggestion,
                );
                let layout = layout(&list, 96);
                let row = RECT {
                    left: 0,
                    top: 0,
                    right: layout.width,
                    bottom: layout.row_height,
                };
                let (surface, annotation) = candidate_columns(row, layout);
                assert!(surface.left <= surface.right);
                assert!(annotation.left <= annotation.right);
                assert!(surface.right <= annotation.left || layout.annotation_width == 0);
                assert!(annotation.right <= row.right.saturating_sub(layout.padding));
            }
        }
    }

    #[test]
    fn rows_and_footer_fill_the_layout_height_at_every_dpi() {
        for dpi in [96, 120, 125, 144, 168, 192] {
            for count in 1..=CANDIDATE_PAGE_SIZE {
                let list = candidates(
                    (0..count).map(|_| item("候補", "注釈")).collect(),
                    0,
                    CandidateKind::Conversion,
                );
                let layout = layout(&list, dpi);
                assert_eq!(
                    layout.height,
                    layout
                        .row_height
                        .saturating_mul(count as i32)
                        .saturating_add(layout.footer_height)
                );
            }
        }
    }

    #[test]
    fn selection_rail_is_not_overpainted_by_the_window_border() {
        let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
        for dpi in [96, 144, 192] {
            let layout = layout(&list, dpi);
            let row = RECT {
                left: 0,
                top: 0,
                right: layout.width,
                bottom: layout.row_height,
            };
            let rail = selection_rail(row, layout);
            assert_eq!(rail.left, 1);
            assert_eq!(rail.right - rail.left, layout.rail_width);
            assert!(rail.right <= row.right);
        }
    }

    #[test]
    fn page_rail_thumb_is_bounded_for_every_valid_page() {
        let footer = RECT {
            left: 0,
            top: 0,
            right: 300,
            bottom: FOOTER_HEIGHT_96,
        };
        for count in 1..=64 {
            for selected in 0..count {
                let list = candidates(
                    (0..count).map(|_| item("候補", "注釈")).collect(),
                    selected as u16,
                    CandidateKind::Conversion,
                );
                let rail = page_rail(footer, &list, layout(&list, 96));
                assert!(rail.track.top <= rail.thumb.top);
                assert!(rail.thumb.top < rail.thumb.bottom);
                assert!(rail.thumb.bottom <= rail.track.bottom);
                assert_eq!(rail.track.left, rail.thumb.left);
                assert_eq!(rail.track.right, rail.thumb.right);
            }
        }
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
    fn placement_flips_above_at_the_bottom_edge_and_clamps_oversize() {
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
    fn detail_uses_two_visual_lines_and_only_three_relation_words() {
        let lines = definition_lines("😀e\u{301}".repeat(128).as_str(), 40, 96);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
        assert_eq!(
            relation_text(&["a".into(), "b".into(), "c".into(), "d".into()]),
            "a・b・c"
        );
    }

    #[test]
    fn detail_placement_preserves_candidate_geometry_at_dpi_and_work_edges() {
        let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
        let detail = detail();
        let work = RECT {
            left: 0,
            top: 0,
            right: 1_200,
            bottom: 900,
        };
        for dpi in [96, 120, 144, 168, 192, 240] {
            let candidate_layout = layout(&list, dpi);
            let placement = popup_placement(
                ScreenRect {
                    left: 100,
                    top: 100,
                    right: 120,
                    bottom: 124,
                },
                candidate_layout,
                Some(detail_layout("候補", &detail, dpi)),
                work,
            );
            assert_eq!(
                placement.layout.candidates.right - placement.layout.candidates.left,
                candidate_layout.width
            );
            assert_eq!(
                placement.layout.candidates.bottom - placement.layout.candidates.top,
                candidate_layout.height
            );
            assert!(placement.layout.detail.is_some());
            assert!(placement.window.left >= work.left);
            assert!(placement.window.top >= work.top);
            assert!(placement.window.right <= work.right);
            assert!(placement.window.bottom <= work.bottom);
        }
    }

    #[test]
    fn detail_falls_back_right_left_bottom_then_absent() {
        let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
        let candidate_layout = layout(&list, 96);
        let detail_layout = detail_layout("候補", &detail(), 96);

        let right = popup_placement(
            ScreenRect {
                left: 100,
                top: 80,
                right: 120,
                bottom: 104,
            },
            candidate_layout,
            Some(detail_layout),
            RECT {
                left: 0,
                top: 0,
                right: 1_000,
                bottom: 700,
            },
        );
        assert_eq!(
            right.layout.detail.expect("right detail").left,
            right.layout.candidates.right
        );

        let left = popup_placement(
            ScreenRect {
                left: 400,
                top: 80,
                right: 420,
                bottom: 104,
            },
            candidate_layout,
            Some(detail_layout),
            RECT {
                left: 0,
                top: 0,
                right: 800,
                bottom: 700,
            },
        );
        assert_eq!(
            left.layout.detail.expect("left detail").right,
            left.layout.candidates.left
        );

        let below = popup_placement(
            ScreenRect {
                left: 20,
                top: 80,
                right: 40,
                bottom: 104,
            },
            candidate_layout,
            Some(detail_layout),
            RECT {
                left: 0,
                top: 0,
                right: 480,
                bottom: 700,
            },
        );
        assert!(below.layout.detail.expect("bottom detail").top > below.layout.candidates.bottom);

        let absent = popup_placement(
            ScreenRect {
                left: 20,
                top: 600,
                right: 40,
                bottom: 624,
            },
            candidate_layout,
            Some(detail_layout),
            RECT {
                left: 0,
                top: 0,
                right: 480,
                bottom: 700,
            },
        );
        assert!(absent.layout.detail.is_none());
    }
}
