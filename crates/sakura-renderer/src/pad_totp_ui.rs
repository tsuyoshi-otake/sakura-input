//! Native modal dialogs for the optional offline Sakura Pad TOTP gate.
//!
//! This module only collects a code. The caller owns enrollment, password
//! reauthentication, verification, durable state, and the final unlock.

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Once;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetMonitorInfoW,
    GetSysColorBrush, MonitorFromWindow, COLOR_WINDOW, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT,
};
use windows::Win32::UI::Controls::{EM_SETLIMITTEXT, EM_SETREADONLY};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextW, IsDialogMessageW, IsWindow, LoadCursorW,
    PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowTextW, ShowWindow, TranslateMessage, BS_DEFPUSHBUTTON, ES_AUTOHSCROLL, ES_PASSWORD,
    GWLP_USERDATA, HMENU, IDC_ARROW, MSG, SW_SHOW, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_NCDESTROY, WM_PAINT, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};
use zeroize::Zeroizing;

const CLASS: PCWSTR = windows::core::w!("SakuraPadTotpPrompt");
const CODE_ID: u16 = 301;
const ACCEPT_ID: u16 = 302;
const CANCEL_ID: u16 = 303;
const SECRET_ID: u16 = 304;
const URI_ID: u16 = 305;
const MAX_OTPAUTH_URI_BYTES: usize = 256;
const QR_QUIET_ZONE: i32 = 4;

struct QrImage {
    size: i32,
    modules: Zeroizing<Vec<u8>>,
    rect: RECT,
}

impl QrImage {
    fn pixel_geometry(&self) -> (i32, i32, i32) {
        let side = self.size + QR_QUIET_ZONE * 2;
        let cell = ((self.rect.right - self.rect.left) / side)
            .min((self.rect.bottom - self.rect.top) / side)
            .max(1);
        let left = self.rect.left + (self.rect.right - self.rect.left - cell * side) / 2;
        let top = self.rect.top + (self.rect.bottom - self.rect.top - cell * side) / 2;
        (cell, left, top)
    }
}

fn qr_image(uri: &str, rect: RECT) -> Option<QrImage> {
    if uri.len() > MAX_OTPAUTH_URI_BYTES {
        return None;
    }
    let modules = crate::pad_totp_qr::encode(uri)?;
    let size = 49;
    Some(QrImage {
        size,
        modules,
        rect,
    })
}

fn paint_qr(dialog: HWND, image: &QrImage) {
    let mut paint = PAINTSTRUCT::default();
    // SAFETY: USER32 supplied this live HWND in WM_PAINT, and paint remains
    // initialized until EndPaint below.
    let dc = unsafe { BeginPaint(dialog, &mut paint) };
    if dc.is_invalid() {
        // SAFETY: balance BeginPaint so USER32 validates the update region.
        unsafe {
            let _ = EndPaint(dialog, &paint);
        }
        return;
    }
    draw_qr(dc, image);
    // SAFETY: this balances the successful BeginPaint above.
    unsafe {
        let _ = EndPaint(dialog, &paint);
    }
}

fn draw_qr(dc: HDC, image: &QrImage) {
    // A fixed black-on-white symbol keeps the ISO quiet zone and contrast
    // even when the surrounding native dialog uses a high-contrast palette.
    // SAFETY: brushes and DC remain valid through all synchronous fills.
    unsafe {
        let white = CreateSolidBrush(COLORREF(0x00ff_ffff));
        let black = CreateSolidBrush(COLORREF(0));
        if !white.is_invalid() && !black.is_invalid() {
            let _ = FillRect(dc, &image.rect, white);
            let (cell, quiet_left, quiet_top) = image.pixel_geometry();
            let left = quiet_left + cell * QR_QUIET_ZONE;
            let top = quiet_top + cell * QR_QUIET_ZONE;
            for y in 0..image.size {
                for x in 0..image.size {
                    if image.modules[(y * image.size + x) as usize] != 0 {
                        let block = RECT {
                            left: left + x * cell,
                            top: top + y * cell,
                            right: left + (x + 1) * cell,
                            bottom: top + (y + 1) * cell,
                        };
                        let _ = FillRect(dc, &block, black);
                    }
                }
            }
        }
        if !black.is_invalid() {
            let _ = DeleteObject(black.into());
        }
        if !white.is_invalid() {
            let _ = DeleteObject(white.into());
        }
    }
}

