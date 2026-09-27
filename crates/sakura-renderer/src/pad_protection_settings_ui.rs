//! Native security-setting dialogs for the whole Pad.
//!
//! This module collects credentials only. The caller authenticates the old
//! password and owns the storage transaction and its final result.

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Once;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, GetSysColorBrush, MonitorFromWindow, COLOR_WINDOW, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Controls::EM_SETLIMITTEXT;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, IsWindowEnabled, SetFocus,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsChild,
    IsDialogMessageW, IsWindow, LoadCursorW, PostQuitMessage, RegisterClassW, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage,
    BS_DEFPUSHBUTTON, ES_AUTOHSCROLL, ES_PASSWORD, GWLP_USERDATA, HMENU, IDC_ARROW, MSG, SW_SHOW,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_NCDESTROY, WNDCLASSW, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use zeroize::Zeroizing;

const CLASS: PCWSTR = windows::core::w!("SakuraPadPasswordChangePrompt");
const CHOOSER_CLASS: PCWSTR = windows::core::w!("SakuraPadProtectionSettingsPrompt");
const OLD_ID: u16 = 401;
const NEW_ID: u16 = 402;
const CONFIRM_ID: u16 = 403;
const ACCEPT_ID: u16 = 404;
const CANCEL_ID: u16 = 405;
const MAX_PASSWORD_UTF16_UNITS: usize = 256;
const MAX_PASSWORD_BYTES: usize = 1024;
const WIDTH: i32 = 510;
const HEIGHT: i32 = 350;
const CHOOSER_WIDTH: i32 = 480;
const CHOOSER_HEIGHT: i32 = 195;
const CHOOSE_PASSWORD_ID: u16 = 411;
const CHOOSE_TOTP_ID: u16 = 412;
const CHOOSE_CANCEL_ID: u16 = 413;

/// A security-setting choice. The caller owns authentication, TOTP state,
/// password changes, and durable completion for the selected action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectionSettingsAction {
    ChangePassword,
    TotpSettings,
    Cancel,
}

/// Choose which whole-Pad security setting to open. Closing the window or
/// pressing Esc has the same result as the explicit Cancel button.
pub fn choose_protection_setting(
    parent: HWND,
    password_change_available: bool,
) -> ProtectionSettingsAction {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: the callback and class name are static for the process.
        unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(chooser_procedure),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                lpszClassName: CHOOSER_CLASS,
                ..Default::default()
            };
            let _ = RegisterClassW(&class);
        }
    });
    let (x, y, width, height, dpi) = window_geometry(parent, CHOOSER_WIDTH, CHOOSER_HEIGHT);
    let scale = |value: i32| value.saturating_mul(dpi) / 96;
    // SAFETY: the registered class and owner are live for this modal call.
    let Some(dialog) = (unsafe {
        CreateWindowExW(
            Default::default(),
            CHOOSER_CLASS,
            windows::core::w!("Sakura Pad · 保護設定"),
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
    })
    .ok() else {
        return ProtectionSettingsAction::Cancel;
    };
    let mut action = ProtectionSettingsAction::Cancel;
    // SAFETY: this stack value outlives the nested message loop. The pointer
    // is removed by WM_NCDESTROY before returning.
    unsafe {
        SetWindowLongPtrW(
            dialog,
            GWLP_USERDATA,
            (&mut action as *mut ProtectionSettingsAction) as isize,
        );
    }
    let controls = (|| -> windows::core::Result<HWND> {
        child(
            dialog,
            windows::core::w!("STATIC"),
            windows::core::w!("Sakura Pad 全体の保護設定を選んでください。"),
            24,
            20,
            430,
            26,
            0,
            0,
            &scale,
        )?;
        let first = child(
            dialog,
            windows::core::w!("BUTTON"),
            windows::core::w!("パスワードを変更"),
            24,
            77,
            155,
            35,
            CHOOSE_PASSWORD_ID,
            WS_TABSTOP.0 as i32 | BS_DEFPUSHBUTTON,
            &scale,
        )?;
        let totp = child(
            dialog,
            windows::core::w!("BUTTON"),
            windows::core::w!("確認コードを設定・解除"),
            190,
            77,
            190,
            35,
            CHOOSE_TOTP_ID,
            WS_TABSTOP.0 as i32,
            &scale,
        )?;
        if !password_change_available {
            // SAFETY: first is the live chooser button created above.
            unsafe {
                let _ = EnableWindow(first, false);
            }
            child(
                dialog,
                windows::core::w!("STATIC"),
                windows::core::w!("セキュリティキー保護の方法変更は今後対応します。"),
                24,
                125,
                430,
                26,
                0,
                0,
                &scale,
            )?;
        }
        child(
            dialog,
            windows::core::w!("BUTTON"),
            windows::core::w!("キャンセル"),
            384,
            77,
            75,
            35,
            CHOOSE_CANCEL_ID,
            WS_TABSTOP.0 as i32,
            &scale,
        )?;
        Ok(if password_change_available {
            first
        } else {
            totp
        })
    })();
    let Ok(first) = controls else {
        // SAFETY: close the partially created dialog before returning.
        unsafe {
            let _ = DestroyWindow(dialog);
        }
        return ProtectionSettingsAction::Cancel;
    };
    // SAFETY: restore only an owner that this call disabled.
    let previous_focus = unsafe { GetFocus() };
    // SAFETY: parent is the live Pad window supplied by the caller.
    let owner_was_enabled = unsafe { IsWindowEnabled(parent).as_bool() };
    if owner_was_enabled {
        // SAFETY: parent remains live throughout this synchronous modal call.
        unsafe {
            let _ = EnableWindow(parent, false);
        }
    }
    // SAFETY: focus the first of three native, keyboard-accessible buttons.
    unsafe {
        let _ = ShowWindow(dialog, SW_SHOW);
        let _ = SetForegroundWindow(dialog);
        let _ = SetFocus(Some(first));
    }
    modal_loop(dialog);
    if owner_was_enabled {
        // SAFETY: parent was disabled by this call and is still the owner.
        unsafe {
            let _ = EnableWindow(parent, true);
            let _ = SetForegroundWindow(parent);
            if IsWindow(Some(previous_focus)).as_bool() && IsChild(parent, previous_focus).as_bool()
            {
                let _ = SetFocus(Some(previous_focus));
            }
        }
    }
    action
}

