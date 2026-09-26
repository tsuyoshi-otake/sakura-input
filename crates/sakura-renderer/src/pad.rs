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
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use sakura_pad_session_proto::SecretBytes;
use sakura_proto::AppearanceTheme;
use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, CreatePatternBrush, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, GetDC, InvalidateRect, RedrawWindow,
    ReleaseDC, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, HBRUSH, HDC, HFONT, OPAQUE, PAINTSTRUCT, PS_SOLID,
    RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW, TRANSPARENT,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_GETFIRSTVISIBLELINE, EM_GETLINECOUNT, EM_GETMARGINS, EM_SETLIMITTEXT,
    EM_SETREADONLY, EM_SETSEL, MEASUREITEMSTRUCT, ODS_FOCUS, ODS_SELECTED, ODT_BUTTON, ODT_LISTBOX,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, GetKeyState, SetFocus, VK_A, VK_CONTROL, VK_ESCAPE, VK_LBUTTON,
    VK_MENU, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, CallWindowProcW, CreateWindowExW, DefWindowProcW, DeferWindowPos,
    DestroyWindow, EndDeferWindowPos, FlashWindowEx, GetAncestor, GetClassNameW, GetClientRect,
    GetDlgCtrlID, GetParent, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    IsDialogMessageW, IsIconic, IsWindowVisible, KillTimer, LoadCursorW, MessageBoxW, PostMessageW,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, BN_CLICKED, BS_OWNERDRAW, CREATESTRUCTW, EN_CHANGE, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_LEFT, ES_MULTILINE, ES_NOHIDESEL, ES_PASSWORD, ES_WANTRETURN, FLASHWINFO,
    GA_ROOT, GWLP_USERDATA, GWLP_WNDPROC, HMENU, HWND_TOP, HWND_TOPMOST, IDC_ARROW, IDYES,
    LBN_DBLCLK, LBN_SELCHANGE, LBS_HASSTRINGS, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY,
    LBS_OWNERDRAWFIXED, LB_ADDSTRING, LB_DELETESTRING, LB_GETTOPINDEX, LB_INSERTSTRING,
    LB_RESETCONTENT, LB_SETCURSEL, LB_SETITEMHEIGHT, LB_SETTOPINDEX, MB_ICONWARNING, MB_YESNO, MSG,
    SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE,
    SW_RESTORE, SW_SHOW, WINDOW_EX_STYLE, WM_APP, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_GETFONT, WM_GETMINMAXINFO, WM_GETTEXTLENGTH,
    WM_KEYDOWN, WM_KILLFOCUS, WM_MEASUREITEM, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETFOCUS,
    WM_SETFONT, WM_SETTEXT, WM_SETTINGCHANGE, WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_THEMECHANGED,
    WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSW, WNDPROC, WS_CHILD, WS_CLIPCHILDREN, WS_EX_TOPMOST,
    WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WTS_SESSION_LOCK,
};
use zeroize::{Zeroize, Zeroizing};