/// Show a selectable manual key and authenticator URI, then collect the first
/// six-digit code. The caller must verify it and only then enable the gate.
/// The supplied secret is the one-time `begin_enrollment` value.
pub fn prompt_setup(parent: HWND, manual_secret: &str) -> Option<Zeroizing<String>> {
    if !valid_manual_secret(manual_secret) {
        return None;
    }
    let uri = otpauth_uri(manual_secret);
    show(
        parent,
        DialogKind::Setup,
        Some(manual_secret),
        Some(&uri),
        "Sakura Pad",
    )
    .0
}

/// Give each protected memo a distinct authenticator account label. The ID
/// is stable and discloses no locked title or body content.
pub fn prompt_memo_setup(
    parent: HWND,
    manual_secret: &str,
    memo_id: u64,
) -> Option<Zeroizing<String>> {
    if memo_id == 0 || !valid_manual_secret(manual_secret) {
        return None;
    }
    let uri = memo_otpauth_uri(manual_secret, memo_id);
    let label = format!("メモ {memo_id:016x}");
    show(
        parent,
        DialogKind::Setup,
        Some(manual_secret),
        Some(&uri),
        &label,
    )
    .0
}

fn memo_otpauth_uri(secret: &str, memo_id: u64) -> Zeroizing<String> {
    Zeroizing::new(format!(
        "otpauth://totp/Sakura%20Pad%20Memo%20{memo_id:016x}?secret={secret}&issuer=Sakura%20Input&algorithm=SHA1&digits=6&period=30"
    ))
}

fn otpauth_uri(secret: &str) -> Zeroizing<String> {
    Zeroizing::new(format!(
        "otpauth://totp/Sakura%20Pad?secret={secret}&issuer=Sakura%20Input&algorithm=SHA1&digits=6&period=30"
    ))
}

/// Collect a six-digit code for a Pad or memo unlock. Cancel returns `None`.
pub fn prompt_code(parent: HWND, scope_label: &str) -> Option<Zeroizing<String>> {
    show(parent, DialogKind::Code, None, None, scope_label).0
}

