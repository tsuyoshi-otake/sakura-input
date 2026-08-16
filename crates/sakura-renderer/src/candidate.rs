//! Renderer-owned candidate popup.
//!
//! The popup follows the TSF caret without taking focus. Its layout is computed
//! from 96-DPI tokens, while painting uses a restrained Sakura palette unless
//! Windows high-contrast mode requests system colors instead.

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::mpsc::{SyncSender, TrySendError};

use sakura_proto::{
    AppearanceTheme, CandidateDetail, CandidateKind, CandidateList, ScreenRect, UiState,
    CANDIDATE_PAGE_SIZE,
};
use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CombineRgn, CreateFontW, CreateRectRgn, CreateSolidBrush, DeleteObject, DrawTextW,
    EndPaint, FillRect, GetMonitorInfoW, GetSysColor, InvalidateRect, MonitorFromRect,
    SelectObject, SetBkMode, SetTextColor, SetWindowRgn, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    COLOR_3DSHADOW, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW,
    COLOR_WINDOWTEXT, DEFAULT_CHARSET, DEFAULT_PITCH, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX,
    DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_NORMAL, HBRUSH, HGDIOBJ, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, OUT_TT_PRECIS, PAINTSTRUCT, RGN_ERROR, RGN_OR, TRANSPARENT,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    GetWindowRect, RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    SystemParametersInfoW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HTCLIENT, HWND_TOPMOST,
    MA_NOACTIVATE, SPI_GETHIGHCONTRAST, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WINDOW_EX_STYLE, WINDOW_STYLE, WM_DESTROY, WM_DPICHANGED,
    WM_ERASEBKGND, WM_GETOBJECT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCHITTEST,
    WM_PAINT, WNDCLASSW, WS_DISABLED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::WS_EX_TRANSPARENT;

use crate::accessibility::CandidateAccessibility;
use crate::watch::HistoryDeleteRequest;

const DISPLAY_CLASS: PCWSTR = windows::core::w!("SakuraInputCandidates");
const DELETE_OVERLAY_CLASS: PCWSTR = windows::core::w!("SakuraInputCandidateDeleteTargets");

// All measurements are logical pixels at 96 DPI.
pub(crate) const ROW_HEIGHT_96: i32 = 28;
const FOOTER_HEIGHT_96: i32 = 22;
pub(crate) const PADDING_96: i32 = 8;
pub(crate) const GAP_96: i32 = 8;
/// How far the popup may be moved away from the caret to keep the host's
/// editable area readable. Roughly five candidate rows: past that the list
/// stops reading as attached to what is being typed, and a list that is hard
/// to find costs the user more than the covered text it bought.
const MAX_CARET_DETOUR_96: i32 = 160;
pub(crate) const NUMBER_WIDTH_96: i32 = 28;
pub(crate) const RAIL_WIDTH_96: i32 = 2;
const RAIL_MARGIN_96: i32 = 4;
const MIN_WIDTH_96: i32 = 260;
const MAX_WIDTH_96: i32 = 480;
pub(crate) const BODY_FONT_96: i32 = 16;
pub(crate) const SUPPORT_FONT_96: i32 = 13;
const DETAIL_WIDTH_96: i32 = 360;
const DETAIL_PADDING_96: i32 = 12;
const DETAIL_TITLE_HEIGHT_96: i32 = 22;
const DETAIL_LINE_HEIGHT_96: i32 = 18;
const DETAIL_SECTION_GAP_96: i32 = 4;
const HISTORY_DELETE_GLYPH_SIZE_96: i32 = 12;
const HISTORY_DELETE_HIT_SIZE_96: i32 = 24;
const HISTORY_DELETE_GAP_96: i32 = 6;
const HISTORY_DELETE_STROKE_96: i32 = 1;

fn display_popup_ex_style() -> WINDOW_EX_STYLE {
    WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE
}

fn display_popup_style() -> WINDOW_STYLE {
    WS_POPUP | WS_DISABLED
}

fn delete_overlay_ex_style() -> WINDOW_EX_STYLE {
    WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE
}

fn delete_overlay_style() -> WINDOW_STYLE {
    WS_POPUP
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    pub(crate) surface: COLORREF,
    pub(crate) ink: COLORREF,
    pub(crate) annotation: COLORREF,
    pub(crate) selected: COLORREF,
    pub(crate) selected_ink: COLORREF,
    pub(crate) rail: COLORREF,
    pub(crate) border: COLORREF,
    pub(crate) action: COLORREF,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    width: i32,
    height: i32,
    row_height: i32,
    footer_height: i32,
    padding: i32,
    gap: i32,
    detour: i32,
    number_width: i32,
    annotation_width: i32,
    rail_width: i32,
    rail_margin: i32,
    history_delete_glyph_size: i32,
    history_delete_hit_size: i32,
    history_delete_stroke: i32,
    history_delete_gutter: i32,
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
    display_window: HWND,
    appearance_theme: AppearanceTheme,
    candidates: Option<CandidateList>,
    detail: Option<CandidateDetail>,
    layout: Option<PopupLayout>,
    candidate_layout: Option<Layout>,
    revision: u64,
    delete_history: SyncSender<HistoryDeleteRequest>,
    pending_history_deletes: Vec<HistoryDeleteRequest>,
    visible: bool,
    delete_overlay: HWND,
    accessibility: CandidateAccessibility,
}

/// One candidate popup, created once and reused for every conversion.
#[derive(Debug)]
pub struct CandidateWindow {
    window: HWND,
    delete_overlay: HWND,
    state: Box<PaintState>,
}

