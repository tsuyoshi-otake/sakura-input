//! Sakura Input's renderer: the parts of the IME the user looks at.
//!
//! The renderer owns the floating mode indicator, notification-area icon,
//! candidate popup, and engine watchdog. Keeping all actual windows here
//! means neither conversion nor a DLL inside somebody else's application can
//! be stalled by paint work.
//!
//! # Shape
//!
//! Two threads and a strict rule about which touches what.
//!
//! - [`watch`] owns the pipe. It blocks — that is the whole design of the
//!   `WatchUi` long poll — so it can never be the thread that pumps
//!   messages, because a blocked message pump is a hung desktop.
//! - The main thread owns every window. Window handles belong to the
//!   thread that created them, so the watcher reports what it learns by
//!   posting a message rather than by touching a window.
//!
//! That is the whole of the concurrency in this process, and it is
//! deliberately this small.

#![cfg(windows)]
// A console window flashing up at logon, in front of whatever the user is
// doing, for a process that has no console output, is not acceptable. Only
// outside tests, which need the ordinary test-harness console.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod accessibility;
mod candidate;
mod glyph;
mod indicator;
mod tray;
mod watch;

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use sakura_proto::UiState;
use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetWindowLongPtrW,
    TranslateMessage, GWLP_USERDATA, MSG, WM_APP, WM_CLOSE, WM_DESTROY, WM_ENDSESSION, WNDCLASSW,
    WS_OVERLAPPED,
};

use candidate::CandidateWindow;
use indicator::Indicator;
use tray::Tray;
use watch::Signal;

/// The class of the window that owns the tray icon and receives the
/// watcher's reports.
///
/// Named rather than anonymous so that a person debugging a machine can
/// find this process's window. It is *not* how the renderer is asked to
/// exit: that arrives over the pipe, as the engine's shutdown announcement
/// (DESIGN 11 specifies `--stop` as asking engine and renderer to exit over
/// the pipe, and the renderer only holds the client end of it).
const HOST_CLASS: PCWSTR = windows::core::w!("SakuraInputRenderer");

/// Guards against two renderers in one logon session.
///
/// `Local\` scopes it to the session, which is the same scope the engine's
/// pipe has. Two renderers would mean two tray icons, two indicators
/// fighting over the same screen position, and two watchdogs racing to
/// restart one engine — and the logon task that starts this can fire twice
/// (a fast user switch, a repaired install) without anything being wrong.
const SINGLE_INSTANCE: PCWSTR = windows::core::w!(r"Local\SakuraInputRenderer");

/// The shell's callback for the tray icon. Owned here, not in [`tray`],
/// because `WM_APP`-relative ids are only unique within one window and this
/// is the module that knows what else that window uses.
pub const WM_TRAY: u32 = WM_APP + 1;

/// The watcher reporting that the single-slot UI mailbox has new state.
const WM_UI: u32 = WM_APP + 2;

/// The watcher reporting that the feed has ended for good.
const WM_ENDED: u32 = WM_APP + 3;

/// The windows the main thread owns, reached from the window procedure
/// through the host window's user data.
struct App {
    indicator: Indicator,
    candidates: CandidateWindow,
    tray: Tray,
    /// A latest-value mailbox shared with the blocking watcher. Multiple
    /// engine revisions can coalesce while the UI thread is busy painting.
    mailbox: Arc<Mutex<Option<UiState>>>,
    /// `TaskbarCreated`, resolved at startup. Its value is assigned by the
    /// system at run time, so it cannot be a constant and has to be carried
    /// to the place that compares against it.
    taskbar_created: u32,
}

fn main() -> Result<()> {
    let _com = accessibility::ComApartment::new()?;
    // SAFETY: called before any window is created. Failure means a manifest
    // or host policy already chose awareness; continuing is the safe fallback.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if already_running()? {
        return Ok(());
    }

    let host = create_host()?;
    let mailbox = Arc::new(Mutex::new(None));
    let mut app = App {
        indicator: Indicator::new()?,
        candidates: CandidateWindow::new()?,
        tray: Tray::new(host),
        mailbox: Arc::clone(&mailbox),
        // SAFETY: the name is a static wide literal. A zero result means
        // the message could not be registered, which only costs the
        // Explorer-restart recovery — and zero matches no real message, so
        // the comparison simply never fires.
        taskbar_created: unsafe { RegisterWindowMessageW(windows::core::w!("TaskbarCreated")) },
    };

    // Published before the watcher starts, so no message can arrive at a
    // window that cannot yet reach the state it needs.
    // SAFETY: `host` is live and `app` outlives the pump below, which is
    // the only thing that reads this pointer. It is cleared before `app`
    // is dropped.
    unsafe {
        SetWindowLongPtrW(host, GWLP_USERDATA, &raw mut app as isize);
    }

    // The watcher thread cannot hold an `HWND` — window handles are not
    // `Send`, and for good reason — so it carries the numeric value and
    // hands it straight back to `PostMessageW`, which is documented to be
    // callable from any thread.
    let target = host.0 as isize;
    watch::spawn(move |signal| report(target, &mailbox, signal));

    pump();

    // SAFETY: the pump has returned, so no further message can read this
    // pointer, and `app` is about to go out of scope.
    unsafe {
        SetWindowLongPtrW(host, GWLP_USERDATA, 0);
    }
    drop(app);
    Ok(())
}