use crate::memo_protection::{
    classify_envelope, MemoEnvelopeFormat, MemoError, MemoProtectionSession,
};
use crate::pad_caption;
use crate::pad_crypto_client::PadCryptoCancellation;
use crate::pad_icon::{self, PadIcon};
use crate::pad_list::{self, CalendarTime};
use crate::pad_protection::{
    FailurePhase, FailureReason, PadProtectionEngine, ProtectedLockStatus, ProtectedSaveActor,
    ProtectedSaveStatus, ProtectionError, SubmitRejectReason,
};
use crate::pad_rail;
use crate::pad_storage::{
    now_ms, MemoPayloadV1, PadDocument, PadMemo, PadStore, SaveStatus, StorageError, StorageWorker,
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
const PAD_UNLOCK_RETRY_TIMER: usize = 0x5345;
const PAD_PROTECTED_CLIPBOARD_TIMER: usize = 0x5346;
const PROTECTED_CLIPBOARD_LIFETIME_MS: u32 = 15_000;
const WM_PAD_UNLOCK_FINISHED: u32 = WM_APP + 7;
const WM_PAD_MASK_FOR_SESSION: u32 = WM_APP + 8;
const MAX_PASSWORD_UTF16_UNITS: usize = 256;

fn unlock_retry_delay(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(5);
    Duration::from_secs((1u64 << exponent).min(30))
}
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
const LOCK_HEADLINE_ID: u16 = 117;
const LOCK_PASSWORD_ID: u16 = 118;
const LOCK_UNLOCK_ID: u16 = 119;
const LOCK_STATUS_ID: u16 = 120;
const LOCK_PASSWORD_LABEL_ID: u16 = 121;
const PROTECT_ID: u16 = 122;
const ENROLL_HEADLINE_ID: u16 = 123;
const ENROLL_PASSWORD_LABEL_ID: u16 = 124;
const ENROLL_PASSWORD_ID: u16 = 125;
const ENROLL_CONFIRM_LABEL_ID: u16 = 126;
const ENROLL_CONFIRM_PASSWORD_ID: u16 = 127;
const ENROLL_SUBMIT_ID: u16 = 128;
const ENROLL_CANCEL_ID: u16 = 129;
const ENROLL_STATUS_ID: u16 = 130;
const MEMO_PROTECT_ID: u16 = 131;
const WM_PAD_ENROLL_FINISHED: u32 = WM_APP + 9;
const WM_PAD_MEMO_FINISHED: u32 = WM_APP + 10;
const ENROLL_CONTROL_IDS: [u16; 8] = [
    ENROLL_HEADLINE_ID,
    ENROLL_PASSWORD_LABEL_ID,
    ENROLL_PASSWORD_ID,
    ENROLL_CONFIRM_LABEL_ID,
    ENROLL_CONFIRM_PASSWORD_ID,
    ENROLL_SUBMIT_ID,
    ENROLL_CANCEL_ID,
    ENROLL_STATUS_ID,
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EnrollPhase {
    #[default]
    None,
    Prompt,
    Running,
    MemoProtectPrompt,
    MemoProtectRunning,
    MemoRecoveryPrompt,
    MemoUnlockPrompt,
    MemoUnlockRunning,
}

struct OpenMemo {
    id: u64,
    title: Zeroizing<String>,
    body: Zeroizing<String>,
    session: Option<MemoProtectionSession>,
    dirty: bool,
    revision: u64,
}

struct MemoDraft {
    id: u64,
    title: Zeroizing<String>,
    body: Zeroizing<String>,
    base_envelope: Vec<u8>,
}

impl Drop for OpenMemo {
    fn drop(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.lock();
        }
    }
}

enum MemoTaskResult {
    RecoveryReady(Zeroizing<String>),
    Created(std::result::Result<PadDocument, MemoCreateFailure>),
    Prepared(std::result::Result<PadDocument, MemoError>),
    Unlocked(
        std::result::Result<
            (MemoProtectionSession, Zeroizing<String>, Zeroizing<String>),
            MemoError,
        >,
    ),
    Resealed(std::result::Result<(MemoProtectionSession, Vec<u8>), MemoError>),
}

struct MemoCreateFailure {
    message: String,
    cutover_started: bool,
    legacy_exact: bool,
}

struct MemoTaskCompletion {
    epoch: u64,
    memo_id: u64,
    revision: u64,
    result: MemoTaskResult,
}

fn await_memo_recovery_confirmation(
    sender: &Sender<MemoTaskCompletion>,
    confirmation: &Receiver<bool>,
    raw_window: isize,
    epoch: u64,
    memo_id: u64,
    key: Zeroizing<String>,
) -> bool {
    if sender
        .send(MemoTaskCompletion {
            epoch,
            memo_id,
            revision: 0,
            result: MemoTaskResult::RecoveryReady(key),
        })
        .is_err()
    {
        return false;
    }
    // SAFETY: the UI receiver verifies the epoch and selected memo before
    // displaying the key. A destroyed HWND only makes posting fail.
    unsafe {
        let _ = PostMessageW(
            Some(HWND(raw_window as *mut c_void)),
            WM_PAD_MEMO_FINISHED,
            WPARAM(0),
            LPARAM(0),
        );
    }
    confirmation
        .recv_timeout(Duration::from_secs(600))
        .is_ok_and(|approved| approved)
}

struct EnrollCompletion {
    epoch: u64,
    result: std::result::Result<(), ProtectionError>,
    legacy_exact: bool,
}

impl std::fmt::Debug for EnrollCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollCompletion")
            .field("epoch", &self.epoch)
            .field("success", &self.result.is_ok())
            .field("legacy_exact", &self.legacy_exact)
            .finish()
    }
}

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
    pub(crate) protect: RECT,
    pub(crate) memo_protect: RECT,
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
    let (copy, delete, protect, memo_protect, title, status, count, new, sort, sync) =
        if let Some(meta) = meta {
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
            let protect = meta_glyph(copy.left.saturating_sub(gap));
            let memo_protect = meta_glyph(protect.left.saturating_sub(gap));
            // The title is the heading of the row and the two readings beside it
            // are supports. A narrow editor column drops the supports — the length
            // before the time, because when a memo last changed is the more useful
            // of the two — rather than squeezing the heading into a stub.
            let text_left = meta.left.saturating_add(pad);
            let right_edge = memo_protect.left.saturating_sub(gap);
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
            let status =
                (status_want > 0 && available >= title_min + gap + status_width).then(|| {
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
            (
                copy,
                delete,
                protect,
                memo_protect,
                Some(title),
                status,
                count,
                new,
                sort,
                sync,
            )
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
            let group = glyph * 5 + gap * 4;
            let group_left = (bar.left + (bar.right - bar.left - group) / 2)
                .min(delete.left.saturating_sub(gap).saturating_sub(group))
                .max(new.right.saturating_add(gap));
            let sort = within(bar_glyph(group_left, glyph), bar);
            let sync = within(bar_glyph(sort.right.saturating_add(gap), glyph), bar);
            let copy = within(bar_glyph(sync.right.saturating_add(gap), glyph), bar);
            let protect = within(bar_glyph(copy.right.saturating_add(gap), glyph), bar);
            let memo_protect = within(bar_glyph(protect.right.saturating_add(gap), glyph), bar);
            (
                copy,
                delete,
                protect,
                memo_protect,
                title,
                status,
                None,
                new,
                sort,
                sync,
            )
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
        protect,
        memo_protect,
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
struct UnlockCompletion {
    epoch: u64,
    result: std::result::Result<
        (crate::pad_storage::LoadOutcome, PadProtectionEngine),
        ProtectionError,
    >,
}

impl std::fmt::Debug for UnlockCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnlockCompletion")
            .field("epoch", &self.epoch)
            .field("success", &self.result.is_ok())
            .finish()
    }
}

#[allow(missing_debug_implementations)]
struct PadState {
    locked: bool,
    v4_mode: bool,
    lock_headline: HWND,
    lock_password_label: HWND,
    lock_password: HWND,
    lock_unlock: HWND,
    lock_status: HWND,
    unlock_epoch: u64,
    unlock_in_flight: bool,
    unlock_result: Option<Receiver<UnlockCompletion>>,
    unlock_failures: u32,
    unlock_retry_at: Option<Instant>,
    enroll_phase: EnrollPhase,
    enroll_controls: [HWND; 8],
    enroll_epoch: u64,
    enroll_result: Option<Receiver<EnrollCompletion>>,
    memo_open: Option<OpenMemo>,
    memo_recovery: Option<MemoDraft>,
    memo_task_epoch: u64,
    memo_task_result: Option<Receiver<MemoTaskCompletion>>,
    memo_recovery_confirmation: Option<Sender<bool>>,
    memo_unlock_with_recovery: bool,
    memo_cancel: Option<PadCryptoCancellation>,
    memo_save_pending: Option<u64>,
    memo_protect_pending: Option<PadDocument>,
    mask_after_memo_protect: bool,
    protected_clipboard_sequence: Option<u32>,
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
    protect: HWND,
    memo_protect: HWND,
    fonts: PadFonts,
    brushes: PadBrushes,
    /// The hover text for the drawn faces. Optional because losing it costs
    /// the pad an explanation and nothing else.
    tooltips: Option<Tooltips>,
    worker: Option<StorageWorker>,
    protected_worker: Option<ProtectedSaveActor>,
    protected_session_seen: bool,
    /// Plaintext edits that the protected actor did not confirm. Kept only
    /// inside this live Pad process for reauthentication and retry.
    unsaved_recovery: Option<PadDocument>,
    recovery_base: Option<PadDocument>,
    recovery_conflict: bool,
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
#[allow(missing_debug_implementations)]
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
        let has_v4 = store.has_v4_cutover().map_err(storage_error)?;
        let mut v4_recovered = false;
        let (document, recovered_from_backup, mut load_status, mut save_blocked, locked, v4_mode) =
            if has_v4 {
                match store.load_v4().or_else(|error| {
                    if matches!(
                        error,
                        StorageError::ProtectedCutover | StorageError::ProtectedVerification
                    ) {
                        store.recover_v4_cutover()?;
                        v4_recovered = true;
                        store.load_v4()
                    } else {
                        Err(error)
                    }
                }) {
                    Ok(loaded) => (
                        loaded.document,
                        loaded.recovered_from_backup,
                        v4_recovered.then(|| "保護されたバックアップから復元しました".to_owned()),
                        false,
                        false,
                        true,
                    ),
                    Err(error) => (
                        PadDocument::default(),
                        false,
                        Some(format!("メモ別保護の復旧が必要です ({error})")),
                        true,
                        true,
                        true,
                    ),
                }
            } else {
                match store.load() {
                    Ok(loaded) => (
                        loaded.document,
                        loaded.recovered_from_backup,
                        None,
                        false,
                        false,
                        false,
                    ),
                    Err(StorageError::ProtectedCutover) => {
                        (PadDocument::default(), false, None, true, true, false)
                    }
                    Err(error) => (
                        PadDocument::default(),
                        false,
                        Some(format!(
                            "メモを復元できません。既存データは保護されています ({error})"
                        )),
                        true,
                        false,
                        false,
                    ),
                }
            };
        let active = document
            .live()
            .next()
            .map(|memo| memo.id)
            .unwrap_or_else(|| document.next_id());
        let generation = document.generation;
        let v4_actor = if v4_mode && !locked {
            let started = ProtectedSaveActor::spawn_v4(store.clone(), document.clone()).ok();
            if started.is_none() {
                save_blocked = true;
                load_status =
                    Some("保護された保存を開始できません。メモをコピーしてください".to_owned());
            }
            started
        } else {
            None
        };
        let mut state = Box::new(PadState {
            locked,
            v4_mode,
            lock_headline: HWND::default(),
            lock_password_label: HWND::default(),
            lock_password: HWND::default(),
            lock_unlock: HWND::default(),
            lock_status: HWND::default(),
            unlock_epoch: 0,
            unlock_in_flight: false,
            unlock_result: None,
            unlock_failures: 0,
            unlock_retry_at: None,
            enroll_phase: EnrollPhase::None,
            enroll_controls: [HWND::default(); 8],
            enroll_epoch: 0,
            enroll_result: None,
            memo_open: None,
            memo_recovery: None,
            memo_task_epoch: 0,
            memo_task_result: None,
            memo_recovery_confirmation: None,
            memo_unlock_with_recovery: false,
            memo_cancel: None,
            memo_save_pending: None,
            memo_protect_pending: None,
            mask_after_memo_protect: false,
            protected_clipboard_sequence: None,
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
            protect: HWND::default(),
            memo_protect: HWND::default(),
            fonts: PadFonts::new(96),
            brushes: PadBrushes::default(),
            tooltips: None,
            worker: if locked || v4_mode {
                None
            } else {
                Some(StorageWorker::spawn(store.clone()).map_err(storage_error)?)
            },
            protected_worker: v4_actor,
            protected_session_seen: v4_mode,
            unsaved_recovery: None,
            recovery_base: None,
            recovery_conflict: false,
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
                WS_EX_TOPMOST,
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
        // SAFETY: hwnd is the live Pad window created above; USER32 owns the
        // session notification registration until Drop unregisters it.
        if let Err(error) = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }
        {
            // SAFETY: registration failed, so this newly created HWND can be
            // destroyed before it becomes visible or escapes this method.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(error);
        }
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
            // Keep the pad above ordinary windows whenever it is requested.
            // NOACTIVATE leaves foreground ownership to the explicit call
            // below, so enforcing z-order does not itself steal focus.
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            if !SetForegroundWindow(self.hwnd).as_bool() {
                flash(self.hwnd);
            }
            let focus = if self.state.locked {
                self.state.lock_password
            } else if matches!(
                self.state.enroll_phase,
                EnrollPhase::Prompt
                    | EnrollPhase::MemoProtectPrompt
                    | EnrollPhase::MemoRecoveryPrompt
                    | EnrollPhase::MemoUnlockPrompt
            ) {
                self.state.enroll_controls[2]
            } else if matches!(
                self.state.enroll_phase,
                EnrollPhase::Running
                    | EnrollPhase::MemoProtectRunning
                    | EnrollPhase::MemoUnlockRunning
            ) {
                self.state.enroll_controls[0]
            } else if self.state.pane == PadPane::Editor || is_wide(self.hwnd) {
                self.state.body
            } else {
                self.state.list
            };
            if SetFocus(Some(focus)).is_err() {
                flash(self.hwnd);
            }
        }
    }

    /// Returns false when protected edits remain unsaved and the window must
    /// stay available for the user to copy them before renderer shutdown.
    pub fn hide(&self) -> bool {
        // Route ordinary host close through the same protected save boundary
        // as the Pad title-bar button. Session lock has a separate mask path.
        // SAFETY: self owns this live HWND and its procedure defines WM_CLOSE
        // with no pointer parameters and a boolean result.
        unsafe { SendMessageW(self.hwnd, WM_CLOSE, None, None).0 != 0 }
    }

    /// OS session lock must remove exposed memo HWNDs even when persistence
    /// is uncertain. The draft stays in this Pad process for reauthentication.
    pub fn mask_for_session(&self) {
        // SAFETY: only the Pad's UI thread handles its own synchronous message.
        unsafe {
            let _ = SendMessageW(self.hwnd, WM_PAD_MASK_FOR_SESSION, None, None);
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
        if self.state.protected_worker.is_some() {
            self.state.lock_protected(self.hwnd, true);
        }
        // Shutdown can precede the edit timer. The worker can flush only
        // snapshots it has received, so capture while the controls still live.
        if self.state.capture_controls() {
            self.state.publish(self.hwnd);
        }
        // SAFETY: the timers and window belong to this object.
        unsafe {
            let _ = WTSUnRegisterSessionNotification(self.hwnd);
            let _ = KillTimer(Some(self.hwnd), PAD_EDIT_TIMER);
            let _ = KillTimer(Some(self.hwnd), PAD_COMPLETION_TIMER);
            let _ = KillTimer(Some(self.hwnd), PAD_NOTICE_TIMER);
            let _ = KillTimer(Some(self.hwnd), PAD_UNLOCK_RETRY_TIMER);
            let _ = KillTimer(Some(self.hwnd), PAD_PROTECTED_CLIPBOARD_TIMER);
            let _ = DestroyWindow(self.hwnd);
        }
        if let Some(worker) = self.state.worker.as_mut() {
            let _ = worker.shutdown(SHUTDOWN_FLUSH_BUDGET);
        }
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
    if message.message == WM_KEYDOWN
        && matches!(message.wParam.0, key if key == VK_RETURN.0 as usize || key == VK_ESCAPE.0 as usize)
    {
        // SAFETY: this Pad-class top-level HWND owns the state pointer until
        // WM_NCDESTROY; a null pointer is treated as an uninitialized Pad.
        let state_ptr = unsafe { GetWindowLongPtrW(root, GWLP_USERDATA) as *const PadState };
        if !state_ptr.is_null()
            // SAFETY: the pointer is read only for this phase check before
            // the synchronous command can mutate the state.
            && unsafe { (*state_ptr).enroll_phase == EnrollPhase::MemoRecoveryPrompt }
        {
            let id = if message.wParam.0 == VK_RETURN.0 as usize {
                ENROLL_SUBMIT_ID
            } else {
                ENROLL_CANCEL_ID
            };
            // SAFETY: the live Pad processes this command synchronously and
            // owns both the prompt and its current keyboard focus.
            unsafe {
                let _ = SendMessageW(root, WM_COMMAND, Some(WPARAM(id as usize)), None);
            }
            return true;
        }
    }
    if select_all(message) {
        return true;
    }
    // SAFETY: `root` is a live top-level window and the message outlives the
    // call.
    unsafe { IsDialogMessageW(root, message).as_bool() }
}

/// Selects everything in the pad's field under the caret, for Ctrl+A.
///
/// A plain EDIT does not implement the shortcut, and neither does
/// `IsDialogMessageW`: without this, Ctrl+A over the body does nothing at
/// all. Swallowing the key press is what keeps the control character it
/// would otherwise translate to out of the text.
fn select_all(message: &MSG) -> bool {
    // SAFETY: both read one key's state and take no pointer; the message
    // carries a window handle Windows just validated.
    let (control, alt, id) = unsafe {
        (
            GetKeyState(VK_CONTROL.0 as i32) < 0,
            GetKeyState(VK_MENU.0 as i32) < 0,
            GetDlgCtrlID(message.hwnd),
        )
    };
    if !selects_all(message.message, message.wParam.0, control, alt, id) {
        return false;
    }
    // SAFETY: the field is live and `EM_SETSEL` takes two plain values; -1 as
    // the end of the range means "to the end of the text".
    unsafe {
        let _ = SendMessageW(message.hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
    true
}

/// Whether this key press is Ctrl+A over a field of the pad's that has text
/// to select.
///
/// Ctrl+Alt+A is left alone: that is AltGr+A on the keyboards which have one,
/// and it is a character rather than a command. The list is left alone too —
/// selecting every memo means nothing there, because the pad edits one memo
/// at a time.
fn selects_all(message: u32, key: usize, control: bool, alt: bool, id: i32) -> bool {
    message == WM_KEYDOWN
        && key == VK_A.0 as usize
        && control
        && !alt
        && matches!(
            u16::try_from(id),
            Ok(SEARCH_ID) | Ok(TITLE_ID) | Ok(BODY_ID) | Ok(ENROLL_PASSWORD_ID)
        )
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
    if state.locked {
        // Construct no memo controls in protected mode. Native child text is
        // also UI Automation text, so hiding old controls after creation is
        // too late for the first accessible frame.
        state.lock_headline = create_child(
            windows::core::w!("STATIC"),
            windows::core::w!("Sakura Pad はロックされています"),
            STATIC_CENTERED_ELLIPSIS,
            parent,
            LOCK_HEADLINE_ID,
        )?;
        state.lock_password_label = create_child(
            windows::core::w!("STATIC"),
            windows::core::w!("パスワード"),
            STATIC_CENTERED_ELLIPSIS,
            parent,
            LOCK_PASSWORD_LABEL_ID,
        )?;
        state.lock_password = create_child(
            windows::core::w!("EDIT"),
            windows::core::w!(""),
            WS_TABSTOP.0 as i32 | ES_LEFT | ES_AUTOHSCROLL | ES_PASSWORD,
            parent,
            LOCK_PASSWORD_ID,
        )?;
        // SAFETY: the edit is live; 256 UTF-16 units encode to at most the
        // protocol's 1024 password bytes.
        unsafe {
            let _ = SendMessageW(
                state.lock_password,
                EM_SETLIMITTEXT,
                Some(WPARAM(MAX_PASSWORD_UTF16_UNITS)),
                Some(LPARAM(0)),
            );
        }
        state.lock_unlock = create_child(
            windows::core::w!("BUTTON"),
            windows::core::w!("解除"),
            WS_TABSTOP.0 as i32,
            parent,
            LOCK_UNLOCK_ID,
        )?;
        state.lock_status = create_child(
            windows::core::w!("STATIC"),
            windows::core::w!("パスワードを入力して解除してください"),
            STATIC_CENTERED_ELLIPSIS,
            parent,
            LOCK_STATUS_ID,
        )?;
        state.apply_dpi(dpi_of(parent));
        state.refresh_brushes();
        return Ok(());
    }
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
    state.protect = button(windows::core::w!("この Pad をパスワードで保護"), PROTECT_ID)?;
    state.memo_protect = button(
        windows::core::w!("このメモをパスワードで保護"),
        MEMO_PROTECT_ID,
    )?;

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
        (state.protect, PROTECT_ID),
        (state.memo_protect, MEMO_PROTECT_ID),
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
    install_placeholder(state.search);
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
    install_placeholder(state.title);
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

fn set_secret_control_text(window: HWND, value: &str) {
    let mut wide = Zeroizing::new(value.encode_utf16().collect::<Vec<u16>>());
    wide.push(0);
    // SAFETY: the zeroizing UTF-16 buffer is NUL-terminated and remains live
    // through the synchronous copy into this owned child control.
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

fn get_password_text(window: HWND) -> Option<String> {
    let mut buffer = Zeroizing::new(vec![0u16; MAX_PASSWORD_UTF16_UNITS + 1]);
    // SAFETY: the zeroizing buffer bounds the native copy and stays live
    // through UTF-16 decoding. Invalid UTF-16 is rejected, not replaced.
    let length = unsafe { GetWindowTextW(window, &mut buffer) };
    String::from_utf16(&buffer[..(length.max(0) as usize).min(MAX_PASSWORD_UTF16_UNITS)]).ok()
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
    if state.locked {
        let width = (client.right - client.left).max(0);
        let center = client.left + width / 2;
        let field_width = scaled(320, dpi).min((width - scaled(32, dpi)).max(1));
        let left = center - field_width / 2;
        let top = client.top + ((client.bottom - client.top - scaled(180, dpi)) / 2).max(0);
        let height = scaled(30, dpi);
        let placements = [
            (
                state.lock_headline,
                RECT {
                    left,
                    top,
                    right: left + field_width,
                    bottom: top + height,
                },
            ),
            (
                state.lock_password_label,
                RECT {
                    left,
                    top: top + scaled(38, dpi),
                    right: left + field_width,
                    bottom: top + scaled(38, dpi) + height,
                },
            ),
            (
                state.lock_password,
                RECT {
                    left,
                    top: top + scaled(70, dpi),
                    right: left + field_width,
                    bottom: top + scaled(70, dpi) + height,
                },
            ),
            (
                state.lock_unlock,
                RECT {
                    left,
                    top: top + scaled(110, dpi),
                    right: left + field_width,
                    bottom: top + scaled(110, dpi) + height,
                },
            ),
            (
                state.lock_status,
                RECT {
                    left,
                    top: top + scaled(150, dpi),
                    right: left + field_width,
                    bottom: top + scaled(150, dpi) + height,
                },
            ),
        ];
        // SAFETY: all lock controls are live children of this window.
        unsafe {
            for (child, rect) in placements {
                let _ = SetWindowPos(
                    child,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        return;
    }
    if state.enroll_phase != EnrollPhase::None {
        let width = (client.right - client.left).max(0);
        let height = (client.bottom - client.top).max(0);
        let recovery = state.enroll_phase == EnrollPhase::MemoRecoveryPrompt;
        let field_width =
            scaled(if recovery { 440 } else { 360 }, dpi).min((width - scaled(32, dpi)).max(1));
        let left = client.left + (width - field_width) / 2;
        let top =
            client.top + ((height - scaled(if recovery { 320 } else { 296 }, dpi)) / 2).max(0);
        let line = scaled(30, dpi);
        let offsets = if recovery {
            [0, 42, 72, 0, 0, 210, 210, 258]
        } else {
            [0, 40, 70, 110, 140, 190, 190, 238]
        };
        for (index, child) in state.enroll_controls.iter().enumerate() {
            if child.is_invalid() {
                continue;
            }
            let y = top + scaled(offsets[index], dpi);
            let control_height = if recovery && index == 2 {
                scaled(104, dpi)
            } else {
                line
            };
            let (x, w) = match index {
                5 => (left, (field_width - scaled(8, dpi)) / 2),
                6 => (
                    left + (field_width + scaled(8, dpi)) / 2,
                    (field_width - scaled(8, dpi)) / 2,
                ),
                _ => (left, field_width),
            };
            // SAFETY: child is a live Pad child in the current layout and the
            // coordinates are bounded by the computed Pad client rectangle.
            unsafe {
                let _ = SetWindowPos(
                    *child,
                    None,
                    x,
                    y,
                    w.max(1),
                    control_height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        return;
    }
    let want = state.status_want();
    state.status_slot.set(want);
    let plan = layout(client, dpi, state.pane, want);
    let field = |frame: Option<RECT>| frame.map(|frame| field_child(frame, dpi));
    let placements: [(HWND, Option<RECT>); 17] = [
        (state.menu, plan.menu),
        (state.header_title, plan.header_title),
        (state.status, plan.status),
        (state.count, plan.count),
        (state.copy, Some(plan.copy)),
        (state.delete, Some(plan.delete)),
        (
            state.protect,
            (!state.protected_session_seen).then_some(plan.protect),
        ),
        (
            state.memo_protect,
            (!state.protected_session_seen || state.v4_mode).then_some(plan.memo_protect),
        ),
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
        if state.locked || state.enroll_phase != EnrollPhase::None {
            fill_color(dc, &client, colors.surface);
            // SAFETY: BeginPaint above always pairs with EndPaint.
            unsafe {
                let _ = EndPaint(window, &ps);
            }
            return;
        }
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

/// What the empty title field says the memo is called.
///
/// The same word the list writes on a memo with no title of its own, so a
/// fresh pad reads the same in both panes and a memo that is never named
/// never changes what it is called. Text here would be worse for a second
/// reason on top of the search field's: it would save as a real title, and
/// every untitled memo would be called `無題` for good.
const TITLE_PLACEHOLDER: &str = "無題";

/// The hint `control` shows when it is empty, or `None` where it has none.
fn placeholder_for(control: HWND) -> Option<&'static str> {
    // SAFETY: the control is live and this only reads its id.
    placeholder_text(unsafe { GetDlgCtrlID(control) })
}

/// The hint the control with this id shows when it is empty.
///
/// The body has none on purpose: it is the one field whose emptiness is the
/// point, and a hint on ruled paper would read as a first line of text.
fn placeholder_text(id: i32) -> Option<&'static str> {
    match u16::try_from(id) {
        Ok(SEARCH_ID) => Some(SEARCH_PLACEHOLDER),
        Ok(TITLE_ID) => Some(TITLE_PLACEHOLDER),
        _ => None,
    }
}

/// The EDIT class procedure, captured once. The search and title fields are
/// instances of the same class, so there is exactly one to remember.
static PLACEHOLDER_PROC: AtomicIsize = AtomicIsize::new(0);

fn install_placeholder(field: HWND) {
    if field.is_invalid() {
        return;
    }
    let ours = placeholder_proc as *const () as isize;
    // SAFETY: `field` is a live child created on this thread a moment ago.
    let previous = unsafe { SetWindowLongPtrW(field, GWLP_WNDPROC, ours) };
    // Never remember our own procedure: that would make `CallWindowProcW`
    // below recurse until the stack ends.
    if previous != 0 && previous != ours {
        let _ =
            PLACEHOLDER_PROC.compare_exchange(0, previous, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// The hint shows only in the field's resting state: nothing typed, and the
/// caret somewhere else.
fn shows_placeholder(text_units: i32, focused: bool) -> bool {
    text_units <= 0 && !focused
}

unsafe extern "system" fn placeholder_proc(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
) -> LRESULT {
    let captured = PLACEHOLDER_PROC.load(Ordering::Relaxed);
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
        WM_PAINT => paint_placeholder(window),
        _ => {}
    }
    result
}

fn paint_placeholder(search: HWND) {
    let Some(hint) = placeholder_for(search) else {
        return;
    };
    // SAFETY: the field is live; both calls only read window state.
    let (units, focused) = unsafe { (GetWindowTextLengthW(search), GetFocus() == search) };
    if !shows_placeholder(units, focused) {
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
    text(dc, hint, rect, colors.annotation, DT_LEFT | DT_END_ELLIPSIS);
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

#[cfg(test)]
#[path = "pad_placeholder_tests.rs"]
mod placeholder_tests;

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

fn draw_protect_button(item: &DRAWITEMSTRUCT, colors: Palette, dpi: u32, label: &str) {
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
    fill_color(item.hDC, &item.rcItem, colors.surface);
    rounded_box(
        item.hDC,
        item.rcItem,
        if pressed {
            colors.selected
        } else {
            colors.surface
        },
        Some(if focused { colors.rail } else { colors.border }),
        scaled(BORDER_96, dpi).max(1),
        scaled(CORNER_96, dpi).max(1),
    );
    text(
        item.hDC,
        label,
        item.rcItem,
        if pressed {
            colors.selected_ink
        } else {
            colors.ink
        },
        DT_CENTER,
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
    let opened = state
        .memo_open
        .as_ref()
        .filter(|open| open.id == memo.id)
        .map(|open| pad_list::UnlockedMemo {
            title: &open.title,
            body: &open.body,
        });
    let projection = pad_list::projection(memo, opened);
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
    let time_left = if projection.updated_ms().is_some() {
        inner_right.saturating_sub(time_width).max(inner_left)
    } else {
        inner_right
    };

    let ink = if selected {
        colors.selected_ink
    } else {
        colors.ink
    };
    let previous = select_font(item.hDC, state.fonts.body);
    text(
        item.hDC,
        projection.title(),
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
    if let Some(updated_ms) = projection.updated_ms() {
        text(
            item.hDC,
            &pad_list::format_time(state.now(), pad_list::local_time(updated_ms)),
            RECT {
                left: time_left,
                top: row.top,
                right: inner_right,
                bottom: title_bottom,
            },
            colors.annotation,
            DT_RIGHT,
        );
    }
    text(
        item.hDC,
        &projection.preview(),
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
        PROTECT_ID => "Pad 全体をパスワードで保護",
        MEMO_PROTECT_ID => "このメモだけをパスワードで保護または解除",
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
    fn show_memo_prompt(&mut self, window: HWND) {
        if self.locked || self.enroll_phase != EnrollPhase::None {
            return;
        }
        let Some(memo) = self.document.find(self.active) else {
            self.set_status("保護するメモを選択してください".to_owned());
            self.update_status();
            return;
        };
        if memo.tombstone {
            return;
        }
        let unlock = memo.protected_envelope().is_some();
        if self
            .memo_open
            .as_ref()
            .is_some_and(|open| open.id == self.active)
        {
            if self.close_open_memo(window) {
                self.refresh_editor();
                self.set_status("このメモをロックしました".to_owned());
            }
            self.update_status();
            return;
        }
        let title = if unlock {
            "このメモを解除"
        } else {
            "このメモだけを保護"
        };
        self.memo_unlock_with_recovery = false;
        let specs = [
            (windows::core::w!("STATIC"), title, STATIC_CENTERED_ELLIPSIS),
            (
                windows::core::w!("STATIC"),
                "パスワード",
                STATIC_CENTERED_ELLIPSIS,
            ),
            (
                windows::core::w!("EDIT"),
                "",
                WS_TABSTOP.0 as i32 | ES_LEFT | ES_AUTOHSCROLL | ES_PASSWORD,
            ),
            (
                if unlock {
                    windows::core::w!("BUTTON")
                } else {
                    windows::core::w!("STATIC")
                },
                if unlock {
                    "復旧キーを使う"
                } else {
                    "パスワードを再入力"
                },
                if unlock {
                    WS_TABSTOP.0 as i32
                } else {
                    STATIC_CENTERED_ELLIPSIS
                },
            ),
            (
                windows::core::w!("EDIT"),
                "",
                WS_TABSTOP.0 as i32 | ES_LEFT | ES_AUTOHSCROLL | ES_PASSWORD,
            ),
            (
                windows::core::w!("BUTTON"),
                if unlock {
                    "解除"
                } else {
                    "このメモを保護"
                },
                WS_TABSTOP.0 as i32,
            ),
            (
                windows::core::w!("BUTTON"),
                "キャンセル",
                WS_TABSTOP.0 as i32,
            ),
            (
                windows::core::w!("STATIC"),
                if unlock {
                    "パスワードまたは復旧キーで解除できます"
                } else {
                    "復旧キーを保存してから保護を確定します"
                },
                STATIC_CENTERED_ELLIPSIS,
            ),
        ];
        pad_caption::cloak(window, true);
        for (index, (class, label, style)) in specs.into_iter().enumerate() {
            let wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            match create_child(
                class,
                PCWSTR(wide.as_ptr()),
                style,
                window,
                ENROLL_CONTROL_IDS[index],
            ) {
                Ok(child) => self.enroll_controls[index] = child,
                Err(_) => {
                    self.destroy_enroll_controls();
                    pad_caption::cloak(window, false);
                    self.set_status("メモ保護画面を開けません".to_owned());
                    self.update_status();
                    return;
                }
            }
        }
        for index in [2, 4] {
            // SAFETY: both HWNDs are live password edits created above; the
            // message carries an integer limit and no pointer payload.
            unsafe {
                let _ = SendMessageW(
                    self.enroll_controls[index],
                    EM_SETLIMITTEXT,
                    Some(WPARAM(MAX_PASSWORD_UTF16_UNITS)),
                    None,
                );
            }
        }
        self.enroll_phase = if unlock {
            EnrollPhase::MemoUnlockPrompt
        } else {
            EnrollPhase::MemoProtectPrompt
        };
        self.hide_normal_controls();
        self.apply_dpi(dpi_of(window));
        update_layout(self, window);
        if unlock {
            // SAFETY: the confirmation password edit is live but not part of
            // an existing memo's unlock operation.
            unsafe {
                let _ = ShowWindow(self.enroll_controls[4], SW_HIDE);
            }
        }
        pad_caption::cloak(window, false);
        // SAFETY: the password edit is a live child of this prompt.
        unsafe {
            let _ = SetFocus(Some(self.enroll_controls[2]));
        }
    }

    fn start_memo_operation(&mut self, window: HWND) {
        let phase = self.enroll_phase;
        if phase == EnrollPhase::MemoRecoveryPrompt {
            // The generated key must leave the HWND before the worker may
            // publish its ciphertext. The explicit button is the user's
            // confirmation that they saved the key outside Pad.
            set_control_text(self.enroll_controls[2], "");
            set_control_text(
                self.enroll_controls[7],
                "復旧キーを消去しました。保存を確認しています…",
            );
            self.enroll_phase = EnrollPhase::MemoProtectRunning;
            if let Some(confirmation) = self.memo_recovery_confirmation.take() {
                let _ = confirmation.send(true);
            }
            // SAFETY: the prompt buttons remain live while the worker resumes.
            unsafe {
                let _ = EnableWindow(self.enroll_controls[5], false);
                let _ = EnableWindow(self.enroll_controls[6], false);
            }
            return;
        }
        if !matches!(
            phase,
            EnrollPhase::MemoProtectPrompt | EnrollPhase::MemoUnlockPrompt
        ) {
            return;
        }
        // SAFETY: enrollment edits are live children while either prompt is
        // visible. Reject programmatic WM_SETTEXT that exceeds the UI limit.
        if unsafe { GetWindowTextLengthW(self.enroll_controls[2]) } as usize
            > MAX_PASSWORD_UTF16_UNITS
        {
            set_control_text(self.enroll_controls[2], "");
            set_control_text(self.enroll_controls[7], "パスワードが長すぎます");
            return;
        }
        let first = get_password_text(self.enroll_controls[2]);
        let mut second = if phase == EnrollPhase::MemoProtectPrompt {
            get_password_text(self.enroll_controls[4])
        } else {
            None
        };
        set_control_text(self.enroll_controls[2], "");
        set_control_text(self.enroll_controls[4], "");
        let Some(mut password) = first else {
            set_control_text(self.enroll_controls[7], "パスワードを読み取れません");
            return;
        };
        if password.is_empty()
            || (phase == EnrollPhase::MemoProtectPrompt
                && second.as_deref() != Some(password.as_str()))
        {
            password.zeroize();
            if let Some(confirm) = second.as_mut() {
                confirm.zeroize();
            }
            set_control_text(self.enroll_controls[7], "パスワードが一致しません");
            return;
        }
        if let Some(confirm) = second.as_mut() {
            confirm.zeroize();
        }
        let secret = SecretBytes::new(password.as_bytes().to_vec());
        password.zeroize();
        if phase == EnrollPhase::MemoProtectPrompt {
            self.start_memo_protect(window, secret);
        } else {
            self.start_memo_unlock(window, secret, self.memo_unlock_with_recovery);
        }
    }

    fn toggle_memo_unlock_method(&mut self) {
        if self.enroll_phase != EnrollPhase::MemoUnlockPrompt {
            return;
        }
        self.memo_unlock_with_recovery = !self.memo_unlock_with_recovery;
        set_control_text(self.enroll_controls[2], "");
        set_control_text(
            self.enroll_controls[1],
            if self.memo_unlock_with_recovery {
                "復旧キー"
            } else {
                "パスワード"
            },
        );
        set_control_text(
            self.enroll_controls[3],
            if self.memo_unlock_with_recovery {
                "パスワードを使う"
            } else {
                "復旧キーを使う"
            },
        );
        set_control_text(
            self.enroll_controls[7],
            if self.memo_unlock_with_recovery {
                "旧形式のメモには復旧キーがありません"
            } else {
                "このメモのパスワードを入力してください"
            },
        );
        // SAFETY: the password edit is the live child of this prompt.
        unsafe {
            let _ = SetFocus(Some(self.enroll_controls[2]));
        }
    }

    fn start_memo_unlock(&mut self, window: HWND, password: SecretBytes, with_recovery: bool) {
        let Some(envelope) = self
            .document
            .find(self.active)
            .and_then(PadMemo::protected_envelope)
            .map(Vec::from)
        else {
            set_control_text(self.enroll_controls[7], "保護されたメモがありません");
            return;
        };
        let format = classify_envelope(&envelope);
        if with_recovery && format == MemoEnvelopeFormat::PasswordOnlyV1 {
            set_control_text(
                self.enroll_controls[7],
                "旧形式のメモには復旧キーがありません。パスワードを使ってください",
            );
            return;
        }
        if format == MemoEnvelopeFormat::Unknown {
            set_control_text(self.enroll_controls[7], "メモの暗号形式を確認できません");
            return;
        }
        let document_id = self.document.document_id;
        let memo_id = self.active;
        let epoch = self.memo_task_epoch.wrapping_add(1);
        let (sender, receiver) = mpsc::channel();
        let raw_window = window.0 as isize;
        let launched = thread::Builder::new()
            .name("sakura-memo-unlock".to_owned())
            .spawn(move || {
                let result = (|| {
                    let mut session =
                        MemoProtectionSession::new(document_id, memo_id, Duration::from_secs(15))?;
                    let mut bytes = if with_recovery {
                        session.unlock_with_recovery(password, SecretBytes::new(envelope))?
                    } else {
                        session.unlock(password, SecretBytes::new(envelope))?
                    };
                    let decoded = MemoPayloadV1::decode(&bytes).map_err(|_| MemoError::Protocol);
                    bytes.as_mut_slice().zeroize();
                    let (title, body) = decoded?;
                    Ok((session, Zeroizing::new(title), Zeroizing::new(body)))
                })();
                if sender
                    .send(MemoTaskCompletion {
                        epoch,
                        memo_id,
                        revision: 0,
                        result: MemoTaskResult::Unlocked(result),
                    })
                    .is_ok()
                {
                    // SAFETY: the HWND value originated on the Pad UI thread;
                    // receiver/epoch checks reject a stale completion.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(raw_window as *mut c_void)),
                            WM_PAD_MEMO_FINISHED,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            });
        if launched.is_err() {
            set_control_text(self.enroll_controls[7], "解除を開始できません");
            return;
        }
        self.memo_task_epoch = epoch;
        self.memo_task_result = Some(receiver);
        self.enroll_phase = EnrollPhase::MemoUnlockRunning;
        set_control_text(self.enroll_controls[7], "このメモを解除しています…");
        // SAFETY: these are live buttons of the running memo prompt.
        unsafe {
            let _ = EnableWindow(self.enroll_controls[5], false);
            let _ = EnableWindow(self.enroll_controls[6], false);
        }
    }

    fn start_memo_protect(&mut self, window: HWND, password: SecretBytes) {
        if self.v4_mode {
            self.start_existing_v4_memo_protect(window, password);
            return;
        }
        if self.protected_session_seen {
            self.start_v3_memo_protect(window, password);
            return;
        }
        // Legacy v2 transitions use the atomic v4 migration below; v3
        // sessions take their separate outer-envelope path above.
        if self.save_blocked {
            set_control_text(
                self.enroll_controls[7],
                "この Pad ではメモ別保護を開始できません",
            );
            return;
        }
        // SAFETY: window is the live Pad HWND and owns these legacy timers.
        unsafe {
            let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
        }
        if self.capture_controls() {
            self.publish(window);
        }
        let Some(mut worker) = self.worker.take() else {
            self.save_blocked = true;
            self.cancel_enroll_prompt(window);
            self.set_status("保存を確認できません。メモをコピーしてください".to_owned());
            self.update_status();
            return;
        };
        if !worker.shutdown(SHUTDOWN_FLUSH_BUDGET) {
            self.save_blocked = true;
            self.cancel_enroll_prompt(window);
            self.set_status("保存を確認できません。メモをコピーしてください".to_owned());
            self.update_status();
            return;
        }
        // SAFETY: the stopped legacy worker no longer needs this Pad timer.
        unsafe {
            let _ = KillTimer(Some(window), PAD_COMPLETION_TIMER);
        }
        let Some(memo) = self.document.find(self.active) else {
            self.cancel_enroll_prompt(window);
            return;
        };
        let Some((title, body)) = memo.plain_content() else {
            self.cancel_enroll_prompt(window);
            return;
        };
        let title = Zeroizing::new(title.to_owned());
        let body = Zeroizing::new(body.to_owned());
        let expected = self.document.clone();
        let memo_id = self.active;
        let epoch = self.memo_task_epoch.wrapping_add(1);
        let (sender, receiver) = mpsc::channel();
        let (confirmation_sender, confirmation_receiver) = mpsc::channel();
        let raw_window = window.0 as isize;
        let store = match PadStore::default() {
            Ok(store) => store,
            Err(_) => {
                self.save_blocked = true;
                self.cancel_enroll_prompt(window);
                self.set_status("保存先を確認できません。メモをコピーしてください".to_owned());
                self.update_status();
                return;
            }
        };
        let launched = thread::Builder::new()
            .name("sakura-memo-protect".to_owned())
            .spawn(move || {
                let session = RefCell::new(None::<MemoProtectionSession>);
                let result = store.migrate_to_v4(
                    &expected,
                    |document_id, before| {
                        let mut worker = MemoProtectionSession::new(
                            document_id,
                            memo_id,
                            Duration::from_secs(15),
                        )
                        .map_err(|_| StorageError::ProtectedVerification)?;
                        let payload = MemoPayloadV1::encode(&title, &body)?;
                        let (envelope, recovery_key) = worker
                            .create_with_recovery(password, SecretBytes::new(payload))
                            .map_err(|_| StorageError::ProtectedVerification)?;
                        if !await_memo_recovery_confirmation(
                            &sender,
                            &confirmation_receiver,
                            raw_window,
                            epoch,
                            memo_id,
                            recovery_key,
                        ) {
                            worker.lock();
                            return Err(StorageError::ProtectedVerification);
                        }
                        let mut next = before.clone();
                        next.document_id = document_id;
                        next.find_mut(memo_id)
                            .ok_or(StorageError::InvalidFormat)?
                            .protect_with_envelope(envelope.to_vec())?;
                        *session.borrow_mut() = Some(worker);
                        Ok(next)
                    },
                    |_document_id, candidate_id, envelope| {
                        if candidate_id != memo_id {
                            return Err(StorageError::ProtectedVerification);
                        }
                        let mut borrowed = session.borrow_mut();
                        let worker = borrowed
                            .as_mut()
                            .ok_or(StorageError::ProtectedVerification)?;
                        let opened = worker
                            .verify(SecretBytes::new(envelope.to_vec()))
                            .map_err(|_| StorageError::ProtectedVerification)?;
                        let (verified_title, verified_body) = MemoPayloadV1::decode(&opened)?;
                        let verified_title = Zeroizing::new(verified_title);
                        let verified_body = Zeroizing::new(verified_body);
                        if verified_title.as_str() != title.as_str()
                            || verified_body.as_str() != body.as_str()
                        {
                            return Err(StorageError::ProtectedVerification);
                        }
                        Ok(())
                    },
                );
                if let Some(mut worker) = session.into_inner() {
                    worker.lock();
                }
                let result = result.map_err(|error| MemoCreateFailure {
                    message: error.to_string(),
                    cutover_started: store.has_v4_cutover().unwrap_or(true),
                    legacy_exact: store.load().is_ok_and(|loaded| {
                        !loaded.recovered_from_backup && loaded.document == expected
                    }),
                });
                if sender
                    .send(MemoTaskCompletion {
                        epoch,
                        memo_id,
                        revision: 0,
                        result: MemoTaskResult::Created(result),
                    })
                    .is_ok()
                {
                    // SAFETY: the captured HWND value is checked by receiver and
                    // epoch before a UI transition, even if it has closed.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(raw_window as *mut c_void)),
                            WM_PAD_MEMO_FINISHED,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            });
        if launched.is_err() {
            self.save_blocked = true;
            self.cancel_enroll_prompt(window);
            self.set_status("保護を開始できません。メモをコピーしてください".to_owned());
            self.update_status();
            return;
        }
        self.memo_task_epoch = epoch;
        self.memo_task_result = Some(receiver);
        self.memo_recovery_confirmation = Some(confirmation_sender);
        self.enroll_phase = EnrollPhase::MemoProtectRunning;
        pad_caption::cloak(window, true);
        self.destroy_normal_controls();
        set_control_text(self.enroll_controls[7], "このメモを保護しています…");
        // SAFETY: these are live buttons belonging to this prompt.
        unsafe {
            let _ = EnableWindow(self.enroll_controls[5], false);
            let _ = EnableWindow(self.enroll_controls[6], false);
        }
        update_layout(self, window);
        pad_caption::cloak(window, false);
    }

    fn start_existing_v4_memo_protect(&mut self, window: HWND, password: SecretBytes) {
        if self.save_blocked {
            set_control_text(
                self.enroll_controls[7],
                "保存を確認できないため保護を開始できません",
            );
            return;
        }
        // SAFETY: this is the live Pad edit timer; no editor mutation can
        // occur once the prompt has hidden the ordinary controls.
        unsafe {
            let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
        }
        if self.capture_controls() {
            self.publish(window);
        }
        let Some(mut actor) = self.protected_worker.take() else {
            self.save_blocked = true;
            self.cancel_enroll_prompt(window);
            self.set_status("保存を確認できません。メモをコピーしてください".to_owned());
            self.update_status();
            return;
        };
        actor.begin_lock();
        let outcome = actor.finish_lock(SHUTDOWN_FLUSH_BUDGET);
        if !matches!(outcome.status, ProtectedLockStatus::Saved)
            || self.document != *outcome.confirmed_document
        {
            self.save_blocked = true;
            self.cancel_enroll_prompt(window);
            self.set_status("保存を確認できません。メモをコピーしてください".to_owned());
            self.update_status();
            return;
        }
        self.memo_save_pending = None;
        let expected = self.document.clone();
        let memo_id = self.active;
        let Some((title, body)) = expected.find(memo_id).and_then(PadMemo::plain_content) else {
            self.protected_worker = PadStore::default()
                .ok()
                .and_then(|store| ProtectedSaveActor::spawn_v4(store, self.document.clone()).ok());
            self.save_blocked = self.protected_worker.is_none();
            self.cancel_enroll_prompt(window);
            return;
        };
        let title = Zeroizing::new(title.to_owned());
        let body = Zeroizing::new(body.to_owned());
        let store = match PadStore::default() {
            Ok(store) => store,
            Err(_) => {
                self.save_blocked = true;
                self.cancel_enroll_prompt(window);
                self.set_status("保存先を確認できません。メモをコピーしてください".to_owned());
                self.update_status();
                return;
            }
        };
        let epoch = self.memo_task_epoch.wrapping_add(1);
        let (sender, receiver) = mpsc::channel();
        let (confirmation_sender, confirmation_receiver) = mpsc::channel();
        let raw_window = window.0 as isize;
        let launched = thread::Builder::new()
            .name("sakura-memo-add-protection".to_owned())
            .spawn(move || {
                let result: std::result::Result<PadDocument, StorageError> = (|| {
                    let mut session = MemoProtectionSession::new(
                        expected.document_id,
                        memo_id,
                        Duration::from_secs(15),
                    )
                    .map_err(|_| StorageError::ProtectedVerification)?;
                    let payload = MemoPayloadV1::encode(&title, &body)?;
                    let (envelope, recovery_key) = session
                        .create_with_recovery(password, SecretBytes::new(payload))
                        .map_err(|_| StorageError::ProtectedVerification)?;
                    if !await_memo_recovery_confirmation(
                        &sender,
                        &confirmation_receiver,
                        raw_window,
                        epoch,
                        memo_id,
                        recovery_key,
                    ) {
                        session.lock();
                        return Err(StorageError::ProtectedVerification);
                    }
                    let opened = session
                        .verify(SecretBytes::new(envelope.to_vec()))
                        .map_err(|_| StorageError::ProtectedVerification)?;
                    let (checked_title, checked_body) = MemoPayloadV1::decode(&opened)?;
                    let checked_title = Zeroizing::new(checked_title);
                    let checked_body = Zeroizing::new(checked_body);
                    if checked_title.as_str() != title.as_str()
                        || checked_body.as_str() != body.as_str()
                    {
                        return Err(StorageError::ProtectedVerification);
                    }
                    let mut next = expected.clone();
                    next.generation = next
                        .generation
                        .checked_add(1)
                        .ok_or(StorageError::LimitExceeded)?;
                    next.find_mut(memo_id)
                        .ok_or(StorageError::InvalidFormat)?
                        .protect_with_envelope(envelope.to_vec())?;
                    store.write_v4(&expected, &next)?;
                    session.lock();
                    Ok(next)
                })();
                let result = result.map_err(|error| MemoCreateFailure {
                    message: error.to_string(),
                    cutover_started: !store
                        .load_v4()
                        .is_ok_and(|loaded| loaded.document == expected),
                    legacy_exact: false,
                });
                if sender
                    .send(MemoTaskCompletion {
                        epoch,
                        memo_id,
                        revision: 0,
                        result: MemoTaskResult::Created(result),
                    })
                    .is_ok()
                {
                    // SAFETY: UI receiver and epoch gate this captured Pad HWND.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(raw_window as *mut c_void)),
                            WM_PAD_MEMO_FINISHED,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            });
        if launched.is_err() {
            self.protected_worker = PadStore::default()
                .ok()
                .and_then(|store| ProtectedSaveActor::spawn_v4(store, self.document.clone()).ok());
            self.save_blocked = self.protected_worker.is_none();
            self.cancel_enroll_prompt(window);
            self.set_status("メモ保護を開始できません".to_owned());
            self.update_status();
            return;
        }
        self.memo_task_epoch = epoch;
        self.memo_task_result = Some(receiver);
        self.memo_recovery_confirmation = Some(confirmation_sender);
        self.enroll_phase = EnrollPhase::MemoProtectRunning;
        pad_caption::cloak(window, true);
        self.destroy_normal_controls();
        set_control_text(self.enroll_controls[7], "このメモを保護しています…");
        // SAFETY: both prompt buttons are live while the cutover runs.
        unsafe {
            let _ = EnableWindow(self.enroll_controls[5], false);
            let _ = EnableWindow(self.enroll_controls[6], false);
        }
        update_layout(self, window);
        pad_caption::cloak(window, false);
    }

    fn start_v3_memo_protect(&mut self, window: HWND, password: SecretBytes) {
        if self.save_blocked || self.protected_worker.is_none() {
            set_control_text(self.enroll_controls[7], "Pad 全体の保存を確認できません");
            return;
        }
        // SAFETY: the live Pad owns this timer; a final editor snapshot is
        // submitted before the memo-specific key operation starts.
        unsafe {
            let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
        }
        if self.capture_controls() {
            self.publish(window);
        }
        let store = match PadStore::default() {
            Ok(store) => store,
            Err(_) => {
                set_control_text(self.enroll_controls[7], "Pad の保存先を確認できません");
                return;
            }
        };
        let document_id = match store.protected_vault_id() {
            Ok(id) if self.document.document_id == [0; 16] || self.document.document_id == id => id,
            _ => {
                set_control_text(self.enroll_controls[7], "Pad 全体の保護 ID が一致しません");
                return;
            }
        };
        let expected = self.document.clone();
        let memo_id = self.active;
        let Some((title, body)) = expected.find(memo_id).and_then(PadMemo::plain_content) else {
            return;
        };
        let title = Zeroizing::new(title.to_owned());
        let body = Zeroizing::new(body.to_owned());
        let epoch = self.memo_task_epoch.wrapping_add(1);
        let (sender, receiver) = mpsc::channel();
        let (confirmation_sender, confirmation_receiver) = mpsc::channel();
        let raw_window = window.0 as isize;
        let launched = thread::Builder::new()
            .name("sakura-v3-memo-protect".to_owned())
            .spawn(move || {
                let result = (|| {
                    let mut session =
                        MemoProtectionSession::new(document_id, memo_id, Duration::from_secs(15))?;
                    let payload =
                        MemoPayloadV1::encode(&title, &body).map_err(|_| MemoError::Protocol)?;
                    let (envelope, recovery_key) =
                        session.create_with_recovery(password, SecretBytes::new(payload))?;
                    if !await_memo_recovery_confirmation(
                        &sender,
                        &confirmation_receiver,
                        raw_window,
                        epoch,
                        memo_id,
                        recovery_key,
                    ) {
                        session.lock();
                        return Err(MemoError::Unavailable);
                    }
                    let opened = session.verify(SecretBytes::new(envelope.to_vec()))?;
                    let (checked_title, checked_body) =
                        MemoPayloadV1::decode(&opened).map_err(|_| MemoError::Protocol)?;
                    let checked_title = Zeroizing::new(checked_title);
                    let checked_body = Zeroizing::new(checked_body);
                    if checked_title.as_str() != title.as_str()
                        || checked_body.as_str() != body.as_str()
                    {
                        return Err(MemoError::Protocol);
                    }
                    let mut next = expected;
                    next.document_id = document_id;
                    next.generation = next.generation.checked_add(1).ok_or(MemoError::Stale)?;
                    next.find_mut(memo_id)
                        .ok_or(MemoError::Protocol)?
                        .protect_with_envelope(envelope.to_vec())
                        .map_err(|_| MemoError::Protocol)?;
                    session.lock();
                    Ok(next)
                })();
                if sender
                    .send(MemoTaskCompletion {
                        epoch,
                        memo_id,
                        revision: 0,
                        result: MemoTaskResult::Prepared(result),
                    })
                    .is_ok()
                {
                    // SAFETY: UI receiver and epoch gate this captured Pad HWND.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(raw_window as *mut c_void)),
                            WM_PAD_MEMO_FINISHED,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            });
        if launched.is_err() {
            set_control_text(self.enroll_controls[7], "メモ保護を開始できません");
            return;
        }
        self.memo_task_epoch = epoch;
        self.memo_task_result = Some(receiver);
        self.memo_recovery_confirmation = Some(confirmation_sender);
        self.enroll_phase = EnrollPhase::MemoProtectRunning;
        pad_caption::cloak(window, true);
        self.destroy_normal_controls();
        set_control_text(self.enroll_controls[7], "このメモを保護しています…");
        // SAFETY: both live prompt buttons are disabled until the outer
        // protected actor confirms the new encrypted document generation.
        unsafe {
            let _ = EnableWindow(self.enroll_controls[5], false);
            let _ = EnableWindow(self.enroll_controls[6], false);
        }
        update_layout(self, window);
        pad_caption::cloak(window, false);
    }

    fn finish_memo_task(&mut self, window: HWND) {
        let Some(completion) = self
            .memo_task_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        else {
            return;
        };
        let recovery_ready = matches!(completion.result, MemoTaskResult::RecoveryReady(_));
        if !recovery_ready {
            self.memo_task_result = None;
            self.memo_recovery_confirmation = None;
            self.memo_cancel = None;
        }
        if completion.epoch != self.memo_task_epoch || completion.memo_id != self.active {
            if recovery_ready {
                self.memo_task_result = None;
                if let Some(confirmation) = self.memo_recovery_confirmation.take() {
                    let _ = confirmation.send(false);
                }
            }
            return;
        }
        match completion.result {
            MemoTaskResult::RecoveryReady(key)
                if self.enroll_phase == EnrollPhase::MemoProtectRunning =>
            {
                if self.mask_after_memo_protect {
                    if let Some(confirmation) = self.memo_recovery_confirmation.take() {
                        let _ = confirmation.send(false);
                    }
                    return;
                }
                pad_caption::cloak(window, true);
                self.enroll_phase = EnrollPhase::MemoRecoveryPrompt;
                set_control_text(self.enroll_controls[0], "復旧キーを保存してください");
                set_control_text(
                    self.enroll_controls[1],
                    "復旧キー（選択してコピーできます）",
                );
                // A single-line password EDIT cannot wrap a canonical key.
                // Replace it with a read-only multiline EDIT; soft wrapping
                // changes display only, not the underlying copied text.
                set_control_text(self.enroll_controls[2], "");
                // SAFETY: this is the owned prompt child, and its HWND is
                // cleared immediately so failure cannot double-destroy it.
                unsafe {
                    let _ = DestroyWindow(self.enroll_controls[2]);
                }
                self.enroll_controls[2] = HWND::default();
                let key_edit = match create_child(
                    windows::core::w!("EDIT"),
                    windows::core::w!(""),
                    WS_TABSTOP.0 as i32 | ES_LEFT | ES_MULTILINE | ES_AUTOVSCROLL,
                    window,
                    ENROLL_PASSWORD_ID,
                ) {
                    Ok(child) => child,
                    Err(_) => {
                        self.cancel_enroll_prompt(window);
                        set_control_text(
                            self.enroll_controls[7],
                            "復旧キーを表示できないため保護を中止しています",
                        );
                        pad_caption::cloak(window, false);
                        return;
                    }
                };
                self.enroll_controls[2] = key_edit;
                // SAFETY: the new EDIT belongs to this live prompt. Its font
                // stays owned by PadState until the next DPI refresh.
                unsafe {
                    let _ = SendMessageW(
                        key_edit,
                        WM_SETFONT,
                        Some(WPARAM(self.fonts.body.0 as usize)),
                        Some(LPARAM(1)),
                    );
                    let _ = SendMessageW(key_edit, EM_SETREADONLY, Some(WPARAM(1)), None);
                    let _ = ShowWindow(self.enroll_controls[3], SW_HIDE);
                    let _ = ShowWindow(self.enroll_controls[4], SW_HIDE);
                }
                set_secret_control_text(key_edit, &key);
                set_control_text(self.enroll_controls[5], "保存したので続ける");
                set_control_text(
                    self.enroll_controls[7],
                    "Pad の外に保存後、確認を押す（10 分で中止）",
                );
                update_layout(self, window);
                // SAFETY: these prompt children remain live until the
                // explicit approval, cancellation, or terminal timeout.
                unsafe {
                    let _ = EnableWindow(self.enroll_controls[5], true);
                    let _ = EnableWindow(self.enroll_controls[6], true);
                    let _ = SetFocus(Some(key_edit));
                    let _ = SendMessageW(key_edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                    let _ = RedrawWindow(
                        Some(window),
                        None,
                        None,
                        RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
                    );
                }
                pad_caption::cloak(window, false);
            }
            MemoTaskResult::Unlocked(Ok((session, mut title, mut body)))
                if self.enroll_phase == EnrollPhase::MemoUnlockRunning =>
            {
                let mut dirty = false;
                if let Some(recovery) = self.memo_recovery.take() {
                    let current = self
                        .document
                        .find(self.active)
                        .and_then(PadMemo::protected_envelope);
                    if recovery.id == self.active
                        && current == Some(recovery.base_envelope.as_slice())
                    {
                        title = recovery.title;
                        body = recovery.body;
                        dirty = true;
                    } else {
                        title = recovery.title;
                        body = recovery.body;
                        dirty = true;
                        self.save_blocked = true;
                    }
                }
                self.memo_open = Some(OpenMemo {
                    id: self.active,
                    title,
                    body,
                    session: Some(session),
                    dirty,
                    revision: u64::from(dirty),
                });
                pad_caption::cloak(window, true);
                self.destroy_enroll_controls();
                self.enroll_phase = EnrollPhase::None;
                self.refresh_list();
                self.refresh_editor();
                self.set_status(
                    if self.save_blocked {
                        "保存済みの内容が変わりました。未保存メモをコピーしてください"
                    } else if dirty {
                        "未保存のメモを復元しました。保存しています…"
                    } else {
                        "このメモを解除しました"
                    }
                    .to_owned(),
                );
                self.update_status();
                update_layout(self, window);
                pad_caption::cloak(window, false);
                if dirty && !self.save_blocked {
                    self.start_memo_reseal(window);
                }
                // SAFETY: the normal body edit remains a live child after
                // the prompt controls are destroyed.
                unsafe {
                    let _ = SetFocus(Some(self.body));
                }
            }
            MemoTaskResult::Unlocked(Err(error))
                if self.enroll_phase == EnrollPhase::MemoUnlockRunning =>
            {
                self.enroll_phase = EnrollPhase::MemoUnlockPrompt;
                set_control_text(
                    self.enroll_controls[7],
                    if error == MemoError::Authentication {
                        if self.memo_unlock_with_recovery {
                            "復旧キーが正しくありません"
                        } else {
                            "パスワードが正しくありません"
                        }
                    } else {
                        "このメモを解除できません"
                    },
                );
                // SAFETY: both buttons remain live in this prompt and may
                // accept a fresh explicit attempt or cancellation.
                unsafe {
                    let _ = EnableWindow(self.enroll_controls[5], true);
                    let _ = EnableWindow(self.enroll_controls[6], true);
                }
            }
            MemoTaskResult::Created(Ok(document))
                if self.enroll_phase == EnrollPhase::MemoProtectRunning =>
            {
                self.mask_after_memo_protect = false;
                pad_caption::cloak(window, true);
                self.destroy_enroll_controls();
                self.enroll_phase = EnrollPhase::None;
                self.document = document;
                self.generation = self.document.generation;
                self.latest_submitted = self.generation;
                self.v4_mode = true;
                self.protected_session_seen = true;
                self.protected_worker = PadStore::default().ok().and_then(|store| {
                    ProtectedSaveActor::spawn_v4(store, self.document.clone()).ok()
                });
                self.save_blocked = self.protected_worker.is_none();
                self.set_status(
                    if self.save_blocked {
                        "保護は完了しましたが保存を再開できません"
                    } else {
                        "このメモを保護しました"
                    }
                    .to_owned(),
                );
                if create_controls(self, window).is_err() {
                    self.save_blocked = true;
                    self.destroy_normal_controls();
                } else {
                    self.refresh_list();
                    self.refresh_editor();
                    self.update_status();
                    update_layout(self, window);
                }
                pad_caption::cloak(window, false);
            }
            MemoTaskResult::Created(Err(failure))
                if self.enroll_phase == EnrollPhase::MemoProtectRunning =>
            {
                self.mask_after_memo_protect = false;
                pad_caption::cloak(window, true);
                self.destroy_enroll_controls();
                self.enroll_phase = EnrollPhase::None;
                if failure.cutover_started {
                    self.unsaved_recovery = Some(self.document.clone());
                    self.document = PadDocument::default();
                    self.v4_mode = true;
                    self.locked = true;
                    self.save_blocked = true;
                    self.set_status(format!("メモ別保護の復旧が必要です ({})", failure.message));
                    let _ = create_controls(self, window);
                    set_control_text(self.lock_status, &self.status_message);
                    // SAFETY: lock_unlock exists on the recovery surface but
                    // the whole-Pad password cannot repair a v4 cutover.
                    unsafe {
                        let _ = EnableWindow(self.lock_unlock, false);
                    }
                } else {
                    if self.v4_mode {
                        self.protected_worker = PadStore::default().ok().and_then(|store| {
                            ProtectedSaveActor::spawn_v4(store, self.document.clone()).ok()
                        });
                        self.save_blocked = self.protected_worker.is_none();
                    } else {
                        self.worker = failure
                            .legacy_exact
                            .then(|| {
                                PadStore::default()
                                    .ok()
                                    .and_then(|store| StorageWorker::spawn(store).ok())
                            })
                            .flatten();
                        self.save_blocked = self.worker.is_none();
                    }
                    self.set_status(
                        if self.save_blocked {
                            "保護できず保存も確認できません。メモをコピーしてください"
                        } else {
                            "このメモを保護できませんでした"
                        }
                        .to_owned(),
                    );
                    if create_controls(self, window).is_err() {
                        self.save_blocked = true;
                        self.destroy_normal_controls();
                    } else {
                        self.refresh_list();
                        self.refresh_editor();
                        self.update_status();
                    }
                }
                update_layout(self, window);
                pad_caption::cloak(window, false);
            }
            MemoTaskResult::Prepared(Ok(next))
                if self.enroll_phase == EnrollPhase::MemoProtectRunning =>
            {
                let generation = next.generation;
                let submitted = self
                    .protected_worker
                    .as_ref()
                    .is_some_and(|actor| actor.submit(next.clone()).is_ok());
                if submitted {
                    self.memo_protect_pending = Some(next);
                    self.latest_submitted = generation;
                    self.memo_save_pending = Some(generation);
                    set_control_text(self.enroll_controls[7], "暗号化した保存を確認しています…");
                    // SAFETY: this live Pad timer polls the protected actor
                    // until the exact new generation is committed or fails.
                    unsafe {
                        let _ = SetTimer(Some(window), PAD_COMPLETION_TIMER, 100, None);
                    }
                } else {
                    self.enter_memo_cutover_recovery(window, "保護したメモの保存を開始できません");
                }
            }
            MemoTaskResult::Prepared(Err(_))
                if self.enroll_phase == EnrollPhase::MemoProtectRunning =>
            {
                pad_caption::cloak(window, true);
                self.destroy_enroll_controls();
                self.enroll_phase = EnrollPhase::None;
                if create_controls(self, window).is_err() {
                    self.save_blocked = true;
                    self.destroy_normal_controls();
                } else {
                    self.refresh_list();
                    self.refresh_editor();
                    self.set_status("このメモを保護できませんでした".to_owned());
                    self.update_status();
                    update_layout(self, window);
                }
                pad_caption::cloak(window, false);
                if std::mem::take(&mut self.mask_after_memo_protect) {
                    let _ = self.lock_protected(window, true);
                }
            }
            MemoTaskResult::Resealed(Ok((session, envelope))) => {
                let Some(open) = self
                    .memo_open
                    .as_mut()
                    .filter(|open| open.id == completion.memo_id)
                else {
                    return;
                };
                open.session = Some(session);
                open.dirty = open.revision != completion.revision;
                let Some(memo) = self.document.find_mut(completion.memo_id) else {
                    return;
                };
                if memo.replace_protected_envelope(envelope).is_err() {
                    self.save_blocked = true;
                    self.set_status(
                        "暗号化したメモを保存できません。コピーしてください".to_owned(),
                    );
                    self.update_status();
                    return;
                }
                memo.updated_ms = now_ms();
                self.publish(window);
                self.memo_save_pending = Some(self.latest_submitted);
                if self.memo_open.as_ref().is_some_and(|open| open.dirty) {
                    self.start_memo_reseal(window);
                }
            }
            MemoTaskResult::Resealed(Err(_)) => {
                self.save_blocked = true;
                self.set_status("メモの再暗号化に失敗しました。コピーしてください".to_owned());
                self.update_status();
            }
            MemoTaskResult::Unlocked(_)
            | MemoTaskResult::Created(_)
            | MemoTaskResult::Prepared(_)
            | MemoTaskResult::RecoveryReady(_) => {}
        }
    }

    fn finish_v3_memo_protection(&mut self, window: HWND) {
        let Some(next) = self.memo_protect_pending.take() else {
            return;
        };
        pad_caption::cloak(window, true);
        self.destroy_enroll_controls();
        self.enroll_phase = EnrollPhase::None;
        self.document = next;
        self.generation = self.document.generation;
        self.memo_save_pending = None;
        self.set_status("このメモを保護しました".to_owned());
        if create_controls(self, window).is_err() {
            self.save_blocked = true;
            self.destroy_normal_controls();
        } else {
            self.refresh_list();
            self.refresh_editor();
            self.update_status();
            update_layout(self, window);
        }
        pad_caption::cloak(window, false);
        if std::mem::take(&mut self.mask_after_memo_protect) {
            let _ = self.lock_protected(window, true);
        }
    }

    fn enter_memo_cutover_recovery(&mut self, window: HWND, message: &str) {
        pad_caption::cloak(window, true);
        self.unsaved_recovery = Some(self.document.clone());
        self.memo_protect_pending = None;
        self.memo_save_pending = None;
        self.destroy_enroll_controls();
        self.enroll_phase = EnrollPhase::None;
        self.document = PadDocument::default();
        self.locked = true;
        self.save_blocked = true;
        self.set_status(message.to_owned());
        let _ = create_controls(self, window);
        set_control_text(self.lock_status, message);
        update_layout(self, window);
        pad_caption::cloak(window, false);
    }

    fn start_memo_reseal(&mut self, window: HWND) {
        if self.memo_task_result.is_some() || self.save_blocked {
            return;
        }
        let Some(open) = self.memo_open.as_mut().filter(|open| open.dirty) else {
            return;
        };
        let Some(mut session) = open.session.take() else {
            return;
        };
        let encoded = match MemoPayloadV1::encode(&open.title, &open.body) {
            Ok(encoded) => encoded,
            Err(_) => {
                open.session = Some(session);
                self.save_blocked = true;
                self.set_status("メモが長すぎて暗号化できません。コピーしてください".to_owned());
                self.update_status();
                return;
            }
        };
        self.memo_cancel = session.cancellation_handle();
        let revision = open.revision;
        let memo_id = open.id;
        let epoch = self.memo_task_epoch.wrapping_add(1);
        let (sender, receiver) = mpsc::channel();
        let raw_window = window.0 as isize;
        let launched = thread::Builder::new()
            .name("sakura-memo-reseal".to_owned())
            .spawn(move || {
                let result = session
                    .reseal(SecretBytes::new(encoded))
                    .map(|envelope| (session, envelope.to_vec()));
                if sender
                    .send(MemoTaskCompletion {
                        epoch,
                        memo_id,
                        revision,
                        result: MemoTaskResult::Resealed(result),
                    })
                    .is_ok()
                {
                    // SAFETY: this HWND value originated on the Pad UI thread;
                    // receiver/epoch checks reject a stale completion.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(raw_window as *mut c_void)),
                            WM_PAD_MEMO_FINISHED,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            });
        if launched.is_err() {
            self.save_blocked = true;
            self.set_status("再暗号化を開始できません。コピーしてください".to_owned());
            self.update_status();
            return;
        }
        self.memo_task_epoch = epoch;
        self.memo_task_result = Some(receiver);
        self.set_status("メモを再暗号化しています…".to_owned());
        self.update_status();
    }

    fn close_open_memo(&mut self, window: HWND) -> bool {
        if self.memo_open.is_none() {
            return true;
        }
        self.capture_controls();
        if self.memo_open.as_ref().is_some_and(|open| open.dirty) {
            self.start_memo_reseal(window);
        }
        if self.memo_task_result.is_some()
            || self.memo_save_pending.is_some()
            || self.save_blocked
            || self.memo_open.as_ref().is_some_and(|open| open.dirty)
        {
            self.set_status(
                "暗号化した保存の完了を待っています。切り替えを中止しました".to_owned(),
            );
            self.update_status();
            return false;
        }
        pad_caption::cloak(window, true);
        self.updating_controls = true;
        set_control_text(self.title, "");
        set_control_text(self.body, "");
        self.updating_controls = false;
        self.memo_open = None;
        self.refresh_list();
        pad_caption::cloak(window, false);
        true
    }

    fn force_lock_open_memo(&mut self, window: HWND) {
        self.capture_controls();
        if let Some(open) = self.memo_open.as_ref() {
            if open.dirty
                || self.memo_task_result.is_some()
                || self.memo_save_pending.is_some()
                || self.save_blocked
            {
                let base_envelope = self
                    .document
                    .find(open.id)
                    .and_then(PadMemo::protected_envelope)
                    .map(Vec::from)
                    .unwrap_or_default();
                self.memo_recovery = Some(MemoDraft {
                    id: open.id,
                    title: Zeroizing::new((*open.title).clone()),
                    body: Zeroizing::new((*open.body).clone()),
                    base_envelope,
                });
            }
        }
        if let Some(cancel) = self.memo_cancel.take() {
            cancel.cancel();
        }
        self.memo_task_epoch = self.memo_task_epoch.wrapping_add(1);
        self.memo_task_result = None;
        pad_caption::cloak(window, true);
        self.updating_controls = true;
        set_control_text(self.title, "");
        set_control_text(self.body, "");
        self.updating_controls = false;
        self.memo_open = None;
        self.refresh_list();
        pad_caption::cloak(window, false);
        self.refresh_editor();
    }

    fn mask_memo_for_session(&mut self, window: HWND) {
        match self.enroll_phase {
            EnrollPhase::MemoUnlockRunning => {
                self.memo_task_epoch = self.memo_task_epoch.wrapping_add(1);
                self.memo_task_result = None;
                pad_caption::cloak(window, true);
                self.destroy_enroll_controls();
                self.enroll_phase = EnrollPhase::None;
                self.refresh_editor();
                update_layout(self, window);
                pad_caption::cloak(window, false);
            }
            EnrollPhase::Prompt
            | EnrollPhase::MemoProtectPrompt
            | EnrollPhase::MemoUnlockPrompt => self.cancel_enroll_prompt(window),
            EnrollPhase::MemoRecoveryPrompt => {
                self.mask_after_memo_protect = true;
                self.cancel_enroll_prompt(window);
            }
            EnrollPhase::None => {
                let _ = self.lock_protected(window, true);
            }
            EnrollPhase::MemoProtectRunning => {
                self.mask_after_memo_protect = true;
            }
            EnrollPhase::Running => {}
        }
    }

    fn show_enroll_prompt(&mut self, window: HWND) {
        if self.v4_mode {
            self.set_status("メモ別保護と Pad 全体の保護は併用できません".to_owned());
            self.update_status();
            return;
        }
        if self.locked || self.enroll_phase != EnrollPhase::None || self.protected_session_seen {
            return;
        }
        // SAFETY: window is the live Pad HWND; this cancels its pending edit
        // timer before taking a final snapshot for the enrollment prompt.
        unsafe {
            let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
        }
        if self.capture_controls() {
            self.publish(window);
        }
        if self.save_blocked {
            self.set_status("保存を確認できません。メモをコピーして保管してください".to_owned());
            self.update_status();
            return;
        }
        let specs = [
            (
                windows::core::w!("STATIC"),
                windows::core::w!("Pad 全体をパスワードで保護"),
                STATIC_CENTERED_ELLIPSIS,
            ),
            (
                windows::core::w!("STATIC"),
                windows::core::w!("パスワード"),
                STATIC_CENTERED_ELLIPSIS,
            ),
            (
                windows::core::w!("EDIT"),
                windows::core::w!(""),
                WS_TABSTOP.0 as i32 | ES_LEFT | ES_AUTOHSCROLL | ES_PASSWORD,
            ),
            (
                windows::core::w!("STATIC"),
                windows::core::w!("パスワードを再入力"),
                STATIC_CENTERED_ELLIPSIS,
            ),
            (
                windows::core::w!("EDIT"),
                windows::core::w!(""),
                WS_TABSTOP.0 as i32 | ES_LEFT | ES_AUTOHSCROLL | ES_PASSWORD,
            ),
            (
                windows::core::w!("BUTTON"),
                windows::core::w!("保護を有効化"),
                WS_TABSTOP.0 as i32,
            ),
            (
                windows::core::w!("BUTTON"),
                windows::core::w!("キャンセル"),
                WS_TABSTOP.0 as i32,
            ),
            (
                windows::core::w!("STATIC"),
                windows::core::w!("パスワードを失うと復元できません"),
                STATIC_CENTERED_ELLIPSIS,
            ),
        ];
        pad_caption::cloak(window, true);
        for (index, (class, label, style)) in specs.into_iter().enumerate() {
            match create_child(class, label, style, window, ENROLL_CONTROL_IDS[index]) {
                Ok(child) => self.enroll_controls[index] = child,
                Err(_) => {
                    self.destroy_enroll_controls();
                    pad_caption::cloak(window, false);
                    self.set_status("保護画面を開けません".to_owned());
                    self.update_status();
                    return;
                }
            }
        }
        for index in [2, 4] {
            // SAFETY: both indexed HWNDs were created above as Pad password
            // edits; EM_SETLIMITTEXT takes an integer limit and no pointer.
            unsafe {
                let _ = SendMessageW(
                    self.enroll_controls[index],
                    EM_SETLIMITTEXT,
                    Some(WPARAM(MAX_PASSWORD_UTF16_UNITS)),
                    Some(LPARAM(0)),
                );
            }
        }
        self.enroll_phase = EnrollPhase::Prompt;
        self.hide_normal_controls();
        self.apply_dpi(dpi_of(window));
        update_layout(self, window);
        // SAFETY: window is live and the flags request repaint without
        // passing an unowned region or update rectangle.
        unsafe {
            let _ = RedrawWindow(
                Some(window),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
        pad_caption::cloak(window, false);
        // SAFETY: the first password edit was created as a live Pad child.
        unsafe {
            let _ = SetFocus(Some(self.enroll_controls[2]));
        }
    }

    fn hide_normal_controls(&self) {
        for child in [
            self.menu,
            self.header_title,
            self.status,
            self.count,
            self.search,
            self.list,
            self.list_rail,
            self.title,
            self.body,
            self.body_rail,
            self.new,
            self.sort,
            self.sync,
            self.copy,
            self.delete,
            self.protect,
            self.memo_protect,
        ] {
            if !child.is_invalid() {
                // SAFETY: each non-invalid HWND is a live Pad child owned by
                // this state; hiding it does not destroy its editor content.
                unsafe {
                    let _ = ShowWindow(child, SW_HIDE);
                }
            }
        }
    }

    fn destroy_enroll_controls(&mut self) {
        for child in &mut self.enroll_controls {
            if !child.is_invalid() {
                // SAFETY: the enrollment state owns each created child and
                // clears the handle immediately after destroying it.
                unsafe {
                    let _ = DestroyWindow(*child);
                }
                *child = HWND::default();
            }
        }
    }

    fn cancel_enroll_prompt(&mut self, window: HWND) {
        if self.enroll_phase == EnrollPhase::MemoRecoveryPrompt {
            set_control_text(self.enroll_controls[2], "");
            if let Some(confirmation) = self.memo_recovery_confirmation.take() {
                let _ = confirmation.send(false);
            }
            self.enroll_phase = EnrollPhase::MemoProtectRunning;
            set_control_text(self.enroll_controls[7], "保護を中止しています…");
            // SAFETY: these are the live prompt controls while the worker
            // reaches its explicit terminal cancellation result.
            unsafe {
                let _ = EnableWindow(self.enroll_controls[5], false);
                let _ = EnableWindow(self.enroll_controls[6], false);
            }
            return;
        }
        if !matches!(
            self.enroll_phase,
            EnrollPhase::Prompt
                | EnrollPhase::MemoProtectPrompt
                | EnrollPhase::MemoRecoveryPrompt
                | EnrollPhase::MemoUnlockPrompt
        ) {
            return;
        }
        for index in [2, 4] {
            set_control_text(self.enroll_controls[index], "");
        }
        if let Some(confirmation) = self.memo_recovery_confirmation.take() {
            let _ = confirmation.send(false);
        }
        pad_caption::cloak(window, true);
        self.destroy_enroll_controls();
        self.enroll_phase = EnrollPhase::None;
        update_layout(self, window);
        // SAFETY: window remains live while cancelling the prompt; no
        // caller-owned drawing buffers are passed to USER32.
        unsafe {
            let _ = RedrawWindow(
                Some(window),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
        pad_caption::cloak(window, false);
        // SAFETY: the normal editor remains a live Pad child after layout.
        unsafe {
            let _ = SetFocus(Some(self.body));
        }
    }

    fn start_enrollment(&mut self, window: HWND) {
        if self.enroll_phase != EnrollPhase::Prompt {
            return;
        }
        for index in [2, 4] {
            // SAFETY: these two indexed HWNDs are live password edit controls
            // owned by the visible enrollment prompt.
            if unsafe { GetWindowTextLengthW(self.enroll_controls[index]) } as usize
                > MAX_PASSWORD_UTF16_UNITS
            {
                for clear in [2, 4] {
                    set_control_text(self.enroll_controls[clear], "");
                }
                set_control_text(self.enroll_controls[7], "パスワードが長すぎます");
                return;
            }
        }
        let first = get_password_text(self.enroll_controls[2]);
        let second = get_password_text(self.enroll_controls[4]);
        for index in [2, 4] {
            set_control_text(self.enroll_controls[index], "");
        }
        let (Some(mut first), Some(mut second)) = (first, second) else {
            set_control_text(self.enroll_controls[7], "パスワードを読み取れません");
            return;
        };
        if first.is_empty() || first != second {
            first.zeroize();
            second.zeroize();
            set_control_text(self.enroll_controls[7], "パスワードが一致しません");
            return;
        }
        second.zeroize();
        // This release has no recovery wrap. Ask for a separate, explicit
        // no-recovery decision before the legacy writer is stopped.
        // SAFETY: the live Pad HWND owns this modal dialog, and all message
        // and caption pointers refer to static NUL-terminated UTF-16 data.
        let no_recovery = unsafe {
            MessageBoxW(Some(window),
                windows::core::w!("この Pad 全体をパスワードで保護します。復旧キーは作成されません。パスワードを失うと、すべてのメモを復元できません。それでも続けますか？"),
                windows::core::w!("復旧手段なしで保護"), MB_YESNO | MB_ICONWARNING)
        };
        if no_recovery != IDYES {
            first.zeroize();
            set_control_text(
                self.enroll_controls[7],
                "保護を中止しました。Pad は保護されていません",
            );
            return;
        }
        let password = SecretBytes::new(first.as_bytes().to_vec());
        first.zeroize();
        // SAFETY: window is the live Pad HWND and the timer belongs to it.
        unsafe {
            let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
        }
        if self.capture_controls() {
            self.publish(window);
        }
        if self.save_blocked {
            self.cancel_enroll_prompt(window);
            self.set_status("保存を確認できません。メモをコピーして保管してください".to_owned());
            self.update_status();
            return;
        }
        let Some(mut legacy_worker) = self.worker.take() else {
            self.save_blocked = true;
            self.cancel_enroll_prompt(window);
            self.set_status("保存を確認できません。メモをコピーして保管してください".to_owned());
            self.update_status();
            return;
        };
        if !legacy_worker.shutdown(SHUTDOWN_FLUSH_BUDGET) {
            self.save_blocked = true;
            self.cancel_enroll_prompt(window);
            self.set_status("保存を確認できません。メモをコピーして保管してください".to_owned());
            self.update_status();
            return;
        }
        // SAFETY: window is live and this pending completion timer belongs to
        // the stopped legacy storage worker.
        unsafe {
            let _ = KillTimer(Some(window), PAD_COMPLETION_TIMER);
        }
        let store = match PadStore::default() {
            Ok(store) => store,
            Err(_) => {
                self.save_blocked = true;
                self.cancel_enroll_prompt(window);
                self.set_status(
                    "保存先を確認できません。メモをコピーして保管してください".to_owned(),
                );
                self.update_status();
                return;
            }
        };
        let expected = self.document.clone();
        self.enroll_epoch = self.enroll_epoch.wrapping_add(1);
        let epoch = self.enroll_epoch;
        let (sender, receiver) = mpsc::channel();
        let raw_window = window.0 as isize;
        let spawn = thread::Builder::new()
            .name("sakura-pad-enroll".to_owned())
            .spawn(move || {
                let mut engine = PadProtectionEngine::new(Duration::from_secs(15));
                let result = engine.enroll(&store, &expected, password);
                let legacy_exact = result.is_err()
                    && store.load().is_ok_and(|loaded| {
                        !loaded.recovered_from_backup && loaded.document == expected
                    });
                drop(engine);
                if sender
                    .send(EnrollCompletion {
                        epoch,
                        result,
                        legacy_exact,
                    })
                    .is_ok()
                {
                    // SAFETY: raw_window came from the Pad HWND before this
                    // thread started. A stale HWND is harmless: the UI-side
                    // receiver and epoch gate any eventual message.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(raw_window as *mut c_void)),
                            WM_PAD_ENROLL_FINISHED,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            });
        if spawn.is_err() {
            self.save_blocked = true;
            self.cancel_enroll_prompt(window);
            self.set_status("保護を開始できません。メモをコピーして保管してください".to_owned());
            self.update_status();
            return;
        }
        self.enroll_result = Some(receiver);
        self.enroll_phase = EnrollPhase::Running;
        // No legacy child text survives while a protected cutover may publish.
        pad_caption::cloak(window, true);
        self.destroy_normal_controls();
        set_control_text(self.enroll_controls[7], "保護へ切り替えています…");
        // SAFETY: both HWNDs are live enrollment buttons; disable them while
        // the one in-flight cutover owns this state transition.
        unsafe {
            let _ = EnableWindow(self.enroll_controls[5], false);
            let _ = EnableWindow(self.enroll_controls[6], false);
        }
        update_layout(self, window);
        pad_caption::cloak(window, false);
    }

    fn finish_enrollment(&mut self, window: HWND) {
        let Some(completion) = self
            .enroll_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        else {
            return;
        };
        if self.enroll_phase != EnrollPhase::Running || completion.epoch != self.enroll_epoch {
            return;
        }
        self.enroll_result = None;
        pad_caption::cloak(window, true);
        self.destroy_enroll_controls();
        self.enroll_phase = EnrollPhase::None;
        let mut reading = "Pad を保護しました。パスワードで解除してください";
        match completion.result {
            Ok(()) => {
                self.locked = true;
                self.save_blocked = true;
                self.document = PadDocument::default();
                self.rows.clear();
                self.query.clear();
                let _ = create_controls(self, window);
            }
            Err(error) if error.phase == FailurePhase::BeforeIntent => {
                // No protected intent was published. Keep the in-memory
                // editor available for copying even if the disk snapshot
                // changed; only the exact original may restart a writer.
                self.worker = completion
                    .legacy_exact
                    .then(|| {
                        PadStore::default()
                            .ok()
                            .and_then(|store| StorageWorker::spawn(store).ok())
                    })
                    .flatten();
                self.save_blocked = self.worker.is_none();
                let _ = create_controls(self, window);
                reading = if self.save_blocked {
                    "保護できず、保存も確認できません。メモをコピーしてください"
                } else {
                    "保護できませんでした。Pad は保護されていません"
                };
                self.set_status(reading.to_owned());
                self.update_status();
            }
            Err(_) => {
                // Intent may already be durable. Never restart a v2 writer
                // or recreate old memo child text after an uncertain cutover.
                // Retain this draft until an authenticated load can check it
                // against a known base; without that base it stays copy-only.
                self.unsaved_recovery = Some(self.document.clone());
                self.recovery_base = None;
                self.recovery_conflict = true;
                self.locked = true;
                self.save_blocked = true;
                self.document = PadDocument::default();
                self.rows.clear();
                self.query.clear();
                let _ = create_controls(self, window);
                reading = "保護の切替を確認できません。復旧が必要です";
            }
        }
        if self.locked {
            set_control_text(self.lock_status, reading);
        }
        update_layout(self, window);
        // SAFETY: window remains live throughout this UI-thread transition;
        // no external drawing buffers are passed to USER32.
        unsafe {
            let _ = RedrawWindow(
                Some(window),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
        pad_caption::cloak(window, false);
        let focus = if self.locked {
            self.lock_password
        } else {
            self.body
        };
        // SAFETY: focus is the live child created for the resulting surface.
        unsafe {
            let _ = SetFocus(Some(focus));
        }
    }
    fn update_unlock_retry(&mut self) -> bool {
        let Some(deadline) = self.unlock_retry_at else {
            return false;
        };
        if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            set_control_text(
                self.lock_status,
                &format!(
                    "パスワードが正しくありません。再試行まで {} 秒",
                    remaining.as_secs() + 1
                ),
            );
            return true;
        }
        self.unlock_retry_at = None;
        set_control_text(self.lock_status, "再試行できます");
        // SAFETY: self.window is this live Pad HWND; only its own retry timer
        // is cancelled after the monotonic deadline has elapsed.
        unsafe {
            let _ = KillTimer(Some(self.window), PAD_UNLOCK_RETRY_TIMER);
        }
        false
    }

    fn start_unlock(&mut self) {
        if !self.locked || self.unlock_in_flight {
            return;
        }
        if self.v4_mode {
            set_control_text(self.lock_status, "メモ別保護の復旧が必要です");
            return;
        }
        if self.update_unlock_retry() {
            return;
        }
        // WM_SETTEXT can bypass a user-typing limit. Reject rather than
        // silently truncating a password and trying a different secret.
        // SAFETY: lock_password is the live edit child of the locked Pad.
        if unsafe { GetWindowTextLengthW(self.lock_password) } as usize > MAX_PASSWORD_UTF16_UNITS {
            set_control_text(self.lock_password, "");
            set_control_text(self.lock_status, "パスワードが長すぎます");
            return;
        }
        let password = get_password_text(self.lock_password);
        // Erase the edit before dispatch; a queued worker owns only the
        // zeroizing byte buffer, and no hidden HWND retains the passphrase.
        set_control_text(self.lock_password, "");
        let Some(mut password) = password else {
            set_control_text(self.lock_status, "パスワードを読み取れません");
            return;
        };
        if password.is_empty() {
            set_control_text(self.lock_status, "パスワードを入力してください");
            return;
        }
        let secret = SecretBytes::new(password.as_bytes().to_vec());
        password.zeroize();
        self.unlock_epoch = self.unlock_epoch.wrapping_add(1);
        let epoch = self.unlock_epoch;
        let (sender, receiver) = mpsc::channel();
        let store = match PadStore::default() {
            Ok(store) => store,
            Err(_) => {
                set_control_text(self.lock_status, "保護されたメモを開けません");
                return;
            }
        };
        let window = self.window.0 as isize;
        let spawn = thread::Builder::new()
            .name("sakura-pad-unlock".to_owned())
            .spawn(move || {
                let mut engine = PadProtectionEngine::new(Duration::from_secs(15));
                let result = engine.unlock(&store, secret).map(|loaded| (loaded, engine));
                if sender.send(UnlockCompletion { epoch, result }).is_ok() {
                    // SAFETY: posting is harmless if the Pad closed during the
                    // bounded request; the epoch and receiver gate delivery.
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(window as *mut c_void)),
                            WM_PAD_UNLOCK_FINISHED,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            });
        match spawn {
            Ok(_) => {
                self.unlock_result = Some(receiver);
                self.unlock_in_flight = true;
                set_control_text(self.lock_status, "解除しています…");
                // SAFETY: the button belongs to this Pad and is disabled for
                // the entire request, including retries and completion.
                unsafe {
                    let _ = EnableWindow(self.lock_unlock, false);
                }
            }
            Err(_) => set_control_text(self.lock_status, "解除を開始できません"),
        }
    }

    fn cancel_unlock(&mut self) {
        let cancelled = self.unlock_in_flight;
        self.unlock_epoch = self.unlock_epoch.wrapping_add(1);
        self.unlock_in_flight = false;
        self.unlock_result = None;
        if self.locked {
            set_control_text(self.lock_password, "");
            if cancelled {
                set_control_text(self.lock_status, "解除を中止しました");
            } else if self.unlock_retry_at.is_some() {
                self.update_unlock_retry();
            }
            // SAFETY: the child button remains live in locked mode.
            unsafe {
                let _ = EnableWindow(self.lock_unlock, true);
            }
        }
    }

    fn finish_unlock(&mut self, window: HWND) {
        let Some(completion) = self
            .unlock_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        else {
            return;
        };
        if !self.locked || !self.unlock_in_flight || completion.epoch != self.unlock_epoch {
            return;
        }
        self.unlock_result = None;
        self.unlock_in_flight = false;
        let (loaded, engine) = match completion.result {
            Ok(success) => success,
            Err(error) => {
                let status = match error.reason {
                    FailureReason::Authentication => {
                        self.unlock_failures = self.unlock_failures.saturating_add(1);
                        self.unlock_retry_at =
                            Some(Instant::now() + unlock_retry_delay(self.unlock_failures));
                        // SAFETY: window is the live Pad HWND. The timer only
                        // refreshes a monotonic retry countdown on its UI thread.
                        unsafe {
                            let _ = SetTimer(Some(window), PAD_UNLOCK_RETRY_TIMER, 250, None);
                        }
                        self.update_unlock_retry();
                        // SAFETY: lock_unlock remains a live child of the
                        // locked surface. Clicks during cooldown are bounded
                        // by the monotonic deadline before any Argon2 work.
                        unsafe {
                            let _ = EnableWindow(self.lock_unlock, true);
                        }
                        return;
                    }
                    FailureReason::Unavailable => "保護機能を利用できません",
                    _ => "メモを解除できませんでした",
                };
                set_control_text(self.lock_status, status);
                // SAFETY: the live button can accept another explicit attempt.
                unsafe {
                    let _ = EnableWindow(self.lock_unlock, true);
                }
                return;
            }
        };
        let store = match PadStore::default() {
            Ok(store) => store,
            Err(_) => {
                set_control_text(self.lock_status, "保護されたメモを開けません");
                // SAFETY: lock_unlock is still a live child while locked;
                // re-enable it for a later explicit retry.
                unsafe {
                    let _ = EnableWindow(self.lock_unlock, true);
                }
                return;
            }
        };
        let Ok(actor) = ProtectedSaveActor::spawn_unlocked(store, engine, loaded.document.clone())
        else {
            set_control_text(self.lock_status, "保護された保存を開始できません");
            // SAFETY: lock_unlock remains live because the locked surface
            // has not yet been replaced with memo controls.
            unsafe {
                let _ = EnableWindow(self.lock_unlock, true);
            }
            return;
        };
        self.unlock_failures = 0;
        self.unlock_retry_at = None;
        // SAFETY: window is the live Pad HWND; no retry timer is needed after
        // a successful authenticated unlock.
        unsafe {
            let _ = KillTimer(Some(window), PAD_UNLOCK_RETRY_TIMER);
        }
        // The document was authenticated before any ordinary HWND exists.
        // Cloak through child creation so neither sighted users nor screen
        // readers observe a partial editor during the transition.
        pad_caption::cloak(window, true);
        let mut shown_document = loaded.document;
        let mut restored_unsaved = false;
        let mut recovery_rejected = false;
        let mut recovery_conflict = false;
        if let Some(mut recovery) = self.unsaved_recovery.take() {
            if recovery != shown_document {
                if !self.recovery_conflict && self.recovery_base.as_ref() == Some(&shown_document) {
                    recovery.generation = recovery
                        .generation
                        .max(shown_document.generation)
                        .wrapping_add(1);
                    recovery_rejected = actor.submit(recovery.clone()).is_err();
                } else {
                    // Authenticated persisted content changed since this
                    // draft's last confirmed base. Show the retained draft
                    // for copying, but never auto-publish over that change.
                    recovery_conflict = true;
                    self.recovery_conflict = true;
                    recovery_rejected = true;
                    self.unsaved_recovery = Some(recovery.clone());
                }
                shown_document = recovery;
                restored_unsaved = true;
            }
        }
        if !recovery_conflict {
            self.recovery_base = None;
            self.recovery_conflict = false;
        }
        self.protected_worker = Some(actor);
        self.protected_session_seen = true;
        self.document = shown_document;
        self.active = self
            .document
            .live()
            .next()
            .map(|memo| memo.id)
            .unwrap_or_else(|| self.document.next_id());
        self.generation = self.document.generation;
        self.latest_submitted = self.generation;
        self.locked = false;
        self.save_blocked = recovery_rejected;
        if restored_unsaved {
            self.set_status(if recovery_conflict {
                "保存済みのメモが変更されました。未保存のメモをコピーしてください".to_owned()
            } else if recovery_rejected {
                "未保存のメモを復元しました。保存できません".to_owned()
            } else {
                "未保存のメモを復元し、保存しています…".to_owned()
            });
            if !recovery_rejected {
                // SAFETY: window is the live Pad HWND; this timer polls only
                // the protected actor for the pending accepted snapshot.
                unsafe {
                    let _ = SetTimer(Some(window), PAD_COMPLETION_TIMER, 100, None);
                }
            }
        } else {
            self.set_status(String::new());
        }
        for child in [
            self.lock_headline,
            self.lock_password_label,
            self.lock_password,
            self.lock_unlock,
            self.lock_status,
        ] {
            // SAFETY: these are live children, no longer needed after auth.
            unsafe {
                let _ = DestroyWindow(child);
            }
        }
        self.lock_headline = HWND::default();
        self.lock_password_label = HWND::default();
        self.lock_password = HWND::default();
        self.lock_unlock = HWND::default();
        self.lock_status = HWND::default();
        if create_controls(self, window).is_err() {
            // Control construction failed. Never expose a partly initialized
            // editor or accept edits without a complete native surface.
            self.save_blocked = true;
            self.locked = true;
            self.unsaved_recovery = Some(self.document.clone());
            self.protected_worker = None;
            self.destroy_normal_controls();
            self.document = PadDocument::default();
            let _ = create_controls(self, window);
            set_control_text(self.lock_status, "メモ画面を作成できません");
        }
        update_layout(self, window);
        // SAFETY: the Pad HWND remains live throughout this UI-thread change.
        unsafe {
            let _ = RedrawWindow(
                Some(window),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
        pad_caption::cloak(window, false);
        if !self.locked {
            // SAFETY: the normal editor now exists and may receive focus.
            unsafe {
                let _ = SetFocus(Some(self.body));
            }
        }
    }

    fn destroy_normal_controls(&mut self) {
        self.tooltips = None;
        for child in [
            self.menu,
            self.header_title,
            self.status,
            self.count,
            self.search,
            self.list,
            self.list_rail,
            self.title,
            self.body,
            self.body_rail,
            self.new,
            self.sort,
            self.sync,
            self.copy,
            self.delete,
            self.protect,
            self.memo_protect,
        ] {
            if !child.is_invalid() {
                // SAFETY: every initialized HWND is a child of this Pad.
                unsafe {
                    let _ = DestroyWindow(child);
                }
            }
        }
        self.menu = HWND::default();
        self.header_title = HWND::default();
        self.status = HWND::default();
        self.count = HWND::default();
        self.search = HWND::default();
        self.list = HWND::default();
        self.list_rail = HWND::default();
        self.title = HWND::default();
        self.body = HWND::default();
        self.body_rail = HWND::default();
        self.new = HWND::default();
        self.sort = HWND::default();
        self.sync = HWND::default();
        self.copy = HWND::default();
        self.delete = HWND::default();
        self.protect = HWND::default();
        self.memo_protect = HWND::default();
    }

    fn lock_protected(&mut self, window: HWND, mask_even_if_unsaved: bool) -> bool {
        if self.v4_mode && !self.locked {
            return self.finish_v4_save_for_close(window, mask_even_if_unsaved);
        }
        if self.locked {
            self.cancel_unlock();
            return true;
        }
        if self.protected_worker.is_none() {
            if self.protected_session_seen && self.save_blocked {
                if mask_even_if_unsaved {
                    self.unsaved_recovery = Some(self.document.clone());
                    self.mask_protected_contents(
                        window,
                        "保存を確認できません。未保存のメモを保持しています",
                    );
                    return true;
                }
                return false;
            }
            return true;
        }
        // Capture the final edit before controls are destroyed and before the
        // actor stops accepting complete snapshots.
        // SAFETY: window is the live Pad HWND and owns this edit timer.
        unsafe {
            let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
        }
        if self.capture_controls() {
            self.publish(window);
        }
        let mut actor = self
            .protected_worker
            .take()
            .expect("checked protected actor");
        actor.begin_lock();
        let outcome = actor.finish_lock(SHUTDOWN_FLUSH_BUDGET);
        let unsaved = !matches!(outcome.status, ProtectedLockStatus::Saved)
            || self.save_blocked
            || self.document.generation > outcome.confirmed_generation;
        if unsaved {
            let actor_draft = outcome.latest_unsaved.as_ref().map(|doc| doc.as_ref());
            self.unsaved_recovery = Some(match actor_draft {
                Some(doc) if doc.generation > self.document.generation => doc.clone(),
                _ => self.document.clone(),
            });
            if !self.recovery_conflict {
                self.recovery_base = Some((*outcome.confirmed_document).clone());
            }
        } else {
            self.unsaved_recovery = None;
            self.recovery_base = None;
            self.recovery_conflict = false;
        }
        if unsaved && !mask_even_if_unsaved {
            self.save_blocked = true;
            self.set_status(match outcome.status {
                ProtectedLockStatus::Unsaved(_) => {
                    "保存に失敗しました。メモをコピーして保管してください".to_owned()
                }
                _ => "保存を確認できません。メモをコピーして保管してください".to_owned(),
            });
            self.update_status();
            return false;
        }
        let reading = if unsaved {
            "保存を確認できません。未保存のメモを保持しています"
        } else {
            "保存してロックしました"
        };
        self.mask_protected_contents(window, reading);
        true
    }

    fn finish_v4_save_for_close(&mut self, window: HWND, mask_even_if_unsaved: bool) -> bool {
        // The v4 document stores opaque envelopes. A close may only leave
        // after its latest full snapshot is confirmed by the v4 save actor.
        if mask_even_if_unsaved {
            self.force_lock_open_memo(window);
        } else if !self.close_open_memo(window) {
            return false;
        }
        // SAFETY: window is the live Pad HWND and owns this edit timer.
        unsafe {
            let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
        }
        if self.capture_controls() {
            self.publish(window);
        }
        let Some(mut actor) = self.protected_worker.take() else {
            self.save_blocked = true;
            self.set_status("保存を確認できません。メモをコピーしてください".to_owned());
            self.update_status();
            return mask_even_if_unsaved;
        };
        actor.begin_lock();
        let outcome = actor.finish_lock(SHUTDOWN_FLUSH_BUDGET);
        let saved = matches!(outcome.status, ProtectedLockStatus::Saved)
            && !self.save_blocked
            && self.document == *outcome.confirmed_document;
        if !saved {
            self.save_blocked = true;
            self.set_status("保存を確認できません。メモをコピーしてください".to_owned());
            self.update_status();
            return mask_even_if_unsaved;
        }
        self.memo_save_pending = None;
        let restarted = PadStore::default()
            .ok()
            .and_then(|store| ProtectedSaveActor::spawn_v4(store, self.document.clone()).ok());
        self.protected_worker = restarted;
        if self.protected_worker.is_none() {
            self.save_blocked = true;
            self.set_status("保存機能を再開できません。メモをコピーしてください".to_owned());
            self.update_status();
            return mask_even_if_unsaved;
        }
        true
    }

    fn mask_protected_contents(&mut self, window: HWND, reading: &str) {
        // Never let an unlocked child or its UI Automation text survive the
        // transition back to the locked surface.
        pad_caption::cloak(window, true);
        self.destroy_normal_controls();
        self.document = PadDocument::default();
        self.rows.clear();
        self.query.clear();
        self.locked = true;
        self.save_blocked = true;
        let _ = create_controls(self, window);
        set_control_text(self.lock_status, reading);
        update_layout(self, window);
        // SAFETY: window remains live during masking; its timers and child
        // paint requests use no caller-owned buffers or pointers.
        unsafe {
            let _ = KillTimer(Some(window), PAD_COMPLETION_TIMER);
            let _ = KillTimer(Some(window), PAD_NOTICE_TIMER);
            let _ = RedrawWindow(
                Some(window),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
        pad_caption::cloak(window, false);
    }

    fn now(&self) -> Option<CalendarTime> {
        pad_list::local_time(now_ms())
    }

    fn apply_dpi(&mut self, dpi: u32) {
        if self.fonts.dpi != dpi || self.fonts.body.is_invalid() {
            let mut replaced = std::mem::replace(&mut self.fonts, PadFonts::new(dpi));
            replaced.destroy();
        }
        let assignments = [
            (self.lock_headline, self.fonts.heading),
            (self.lock_password_label, self.fonts.small),
            (self.lock_password, self.fonts.body),
            (self.lock_unlock, self.fonts.body),
            (self.lock_status, self.fonts.small),
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
            (self.protect, self.fonts.small),
            (self.memo_protect, self.fonts.small),
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
            for (index, child) in self.enroll_controls.iter().enumerate() {
                if !child.is_invalid() {
                    let font = if index == 0 {
                        self.fonts.heading
                    } else {
                        self.fonts.body
                    };
                    let _ = SendMessageW(
                        *child,
                        WM_SETFONT,
                        Some(WPARAM(font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
            if !self.list.is_invalid() {
                let _ = SendMessageW(
                    self.list,
                    LB_SETITEMHEIGHT,
                    Some(WPARAM(0)),
                    Some(LPARAM(scaled(pad_list::ROW_HEIGHT_96, dpi) as isize)),
                );
            }
        }
        // The ruled squares are a physical size too.
        self.refresh_brushes();
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
                    .map(|memo| {
                        let opened = self
                            .memo_open
                            .as_ref()
                            .filter(|open| open.id == memo.id)
                            .map(|open| pad_list::UnlockedMemo {
                                title: &open.title,
                                body: &open.body,
                            });
                        pad_list::projection(memo, opened).title().to_owned()
                    })
                    .unwrap_or_else(|| pad_list::UNTITLED.to_owned());
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
        if let Some(open) = self
            .memo_open
            .as_ref()
            .filter(|open| open.id == self.active)
        {
            set_control_text(self.memo_protect, "このメモをロック");
            self.updating_controls = true;
            set_control_text(self.title, &open.title);
            set_control_text(self.body, &open.body);
            self.updating_controls = false;
            // SAFETY: both edits are live children; only this authenticated
            // memo's decrypted text is placed into them.
            unsafe {
                let _ = EnableWindow(self.title, true);
                let _ = EnableWindow(self.body, true);
            }
            return;
        }
        let memo_is_protected = self
            .document
            .find(self.active)
            .is_some_and(|memo| memo.protected_envelope().is_some());
        set_control_text(
            self.memo_protect,
            if memo_is_protected {
                "このメモを解除"
            } else {
                "このメモをパスワードで保護"
            },
        );
        let (title, body) = self
            .document
            .find(self.active)
            .and_then(PadMemo::plain_content)
            .map(|(title, body)| (title.to_owned(), body.to_owned()))
            .unwrap_or_default();
        self.updating_controls = true;
        set_control_text(self.title, &title);
        set_control_text(self.body, &body);
        self.updating_controls = false;
        // SAFETY: both HWNDs are live editor children. An encrypted memo has
        // no title/body to edit until its own password authenticates it.
        unsafe {
            let _ = EnableWindow(self.title, !memo_is_protected);
            let _ = EnableWindow(self.body, !memo_is_protected);
        }
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
        if self.locked || self.updating_controls || self.title.is_invalid() {
            return false;
        }
        let title = get_control_text(self.title, MAX_TITLE_UTF16_UNITS);
        let body = get_control_text(self.body, MAX_BODY_UTF16_UNITS);
        if let Some(open) = self
            .memo_open
            .as_mut()
            .filter(|open| open.id == self.active)
        {
            if *open.title == title && *open.body == body {
                return false;
            }
            open.title = Zeroizing::new(title);
            open.body = Zeroizing::new(body);
            open.dirty = true;
            open.revision = open.revision.wrapping_add(1);
            return true;
        }
        if self
            .document
            .find(self.active)
            .is_some_and(|memo| memo.protected_envelope().is_some())
        {
            return false;
        }
        if self.document.find(self.active).is_none() && title.is_empty() && body.is_empty() {
            // An untouched new memo is not a memo yet.
            return false;
        }
        let now = now_ms();
        let active = self.active;
        let outcome = self.document.entry(active, now).map(|memo| {
            if memo.plain_content() == Some((title.as_str(), body.as_str())) {
                false
            } else {
                memo.edit(title, body, now).is_ok()
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
        if self.locked {
            return;
        }
        if self.save_blocked {
            self.set_status("既存データを保護するため保存を停止しています".to_owned());
            self.update_status();
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.document.generation = self.generation;
        self.latest_submitted = self.generation;
        let protected_submission = self
            .protected_worker
            .as_ref()
            .map(|worker| worker.submit(self.document.clone()));
        let mut rejected_status = None;
        let submitted = if let Some(result) = protected_submission {
            match result {
                Ok(()) => true,
                Err(rejected) => {
                    self.unsaved_recovery = Some(rejected.document);
                    rejected_status = Some(match rejected.reason {
                        SubmitRejectReason::Stale => {
                            "保存順序が古くなりました。未保存のメモを保持しています"
                        }
                        SubmitRejectReason::Closing => {
                            "ロック処理中です。未保存のメモを保持しています"
                        }
                        SubmitRejectReason::Failed => {
                            "保存に失敗しました。未保存のメモを保持しています"
                        }
                    });
                    false
                }
            }
        } else {
            self.worker
                .as_mut()
                .is_some_and(|worker| worker.submit(self.document.clone()))
        };
        if submitted {
            self.set_status("保存中…".to_owned());
            // SAFETY: the window is live for the lifetime of this state.
            unsafe {
                let _ = SetTimer(Some(window), PAD_COMPLETION_TIMER, 100, None);
            }
        } else {
            self.save_blocked = true;
            self.set_status(
                rejected_status
                    .unwrap_or("保存できません。未保存のメモを保持しています")
                    .to_owned(),
            );
        }
        self.update_status();
    }

    /// Returns true once the latest submitted generation has a terminal
    /// completion and the polling timer can stop.
    fn poll_storage(&mut self) -> bool {
        if self.locked {
            return true;
        }
        let mut terminal = false;
        if self.protected_worker.is_some() {
            let completion = self
                .protected_worker
                .as_ref()
                .and_then(ProtectedSaveActor::try_completion);
            if let Some(completion) = completion {
                match completion.status {
                    ProtectedSaveStatus::Written(_)
                        if completion.generation == self.latest_submitted =>
                    {
                        if self
                            .memo_protect_pending
                            .as_ref()
                            .is_some_and(|pending| pending.generation == completion.generation)
                        {
                            self.finish_v3_memo_protection(self.window);
                            terminal = true;
                            return terminal;
                        }
                        if self
                            .memo_save_pending
                            .is_some_and(|pending| completion.generation >= pending)
                        {
                            self.memo_save_pending = None;
                        }
                        self.set_status(String::new());
                        terminal = true;
                    }
                    ProtectedSaveStatus::Written(_) => {
                        if self
                            .memo_save_pending
                            .is_some_and(|pending| completion.generation >= pending)
                        {
                            self.memo_save_pending = None;
                        }
                    }
                    ProtectedSaveStatus::Failed(_) => {
                        if self.memo_protect_pending.is_some() {
                            self.enter_memo_cutover_recovery(
                                self.window,
                                "メモ別保護の保存を確認できません。復旧が必要です",
                            );
                            terminal = true;
                            return terminal;
                        }
                        self.save_blocked = true;
                        self.set_status(
                            "保護された保存に失敗しました。未保存のメモを保持しています".to_owned(),
                        );
                        terminal = true;
                    }
                }
            }
            self.update_status();
            return terminal;
        }
        while let Some(completion) = self.worker.as_ref().and_then(StorageWorker::try_completion) {
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
            return;
        }
        // Stable row IDs do not imply stable titles. Keep the LISTBOX's
        // accessible label in sync with the owner-drawn text without resetting
        // the list's selection or scroll position on each debounced edit.
        let Some(index) = self.rows.iter().position(|id| *id == self.active) else {
            return;
        };
        let Some(memo) = self.document.find(self.active) else {
            return;
        };
        let opened = self
            .memo_open
            .as_ref()
            .filter(|open| open.id == memo.id)
            .map(|open| pad_list::UnlockedMemo {
                title: &open.title,
                body: &open.body,
            });
        let label: Vec<u16> = pad_list::projection(memo, opened)
            .title()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let selection = selected_row(self.list).unwrap_or(usize::MAX);
        // SAFETY: the list belongs to this state and the string outlives the
        // synchronous insert. These messages do not send selection notifications.
        unsafe {
            let top = SendMessageW(self.list, LB_GETTOPINDEX, None, None);
            let removed = SendMessageW(self.list, LB_DELETESTRING, Some(WPARAM(index)), None);
            if removed.0 < 0
                || SendMessageW(
                    self.list,
                    LB_INSERTSTRING,
                    Some(WPARAM(index)),
                    Some(LPARAM(label.as_ptr() as isize)),
                )
                .0 < 0
            {
                self.refresh_list();
                return;
            }
            let _ = SendMessageW(self.list, LB_SETCURSEL, Some(WPARAM(selection)), None);
            if top.0 >= 0 {
                let _ = SendMessageW(
                    self.list,
                    LB_SETTOPINDEX,
                    Some(WPARAM(top.0 as usize)),
                    None,
                );
            }
        }
        // Invalidate after capture; EN_CHANGE still paints the previous snapshot.
        self.invalidate_rows();
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
            if !self.close_open_memo(window) {
                return;
            }
            let changed = self.capture_controls();
            self.active = id;
            self.refresh_editor();
            changed
        };
        if changed {
            // The memo just left behind may belong somewhere else now. This
            // is the moment to move it: the user is no longer reading it.
            self.refresh_list();
            self.publish(window);
        }
        self.update_status();
        self.invalidate_rows();
        if open {
            self.set_pane(PadPane::Editor, window, self.body);
        }
    }

    fn create_memo(&mut self, window: HWND) {
        if !self.close_open_memo(window) {
            return;
        }
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
        if !self.close_open_memo(window) {
            return;
        }
        if self.save_blocked {
            self.set_status("既存データを保護するため削除を保存できません".to_owned());
            self.update_status();
            return;
        }
        if self
            .document
            .find(self.active)
            .is_some_and(|memo| memo.protected_envelope().is_some())
        {
            self.set_status("保護されたメモは削除できません".to_owned());
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
        if self.locked {
            return;
        }
        if !self.close_open_memo(window) {
            return;
        }
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
        if self.locked {
            return;
        }
        if self.updating_controls {
            return;
        }
        self.query = get_control_text(self.search, MAX_QUERY_UTF16_UNITS);
        self.refresh_list();
        self.invalidate_rows();
    }

    fn copy_memo(&mut self, window: HWND) {
        if self.locked {
            return;
        }
        let changed = self.capture_controls();
        if self.memo_open.is_some() {
            if changed {
                self.start_memo_reseal(window);
            }
            let Some(open) = self.memo_open.as_ref() else {
                return;
            };
            let mut markdown = if open.title.trim().is_empty() {
                (*open.body).clone()
            } else {
                format!("# {}\n\n{}", open.title.as_str(), open.body.as_str())
            };
            let copied = copy_to_clipboard(window, &markdown);
            markdown.zeroize();
            let expiry_armed = if copied {
                // SAFETY: this reads the clipboard sequence immediately
                // after our successful write and owns the Pad timer below.
                unsafe {
                    self.protected_clipboard_sequence = Some(GetClipboardSequenceNumber());
                    SetTimer(
                        Some(window),
                        PAD_PROTECTED_CLIPBOARD_TIMER,
                        PROTECTED_CLIPBOARD_LIFETIME_MS,
                        None,
                    ) != 0
                }
            } else {
                false
            };
            if copied && !expiry_armed {
                // A protected copy must not outlive the promised clearing
                // window when USER32 could not arm its expiration timer.
                self.expire_protected_clipboard();
            }
            self.notify(if expiry_armed {
                "コピーしました。15 秒後にクリップボードを消去します".to_owned()
            } else {
                "コピーできません".to_owned()
            });
            self.update_status();
            return;
        }
        if changed {
            self.sync_rows();
            self.publish(window);
        }
        let Some(memo) = self.document.find(self.active) else {
            self.notify("メモがありません".to_owned());
            self.update_status();
            return;
        };
        let Some((title, body)) = memo.plain_content() else {
            self.notify("保護されたメモは解除が必要です".to_owned());
            self.update_status();
            return;
        };
        let markdown = if title.trim().is_empty() {
            body.to_owned()
        } else {
            format!("# {title}\n\n{body}")
        };
        self.notify(if copy_to_clipboard(window, &markdown) {
            "コピーしました".to_owned()
        } else {
            "コピーできません".to_owned()
        });
        self.update_status();
    }

    fn expire_protected_clipboard(&mut self) {
        let Some(sequence) = self.protected_clipboard_sequence.take() else {
            return;
        };
        // SAFETY: this checks that no other application has replaced the
        // clipboard since this Pad copied the protected memo. Only then does
        // the Pad empty the clipboard it previously populated.
        unsafe {
            let _ = KillTimer(Some(self.window), PAD_PROTECTED_CLIPBOARD_TIMER);
            if GetClipboardSequenceNumber() == sequence && OpenClipboard(Some(self.window)).is_ok()
            {
                let _ = EmptyClipboard();
                let _ = CloseClipboard();
            }
        }
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
            pad_caption::hide_icon(window);
            pad_caption::dress(window, state.theme);
            if create_controls(state, window).is_err() {
                // SAFETY: posting to the window being created is legal and the
                // negative result aborts creation.
                unsafe {
                    let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                return LRESULT(-1);
            }
            if state.locked && state.v4_mode {
                set_control_text(state.lock_status, &state.status_message);
                // SAFETY: lock_unlock is a live child, but a damaged v4
                // cutover cannot be opened with the whole-Pad password path.
                unsafe {
                    let _ = EnableWindow(state.lock_unlock, false);
                }
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
                if id == PROTECT_ID || id == MEMO_PROTECT_ID {
                    let previous = select_font(item.hDC, state.fonts.small);
                    draw_protect_button(
                        item,
                        colors,
                        dpi,
                        if id == PROTECT_ID { "保護" } else { "鍵" },
                    );
                    if let Some(previous) = previous {
                        // SAFETY: previous was selected out of this same
                        // DRAWITEMSTRUCT HDC just above; restore it before
                        // returning the drawing context to USER32.
                        unsafe {
                            let _ = windows::Win32::Graphics::Gdi::SelectObject(item.hDC, previous);
                        }
                    }
                } else if let Some(face) = button_face(id, wide) {
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
            if state.enroll_phase != EnrollPhase::None {
                let id = (w.0 & 0xffff) as u16;
                let code = ((w.0 >> 16) & 0xffff) as u16;
                if code == BN_CLICKED as u16 {
                    match id {
                        ENROLL_SUBMIT_ID if state.enroll_phase == EnrollPhase::Prompt => {
                            state.start_enrollment(window)
                        }
                        ENROLL_SUBMIT_ID
                            if matches!(
                                state.enroll_phase,
                                EnrollPhase::MemoProtectPrompt
                                    | EnrollPhase::MemoRecoveryPrompt
                                    | EnrollPhase::MemoUnlockPrompt
                            ) =>
                        {
                            state.start_memo_operation(window)
                        }
                        ENROLL_CONFIRM_LABEL_ID
                            if state.enroll_phase == EnrollPhase::MemoUnlockPrompt =>
                        {
                            state.toggle_memo_unlock_method()
                        }
                        ENROLL_CANCEL_ID => state.cancel_enroll_prompt(window),
                        _ => {}
                    }
                }
                return LRESULT(0);
            }
            if state.locked {
                let id = (w.0 & 0xffff) as u16;
                let code = ((w.0 >> 16) & 0xffff) as u16;
                if id == LOCK_UNLOCK_ID && code == BN_CLICKED as u16 {
                    state.start_unlock();
                }
                return LRESULT(0);
            }
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
                (PROTECT_ID, value) if value == BN_CLICKED as u16 => {
                    state.show_enroll_prompt(window)
                }
                (MEMO_PROTECT_ID, value) if value == BN_CLICKED as u16 => {
                    state.show_memo_prompt(window)
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_PAD_UNLOCK_FINISHED if !state_ptr.is_null() => {
            // SAFETY: completion was posted to this live Pad HWND; the epoch
            // check inside rejects a result from an earlier visible session.
            let state = unsafe { &mut *state_ptr };
            state.finish_unlock(window);
            LRESULT(0)
        }
        WM_PAD_ENROLL_FINISHED if !state_ptr.is_null() => {
            // SAFETY: state_ptr was stored on this live Pad HWND and the
            // procedure is executing on its single UI thread.
            let state = unsafe { &mut *state_ptr };
            state.finish_enrollment(window);
            LRESULT(0)
        }
        WM_PAD_MEMO_FINISHED if !state_ptr.is_null() => {
            // SAFETY: state_ptr belongs to this live Pad HWND and this
            // completion is processed on its owning UI thread.
            let state = unsafe { &mut *state_ptr };
            state.finish_memo_task(window);
            LRESULT(0)
        }
        WM_TIMER if !state_ptr.is_null() && w.0 == PAD_EDIT_TIMER => {
            // SAFETY: the timer belongs to this window.
            unsafe {
                let _ = KillTimer(Some(window), PAD_EDIT_TIMER);
            }
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            if state.locked {
                return LRESULT(0);
            }
            if state.capture_controls() {
                if state.memo_open.is_some() {
                    state.start_memo_reseal(window);
                } else {
                    state.sync_rows();
                    state.publish(window);
                }
            } else if state.memo_open.as_ref().is_some_and(|open| open.dirty) {
                state.start_memo_reseal(window);
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
            if state.locked {
                return LRESULT(0);
            }
            // A state that arrived while the notice stood is the newer fact
            // and keeps the row; only the notice itself expires.
            if state.status_notice {
                state.set_status(String::new());
                state.update_status();
            }
            LRESULT(0)
        }
        WM_TIMER if !state_ptr.is_null() && w.0 == PAD_UNLOCK_RETRY_TIMER => {
            // SAFETY: state_ptr is the non-null Pad state stored on this live
            // HWND; its message procedure runs on the owning UI thread.
            let state = unsafe { &mut *state_ptr };
            if state.locked {
                state.update_unlock_retry();
            } else {
                // SAFETY: window is the live Pad HWND and owns this timer.
                unsafe {
                    let _ = KillTimer(Some(window), PAD_UNLOCK_RETRY_TIMER);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if !state_ptr.is_null() && w.0 == PAD_PROTECTED_CLIPBOARD_TIMER => {
            // SAFETY: state_ptr is owned by this live Pad HWND and all its
            // timer callbacks execute on the Pad UI thread.
            let state = unsafe { &mut *state_ptr };
            state.expire_protected_clipboard();
            LRESULT(0)
        }
        WM_TIMER if !state_ptr.is_null() && w.0 == PAD_COMPLETION_TIMER => {
            // SAFETY: as above.
            let state = unsafe { &mut *state_ptr };
            if state.locked {
                // SAFETY: window is the live Pad HWND and owns this timer.
                unsafe {
                    let _ = KillTimer(Some(window), PAD_COMPLETION_TIMER);
                }
                return LRESULT(0);
            }
            if state.poll_storage() {
                // SAFETY: the timer belongs to this window.
                unsafe {
                    let _ = KillTimer(Some(window), PAD_COMPLETION_TIMER);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let mut may_hide = true;
            if !state_ptr.is_null() {
                // SAFETY: the state and controls belong to the live Pad.
                let state = unsafe { &mut *state_ptr };
                if matches!(
                    state.enroll_phase,
                    EnrollPhase::Running
                        | EnrollPhase::MemoProtectRunning
                        | EnrollPhase::MemoUnlockRunning
                ) {
                    set_control_text(state.enroll_controls[7], "切替結果を確認中です");
                    may_hide = false;
                } else if state.enroll_phase == EnrollPhase::MemoRecoveryPrompt {
                    state.cancel_enroll_prompt(window);
                    may_hide = false;
                } else if matches!(
                    state.enroll_phase,
                    EnrollPhase::Prompt
                        | EnrollPhase::MemoProtectPrompt
                        | EnrollPhase::MemoUnlockPrompt
                ) {
                    state.cancel_enroll_prompt(window);
                } else if state.locked {
                    state.cancel_unlock();
                } else {
                    may_hide = state.lock_protected(window, false);
                }
            }
            if may_hide {
                // SAFETY: the window is live.
                unsafe {
                    let _ = ShowWindow(window, SW_HIDE);
                }
            }
            LRESULT(isize::from(may_hide))
        }
        WM_PAD_MASK_FOR_SESSION if !state_ptr.is_null() => {
            // SAFETY: the Pad owns this state on the UI thread.
            let state = unsafe { &mut *state_ptr };
            state.mask_memo_for_session(window);
            // SAFETY: session lock must mask the Pad even if the last save
            // outcome is uncertain; the in-process draft remains retained.
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
            }
            LRESULT(1)
        }
        WM_WTSSESSION_CHANGE if !state_ptr.is_null() && w.0 == WTS_SESSION_LOCK as usize => {
            // WTS sends this only to registered HWNDs in this session. Keep
            // the draft in memory if persistence is uncertain, but revoke
            // the active key session and remove all memo child HWNDs.
            // SAFETY: state_ptr is the non-null state owned by this Pad HWND;
            // WTS delivery runs on the same UI thread as other Pad messages.
            let state = unsafe { &mut *state_ptr };
            state.mask_memo_for_session(window);
            // SAFETY: window is the registered live Pad HWND; hiding it
            // follows removal of memo child controls above.
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
#[path = "pad_tests.rs"]
mod tests;

#[cfg(test)]
mod unlock_retry_tests {
    use super::unlock_retry_delay;

    #[test]
    fn wrong_password_delay_grows_and_caps_at_thirty_seconds() {
        let seconds: Vec<u64> = (1..=8).map(|n| unlock_retry_delay(n).as_secs()).collect();
        assert_eq!(seconds, [1, 2, 4, 8, 16, 30, 30, 30]);
    }
}