impl CandidateWindow {
    pub fn new(delete_history: SyncSender<HistoryDeleteRequest>) -> Result<Self> {
        // SAFETY: class names and procedures are static. Duplicate class
        // registration is harmless when tests create more than one object.
        unsafe {
            let display_class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(display_procedure),
                lpszClassName: DISPLAY_CLASS,
                ..Default::default()
            };
            RegisterClassW(&display_class);
            let overlay_class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(delete_overlay_procedure),
                lpszClassName: DELETE_OVERLAY_CLASS,
                ..Default::default()
            };
            RegisterClassW(&overlay_class);
        }
        // SAFETY: both classes were registered above; windows start hidden.
        let window = unsafe {
            CreateWindowExW(
                display_popup_ex_style(),
                DISPLAY_CLASS,
                PCWSTR::null(),
                display_popup_style(),
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
        // SAFETY: the overlay class is registered above and starts hidden.
        let delete_overlay = match unsafe {
            CreateWindowExW(
                delete_overlay_ex_style(),
                DELETE_OVERLAY_CLASS,
                PCWSTR::null(),
                delete_overlay_style(),
                0,
                0,
                MIN_WIDTH_96,
                ROW_HEIGHT_96 + FOOTER_HEIGHT_96,
                None,
                None,
                None,
                None,
            )
        } {
            Ok(overlay) => overlay,
            Err(error) => {
                // SAFETY: this is the sole owner of the just-created display
                // window; no state pointer has been published yet.
                unsafe {
                    let _ = DestroyWindow(window);
                }
                return Err(error);
            }
        };
        let mut state = Box::new(PaintState {
            display_window: window,
            appearance_theme: AppearanceTheme::Auto,
            candidates: None,
            detail: None,
            layout: None,
            candidate_layout: None,
            revision: 0,
            delete_history,
            pending_history_deletes: Vec::new(),
            visible: false,
            delete_overlay,
            accessibility: CandidateAccessibility::new(window),
        });
        // SAFETY: `Box` keeps this address stable until `Drop`, where the
        // pointer is cleared before the box is released.
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, (&raw mut *state) as isize);
            SetWindowLongPtrW(delete_overlay, GWLP_USERDATA, (&raw mut *state) as isize);
        }
        Ok(Self {
            window,
            delete_overlay,
            state,
        })
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
        let work = monitor_work_area(anchor);
        let detail_layout = detail.map(|detail| {
            detail_layout(
                surface,
                detail,
                current_dpi,
                work.bottom.saturating_sub(work.top),
            )
        });
        let placed = popup_placement(anchor, ui.document, candidate_layout, detail_layout, work);
        clear_pending_history_deletes_for_new_revision(
            &mut self.state.pending_history_deletes,
            self.state.revision,
            ui.revision,
        );
        self.state.candidates = Some(candidates.clone());
        self.state.appearance_theme = ui.appearance_theme;
        self.state.detail = detail.cloned();
        self.state.layout = Some(placed.layout);
        self.state.candidate_layout = Some(candidate_layout);
        self.state.revision = ui.revision;
        self.state.visible = true;
        self.state.accessibility.update(candidates, detail);
        let delete_targets = history_delete_targets(candidates, placed.layout, candidate_layout);
        let overlay_ready = rebuild_delete_overlay_region(self.delete_overlay, &delete_targets);
        // SAFETY: the popup is live; `SWP_NOACTIVATE` and
        // `SW_SHOWNOACTIVATE` jointly preserve focus in the host application.
        unsafe {
            let display_positioned = SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                placed.window.left,
                placed.window.top,
                placed.window.right - placed.window.left,
                placed.window.bottom - placed.window.top,
                SWP_NOACTIVATE,
            )
            .is_ok();
            if !display_positioned {
                self.state.visible = false;
                self.state.accessibility.hide();
                let _ = clear_delete_overlay_region(self.delete_overlay);
                let _ = ShowWindow(self.delete_overlay, SW_HIDE);
                let _ = ShowWindow(self.window, SW_HIDE);
                return;
            }
            let _ = InvalidateRect(Some(self.window), None, false);
            let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
            let overlay_positioned = SetWindowPos(
                self.delete_overlay,
                Some(HWND_TOPMOST),
                placed.window.left,
                placed.window.top,
                placed.window.right - placed.window.left,
                placed.window.bottom - placed.window.top,
                SWP_NOACTIVATE,
            )
            .is_ok();
            if delete_overlay_should_be_visible(
                self.state.visible,
                overlay_positioned && overlay_ready,
                &delete_targets,
            ) {
                let _ = InvalidateRect(Some(self.delete_overlay), None, false);
                let _ = ShowWindow(self.delete_overlay, SW_SHOWNOACTIVATE);
            } else {
                // A failed region update must not leave a stale interactive
                // surface above the input-disabled display popup.
                if !overlay_positioned {
                    // Do not let a later overlay-only DPI message reveal this
                    // target region at its old screen position.
                    self.state.visible = false;
                }
                let _ = clear_delete_overlay_region(self.delete_overlay);
                let _ = ShowWindow(self.delete_overlay, SW_HIDE);
            }
        }
    }

    pub fn hide(&mut self) {
        self.state.accessibility.hide();
        self.state.visible = false;
        // SAFETY: the popup is live for this object's lifetime.
        unsafe {
            hide_delete_overlay(self.delete_overlay);
            let _ = ShowWindow(self.window, SW_HIDE);
        }
    }

    /// The popup's current screen rectangle, only while it is visible.
    ///
    /// The mode indicator places itself around this exact rectangle rather
    /// than guessing which side of the composition the popup chose, so the
    /// window itself — not a copy that could go stale across a DPI
    /// reposition — is the source of truth.
    pub fn popup_rect(&self) -> Option<RECT> {
        if !self.state.visible {
            return None;
        }
        let mut rect = RECT::default();
        // SAFETY: the popup is live for this object's lifetime and `rect`
        // outlives the call.
        unsafe { GetWindowRect(self.window, &mut rect) }.ok()?;
        Some(rect)
    }

    /// An authoritative removal remains suppressed until the engine publishes
    /// its next UI revision. A negative or failed attempt releases only that
    /// exact request so the still-visible row can be retried immediately.
    pub fn history_delete_finished(&mut self, request: HistoryDeleteRequest, removed: bool) {
        finish_pending_history_delete(&mut self.state.pending_history_deletes, request, removed);
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        // SAFETY: the pointer is no longer observable after the live window
        // is destroyed, and both operations happen exactly once.
        unsafe {
            self.state.accessibility.disconnect();
            SetWindowLongPtrW(self.delete_overlay, GWLP_USERDATA, 0);
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.delete_overlay);
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

/// Whether a proposed popup rectangle overlaps the composition rectangle.
///
/// Exclusive on the edges: a pane sitting flush against the composition
/// touches it without covering a pixel of it, and that is fine.
fn covers_composition(rect: RECT, anchor: ScreenRect) -> bool {
    rect.left < anchor.right
        && rect.right > anchor.left
        && rect.top < anchor.bottom
        && rect.bottom > anchor.top
}

pub(crate) fn monitor_work_area(anchor: ScreenRect) -> RECT {
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

/// Whether a proposed popup rectangle overlaps the host's editable area.
///
/// Same exclusive-edge rule as [`covers_composition`]: a popup resting flush
/// against the box is not covering it.
fn covers_document(rect: RECT, document: Option<ScreenRect>) -> bool {
    document.is_some_and(|document| covers_composition(rect, document))
}

fn union_rect(a: RECT, b: RECT) -> RECT {
    RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn vertically_padded(anchor: ScreenRect, pad: i32) -> ScreenRect {
    ScreenRect {
        left: anchor.left,
        top: anchor.top.saturating_sub(pad),
        right: anchor.right,
        bottom: anchor.bottom.saturating_add(pad),
    }
}

/// The display HWND is one opaque rectangle around the list and the detail.
/// A pane that misses the caret on its own can still stretch that rectangle
/// over the line being typed, so the union is what the user actually sees.
/// The list is placed a full `gap` away; the union must keep that gap too,
/// otherwise a few pixels of GetTextExt error put the window on the glyphs.
/// When the list itself already covers the composition, stretching further
/// cannot uncover it, and the extra pane is kept.
fn opaque_window_covers(
    candidates: RECT,
    pane: RECT,
    anchor: ScreenRect,
    document: Option<ScreenRect>,
    gap: i32,
) -> bool {
    let window = union_rect(candidates, pane);
    let padded = vertically_padded(anchor, gap);
    let stretches_over_composition =
        covers_composition(window, padded) && !covers_composition(candidates, anchor);
    stretches_over_composition || covers_document(window, document)
}

/// Places below the composition when possible, flips above when needed, and
/// clamps to the selected monitor's (possibly negative) work coordinates.
///
/// The one thing this must never do avoidably is cover the composition the
/// user is still typing. Below and above both leave it clear; only when the
/// popup is taller than the free space on *both* sides — where covering it
/// is a geometric certainty — does the popup take the roomier side, pinned
/// inside the work area, so as much of the composition's neighbourhood as
/// possible stays readable.
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
        let space_below = work.bottom.saturating_sub(below);
        let space_above = anchor.top.saturating_sub(gap).saturating_sub(work.top);
        if space_above > space_below {
            work.top
        } else {
            max_y
        }
    };
    RECT {
        left: x,
        top: y,
        right: x.saturating_add(width),
        bottom: y.saturating_add(height),
    }
}

/// The gap between a proposed popup rectangle and the composition: how far
/// the user's eye has to travel from the caret to the list. Zero when they
/// overlap, and the larger of the two axes otherwise, so a popup that is
/// close vertically but a screen away horizontally is correctly far.
fn caret_distance(rect: RECT, anchor: ScreenRect) -> i32 {
    let horizontal = (anchor.left.saturating_sub(rect.right))
        .max(rect.left.saturating_sub(anchor.right))
        .max(0);
    let vertical = (anchor.top.saturating_sub(rect.bottom))
        .max(rect.top.saturating_sub(anchor.bottom))
        .max(0);
    horizontal.max(vertical)
}

/// Chooses the candidate rectangle: next to the caret first, and out of the
/// host's editable area only when that costs a short step.
///
/// Below the composition is where every IME puts the list and where the user
/// looks for it, so [`place`] is both the first choice and the last resort.
/// It is worth leaving only when the box being typed into extends past the
/// caret line — exactly what `document` describes — because there the popup
/// lands on the text being entered. The alternatives step outward: above the
/// composition, just outside the box's bottom or top edge, then beside it.
///
/// Each alternative must fit the work area whole, clear both rectangles, and
/// stay within `detour` of the caret. That last condition is what keeps the
/// list findable: in a tall editor the nearest clear spot is most of a screen
/// away, and a list that far from the caret is worse than one sitting on the
/// empty part of the box. Those cases — and a host that reports no document —
/// keep exactly the geometry they had before the editable area was known.
fn place_candidates(
    anchor: ScreenRect,
    document: Option<ScreenRect>,
    width: i32,
    height: i32,
    work: RECT,
    gap: i32,
    detour: i32,
) -> RECT {
    let fallback = place(anchor, width, height, work, gap);
    let Some(document) = document else {
        return fallback;
    };
    if !covers_composition(fallback, document) {
        return fallback;
    }

    let max_x = (work.right.saturating_sub(width)).max(work.left);
    let max_y = (work.bottom.saturating_sub(height)).max(work.top);
    let anchored_x = anchor.left.clamp(work.left, max_x);
    // Beside the box, the popup can no longer follow the caret horizontally,
    // so it follows it vertically instead and stays level with the anchor.
    let anchored_y = anchor.top.clamp(work.top, max_y);
    let proposals = [
        // Above the composition: still the closest place to the caret.
        (
            anchored_x,
            anchor.top.saturating_sub(gap).saturating_sub(height),
        ),
        // Just outside the box, below then above.
        (anchored_x, document.bottom.saturating_add(gap)),
        (
            anchored_x,
            document.top.saturating_sub(gap).saturating_sub(height),
        ),
        // Beside the box, right then left.
        (document.right.saturating_add(gap), anchored_y),
        (
            document.left.saturating_sub(gap).saturating_sub(width),
            anchored_y,
        ),
    ];
    for (left, top) in proposals {
        let rect = RECT {
            left,
            top,
            right: left.saturating_add(width),
            bottom: top.saturating_add(height),
        };
        if rect.left >= work.left
            && rect.right <= work.right
            && rect.top >= work.top
            && rect.bottom <= work.bottom
            && caret_distance(rect, anchor) <= detour
            && !covers_composition(rect, document)
            && !covers_composition(rect, anchor)
        {
            return rect;
        }
    }
    fallback
}

/// Keeps the established candidate rectangle intact, then attaches the
/// selected-candidate detail to its right, left, or bottom. The detail is
/// omitted when no complete placement fits the current monitor work area —
/// or when every placement that fits would cover the composition the user
/// is typing, which an auxiliary pane is never worth. The painted HWND is
/// the bounding rectangle of both panes, so a taller side definition is
/// rejected when that bounding box — not just the pane — would cover the
/// line.
fn popup_placement(
    anchor: ScreenRect,
    document: Option<ScreenRect>,
    candidate_layout: Layout,
    detail_layout: Option<DetailLayout>,
    work: RECT,
) -> PopupPlacement {
    let candidates = place_candidates(
        anchor,
        document,
        candidate_layout.width,
        candidate_layout.height,
        work,
        candidate_layout.gap,
        candidate_layout.detour,
    );
    // An auxiliary pane is never worth covering the editable area either --
    // but only while the list itself managed to clear it. Once the list had
    // to fall back and cover the box, suppressing the detail as well would
    // cost the definition without buying back any of the covered text.
    let keep_document_clear = (!covers_document(candidates, document))
        .then_some(document)
        .flatten();
    let detail = detail_layout.and_then(|detail_layout| {
        if detail_layout.width > work.right.saturating_sub(work.left)
            || detail_layout.height > work.bottom.saturating_sub(work.top)
        {
            return None;
        }
        // A long definition may be taller than the candidate list. Keep the
        // candidate rectangle fixed and slide only the detail vertically into
        // the work area instead of dropping the pane or moving the list.
        let detail_top = candidates.top.clamp(
            work.top,
            work.bottom
                .saturating_sub(detail_layout.height)
                .max(work.top),
        );
        let right = RECT {
            left: candidates.right,
            top: detail_top,
            right: candidates.right.saturating_add(detail_layout.width),
            bottom: detail_top.saturating_add(detail_layout.height),
        };
        if right.right <= work.right
            && right.bottom <= work.bottom
            && !covers_composition(right, anchor)
            && !covers_document(right, keep_document_clear)
            && !opaque_window_covers(
                candidates,
                right,
                anchor,
                keep_document_clear,
                candidate_layout.gap,
            )
        {
            return Some(right);
        }

        let left = RECT {
            left: candidates.left.saturating_sub(detail_layout.width),
            top: detail_top,
            right: candidates.left,
            bottom: detail_top.saturating_add(detail_layout.height),
        };
        if left.left >= work.left
            && left.bottom <= work.bottom
            && !covers_composition(left, anchor)
            && !covers_document(left, keep_document_clear)
            && !opaque_window_covers(
                candidates,
                left,
                anchor,
                keep_document_clear,
                candidate_layout.gap,
            )
        {
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
        (below.bottom <= work.bottom
            && !covers_composition(below, anchor)
            && !covers_document(below, keep_document_clear)
            && !opaque_window_covers(
                candidates,
                below,
                anchor,
                keep_document_clear,
                candidate_layout.gap,
            ))
        .then_some(below)
    });

    let window = detail.map_or(candidates, |detail| union_rect(candidates, detail));
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

extern "system" fn display_procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_PAINT => {
            paint_display(window);
            LRESULT(0)
        }
        // Painting fills the complete client area, so Windows need not erase
        // it first. This removes the usual popup resize flash.
        WM_ERASEBKGND => LRESULT(1),
        WM_GETOBJECT => return_candidate_accessibility_provider(window, message, w, l),
        WM_DPICHANGED => {
            apply_display_dpi_change(window, l);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        // SAFETY: every unhandled message belongs to the default procedure.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

extern "system" fn delete_overlay_procedure(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            paint_delete_overlay(window);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // The HWND itself is clipped to delete targets. Every point Windows
        // can deliver to this procedure is therefore a deliberate control.
        WM_NCHITTEST => delete_overlay_non_client_hit_test_result(),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // A target click must remain entirely passive: no default processing
        // is allowed to activate the renderer or establish mouse capture.
        WM_LBUTTONDOWN => LRESULT(0),
        WM_LBUTTONUP => {
            queue_history_delete(window, point_from_lparam(l));
            LRESULT(0)
        }
        // Return the same retained candidate provider for the input overlay.
        // `UiaReturnRawElementProvider` must receive the HWND that received
        // WM_GETOBJECT so ElementFromPoint resolves this overlay to the
        // named candidate surface rather than its generic Win32 host element.
        WM_GETOBJECT => return_candidate_accessibility_provider(window, message, w, l),
        WM_DPICHANGED => {
            apply_overlay_dpi_change(window, l);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        // SAFETY: every unhandled message belongs to the default procedure.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

fn return_candidate_accessibility_provider(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
) -> LRESULT {
    // SAFETY: this only reads the per-window pointer installed by
    // CandidateWindow; its validity is checked before dereference.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const PaintState;
    if state.is_null() {
        // SAFETY: every unhandled object request belongs to the default
        // procedure for the HWND that actually received WM_GETOBJECT.
        return unsafe { DefWindowProcW(window, message, w, l) };
    }
    // SAFETY: CandidateWindow owns this stable box for the live base and
    // overlay HWNDs and clears both pointers before either is destroyed.
    unsafe { &*state }.accessibility.return_provider(
        candidate_accessibility_request_window(window),
        w,
        l,
    )
}

fn candidate_accessibility_request_window(window: HWND) -> HWND {
    // UIA requires the source HWND from the current WM_GETOBJECT message,
    // even though CandidateProvider's HostRawElementProvider remains bound to
    // the base candidate HWND for its stable provider identity.
    window
}

fn delete_overlay_non_client_hit_test_result() -> LRESULT {
    LRESULT(HTCLIENT as isize)
}

fn suggested_rect(value: LPARAM) -> Option<RECT> {
    let suggested = value.0 as *const RECT;
    // SAFETY: Windows supplies a readable suggested rectangle for
    // WM_DPICHANGED. A null value is not usable.
    (!suggested.is_null()).then(|| unsafe { *suggested })
}

fn set_window_rect(window: HWND, rect: RECT, z_order: Option<HWND>) -> bool {
    // SAFETY: the caller supplies a live popup HWND and a rectangle copied
    // from Windows or calculated from the current monitor work area.
    unsafe {
        let flags = if z_order.is_none() {
            SWP_NOACTIVATE | SWP_NOZORDER
        } else {
            SWP_NOACTIVATE
        };
        SetWindowPos(
            window,
            z_order,
            rect.left,
            rect.top,
            rect.right.saturating_sub(rect.left),
            rect.bottom.saturating_sub(rect.top),
            flags,
        )
        .is_ok()
    }
}

fn refresh_delete_overlay_for_dpi(state: &mut PaintState) -> bool {
    if !state.visible {
        hide_delete_overlay(state.delete_overlay);
        return false;
    }
    let (Some(candidates), Some(mut popup)) = (state.candidates.as_ref(), state.layout) else {
        hide_delete_overlay(state.delete_overlay);
        return false;
    };
    let layout = layout(candidates, dpi(state.display_window));
    popup.candidates.right = popup.candidates.left.saturating_add(layout.width);
    popup.candidates.bottom = popup.candidates.top.saturating_add(layout.height);
    let targets = history_delete_targets(candidates, popup, layout);
    state.layout = Some(popup);
    state.candidate_layout = Some(layout);
    let ready = rebuild_delete_overlay_region(state.delete_overlay, &targets);
    // SAFETY: both HWNDs remain live for the PaintState lifetime.
    unsafe {
        let _ = InvalidateRect(Some(state.display_window), None, false);
        if delete_overlay_should_be_visible(state.visible, ready, &targets) {
            let _ = InvalidateRect(Some(state.delete_overlay), None, false);
            let _ = ShowWindow(state.delete_overlay, SW_SHOWNOACTIVATE);
        } else {
            hide_delete_overlay(state.delete_overlay);
        }
    }
    ready
}

fn apply_display_dpi_change(window: HWND, value: LPARAM) {
    let Some(rect) = suggested_rect(value) else {
        // A malformed transition must not leave an independently visible
        // target surface from the previous DPI arrangement.
        hide_delete_overlay_for_display(window);
        return;
    };
    if !set_window_rect(window, rect, None) {
        hide_delete_overlay_for_display(window);
        // SAFETY: this is the display HWND that received the failed position
        // request, so hiding it cannot affect another popup.
        unsafe {
            let _ = ShowWindow(window, SW_HIDE);
        }
        return;
    }
    // SAFETY: CandidateWindow owns the stable pointer until display teardown.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut PaintState;
    if state.is_null() {
        return;
    }
    // SAFETY: window messages are serialized on the renderer thread.
    let state = unsafe { &mut *state };
    if !set_window_rect(state.delete_overlay, rect, Some(HWND_TOPMOST)) {
        state.visible = false;
        hide_delete_overlay(state.delete_overlay);
        return;
    }
    let _ = refresh_delete_overlay_for_dpi(state);
}

fn apply_overlay_dpi_change(window: HWND, _value: LPARAM) {
    // The display popup owns screen placement. Do not use the overlay's
    // independently delivered DPI notification to redraw or reveal a region:
    // it can arrive before the display has accepted its suggested rectangle.
    // The display WM_DPICHANGED path rebuilds this overlay once both HWNDs
    // share the same screen rectangle; until then, no stale target exists.
    hide_delete_overlay(window);
}

fn hide_delete_overlay_for_display(display_window: HWND) {
    // SAFETY: this only reads the shared pointer installed by CandidateWindow.
    let state = unsafe { GetWindowLongPtrW(display_window, GWLP_USERDATA) } as *mut PaintState;
    if !state.is_null() {
        // SAFETY: CandidateWindow owns the box until it clears the window data
        // during teardown, and WM_DPICHANGED is serialized on its UI thread.
        let state = unsafe { &mut *state };
        state.visible = false;
        state.accessibility.hide();
        hide_delete_overlay(state.delete_overlay);
    }
}

fn point_from_lparam(value: LPARAM) -> POINT {
    let packed = value.0 as u32;
    POINT {
        x: (packed as u16 as i16) as i32,
        y: ((packed >> 16) as u16 as i16) as i32,
    }
}

fn point_in_rect(point: POINT, rect: RECT) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

fn history_delete_at_client_point(
    state: &PaintState,
    point: POINT,
) -> Option<HistoryDeleteRequest> {
    let candidates = state.candidates.as_ref()?;
    let popup = state.layout?;
    let layout = state.candidate_layout?;
    history_delete_request_at_client_point(candidates, popup, layout, state.revision, point)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HistoryDeleteTarget {
    candidate_index: usize,
    row: RECT,
    hit: RECT,
}

fn history_delete_targets(
    candidates: &CandidateList,
    popup: PopupLayout,
    layout: Layout,
) -> Vec<HistoryDeleteTarget> {
    candidates
        .visible_range()
        .enumerate()
        .filter_map(|(row_index, candidate_index)| {
            let candidate = candidates.items.get(candidate_index)?;
            is_deletable_history(candidate).then(|| {
                let row = RECT {
                    left: popup.candidates.left,
                    top: popup
                        .candidates
                        .top
                        .saturating_add(layout.row_height.saturating_mul(row_index as i32)),
                    right: popup.candidates.right,
                    bottom: popup.candidates.top.saturating_add(
                        layout
                            .row_height
                            .saturating_mul((row_index as i32).saturating_add(1)),
                    ),
                };
                HistoryDeleteTarget {
                    candidate_index,
                    row,
                    hit: history_delete_hit_rect(row, layout),
                }
            })
        })
        .collect()
}

fn history_delete_request_at_client_point(
    candidates: &CandidateList,
    popup: PopupLayout,
    layout: Layout,
    revision: u64,
    point: POINT,
) -> Option<HistoryDeleteRequest> {
    history_delete_targets(candidates, popup, layout)
        .into_iter()
        .find(|target| point_in_rect(point, target.hit))
        .and_then(|target| u16::try_from(target.candidate_index).ok())
        .map(|candidate_index| HistoryDeleteRequest {
            revision,
            candidate_index,
        })
}

/// Replaces the overlay shape atomically. Every component region is released
/// locally; after successful `SetWindowRgn`, Windows owns `aggregate`.
fn rebuild_delete_overlay_region(window: HWND, targets: &[HistoryDeleteTarget]) -> bool {
    if targets.is_empty() {
        return clear_delete_overlay_region(window);
    }
    // SAFETY: all regions are owned by this routine until ownership transfers
    // to the live overlay HWND through successful `SetWindowRgn`.
    unsafe {
        let aggregate = CreateRectRgn(0, 0, 0, 0);
        if aggregate.is_invalid() {
            return false;
        }
        for target in targets {
            let component = CreateRectRgn(
                target.hit.left,
                target.hit.top,
                target.hit.right,
                target.hit.bottom,
            );
            if component.is_invalid() {
                let _ = DeleteObject(aggregate.into());
                return false;
            }
            let combined = CombineRgn(Some(aggregate), Some(aggregate), Some(component), RGN_OR);
            let _ = DeleteObject(component.into());
            if combined == RGN_ERROR {
                let _ = DeleteObject(aggregate.into());
                return false;
            }
        }
        if SetWindowRgn(window, Some(aggregate), true) != 0 {
            true
        } else {
            let _ = DeleteObject(aggregate.into());
            false
        }
    }
}

fn clear_delete_overlay_region(window: HWND) -> bool {
    // SAFETY: `None` removes any region currently owned by Windows. It does
    // not transfer or release a caller-owned GDI object.
    unsafe { SetWindowRgn(window, None, true) != 0 }
}

fn hide_delete_overlay(window: HWND) {
    // Clear before hiding so a later valid update can never reveal a stale
    // region or pixels while the display popup is input-disabled.
    let _ = clear_delete_overlay_region(window);
    // SAFETY: the caller owns this live overlay HWND; hiding is non-activating.
    unsafe {
        let _ = ShowWindow(window, SW_HIDE);
    }
}

fn delete_overlay_should_be_visible(
    display_visible: bool,
    region_ready: bool,
    targets: &[HistoryDeleteTarget],
) -> bool {
    display_visible && region_ready && !targets.is_empty()
}

fn clear_pending_history_deletes_for_new_revision(
    pending: &mut Vec<HistoryDeleteRequest>,
    previous_revision: u64,
    next_revision: u64,
) {
    if previous_revision != next_revision {
        pending.clear();
    }
}

fn is_history_delete_pending(
    pending: &[HistoryDeleteRequest],
    request: HistoryDeleteRequest,
) -> bool {
    pending.contains(&request)
}

fn finish_pending_history_delete(
    pending: &mut Vec<HistoryDeleteRequest>,
    request: HistoryDeleteRequest,
    removed: bool,
) {
    if !removed {
        pending.retain(|pending_request| *pending_request != request);
    }
}

fn queue_history_delete(window: HWND, point: POINT) {
    // SAFETY: CandidateWindow owns this stable box until the HWND is destroyed.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut PaintState;
    if state.is_null() {
        return;
    }
    // SAFETY: candidate window messages run on its owner thread, so this is
    // the only mutable access to the state during the handler.
    let state = unsafe { &mut *state };
    let Some(request) = history_delete_at_client_point(state, point) else {
        return;
    };
    if is_history_delete_pending(&state.pending_history_deletes, request) {
        return;
    }
    match state.delete_history.try_send(request) {
        Ok(()) => state.pending_history_deletes.push(request),
        // A bounded queue must never turn a click storm into unbounded work.
        // Keep the candidate visible and wait for the next engine UiState.
        Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
    }
}

fn paint_display(window: HWND) {
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
                palette(state.appearance_theme),
            );
        }
    }
    // SAFETY: pairs with the successful `BeginPaint` above.
    unsafe {
        let _ = EndPaint(window, &ps);
    }
}

fn paint_delete_overlay(window: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: every non-invalid paint DC is paired with `EndPaint` below.
    let dc = unsafe { BeginPaint(window, &mut ps) };
    if dc.is_invalid() {
        return;
    }
    // SAFETY: this only reads the stable pointer installed by CandidateWindow.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const PaintState;
    if !state.is_null() {
        // SAFETY: the owner clears the pointer before destroying the overlay.
        let state = unsafe { &*state };
        if let (Some(candidates), Some(popup), Some(layout)) = (
            state.candidates.as_ref(),
            state.layout,
            state.candidate_layout,
        ) {
            draw_delete_overlay(
                dc,
                candidates,
                popup,
                layout,
                palette(state.appearance_theme),
            );
        }
    }
    // SAFETY: pairs with the successful `BeginPaint` above.
    unsafe {
        let _ = EndPaint(window, &ps);
    }
}

fn draw_delete_overlay(
    dc: windows::Win32::Graphics::Gdi::HDC,
    candidates: &CandidateList,
    popup: PopupLayout,
    layout: Layout,
    palette: Palette,
) {
    for target in history_delete_targets(candidates, popup, layout) {
        let selected = target.candidate_index == usize::from(candidates.selected);
        fill_color(
            dc,
            &target.hit,
            if selected {
                palette.selected
            } else {
                palette.surface
            },
        );
        draw_history_delete_glyph(
            dc,
            history_delete_rect(target.row, layout),
            palette.action,
            layout.history_delete_stroke,
        );
    }
}

fn draw(
    dc: windows::Win32::Graphics::Gdi::HDC,
    client: RECT,
    candidates: &CandidateList,
    detail: Option<&CandidateDetail>,
    popup: PopupLayout,
    dpi: u32,
    palette: Palette,
) {
    let candidate_client = popup.candidates;
    let layout = layout(candidates, dpi);
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
                DT_LEFT | DT_END_ELLIPSIS,
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
    // Size from the complete current page even when compact presentation only
    // paints the selected row. Selection changes must never make the popup or
    // either text column jump horizontally.
    let sizing_range = candidates.current_page_range();
    let annotation_width_96 = sizing_range
        .clone()
        .map(|index| text_width_96(&candidates.items[index].annotation, true))
        .max()
        .unwrap_or(0)
        .min(MAX_WIDTH_96 / 2);
    let surface_width_96 = sizing_range
        .map(|index| text_width_96(&candidates.items[index].text, false))
        .max()
        .unwrap_or(0)
        .min(MAX_WIDTH_96);
    let annotation_gap_96 = i32::from(annotation_width_96 > 0) * GAP_96;
    let content_width_96 = PADDING_96
        .saturating_mul(2)
        .saturating_add(if candidates.items.iter().any(is_deletable_history) {
            HISTORY_DELETE_HIT_SIZE_96.saturating_add(HISTORY_DELETE_GAP_96)
        } else {
            0
        })
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
        detour: scaled(MAX_CARET_DETOUR_96, dpi),
        number_width: scaled(NUMBER_WIDTH_96, dpi),
        annotation_width: scaled(annotation_width_96, dpi),
        rail_width: scaled(RAIL_WIDTH_96, dpi).max(1),
        rail_margin: scaled(RAIL_MARGIN_96, dpi),
        history_delete_glyph_size: scaled(HISTORY_DELETE_GLYPH_SIZE_96, dpi).max(1),
        history_delete_hit_size: scaled(HISTORY_DELETE_HIT_SIZE_96, dpi).max(1),
        history_delete_stroke: scaled(HISTORY_DELETE_STROKE_96, dpi).max(1),
        history_delete_gutter: if candidates.items.iter().any(is_deletable_history) {
            scaled(
                HISTORY_DELETE_HIT_SIZE_96.saturating_add(HISTORY_DELETE_GAP_96),
                dpi,
            )
        } else {
            0
        },
    }
}

fn detail_layout(
    surface: &str,
    detail: &CandidateDetail,
    dpi: u32,
    max_height: i32,
) -> DetailLayout {
    let width = scaled(DETAIL_WIDTH_96, dpi);
    let padding = scaled(DETAIL_PADDING_96, dpi);
    let title_height = scaled(DETAIL_TITLE_HEIGHT_96, dpi);
    let line_height = scaled(DETAIL_LINE_HEIGHT_96, dpi);
    let section_gap = scaled(DETAIL_SECTION_GAP_96, dpi);
    let sections = [
        &detail.aliases,
        &detail.related,
        &detail.similar,
        &detail.antonyms,
    ]
    .into_iter()
    .filter(|group| !group.is_empty())
    .count() as i32;
    let reading_height = i32::from(detail.reading != surface).saturating_mul(line_height);
    let fixed_height = padding
        .saturating_mul(2)
        .saturating_add(title_height)
        .saturating_add(reading_height)
        .saturating_add(sections.saturating_mul(line_height.saturating_add(section_gap)));
    let content_width = width.saturating_sub(padding.saturating_mul(2));
    let full_line_count = wrapped_line_count(&detail.definition, content_width, dpi);
    let available_definition_height = max_height.saturating_sub(fixed_height).max(line_height);
    let visible_line_count = full_line_count
        .min((available_definition_height / line_height).max(1) as usize)
        .max(1);
    DetailLayout {
        width,
        height: fixed_height.saturating_add(line_height.saturating_mul(visible_line_count as i32)),
        padding,
        title_height,
        line_height,
        section_gap,
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
    let layout = detail_layout(surface, detail, dpi, rect.bottom.saturating_sub(rect.top));
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
    let relation_height = [
        &detail.aliases,
        &detail.related,
        &detail.similar,
        &detail.antonyms,
    ]
    .into_iter()
    .filter(|group| !group.is_empty())
    .count() as i32
        * layout.line_height.saturating_add(layout.section_gap);
    let available_definition_height = content
        .bottom
        .saturating_sub(cursor)
        .saturating_sub(relation_height);
    let max_definition_lines = (available_definition_height / layout.line_height).max(1) as usize;
    for line in definition_lines(
        &detail.definition,
        content.right.saturating_sub(content.left),
        dpi,
        max_definition_lines,
        detail.definition_truncated,
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

fn characters_per_line(available_width: i32, dpi: u32) -> usize {
    // Use the widest expected glyph advance so CJK, emoji, and combining text
    // remain inside the measured column at every DPI.
    (available_width / scaled(SUPPORT_FONT_96, dpi).max(1)).max(1) as usize
}

fn wrapped_line_count(definition: &str, available_width: i32, dpi: u32) -> usize {
    definition
        .chars()
        .count()
        .max(1)
        .div_ceil(characters_per_line(available_width, dpi))
}

fn definition_lines(
    definition: &str,
    available_width: i32,
    dpi: u32,
    max_lines: usize,
    source_truncated: bool,
) -> Vec<String> {
    let per_line = characters_per_line(available_width, dpi);
    let max_lines = max_lines.max(1);
    let mut characters = definition.chars().peekable();
    let mut lines =
        Vec::with_capacity(max_lines.min(wrapped_line_count(definition, available_width, dpi)));
    while characters.peek().is_some() && lines.len() < max_lines {
        lines.push(characters.by_ref().take(per_line).collect::<String>());
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    if characters.peek().is_some() || source_truncated {
        let last = lines.last_mut().expect("one definition line");
        if last.chars().count() >= per_line {
            last.pop();
        }
        last.push('…');
    }
    lines
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
    // The delete target is a right-side affordance. Reserve its hit-target
    // gutter for every row in a list that has history entries, without moving
    // the established number column away from the left edge.
    let annotation_right = row
        .right
        .saturating_sub(layout.padding)
        .saturating_sub(layout.history_delete_gutter);
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

/// Only the engine-marked history capability is interactive. The renderer does
/// not infer history status from text, annotations, aliases, or any other
/// presentation detail.
fn is_deletable_history(candidate: &sakura_proto::Candidate) -> bool {
    candidate.deletable_history
}

fn history_delete_rect(row: RECT, layout: Layout) -> RECT {
    let right = row.right.saturating_sub(layout.padding);
    let left = right.saturating_sub(layout.history_delete_glyph_size);
    let top = row.top.saturating_add(
        (row.bottom
            .saturating_sub(row.top)
            .saturating_sub(layout.history_delete_glyph_size))
            / 2,
    );
    RECT {
        left,
        top,
        right,
        bottom: top.saturating_add(layout.history_delete_glyph_size),
    }
}

/// The visual glyph stays restrained, while this larger independent target is
/// what receives pointer input. Clamp it to the row so no hit test leaks into
/// adjacent rows or the surrounding passive popup.
fn history_delete_hit_rect(row: RECT, layout: Layout) -> RECT {
    let glyph = history_delete_rect(row, layout);
    let row_width = row.right.saturating_sub(row.left).max(0);
    let row_height = row.bottom.saturating_sub(row.top).max(0);
    let width = layout.history_delete_hit_size.min(row_width);
    let height = layout.history_delete_hit_size.min(row_height);
    let center_x = glyph.left.saturating_add(glyph.right).div_euclid(2);
    let center_y = glyph.top.saturating_add(glyph.bottom).div_euclid(2);
    let left = center_x
        .saturating_sub(width / 2)
        .clamp(row.left, row.right.saturating_sub(width));
    let top = center_y
        .saturating_sub(height / 2)
        .clamp(row.top, row.bottom.saturating_sub(height));
    RECT {
        left,
        top,
        right: left.saturating_add(width),
        bottom: top.saturating_add(height),
    }
}

/// Draw a small independent trash can using only Sakura's restrained palette.
/// It intentionally avoids a system icon or third-party glyph asset.
fn draw_history_delete_glyph(
    dc: windows::Win32::Graphics::Gdi::HDC,
    rect: RECT,
    color: COLORREF,
    stroke: i32,
) {
    let width = rect.right.saturating_sub(rect.left).max(1);
    let stroke = stroke.max(1).min(width / 3);
    let lid = RECT {
        left: rect.left.saturating_add(stroke),
        top: rect.top.saturating_add(stroke.saturating_mul(2)),
        right: rect.right.saturating_sub(stroke),
        bottom: rect.top.saturating_add(stroke.saturating_mul(3)),
    };
    let handle = RECT {
        left: rect
            .left
            .saturating_add(width / 2)
            .saturating_sub(stroke / 2),
        top: rect.top,
        right: rect
            .left
            .saturating_add(width / 2)
            .saturating_add((stroke.saturating_add(1)) / 2),
        bottom: rect.top.saturating_add(stroke.saturating_mul(2)),
    };
    let body = RECT {
        left: rect.left.saturating_add(stroke.saturating_mul(2)),
        top: lid.bottom,
        right: rect.right.saturating_sub(stroke.saturating_mul(2)),
        bottom: rect.bottom.saturating_sub(stroke),
    };
    fill_color(dc, &lid, color);
    fill_color(dc, &handle, color);
    // Four one-stroke bands make the can a thin outline rather than a filled
    // body, retaining the row surface through its center at every DPI.
    for edge in [
        RECT {
            bottom: body.top.saturating_add(stroke),
            ..body
        },
        RECT {
            top: body.bottom.saturating_sub(stroke),
            ..body
        },
        RECT {
            right: body.left.saturating_add(stroke),
            ..body
        },
        RECT {
            left: body.right.saturating_sub(stroke),
            ..body
        },
    ] {
        fill_color(dc, &edge, color);
    }
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

pub(crate) fn palette(theme: AppearanceTheme) -> Palette {
    if high_contrast_enabled() {
        high_contrast_palette()
    } else {
        resolve_palette(theme, false, windows_apps_use_light_theme())
    }
}

/// Resolves the palette from inputs that can be tested without reading global
/// Windows state. High contrast deliberately precedes an explicit preference:
/// accessibility system roles must remain authoritative for every Sakura-owned
/// candidate surface.
fn resolve_palette(
    theme: AppearanceTheme,
    high_contrast: bool,
    apps_use_light_theme: bool,
) -> Palette {
    if high_contrast {
        high_contrast_palette()
    } else {
        match theme {
            AppearanceTheme::Auto if apps_use_light_theme => light_palette(),
            AppearanceTheme::Auto | AppearanceTheme::Dark => dark_palette(),
            AppearanceTheme::Light => light_palette(),
        }
    }
}

fn windows_apps_use_light_theme() -> bool {
    let mut value = 1_u32;
    let mut bytes = size_of::<u32>() as u32;
    // SAFETY: the registry names are static NUL-terminated strings, and the
    // value buffer has the exact REG_DWORD size supplied through `bytes`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast::<c_void>()),
            Some(&mut bytes),
        )
    };
    // Missing, malformed, or unreadable values must preserve a legible
    // candidate window. Windows applications conventionally default to light.
    if status == windows::Win32::Foundation::ERROR_SUCCESS && bytes == size_of::<u32>() as u32 {
        value != 0
    } else {
        true
    }
}

fn high_contrast_palette() -> Palette {
    Palette {
        surface: system_color(COLOR_WINDOW),
        ink: system_color(COLOR_WINDOWTEXT),
        annotation: system_color(COLOR_GRAYTEXT),
        selected: system_color(COLOR_HIGHLIGHT),
        selected_ink: system_color(COLOR_HIGHLIGHTTEXT),
        rail: system_color(COLOR_HIGHLIGHT),
        border: system_color(COLOR_3DSHADOW),
        action: system_color(COLOR_WINDOWTEXT),
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
        ink: rgb(0x2F, 0x2F, 0x2F),
        annotation: rgb(0x70, 0x70, 0x70),
        selected: rgb(0xE8, 0xE5, 0xE2),
        selected_ink: rgb(0x2F, 0x2F, 0x2F),
        rail: rgb(0xB2, 0x8D, 0x96),
        border: rgb(0xBD, 0xB9, 0xB5),
        action: rgb(0x89, 0x72, 0x77),
    }
}

fn dark_palette() -> Palette {
    Palette {
        surface: rgb(0x35, 0x35, 0x35),
        ink: rgb(0xF2, 0xF0, 0xEE),
        annotation: rgb(0xC9, 0xC4, 0xC0),
        selected: rgb(0x25, 0x25, 0x25),
        selected_ink: rgb(0xFF, 0xFD, 0xFB),
        rail: rgb(0xD7, 0xA6, 0xB1),
        border: rgb(0x72, 0x6E, 0x6B),
        action: rgb(0xE7, 0xC1, 0xC8),
    }
}

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(u32::from_le_bytes([red, green, blue, 0]))
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
            deletable_history: false,
        }
    }

    fn deletable_history_item(text: &str) -> Candidate {
        Candidate {
            text: text.to_owned(),
            annotation: "presentation-only".to_owned(),
            deletable_history: true,
        }
    }

    fn detail() -> CandidateDetail {
        CandidateDetail {
            reading: "ようご".to_owned(),
            definition: "絵文字😀と結合文字e\u{301}を含む、全文折り返し表示の説明です。".repeat(8),
            definition_truncated: true,
            aliases: vec!["別名A".to_owned(), "別名B".to_owned()],
            related: vec!["関連語".to_owned()],
            similar: Vec::new(),
            antonyms: vec!["反対語".to_owned()],
        }
    }

    fn screen(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
        ScreenRect {
            left,
            top,
            right,
            bottom,
        }
    }

    fn inflate_y(rect: ScreenRect, px: i32) -> ScreenRect {
        ScreenRect {
            top: rect.top.saturating_sub(px),
            bottom: rect.bottom.saturating_add(px),
            ..rect
        }
    }

    fn composition_cover_is_unavoidable(
        anchor: ScreenRect,
        height: i32,
        work: RECT,
        gap: i32,
    ) -> bool {
        let below = anchor.bottom.saturating_add(gap);
        let above = anchor.top.saturating_sub(gap).saturating_sub(height);
        below.saturating_add(height) > work.bottom && above < work.top
    }

    /// A 5px-grid oracle: if any popup origin on that grid clears both
    /// rectangles and stays within the detour, the production placement
    /// had a nearby alternative to covering the composition.
    fn clear_origin_on_five_px_grid(
        anchor: ScreenRect,
        document: Option<ScreenRect>,
        width: i32,
        height: i32,
        work: RECT,
        detour: i32,
    ) -> Option<RECT> {
        let max_x = (work.right.saturating_sub(width)).max(work.left);
        let max_y = (work.bottom.saturating_sub(height)).max(work.top);
        let mut xs = vec![work.left, max_x, anchor.left.clamp(work.left, max_x)];
        if let Some(document) = document {
            xs.push(document.left.saturating_sub(8).saturating_sub(width));
            xs.push(document.right.saturating_add(8));
        }
        xs.sort_unstable();
        xs.dedup();
        let mut y = work.top;
        while y <= max_y {
            for &x in &xs {
                let left = x.clamp(work.left, max_x);
                let rect = RECT {
                    left,
                    top: y,
                    right: left.saturating_add(width),
                    bottom: y.saturating_add(height),
                };
                if rect.left >= work.left
                    && rect.right <= work.right
                    && rect.top >= work.top
                    && rect.bottom <= work.bottom
                    && caret_distance(rect, anchor) <= detour
                    && !covers_composition(rect, anchor)
                    && !covers_document(rect, document)
                {
                    return Some(rect);
                }
            }
            let next = y.saturating_add(5);
            if next <= y {
                break;
            }
            y = next;
        }
        None
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
        assert_eq!(light_palette().ink, rgb(0x2F, 0x2F, 0x2F));
        assert_eq!(light_palette().annotation, rgb(0x70, 0x70, 0x70));
        assert_eq!(light_palette().selected, rgb(0xE8, 0xE5, 0xE2));
        assert_eq!(light_palette().selected_ink, rgb(0x2F, 0x2F, 0x2F));
        assert_eq!(light_palette().rail, rgb(0xB2, 0x8D, 0x96));
        assert_eq!(light_palette().border, rgb(0xBD, 0xB9, 0xB5));
        assert_eq!(light_palette().action, rgb(0x89, 0x72, 0x77));
        assert_eq!(dark_palette().surface, rgb(0x35, 0x35, 0x35));
        assert_eq!(dark_palette().ink, rgb(0xF2, 0xF0, 0xEE));
        assert_eq!(dark_palette().annotation, rgb(0xC9, 0xC4, 0xC0));
        assert_eq!(dark_palette().selected, rgb(0x25, 0x25, 0x25));
        assert_eq!(dark_palette().rail, rgb(0xD7, 0xA6, 0xB1));
        assert_eq!(dark_palette().border, rgb(0x72, 0x6E, 0x6B));
        assert_eq!(dark_palette().action, rgb(0xE7, 0xC1, 0xC8));
        assert_ne!(light_palette(), dark_palette());
    }

    #[test]
    fn explicit_themes_ignore_the_windows_auto_preference() {
        for apps_use_light_theme in [false, true] {
            assert_eq!(
                resolve_palette(AppearanceTheme::Light, false, apps_use_light_theme),
                light_palette()
            );
            assert_eq!(
                resolve_palette(AppearanceTheme::Dark, false, apps_use_light_theme),
                dark_palette()
            );
        }
    }

    #[test]
    fn auto_theme_follows_the_injected_apps_use_light_theme_preference() {
        assert_eq!(
            resolve_palette(AppearanceTheme::Auto, false, true),
            light_palette()
        );
        assert_eq!(
            resolve_palette(AppearanceTheme::Auto, false, false),
            dark_palette()
        );
    }

    #[test]
    fn high_contrast_has_priority_over_all_appearance_choices() {
        for theme in [
            AppearanceTheme::Auto,
            AppearanceTheme::Light,
            AppearanceTheme::Dark,
        ] {
            for apps_use_light_theme in [false, true] {
                assert_eq!(
                    resolve_palette(theme, true, apps_use_light_theme),
                    high_contrast_palette()
                );
            }
        }
    }

    #[test]
    fn high_contrast_delete_glyph_uses_the_system_window_text_role() {
        let palette = high_contrast_palette();
        assert_eq!(palette.action, system_color(COLOR_WINDOWTEXT));
        assert_eq!(palette.surface, system_color(COLOR_WINDOW));
        assert_eq!(palette.selected, system_color(COLOR_HIGHLIGHT));
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
    fn history_delete_is_typed_right_aligned_and_has_a_larger_dpi_scaled_hit_target() {
        let list = candidates(
            vec![
                item("annotation-is-not-a-capability", "履歴"),
                deletable_history_item("history"),
            ],
            0,
            CandidateKind::Suggestion,
        );
        for dpi in [96, 120, 144, 168, 192] {
            let layout = layout(&list, dpi);
            assert_eq!(
                layout.history_delete_glyph_size,
                scaled(HISTORY_DELETE_GLYPH_SIZE_96, dpi)
            );
            assert_eq!(
                layout.history_delete_hit_size,
                scaled(HISTORY_DELETE_HIT_SIZE_96, dpi)
            );
            assert_eq!(
                layout.history_delete_stroke,
                scaled(HISTORY_DELETE_STROKE_96, dpi)
            );
            assert_eq!(
                layout.history_delete_gutter,
                scaled(HISTORY_DELETE_HIT_SIZE_96 + HISTORY_DELETE_GAP_96, dpi)
            );
            let popup = PopupLayout {
                candidates: RECT {
                    left: 0,
                    top: 0,
                    right: layout.width,
                    bottom: layout.height,
                },
                detail: None,
            };
            let second_row = RECT {
                left: 0,
                top: layout.row_height,
                right: layout.width,
                bottom: layout.row_height * 2,
            };
            let glyph = history_delete_rect(second_row, layout);
            let hit_rect = history_delete_hit_rect(second_row, layout);
            assert_eq!(glyph.right, second_row.right - layout.padding);
            assert_eq!(glyph.right - glyph.left, layout.history_delete_glyph_size);
            assert_eq!(
                hit_rect.right - hit_rect.left,
                layout.history_delete_hit_size
            );
            assert_eq!(
                hit_rect.bottom - hit_rect.top,
                layout.history_delete_hit_size
            );
            assert!(hit_rect.left <= glyph.left);
            assert!(glyph.right <= hit_rect.right);
            assert!(
                (hit_rect.left + hit_rect.right - glyph.left - glyph.right).abs() <= 1,
                "the larger target stays centered on the visual glyph"
            );
            assert!(second_row.left <= hit_rect.left);
            assert!(hit_rect.right <= second_row.right);
            assert!(second_row.top <= hit_rect.top);
            assert!(hit_rect.bottom <= second_row.bottom);
            let hit = POINT {
                x: hit_rect.left,
                y: (hit_rect.top + hit_rect.bottom) / 2,
            };
            assert!(hit.x < glyph.left, "pointer target exceeds the glyph");
            let targets = history_delete_targets(&list, popup, layout);
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].candidate_index, 1);
            assert_eq!(targets[0].row, second_row);
            assert_eq!(targets[0].hit, hit_rect);
            assert_eq!(
                history_delete_request_at_client_point(&list, popup, layout, 41, hit),
                Some(HistoryDeleteRequest {
                    revision: 41,
                    candidate_index: 1,
                })
            );
            let first_row_hit = history_delete_hit_rect(
                RECT {
                    left: 0,
                    top: 0,
                    right: layout.width,
                    bottom: layout.row_height,
                },
                layout,
            );
            assert_eq!(
                history_delete_request_at_client_point(
                    &list,
                    popup,
                    layout,
                    41,
                    POINT {
                        x: (first_row_hit.left + first_row_hit.right) / 2,
                        y: (first_row_hit.top + first_row_hit.bottom) / 2,
                    },
                ),
                None,
                "annotation alone must never create a deletion capability"
            );
            assert_eq!(
                history_delete_request_at_client_point(
                    &list,
                    popup,
                    layout,
                    41,
                    POINT {
                        x: hit_rect.left.saturating_sub(1),
                        y: hit.y,
                    },
                ),
                None,
                "row text remains click-through"
            );

            let (surface, annotation) = candidate_columns(second_row, layout);
            assert!(surface.right <= annotation.left || layout.annotation_width == 0);
            assert!(annotation.right <= hit_rect.left);
        }
    }

    #[test]
    fn input_disabled_display_and_enabled_overlay_styles_are_separate() {
        let display_ex = display_popup_ex_style();
        let overlay_ex = delete_overlay_ex_style();
        for style in [display_ex, overlay_ex] {
            assert_ne!(style.0 & WS_EX_NOACTIVATE.0, 0);
            assert_ne!(style.0 & WS_EX_TOOLWINDOW.0, 0);
            assert_ne!(style.0 & WS_EX_TOPMOST.0, 0);
            assert_eq!(style.0 & WS_EX_TRANSPARENT.0, 0);
        }
        assert_ne!(display_popup_style().0 & WS_DISABLED.0, 0);
        assert_eq!(delete_overlay_style().0 & WS_DISABLED.0, 0);
        assert_eq!(
            delete_overlay_non_client_hit_test_result().0,
            HTCLIENT as isize,
            "the region-clipped overlay receives only delete-target pointer input"
        );
    }

    #[test]
    fn overlay_visibility_requires_a_visible_display_a_valid_region_and_current_page_targets() {
        let target = HistoryDeleteTarget {
            candidate_index: 3,
            row: RECT {
                left: 0,
                top: 0,
                right: 28,
                bottom: 28,
            },
            hit: RECT {
                left: 2,
                top: 2,
                right: 26,
                bottom: 26,
            },
        };

        assert!(delete_overlay_should_be_visible(true, true, &[target]));
        assert!(!delete_overlay_should_be_visible(false, true, &[target]));
        assert!(!delete_overlay_should_be_visible(true, false, &[target]));
        assert!(!delete_overlay_should_be_visible(true, true, &[]));
    }

    #[test]
    fn pending_deletes_keep_each_typed_target_suppressed_until_a_new_ui_revision() {
        let first = HistoryDeleteRequest {
            revision: 41,
            candidate_index: 2,
        };
        let second = HistoryDeleteRequest {
            revision: 41,
            candidate_index: 7,
        };
        let mut pending = Vec::new();

        assert!(!is_history_delete_pending(&pending, first));
        pending.push(first);
        assert!(is_history_delete_pending(&pending, first));
        assert!(!is_history_delete_pending(&pending, second));
        pending.push(second);
        assert!(is_history_delete_pending(&pending, first));
        assert!(is_history_delete_pending(&pending, second));

        clear_pending_history_deletes_for_new_revision(&mut pending, 41, 41);
        assert_eq!(pending, vec![first, second]);
        clear_pending_history_deletes_for_new_revision(&mut pending, 41, 42);
        assert!(pending.is_empty());
        assert!(!is_history_delete_pending(&pending, first));
    }

    #[test]
    fn failed_delete_releases_only_its_target_while_success_waits_for_new_revision() {
        let failed = HistoryDeleteRequest {
            revision: 41,
            candidate_index: 2,
        };
        let removed = HistoryDeleteRequest {
            revision: 41,
            candidate_index: 7,
        };
        let mut pending = vec![failed, removed];

        finish_pending_history_delete(&mut pending, failed, false);
        assert_eq!(pending, vec![removed]);
        finish_pending_history_delete(&mut pending, removed, true);
        assert_eq!(
            pending,
            vec![removed],
            "an authoritative removal stays suppressed until a newer UiState"
        );
    }

    #[test]
    fn accessibility_uses_the_requesting_hwnd_for_base_and_overlay() {
        let base = HWND::default();
        let overlay = HWND(std::ptr::dangling_mut());

        assert_eq!(candidate_accessibility_request_window(base), base);
        assert_eq!(candidate_accessibility_request_window(overlay), overlay);
        assert_ne!(
            candidate_accessibility_request_window(base),
            candidate_accessibility_request_window(overlay),
            "each WM_GETOBJECT response must be returned through its source HWND"
        );
    }

    #[test]
    fn delete_overlay_targets_only_include_the_current_page_and_global_indices() {
        let mut items = (0..=CANDIDATE_PAGE_SIZE)
            .map(|index| item(&format!("candidate-{index}"), ""))
            .collect::<Vec<_>>();
        items[0] = deletable_history_item("first-page-history");
        items[CANDIDATE_PAGE_SIZE] = deletable_history_item("second-page-history");
        let list = candidates(items, CANDIDATE_PAGE_SIZE as u16, CandidateKind::Suggestion);
        let layout = layout(&list, 144);
        let popup = PopupLayout {
            candidates: RECT {
                left: 11,
                top: 13,
                right: 11 + layout.width,
                bottom: 13 + layout.height,
            },
            detail: None,
        };
        let targets = history_delete_targets(&list, popup, layout);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].candidate_index, CANDIDATE_PAGE_SIZE);
        assert_eq!(targets[0].row.top, popup.candidates.top);
        assert_eq!(
            history_delete_request_at_client_point(
                &list,
                popup,
                layout,
                99,
                POINT {
                    x: (targets[0].hit.left + targets[0].hit.right) / 2,
                    y: (targets[0].hit.top + targets[0].hit.bottom) / 2,
                },
            ),
            Some(HistoryDeleteRequest {
                revision: 99,
                candidate_index: CANDIDATE_PAGE_SIZE as u16,
            })
        );
    }

    #[test]
    fn compact_selection_never_changes_page_sized_width_or_columns() {
        let items = (0..CANDIDATE_PAGE_SIZE)
            .map(|index| {
                item(
                    &"候補".repeat(index.saturating_add(1)),
                    if index % 3 == 0 { "履歴" } else { "" },
                )
            })
            .collect::<Vec<_>>();
        let mut list = candidates(items, 0, CandidateKind::Conversion);
        list.presentation = CandidatePresentation::Compact;
        let baseline = layout(&list, 96);
        for selected in 0..CANDIDATE_PAGE_SIZE {
            list.selected = selected as u16;
            let current = layout(&list, 96);
            assert_eq!(current.width, baseline.width);
            assert_eq!(current.annotation_width, baseline.annotation_width);
            assert_eq!(current.height, baseline.height);
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

    /// When the popup is taller than the free space both below and above the
    /// composition, covering it is unavoidable — but the popup must take the
    /// roomier side, not blindly slide up over the caret line from below.
    #[test]
    fn placement_covers_from_the_roomier_side_only_when_nothing_fits() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1_000,
            bottom: 600,
        };
        let low_anchor = ScreenRect {
            left: 100,
            top: 500,
            right: 300,
            bottom: 524,
        };
        let pinned_top = place(low_anchor, 300, 550, work, 8);
        assert_eq!(pinned_top.top, work.top);

        let high_anchor = ScreenRect {
            left: 100,
            top: 30,
            right: 300,
            bottom: 54,
        };
        let pinned_bottom = place(high_anchor, 300, 550, work, 8);
        assert_eq!(pinned_bottom.bottom, work.bottom);
    }

    /// The reported bug, at its measured geometry: a multi-line box whose
    /// caret sits near the top. "Below the composition" clears the caret
    /// line and still lands inside the box the user is typing into.
    /// The measured 884x581 host edit control. Every placement that clears a
    /// box that deep is a long way from the caret, so the popup stays where
    /// the user is looking: directly under the caret, on the box's empty
    /// part. Moving it beside the box instead put it 858 px away.
    #[test]
    fn a_deep_editable_box_keeps_the_popup_next_to_the_caret() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1_920,
            bottom: 1_032,
        };
        let document = ScreenRect {
            left: 208,
            top: 191,
            right: 1_092,
            bottom: 772,
        };
        let anchor = ScreenRect {
            left: 216,
            top: 195,
            right: 280,
            bottom: 225,
        };

        let placed = place_candidates(anchor, Some(document), 260, 274, work, 4, 160);
        assert_eq!(placed, place(anchor, 260, 274, work, 4));
        assert_eq!(caret_distance(placed, anchor), 4);
        assert!(placed.left >= work.left && placed.right <= work.right);
        assert!(placed.top >= work.top && placed.bottom <= work.bottom);
    }

    /// Each rung of the ladder in turn — and the budget that stops the
    /// ladder — so neither a later rung nor the fallback can silently become
    /// the answer for a case the other should have taken.
    #[test]
    fn placement_only_detours_while_it_stays_next_to_the_caret() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1_000,
            bottom: 600,
        };
        let gap = 4;
        let detour = 160;

        // A box no taller than the caret line: below is already clear, and
        // the popup must not move off it.
        let short = ScreenRect {
            left: 100,
            top: 100,
            right: 400,
            bottom: 130,
        };
        let anchor = ScreenRect {
            left: 100,
            top: 100,
            right: 160,
            bottom: 130,
        };
        assert_eq!(
            place_candidates(anchor, Some(short), 200, 200, work, gap, detour),
            place(anchor, 200, 200, work, gap),
        );

        // A chat-height box at the bottom of the screen. Below does not fit,
        // so the composition-only placement flips above the caret and lands
        // on the lines already typed; stepping past the box's top edge costs
        // a few dozen pixels and keeps them readable.
        let chat_box = ScreenRect {
            left: 100,
            top: 480,
            right: 700,
            bottom: 570,
        };
        let caret_in_chat_box = ScreenRect {
            left: 100,
            top: 510,
            right: 160,
            bottom: 540,
        };
        assert!(covers_composition(
            place(caret_in_chat_box, 200, 100, work, gap),
            chat_box
        ));
        let above_box = place_candidates(
            caret_in_chat_box,
            Some(chat_box),
            200,
            100,
            work,
            gap,
            detour,
        );
        assert_eq!(above_box.bottom, chat_box.top - gap);
        assert!(caret_distance(above_box, caret_in_chat_box) <= detour);

        // The same idea with room under the box instead of above it.
        let high_box = ScreenRect {
            left: 100,
            top: 100,
            right: 400,
            bottom: 230,
        };
        let caret_in_high_box = ScreenRect {
            left: 100,
            top: 105,
            right: 160,
            bottom: 135,
        };
        let below_box = place_candidates(
            caret_in_high_box,
            Some(high_box),
            200,
            100,
            work,
            gap,
            detour,
        );
        assert_eq!(below_box.top, high_box.bottom + gap);
        assert!(caret_distance(below_box, caret_in_high_box) <= detour);

        // A narrow box with no room above or below it. Beside it is still
        // within reach of the caret, so the popup goes there, level with it.
        let narrow_box = ScreenRect {
            left: 100,
            top: 50,
            right: 220,
            bottom: 550,
        };
        let caret_in_narrow_box = ScreenRect {
            left: 100,
            top: 60,
            right: 160,
            bottom: 90,
        };
        let beside = place_candidates(
            caret_in_narrow_box,
            Some(narrow_box),
            200,
            100,
            work,
            gap,
            detour,
        );
        assert_eq!(beside.left, narrow_box.right + gap);
        assert_eq!(beside.top, caret_in_narrow_box.top);
        assert!(caret_distance(beside, caret_in_narrow_box) <= detour);

        // The same box made wide: now every clear placement is far from the
        // caret, so the popup stays under it and accepts the overlap.
        let wide_box = ScreenRect {
            left: 100,
            top: 50,
            right: 800,
            bottom: 550,
        };
        let stays = place_candidates(
            caret_in_narrow_box,
            Some(wide_box),
            200,
            100,
            work,
            gap,
            detour,
        );
        assert_eq!(stays, place(caret_in_narrow_box, 200, 100, work, gap));
        assert_eq!(caret_distance(stays, caret_in_narrow_box), gap);
    }

    /// Fixed-seed sweep over caret, box, and work-area geometries. Whatever
    /// the shape, the popup is either the composition-only placement or a
    /// detour that is both within budget and clear of what it stepped off.
    #[test]
    fn placement_never_strays_further_from_the_caret_than_the_budget() {
        let work = RECT {
            left: -400,
            top: -200,
            right: 1_600,
            bottom: 1_000,
        };
        let gap = 4;
        let detour = 160;
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as i32
        };
        let mut detours = 0usize;
        for _ in 0..8_192 {
            let left = work.left + next() % 1_800;
            let top = work.top + next() % 1_100;
            let document = ScreenRect {
                left,
                top,
                right: left + 40 + next() % 1_200,
                bottom: top + 24 + next() % 900,
            };
            let caret_left = left + next() % (document.right - left).max(1);
            let caret_top = top + next() % (document.bottom - top).max(1);
            let anchor = ScreenRect {
                left: caret_left,
                top: caret_top,
                right: caret_left + 8 + next() % 200,
                bottom: caret_top + 16 + next() % 40,
            };
            let width = 200 + next() % 280;
            let height = 60 + next() % 400;

            let placed = place_candidates(anchor, Some(document), width, height, work, gap, detour);
            if placed == place(anchor, width, height, work, gap) {
                continue;
            }
            detours += 1;
            assert!(
                caret_distance(placed, anchor) <= detour,
                "detour {placed:?} strayed from {anchor:?}"
            );
            assert!(!covers_composition(placed, document));
            assert!(!covers_composition(placed, anchor));
            assert!(placed.left >= work.left && placed.right <= work.right);
            assert!(placed.top >= work.top && placed.bottom <= work.bottom);
        }
        assert!(
            detours > 0,
            "the sweep never took a detour, so it proved nothing about them"
        );
    }

    /// A document filling the work area — a full-window editor — has no
    /// placement that clears it. That must cost nothing: the popup keeps
    /// exactly the geometry it had before the editable area was known.
    #[test]
    fn a_document_with_no_clear_placement_keeps_the_previous_behaviour() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1_000,
            bottom: 600,
        };
        let anchor = ScreenRect {
            left: 100,
            top: 100,
            right: 160,
            bottom: 130,
        };
        let whole_screen = ScreenRect {
            left: 0,
            top: 0,
            right: 1_000,
            bottom: 600,
        };
        let expected = place(anchor, 200, 200, work, 4);
        assert_eq!(
            place_candidates(anchor, Some(whole_screen), 200, 200, work, 4, 160),
            expected,
        );
        // And a host that reports no editable area at all is the same case.
        assert_eq!(
            place_candidates(anchor, None, 200, 200, work, 4, 160),
            expected
        );
    }

    /// Every caret line on a 5px vertical grid, plus a ±5px GetTextExt wobble.
    /// Covering the composition is allowed only when the popup is taller than
    /// the free space on both sides. A nearby clear origin on the same grid
    /// means the cover was avoidable.
    #[test]
    fn five_px_vertical_sweep_keeps_the_composition_readable() {
        let list = candidates(
            (0..CANDIDATE_PAGE_SIZE)
                .map(|_| item("変換", "へんかん"))
                .collect(),
            3,
            CandidateKind::Conversion,
        );
        let mut avoidable_cover = Vec::new();
        let mut avoidable_cover_count = 0usize;
        let mut fragile_five_px = Vec::new();
        let mut fragile_five_px_count = 0usize;
        let mut covered_document = 0usize;
        let mut placements = 0usize;
        let mut unavoidable_cover = 0usize;

        for dpi in [96u32, 120, 144, 192] {
            let candidate_layout = layout(&list, dpi);
            let width = candidate_layout.width;
            let height = candidate_layout.height;
            let gap = candidate_layout.gap;
            let detour = candidate_layout.detour;
            let caret_h = scaled(24, dpi).max(16);
            let caret_w = scaled(64, dpi).max(24);
            let works = [
                RECT {
                    left: 0,
                    top: 0,
                    right: 1_920,
                    bottom: 1_040,
                },
                RECT {
                    left: 0,
                    top: 0,
                    right: 1_280,
                    bottom: 720,
                },
                RECT {
                    left: 0,
                    top: 0,
                    right: 1_366,
                    bottom: 728,
                },
            ];
            for work in works {
                let with_detail =
                    detail_layout("変換", &detail(), dpi, work.bottom.saturating_sub(work.top));
                let documents: [Option<ScreenRect>; 6] = [
                    None,
                    Some(screen(16, 24, 688, 60)),
                    Some(screen(208, 191, 1_092, 772)),
                    Some(screen(
                        308,
                        901.min(work.bottom - 40),
                        1_192,
                        work.bottom - 18,
                    )),
                    Some(screen(
                        400,
                        640.min(work.bottom - 80),
                        1_400,
                        work.bottom - 30,
                    )),
                    Some(screen(work.left, work.top, work.right, work.bottom)),
                ];
                for document in documents {
                    let (x_left, x_right, y_top, y_bottom) = match document {
                        Some(document) => {
                            (document.left, document.right, document.top, document.bottom)
                        }
                        None => (work.left + 40, work.right - 40, work.top, work.bottom),
                    };
                    let xs = [
                        x_left.saturating_add(8),
                        (x_left.saturating_add(x_right)) / 2,
                        x_right.saturating_sub(caret_w).saturating_sub(8),
                    ];
                    let mut caret_top = y_top;
                    let last_top = y_bottom.saturating_sub(caret_h).max(y_top);
                    while caret_top <= last_top {
                        for &caret_left in &xs {
                            let anchor = screen(
                                caret_left,
                                caret_top,
                                caret_left.saturating_add(caret_w),
                                caret_top.saturating_add(caret_h),
                            );
                            if !anchor.is_valid() {
                                continue;
                            }
                            placements += 1;
                            let placed = popup_placement(
                                anchor,
                                document,
                                candidate_layout,
                                Some(with_detail),
                                work,
                            );
                            let covers_text = covers_composition(placed.window, anchor);
                            if covers_text {
                                if composition_cover_is_unavoidable(anchor, height, work, gap) {
                                    unavoidable_cover += 1;
                                } else {
                                    avoidable_cover_count += 1;
                                    if avoidable_cover.len() < 24 {
                                        let oracle = clear_origin_on_five_px_grid(
                                            anchor, document, width, height, work, detour,
                                        );
                                        avoidable_cover.push(format!(
                                            "dpi={dpi} caret=({caret_left},{caret_top}) placed=[{},{} {}x{}] oracle={oracle:?} doc={document:?}",
                                            placed.window.left,
                                            placed.window.top,
                                            placed.window.right - placed.window.left,
                                            placed.window.bottom - placed.window.top,
                                        ));
                                    }
                                }
                            } else {
                                let wobble = inflate_y(anchor, 5);
                                if covers_composition(placed.window, wobble)
                                    && !composition_cover_is_unavoidable(wobble, height, work, gap)
                                {
                                    fragile_five_px_count += 1;
                                    if fragile_five_px.len() < 24 {
                                        fragile_five_px.push(format!(
                                            "dpi={dpi} caret=({caret_left},{caret_top}) placed=[{},{} {}x{}] wobble={wobble:?}",
                                            placed.window.left,
                                            placed.window.top,
                                            placed.window.right - placed.window.left,
                                            placed.window.bottom - placed.window.top,
                                        ));
                                    }
                                }
                            }
                            if covers_document(placed.window, document) {
                                covered_document += 1;
                            }
                        }
                        let next = caret_top.saturating_add(5);
                        if next <= caret_top {
                            break;
                        }
                        caret_top = next;
                    }
                }
            }
        }

        let summary = format!(
            "5px sweep: placements={placements} unavoidable_composition_cover={unavoidable_cover} document_cover={covered_document} avoidable={avoidable_cover_count} fragile_5px={fragile_five_px_count}"
        );
        println!("{summary}");
        if let Ok(path) = std::env::var("SAKURA_SWEEP_OUT") {
            std::fs::write(&path, summary.as_bytes()).expect("write sweep summary");
        }
        assert!(
            avoidable_cover.is_empty(),
            "popup covered the composition while below or above still fit ({avoidable_cover_count} hits):\n{}",
            avoidable_cover.join("\n")
        );
        assert!(
            fragile_five_px.is_empty(),
            "a ±5px GetTextExt wobble covered the composition ({fragile_five_px_count} hits):\n{}",
            fragile_five_px.join("\n")
        );
    }

    /// The HWND is one opaque rectangle around the list and the detail. A
    /// taller side pane that itself misses the caret can still stretch that
    /// rectangle down over the line being typed.
    #[test]
    fn taller_side_detail_must_not_stretch_the_window_over_the_composition() {
        let list = candidates(
            (0..CANDIDATE_PAGE_SIZE)
                .map(|_| item("変換", "へんかん"))
                .collect(),
            3,
            CandidateKind::Conversion,
        );
        let candidate_layout = layout(&list, 96);
        let work = RECT {
            left: 0,
            top: 0,
            right: 1_920,
            bottom: 1_040,
        };
        let detail_layout = detail_layout("変換", &detail(), 96, work.bottom - work.top);
        assert!(
            detail_layout.height > candidate_layout.height,
            "this case needs a definition taller than the list"
        );
        let anchor = screen(410, 980, 474, 1_004);
        // VS Code / Electron often reports the whole frame as GetScreenExt, so
        // every off-document detour is farther than the caret budget. Fallback
        // sits just above the caret; a taller detail must not then stretch the
        // opaque HWND back down over the line.
        let document = screen(work.left, work.top, work.right, work.bottom);
        let placed = popup_placement(
            anchor,
            Some(document),
            candidate_layout,
            Some(detail_layout),
            work,
        );
        assert!(
            !covers_composition(placed.window, anchor),
            "window L{} T{} R{} B{} covered composition {anchor:?}; detail={:?}",
            placed.window.left,
            placed.window.top,
            placed.window.right,
            placed.window.bottom,
            placed.layout.detail
        );
    }

    /// A candidate list that flipped above the composition leaves the
    /// bottom detail slot sitting exactly on the text being typed. The
    /// detail must go absent rather than cover it. A taller side pane is
    /// also omitted when the opaque HWND around both panes would stretch
    /// back down over the composition; a one-line caret with room beside
    /// the list still keeps a side pane that stays a full gap away.
    #[test]
    fn detail_below_is_omitted_rather_than_covering_the_composition() {
        let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
        let candidate_layout = layout(&list, 96);
        let detail_layout = detail_layout("候補", &detail(), 96, 148);
        assert_eq!(detail_layout.height, 148);
        // A composition wrapped over several lines, ending near the bottom
        // of a short work area, so the candidate list flips above it.
        let anchor = ScreenRect {
            left: 20,
            top: 200,
            right: 260,
            bottom: 340,
        };
        let narrow = RECT {
            left: 0,
            top: 0,
            right: 480,
            bottom: 390,
        };
        let placement =
            popup_placement(anchor, None, candidate_layout, Some(detail_layout), narrow);
        assert_eq!(placement.window.bottom, anchor.top - candidate_layout.gap);
        assert!(placement.layout.detail.is_none());

        // The same tall composition with room on the right still omits: the
        // side pane itself misses the text, but the HWND would not.
        let wide = RECT {
            left: 0,
            top: 0,
            right: 1_000,
            bottom: 390,
        };
        let control = popup_placement(anchor, None, candidate_layout, Some(detail_layout), wide);
        assert!(control.layout.detail.is_none());
        assert_eq!(control.window.bottom, anchor.top - candidate_layout.gap);

        let one_line = screen(20, 80, 80, 104);
        let with_side =
            popup_placement(one_line, None, candidate_layout, Some(detail_layout), wide);
        let side = with_side
            .layout
            .detail
            .expect("side detail stays a full gap from a one-line caret");
        assert_eq!(side.left, with_side.layout.candidates.right);
        assert!(!covers_composition(
            with_side.window,
            vertically_padded(one_line, candidate_layout.gap)
        ));
    }

    /// A tall detail pane beside the list slides up to fit the work area.
    /// When the composition is wide enough that the slide would put the pane
    /// on top of it, the pane goes absent instead.
    #[test]
    fn side_detail_that_would_slide_over_the_composition_is_omitted() {
        let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
        let candidate_layout = layout(&list, 96);
        let mut long = detail();
        long.definition = "全文を折り返して表示する長い日本語説明。".repeat(1_000);
        let detail_layout = detail_layout("候補", &long, 96, 340);
        let anchor = ScreenRect {
            left: 20,
            top: 300,
            right: 700,
            bottom: 324,
        };
        let work = RECT {
            left: 0,
            top: 0,
            right: 1_200,
            bottom: 400,
        };
        let placement = popup_placement(anchor, None, candidate_layout, Some(detail_layout), work);
        assert!(placement.layout.detail.is_none());
    }

    #[test]
    fn detail_wraps_all_available_lines_and_only_three_relation_words() {
        let value = "😀e\u{301}".repeat(128);
        let full = definition_lines(&value, 40, 96, usize::MAX, false);
        assert!(full.len() > 2);
        assert!(!full.last().expect("last full line").ends_with('…'));
        let bounded = definition_lines(&value, 40, 96, 3, false);
        assert_eq!(bounded.len(), 3);
        assert!(bounded[2].ends_with('…'));
        let source_bounded = definition_lines("complete preview", 400, 96, 8, true);
        assert!(source_bounded[0].ends_with('…'));
        assert_eq!(
            relation_text(&["a".into(), "b".into(), "c".into(), "d".into()]),
            "a・b・c"
        );
    }

    #[test]
    fn detail_width_is_constant_and_height_grows_then_caps_at_every_dpi() {
        for dpi in [96, 120, 144, 168, 192, 240] {
            let mut short = detail();
            short.definition = "短い説明。".to_owned();
            let mut long = detail();
            long.definition = "全文を折り返して表示する長い日本語説明。".repeat(1_000);
            let max_height = scaled(720, dpi);
            let short_layout = detail_layout("用語", &short, dpi, max_height);
            let long_layout = detail_layout("長さの異なる用語", &long, dpi, max_height);
            assert_eq!(short_layout.width, scaled(DETAIL_WIDTH_96, dpi));
            assert_eq!(long_layout.width, short_layout.width);
            assert!(long_layout.height > short_layout.height);
            assert!(long_layout.height <= max_height);
            assert!(max_height - long_layout.height < long_layout.line_height);
        }
    }

    #[test]
    fn wrapping_preserves_every_scalar_when_height_is_available() {
        for dpi in [96, 120, 144, 168, 192, 240] {
            for width in 1..=512 {
                let value = "日本語😀e\u{301}ABC・説明".repeat(5);
                let lines = definition_lines(&value, width, dpi, usize::MAX, false);
                assert_eq!(lines.concat(), value);
            }
        }
    }

    #[test]
    fn detail_placement_preserves_candidate_geometry_at_dpi_and_work_edges() {
        let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
        let detail = detail();
        let work = RECT {
            left: 0,
            top: 0,
            right: 2_500,
            bottom: 1_800,
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
                None,
                candidate_layout,
                Some(detail_layout("候補", &detail, dpi, work.bottom - work.top)),
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
        let detail_layout = detail_layout("候補", &detail(), 96, 700);

        let right = popup_placement(
            ScreenRect {
                left: 100,
                top: 80,
                right: 120,
                bottom: 104,
            },
            None,
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
            None,
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
            None,
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
            None,
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