/// Collect a masked password for security-setting reauthentication. The
/// caller owns authentication and must drop the returned value after use.
pub fn prompt_password(parent: HWND, purpose: &str) -> Option<Zeroizing<String>> {
    show(parent, DialogKind::Password, None, None, purpose).0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TotpAction {
    Enable,
    Disable,
    Cancel,
}

/// Show the current local TOTP state and the available action. Recovery-key
/// access remains available with either setting.
pub fn choose_totp_action(parent: HWND, enabled: bool) -> TotpAction {
    let kind = if enabled {
        DialogKind::ActionEnabled
    } else {
        DialogKind::ActionDisabled
    };
    show(parent, kind, None, None, "").1
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DialogKind {
    Setup,
    Code,
    Password,
    ActionEnabled,
    ActionDisabled,
}

impl DialogKind {
    fn is_action(self) -> bool {
        matches!(self, Self::ActionEnabled | Self::ActionDisabled)
    }
}

fn fitted_dpi(requested: u32, work: RECT, setup: bool) -> i32 {
    let base_width = if setup { 820 } else { 440 };
    let base_height = if setup { 450 } else { 220 };
    let available_width = (work.right - work.left).max(1);
    let available_height = (work.bottom - work.top).max(1);
    (requested as i32)
        .min(available_width.saturating_mul(96) / base_width)
        .min(available_height.saturating_mul(96) / base_height)
        .max(1)
}

struct DialogState {
    kind: DialogKind,
    label: Zeroizing<Vec<u16>>,
    secret: Option<Zeroizing<Vec<u16>>>,
    uri: Option<Zeroizing<Vec<u16>>>,
    code: HWND,
    secret_edit: HWND,
    uri_edit: HWND,
    status: HWND,
    result: Option<Zeroizing<String>>,
    action: TotpAction,
    accept: HWND,
    qr: Option<QrImage>,
}

fn show(
    parent: HWND,
    kind: DialogKind,
    secret: Option<&str>,
    uri: Option<&str>,
    scope_label: &str,
) -> (Option<Zeroizing<String>>, TotpAction) {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: the static class name and procedure live for the process.
        unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                lpszClassName: CLASS,
                ..Default::default()
            };
            let _ = RegisterClassW(&class);
        }
    });

    let setup = kind == DialogKind::Setup;
    // SAFETY: parent is the caller-supplied Pad HWND. The nearest monitor's
    // work area keeps the QR and buttons on screen at high DPI.
    let (requested_dpi, work) = unsafe {
        let monitor = MonitorFromWindow(parent, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let work = if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        };
        (GetDpiForWindow(parent).max(96), work)
    };
    let dpi = fitted_dpi(requested_dpi, work, setup);
    let scale = |value: i32| value.saturating_mul(dpi) / 96;
    let width = scale(if setup { 820 } else { 440 });
    let height = scale(if setup { 450 } else { 220 });
    let mut rect = RECT::default();
    // SAFETY: parent is supplied by the caller; invalid HWND leaves a
    // zero rectangle and the dialog still remains visible at the origin.
    let _ = unsafe { GetWindowRect(parent, &mut rect) };
    let center_x = rect.left + (rect.right - rect.left - width) / 2;
    let center_y = rect.top + (rect.bottom - rect.top - height) / 2;
    let x = center_x
        .max(work.left)
        .min((work.right - width).max(work.left));
    let y = center_y
        .max(work.top)
        .min((work.bottom - height).max(work.top));
    let qr = if setup {
        let qr_rect = RECT {
            left: scale(545),
            top: scale(45),
            right: scale(785),
            bottom: scale(285),
        };
        match uri.and_then(|value| qr_image(value, qr_rect)) {
            Some(image) => Some(image),
            None => return (None, TotpAction::Cancel),
        }
    } else {
        None
    };
    let clipped_label = Zeroizing::new(scope_label.chars().take(80).collect::<String>());
    let mut state = DialogState {
        kind,
        label: wide(&clipped_label),
        secret: secret.map(wide),
        uri: uri.map(wide),
        code: HWND::default(),
        secret_edit: HWND::default(),
        uri_edit: HWND::default(),
        status: HWND::default(),
        result: None,
        action: TotpAction::Cancel,
        accept: HWND::default(),
        qr,
    };
    let title = match kind {
        DialogKind::Setup => windows::core::w!("Sakura Pad · TOTP 設定"),
        DialogKind::Code => windows::core::w!("Sakura Pad · 確認コード"),
        DialogKind::Password => windows::core::w!("Sakura Pad · 再認証"),
        DialogKind::ActionEnabled | DialogKind::ActionDisabled => {
            windows::core::w!("Sakura Pad · TOTP")
        }
    };
    // SAFETY: class registration ran above; owner remains live for this
    // synchronous modal call. USER32 owns the returned window.
    let dialog = unsafe {
        CreateWindowExW(
            Default::default(),
            CLASS,
            title,
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            width,
            height,
            Some(parent),
            None,
            None,
            None,
        )
    }
    .ok();
    let Some(dialog) = dialog else {
        return (None, TotpAction::Cancel);
    };
    // SAFETY: state is stack-owned throughout the nested modal loop. The
    // pointer is cleared by WM_NCDESTROY before state can be dropped.
    unsafe {
        SetWindowLongPtrW(
            dialog,
            GWLP_USERDATA,
            (&mut state as *mut DialogState) as isize,
        );
    }
    if create_controls(dialog, &mut state, scale).is_err() {
        // SAFETY: this function owns the just-created dialog.
        unsafe {
            let _ = DestroyWindow(dialog);
        }
        return (None, TotpAction::Cancel);
    }

    // SAFETY: disabling an enabled owner provides modal input behavior;
    // restoration is unconditional after the dialog has been destroyed.
    let owner_was_enabled = unsafe { IsWindowEnabled(parent).as_bool() };
    if owner_was_enabled {
        // SAFETY: this is the caller-supplied live owner, restored below.
        unsafe {
            let _ = EnableWindow(parent, false);
        }
    }
    // SAFETY: dialog and its chosen focus child are live and owned by this call.
    unsafe {
        let _ = ShowWindow(dialog, SW_SHOW);
        let _ = SetForegroundWindow(dialog);
        let _ = SetFocus(Some(if kind.is_action() {
            state.accept
        } else {
            state.code
        }));
    }
    let mut message = MSG::default();
    loop {
        // SAFETY: the dialog is owned by this call and message is initialized.
        if !unsafe { IsWindow(Some(dialog)).as_bool() } {
            break;
        }
        // SAFETY: message is initialized and remains live for the call.
        let got = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if got <= 0 {
            if got == 0 {
                // SAFETY: preserve the host's shutdown signal after exiting
                // this nested modal loop.
                unsafe {
                    PostQuitMessage(message.wParam.0 as i32);
                }
            }
            // SAFETY: close our dialog even when host shutdown interrupts it.
            unsafe {
                let _ = DestroyWindow(dialog);
            }
            break;
        }
        // SAFETY: IsDialogMessage handles Tab/default button for native child
        // controls; other messages keep the renderer UI loop responsive.
        unsafe {
            if !IsDialogMessageW(dialog, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    if owner_was_enabled {
        // SAFETY: restore only the owner we disabled above.
        unsafe {
            let _ = EnableWindow(parent, true);
            let _ = SetForegroundWindow(parent);
        }
    }
    (state.result.take(), state.action)
}

fn create_controls(
    dialog: HWND,
    state: &mut DialogState,
    scale: impl Fn(i32) -> i32,
) -> windows::core::Result<()> {
    if state.kind.is_action() {
        let enabled = state.kind == DialogKind::ActionEnabled;
        child(
            dialog,
            windows::core::w!("STATIC"),
            if enabled {
                windows::core::w!("TOTP: 有効")
            } else {
                windows::core::w!("TOTP: 未設定")
            },
            24,
            22,
            392,
            28,
            0,
            0,
            &scale,
        )?;
        child(
            dialog,
            windows::core::w!("STATIC"),
            windows::core::w!("同じ PC での確認です。オンライン 2FA ではありません。\n暗号化の鍵にはなりません。復旧キーでも解除できます。"),
            24,
            62,
            392,
            48,
            0,
            0,
            &scale,
        )?;
        state.accept = child(
            dialog,
            windows::core::w!("BUTTON"),
            if enabled {
                windows::core::w!("無効にする")
            } else {
                windows::core::w!("有効にする")
            },
            225,
            140,
            92,
            31,
            ACCEPT_ID,
            WS_TABSTOP.0 as i32 | BS_DEFPUSHBUTTON,
            &scale,
        )?;
        child(
            dialog,
            windows::core::w!("BUTTON"),
            windows::core::w!("キャンセル"),
            327,
            140,
            92,
            31,
            CANCEL_ID,
            WS_TABSTOP.0 as i32,
            &scale,
        )?;
        return Ok(());
    }
    let setup = state.kind == DialogKind::Setup;
    let password = state.kind == DialogKind::Password;
    if setup {
        child(
            dialog,
            windows::core::w!("STATIC"),
            windows::core::w!("認証アプリに手動キーまたは URI を登録してください。"),
            24,
            18,
            500,
            22,
            0,
            0,
            &scale,
        )?;
        child(
            dialog,
            windows::core::w!("STATIC"),
            windows::core::w!("手動キー (選択してコピーできます)"),
            24,
            52,
            500,
            20,
            0,
            0,
            &scale,
        )?;
        state.secret_edit = child(
            dialog,
            windows::core::w!("EDIT"),
            PCWSTR(state.secret.as_ref().unwrap().as_ptr()),
            24,
            75,
            500,
            27,
            SECRET_ID,
            ES_AUTOHSCROLL | WS_BORDER.0 as i32 | WS_TABSTOP.0 as i32,
            &scale,
        )?;
        child(
            dialog,
            windows::core::w!("STATIC"),
            windows::core::w!("otpauth URI (選択してコピーできます)"),
            24,
            112,
            500,
            20,
            0,
            0,
            &scale,
        )?;
        state.uri_edit = child(
            dialog,
            windows::core::w!("EDIT"),
            PCWSTR(state.uri.as_ref().unwrap().as_ptr()),
            24,
            135,
            500,
            27,
            URI_ID,
            ES_AUTOHSCROLL | WS_BORDER.0 as i32 | WS_TABSTOP.0 as i32,
            &scale,
        )?;
        child(
            dialog,
            windows::core::w!("STATIC"),
            windows::core::w!("ローカル確認です（暗号鍵・オンライン 2FA ではありません）。"),
            24,
            178,
            500,
            22,
            0,
            0,
            &scale,
        )?;
        child(
            dialog,
            windows::core::w!("STATIC"),
            windows::core::w!("認証アプリで QR コードを読み取る"),
            545,
            20,
            240,
            22,
            0,
            0,
            &scale,
        )?;
        // SAFETY: both native edits are live children; EM_SETREADONLY changes
        // interaction without copying or modifying their text.
        unsafe {
            let _ = SendMessageW(state.secret_edit, EM_SETREADONLY, Some(WPARAM(1)), None);
            let _ = SendMessageW(state.uri_edit, EM_SETREADONLY, Some(WPARAM(1)), None);
        }
    } else {
        child(
            dialog,
            windows::core::w!("STATIC"),
            PCWSTR(state.label.as_ptr()),
            24,
            20,
            392,
            24,
            0,
            0,
            &scale,
        )?;
        child(
            dialog,
            windows::core::w!("STATIC"),
            if password {
                windows::core::w!("パスワード")
            } else {
                windows::core::w!("認証アプリの 6 桁コード")
            },
            24,
            57,
            392,
            22,
            0,
            0,
            &scale,
        )?;
    }
    let code_y = if setup { 235 } else { 85 };
    state.code = child(
        dialog,
        windows::core::w!("EDIT"),
        windows::core::w!(""),
        24,
        code_y,
        if password { 280 } else { 170 },
        29,
        CODE_ID,
        ES_AUTOHSCROLL
            | WS_BORDER.0 as i32
            | WS_TABSTOP.0 as i32
            | if password { ES_PASSWORD } else { 0 },
        &scale,
    )?;
    // SAFETY: the edit child is live and the limit matches the mode's contract.
    unsafe {
        let _ = SendMessageW(
            state.code,
            EM_SETLIMITTEXT,
            Some(WPARAM(if password { 256 } else { 6 })),
            None,
        );
    }
    state.status = child(
        dialog,
        windows::core::w!("STATIC"),
        windows::core::w!(""),
        if password { 24 } else { 205 },
        if password { code_y + 35 } else { code_y + 4 },
        if password {
            392
        } else if setup {
            310
        } else {
            210
        },
        if password { 19 } else { 22 },
        0,
        0,
        &scale,
    )?;
    let button_y = if setup {
        325
    } else if password {
        155
    } else {
        140
    };
    state.accept = child(
        dialog,
        windows::core::w!("BUTTON"),
        windows::core::w!("確認"),
        if setup { 570 } else { 225 },
        button_y,
        92,
        31,
        ACCEPT_ID,
        WS_TABSTOP.0 as i32 | BS_DEFPUSHBUTTON,
        &scale,
    )?;
    child(
        dialog,
        windows::core::w!("BUTTON"),
        windows::core::w!("キャンセル"),
        if setup { 675 } else { 327 },
        button_y,
        92,
        31,
        CANCEL_ID,
        WS_TABSTOP.0 as i32,
        &scale,
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn child(
    dialog: HWND,
    class: PCWSTR,
    text: PCWSTR,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: u16,
    extra_style: i32,
    scale: &impl Fn(i32) -> i32,
) -> windows::core::Result<HWND> {
    // SAFETY: the caller owns dialog and all text buffers remain live during
    // CreateWindowExW, which copies their text into native child controls.
    unsafe {
        CreateWindowExW(
            Default::default(),
            class,
            text,
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(extra_style as u32),
            scale(x),
            scale(y),
            scale(width),
            scale(height),
            Some(dialog),
            if id == 0 {
                None
            } else {
                Some(HMENU(id as usize as *mut c_void))
            },
            None,
            None,
        )
    }
}

extern "system" fn procedure(dialog: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: the pointer is set before the dialog is shown and cleared in
    // WM_NCDESTROY. Calls during creation safely use DefWindowProcW.
    let state = unsafe { GetWindowLongPtrW(dialog, GWLP_USERDATA) as *mut DialogState };
    match message {
        WM_COMMAND if !state.is_null() => {
            match (w.0 & 0xffff) as u16 {
                ACCEPT_ID | 1 => {
                    // SAFETY: state is owned by the active modal call, and
                    // this branch does not hold a Rust borrow across HWND work.
                    let kind = unsafe { (*state).kind };
                    if kind.is_action() {
                        // SAFETY: one active dialog owns this state pointer.
                        unsafe {
                            (*state).action = if kind == DialogKind::ActionEnabled {
                                TotpAction::Disable
                            } else {
                                TotpAction::Enable
                            };
                        }
                        // SAFETY: close the dialog after capturing the action.
                        unsafe {
                            let _ = DestroyWindow(dialog);
                        }
                        return LRESULT(0);
                    }
                    let max = if kind == DialogKind::Password { 256 } else { 6 };
                    let mut units = Zeroizing::new(vec![0_u16; max + 1]);
                    // SAFETY: the bounded buffer matches EM_SETLIMITTEXT plus
                    // NUL, and the edit is a live child of this dialog.
                    let len = unsafe { GetWindowTextW((*state).code, &mut units) } as usize;
                    let value = if kind == DialogKind::Password {
                        decode_password(&units[..len.min(max)])
                    } else {
                        decode_code(&units[..len.min(max)])
                    };
                    if let Some(code) = value {
                        // SAFETY: this modal call owns the live state pointer.
                        unsafe {
                            (*state).result = Some(code);
                        }
                        // SAFETY: this procedure owns the dialog HWND; its
                        // destroy messages clear all native text first.
                        unsafe {
                            let _ = DestroyWindow(dialog);
                        }
                    } else {
                        // SAFETY: the status child is live until destruction.
                        unsafe {
                            let _ = SetWindowTextW(
                                (*state).status,
                                if kind == DialogKind::Password {
                                    windows::core::w!("パスワードを入力してください")
                                } else {
                                    windows::core::w!("6 桁の数字を入力してください")
                                },
                            );
                        }
                    }
                    return LRESULT(0);
                }
                CANCEL_ID | 2 => {
                    // SAFETY: cancel owns this dialog and its child controls.
                    unsafe {
                        let _ = DestroyWindow(dialog);
                    }
                    return LRESULT(0);
                }
                _ => {}
            }
        }
        WM_CLOSE => {
            // SAFETY: WM_CLOSE targets this live dialog HWND.
            unsafe {
                let _ = DestroyWindow(dialog);
            }
            return LRESULT(0);
        }
        WM_PAINT if !state.is_null() => {
            // SAFETY: the dialog's state pointer remains live for this modal
            // call. Painting reads the immutable QR modules only.
            if let Some(image) = unsafe { &(*state).qr } {
                paint_qr(dialog, image);
                return LRESULT(0);
            }
        }
        WM_DESTROY if !state.is_null() => {
            // SAFETY: capture HWND values before any SetWindowTextW call can
            // re-enter this procedure; no Rust reference lives across it.
            let (code, secret_edit, uri_edit) =
                unsafe { ((*state).code, (*state).secret_edit, (*state).uri_edit) };
            // SAFETY: HWND values are valid children until DefWindowProc
            // completes destruction. Clear native copies before then; the
            // returned code remains in its Zeroizing<String> owner.
            unsafe {
                if !code.is_invalid() {
                    let _ = SetWindowTextW(code, windows::core::w!(""));
                }
                if !secret_edit.is_invalid() {
                    let _ = SetWindowTextW(secret_edit, windows::core::w!(""));
                }
                if !uri_edit.is_invalid() {
                    let _ = SetWindowTextW(uri_edit, windows::core::w!(""));
                }
            }
            return LRESULT(0);
        }
        // SAFETY: this is the final native message for the owned HWND; the
        // pointer must be cleared before stack state can be dropped.
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(dialog, GWLP_USERDATA, 0);
        },
        _ => {}
    }
    // SAFETY: USER32 supplied this valid HWND/message pair to the procedure.
    unsafe { DefWindowProcW(dialog, message, w, l) }
}

fn wide(text: &str) -> Zeroizing<Vec<u16>> {
    Zeroizing::new(text.encode_utf16().chain(std::iter::once(0)).collect())
}

fn valid_manual_secret(secret: &str) -> bool {
    secret.len() == 32
        && secret
            .bytes()
            .all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b))
}

fn decode_code(units: &[u16]) -> Option<Zeroizing<String>> {
    if units.len() != 6 || !units.iter().all(|unit| (0x30..=0x39).contains(unit)) {
        return None;
    }
    String::from_utf16(units).ok().map(Zeroizing::new)
}

fn decode_password(units: &[u16]) -> Option<Zeroizing<String>> {
    if units.is_empty() || units.len() > 256 {
        return None;
    }
    String::from_utf16(units).ok().map(Zeroizing::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_accepts_only_our_base32_key_shape() {
        assert!(valid_manual_secret("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"));
        assert!(!valid_manual_secret("abcdefghijklmnopqrstuvwxyz234567"));
        assert!(!valid_manual_secret("ABCDEFGHIJKLMNOPQRSTUVWXYZ23456"));
        assert!(!valid_manual_secret("ABCDEFGHIJKLMNOPQRSTUVWXYZ234568"));
    }

    #[test]
    fn code_entry_requires_exactly_six_ascii_digits() {
        assert_eq!(decode_code(&[b'1' as u16; 6]).unwrap().as_str(), "111111");
        assert!(decode_code(&[b'1' as u16; 5]).is_none());
        assert!(decode_code(&[0xff11; 6]).is_none());
        assert!(decode_code(&[
            b'1' as u16,
            b'2' as u16,
            b'3' as u16,
            b'4' as u16,
            b'5' as u16,
            b'A' as u16
        ])
        .is_none());
    }

    #[test]
    fn password_entry_is_bounded_and_rejects_invalid_utf16() {
        assert!(decode_password(&[]).is_none());
        assert_eq!(
            decode_password(&[0x3042; 256])
                .unwrap()
                .encode_utf16()
                .count(),
            256
        );
        assert!(decode_password(&[b'a' as u16; 257]).is_none());
        assert!(decode_password(&[0xd800]).is_none());
    }

    #[test]
    fn otpauth_qr_is_bounded_and_has_integer_modules_with_quiet_zone() {
        let uri = otpauth_uri("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567");
        assert!(uri.len() <= MAX_OTPAUTH_URI_BYTES);
        assert!(uri.starts_with("otpauth://totp/Sakura%20Pad?secret="));
        let rect = RECT {
            left: 545,
            top: 45,
            right: 785,
            bottom: 285,
        };
        let image = qr_image(&uri, rect).unwrap();
        assert!(image.size >= 21 && image.size <= 65);
        assert_eq!(image.modules.len(), (image.size * image.size) as usize);
        assert_eq!(image.modules[0], 1); // top-left finder pattern
        let (cell, left, top) = image.pixel_geometry();
        assert!(cell >= 3);
        assert!(left + QR_QUIET_ZONE * cell > rect.left);
        assert!(top + QR_QUIET_ZONE * cell > rect.top);
        let doubled = qr_image(
            &uri,
            RECT {
                left: 0,
                top: 0,
                right: 480,
                bottom: 480,
            },
        )
        .unwrap();
        assert_eq!(doubled.pixel_geometry().0, cell * 2);
        assert!(qr_image(&"x".repeat(MAX_OTPAUTH_URI_BYTES + 1), rect).is_none());
    }

    #[test]
    fn memo_otpauth_labels_are_distinct_without_locked_content() {
        let first = memo_otpauth_uri("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567", 1);
        let second = memo_otpauth_uri("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567", 2);
        assert_ne!(first, second);
        assert!(first.contains("Sakura%20Pad%20Memo%200000000000000001"));
        assert!(first.len() <= MAX_OTPAUTH_URI_BYTES);
    }

    #[test]
    fn setup_dpi_fits_a_small_monitor_work_area() {
        let work = RECT {
            left: -1366,
            top: 0,
            right: 0,
            bottom: 768,
        };
        let dpi = fitted_dpi(192, work, true);
        assert_eq!(dpi, 159);
        assert!(820 * dpi / 96 <= work.right - work.left);
        assert!(450 * dpi / 96 <= work.bottom - work.top);
        assert_eq!(fitted_dpi(96, work, true), 96);
    }

    #[test]
    fn native_gdi_qr_paints_black_modules_and_white_quiet_zone() {
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, GetDC, GetPixel, ReleaseDC,
            SelectObject,
        };

        let uri = otpauth_uri("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567");
        let image = qr_image(
            &uri,
            RECT {
                left: 0,
                top: 0,
                right: 240,
                bottom: 240,
            },
        )
        .unwrap();
        let (cell, left, top) = image.pixel_geometry();
        // SAFETY: each GDI object is checked before use and restored/freed
        // before assertions, including if a sampled color is unexpected.
        let (quiet, finder) = unsafe {
            let screen = GetDC(None);
            assert!(!screen.is_invalid());
            let memory = CreateCompatibleDC(Some(screen));
            assert!(!memory.is_invalid());
            let bitmap = CreateCompatibleBitmap(screen, 240, 240);
            assert!(!bitmap.is_invalid());
            let previous = SelectObject(memory, bitmap.into());
            draw_qr(memory, &image);
            let quiet = GetPixel(memory, left + cell / 2, top + cell / 2);
            let finder = GetPixel(
                memory,
                left + (QR_QUIET_ZONE * cell) + cell / 2,
                top + (QR_QUIET_ZONE * cell) + cell / 2,
            );
            let _ = SelectObject(memory, previous);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(memory);
            let _ = ReleaseDC(None, screen);
            (quiet, finder)
        };
        assert_eq!(quiet, COLORREF(0x00ff_ffff));
        assert_eq!(finder, COLORREF(0));
    }
}
