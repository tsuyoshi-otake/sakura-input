//! Thread-local wait cursor while an AI transform or proofread job is in flight.
//!
//! [`SetCursor`] affects only the calling thread's input queue, so the wait
//! cursor is visible in the host application's windows only while they are
//! handled on the thread that called [`show`]. `IDC_APPSTARTING` keeps an
//! arrow so the user can still click or change focus; a full wait cursor
//! would look like a freeze. Host `WM_SETCURSOR` handling restores the
//! I-beam on mouse move, so the 50 ms AI poll timer reasserts this cursor
//! until the job ends.
//!
//! [`restore`] asks the window under the pointer to apply its own cursor
//! again. When the calling thread owns that window, the `WM_SETCURSOR` send
//! dispatches the window procedure directly. When another thread or process
//! owns it, the send is bounded by `SendMessageTimeoutW` so a hung
//! application cannot freeze the host UI thread.

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::GetCursor;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindowThreadProcessId, LoadCursorW, SendMessageTimeoutW, SendMessageW,
    SetCursor, WindowFromPoint, HTCLIENT, IDC_APPSTARTING, SMTO_ABORTIFHUNG, WM_MOUSEMOVE,
    WM_SETCURSOR,
};

thread_local! {
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

/// Shows the working cursor for the calling host thread.
pub fn show() {
    BUSY.with(|busy| busy.set(true));
    set_appstarting();
}

/// Restores the window under the mouse to its own cursor, if we set the wait
/// cursor. Harmless when no wait cursor is active.
pub fn restore() {
    let was_busy = BUSY.with(|busy| busy.replace(false));
    if was_busy {
        restore_host_cursor();
    }
}

#[cfg(test)]
fn is_busy() -> bool {
    BUSY.with(Cell::get)
}

fn set_appstarting() {
    // SAFETY: `IDC_APPSTARTING` is a shared system cursor; it must not be
    // destroyed. `SetCursor` only affects this thread's windows.
    unsafe {
        let Ok(cursor) = LoadCursorW(None, IDC_APPSTARTING) else {
            return;
        };
        let _ = SetCursor(Some(cursor));
    }
}

fn restore_host_cursor() {
    let mut point = POINT::default();
    // SAFETY: `point` is caller-owned writable storage for the current
    // pointer position.
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return;
    }
    // SAFETY: `WindowFromPoint` reads screen coordinates and returns a
    // borrowed HWND that is only sent a standard `WM_SETCURSOR`.
    let hwnd = unsafe { WindowFromPoint(point) };
    if hwnd.is_invalid() {
        return;
    }
    send_restore(hwnd);
}

/// Upper bound for a `WM_SETCURSOR` restore sent to a window owned by
/// another thread. The wait happens on the host UI thread, so it must fit
/// the 50 ms per-key budget (#148), which is also the AI poll period. On
/// timeout the other thread keeps control of its own cursor: the cursor
/// over that thread's window is already set by its own `WM_SETCURSOR`
/// handling.
const CROSS_THREAD_RESTORE_TIMEOUT_MS: u32 = 50;

fn send_restore(hwnd: HWND) {
    let hit = HTCLIENT & 0xffff;
    let mouse = WM_MOUSEMOVE << 16;
    let lparam = LPARAM((mouse | hit) as isize);
    let wparam = WPARAM(hwnd.0 as usize);
    // SAFETY: `GetWindowThreadProcessId` takes a null process-id out
    // pointer via `None`; a 0 return means the window is already gone and
    // falls through to the bounded send, which then fails harmlessly.
    let owner_thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
    // SAFETY: `GetCurrentThreadId` takes no pointers and cannot fail.
    if owner_thread == unsafe { GetCurrentThreadId() } {
        // SAFETY: a same-thread send dispatches the window procedure
        // directly and never waits on another thread's message loop. Asking
        // the window under the cursor to apply its own class or client
        // cursor is the documented restore after a temporary `SetCursor`.
        unsafe {
            SendMessageW(hwnd, WM_SETCURSOR, Some(wparam), Some(lparam));
        }
    } else {
        // SAFETY: `SMTO_ABORTIFHUNG` bounds the cross-thread wait to
        // `CROSS_THREAD_RESTORE_TIMEOUT_MS` on the host UI thread. Without
        // `SMTO_BLOCK`, this thread still processes incoming sent messages
        // while it waits, exactly as `SendMessageW` did, so reentrancy is
        // unchanged; only the wait is bounded. The result is ignored: on
        // timeout the owning thread keeps control of its own cursor.
        unsafe {
            let _ = SendMessageTimeoutW(
                hwnd,
                WM_SETCURSOR,
                wparam,
                lparam,
                SMTO_ABORTIFHUNG,
                CROSS_THREAD_RESTORE_TIMEOUT_MS,
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_marks_the_thread_busy_and_restore_clears_it() {
        restore();
        assert!(!is_busy());
        show();
        assert!(is_busy());
        // SAFETY: both handles are shared system cursors loaded for this test.
        let loaded = unsafe { LoadCursorW(None, IDC_APPSTARTING) };
        assert!(loaded.is_ok(), "system IDC_APPSTARTING cursor must load");
        if let Ok(wait) = loaded {
            // SAFETY: GetCursor returns the current thread cursor handle and
            // does not dereference the handle.
            let current_cursor = unsafe { GetCursor() };
            assert_eq!(
                current_cursor, wait,
                "AI wait should use IDC_APPSTARTING, not a blocking wait cursor"
            );
        }
        restore();
        assert!(!is_busy());
        restore();
        assert!(!is_busy());
    }
}

#[cfg(test)]
#[path = "ai_wait_cursor_restore_tests.rs"]
mod restore_tests;