extern "system" fn chooser_procedure(dialog: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: set before showing; removed on final native destruction.
    let action =
        unsafe { GetWindowLongPtrW(dialog, GWLP_USERDATA) as *mut ProtectionSettingsAction };
    match message {
        WM_COMMAND if !action.is_null() => {
            let selected = match (w.0 & 0xffff) as u16 {
                CHOOSE_PASSWORD_ID | 1 => Some(ProtectionSettingsAction::ChangePassword),
                CHOOSE_TOTP_ID => Some(ProtectionSettingsAction::TotpSettings),
                CHOOSE_CANCEL_ID | 2 => Some(ProtectionSettingsAction::Cancel),
                _ => None,
            };
            if let Some(selected) = selected {
                // SAFETY: the modal caller owns the stack value. DestroyWindow
                // reenters this callback only after this write is complete.
                unsafe {
                    *action = selected;
                    let _ = DestroyWindow(dialog);
                }
                return LRESULT(0);
            }
        }
        WM_CLOSE => {
            // SAFETY: the initialized action remains Cancel.
            unsafe {
                let _ = DestroyWindow(dialog);
            }
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: clear the pointer before the stack value can return.
            unsafe {
                SetWindowLongPtrW(dialog, GWLP_USERDATA, 0);
            }
        }
        _ => {}
    }
    // SAFETY: USER32 supplied this live HWND/message pair.
    unsafe { DefWindowProcW(dialog, message, w, l) }
}

/// The two credentials are cleared on drop. The confirmation text is checked
/// and discarded inside the dialog, never returned to the caller.
pub struct PasswordChange {
    pub old_password: Zeroizing<String>,
    pub new_password: Zeroizing<String>,
}

#[derive(Default)]
struct DialogState {
    old: HWND,
    new: HWND,
    confirm: HWND,
    status: HWND,
    result: Option<PasswordChange>,
}

