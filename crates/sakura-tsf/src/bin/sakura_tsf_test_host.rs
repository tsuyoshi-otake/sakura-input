#![windows_subsystem = "windows"]

//! Minimal native edit host for physical TSF integration tests.
//!
//! The executable intentionally contains no Sakura-specific input logic. A
//! standard Win32 EDIT control, the active Windows language profile, and the
//! normal message loop are the entire host surface. This keeps the E2E honest:
//! synthesized keyboard input must travel through User32 and TSF exactly as it
//! does in an ordinary desktop application.

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, DEFAULT_GUI_FONT};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowTextW, LoadCursorW, MoveWindow, PostQuitMessage, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT, ES_AUTOHSCROLL, GWLP_USERDATA, IDC_ARROW, MSG,
    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_CREATE, WM_DESTROY, WM_SETFOCUS,
    WM_SETFONT, WM_SIZE, WNDCLASSW, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_TABSTOP,
    WS_VISIBLE,
};

const HOST_CLASS: windows::core::PCWSTR = w!("SakuraInputTsfTestHost");
const HOST_TITLE: windows::core::PCWSTR = w!("Sakura Input TSF Test Host");
const HOST_STARTING_TITLE: windows::core::PCWSTR = w!("Sakura Input TSF Test Host (starting)");
const SNAPSHOT_EDIT_TEXT: u32 = WM_APP + 37;

fn main() -> Result<()> {
    // SAFETY: process DPI awareness is configured before any HWND is created.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    register_class()?;
    let window = create_host_window()?;
    // SAFETY: `window` is a live top-level HWND created on this thread.
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
        let _ = SetForegroundWindow(window);
        let edit = HWND(GetWindowLongPtrW(window, GWLP_USERDATA) as *mut _);
        if !edit.0.is_null() {
            let _ = SetFocus(Some(edit));
        }
        // The controller finds the final title only after the UI thread has
        // completed its own foreground/focus initialization. Publishing that
        // title is the host's explicit ready boundary.
        let _ = SetWindowTextW(window, HOST_TITLE);
    }

    let mut message = MSG::default();
    // SAFETY: the message structure remains live for the duration of the loop.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        // SAFETY: `message` was initialized by a successful GetMessageW call.
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn register_class() -> Result<()> {
    // SAFETY: the class strings and procedure have static lifetime. The stock
    // cursor remains owned by the system.
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_procedure),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: HOST_CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
    }
    Ok(())
}

fn create_host_window() -> Result<HWND> {
    // SAFETY: the class was registered immediately above and all pointer
    // arguments either have static lifetime or are null.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            HOST_CLASS,
            HOST_STARTING_TITLE,
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            720,
            220,
            None,
            None,
            None,
            None,
        )
    }
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => {
            // SAFETY: WM_CREATE provides a valid CREATESTRUCTW for this call.
            let _create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            // SAFETY: this creates a standard child EDIT owned by `window`.
            let edit = unsafe {
                CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    w!("EDIT"),
                    w!(""),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                    16,
                    24,
                    672,
                    36,
                    Some(window),
                    None,
                    None,
                    None,
                )
            };
            let Ok(edit) = edit else {
                return LRESULT(-1);
            };
            // SAFETY: the stock font is process-independent and the synchronous
            // message does not outlive it. The HWND value fits in LONG_PTR.
            unsafe {
                let font = GetStockObject(DEFAULT_GUI_FONT);
                SendMessageW(
                    edit,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
                SetWindowLongPtrW(window, GWLP_USERDATA, edit.0 as isize);
                let _ = SetFocus(Some(edit));
            }
            LRESULT(0)
        }
        WM_SIZE => {
            let width = (lparam.0 & 0xFFFF) as i32;
            let height = ((lparam.0 >> 16) & 0xFFFF) as i32;
            // SAFETY: the stored value is either zero before WM_CREATE settles
            // or the child HWND written by that handler.
            let edit = HWND(unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut _);
            if !edit.0.is_null() {
                // SAFETY: `edit` is the live child window owned by `window`.
                unsafe {
                    let _ = MoveWindow(
                        edit,
                        16,
                        24,
                        (width - 32).max(80),
                        (height - 48).max(28),
                        true,
                    );
                }
            }
            LRESULT(0)
        }
        WM_SETFOCUS => {
            // SAFETY: see WM_SIZE; focusing a null handle is avoided.
            let edit = HWND(unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut _);
            if !edit.0.is_null() {
                // SAFETY: `edit` is a child of the active host window.
                unsafe {
                    let _ = SetFocus(Some(edit));
                }
            }
            LRESULT(0)
        }
        SNAPSHOT_EDIT_TEXT => {
            // Test-only observation stays inside the host process: unlike a
            // top-level caption, Windows does not expose another process's EDIT
            // text through GetWindowTextW. Copying it to our own caption gives
            // the controller a bounded, pointer-free User32 observation path.
            // SAFETY: the value was written by this UI thread during WM_CREATE.
            let edit = HWND(unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut _);
            if !edit.0.is_null() {
                let mut text = [0u16; 2048];
                // SAFETY: both HWNDs belong to this UI thread and `text` is
                // retained as writable UTF-16 storage for both calls.
                unsafe {
                    let copied = GetWindowTextW(edit, &mut text) as usize;
                    let terminator_index = copied.min(text.len() - 1);
                    if let Some(terminator) = text.get_mut(terminator_index) {
                        *terminator = 0;
                    }
                    let _ = SetWindowTextW(window, windows::core::PCWSTR(text.as_ptr()));
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: `window` is the top-level HWND receiving WM_CLOSE.
            unsafe {
                let _ = DestroyWindow(window);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: this thread owns the only message loop in the process.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            // SAFETY: unhandled messages retain the standard Win32 behavior.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}
