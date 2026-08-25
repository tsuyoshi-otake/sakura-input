//! The singleton Sakura Pad window.
//!
//! A normal, activatable top-level HWND owned by the renderer UI thread. The
//! hidden renderer host receives the shortcut and posts a deferred application
//! message; this window owns the memo list, the editor, and the storage
//! worker, and nothing else.
//!
//! Two shapes, one window. Below 520 logical pixels of client width the pad
//! shows the list *or* the editor and `≡` moves between them; at or above it
//! the list stays resident beside the editor. The whole arrangement is
//! [`layout`], a pure function of the client rectangle, the DPI and which pane
//! is showing, so the breakpoint and every control position are testable
//! without a window.
//!
//! No color is decided here. Every surface, every rule and every glyph
//! resolves through [`crate::theme`], which the candidate popup also uses, so
//! the two Sakura-owned windows cannot drift into two products.

use std::cell::Cell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicIsize, Ordering};

use sakura_proto::AppearanceTheme;
use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, CreatePatternBrush, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, GetDC, InvalidateRect, RedrawWindow,
    ReleaseDC, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, DT_END_ELLIPSIS,
    DT_LEFT, DT_RIGHT, HBRUSH, HDC, HFONT, OPAQUE, PAINTSTRUCT, PS_SOLID, RDW_ALLCHILDREN,
    RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW, TRANSPARENT,
};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_GETFIRSTVISIBLELINE, EM_GETLINECOUNT, EM_GETMARGINS, EM_SETLIMITTEXT,
    MEASUREITEMSTRUCT, ODS_FOCUS, ODS_SELECTED, ODT_BUTTON, ODT_LISTBOX,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, GetKeyState, SetFocus, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, CallWindowProcW, CreateWindowExW, DefWindowProcW, DeferWindowPos,
    DestroyWindow, EndDeferWindowPos, FlashWindowEx, GetAncestor, GetClassNameW, GetClientRect,
    GetParent, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW, IsIconic,
    IsWindowVisible, KillTimer, LoadCursorW, PostMessageW, RegisterClassW, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    BN_CLICKED, BS_OWNERDRAW, CREATESTRUCTW, EN_CHANGE, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_LEFT,
    ES_MULTILINE, ES_NOHIDESEL, ES_WANTRETURN, FLASHWINFO, GA_ROOT, GWLP_USERDATA, GWLP_WNDPROC,
    HMENU, HWND_TOP, IDC_ARROW, LBN_DBLCLK, LBN_SELCHANGE, LBS_HASSTRINGS, LBS_NOINTEGRALHEIGHT,
    LBS_NOTIFY, LBS_OWNERDRAWFIXED, LB_ADDSTRING, LB_RESETCONTENT, LB_SETCURSEL, LB_SETITEMHEIGHT,
    MSG, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE,
    SW_RESTORE, SW_SHOW, WINDOW_EX_STYLE, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM,
    WM_ERASEBKGND, WM_GETFONT, WM_GETMINMAXINFO, WM_GETTEXTLENGTH, WM_KEYDOWN, WM_KILLFOCUS,
    WM_MEASUREITEM, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETFOCUS, WM_SETFONT, WM_SETTEXT,
    WM_SETTINGCHANGE, WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_THEMECHANGED, WM_TIMER, WNDCLASSW,
    WNDPROC, WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};

use crate::pad_caption::{self, CaptionIcons};
use crate::pad_icon::{self, PadIcon};
use crate::pad_list::{self, CalendarTime};
use crate::pad_rail;
use crate::pad_storage::{
    now_ms, PadDocument, PadMemo, PadStore, SaveStatus, StorageError, StorageWorker,
    MAX_BODY_UTF16_UNITS, MAX_MEMOS, MAX_TITLE_UTF16_UNITS, SHUTDOWN_FLUSH_BUDGET,
};
use crate::pad_tooltip::Tooltips;
use crate::theme::{
    fill_color, font, font_weighted, palette, scaled, select_font, text, text_width, Palette,
    BODY_FONT_96, GAP_96, PADDING_96, SUPPORT_FONT_96,
};

pub const PAD_CLASS: PCWSTR = windows::core::w!("SakuraInputPad");
pub const PAD_COMPLETION_TIMER: usize = 0x5341;
pub const PAD_EDIT_TIMER: usize = 0x5343;
/// When a notice hands the status row back to the memo.
pub const PAD_NOTICE_TIMER: usize = 0x5344;
/// How long a notice stays, in milliseconds. Long enough to read a sentence
/// that was not asked for, short enough that it does not become the row.
const NOTICE_MS: u32 = 4_000;
pub const PAD_MIN_WIDTH_LOGICAL: i32 = 480;
pub const PAD_MIN_HEIGHT_LOGICAL: i32 = 360;
pub const PAD_WIDTH_LOGICAL: i32 = 640;
pub const PAD_HEIGHT_LOGICAL: i32 = 520;

/// Where the single-pane shape becomes the two-pane shape, in logical pixels
/// of client width.
///
/// 520 is not a device: it is where a 240-wide list stops being a column and
/// starts being most of the window. Below it the editor would be narrower
/// than the list beside it, which is the wrong thing to be reading.
pub(crate) const BREAKPOINT_96: i32 = 520;

/// The band above the panes, which only the folded shape has: the two-pane
/// shape reaches every memo from the list beside it, so the window's own
/// caption is the only chrome it needs above the panes.
const HEADER_HEIGHT_96: i32 = 40;
/// The editor's own first row: the memo's title, when it last synced, how
/// long it is, and the two controls that act on it.
const EDITOR_HEAD_HEIGHT_96: i32 = 44;
const BOTTOM_BAR_HEIGHT_96: i32 = 48;
const LIST_WIDTH_96: i32 = 240;
const SEARCH_HEIGHT_96: i32 = 30;
const GLYPH_BUTTON_96: i32 = 32;
/// `＋ 新規メモ` carries its own name, so it is the one control in the bar
/// that is read rather than recognised.
const NEW_BUTTON_MIN_96: i32 = 92;
/// Enough for `1024字` at the support size.
const COUNT_WIDTH_96: i32 = 52;
/// Enough for `10:27 同期済` at the support size. This is the resting width:
/// a notice is measured and given more, up to what the title can spare.
const STATUS_WIDTH_96: i32 = 108;
/// `SS_ENDELLIPSIS` cuts a reading that exactly fills its rectangle, so a
/// measured status asks for a hair more than it measured.
const STATUS_SLACK_96: i32 = 6;
/// How tall one line of the body font is at 96 dpi.
///
/// The row a heading shares with its supports is taller than the text in it,
/// and the difference is breathing room rather than something to fill.
const TEXT_LINE_96: i32 = 22;
/// A title narrower than this is a field, not a heading.
const TITLE_MIN_96: i32 = 120;
const BORDER_96: i32 = 1;
/// The ruled squares behind the writing area, at 96 DPI. The same 24 the
/// design draws, which is also the body font's line height, so a written line
/// sits on a rule rather than across one.
const GRID_96: i32 = 24;
/// A field's contents sit inside its drawn frame rather than on it.
const FIELD_INSET_96: i32 = 4;
/// How round a pressable corner is, at 96 DPI.
///
/// The pad's bands are rectangles because they are architecture; the things
/// inside them that can be pressed are softened, which is the whole visual
/// difference between a surface and a control here.
const CORNER_96: i32 = 6;
/// Enough for `12/31` at the support size; the row title yields it.
const ROW_TIME_WIDTH_96: i32 = 56;

const MENU_ID: u16 = 101;
const COUNT_ID: u16 = 102;
const SEARCH_ID: u16 = 104;
const LIST_ID: u16 = 105;
const STATUS_ID: u16 = 106;
const HEADER_TITLE_ID: u16 = 107;
const TITLE_ID: u16 = 108;
const BODY_ID: u16 = 109;
const NEW_ID: u16 = 110;
const SORT_ID: u16 = 111;
const SYNC_ID: u16 = 112;
const COPY_ID: u16 = 113;
const DELETE_ID: u16 = 114;
const LIST_RAIL_ID: u16 = 115;
const BODY_RAIL_ID: u16 = 116;

/// `SS_CENTERIMAGE | SS_ENDELLIPSIS`. The windows crate exposes the static
/// styles from `Win32_System_SystemServices`, a feature nothing else in this
/// binary needs; the values are fixed by the Win32 ABI.
const STATIC_CENTERED_ELLIPSIS: i32 = 0x0200 | 0x4000;
/// `SS_RIGHT`. A length reads against the edge it is measured to.
const STATIC_RIGHT: i32 = 0x0002;

/// A search box holds a phrase, not a document.
const MAX_QUERY_UTF16_UNITS: usize = 128;

/// Which pane the narrow shape is showing. The wide shape shows both and
/// carries this only so returning to narrow lands where the user left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PadPane {
    #[default]
    List,
    Editor,
}

/// Every rectangle the pad arranges, in physical pixels.
///
/// `None` means the control is not part of this shape and is hidden rather
/// than moved offscreen, so a hidden control cannot be tabbed into or read by
/// a screen reader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PadLayout {
    pub(crate) wide: bool,
    pub(crate) header: Option<RECT>,
    pub(crate) menu: Option<RECT>,
    pub(crate) header_title: Option<RECT>,
    pub(crate) meta: Option<RECT>,
    pub(crate) title: Option<RECT>,
    pub(crate) status: Option<RECT>,
    pub(crate) count: Option<RECT>,
    pub(crate) copy: RECT,
    pub(crate) delete: RECT,
    pub(crate) search: Option<RECT>,
    pub(crate) list: Option<RECT>,
    /// The strip beside the list holding its own scroll rail.
    pub(crate) list_rail: Option<RECT>,
    pub(crate) divider: Option<RECT>,
    pub(crate) body: Option<RECT>,
    /// The strip beside the writing area holding its own scroll rail.
    pub(crate) body_rail: Option<RECT>,
    /// The whole writing surface, the editor's own head row included, which is
    /// painted as paper rather than as more of the window's chrome.
    pub(crate) paper: Option<RECT>,
    pub(crate) bottom: RECT,
    pub(crate) new: RECT,
    pub(crate) sort: RECT,
    pub(crate) sync: RECT,
}

