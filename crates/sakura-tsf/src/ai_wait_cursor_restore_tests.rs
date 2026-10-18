#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression tests for the `send_restore` split in `ai_wait_cursor` (#360
//! A7). `WindowFromPoint` depends on the real mouse position, so the tests
//! call `super::send_restore` directly on message-only windows (parent
//! `HWND_MESSAGE`): nothing appears on screen and the windows still receive
//! sent messages like any real window.
//!
//! Each window class is registered against `GetModuleHandleW(None)` (the
//! test executable, never `user32.dll`) under a class name unique to its
//! test, and every window is destroyed and every class unregistered before
//! the test ends, including on failure (drop guards).
//!
//! Oracle values are written from the Win32 documentation rather than the
//! module's own computation: `WM_SETCURSOR` must arrive with
//! `wParam == hwnd`, `LOWORD(lParam) == HTCLIENT (1)`, and
//! `HIWORD(lParam) == WM_MOUSEMOVE (0x0200)`.

use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW,
    SetWindowLongPtrW, UnregisterClassW, CW_USEDEFAULT, GWLP_USERDATA, HWND_MESSAGE, MSG,
    PM_REMOVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_SETCURSOR, WNDCLASSW,
};

use super::send_restore;

/// Win32-documented oracle constants for the expected `WM_SETCURSOR` send.
const ORACLE_HTCLIENT: usize = 1;
const ORACLE_WM_MOUSEMOVE: usize = 0x0200;
/// Bound for a hung-window restore, far above the 50 ms implementation
/// timeout and far below any wait a human would accept.
const CALLER_BUDGET: Duration = Duration::from_secs(2);

/// What a test window procedure observed. Owned by the test (or by the
/// window thread for T2, which does not assert on it) and reached through
/// `GWLP_USERDATA`.
#[derive(Default)]
struct Recorded {
    hits: AtomicUsize,
    last_wparam: AtomicUsize,
    last_lparam: AtomicIsize,
}

impl Recorded {
    fn assert_one_oracle_send(&self, hwnd: HWND) {
        assert_eq!(
            self.hits.load(Ordering::SeqCst),
            1,
            "window procedure must receive WM_SETCURSOR exactly once"
        );
        assert_eq!(
            self.last_wparam.load(Ordering::SeqCst),
            hwnd.0 as usize,
            "WM_SETCURSOR wParam must be the window handle"
        );
        let lparam = self.last_lparam.load(Ordering::SeqCst) as usize;
        assert_eq!(
            lparam & 0xffff,
            ORACLE_HTCLIENT,
            "LOWORD(lParam) must be HTCLIENT (1)"
        );
        assert_eq!(
            (lparam >> 16) & 0xffff,
            ORACLE_WM_MOUSEMOVE,
            "HIWORD(lParam) must be WM_MOUSEMOVE (0x0200)"
        );
    }
}

unsafe extern "system" fn recording_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_SETCURSOR {
        // SAFETY: the owning thread stored a `*const Recorded` in
        // GWLP_USERDATA before publishing the HWND, and the pointee outlives
        // the window because cleanup happens only after the threads join.
        let rec = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Recorded;
        if !rec.is_null() {
            // SAFETY: see above.
            let rec = unsafe { &*rec };
            rec.hits.fetch_add(1, Ordering::SeqCst);
            rec.last_wparam.store(wparam.0, Ordering::SeqCst);
            rec.last_lparam.store(lparam.0, Ordering::SeqCst);
        }
    } else if message == WM_CLOSE {
        // SAFETY: both calls are made on the thread that owns `hwnd`.
        unsafe {
            let _ = DestroyWindow(hwnd);
            PostQuitMessage(0);
        }
        return LRESULT(0);
    }
    // SAFETY: forwarding to the default procedure is always valid.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Registers `class` on the test executable's module and creates a
/// message-only window that reports to `rec`. Must be called on the thread
/// that will own the window.
fn register_and_create(
    class: PCWSTR,
    rec: *const Recorded,
) -> (HWND, windows::Win32::Foundation::HMODULE) {
    // SAFETY: `None` resolves to this executable's module handle.
    let instance = unsafe { GetModuleHandleW(None) }.expect("test executable module handle");
    let wndclass = WNDCLASSW {
        lpfnWndProc: Some(recording_proc),
        hInstance: instance.into(),
        lpszClassName: class,
        ..Default::default()
    };
    // SAFETY: the class strings and procedure have static lifetime.
    let atom = unsafe { RegisterClassW(&wndclass) };
    assert_ne!(atom, 0, "RegisterClassW failed for {:?}", class);
    // SAFETY: the class was just registered, `HWND_MESSAGE` makes the window
    // message-only, and every optional argument is null.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            class,
            WINDOW_STYLE::default(),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    }
    .expect("message-only test window");
    // SAFETY: called on the thread that owns `hwnd`; `rec` outlives the
    // window as described on `recording_proc`.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, rec as isize);
    }
    (hwnd, instance)
}

/// Destroys a same-thread window and unregisters its class, also on panic.
struct OwnedWindow {
    hwnd: HWND,
    class: PCWSTR,
    instance: windows::Win32::Foundation::HMODULE,
}