/// Prompt for the current and replacement passwords. `None` means Cancel,
/// Esc, shutdown, or inability to create the native dialog. No change is made.
pub fn prompt_password_change(parent: HWND) -> Option<PasswordChange> {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: the static class name and callback live for the process.
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

    let (x, y, width, height, dpi) = window_geometry(parent, WIDTH, HEIGHT);
    let scale = |value: i32| value.saturating_mul(dpi) / 96;

    // SAFETY: the registered class and caller-owned parent remain live for
    // the synchronous modal call. USER32 owns the returned window.
    let dialog = unsafe {
        CreateWindowExW(
            Default::default(),
            CLASS,
            windows::core::w!("Sakura Pad · パスワード変更"),
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
    .ok()?;
    let mut state = DialogState::default();
    // SAFETY: the stack state remains live through the nested message loop;
    // WM_NCDESTROY removes the pointer before it can be dropped.
    unsafe {
        SetWindowLongPtrW(
            dialog,
            GWLP_USERDATA,
            (&mut state as *mut DialogState) as isize,
        );
    }
    if create_controls(dialog, &mut state, scale).is_err() {
        // SAFETY: this call owns the new dialog.
        unsafe {
            let _ = DestroyWindow(dialog);
        }
        return None;
    }

    // SAFETY: this call restores the owner's original enabled state below.
    let previous_focus = unsafe { GetFocus() };
    // SAFETY: parent is the live Pad window supplied by the caller.
    let owner_was_enabled = unsafe { IsWindowEnabled(parent).as_bool() };
    if owner_was_enabled {
        // SAFETY: parent remains live throughout this synchronous modal call.
        unsafe {
            let _ = EnableWindow(parent, false);
        }
    }
    // SAFETY: the edit and dialog are live until the modal loop closes.
    unsafe {
        let _ = ShowWindow(dialog, SW_SHOW);
        let _ = SetForegroundWindow(dialog);
        let _ = SetFocus(Some(state.old));
    }
    modal_loop(dialog);
    if owner_was_enabled {
        // SAFETY: restore only the owner this call disabled.
        unsafe {
            let _ = EnableWindow(parent, true);
            let _ = SetForegroundWindow(parent);
            if IsWindow(Some(previous_focus)).as_bool() && IsChild(parent, previous_focus).as_bool()
            {
                let _ = SetFocus(Some(previous_focus));
            }
        }
    }
    state.result.take()
}

fn fitted_dpi(requested: u32, work: RECT, base_width: i32, base_height: i32) -> i32 {
    (requested as i32)
        .min((work.right - work.left).max(1).saturating_mul(96) / base_width)
        .min((work.bottom - work.top).max(1).saturating_mul(96) / base_height)
        .max(1)
}

fn window_geometry(parent: HWND, base_width: i32, base_height: i32) -> (i32, i32, i32, i32, i32) {
    // SAFETY: the owner identifies the nearest work area. Native controls and
    // system colors retain the user's DPI and high-contrast configuration.
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
    let dpi = fitted_dpi(requested_dpi, work, base_width, base_height);
    let width = base_width.saturating_mul(dpi) / 96;
    let height = base_height.saturating_mul(dpi) / 96;
    let mut owner_rect = RECT::default();
    // SAFETY: an invalid owner leaves a visible work-area position.
    let _ = unsafe { GetWindowRect(parent, &mut owner_rect) };
    let x = (owner_rect.left + (owner_rect.right - owner_rect.left - width) / 2)
        .max(work.left)
        .min((work.right - width).max(work.left));
    let y = (owner_rect.top + (owner_rect.bottom - owner_rect.top - height) / 2)
        .max(work.top)
        .min((work.bottom - height).max(work.top));
    (x, y, width, height, dpi)
}

fn modal_loop(dialog: HWND) {
    let mut message = MSG::default();
    loop {
        // SAFETY: the caller owns the dialog until WM_NCDESTROY.
        if !unsafe { IsWindow(Some(dialog)).as_bool() } {
            break;
        }
        // SAFETY: message is initialized and stays live through dispatch.
        let got = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if got <= 0 {
            if got == 0 {
                // SAFETY: forward host shutdown after this nested loop.
                unsafe {
                    PostQuitMessage(message.wParam.0 as i32);
                }
            }
            // SAFETY: closure clears native text before returning.
            unsafe {
                let _ = DestroyWindow(dialog);
            }
            break;
        }
        // SAFETY: IsDialogMessage provides Tab, Enter, and Esc behavior.
        unsafe {
            if !IsDialogMessageW(dialog, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

fn create_controls(
    dialog: HWND,
    state: &mut DialogState,
    scale: impl Fn(i32) -> i32,
) -> windows::core::Result<()> {
    child(
        dialog,
        windows::core::w!("STATIC"),
        windows::core::w!("Sakura Pad 全体の解除パスワードを変更します。"),
        24,
        18,
        460,
        24,
        0,
        0,
        &scale,
    )?;
    child(
        dialog,
        windows::core::w!("STATIC"),
        windows::core::w!("変更後は新しいパスワードで Pad 全体を開きます。"),
        24,
        43,
        460,
        24,
        0,
        0,
        &scale,
    )?;
    child(
        dialog,
        windows::core::w!("STATIC"),
        windows::core::w!("現在のパスワード"),
        24,
        85,
        155,
        24,
        0,
        0,
        &scale,
    )?;
    state.old = password_edit(dialog, OLD_ID, 85, &scale)?;
    child(
        dialog,
        windows::core::w!("STATIC"),
        windows::core::w!("新しいパスワード"),
        24,
        130,
        155,
        24,
        0,
        0,
        &scale,
    )?;
    state.new = password_edit(dialog, NEW_ID, 130, &scale)?;
    child(
        dialog,
        windows::core::w!("STATIC"),
        windows::core::w!("新しいパスワード（確認）"),
        24,
        175,
        180,
        24,
        0,
        0,
        &scale,
    )?;
    state.confirm = password_edit(dialog, CONFIRM_ID, 175, &scale)?;
    state.status = child(
        dialog,
        windows::core::w!("STATIC"),
        windows::core::w!(""),
        24,
        225,
        460,
        30,
        0,
        0,
        &scale,
    )?;
    child(
        dialog,
        windows::core::w!("BUTTON"),
        windows::core::w!("変更する"),
        270,
        270,
        100,
        32,
        ACCEPT_ID,
        WS_TABSTOP.0 as i32 | BS_DEFPUSHBUTTON,
        &scale,
    )?;
    child(
        dialog,
        windows::core::w!("BUTTON"),
        windows::core::w!("キャンセル"),
        382,
        270,
        100,
        32,
        CANCEL_ID,
        WS_TABSTOP.0 as i32,
        &scale,
    )?;
    Ok(())
}

fn password_edit(
    dialog: HWND,
    id: u16,
    y: i32,
    scale: &impl Fn(i32) -> i32,
) -> windows::core::Result<HWND> {
    let edit = child(
        dialog,
        windows::core::w!("EDIT"),
        windows::core::w!(""),
        205,
        y - 3,
        278,
        28,
        id,
        WS_TABSTOP.0 as i32 | WS_BORDER.0 as i32 | ES_AUTOHSCROLL | ES_PASSWORD,
        scale,
    )?;
    // SAFETY: the live edit accepts ordinary paste, bounded to the Pad UI's
    // existing UTF-16 limit. There is no clipboard hook or paste filter.
    unsafe {
        let _ = SendMessageW(
            edit,
            EM_SETLIMITTEXT,
            Some(WPARAM(MAX_PASSWORD_UTF16_UNITS)),
            None,
        );
    }
    Ok(edit)
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
    // SAFETY: native controls copy the static labels during creation.
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
    // SAFETY: the pointer is installed before showing the dialog and removed
    // in WM_NCDESTROY. Creation messages safely use DefWindowProcW.
    let state = unsafe { GetWindowLongPtrW(dialog, GWLP_USERDATA) as *mut DialogState };
    match message {
        WM_COMMAND if !state.is_null() => match (w.0 & 0xffff) as u16 {
            ACCEPT_ID | 1 => {
                // SAFETY: this modal call exclusively owns the live state.
                let change = unsafe { read_change(&*state) };
                match change {
                    Ok(change) => {
                        // SAFETY: the raw pointer remains valid through the
                        // active modal call. Avoid a Rust borrow across the
                        // reentrant DestroyWindow messages below.
                        unsafe {
                            (*state).result = Some(change);
                        }
                        // SAFETY: WM_DESTROY clears the native edit buffers.
                        unsafe {
                            let _ = DestroyWindow(dialog);
                        }
                    }
                    Err(message) => {
                        // SAFETY: only non-secret, static diagnostics are used.
                        unsafe {
                            let _ = SetWindowTextW((*state).status, message);
                        }
                    }
                }
                return LRESULT(0);
            }
            CANCEL_ID | 2 => {
                // SAFETY: WM_DESTROY clears all three native edit buffers.
                unsafe {
                    let _ = DestroyWindow(dialog);
                }
                return LRESULT(0);
            }
            _ => {}
        },
        WM_CLOSE => {
            // SAFETY: closing is equivalent to Cancel.
            unsafe {
                let _ = DestroyWindow(dialog);
            }
            return LRESULT(0);
        }
        WM_DESTROY if !state.is_null() => {
            // SAFETY: child edits remain valid during parent destruction.
            // Clear their native text whether confirmed or cancelled.
            let state = unsafe { &*state };
            for edit in [state.old, state.new, state.confirm] {
                if !edit.is_invalid() {
                    // SAFETY: each HWND is a live child edit until its parent
                    // finishes this WM_DESTROY dispatch.
                    unsafe {
                        let _ = SetWindowTextW(edit, windows::core::w!(""));
                    }
                }
            }
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: remove the stack pointer before its owner can return.
            unsafe {
                SetWindowLongPtrW(dialog, GWLP_USERDATA, 0);
            }
        }
        _ => {}
    }
    // SAFETY: USER32 supplied this live HWND/message pair.
    unsafe { DefWindowProcW(dialog, message, w, l) }
}

fn read_change(state: &DialogState) -> Result<PasswordChange, PCWSTR> {
    let old =
        read_password(state.old).ok_or(windows::core::w!("現在のパスワードを入力してください"))?;
    let new = read_password(state.new).ok_or(windows::core::w!(
        "新しいパスワードを入力してください（最大 256 文字）"
    ))?;
    let confirmation = read_password(state.confirm)
        .ok_or(windows::core::w!("確認用パスワードを入力してください"))?;
    if new != confirmation {
        return Err(windows::core::w!("新しいパスワードが一致しません"));
    }
    if new == old {
        return Err(windows::core::w!(
            "現在と異なるパスワードを入力してください"
        ));
    }
    Ok(PasswordChange {
        old_password: old,
        new_password: new,
    })
}

fn read_password(edit: HWND) -> Option<Zeroizing<String>> {
    // SAFETY: this live edit's length is checked again to reject oversized
    // programmatic WM_SETTEXT, which bypasses EM_SETLIMITTEXT.
    let len = unsafe { GetWindowTextLengthW(edit) };
    if len <= 0 || len as usize > MAX_PASSWORD_UTF16_UNITS {
        return None;
    }
    let mut units = Zeroizing::new(vec![0_u16; len as usize + 1]);
    // SAFETY: the buffer holds the bounded UTF-16 value plus NUL.
    let copied = unsafe { GetWindowTextW(edit, &mut units) } as usize;
    if copied == 0 || copied != len as usize {
        return None;
    }
    decode_password(&units[..copied])
}

fn decode_password(units: &[u16]) -> Option<Zeroizing<String>> {
    if units.is_empty() || units.len() > MAX_PASSWORD_UTF16_UNITS {
        return None;
    }
    let text = Zeroizing::new(String::from_utf16(units).ok()?);
    if text.is_empty() || text.len() > MAX_PASSWORD_BYTES {
        return None;
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_policy_matches_pad_ui_and_worker_bounds() {
        assert!(decode_password(&[]).is_none());
        assert!(decode_password(&[b'x' as u16; 256]).is_some());
        assert!(decode_password(&[b'x' as u16; 257]).is_none());
        assert!(decode_password(&[0xd800]).is_none());
        assert!(decode_password(&[0x3042; 256]).is_some());
    }

    #[test]
    fn dialog_fits_small_monitor_work_area() {
        let work = RECT {
            left: -1366,
            top: 0,
            right: 0,
            bottom: 768,
        };
        let dpi = fitted_dpi(192, work, WIDTH, HEIGHT);
        assert!(WIDTH * dpi / 96 <= work.right - work.left);
        assert!(HEIGHT * dpi / 96 <= work.bottom - work.top);
        let chooser_dpi = fitted_dpi(192, work, CHOOSER_WIDTH, CHOOSER_HEIGHT);
        assert!(CHOOSER_WIDTH * chooser_dpi / 96 <= work.right - work.left);
        assert!(CHOOSER_HEIGHT * chooser_dpi / 96 <= work.bottom - work.top);
    }
}