/// Clamps a rectangle inside `bounds`, keeping its edges ordered.
///
/// Every rectangle below goes through this, which is what makes the function
/// total: a minimised window, a 1×1 client, or a DPI that scales a band past
/// the client all produce empty rectangles rather than inverted ones.
fn within(rect: RECT, bounds: RECT) -> RECT {
    let left = rect.left.clamp(bounds.left, bounds.right);
    let right = rect.right.clamp(left, bounds.right);
    let top = rect.top.clamp(bounds.top, bounds.bottom);
    let bottom = rect.bottom.clamp(top, bounds.bottom);
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

fn is_empty(rect: RECT) -> bool {
    rect.right <= rect.left || rect.bottom <= rect.top
}

/// The whole arrangement, from the client rectangle alone.
///
/// `status_want` of zero is a row with nothing to report, and it takes no
/// room at all: the memo's name has the width instead.
///
/// `status_want` is how wide the status reading is asking to be, in device
/// pixels; zero asks for the resting width. The slot is never given so much
/// that the title falls below its own minimum.
pub(crate) fn layout(client: RECT, dpi: u32, pane: PadPane, status_want: i32) -> PadLayout {
    let dpi = dpi.max(96);
    let pad = scaled(PADDING_96, dpi);
    let gap = scaled(GAP_96, dpi);
    let glyph = scaled(GLYPH_BUTTON_96, dpi);
    let header_height = scaled(HEADER_HEIGHT_96, dpi);
    let head_height = scaled(EDITOR_HEAD_HEIGHT_96, dpi);
    let bar_height = scaled(BOTTOM_BAR_HEIGHT_96, dpi);
    let search_height = scaled(SEARCH_HEIGHT_96, dpi);
    let border = scaled(BORDER_96, dpi).max(1);

    let client = RECT {
        left: client.left,
        top: client.top,
        right: client.right.max(client.left),
        bottom: client.bottom.max(client.top),
    };
    let (left, top, right, bottom) = (client.left, client.top, client.right, client.bottom);
    let wide = right.saturating_sub(left) >= scaled(BREAKPOINT_96, dpi);
    let shows_list = wide || pane == PadPane::List;
    let shows_editor = wide || pane == PadPane::Editor;

    // Only the folded shape has a band above the panes. Wide, the list is
    // already on screen and the memo's title belongs to the editor beside it,
    // so a second title band would name the same thing twice.
    let header = (!wide).then(|| {
        within(
            RECT {
                left,
                top,
                right,
                bottom: top.saturating_add(header_height),
            },
            client,
        )
    });
    let content = within(
        RECT {
            left,
            top: header.map_or(top, |header| header.bottom),
            right,
            bottom,
        },
        client,
    );

    // The columns, the pane regions inside them, and the bar.
    let (list_column, divider, editor_column, bar) = if wide {
        let column_right = left.saturating_add(scaled(LIST_WIDTH_96, dpi));
        let list_column = within(
            RECT {
                right: column_right,
                ..content
            },
            content,
        );
        let divider = within(
            RECT {
                left: list_column.right,
                right: list_column.right.saturating_add(border),
                ..content
            },
            content,
        );
        let editor_column = within(
            RECT {
                left: divider.right,
                ..content
            },
            content,
        );
        // The bar belongs to the list in the two-pane shape: it acts on the
        // list, and an editor that runs to the window's edge is more page.
        let bar = within(
            RECT {
                top: bottom.saturating_sub(bar_height),
                ..list_column
            },
            list_column,
        );
        (list_column, Some(divider), editor_column, bar)
    } else {
        let bar = within(
            RECT {
                top: bottom.saturating_sub(bar_height),
                ..content
            },
            content,
        );
        (content, None, content, bar)
    };

    // The editor's first row, which the folded shape does not have: there the
    // header band carries the title and the sync state, and the bar carries
    // the two controls that act on the memo.
    let meta = wide.then(|| {
        within(
            RECT {
                bottom: editor_column.top.saturating_add(head_height),
                ..editor_column
            },
            editor_column,
        )
    });

    // The editor is paper: the one region of the window holding the user's
    // own words rather than the program's. The head row above it is the
    // program describing the memo — a name, a reading, two controls — so it
    // stands on the window's chrome, which is where the folded shape puts the
    // same readings. One row, one ground, either side of the breakpoint.
    let paper = shows_editor.then(|| {
        within(
            RECT {
                top: meta.map_or(editor_column.top, |meta| meta.bottom),
                bottom: if wide { editor_column.bottom } else { bar.top },
                ..editor_column
            },
            editor_column,
        )
    });

    let centred = |band: RECT, height: i32| {
        band.top
            .saturating_add(((band.bottom - band.top - height) / 2).max(0))
    };
    let bar_glyph_top = centred(bar, glyph);
    let bar_glyph = |left_edge: i32, width: i32| RECT {
        left: left_edge,
        top: bar_glyph_top,
        right: left_edge.saturating_add(width),
        bottom: bar_glyph_top.saturating_add(glyph),
    };
    let menu = header.map(|header| {
        let glyph_top = centred(header, glyph);
        within(
            RECT {
                left: header.left.saturating_add(pad),
                top: glyph_top,
                right: header.left.saturating_add(pad).saturating_add(glyph),
                bottom: glyph_top.saturating_add(glyph),
            },
            header,
        )
    });

    // Copying and deleting act on the memo, so they sit with the memo: in the
    // editor's own first row when there is an editor beside the list, and in
    // the bar when the bar is the only place the whole window has for them.
    let (copy, delete, title, status, count, new, sort, sync) = if let Some(meta) = meta {
        let meta_glyph_top = centred(meta, glyph);
        let meta_glyph = |edge: i32| {
            within(
                RECT {
                    left: edge.saturating_sub(glyph),
                    top: meta_glyph_top,
                    right: edge,
                    bottom: meta_glyph_top.saturating_add(glyph),
                },
                meta,
            )
        };
        let delete = meta_glyph(meta.right.saturating_sub(pad));
        let copy = meta_glyph(delete.left.saturating_sub(gap));
        // The title is the heading of the row and the two readings beside it
        // are supports. A narrow editor column drops the supports — the length
        // before the time, because when a memo last changed is the more useful
        // of the two — rather than squeezing the heading into a stub.
        let text_left = meta.left.saturating_add(pad);
        let right_edge = copy.left.saturating_sub(gap);
        let title_min = scaled(TITLE_MIN_96, dpi);
        let status_width = scaled(STATUS_WIDTH_96, dpi);
        let count_width = scaled(COUNT_WIDTH_96, dpi);
        let available = right_edge.saturating_sub(text_left);
        // A static centres its one line inside whatever rectangle it is
        // given; an `EDIT` draws its line at the top of one. Handing the row
        // out whole therefore left the memo's own title riding above the two
        // readings beside it, so the rectangle here is the line, centred.
        let line = scaled(TEXT_LINE_96, dpi);
        let line_top = centred(
            RECT {
                bottom: meta.bottom.saturating_sub(border),
                ..meta
            },
            line,
        );
        let row = |left: i32, right: i32| {
            within(
                RECT {
                    left,
                    right,
                    top: line_top,
                    bottom: line_top.saturating_add(line),
                },
                meta,
            )
        };
        // Whether the length shows is decided against the resting status
        // width: a notice takes its room from the gap the title is not using,
        // not from the count the writer is watching.
        let count = (available >= title_min + gap + status_width + gap + count_width)
            .then(|| row(right_edge.saturating_sub(count_width), right_edge));
        let status_right = count.map_or(right_edge, |count| count.left.saturating_sub(gap));
        let status_room = status_right
            .saturating_sub(text_left)
            .saturating_sub(title_min + gap);
        let status = (status_want > 0 && available >= title_min + gap + status_width).then(|| {
            let width = status_want.clamp(status_width, status_room.max(status_width));
            row(status_right.saturating_sub(width), status_right)
        });
        let title = row(
            text_left,
            status
                .map_or(status_right, |status| status.left.saturating_sub(gap))
                .max(text_left),
        );

        // Wide, the bar is the list's own and `＋ 新規メモ` says what it makes.
        let sync = within(bar_glyph(bar.right.saturating_sub(pad + glyph), glyph), bar);
        let sort = within(bar_glyph(sync.left.saturating_sub(gap + glyph), glyph), bar);
        let new_left = bar.left.saturating_add(pad);
        let new = within(
            bar_glyph(
                new_left,
                sort.left
                    .saturating_sub(gap)
                    .saturating_sub(new_left)
                    .max(scaled(NEW_BUTTON_MIN_96, dpi)),
            ),
            bar,
        );
        (copy, delete, Some(title), status, count, new, sort, sync)
    } else {
        let band = header.unwrap_or(content);
        let text_left = menu.map_or(band.left.saturating_add(pad), |menu| {
            menu.right.saturating_add(gap)
        });
        let text_right = band.right.saturating_sub(pad);
        let status_width = scaled(STATUS_WIDTH_96, dpi);
        let title_min = scaled(TITLE_MIN_96, dpi);
        let status_room = text_right
            .saturating_sub(text_left)
            .saturating_sub(title_min + gap);
        // The band's own centre line, as in the two-pane row: the heading is
        // the same `EDIT` either side of the breakpoint, and an `EDIT` given a
        // whole band draws its one line along the top of it.
        let line = scaled(TEXT_LINE_96, dpi);
        let line_top = centred(
            RECT {
                bottom: band.bottom.saturating_sub(border),
                ..band
            },
            line,
        );
        // As in the two-pane row, the heading keeps its minimum and the
        // reading beside it yields — and, when the reading is a notice rather
        // than a time, it is the heading's spare gap that yields instead.
        let status = (status_want > 0
            && shows_editor
            && text_right.saturating_sub(text_left) >= title_min + gap + status_width)
            .then(|| {
                let width = status_want.clamp(status_width, status_room.max(status_width));
                within(
                    RECT {
                        left: text_right.saturating_sub(width),
                        right: text_right,
                        top: line_top,
                        bottom: line_top.saturating_add(line),
                    },
                    band,
                )
            });
        let title = shows_editor.then(|| {
            within(
                RECT {
                    left: text_left,
                    right: status
                        .map_or(text_right, |status| status.left.saturating_sub(gap))
                        .max(text_left),
                    top: line_top,
                    bottom: line_top.saturating_add(line),
                },
                band,
            )
        });

        // Folded, the bar carries everything: new at the near edge, delete at
        // the far one, and the three that neither create nor destroy between.
        let new = within(bar_glyph(bar.left.saturating_add(pad), glyph), bar);
        let delete = within(bar_glyph(bar.right.saturating_sub(pad + glyph), glyph), bar);
        let group = glyph * 3 + gap * 2;
        let group_left = (bar.left + (bar.right - bar.left - group) / 2)
            .min(delete.left.saturating_sub(gap).saturating_sub(group))
            .max(new.right.saturating_add(gap));
        let sort = within(bar_glyph(group_left, glyph), bar);
        let sync = within(bar_glyph(sort.right.saturating_add(gap), glyph), bar);
        let copy = within(bar_glyph(sync.right.saturating_add(gap), glyph), bar);
        (copy, delete, title, status, None, new, sort, sync)
    };

    // The band wears its own name only while the list is the pane on screen;
    // with the editor showing, the memo's title takes the row.
    let header_title = match (header, menu) {
        (Some(header), Some(menu)) if !shows_editor => Some(within(
            RECT {
                left: menu.right.saturating_add(gap),
                right: header.right.saturating_sub(pad),
                top: header.top,
                bottom: header.bottom.saturating_sub(border),
            },
            header,
        )),
        _ => None,
    };

    // What is left over once the bands have taken their room.
    let list_region = within(
        RECT {
            bottom: bar.top,
            ..list_column
        },
        list_column,
    );
    let editor_region = within(
        RECT {
            top: meta.map_or(editor_column.top, |meta| meta.bottom),
            bottom: if wide { editor_column.bottom } else { bar.top },
            ..editor_column
        },
        editor_column,
    );

    let search = shows_list.then(|| {
        within(
            RECT {
                left: list_region.left.saturating_add(pad),
                top: list_region.top.saturating_add(pad),
                right: list_region.right.saturating_sub(pad),
                bottom: list_region
                    .top
                    .saturating_add(pad)
                    .saturating_add(search_height),
            },
            list_region,
        )
    });
    // Rows run to the column's edges, like the candidate popup's: the
    // selection is a band across the list, not a chip floating inside it.
    let list = search.map(|search| {
        within(
            RECT {
                top: search.bottom.saturating_add(pad),
                ..list_region
            },
            list_region,
        )
    });

    let body = shows_editor.then(|| {
        within(
            RECT {
                left: editor_region.left.saturating_add(pad),
                top: editor_region.top.saturating_add(gap),
                right: editor_region.right.saturating_sub(pad),
                bottom: editor_region.bottom.saturating_sub(pad),
            },
            editor_region,
        )
    });

    // Each pane keeps a strip of its own right-hand edge for the rail that
    // reads it, taken from the pane rather than added beside it: a rail that
    // appeared and vanished with the length of the document would move the
    // words every time a memo was added.
    let rail_width = scaled(pad_rail::SCROLL_RAIL_96, dpi);
    let carve = |pane: RECT| {
        let split = pane.right.saturating_sub(rail_width).max(pane.left);
        (
            RECT {
                right: split,
                ..pane
            },
            RECT {
                left: split,
                ..pane
            },
        )
    };
    let (list, list_rail) = match list.map(carve) {
        Some((pane, rail)) => (Some(pane), Some(rail)),
        None => (None, None),
    };
    let (body, body_rail) = match body.map(carve) {
        Some((pane, rail)) => (Some(pane), Some(rail)),
        None => (None, None),
    };

    PadLayout {
        wide,
        header,
        menu,
        header_title,
        meta,
        title,
        status,
        count,
        copy,
        delete,
        search,
        list,
        list_rail,
        divider: divider.filter(|_| wide),
        body,
        body_rail,
        paper,
        bottom: bar,
        new,
        sort,
        sync,
    }
}

/// Where a field's control sits inside the chip the pad draws for it.
///
/// The magnifier is painted on the chip rather than put in the control, so
/// the control starts after it: an EDIT has no room for a picture, and a
/// picture typed into its text would be text the filter then searched for.
fn field_child(frame: RECT, dpi: u32) -> RECT {
    let inset = scaled(FIELD_INSET_96, dpi).max(1);
    let lead = scaled(PADDING_96, dpi) + pad_icon::size(dpi) + scaled(GAP_96, dpi) / 2;
    let right = frame.right.saturating_sub(inset);
    RECT {
        left: frame.left.saturating_add(lead).min(right),
        top: frame.top.saturating_add(inset),
        right,
        bottom: frame.bottom.saturating_sub(inset),
    }
}

/// The fonts a repaint needs, rebuilt only when the DPI changes.
///
/// Rows are drawn once per row per repaint; creating a font inside that loop
/// is how a list of two hundred memos becomes a visible stutter.
#[derive(Debug)]
struct PadFonts {
    dpi: u32,
    small: HFONT,
    body: HFONT,
    heading: HFONT,
}

impl PadFonts {
    fn new(dpi: u32) -> Self {
        let dpi = dpi.max(96);
        Self {
            dpi,
            small: font(scaled(SUPPORT_FONT_96, dpi)),
            body: font(scaled(BODY_FONT_96, dpi)),
            // Semibold, not larger: the heading is the same size as the memo
            // text it introduces, and weight is enough to rank it.
            heading: font_weighted(scaled(BODY_FONT_96, dpi), 600),
        }
    }

    fn destroy(&mut self) {
        for handle in [self.small, self.body, self.heading] {
            if !handle.is_invalid() {
                // SAFETY: these fonts are owned here and are never the
                // selected object once every control has been re-fonted or
                // the window destroyed.
                unsafe {
                    let _ = DeleteObject(handle.into());
                }
            }
        }
        self.small = HFONT::default();
        self.body = HFONT::default();
        self.heading = HFONT::default();
    }
}

/// The controls are native, so they are also native UI Automation providers.
/// Stable ids and labels make the pad readable to a screen reader without a
/// custom provider, and the top-level class and title identify the singleton
/// window to test clients.
#[derive(Debug)]
struct PadState {
    menu: HWND,
    header_title: HWND,
    status: HWND,
    count: HWND,
    search: HWND,
    list: HWND,
    list_rail: HWND,
    title: HWND,
    body: HWND,
    body_rail: HWND,
    new: HWND,
    sort: HWND,
    sync: HWND,
    copy: HWND,
    delete: HWND,
    fonts: PadFonts,
    brushes: PadBrushes,
    /// The icons the title bar is showing. `WM_SETICON` borrows rather than
    /// takes, so they are owned here for as long as the window is.
    caption: Option<CaptionIcons>,
    /// The hover text for the drawn faces. Optional because losing it costs
    /// the pad an explanation and nothing else.
    tooltips: Option<Tooltips>,
    worker: StorageWorker,
    /// The full persisted document. Every save carries all of it.
    document: PadDocument,
    /// Which memo the editor is bound to. It need not be in `document`: an
    /// untouched new memo is not persisted.
    active: u64,
    /// The memo ids the list is showing, in the order it is showing them.
    rows: Vec<u64>,
    query: String,
    pane: PadPane,
    generation: u64,
    latest_submitted: u64,
    updating_controls: bool,
    theme: AppearanceTheme,
    /// The pad's own window. A notice needs it to set its own expiry, and a
    /// changed reading needs it to ask for the row to be measured again.
    window: HWND,
    status_message: String,
    /// Whether `status_message` is a notice about something that happened,
    /// rather than a state the memo is currently in. A state is replaced by
    /// its successor; a notice has none, so it is given an expiry.
    status_notice: bool,
    /// The width the status slot was last arranged for. The reading is
    /// rewritten on every keystroke, and re-placing thirteen controls each
    /// time would make the row twitch, so the arrangement is redone only when
    /// what the reading asks for has actually changed.
    status_slot: Cell<i32>,
    /// Existing unreadable data is preserved until the user repairs or
    /// removes it explicitly. Starting with an empty UI must not overwrite it.
    save_blocked: bool,
}

/// Owns exactly one Pad HWND and its state. The object must stay on the
/// renderer's message-pump thread; child controls and the worker mailbox are
/// never touched from another thread.
#[derive(Debug)]
pub struct PadWindow {
    hwnd: HWND,
    state: Box<PadState>,
}

impl PadWindow {
    /// Opens the pad, owned by `owner`.
    ///
    /// The owner is the renderer's hidden host window, and it is what keeps
    /// the pad off the taskbar: a top-level window earns a taskbar button by
    /// being unowned, or by asking for one with `WS_EX_APPWINDOW`. The pad
    /// used to do both. It is summoned by a gesture and dismissed with a key,
    /// so a button that outlives neither belongs down there next to the
    /// programs the owner actually started. `WS_EX_TOOLWINDOW` would also
    /// have removed it, at the price of the small tool caption; an owner
    /// leaves the window itself untouched.
    pub fn new(owner: HWND) -> Result<Self> {
        register_class();
        let store = PadStore::default().map_err(storage_error)?;
        let (document, recovered_from_backup, load_status, save_blocked) = match store.load() {
            Ok(loaded) => (loaded.document, loaded.recovered_from_backup, None, false),
            Err(error) => (
                PadDocument::default(),
                false,
                Some(format!(
                    "メモを復元できません。既存データは保護されています ({error})"
                )),
                true,
            ),
        };
        let active = document
            .live()
            .next()
            .map(|memo| memo.id)
            .unwrap_or_else(|| document.next_id());
        let generation = document.generation;
        let mut state = Box::new(PadState {
            menu: HWND::default(),
            header_title: HWND::default(),
            status: HWND::default(),
            count: HWND::default(),
            search: HWND::default(),
            list: HWND::default(),
            list_rail: HWND::default(),
            title: HWND::default(),
            body: HWND::default(),
            body_rail: HWND::default(),
            new: HWND::default(),
            sort: HWND::default(),
            sync: HWND::default(),
            copy: HWND::default(),
            delete: HWND::default(),
            fonts: PadFonts::new(96),
            brushes: PadBrushes::default(),
            caption: None,
            tooltips: None,
            worker: StorageWorker::spawn(store).map_err(storage_error)?,
            document,
            active,
            rows: Vec::new(),
            query: String::new(),
            pane: PadPane::List,
            generation,
            latest_submitted: generation,
            updating_controls: false,
            theme: AppearanceTheme::Auto,
            window: HWND::default(),
            status_message: load_status.unwrap_or_else(|| {
                if recovered_from_backup {
                    "バックアップから復元しました".to_owned()
                } else {
                    String::new()
                }
            }),
            status_notice: false,
            status_slot: Cell::new(0),
            save_blocked,
        });
        let state_ptr = (&mut *state) as *mut PadState as *const c_void;
        // SAFETY: the class is registered above and the state box outlives the
        // window, which clears the pointer in `WM_NCDESTROY`.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PAD_CLASS,
                windows::core::w!("Sakura Pad"),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                i32::MIN,
                i32::MIN,
                scaled(PAD_WIDTH_LOGICAL, 96),
                scaled(PAD_HEIGHT_LOGICAL, 96),
                Some(owner),
                None,
                None,
                Some(state_ptr),
            )?
        };
        // The size above had to be asked for in 96-DPI pixels, because a
        // window has no monitor — and so no scale — until it exists. On any
        // other display that is the wrong number of pixels for the same amount
        // of window, and it is corrected here rather than at the first resize
        // so that the opening frame is already the right shape.
        resize_to_logical(hwnd);
        Ok(Self { hwnd, state })
    }

    pub fn set_theme(&mut self, theme: AppearanceTheme) {
        self.state.theme = theme;
        self.state.refresh_brushes();
        pad_caption::dress(self.hwnd, theme);
        // Native controls use the system role colors in high contrast. A
        // repaint also applies a newly selected light/dark palette without
        // moving focus.
        // SAFETY: the window is live for the lifetime of this object.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
    }

    /// Show and activate the normal window. If it is already visible, focus
    /// the pane on screen; if foreground activation is denied, flash the title
    /// bar as a non-destructive attention cue.
    pub fn show_or_focus(&self) {
        // SAFETY: the window and its controls are live for this object.
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            } else if !IsWindowVisible(self.hwnd).as_bool() {
                // Shown cloaked, painted, then revealed. A window becomes
                // visible before it has painted, and a window of child
                // controls paints in pieces as each child takes its turn, so
                // an uncloaked show arrives as a flash of undefined ground
                // followed by the parts appearing one at a time.
                pad_caption::cloak(self.hwnd, true);
                let _ = ShowWindow(self.hwnd, SW_SHOW);
                // A renderer started as a hidden background helper can have
                // its first ShowWindow command replaced by STARTUPINFO's
                // initial SW_HIDE. A second explicit request is then the
                // documented way to apply this window's own show state.
                if !IsWindowVisible(self.hwnd).as_bool() {
                    let _ = ShowWindow(self.hwnd, SW_SHOW);
                }
                // UPDATENOW is what makes this synchronous: it paints now
                // rather than posting a WM_PAINT that would arrive after the
                // window is already uncloaked and on screen.
                let _ = RedrawWindow(
                    Some(self.hwnd),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
                );
                pad_caption::cloak(self.hwnd, false);
            }
            if !SetForegroundWindow(self.hwnd).as_bool() {
                flash(self.hwnd);
            }
            let focus = if self.state.pane == PadPane::Editor || is_wide(self.hwnd) {
                self.state.body
            } else {
                self.state.list
            };
            if SetFocus(Some(focus)).is_err() {
                flash(self.hwnd);
            }
        }
    }

    pub fn hide(&self) {
        // SAFETY: the window is live for the lifetime of this object.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    #[cfg(debug_assertions)]
    pub fn is_visible(&self) -> bool {
        // SAFETY: the window is live for the lifetime of this object.
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }
}