/// Whether another renderer already holds this session's slot.
///
/// The mutex handle is deliberately leaked: it must live exactly as long as
/// the process, and the process ends by returning from `main` or by the
/// system tearing it down at logoff. Closing it early would let a second
/// renderer in.
fn already_running() -> Result<bool> {
    // SAFETY: the name is a static wide literal; the returned handle is
    // intentionally never closed.
    match unsafe { CreateMutexW(None, true, SINGLE_INSTANCE) } {
        Ok(_) => {
            // `CreateMutexW` succeeds whether or not the mutex already
            // existed; which case it was is only visible in the last error.
            let last = windows::core::Error::from_thread();
            Ok(last.code() == windows::core::HRESULT::from_win32(ERROR_ALREADY_EXISTS.0))
        }
        // Without the guard the safe answer is to run: a missing indicator
        // is worse than a duplicated one.
        Err(_) => Ok(false),
    }
}

/// Creates the hidden window that owns the tray icon.
///
/// A real top-level window rather than a message-only one, despite only
/// ever receiving messages. `TaskbarCreated` is a broadcast, and broadcasts
/// do not reach message-only windows — so the shell-restart recovery in
/// [`tray`] would silently never fire. It is simply never shown, which
/// makes it invisible in every way that matters.
fn create_host() -> Result<HWND> {
    // SAFETY: the class name is a static wide literal and the procedure is
    // a real `extern "system"` function.
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            lpszClassName: HOST_CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
    }

    // SAFETY: the class was just registered; the window is created without
    // `WS_VISIBLE` and is never shown.
    unsafe {
        CreateWindowExW(
            Default::default(),
            HOST_CLASS,
            windows::core::w!("Sakura Input"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
    }
}

/// Hands a watcher signal to the UI thread.
fn report(target: isize, mailbox: &Mutex<Option<UiState>>, signal: Signal) {
    let window = HWND(target as *mut c_void);
    let message = match signal {
        Signal::Ui(state) => {
            let mut slot = mailbox
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = Some(state);
            WM_UI
        }
        Signal::Ended => WM_ENDED,
    };
    // SAFETY: `window` is the host window, which outlives the watcher
    // thread — that thread is only unblocked by the process ending. A
    // failed post means the window is gone and there is nothing to tell.
    unsafe {
        let _ = PostMessageW(Some(window), message, WPARAM(0), LPARAM(0));
    }
}

/// The classic message loop, ending on `WM_QUIT`.
fn pump() {
    let mut message = MSG::default();
    // SAFETY: `message` outlives each call. `GetMessageW` returns 0 on
    // `WM_QUIT` and -1 on error; both end the loop, because a pump that
    // keeps calling a failing `GetMessageW` is a spin at 100% of a core.
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// The host window's message handler.
extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: reads back the pointer `main` published, which points at a
    // local that outlives the pump. Null until it is published and again
    // after it is cleared, which is why every use below is guarded.
    let app = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
    // SAFETY: as above. Read before the match so the shell's run-time
    // message can be compared without repeating the dereference.
    let taskbar_created = (!app.is_null()).then(|| unsafe { (*app).taskbar_created });

    match message {
        WM_UI if !app.is_null() => {
            // SAFETY: `app` is the live local from `main`, and this runs on
            // the thread that owns it — the pump dispatches on the main
            // thread, so no other reference to it exists at this moment.
            let app = unsafe { &mut *app };
            let state = app
                .mailbox
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            if let Some(state) = state {
                if let Some(mode) = state.mode {
                    app.indicator.show(mode);
                    let _ = app.tray.set(mode);
                }
                app.candidates.update(&state);
            }
            LRESULT(0)
        }
        // The engine stopped on purpose, or is gone for good. Either way
        // there is nothing left to render.
        //
        // `WM_CLOSE` and `WM_ENDSESSION` join it because logoff and
        // shutdown have to end the same way: returning from the pump is
        // what lets `Tray`'s destructor take the icon out of the
        // notification area, which the shell otherwise leaves behind as a
        // ghost until something hovers over it.
        WM_ENDED | WM_CLOSE | WM_ENDSESSION | WM_DESTROY => {
            // SAFETY: no arguments; posts `WM_QUIT` to this thread.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ if taskbar_created == Some(message) => {
            // SAFETY: as above — the live local, on its owning thread.
            let app = unsafe { &mut *app };
            let _ = app.tray.restore();
            LRESULT(0)
        }
        // SAFETY: the default handler is where every unhandled message
        // must go, with the arguments this procedure was given.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_proto::Mode;

    /// The three application messages must be distinct, or the tray's
    /// callback and the watcher's reports would be read as each other — a
    /// click on the icon would show a mode indicator for whatever mode the
    /// click coordinates happened to encode.
    #[test]
    fn the_application_messages_do_not_collide() {
        let all = [WM_TRAY, WM_UI, WM_ENDED];
        for (i, a) in all.iter().enumerate() {
            assert!(
                *a >= WM_APP,
                "{a} is below WM_APP and may collide with a system message"
            );
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    /// A state without a mode does not fabricate one for the indicator.
    #[test]
    fn no_mode_posts_the_code_that_decodes_to_nothing() {
        let absent: Option<Mode> = None;
        assert_eq!(absent.map(glyph::code), None);

        let present = Some(Mode::Katakana);
        assert_eq!(
            present.and_then(|mode| glyph::from_code(glyph::code(mode))),
            Some(Mode::Katakana)
        );
    }
}
