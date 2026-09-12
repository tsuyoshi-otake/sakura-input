//! Renderer-owned candidate popup.
//!
//! The popup follows the TSF caret without taking focus. Its layout is computed
//! from 96-DPI tokens, while painting uses a restrained Sakura palette unless
//! Windows high-contrast mode requests system colors instead.

use std::sync::mpsc::{SyncSender, TrySendError};

use sakura_proto::{
    AppearanceTheme, CandidateDetail, CandidateKind, CandidateList, ScreenRect, UiState,
    CANDIDATE_PAGE_SIZE,
};
use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CombineRgn, CreateRectRgn, DeleteObject, EndPaint, GetMonitorInfoW, InvalidateRect,
    MonitorFromRect, SelectObject, SetBkMode, SetWindowRgn, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, RGN_ERROR, RGN_OR, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
    GetWindowRect, RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, HTCLIENT, HWND_TOPMOST, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_HIDE, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_DESTROY, WM_DPICHANGED,
    WM_ERASEBKGND, WM_GETOBJECT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCHITTEST,
    WM_PAINT, WNDCLASSW, WS_DISABLED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

#[cfg(test)]
use windows::Win32::Graphics::Gdi::{COLOR_HIGHLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT};
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::WS_EX_TRANSPARENT;

#[cfg(test)]
use crate::theme::{drawable_utf16, high_contrast_palette, system_color};

use crate::accessibility::CandidateAccessibility;
use crate::theme::{
    fill_color, font, palette, scaled, select_font, text, Palette, BODY_FONT_96, GAP_96,
    PADDING_96, RAIL_WIDTH_96, SUPPORT_FONT_96,
};
use crate::watch::{CandidateCommitRequest, HistoryDeleteRequest};

const DISPLAY_CLASS: PCWSTR = windows::core::w!("SakuraInputCandidates");
const DELETE_OVERLAY_CLASS: PCWSTR = windows::core::w!("SakuraInputCandidateDeleteTargets");

// All measurements are logical pixels at 96 DPI.
pub(crate) const ROW_HEIGHT_96: i32 = 28;
const FOOTER_HEIGHT_96: i32 = 22;
/// How far the popup may be moved away from the caret to keep the host's
/// editable area readable. Roughly five candidate rows: past that the list
/// stops reading as attached to what is being typed, and a list that is hard
/// to find costs the user more than the covered text it bought.
const MAX_CARET_DETOUR_96: i32 = 160;
pub(crate) const NUMBER_WIDTH_96: i32 = 28;
const RAIL_MARGIN_96: i32 = 4;
const MIN_WIDTH_96: i32 = 260;
const MAX_WIDTH_96: i32 = 480;
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
    commit_candidate: SyncSender<CandidateCommitRequest>,
    pending_history_deletes: Vec<HistoryDeleteRequest>,
    pending_candidate_commits: Vec<CandidateCommitRequest>,
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
    pub fn new(
        delete_history: SyncSender<HistoryDeleteRequest>,
        commit_candidate: SyncSender<CandidateCommitRequest>,
    ) -> Result<Self> {
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
            commit_candidate,
            pending_history_deletes: Vec::new(),
            pending_candidate_commits: Vec::new(),
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
        clear_pending_candidate_commits_for_new_revision(
            &mut self.state.pending_candidate_commits,
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
        let row_targets = candidate_row_targets(candidates, placed.layout, candidate_layout);
        let overlay_ready = rebuild_candidate_overlay_region(self.delete_overlay, &row_targets);
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
                &row_targets,
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

    pub fn candidate_commit_finished(&mut self, request: CandidateCommitRequest, queued: bool) {
        finish_pending_candidate_commit(&mut self.state.pending_candidate_commits, request, queued);
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
            queue_candidate_interaction(window, point_from_lparam(l));
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
    let targets = candidate_row_targets(candidates, popup, layout);
    state.layout = Some(popup);
    state.candidate_layout = Some(layout);
    let ready = rebuild_candidate_overlay_region(state.delete_overlay, &targets);
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

fn candidate_row_targets(
    candidates: &CandidateList,
    popup: PopupLayout,
    layout: Layout,
) -> Vec<RECT> {
    candidates
        .visible_range()
        .enumerate()
        .map(|(row_index, _)| RECT {
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
        })
        .collect()
}

fn candidate_commit_at_client_point(
    state: &PaintState,
    point: POINT,
) -> Option<CandidateCommitRequest> {
    let candidates = state.candidates.as_ref()?;
    let popup = state.layout?;
    let layout = state.candidate_layout?;
    candidate_commit_request_at_client_point(candidates, popup, layout, state.revision, point)
}

fn candidate_commit_request_at_client_point(
    candidates: &CandidateList,
    popup: PopupLayout,
    layout: Layout,
    revision: u64,
    point: POINT,
) -> Option<CandidateCommitRequest> {
    candidate_row_targets(candidates, popup, layout)
        .into_iter()
        .zip(candidates.visible_range())
        .find(|(row, _)| point_in_rect(point, *row))
        .and_then(|(_, index)| u16::try_from(index).ok())
        .map(|candidate_index| CandidateCommitRequest {
            revision,
            candidate_index,
        })
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
fn rebuild_candidate_overlay_region(window: HWND, targets: &[RECT]) -> bool {
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
            let component = CreateRectRgn(target.left, target.top, target.right, target.bottom);
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
    targets: &[RECT],
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

fn clear_pending_candidate_commits_for_new_revision(
    pending: &mut Vec<CandidateCommitRequest>,
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

fn finish_pending_candidate_commit(
    pending: &mut Vec<CandidateCommitRequest>,
    request: CandidateCommitRequest,
    queued: bool,
) {
    if !queued {
        pending.retain(|pending_request| *pending_request != request);
    }
}

fn queue_candidate_interaction(window: HWND, point: POINT) {
    // SAFETY: CandidateWindow owns this stable box until the HWND is destroyed.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut PaintState;
    if state.is_null() {
        return;
    }
    // SAFETY: overlay messages are serialized on its owning renderer thread.
    let state = unsafe { &mut *state };
    if let Some(request) = history_delete_at_client_point(state, point) {
        if is_history_delete_pending(&state.pending_history_deletes, request) {
            return;
        }
        if state.delete_history.try_send(request).is_ok() {
            state.pending_history_deletes.push(request);
        }
        return;
    }
    let Some(request) = candidate_commit_at_client_point(state, point) else {
        return;
    };
    if !state.pending_candidate_commits.is_empty() {
        return;
    }
    match state.commit_candidate.try_send(request) {
        Ok(()) => state.pending_candidate_commits.push(request),
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
            let mut client = RECT::default();
            // SAFETY: the overlay window and output rectangle are live.
            if unsafe { GetClientRect(window, &mut client) }.is_ok() {
                draw(
                    dc,
                    client,
                    candidates,
                    state.detail.as_ref(),
                    popup,
                    dpi(window),
                    palette(state.appearance_theme),
                );
            }
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

fn number_label(index: usize) -> &'static str {
    const LABELS: [&str; CANDIDATE_PAGE_SIZE] =
        ["1.", "2.", "3.", "4.", "5.", "6.", "7.", "8.", "9."];
    LABELS.get(index).copied().unwrap_or("")
}

fn page_summary(start: usize, end: usize, total: usize) -> String {
    format!("{}–{} / {total}", start.saturating_add(1), end)
}

#[cfg(test)]
#[path = "candidate_tests.rs"]
mod tests;