impl Drop for PadWindow {
    fn drop(&mut self) {
        // SAFETY: the timers and window belong to this object.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), PAD_EDIT_TIMER);
            let _ = KillTimer(Some(self.hwnd), PAD_COMPLETION_TIMER);
            let _ = KillTimer(Some(self.hwnd), PAD_NOTICE_TIMER);
            let _ = DestroyWindow(self.hwnd);
        }
        let _ = self.state.worker.shutdown(SHUTDOWN_FLUSH_BUDGET);
    }
}

impl Drop for PadState {
    fn drop(&mut self) {
        self.fonts.destroy();
        self.brushes.destroy();
    }
}

/// The backgrounds `WM_CTLCOLOR*` answers with.
///
/// None of them can be a temporary: Windows may keep using the brush a control
/// was handed until the next message. The grid could not be one in any case,
/// being a bitmap pattern rather than a color.
#[derive(Debug, Default)]
struct PadBrushes {
    surface: HBRUSH,
    paper: HBRUSH,
    /// The search field, which is a filled chip rather than a box on the
    /// chrome, so its control has to be filled to match.
    chip: HBRUSH,
    /// Invalid where the palette has no grid — under Windows high contrast —
    /// in which case the writing area is plain paper.
    grid: HBRUSH,
    /// What the four above were built from. Rebuilding them on every message
    /// would delete a brush a control is still holding.
    built: Option<(Palette, i32)>,
}

impl PadBrushes {
    fn refresh(&mut self, colors: Palette, dpi: u32) {
        let cell = scaled(GRID_96, dpi).max(2);
        if self.built == Some((colors, cell)) && !self.surface.is_invalid() {
            return;
        }
        self.destroy();
        // SAFETY: both brushes are owned here until `destroy` releases them.
        unsafe {
            self.surface = CreateSolidBrush(colors.surface);
            self.paper = CreateSolidBrush(colors.paper);
            self.chip = CreateSolidBrush(colors.selected);
        }
        self.grid = colors.grid.map_or_else(HBRUSH::default, |line| {
            grid_brush(colors.paper, line, cell, scaled(BORDER_96, dpi).max(1))
        });
        self.built = Some((colors, cell));
    }

    /// The background for the writing area: ruled where the palette has a
    /// grid, plain paper where it does not.
    fn writing(&self) -> HBRUSH {
        if self.grid.is_invalid() {
            self.paper
        } else {
            self.grid
        }
    }

    fn destroy(&mut self) {
        for brush in [
            &mut self.surface,
            &mut self.paper,
            &mut self.chip,
            &mut self.grid,
        ] {
            if !brush.is_invalid() {
                // SAFETY: each was created by `refresh` and is no longer held
                // by a control: every path that gets here repaints the whole
                // window afterwards, and at shutdown the children are gone.
                unsafe {
                    let _ = DeleteObject((*brush).into());
                }
            }
            *brush = HBRUSH::default();
        }
        self.built = None;
    }
}

/// One tile of the ruled background: paper with its top and left edges ruled,
/// so laying the tile end to end draws a continuous grid.
fn grid_brush(paper: COLORREF, line: COLORREF, cell: i32, weight: i32) -> HBRUSH {
    // SAFETY: every object below is created, used and released on this path;
    // the screen DC is only borrowed for compatibility and is returned.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return HBRUSH::default();
        }
        let memory = CreateCompatibleDC(Some(screen));
        let tile = CreateCompatibleBitmap(screen, cell, cell);
        let brush = if memory.is_invalid() || tile.is_invalid() {
            HBRUSH::default()
        } else {
            let previous = SelectObject(memory, tile.into());
            let whole = RECT {
                left: 0,
                top: 0,
                right: cell,
                bottom: cell,
            };
            fill_color(memory, &whole, paper);
            fill_color(
                memory,
                &RECT {
                    bottom: weight,
                    ..whole
                },
                line,
            );
            fill_color(
                memory,
                &RECT {
                    right: weight,
                    ..whole
                },
                line,
            );
            SelectObject(memory, previous);
            // `CreatePatternBrush` takes its own copy of the bitmap, so the
            // tile is ours to delete as soon as the brush exists.
            CreatePatternBrush(tile)
        };
        if !tile.is_invalid() {
            let _ = DeleteObject(tile.into());
        }
        if !memory.is_invalid() {
            let _ = DeleteDC(memory);
        }
        ReleaseDC(None, screen);
        brush
    }
}

/// The EDIT class procedure, captured for the body.
///
/// A separate slot from the search field's although both capture the same
/// class procedure: one shared slot would make each field's hook depend on
/// which of the two happened to be created first.
static BODY_PROC: AtomicIsize = AtomicIsize::new(0);

