//! Thread-local wait cursor while an AI transform or proofread job is in flight.
//!
//! The TSF DLL runs on the host UI thread, so [`SetCursor`] is visible only
//! inside that application. `IDC_APPSTARTING` keeps an arrow so the user can
//! still click or change focus; a full wait cursor would look like a freeze.
//! Host `WM_SETCURSOR` handling restores the I-beam on mouse move, so the
//! 50 ms AI poll timer reasserts this cursor until the job ends.

use std::cell::Cell;

use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::GetCursor;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, LoadCursorW, SendMessageW, SetCursor, WindowFromPoint, HTCLIENT, IDC_APPSTARTING,
    WM_MOUSEMOVE, WM_SETCURSOR,
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
    let hit = HTCLIENT & 0xffff;
    let mouse = WM_MOUSEMOVE << 16;
    let lparam = LPARAM((mouse | hit) as isize);
    // SAFETY: asking the window under the cursor to apply its own class or
    // client cursor is the documented restore after a temporary `SetCursor`.
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETCURSOR,
            Some(WPARAM(hwnd.0 as usize)),
            Some(lparam),
        );
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