impl Drop for OwnedWindow {
    fn drop(&mut self) {
        // SAFETY: this guard only ever wraps windows created and dropped on
        // the same thread.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = UnregisterClassW(self.class, Some(self.instance.into()));
        }
    }
}

/// W2: a window owned by the calling thread still receives `WM_SETCURSOR`
/// synchronously with `wParam == hwnd` and
/// `lParam == MAKELPARAM(HTCLIENT, WM_MOUSEMOVE)`. The host's own cursor
/// restore is exactly as before.
#[test]
fn same_thread_window_receives_the_oracle_send() {
    let rec = Recorded::default();
    let (hwnd, instance) = register_and_create(w!("SakuraA7RestoreT1"), &rec);
    let _guard = OwnedWindow {
        hwnd,
        class: w!("SakuraA7RestoreT1"),
        instance,
    };

    send_restore(hwnd);

    rec.assert_one_oracle_send(hwnd);
}

/// W1: `restore` must never wait on another thread without a bound. Thread
/// W owns a window and blocks without pumping messages, so the window is
/// unreachable. The caller thread C must still return from `send_restore`
/// within `CALLER_BUDGET`.
#[test]
fn hung_foreign_thread_send_returns_bounded() {
    let (hwnd_tx, hwnd_rx) = channel::<usize>();
    let (release_tx, release_rx) = channel::<()>();
    let (done_tx, done_rx) = channel::<Duration>();

    // W creates the window, publishes it, then blocks on `release_rx`
    // without pumping messages: the window is unreachable until released.
    let window_thread = thread::spawn(move || {
        let rec = Recorded::default();
        let class = w!("SakuraA7RestoreT2");
        let (hwnd, instance) = register_and_create(class, &rec);
        hwnd_tx.send(hwnd.0 as usize).expect("publish hwnd");
        let _ = release_rx.recv();
        // Drain pending sent messages so a pre-fix caller parked in
        // `SendMessageW` can observe delivery and finish, then clean up.
        // SAFETY: `msg` is caller-owned; all calls run on the owning thread.
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            let _ = DestroyWindow(hwnd);
            let _ = UnregisterClassW(class, Some(instance.into()));
        }
    });

    /// Releases W and joins it even if the test panics early, so the window
    /// is destroyed and the class unregistered before the test ends.
    struct ReleaseGuard {
        release: Sender<()>,
        thread: Option<JoinHandle<()>>,
    }
    impl Drop for ReleaseGuard {
        fn drop(&mut self) {
            let _ = self.release.send(());
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    let _release = ReleaseGuard {
        release: release_tx,
        thread: Some(window_thread),
    };

    let hwnd_usize = hwnd_rx
        .recv_timeout(CALLER_BUDGET)
        .expect("window thread must publish its hwnd");

    // Never call `send_restore` for this window on the test thread itself:
    // the test thread does not own the window, and an unbounded send would
    // deadlock the test.
    let caller = thread::spawn(move || {
        let started = Instant::now();
        send_restore(HWND(hwnd_usize as *mut _));
        let _ = done_tx.send(started.elapsed());
    });

    let result = done_rx.recv_timeout(CALLER_BUDGET);
    drop(_release);
    let _ = caller.join();

    match result {
        Ok(elapsed) => assert!(
            elapsed < CALLER_BUDGET,
            "send_restore took {elapsed:?}, expected < {CALLER_BUDGET:?}"
        ),
        Err(_) => panic!("send_restore did not return within {CALLER_BUDGET:?}"),
    }
}

/// W3: a responsive window on another thread still receives the same
/// `WM_SETCURSOR` (oracle wParam/lParam) before `send_restore` returns.
#[test]
fn responsive_foreign_thread_still_receives_the_send() {
    let rec = Recorded::default();
    let rec_ptr = &rec as *const Recorded as usize;
    let (hwnd_tx, hwnd_rx) = channel::<usize>();

    // W owns the window and pumps messages until told to quit via WM_CLOSE.
    let window_thread = thread::spawn(move || {
        let class = w!("SakuraA7RestoreT3");
        let (hwnd, instance) = register_and_create(class, rec_ptr as *const Recorded);
        hwnd_tx.send(hwnd.0 as usize).expect("publish hwnd");
        // SAFETY: `msg` is caller-owned and this thread owns `hwnd`.
        unsafe {
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
            let _ = UnregisterClassW(class, Some(instance.into()));
        }
    });

    /// Stops W's message loop even if an assertion fails, so the window is
    /// destroyed and the class unregistered before the test ends.
    struct QuitGuard {
        hwnd: HWND,
        thread: Option<JoinHandle<()>>,
    }
    impl Drop for QuitGuard {
        fn drop(&mut self) {
            // SAFETY: posting WM_CLOSE is safe on any thread; the owning
            // thread destroys the window and quits.
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    let hwnd_usize = hwnd_rx
        .recv_timeout(CALLER_BUDGET)
        .expect("window thread must publish its hwnd");
    let hwnd = HWND(hwnd_usize as *mut _);
    let _quit = QuitGuard {
        hwnd,
        thread: Some(window_thread),
    };

    send_restore(hwnd);

    rec.assert_one_oracle_send(hwnd);
}