/// Keeps the ruled background aligned while the body scrolls.
///
/// A control scrolls by moving the pixels it already has and repainting only
/// the strip that appeared, but the grid is tiled from the control's origin,
/// so the pixels that moved carry rules that no longer line up with the ones
/// painted fresh. Redrawing the whole control whenever the first visible line
/// changes costs one repaint per scroll and keeps the paper straight.
fn install_body_grid(body: HWND) {
    if body.is_invalid() {
        return;
    }
    let ours = body_proc as *const () as isize;
    // SAFETY: `body` is a live child created on this thread a moment ago.
    let previous = unsafe { SetWindowLongPtrW(body, GWLP_WNDPROC, ours) };
    // Never remember our own procedure: that would make `CallWindowProcW`
    // below recurse until the stack ends.
    if previous != 0 && previous != ours {
        let _ = BODY_PROC.compare_exchange(0, previous, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// What the body is showing, as far as the ruled paper is concerned: the line
/// at the top, how many lines there are, and how much text.
///
/// Scrolling is not the only thing that moves pixels the rules were drawn on.
/// Typing a newline makes the control shift everything below it down a line,
/// and the shifted pixels carry the rules they were drawn over, so the paper
/// is ruled twice over until something repaints it. Any edit at all can do
/// that, and the length and the line count together are what an edit changes.
fn body_view(control: HWND) -> (isize, isize, isize) {
    PROBING.set(true);
    // SAFETY: the control is live and none of the three change it.
    let view = unsafe {
        (
            SendMessageW(control, EM_GETFIRSTVISIBLELINE, None, None).0,
            SendMessageW(control, EM_GETLINECOUNT, None, None).0,
            SendMessageW(control, WM_GETTEXTLENGTH, None, None).0,
        )
    };
    PROBING.set(false);
    view
}

// Set while the body is being asked what it is showing. Every question is a
// message to the body, and the hook below sees them all; asking again from
// inside the answer would recurse until the stack ended.
thread_local! {
    static PROBING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

unsafe extern "system" fn body_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // Asking what the control is showing is itself a message, and it
    // arrives back here: probing the probe recurses until the stack ends.
    // `WM_NCDESTROY` is left out for the opposite reason — after it the
    // control is gone, and it cannot have moved anything on the way out.
    let watched = !PROBING.get() && message != WM_NCDESTROY;
    let before = watched.then(|| body_view(window));
    let captured = BODY_PROC.load(Ordering::Relaxed);
    let result = if captured == 0 {
        // SAFETY: the class procedure was never captured, so the default
        // handler is the only destination left.
        unsafe { DefWindowProcW(window, message, w, l) }
    } else {
        // SAFETY: `captured` came from `SetWindowLongPtrW(GWLP_WNDPROC)`, so
        // it is the EDIT class procedure and has exactly this signature.
        let previous: WNDPROC = Some(unsafe {
            std::mem::transmute::<
                isize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(captured)
        });
        // SAFETY: the control is live for the whole message.
        unsafe { CallWindowProcW(previous, window, message, w, l) }
    };
    if before.is_some_and(|before| body_view(window) != before) {
        // SAFETY: the control is live.
        unsafe {
            let _ = InvalidateRect(Some(window), None, true);
        }
    }
    result
}

/// Gives the pad's controls the standard dialog keyboard.
///
/// The renderer's pump is a plain `GetMessage`/`Dispatch` loop, which is what
/// every other renderer window wants. Without this, Tab in the pad types a
/// tab character into whichever edit has focus and the toolbar can never be
/// reached from the keyboard at all.
pub fn dialog_navigation(message: &MSG) -> bool {
    if !matches!(
        message.message,
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_CHAR | WM_SYSCHAR
    ) {
        return false;
    }
    // SAFETY: the message carries a window handle Windows just validated;
    // `GetAncestor` tolerates a stale one by returning null.
    let root = unsafe { GetAncestor(message.hwnd, GA_ROOT) };
    if root.is_invalid() || !is_pad_class(root) {
        return false;
    }
    // SAFETY: `root` is a live top-level window and the message outlives the
    // call.
    unsafe { IsDialogMessageW(root, message).as_bool() }
}

fn is_pad_class(window: HWND) -> bool {
    let mut name = [0u16; 32];
    // SAFETY: the buffer bounds the write and the window is live.
    let length = unsafe { GetClassNameW(window, &mut name) };
    if length <= 0 {
        return false;
    }
    let observed = String::from_utf16_lossy(&name[..length as usize]);
    // SAFETY: `PAD_CLASS` is a static NUL-terminated wide string.
    let expected = unsafe { PAD_CLASS.to_string() };
    expected.is_ok_and(|expected| expected == observed)
}

fn register_class() {
    // SAFETY: static class name/procedure and a null instance are valid for a
    // process-local class. Re-registering after another renderer component
    // has done so is harmless (RegisterClassW simply fails with already
    // registered).
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(pad_procedure),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: PAD_CLASS,
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
    }
}

fn create_child(class: PCWSTR, text: PCWSTR, style: i32, parent: HWND, id: u16) -> Result<HWND> {
    // SAFETY: all pointers are static or live through this synchronous call;
    // child controls are owned by the pad parent.
    unsafe {
        CreateWindowExW(
            Default::default(),
            class,
            text,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(style as u32)
                | WS_CHILD
                | WS_VISIBLE,
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
}

fn create_controls(state: &mut PadState, parent: HWND) -> Result<()> {
    state.window = parent;
    let button = |label: PCWSTR, id: u16| -> Result<HWND> {
        create_child(
            windows::core::w!("BUTTON"),
            label,
            WS_TABSTOP.0 as i32 | BS_OWNERDRAW,
            parent,
            id,
        )
    };
    // The window text is the accessible name, and it is a word even where
    // the face is a drawn icon: a screen reader that announced `≡` would be
    // reading the picture out loud instead of the control.
    state.menu = button(windows::core::w!("メニュー"), MENU_ID)?;
    state.new = button(windows::core::w!("新規メモ"), NEW_ID)?;
    state.sort = button(windows::core::w!("並べ替え"), SORT_ID)?;
    state.sync = button(windows::core::w!("同期"), SYNC_ID)?;
    state.copy = button(windows::core::w!("Markdown としてコピー"), COPY_ID)?;
    state.delete = button(windows::core::w!("削除"), DELETE_ID)?;

    // The pointer gets a sentence for each drawn face. The window text above
    // stays the short name, because that is what a screen reader announces
    // and a sentence would be read out on every focus move.
    let hinted = [
        (state.menu, MENU_ID),
        (state.new, NEW_ID),
        (state.sort, SORT_ID),
        (state.sync, SYNC_ID),
        (state.copy, COPY_ID),
        (state.delete, DELETE_ID),
    ];
    state.tooltips = Tooltips::new(parent);
    if let Some(tooltips) = state.tooltips.as_mut() {
        for (control, id) in hinted {
            if let Some(text) = hint(id) {
                tooltips.attach(parent, control, text);
            }
        }
    }

    state.header_title = create_child(
        windows::core::w!("STATIC"),
        windows::core::w!("メモ帳"),
        STATIC_CENTERED_ELLIPSIS,
        parent,
        HEADER_TITLE_ID,
    )?;
    state.status = create_child(
        windows::core::w!("STATIC"),
        windows::core::w!(""),
        STATIC_CENTERED_ELLIPSIS,
        parent,
        STATUS_ID,
    )?;
    state.count = create_child(
        windows::core::w!("STATIC"),
        windows::core::w!(""),
        STATIC_CENTERED_ELLIPSIS | STATIC_RIGHT,
        parent,
        COUNT_ID,
    )?;
    state.search = create_child(
        windows::core::w!("EDIT"),
        windows::core::w!(""),
        WS_TABSTOP.0 as i32 | ES_LEFT | ES_AUTOHSCROLL,
        parent,
        SEARCH_ID,
    )?;
    install_search_placeholder(state.search);
    state.list = create_child(
        windows::core::w!("LISTBOX"),
        windows::core::w!(""),
        WS_TABSTOP.0 as i32
            // No `WS_VSCROLL`: each pane has a rail of the pad's own beside
            // it, drawn on the pane's own ground and at the pad's own width.
            // Left to itself a LISTBOX rounds its height down to a whole
            // number of rows and gives the remainder back as bare surface —
            // up to one row of nothing between the last memo and the bar.
            // The list fills the column it was given instead, and a row met
            // by the bottom edge says there is more to scroll to.
            | LBS_NOINTEGRALHEIGHT
            | LBS_OWNERDRAWFIXED
            | LBS_HASSTRINGS
            | LBS_NOTIFY,
        parent,
        LIST_ID,
    )?;
    state.title = create_child(
        windows::core::w!("EDIT"),
        windows::core::w!(""),
        WS_TABSTOP.0 as i32 | ES_LEFT | ES_AUTOHSCROLL | ES_NOHIDESEL,
        parent,
        TITLE_ID,
    )?;
    state.body = create_child(
        windows::core::w!("EDIT"),
        windows::core::w!(""),
        WS_TABSTOP.0 as i32
            | ES_LEFT
            | ES_MULTILINE
            | ES_AUTOVSCROLL
            | ES_WANTRETURN
            | ES_NOHIDESEL,
        parent,
        BODY_ID,
    )?;

    // SAFETY: every handle above is a live child of `parent`, and the limits
    // are the same ones the document format enforces.
    unsafe {
        let _ = SendMessageW(
            state.title,
            EM_SETLIMITTEXT,
            Some(WPARAM(MAX_TITLE_UTF16_UNITS)),
            Some(LPARAM(0)),
        );
        let _ = SendMessageW(
            state.body,
            EM_SETLIMITTEXT,
            Some(WPARAM(MAX_BODY_UTF16_UNITS)),
            Some(LPARAM(0)),
        );
        let _ = SendMessageW(
            state.search,
            EM_SETLIMITTEXT,
            Some(WPARAM(MAX_QUERY_UTF16_UNITS)),
            Some(LPARAM(0)),
        );
    }

    install_body_grid(state.body);

    // The rails come after the panes they read: each subclasses its pane, and
    // the body's grid hook has to be the inner one so that it still sees every
    // message the rail passes along.
    state.list_rail = pad_rail::create(parent, state.list, pad_rail::Scrolls::Rows, LIST_RAIL_ID)
        .unwrap_or_default();
    state.body_rail = pad_rail::create(parent, state.body, pad_rail::Scrolls::Lines, BODY_RAIL_ID)
        .unwrap_or_default();

    state.apply_dpi(dpi_of(parent));
    state.refresh_brushes();
    state.refresh_list();
    state.refresh_editor();
    state.update_status();
    Ok(())
}

/// Sizes `window` so its client area is the pad's logical size at whatever
/// DPI it landed on.
///
/// Silent on failure: the window keeps the size it was created with, which is
/// what it would have had anyway.
fn resize_to_logical(window: HWND) {
    let dpi = dpi_of(window);
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: scaled(PAD_WIDTH_LOGICAL, dpi),
        bottom: scaled(PAD_HEIGHT_LOGICAL, dpi),
    };
    // SAFETY: the rectangle is a local, and the style pair is the one the
    // window was created with, so the frame it grows by is that window's own.
    unsafe {
        if AdjustWindowRectExForDpi(
            &mut frame,
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            false,
            WINDOW_EX_STYLE::default(),
            dpi,
        )
        .is_err()
        {
            return;
        }
        let _ = SetWindowPos(
            window,
            None,
            0,
            0,
            frame.right.saturating_sub(frame.left),
            frame.bottom.saturating_sub(frame.top),
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

pub(crate) fn dpi_of(window: HWND) -> u32 {
    // SAFETY: a live window; the documented failure value is zero.
    unsafe { GetDpiForWindow(window) }.max(96)
}

fn is_wide(window: HWND) -> bool {
    let mut client = RECT::default();
    // SAFETY: the window and output rectangle are live.
    if unsafe { GetClientRect(window, &mut client) }.is_err() {
        return false;
    }
    layout(client, dpi_of(window), PadPane::List, 0).wide
}

fn set_control_text(window: HWND, value: &str) {
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    wide.push(0);
    // SAFETY: the buffer is NUL-terminated and outlives the call.
    unsafe {
        let _ = SetWindowTextW(window, PCWSTR(wide.as_ptr()));
    }
}

fn get_control_text(window: HWND, max_units: usize) -> String {
    let mut buffer = vec![0u16; max_units.saturating_add(1)];
    // SAFETY: the slice bounds the copy.
    let length = unsafe { GetWindowTextW(window, &mut buffer) };
    String::from_utf16_lossy(&buffer[..(length.max(0) as usize).min(max_units)])
}

fn flash(window: HWND) {
    let info = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd: window,
        dwFlags: windows::Win32::UI::WindowsAndMessaging::FLASHWINFO_FLAGS(3),
        uCount: 2,
        dwTimeout: 0,
    };
    // SAFETY: the structure is fully initialized and sized.
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

fn storage_error(error: StorageError) -> windows::core::Error {
    windows::core::Error::new(
        windows::core::HRESULT(0x8004_0005u32 as i32),
        error.to_string(),
    )
}

/// Puts `value` on the clipboard as Unicode text.
///
/// The block is freed on every failing path and only on those: a successful
/// `SetClipboardData` transfers ownership to the clipboard, and freeing it
/// afterwards is a use-after-free the next paste would find.
fn copy_to_clipboard(owner: HWND, value: &str) -> bool {
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len().saturating_mul(size_of::<u16>());
    // SAFETY: every handle is checked, the copy is bounded by the allocation
    // requested for it, and the clipboard is closed on every path.
    unsafe {
        let Ok(block) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            return false;
        };
        let target = GlobalLock(block);
        if target.is_null() {
            let _ = free_global(block);
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), target.cast::<u16>(), wide.len());
        let _ = GlobalUnlock(block);
        if OpenClipboard(Some(owner)).is_err() {
            let _ = free_global(block);
            return false;
        }
        if EmptyClipboard().is_err() {
            let _ = CloseClipboard();
            let _ = free_global(block);
            return false;
        }
        let format = windows::Win32::System::Ole::CF_UNICODETEXT.0 as u32;
        let placed =
            windows::Win32::System::DataExchange::SetClipboardData(format, Some(HANDLE(block.0)))
                .is_ok();
        let _ = CloseClipboard();
        if !placed {
            let _ = free_global(block);
        }
        placed
    }
}

fn free_global(block: HGLOBAL) -> bool {
    // SAFETY: the block is owned by this call chain and not on the clipboard.
    unsafe { windows::Win32::Foundation::GlobalFree(Some(block)).is_ok() }
}

fn update_layout(state: &PadState, window: HWND) {
    let mut client = RECT::default();
    // SAFETY: the window and output rectangle are live.
    if unsafe { GetClientRect(window, &mut client) }.is_err() {
        return;
    }
    let dpi = dpi_of(window);
    let want = state.status_want();
    state.status_slot.set(want);
    let plan = layout(client, dpi, state.pane, want);
    let field = |frame: Option<RECT>| frame.map(|frame| field_child(frame, dpi));
    let placements: [(HWND, Option<RECT>); 15] = [
        (state.menu, plan.menu),
        (state.header_title, plan.header_title),
        (state.status, plan.status),
        (state.count, plan.count),
        (state.copy, Some(plan.copy)),
        (state.delete, Some(plan.delete)),
        (state.search, field(plan.search)),
        (state.list, plan.list),
        (state.list_rail, plan.list_rail),
        (state.title, plan.title),
        (state.body, plan.body),
        (state.body_rail, plan.body_rail),
        (state.new, Some(plan.new)),
        (state.sort, Some(plan.sort)),
        (state.sync, Some(plan.sync)),
    ];
    // DeferWindowPos keeps a resize to one update and avoids the flicker of
    // moving eleven controls one at a time.
    // SAFETY: every handle is a live child of `window` and the batch handle is
    // threaded through each call as the API requires.
    unsafe {
        let Ok(mut batch) = BeginDeferWindowPos(placements.len() as i32) else {
            return;
        };
        for (child, rect) in placements {
            let (rect, visibility) = match rect {
                Some(rect) if !is_empty(rect) => (rect, SWP_SHOWWINDOW),
                _ => (RECT::default(), SWP_HIDEWINDOW),
            };
            let Ok(next) = DeferWindowPos(
                batch,
                child,
                Some(HWND_TOP),
                rect.left,
                rect.top,
                (rect.right - rect.left).max(1),
                (rect.bottom - rect.top).max(1),
                SWP_NOZORDER | SWP_NOACTIVATE | visibility,
            ) else {
                return;
            };
            batch = next;
        }
        let _ = EndDeferWindowPos(batch);
    }
}

/// Fills the bands the controls do not cover and draws the rules between them.
fn paint(window: HWND, state: &PadState) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: every non-invalid paint DC is paired with `EndPaint` below.
    let dc = unsafe { BeginPaint(window, &mut ps) };
    if dc.is_invalid() {
        return;
    }
    let mut client = RECT::default();
    // SAFETY: the window and output rectangle are live.
    if unsafe { GetClientRect(window, &mut client) }.is_ok() {
        let dpi = dpi_of(window);
        let colors = palette(state.theme);
        let plan = layout(client, dpi, state.pane, state.status_want());
        let border = scaled(BORDER_96, dpi).max(1);
        fill_color(dc, &client, colors.surface);
        // The writing surface, under the controls standing on it. The ruled
        // squares are not painted here: they belong to the body control, which
        // tiles them from its own origin, and a second grid laid down from the
        // window's origin would sit a few pixels out of step behind the first.
        if let Some(paper) = plan.paper {
            fill_color(dc, &paper, colors.paper);
        }
        // Every band the pad owns is closed by a rule: under the header, under
        // the editor's first row, between the columns, and above the bar.
        for band in [plan.header, plan.meta].into_iter().flatten() {
            rule(
                dc,
                RECT {
                    top: band.bottom.saturating_sub(border),
                    ..band
                },
                colors.border,
            );
        }
        if let Some(divider) = plan.divider {
            rule(dc, divider, colors.border);
        }
        rule(
            dc,
            RECT {
                bottom: plan.bottom.top.saturating_add(border),
                ..plan.bottom
            },
            colors.border,
        );
        // The search field is a filled chip rather than an outlined box. An
        // outline here would be the only one in the window, and a lone
        // outlined field reads as a field with something wrong with it.
        if let Some(search) = plan.search {
            rounded_box(
                dc,
                search,
                colors.selected,
                None,
                border,
                scaled(CORNER_96, dpi).max(1),
            );
            let side = pad_icon::size(dpi);
            let left = search.left.saturating_add(scaled(PADDING_96, dpi));
            pad_icon::draw(
                dc,
                pad_icon::box_in(
                    RECT {
                        left,
                        right: left.saturating_add(side),
                        ..search
                    },
                    dpi,
                ),
                PadIcon::Search,
                colors.annotation,
            );
        }
    }
    // SAFETY: pairs with the successful `BeginPaint` above.
    unsafe {
        let _ = EndPaint(window, &ps);
    }
}

/// What the empty search field says it is for.
///
/// `EM_SETCUEBANNER` is not available here: it needs comctl32 version 6, which
/// means shipping a visual-styles manifest that would restyle every control in
/// the pad. Putting the word in the field's *text* would be worse — the filter
/// reads that text, so a resting pad would match no memo and show an empty
/// list. So the field keeps its own window procedure and the hint stays paint,
/// which leaves the text genuinely empty.
const SEARCH_PLACEHOLDER: &str = "検索";

/// The EDIT class procedure, captured once. Every pad's search field is an
/// instance of the same class, so there is exactly one to remember.
static SEARCH_PROC: AtomicIsize = AtomicIsize::new(0);

fn install_search_placeholder(search: HWND) {
    if search.is_invalid() {
        return;
    }
    let ours = search_proc as *const () as isize;
    // SAFETY: `search` is a live child created on this thread a moment ago.
    let previous = unsafe { SetWindowLongPtrW(search, GWLP_WNDPROC, ours) };
    // Never remember our own procedure: that would make `CallWindowProcW`
    // below recurse until the stack ends.
    if previous != 0 && previous != ours {
        let _ = SEARCH_PROC.compare_exchange(0, previous, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// The hint shows only in the field's resting state: nothing typed, and the
/// caret somewhere else.
fn shows_search_placeholder(text_units: i32, focused: bool) -> bool {
    text_units <= 0 && !focused
}

unsafe extern "system" fn search_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let captured = SEARCH_PROC.load(Ordering::Relaxed);
    let result = if captured == 0 {
        // SAFETY: the class procedure was never captured, so the default
        // handler is the only correct destination.
        unsafe { DefWindowProcW(window, message, w, l) }
    } else {
        // SAFETY: `captured` came from `SetWindowLongPtrW(GWLP_WNDPROC)` and
        // is the EDIT class procedure, which has exactly this signature.
        let previous: WNDPROC = Some(unsafe {
            std::mem::transmute::<
                usize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(captured as usize)
        });
        // SAFETY: the field is live for the duration of this message.
        unsafe { CallWindowProcW(previous, window, message, w, l) }
    };
    match message {
        // Whether the hint shows depends on the focus and the text, so a
        // change to either has to redraw the field.
        WM_SETFOCUS | WM_KILLFOCUS | WM_SETTEXT => {
            // SAFETY: the field is live.
            unsafe {
                let _ = InvalidateRect(Some(window), None, true);
            }
        }
        // After the class procedure has painted, not instead of it: the hint
        // belongs on top of the field's own background.
        WM_PAINT => paint_search_placeholder(window),
        _ => {}
    }
    result
}

fn paint_search_placeholder(search: HWND) {
    // SAFETY: the field is live; both calls only read window state.
    let (units, focused) = unsafe { (GetWindowTextLengthW(search), GetFocus() == search) };
    if !shows_search_placeholder(units, focused) {
        return;
    }
    // SAFETY: as above.
    let Ok(parent) = (unsafe { GetParent(search) }) else {
        return;
    };
    // SAFETY: the slot holds a `PadState` for as long as the pad exists, and
    // this runs on the pad's own thread inside its own message.
    let state_ptr = unsafe { GetWindowLongPtrW(parent, GWLP_USERDATA) } as *const PadState;
    if state_ptr.is_null() {
        return;
    }
    // SAFETY: as above; the borrow ends before any message can re-enter.
    let state = unsafe { &*state_ptr };
    let colors = palette(state.theme);

    let mut client = RECT::default();
    // SAFETY: the field is live and the rectangle is a live output.
    if unsafe { GetClientRect(search, &mut client) }.is_err() {
        return;
    }
    // The field's own margins, so the hint starts exactly where typing will.
    // SAFETY: `EM_GETMARGINS` takes no pointer and returns the two margins
    // packed into one value.
    let margins = unsafe { SendMessageW(search, EM_GETMARGINS, None, None) }.0 as u32;
    let rect = RECT {
        left: client.left.saturating_add((margins & 0xffff) as i32),
        right: client.right.saturating_sub((margins >> 16) as i32),
        ..client
    };

    // SAFETY: the field is live; the DC is released below on every path.
    let dc = unsafe { GetDC(Some(search)) };
    if dc.is_invalid() {
        return;
    }
    // SAFETY: the font belongs to the field and outlives this paint.
    let font = HFONT(unsafe { SendMessageW(search, WM_GETFONT, None, None) }.0 as *mut c_void);
    let restore = select_font(dc, font);
    // SAFETY: the DC is live for the rest of this function.
    unsafe {
        SetBkMode(dc, TRANSPARENT);
    }
    text(
        dc,
        SEARCH_PLACEHOLDER,
        rect,
        colors.annotation,
        DT_LEFT | DT_END_ELLIPSIS,
    );
    if let Some(restore) = restore {
        // SAFETY: `restore` is the object the DC held before `select_font`.
        unsafe {
            SelectObject(dc, restore);
        }
    }
    // SAFETY: pairs with the `GetDC` above.
    unsafe {
        ReleaseDC(Some(search), dc);
    }
}

fn rule(dc: HDC, rect: RECT, color: COLORREF) {
    if !is_empty(rect) {
        fill_color(dc, &rect, color);
    }
}

fn draw_frame(dc: HDC, rect: RECT, color: COLORREF, thickness: i32) {
    if is_empty(rect) {
        return;
    }
    for edge in [
        RECT {
            bottom: rect.top.saturating_add(thickness),
            ..rect
        },
        RECT {
            top: rect.bottom.saturating_sub(thickness),
            ..rect
        },
        RECT {
            right: rect.left.saturating_add(thickness),
            ..rect
        },
        RECT {
            left: rect.right.saturating_sub(thickness),
            ..rect
        },
    ] {
        fill_color(dc, &edge, color);
    }
}

/// A filled rounded rectangle, outlined when `frame` says so.
///
/// `RoundRect` outlines with whatever pen the DC holds, so a box with no
/// outline still needs one and uses its own fill color for it.
fn rounded_box(
    dc: HDC,
    rect: RECT,
    fill: COLORREF,
    frame: Option<COLORREF>,
    thickness: i32,
    radius: i32,
) {
    if is_empty(rect) {
        return;
    }
    // SAFETY: both objects are created here, selected here, and restored and
    // deleted on every path, so the DC leaves with the objects it arrived
    // with.
    unsafe {
        let pen = CreatePen(PS_SOLID, thickness, frame.unwrap_or(fill));
        let brush = CreateSolidBrush(fill);
        if pen.is_invalid() || brush.is_invalid() {
            if !pen.is_invalid() {
                let _ = DeleteObject(pen.into());
            }
            if !brush.is_invalid() {
                let _ = DeleteObject(brush.into());
            }
            // Square corners are the wrong shape. An unpainted control is not
            // a shape at all, so the pad falls back to the square one.
            fill_color(dc, &rect, fill);
            if let Some(frame) = frame {
                draw_frame(dc, rect, frame, thickness);
            }
            return;
        }
        let previous_pen = SelectObject(dc, pen.into());
        let previous_brush = SelectObject(dc, brush.into());
        let _ = RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(dc, previous_brush);
        SelectObject(dc, previous_pen);
        let _ = DeleteObject(brush.into());
        let _ = DeleteObject(pen.into());
    }
}

/// The three button shapes. Chrome controls act on the window and stay quiet,
/// framed controls act on the memos and are drawn as things to press, and the
/// one filled control is the one that makes something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonShape {
    Chrome,
    Framed,
    Filled,
}

/// Deleting is the one control in the pad that cannot be undone by pressing it
/// again, so it is the one drawn in a color that says so — but only where it
/// stands among the other controls in the folded bar. Unfolded it is one of
/// two controls in the editor's own head row, and a warning color beside the
/// memo's title reads as something being wrong with the memo.
fn destructive(id: u16, wide: bool) -> bool {
    id == DELETE_ID && !wide
}

fn draw_button(
    item: &DRAWITEMSTRUCT,
    face: ButtonFace,
    shape: ButtonShape,
    danger: bool,
    ground: COLORREF,
    colors: Palette,
    dpi: u32,
) {
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
    let border = scaled(BORDER_96, dpi).max(1);
    let filled = shape == ButtonShape::Filled;
    // The ground goes down first even where a face covers it: `RoundRect`
    // leaves the four corners outside its figure, and an owner-drawn item that
    // skips a pixel shows whatever the DC happened to be holding.
    fill_color(item.hDC, &item.rcItem, ground);
    let face_color = match (shape, pressed) {
        (ButtonShape::Filled, false) => colors.rail,
        // Pressing the filled control darkens it. It cannot lighten: nothing
        // lighter than the rail is still the same control.
        (ButtonShape::Filled, true) => colors.action,
        (_, true) => colors.selected,
        (ButtonShape::Framed, false) => colors.surface,
        (ButtonShape::Chrome, false) => ground,
    };
    // Focus takes the rail, the one saturated color in the product, so
    // keyboard focus is as findable in the pad as a selected candidate. The
    // filled control already is the rail, so it takes the darker action color
    // rather than a ring the color of the thing it rings.
    let frame_color = match (focused, filled, shape) {
        (true, true, _) => Some(colors.action),
        (true, false, _) => Some(colors.rail),
        (false, _, ButtonShape::Framed) => Some(colors.border),
        (false, _, _) => None,
    };
    if frame_color.is_some() || face_color != ground {
        rounded_box(
            item.hDC,
            item.rcItem,
            face_color,
            frame_color,
            border,
            scaled(CORNER_96, dpi).max(1),
        );
    }
    let ink = match (filled, danger, pressed) {
        (true, _, _) => colors.surface,
        (false, true, _) => colors.danger,
        (false, false, true) => colors.selected_ink,
        // A drawn face is a continuous stroke where a word is a row of thin
        // ones, so the same color reads darker on an icon than on the text
        // beside it. The wordless controls are supporting chrome and take the
        // supporting color; pressing one brings it up to full ink.
        (false, false, false) if face.label.is_none() => colors.annotation,
        (false, false, false) => colors.ink,
    };
    let Some(label) = face.label else {
        pad_icon::draw(item.hDC, pad_icon::box_in(item.rcItem, dpi), face.icon, ink);
        return;
    };
    // Icon and word are one thing, so they are centered as one thing rather
    // than each in its own half.
    let gap = scaled(GAP_96, dpi) / 2;
    let side = pad_icon::size(dpi);
    let width = side + gap + text_width(item.hDC, label);
    let left = item
        .rcItem
        .left
        .saturating_add((item.rcItem.right.saturating_sub(item.rcItem.left) - width) / 2)
        .max(item.rcItem.left);
    pad_icon::draw(
        item.hDC,
        pad_icon::box_in(
            RECT {
                left,
                right: left.saturating_add(side),
                ..item.rcItem
            },
            dpi,
        ),
        face.icon,
        ink,
    );
    text(
        item.hDC,
        label,
        RECT {
            left: left.saturating_add(side).saturating_add(gap),
            ..item.rcItem
        },
        ink,
        DT_LEFT,
    );
}

fn draw_row(item: &DRAWITEMSTRUCT, state: &PadState, colors: Palette, dpi: u32) {
    let Some(memo) = state
        .rows
        .get(item.itemID as usize)
        .and_then(|id| state.document.find(*id))
    else {
        fill_color(item.hDC, &item.rcItem, colors.surface);
        return;
    };
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    let row = item.rcItem;
    fill_color(
        item.hDC,
        &row,
        if selected {
            colors.selected
        } else {
            colors.surface
        },
    );
    if selected {
        fill_color(
            item.hDC,
            &RECT {
                right: row
                    .left
                    .saturating_add(scaled(pad_list::ROW_RAIL_96, dpi).max(1)),
                ..row
            },
            colors.rail,
        );
    } else if item.itemID as usize + 1 < state.rows.len() {
        // Rows that share the list's own color are told apart by a hairline.
        // The selected row has a face of its own and needs no help, and the
        // last row is closed by the bar under it.
        fill_color(
            item.hDC,
            &RECT {
                top: row.bottom.saturating_sub(scaled(BORDER_96, dpi).max(1)),
                ..row
            },
            colors.selected,
        );
    }

    let pad = scaled(PADDING_96, dpi);
    let gap = scaled(GAP_96, dpi);
    let inner_left = row.left.saturating_add(pad).saturating_add(pad);
    let inner_right = row.right.saturating_sub(pad);
    let height = row.bottom.saturating_sub(row.top);
    let title_bottom = row.top.saturating_add((height * 5) / 9);
    let time_width = scaled(ROW_TIME_WIDTH_96, dpi);
    let time_left = inner_right.saturating_sub(time_width).max(inner_left);

    let ink = if selected {
        colors.selected_ink
    } else {
        colors.ink
    };
    let previous = select_font(item.hDC, state.fonts.body);
    text(
        item.hDC,
        pad_list::display_title(memo),
        RECT {
            left: inner_left,
            top: row.top,
            right: time_left.saturating_sub(gap),
            bottom: title_bottom,
        },
        ink,
        DT_LEFT | DT_END_ELLIPSIS,
    );
    if let Some(previous) = previous {
        // SAFETY: restoring the DC's original object before this frame's font
        // could be reused elsewhere.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::SelectObject(item.hDC, previous);
        }
    }

    let previous = select_font(item.hDC, state.fonts.small);
    text(
        item.hDC,
        &pad_list::format_time(state.now(), pad_list::local_time(memo.updated_ms)),
        RECT {
            left: time_left,
            top: row.top,
            right: inner_right,
            bottom: title_bottom,
        },
        colors.annotation,
        DT_RIGHT,
    );
    text(
        item.hDC,
        &pad_list::preview(memo),
        RECT {
            left: inner_left,
            top: title_bottom,
            right: inner_right,
            bottom: row.bottom,
        },
        colors.annotation,
        DT_LEFT | DT_END_ELLIPSIS,
    );
    if let Some(previous) = previous {
        // SAFETY: as above; the DC must leave this call with the object it
        // arrived with.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::SelectObject(item.hDC, previous);
        }
    }
}

/// What a button paints: an icon, or an icon and the word beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ButtonFace {
    icon: PadIcon,
    /// Only the one control that makes something says so, and only where
    /// there is room. A row of five words in a folded window is a paragraph.
    label: Option<&'static str>,
}

fn button_face(id: u16, wide: bool) -> Option<ButtonFace> {
    let icon = match id {
        MENU_ID => PadIcon::Menu,
        NEW_ID => PadIcon::Plus,
        SORT_ID => PadIcon::Sort,
        SYNC_ID => PadIcon::Sync,
        COPY_ID => PadIcon::Copy,
        DELETE_ID => PadIcon::Trash,
        _ => return None,
    };
    Some(ButtonFace {
        icon,
        label: (id == NEW_ID && wide).then_some("新規メモ"),
    })
}

/// The hover text for a control.
///
/// Longer than the word on the control, and for a different reader: the
/// window text is the accessible name and says which control this is, while
/// the tip says what pressing it does. The copy control is why this exists —
/// its face was read as sending the memo somewhere, which is not what it
/// does.
fn hint(id: u16) -> Option<&'static str> {
    Some(match id {
        MENU_ID => "メモ一覧と編集を切り替え",
        NEW_ID => "新しいメモを作成",
        SORT_ID => "並べ替え順を変更",
        SYNC_ID => "GitHub と同期",
        COPY_ID => "このメモを Markdown としてコピー",
        DELETE_ID => "このメモを削除",
        _ => return None,
    })
}

fn button_shape(id: u16, wide: bool) -> ButtonShape {
    match id {
        MENU_ID => ButtonShape::Chrome,
        // Creating is the one thing the bar does that pressing again does not
        // undo, so it is the one control that is filled rather than framed.
        NEW_ID => ButtonShape::Filled,
        // Folded, the bar is five controls in one row, and a frame around
        // every one of them reads as a fence. Unfolded they are two and two,
        // far enough apart that each needs an edge to be a control at all.
        _ if wide => ButtonShape::Framed,
        _ => ButtonShape::Chrome,
    }
}

impl PadState {
    fn now(&self) -> Option<CalendarTime> {
        pad_list::local_time(now_ms())
    }

    fn apply_dpi(&mut self, dpi: u32) {
        if self.fonts.dpi != dpi || self.fonts.body.is_invalid() {
            let mut replaced = std::mem::replace(&mut self.fonts, PadFonts::new(dpi));
            replaced.destroy();
        }
        let assignments = [
            (self.menu, self.fonts.body),
            (self.header_title, self.fonts.heading),
            (self.status, self.fonts.small),
            (self.count, self.fonts.small),
            (self.search, self.fonts.small),
            (self.list, self.fonts.body),
            (self.title, self.fonts.body),
            (self.body, self.fonts.body),
            (self.new, self.fonts.small),
            (self.sort, self.fonts.body),
            (self.sync, self.fonts.body),
            (self.copy, self.fonts.body),
            (self.delete, self.fonts.body),
        ];
        // SAFETY: every handle is a live child and every font is owned here
        // until the next DPI change replaces the whole set.
        unsafe {
            for (child, font) in assignments {
                if child.is_invalid() {
                    continue;
                }
                let _ = SendMessageW(
                    child,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
            let _ = SendMessageW(
                self.list,
                LB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(scaled(pad_list::ROW_HEIGHT_96, dpi) as isize)),
            );
        }
        // The ruled squares are a physical size too.
        self.refresh_brushes();
    }

    /// Gives the title bar the product's icon at this DPI.
    ///
    /// A window class with no icon is drawn with Windows' placeholder, which
    /// is the one part of the window that says it belongs to some other
    /// program. A failed load keeps the pair already on screen: an icon a
    /// version behind beats the placeholder.
    fn apply_caption_icons(&mut self, window: HWND) {
        if let Some(icons) = pad_caption::icons(window, self.fonts.dpi) {
            // Assigned, and so the old pair dropped, only after the window has
            // been handed the new one.
            self.caption = Some(icons);
        }
    }

    /// Brings the control backgrounds up to the current palette and DPI.
    fn refresh_brushes(&mut self) {
        let colors = palette(self.theme);
        self.brushes.refresh(colors, self.fonts.dpi);
        // Each rail stands on the ground of the pane it reads, so the strip
        // is invisible until there is something to report. The thumb is the
        // pad's own rule colour, and the pointer over it is the colour a
        // reading is written in — a step up, not a highlight.
        for (rail, track) in [
            (self.list_rail, colors.surface),
            (self.body_rail, colors.paper),
        ] {
            pad_rail::set_colors(rail, track, colors.border, colors.annotation);
        }
    }

    /// Rebuilds the rows from the document and re-selects the active memo.
    fn refresh_list(&mut self) {
        self.rows = pad_list::rows(&self.document, &self.query);
        // SAFETY: the list is a live child owned by this state.
        unsafe {
            let _ = SendMessageW(self.list, LB_RESETCONTENT, None, None);
            for id in &self.rows {
                let label = self
                    .document
                    .find(*id)
                    .map(pad_list::display_title)
                    .unwrap_or(pad_list::UNTITLED);
                let mut wide: Vec<u16> = label.encode_utf16().collect();
                wide.push(0);
                let _ = SendMessageW(
                    self.list,
                    LB_ADDSTRING,
                    Some(WPARAM(0)),
                    Some(LPARAM(wide.as_ptr() as isize)),
                );
            }
            let selected = self
                .rows
                .iter()
                .position(|id| *id == self.active)
                .map_or(WPARAM(usize::MAX), WPARAM);
            let _ = SendMessageW(self.list, LB_SETCURSEL, Some(selected), Some(LPARAM(0)));
        }
        let total = self.document.live().count();
        let heading = if self.query.is_empty() {
            format!("メモ帳（{total}）")
        } else {
            format!("メモ帳（{}/{total}）", self.rows.len())
        };
        set_control_text(self.header_title, &heading);
    }

    fn refresh_editor(&mut self) {
        let (title, body) = self
            .document
            .find(self.active)
            .map(|memo| (memo.title.clone(), memo.body.clone()))
            .unwrap_or_default();
        self.updating_controls = true;
        set_control_text(self.title, &title);
        set_control_text(self.body, &body);
        self.updating_controls = false;
    }

    /// The editor's first row says three things about the open memo: what it
    /// is called, when it last changed, and how long it is. The length is its
    /// own control because it is measured against the right edge and must not
    /// move when the time beside it grows.
    /// The reading the status row is showing.
    ///
    /// A message about what just happened displaces the memo's own state: it
    /// is the newer fact. A notice then expires and gives the row back; a
    /// state stays until its successor replaces it.
    fn status_line(&self) -> String {
        self.status_message.clone()
    }

    /// How wide the status reading is asking its slot to be, in device pixels.
    ///
    /// The slot has a resting width that fits a time and a save state. A
    /// notice is a sentence, and a sentence cut off partway is not one, so
    /// the row is measured against what it is actually holding.
    fn status_want(&self) -> i32 {
        let line = self.status_line();
        if line.is_empty() || self.status.is_invalid() {
            return 0;
        }
        // SAFETY: the status static is a live child; the DC is released on
        // every path out of this function.
        let dc = unsafe { GetDC(Some(self.status)) };
        if dc.is_invalid() {
            return 0;
        }
        let restore = select_font(dc, self.fonts.small);
        let width = text_width(dc, &line);
        if let Some(restore) = restore {
            // SAFETY: `restore` is the object the DC held before `select_font`.
            unsafe {
                SelectObject(dc, restore);
            }
        }
        // SAFETY: pairs with the `GetDC` above.
        unsafe {
            ReleaseDC(Some(self.status), dc);
        }
        width.saturating_add(scaled(STATUS_SLACK_96, self.fonts.dpi))
    }

    /// Reports a state the memo is in. It stays until the next one arrives.
    fn set_status(&mut self, message: String) {
        self.status_message = message;
        self.status_notice = false;
        if !self.window.is_invalid() {
            // SAFETY: the pad's own window; cancelling a timer that was never
            // set is not an error.
            unsafe {
                let _ = KillTimer(Some(self.window), PAD_NOTICE_TIMER);
            }
        }
    }

    /// Reports something that just happened.
    ///
    /// A notice has no successor to replace it, so it is given an expiry and
    /// the row goes back to the memo afterwards. Without one, the notice a
    /// copy leaves behind sat where the memo's last-changed time belongs for
    /// the rest of the session.
    fn notify(&mut self, message: String) {
        self.status_message = message;
        self.status_notice = true;
        if !self.window.is_invalid() {
            // SAFETY: the pad's own window, live for as long as this state.
            unsafe {
                let _ = SetTimer(Some(self.window), PAD_NOTICE_TIMER, NOTICE_MS, None);
            }
        }
    }

    fn update_status(&self) {
        if !self.count.is_invalid() {
            // SAFETY: the body edit is a live child.
            let characters = unsafe { GetWindowTextLengthW(self.body) }.max(0);
            set_control_text(self.count, &format!("{characters}字"));
        }
        if self.status.is_invalid() {
            return;
        }
        set_control_text(self.status, &self.status_line());
        // A reading its slot cannot hold is not a reading. Re-arranging the
        // row is worth doing only when the width it asks for has moved.
        if self.status_slot.get() != self.status_want() && !self.window.is_invalid() {
            update_layout(self, self.window);
        }
    }

    /// Writes what the editor controls hold back into the active memo.
    ///
    /// Returns whether the document changed, which is what decides if the row
    /// order is allowed to move: reordering the list on every keystroke would
    /// pull the row the user is reading out from under them.
    fn capture_controls(&mut self) -> bool {
        if self.updating_controls || self.title.is_invalid() {
            return false;
        }
        let title = get_control_text(self.title, MAX_TITLE_UTF16_UNITS);
        let body = get_control_text(self.body, MAX_BODY_UTF16_UNITS);
        if self.document.find(self.active).is_none() && title.is_empty() && body.is_empty() {
            // An untouched new memo is not a memo yet.
            return false;
        }
        let now = now_ms();
        let active = self.active;
        let outcome = self.document.entry(active, now).map(|memo| {
            if memo.title == title && memo.body == body {
                false
            } else {
                memo.edit(title, body, now);
                true
            }
        });
        match outcome {
            Ok(changed) => changed,
            Err(error) => {
                self.set_status(format!("保存できません ({error})"));
                self.update_status();
                false
            }
        }
    }

    /// Hands the whole document to the storage worker under a new generation.
    fn publish(&mut self, window: HWND) {
        if self.save_blocked {
            self.set_status("既存データを保護するため保存を停止しています".to_owned());
            self.update_status();
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.document.generation = self.generation;
        self.latest_submitted = self.generation;
        if self.worker.submit(self.document.clone()) {
            self.set_status("保存中…".to_owned());
            // SAFETY: the window is live for the lifetime of this state.
            unsafe {
                let _ = SetTimer(Some(window), PAD_COMPLETION_TIMER, 100, None);
            }
        } else {
            self.set_status("保存要求を送信できませんでした".to_owned());
        }
        self.update_status();
    }

    /// Returns true once the latest submitted generation has a terminal
    /// completion and the polling timer can stop.
    fn poll_storage(&mut self) -> bool {
        let mut terminal = false;
        while let Some(completion) = self.worker.try_completion() {
            if completion.generation != self.latest_submitted {
                continue;
            }
            // Saving is the resting state, and a row that reports it
            // reports nothing while hiding the memo's own last-changed time.
            // Only the failure is news.
            let reached = match completion.status {
                SaveStatus::Written(_) => String::new(),
                SaveStatus::Failed => "保存に失敗しました。以前のメモは保持されています".to_owned(),
            };
            self.set_status(reached);
            terminal = true;
        }
        self.update_status();
        terminal
    }

    /// Rebuilds the rows when an edit changed which memos the list shows, or
    /// the order it shows them in.
    ///
    /// The first memo of a fresh pad is the case this exists for: it has no
    /// row until it has text, because typing is what creates it. Until the
    /// rows are rebuilt the list stays empty while the editor holds the
    /// writing, which reads as "this did not save" even though it did.
    fn sync_rows(&mut self) {
        if pad_list::rows(&self.document, &self.query) != self.rows {
            self.refresh_list();
        }
    }

    fn mark_dirty(&mut self, window: HWND) {
        if self.save_blocked {
            self.set_status("既存データを保護するため保存を停止しています".to_owned());
            self.update_status();
            return;
        }
        self.set_status("保存中…".to_owned());
        // SAFETY: the window is live for the lifetime of this state.
        unsafe {
            // A short UI-side debounce. The title and body snapshot is taken
            // once typing pauses, never for each individual key event.
            let _ = SetTimer(Some(window), PAD_EDIT_TIMER, 100, None);
        }
        self.update_status();
        self.invalidate_rows();
    }

    /// Repaints the rows without disturbing their order.
    fn invalidate_rows(&self) {
        if self.list.is_invalid() {
            return;
        }
        // SAFETY: the list is a live child of the pad.
        unsafe {
            let _ = InvalidateRect(Some(self.list), None, false);
        }
    }

    fn set_pane(&mut self, pane: PadPane, window: HWND, focus: HWND) {
        if self.pane == pane {
            return;
        }
        self.pane = pane;
        update_layout(self, window);
        // SAFETY: the window and control are live children of this pad.
        unsafe {
            let _ = InvalidateRect(Some(window), None, true);
            if !focus.is_invalid() {
                let _ = SetFocus(Some(focus));
            }
        }
    }

    fn toggle_pane(&mut self, window: HWND) {
        if is_wide(window) {
            return;
        }
        let (next, focus) = match self.pane {
            PadPane::List => (PadPane::Editor, self.body),
            PadPane::Editor => (PadPane::List, self.list),
        };
        self.set_pane(next, window, focus);
    }

    fn select_row(&mut self, index: usize, open: bool, window: HWND) {
        let Some(id) = self.rows.get(index).copied() else {
            return;
        };
        let changed = if id == self.active {
            false
        } else {
            let changed = self.capture_controls();
            self.active = id;
            self.refresh_editor();
            changed
        };
        if changed {
            // The memo just left behind may belong somewhere else now. This
            // is the moment to move it: the user is no longer reading it.
            self.refresh_list();
        }
        self.update_status();
        self.invalidate_rows();
        if open {
            self.set_pane(PadPane::Editor, window, self.body);
        }
    }

    fn create_memo(&mut self, window: HWND) {
        if self.save_blocked {
            self.set_status("既存データを保護するため新規作成を保存できません".to_owned());
            self.update_status();
            return;
        }
        if self.document.memos.len() >= MAX_MEMOS {
            self.notify(format!("上限{MAX_MEMOS}件です"));
            self.update_status();
            return;
        }
        self.capture_controls();
        let id = self.document.next_id();
        self.document.memos.push(PadMemo::new(id, "", "", now_ms()));
        self.active = id;
        self.query.clear();
        self.updating_controls = true;
        set_control_text(self.search, "");
        self.updating_controls = false;
        self.refresh_list();
        self.refresh_editor();
        self.publish(window);
        self.set_pane(PadPane::Editor, window, self.title);
        // SAFETY: the title edit is a live child; the wide shape shows it
        // without a pane change, which `set_pane` would have skipped.
        unsafe {
            let _ = SetFocus(Some(self.title));
        }
    }

    fn delete_memo(&mut self, window: HWND) {
        if self.save_blocked {
            self.set_status("既存データを保護するため削除を保存できません".to_owned());
            self.update_status();
            return;
        }
        // SAFETY: the edit timer belongs to this window.
        unsafe {
            let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
        }
        let position = self.rows.iter().position(|id| *id == self.active);
        let existed = self
            .document
            .find_mut(self.active)
            .map(|memo| memo.retire(now_ms()))
            .is_some();
        self.updating_controls = true;
        set_control_text(self.title, "");
        set_control_text(self.body, "");
        self.updating_controls = false;
        self.active = self.document.next_id();
        self.refresh_list();
        // Land on whatever took the deleted row's place, or the row before it
        // when the deleted memo was last.
        let replacement = position
            .and_then(|position| {
                self.rows
                    .get(position.min(self.rows.len().saturating_sub(1)))
            })
            .copied();
        if let Some(id) = replacement {
            self.active = id;
            self.refresh_editor();
            self.refresh_list();
        }
        if existed {
            self.publish(window);
        } else {
            self.update_status();
        }
    }

    fn cycle_sort(&mut self, window: HWND) {
        let changed = self.capture_controls();
        self.document.sort = self.document.sort.next();
        self.refresh_list();
        self.notify(format!("{}順にしました", self.document.sort.label()));
        if changed || !self.save_blocked {
            self.publish(window);
        } else {
            self.update_status();
        }
        // SAFETY: the sort button is a live child and carries its order as
        // its label.
        unsafe {
            let _ = InvalidateRect(Some(self.sort), None, false);
        }
    }

    fn search_changed(&mut self) {
        if self.updating_controls {
            return;
        }
        self.query = get_control_text(self.search, MAX_QUERY_UTF16_UNITS);
        self.refresh_list();
        self.invalidate_rows();
    }

    fn copy_memo(&mut self, window: HWND) {
        self.capture_controls();
        let Some(memo) = self.document.find(self.active) else {
            self.notify("メモがありません".to_owned());
            self.update_status();
            return;
        };
        let markdown = if memo.title.trim().is_empty() {
            memo.body.clone()
        } else {
            format!("# {}\n\n{}", memo.title, memo.body)
        };
        self.notify(if copy_to_clipboard(window, &markdown) {
            "コピーしました".to_owned()
        } else {
            "コピーできません".to_owned()
        });
        self.update_status();
    }
}

/// The Pad window procedure. Every unhandled message reaches DefWindowProcW,
/// including the default non-client cleanup path.
extern "system" fn pad_procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: WM_NCCREATE supplies a valid CREATESTRUCTW pointer and the
        // caller keeps the Box alive until the window is destroyed.
        let create = unsafe { &*(l.0 as *const CREATESTRUCTW) };
        let state = create.lpCreateParams as *mut PadState;
        // SAFETY: the window is live and the slot is the documented user slot.
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, state as isize);
        }
    }
    // SAFETY: reads the stable pointer installed above; null is checked at
    // every use below.
    let state_ptr = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut PadState;
    match message {
        WM_CREATE if !state_ptr.is_null() => {
            // SAFETY: the state is the live Box retained by PadWindow.
            let state = unsafe { &mut *state_ptr };
            state.apply_caption_icons(window);
            pad_caption::dress(window, state.theme);
            if create_controls(state, window).is_err() {
                // SAFETY: posting to the window being created is legal and the
                // negative result aborts creation.
                unsafe {
                    let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                return LRESULT(-1);
            }
            LRESULT(0)
        }
        WM_SIZE if !state_ptr.is_null() => {
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            update_layout(state, window);
            // SAFETY: the window is live.
            unsafe {
                let _ = InvalidateRect(Some(window), None, true);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            // WM_PAINT fills the whole client, so erasing first would only
            // flash the old color under the new one.
            LRESULT(1)
        }
        WM_PAINT if !state_ptr.is_null() => {
            // SAFETY: as above.
            let state = unsafe { &*state_ptr };
            paint(window, state);
            LRESULT(0)
        }
        WM_MEASUREITEM if l.0 != 0 => {
            // SAFETY: USER32 provides a writable MEASUREITEMSTRUCT.
            let item = unsafe { &mut *(l.0 as *mut MEASUREITEMSTRUCT) };
            if item.CtlType == ODT_LISTBOX {
                item.itemHeight = scaled(pad_list::ROW_HEIGHT_96, dpi_of(window)).max(1) as u32;
            }
            LRESULT(1)
        }
        WM_DRAWITEM if !state_ptr.is_null() && l.0 != 0 => {
            // SAFETY: USER32 provides a live DRAWITEMSTRUCT for this message
            // and the state box outlives the window.
            let item = unsafe { &*(l.0 as *const DRAWITEMSTRUCT) };
            // SAFETY: the state box outlives the window that is drawing.
            let state = unsafe { &*state_ptr };
            let colors = palette(state.theme);
            let dpi = dpi_of(window);
            // SAFETY: the item DC is valid for this callback.
            unsafe {
                let _ = SetBkMode(item.hDC, TRANSPARENT);
            }
            if item.CtlType == ODT_BUTTON {
                let id = item.CtlID as u16;
                let wide = is_wide(window);
                if let Some(face) = button_face(id, wide) {
                    // Every control in the window stands on the window's own
                    // chrome, including the two in the editor's head row.
                    let ground = colors.surface;
                    // Only the one control that carries a word needs a font,
                    // and it is set in the supporting size beside its icon.
                    let previous = select_font(item.hDC, state.fonts.small);
                    draw_button(
                        item,
                        face,
                        button_shape(id, wide),
                        destructive(id, wide),
                        ground,
                        colors,
                        dpi,
                    );
                    if let Some(previous) = previous {
                        // SAFETY: restores the object the DC arrived with.
                        unsafe {
                            let _ = windows::Win32::Graphics::Gdi::SelectObject(item.hDC, previous);
                        }
                    }
                }
            } else if item.CtlType == ODT_LISTBOX && item.itemID != u32::MAX {
                draw_row(item, state, colors, dpi);
            }
            LRESULT(1)
        }
        WM_COMMAND if !state_ptr.is_null() => {
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            let id = (w.0 & 0xffff) as u16;
            let code = ((w.0 >> 16) & 0xffff) as u16;
            match (id, code) {
                (TITLE_ID | BODY_ID, value) if value == EN_CHANGE as u16 => {
                    if !state.updating_controls {
                        state.mark_dirty(window);
                    }
                }
                (SEARCH_ID, value) if value == EN_CHANGE as u16 => state.search_changed(),
                (LIST_ID, value) if value == LBN_SELCHANGE as u16 => {
                    // A click carries the button down through this
                    // notification; the keyboard does not. In the one-pane
                    // shape that difference is what lets the arrow keys walk
                    // the list without the editor swallowing the window.
                    // SAFETY: no arguments; reads this thread's key state.
                    let by_mouse = unsafe { GetKeyState(VK_LBUTTON.0 as i32) } < 0;
                    let index = selected_row(state.list);
                    if let Some(index) = index {
                        state.select_row(index, by_mouse, window);
                    }
                }
                (LIST_ID, value) if value == LBN_DBLCLK as u16 => {
                    if let Some(index) = selected_row(state.list) {
                        state.select_row(index, true, window);
                    }
                }
                (MENU_ID, value) if value == BN_CLICKED as u16 => state.toggle_pane(window),
                (NEW_ID, value) if value == BN_CLICKED as u16 => state.create_memo(window),
                (SORT_ID, value) if value == BN_CLICKED as u16 => state.cycle_sort(window),
                (DELETE_ID, value) if value == BN_CLICKED as u16 => state.delete_memo(window),
                (COPY_ID, value) if value == BN_CLICKED as u16 => state.copy_memo(window),
                (SYNC_ID, value) if value == BN_CLICKED as u16 => {
                    state.notify("GitHub 未設定".to_owned());
                    state.update_status();
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER if !state_ptr.is_null() && w.0 == PAD_EDIT_TIMER => {
            // SAFETY: the timer belongs to this window.
            unsafe {
                let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
            }
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            if state.capture_controls() {
                state.sync_rows();
                state.publish(window);
            } else {
                state.update_status();
            }
            LRESULT(0)
        }
        WM_TIMER if !state_ptr.is_null() && w.0 == PAD_NOTICE_TIMER => {
            // SAFETY: the timer belongs to this window.
            unsafe {
                let _ = KillTimer(Some(window), PAD_NOTICE_TIMER);
            }
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            // A state that arrived while the notice stood is the newer fact
            // and keeps the row; only the notice itself expires.
            if state.status_notice {
                state.set_status(String::new());
                state.update_status();
            }
            LRESULT(0)
        }
        WM_TIMER if !state_ptr.is_null() && w.0 == PAD_COMPLETION_TIMER => {
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            if state.poll_storage() {
                // SAFETY: the timer belongs to this window.
                unsafe {
                    let _ = KillTimer(Some(window), PAD_COMPLETION_TIMER);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: the window is live.
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO if l.0 > 0 => {
            // SAFETY: USER32 provides a writable MINMAXINFO for this message.
            let info =
                unsafe { &mut *(l.0 as *mut windows::Win32::UI::WindowsAndMessaging::MINMAXINFO) };
            let dpi = dpi_of(window);
            info.ptMinTrackSize.x = scaled(PAD_MIN_WIDTH_LOGICAL, dpi);
            info.ptMinTrackSize.y = scaled(PAD_MIN_HEIGHT_LOGICAL, dpi);
            LRESULT(0)
        }
        WM_DPICHANGED if !state_ptr.is_null() => {
            if l.0 != 0 {
                // SAFETY: WM_DPICHANGED lParam points to a suggested RECT for
                // the duration of this message.
                let suggested = unsafe { &*(l.0 as *const RECT) };
                // SAFETY: the window is live.
                unsafe {
                    let _ = SetWindowPos(
                        window,
                        None,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            state.apply_dpi(dpi_of(window));
            // The caption is drawn at the new scale too, and it is drawn from
            // whichever of the icon's ten sizes fits it.
            state.apply_caption_icons(window);
            update_layout(state, window);
            // SAFETY: the window is live.
            unsafe {
                let _ = InvalidateRect(Some(window), None, true);
            }
            LRESULT(0)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX
            if !state_ptr.is_null() =>
        {
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            state.refresh_brushes();
            let colors = palette(state.theme);
            let child = HWND(l.0 as *mut c_void);
            // Three grounds: the writing area is ruled paper; the search
            // field is the chip drawn under it; everything else — the head
            // row included, in either shape — is the window's own chrome.
            let writing = child == state.body;
            let (brush, back) = if writing {
                (state.brushes.writing(), colors.paper)
            } else if child == state.search {
                (state.brushes.chip, colors.selected)
            } else {
                (state.brushes.surface, colors.surface)
            };
            // The meta line is support text, the same rank the candidate
            // popup gives an annotation.
            let ink = if child == state.status {
                colors.annotation
            } else {
                colors.ink
            };
            // SAFETY: USER32 passes the child control HDC in wParam for all
            // WM_CTLCOLOR* messages; the brush remains owned by the state.
            unsafe {
                let dc = HDC(w.0 as *mut c_void);
                // Opaque text would paint a solid box over every rule it
                // crossed, so words on ruled paper are drawn through.
                SetBkMode(dc, if writing { TRANSPARENT } else { OPAQUE });
                let _ = SetBkColor(dc, back);
                let _ = SetTextColor(dc, ink);
            }
            LRESULT(brush.0 as isize)
        }
        WM_THEMECHANGED | WM_SETTINGCHANGE if !state_ptr.is_null() => {
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            state.refresh_brushes();
            // Turning high contrast on has to hand the title bar back to
            // Windows, which is a thing only this call does.
            pad_caption::dress(window, state.theme);
            // SAFETY: the window is live.
            unsafe {
                let _ = InvalidateRect(Some(window), None, true);
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        WM_NCDESTROY => {
            // SAFETY: the window is being destroyed; clearing the slot stops
            // any later message from following a dangling pointer.
            unsafe {
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            }
            // SAFETY: the default handler must see this message.
            unsafe { DefWindowProcW(window, message, w, l) }
        }
        // SAFETY: the default handler is where every unhandled message must
        // go, with the arguments this procedure was given.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

fn selected_row(list: HWND) -> Option<usize> {
    // SAFETY: the list is a live child of the pad.
    let index = unsafe {
        SendMessageW(
            list,
            windows::Win32::UI::WindowsAndMessaging::LB_GETCURSEL,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        )
    };
    (index.0 >= 0).then_some(index.0 as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPIS: [u32; 3] = [96, 144, 192];

    /// The widths a status reading can ask its slot for: nothing to say, the
    /// resting one, a sentence, and more than the window has.
    fn wants(dpi: u32) -> [i32; 4] {
        [0, 1, scaled(220, dpi), 4_000]
    }

    fn width_of(rect: RECT) -> i32 {
        rect.right.saturating_sub(rect.left)
    }

    fn client(width_96: i32, height_96: i32, dpi: u32) -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: scaled(width_96, dpi),
            bottom: scaled(height_96, dpi),
        }
    }

    /// Every rectangle a control is actually placed at. Containers are checked
    /// separately: they are meant to hold the leaves, not to avoid them.
    fn leaves(plan: &PadLayout) -> Vec<(&'static str, RECT)> {
        let mut leaves = vec![
            ("copy", plan.copy),
            ("delete", plan.delete),
            ("new", plan.new),
            ("sort", plan.sort),
            ("sync", plan.sync),
        ];
        for (name, rect) in [
            ("menu", plan.menu),
            ("header_title", plan.header_title),
            ("status", plan.status),
            ("count", plan.count),
            ("search", plan.search),
            ("list", plan.list),
            ("divider", plan.divider),
            ("title", plan.title),
            ("body", plan.body),
        ] {
            if let Some(rect) = rect {
                leaves.push((name, rect));
            }
        }
        leaves
    }

    fn overlaps(a: RECT, b: RECT) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    fn contains(outer: RECT, inner: RECT) -> bool {
        inner.left >= outer.left
            && inner.right <= outer.right
            && inner.top >= outer.top
            && inner.bottom <= outer.bottom
    }

    /// A notice clipped partway is not a notice — `Markdown をコピーしました`
    /// arrived as `Markdown をコピ`. Notices are phrases now rather than
    /// sentences, and the slot still grows into the gap the title is not
    /// using — stopping there, because a heading squeezed into a stub is the
    /// same failure the other way round.
    #[test]
    fn a_long_reading_widens_the_status_slot_but_never_starves_the_title() {
        for dpi in DPIS {
            for width in [520, 640, 1280] {
                for pane in [PadPane::List, PadPane::Editor] {
                    let area = client(width, 520, dpi);
                    // One device pixel of reading: something to say, and a
                    // slot no narrower than the one a time and a save state
                    // were sized for.
                    let resting = layout(area, dpi, pane, 1);
                    let Some(resting_status) = resting.status else {
                        continue;
                    };
                    assert_eq!(
                        width_of(resting_status),
                        scaled(STATUS_WIDTH_96, dpi),
                        "a time and a save state get the resting width at {width} @ {dpi}"
                    );

                    let sentence = scaled(220, dpi);
                    let widened = layout(area, dpi, pane, sentence);
                    let status = widened.status.expect("the slot does not disappear");
                    assert!(
                        width_of(status) >= width_of(resting_status),
                        "a measured reading never gets less than the resting width                          at {width} @ {dpi}"
                    );
                    assert!(
                        width_of(status) <= sentence,
                        "and never more than it asked for at {width} @ {dpi}"
                    );
                    assert_eq!(
                        status.right, resting_status.right,
                        "the slot grows leftward: its right edge is fixed"
                    );
                    if width == 1280 {
                        assert_eq!(
                            width_of(status),
                            sentence,
                            "a window with room grants the whole request at {dpi}"
                        );
                    }

                    // Asking for more than the window holds is answered with
                    // what the title can spare, not with the title's own room.
                    let greedy = layout(area, dpi, pane, 4_000);
                    let title = greedy.title.expect("the two-pane row always has a title");
                    assert!(
                        width_of(title) >= scaled(TITLE_MIN_96, dpi),
                        "the heading keeps its minimum at {width} @ {dpi} ({pane:?})"
                    );
                    if let Some(count) = resting.count {
                        assert_eq!(
                            greedy.count.map(width_of),
                            Some(width_of(count)),
                            "the length the writer is watching keeps its place"
                        );
                    }
                }
            }
        }
    }

    /// A row with nothing to report says nothing, and a slot held open for a
    /// reading that is not there is a gap the name could have used.
    #[test]
    fn a_silent_row_gives_its_slot_back_to_the_name() {
        for dpi in DPIS {
            for width in [360, 519, 520, 640, 1280] {
                for pane in [PadPane::List, PadPane::Editor] {
                    let area = client(width, 520, dpi);
                    let silent = layout(area, dpi, pane, 0);
                    assert!(
                        silent.status.is_none(),
                        "an empty reading keeps no slot at {width} @ {dpi} ({pane:?})"
                    );
                    let speaking = layout(area, dpi, pane, 1);
                    if let (Some(silent_title), Some(speaking_title)) =
                        (silent.title, speaking.title)
                    {
                        assert!(
                            width_of(silent_title) >= width_of(speaking_title),
                            "the name takes the room the reading is not using at \
                             {width} @ {dpi} ({pane:?})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn pad_window_constants_keep_requested_logical_geometry() {
        assert_eq!(PAD_WIDTH_LOGICAL, 640);
        assert_eq!(PAD_HEIGHT_LOGICAL, 520);
        assert_eq!(PAD_MIN_WIDTH_LOGICAL, 480);
        assert_eq!(PAD_MIN_HEIGHT_LOGICAL, 360);
    }

    #[test]
    fn utf16_control_limits_are_bounded() {
        assert_eq!(MAX_TITLE_UTF16_UNITS, 256);
        assert_eq!(MAX_BODY_UTF16_UNITS, 65_536);
    }

    /// The one behavior the whole responsive design rests on: the shape
    /// changes at 520 logical pixels of client width, and at every DPI.
    #[test]
    fn the_two_pane_shape_begins_exactly_at_the_breakpoint() {
        for dpi in DPIS {
            for pane in [PadPane::List, PadPane::Editor] {
                assert!(
                    !layout(client(519, 400, dpi), dpi, pane, 0).wide,
                    "519 logical px at {dpi} DPI must stay one pane"
                );
                assert!(
                    layout(client(520, 400, dpi), dpi, pane, 0).wide,
                    "520 logical px at {dpi} DPI must be two panes"
                );
                assert!(
                    layout(client(521, 400, dpi), dpi, pane, 0).wide,
                    "521 logical px at {dpi} DPI must be two panes"
                );
            }
        }
    }

    /// A control that overlaps another is a control the user cannot read or
    /// click. Exhaustive over the shapes, the boundary and every shipped DPI.
    #[test]
    fn no_two_controls_overlap_at_any_dpi_or_width() {
        for dpi in DPIS {
            for width in [480, 519, 520, 521, 640, 1280] {
                for height in [360, 520, 900] {
                    for pane in [PadPane::List, PadPane::Editor] {
                        for want in wants(dpi) {
                            let plan = layout(client(width, height, dpi), dpi, pane, want);
                            let placed = leaves(&plan);
                            for (index, (name, rect)) in placed.iter().enumerate() {
                                assert!(
                                    !is_empty(*rect),
                                    "{name} is empty at {width}x{height} @ {dpi} ({pane:?}/{want})"
                                );
                                for (other, other_rect) in &placed[index + 1..] {
                                    assert!(
                                        !overlaps(*rect, *other_rect),
                                        "{name} overlaps {other} at {width}x{height} @ {dpi} ({pane:?}/{want})"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Nothing may be drawn or placed outside the client area: a clipped
    /// control is as unusable as an overlapped one.
    #[test]
    fn every_control_stays_inside_the_client_area() {
        for dpi in DPIS {
            for width in [480, 520, 640, 1280] {
                for pane in [PadPane::List, PadPane::Editor] {
                    for want in wants(dpi) {
                        let area = client(width, 520, dpi);
                        let plan = layout(area, dpi, pane, want);
                        for (name, rect) in leaves(&plan) {
                            assert!(
                                contains(area, rect),
                                "{name} leaves the client at {width} @ {dpi} ({pane:?})"
                            );
                            assert!(
                                contains(area, field_child(rect, dpi)),
                                "{name}'s field child leaves the client at {width} @ {dpi}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The one-pane shape shows one pane and offers the way back to the other;
    /// the two-pane shape shows both and has no use for a toggle.
    #[test]
    fn each_shape_shows_exactly_the_panes_it_promises() {
        for dpi in DPIS {
            let narrow_list = layout(client(500, 520, dpi), dpi, PadPane::List, 0);
            assert!(narrow_list.header.is_some() && narrow_list.menu.is_some());
            assert!(narrow_list.search.is_some() && narrow_list.list.is_some());
            assert!(narrow_list.title.is_none() && narrow_list.body.is_none());
            assert!(narrow_list.divider.is_none());
            assert!(
                narrow_list.header_title.is_some(),
                "with no memo open the header says which list this is"
            );

            let narrow_editor = layout(client(500, 520, dpi), dpi, PadPane::Editor, 0);
            assert!(narrow_editor.header.is_some() && narrow_editor.menu.is_some());
            assert!(narrow_editor.search.is_none() && narrow_editor.list.is_none());
            assert!(narrow_editor.title.is_some() && narrow_editor.body.is_some());
            assert!(
                narrow_editor.header_title.is_none(),
                "the open memo's own title takes the header, so the list name                  does not also claim it"
            );

            assert!(
                layout(client(500, 520, dpi), dpi, PadPane::Editor, 1)
                    .status
                    .is_some(),
                "the folded editor has somewhere to report from"
            );
            let roomy = layout(client(1280, 520, dpi), dpi, PadPane::Editor, 1);
            assert!(
                roomy.status.is_some() && roomy.count.is_some(),
                "given room, the editor's row carries both readings"
            );

            for pane in [PadPane::List, PadPane::Editor] {
                let wide = layout(client(720, 520, dpi), dpi, pane, 0);
                assert!(wide.menu.is_none(), "a resident list needs no toggle");
                assert!(
                    wide.header.is_none() && wide.header_title.is_none(),
                    "reaching every memo from the list beside it leaves the                      window's own caption as the only chrome above the panes"
                );
                assert!(wide.search.is_some() && wide.list.is_some());
                assert!(wide.title.is_some() && wide.body.is_some());
                assert!(wide.meta.is_some(), "the editor carries its own first row");
                assert!(wide.divider.is_some());
            }
        }
    }

    /// Anything the pad draws a face on needs words for the pointer too: an
    /// icon alone is exactly what the owner could not read.
    #[test]
    fn every_drawn_face_has_hover_text() {
        for id in [MENU_ID, NEW_ID, SORT_ID, SYNC_ID, COPY_ID, DELETE_ID] {
            assert!(button_face(id, false).is_some(), "{id} has no face");
            let text = hint(id).unwrap_or_else(|| panic!("{id} has no hint"));
            // A tip repeating the button's own one-word name teaches nothing
            // that looking at the control did not already show.
            assert!(text.chars().count() > 4, "{text}");
        }
        for id in [SEARCH_ID, LIST_ID, TITLE_ID, BODY_ID, STATUS_ID] {
            assert_eq!(hint(id), None, "{id} is not a drawn face");
        }
    }

    /// The one control the owner misread has to name the format it produces,
    /// in both the words it carries and the words it shows.
    #[test]
    fn the_copy_control_says_markdown() {
        let text = hint(COPY_ID).expect("the copy control has hover text");
        assert!(text.contains("Markdown"), "{text}");
        assert!(text.contains("コピー"), "{text}");
        assert_eq!(
            button_face(COPY_ID, false).map(|face| face.icon),
            Some(PadIcon::Copy)
        );
    }

    /// Where the memo's own controls live is the difference between the two
    /// shapes: beside the memo when there is room, in the bar when there is not.
    #[test]
    fn copying_and_deleting_follow_the_memo_between_the_shapes() {
        for dpi in DPIS {
            let wide = layout(client(720, 520, dpi), dpi, PadPane::List, 0);
            let meta = wide.meta.expect("the two-pane editor carries a first row");
            assert!(contains(meta, wide.copy));
            assert!(contains(meta, wide.delete));
            assert!(
                wide.bottom.right <= wide.list_rail.expect("a list has a rail").right,
                "the bar belongs to the list column in the two-pane shape"
            );

            let narrow = layout(client(500, 520, dpi), dpi, PadPane::List, 0);
            assert!(contains(narrow.bottom, narrow.copy));
            assert!(contains(narrow.bottom, narrow.delete));
            assert!(
                narrow.delete.left > narrow.copy.right,
                "delete is the far end of the bar, away from the rest"
            );
        }
    }

    /// The bar, the header and the meta row always hold their own controls.
    #[test]
    fn every_band_contains_the_controls_it_owns() {
        for dpi in DPIS {
            for width in [480, 520, 1280] {
                for pane in [PadPane::List, PadPane::Editor] {
                    let plan = layout(client(width, 520, dpi), dpi, pane, 0);
                    // The header exists only in the folded shape, and whatever
                    // it holds belongs to it.
                    for (name, rect) in [("menu", plan.menu), ("header_title", plan.header_title)] {
                        let Some(rect) = rect else { continue };
                        let header = plan.header.expect("a header control needs a header");
                        assert!(contains(header, rect), "{name} escapes the header");
                    }
                    assert_eq!(
                        plan.header.is_some(),
                        !plan.wide,
                        "only the folded shape needs chrome above the panes"
                    );
                    assert!(
                        plan.status.is_none() || plan.wide || pane == PadPane::Editor,
                        "the status line describes the open memo, so it never \
                         shows without one"
                    );
                    assert!(
                        plan.count.is_none() || plan.wide,
                        "the folded shape has no room for a length beside the \
                         title, and the bar is not where a measurement belongs"
                    );
                    // Whatever the supports do, the heading keeps its minimum.
                    if plan.status.is_some() || plan.count.is_some() {
                        let title = plan.title.expect("a support implies a heading");
                        assert!(
                            title.right - title.left >= scaled(TITLE_MIN_96, dpi),
                            "the heading was squeezed below its minimum at \
                             {width} @ {dpi} ({pane:?})"
                        );
                    }
                    // Where the memo's own row lives moves with the shape: the
                    // editor's first row when there is one, the header when not.
                    let row = plan.meta.or(plan.header);
                    for (name, rect) in [
                        ("status", plan.status),
                        ("count", plan.count),
                        ("title", plan.meta.and(plan.title)),
                    ] {
                        let Some(rect) = rect else { continue };
                        let row = row.expect("a memo row needs a band");
                        assert!(contains(row, rect), "{name} escapes the memo row");
                    }
                    for (name, rect) in
                        [("new", plan.new), ("sort", plan.sort), ("sync", plan.sync)]
                    {
                        assert!(contains(plan.bottom, rect), "{name} escapes the bar");
                    }
                }
            }
        }
    }

    /// The pad paints a rule at the bottom of every band it owns; the children
    /// must not cover it. A child that overlaps a painted edge erases exactly
    /// its own span, which is how a full-width line ends up looking like two
    /// short ones.
    #[test]
    fn no_control_covers_the_rules_the_pad_paints() {
        for dpi in [96, 120, 144, 192] {
            let border = scaled(BORDER_96, dpi).max(1);
            for width in [480, 519, 520, 640, 1280] {
                for pane in [PadPane::List, PadPane::Editor] {
                    let client = RECT {
                        left: 0,
                        top: 0,
                        right: scaled(width, dpi),
                        bottom: scaled(600, dpi),
                    };
                    let plan = layout(client, dpi, pane, 0);
                    for (band, occupants) in [
                        (
                            plan.header,
                            vec![("menu", plan.menu), ("header title", plan.header_title)],
                        ),
                        (
                            plan.meta,
                            vec![
                                ("title", plan.meta.and(plan.title)),
                                ("status", plan.status),
                                ("count", plan.count),
                                ("copy", plan.meta.map(|_| plan.copy)),
                                ("delete", plan.meta.map(|_| plan.delete)),
                            ],
                        ),
                    ] {
                        let Some(band) = band else { continue };
                        let rule_top = band.bottom - border;
                        for (name, rect) in occupants {
                            let Some(rect) = rect else { continue };
                            assert!(
                                rect.bottom <= rule_top,
                                "{name} reaches {} into the rule at {rule_top} \
                                 (dpi {dpi}, width {width}, {pane:?})",
                                rect.bottom
                            );
                        }
                    }
                    // The bar's rule is painted on its top edge instead.
                    let rule_bottom = plan.bottom.top + border;
                    for (name, rect) in
                        [("new", plan.new), ("sort", plan.sort), ("sync", plan.sync)]
                    {
                        assert!(
                            rect.top >= rule_bottom,
                            "{name} reaches {} into the bar rule at {rule_bottom} \
                             (dpi {dpi}, width {width}, {pane:?})",
                            rect.top
                        );
                    }
                }
            }
        }
    }

    /// The hint is the field's resting state. It must not survive a caret
    /// arriving or a character being typed, or it would sit under both.
    /// Paper is where the user's own words go. It is exactly as large as the
    /// editor and never reaches under the chrome around it, because the ruled
    /// squares showing through a list row or a toolbar would read as damage.
    #[test]
    fn paper_covers_the_editor_and_stops_at_the_chrome() {
        for dpi in DPIS {
            for width in [360, 519, 520, 640, 900] {
                for pane in [PadPane::List, PadPane::Editor] {
                    let plan = layout(client(width, 420, dpi), dpi, pane, 0);
                    let Some(paper) = plan.paper else {
                        assert!(
                            plan.body.is_none(),
                            "the editor is showing on no paper at {width} @ {dpi}"
                        );
                        continue;
                    };
                    let body = plan.body.expect("paper without an editor to hold");
                    assert!(
                        contains(paper, body),
                        "the writing area overruns its paper at {width} @ {dpi}"
                    );
                    if let Some(meta) = plan.meta {
                        assert!(
                            !overlaps(paper, meta),
                            "the head row describes the memo rather than holding it, so \
                             it stands on the chrome at {width} @ {dpi}"
                        );
                    }
                    for band in [plan.header, plan.list, plan.search, plan.divider]
                        .into_iter()
                        .flatten()
                        .chain([plan.bottom])
                    {
                        assert!(
                            !overlaps(paper, band),
                            "paper reaches under the chrome at {width} @ {dpi}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_search_hint_shows_only_while_the_field_is_empty_and_unfocused() {
        assert!(shows_search_placeholder(0, false));
        assert!(!shows_search_placeholder(0, true), "the caret is there");
        assert!(!shows_search_placeholder(1, false), "a query is there");
        assert!(!shows_search_placeholder(1, true));
    }

    /// A minimised or degenerate client must produce empty rectangles, never
    /// inverted ones: `DeferWindowPos` would happily place a negative size.
    #[test]
    fn a_degenerate_client_produces_empty_rectangles_not_inverted_ones() {
        for area in [
            RECT::default(),
            RECT {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            RECT {
                left: 40,
                top: 40,
                right: 30,
                bottom: 20,
            },
        ] {
            for dpi in DPIS {
                for pane in [PadPane::List, PadPane::Editor] {
                    let plan = layout(area, dpi, pane, 0);
                    for (name, rect) in leaves(&plan) {
                        assert!(
                            rect.right >= rect.left && rect.bottom >= rect.top,
                            "{name} inverted for a degenerate client"
                        );
                    }
                }
            }
        }
    }

    /// The pad has no colors of its own. Every one it paints comes from the
    /// shared palette, which is what keeps it and the candidate popup one
    /// product rather than two that resemble each other.
    #[test]
    fn the_pad_paints_only_with_the_shared_palette() {
        // Split so the assertion below is not itself a match.
        let literal = concat!("COLORREF", "(0x");
        let call = concat!("rgb", "(");
        let source = include_str!("pad.rs");
        for (number, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            assert!(
                !code.contains(literal) && !code.contains(call),
                "pad.rs:{} names a color of its own: {line}",
                number + 1
            );
        }
        // And the values themselves are the candidate popup's, not a copy.
        // Resolved from explicit inputs, so a machine with high contrast
        // switched on grades the same as one without.
        assert_eq!(
            crate::theme::resolve_palette(AppearanceTheme::Light, false, true),
            crate::theme::light_palette()
        );
        assert_eq!(
            crate::theme::resolve_palette(AppearanceTheme::Dark, false, true),
            crate::theme::dark_palette()
        );
    }

    /// The sort control cycles and always says which order it is in.
    #[test]
    fn the_sort_control_names_the_order_it_cycles_to() {
        let mut state_sort = crate::pad_storage::PadSort::default();
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(state_sort.label());
            state_sort = state_sort.next();
        }
        assert_eq!(seen, ["更新順", "作成順", "名前順"]);
        assert_eq!(state_sort, crate::pad_storage::PadSort::default());
    }
}
