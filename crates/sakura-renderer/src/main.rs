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
mod watch;

use std::ffi::c_void;
use std::sync::{mpsc::Receiver, Arc, Mutex};

use sakura_proto::UiState;
use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    PostMessageW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW, TranslateMessage,
    GWLP_USERDATA, MSG, WM_APP, WM_CLOSE, WM_DESTROY, WM_ENDSESSION, WNDCLASSW, WS_OVERLAPPED,
};

use candidate::CandidateWindow;
use indicator::Indicator;
use watch::{HistoryDeleteCompletion, Signal};

/// The class of the hidden window that receives the watcher's reports.
///
/// Named rather than anonymous so that a person debugging a machine can
/// find this process's window. It is *not* how the renderer is asked to
/// exit: that arrives over the pipe, as the engine's shutdown announcement
/// (DESIGN 11 specifies `--stop` as asking engine and renderer to exit over
/// the pipe, and the renderer only holds the client end of it).
const HOST_CLASS: PCWSTR = windows::core::w!("SakuraInputRenderer");

/// Guards against two production renderers in one logon session.
///
/// `Local\` scopes it to the session, which is the same scope the engine's
/// pipe has. Two renderers would mean two tray icons, two indicators
/// fighting over the same screen position, and two watchdogs racing to
/// restart one engine — and the logon task that starts this can fire twice
/// (a fast user switch, a repaired install) without anything being wrong.
const SINGLE_INSTANCE_NAME: &str = r"Local\SakuraInputRenderer";

/// The narrow namespace reserved for a real-process renderer fixture. This is
/// an argument, rather than an environment variable, so ordinary startup
/// cannot accidentally be redirected away from the engine's production pipe.
const TEST_PIPE_PREFIX: &str = r"\\.\pipe\SakuraInputRendererTest-";

/// The watcher reporting that the single-slot UI mailbox has new state.
const WM_UI: u32 = WM_APP + 2;

/// The watcher reporting that the feed has ended for good.
const WM_ENDED: u32 = WM_APP + 3;

/// A bounded delete request received an engine response. This only releases
/// duplicate-click suppression; it never changes candidates locally.
const WM_HISTORY_DELETE_FINISHED: u32 = WM_APP + 4;

/// The windows the main thread owns, reached from the window procedure
/// through the host window's user data.
struct App {
    indicator: Indicator,
    candidates: CandidateWindow,
    /// A latest-value mailbox shared with the blocking watcher. Multiple
    /// engine revisions can coalesce while the UI thread is busy painting.
    mailbox: Arc<Mutex<Option<UiState>>>,
    history_delete_completions: Receiver<HistoryDeleteCompletion>,
}

fn main() -> Result<()> {
    let options = match startup_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("sakura-renderer: {error}");
            std::process::exit(1);
        }
    };
    let _com = accessibility::ComApartment::new()?;
    // SAFETY: called before any window is created. Failure means a manifest
    // or host policy already chose awareness; continuing is the safe fallback.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if already_running(options.test_pipe.as_deref())? {
        return Ok(());
    }

    let host = create_host()?;
    let mailbox = Arc::new(Mutex::new(None));
    let (history_delete, history_delete_completions) = watch::spawn_history_deleter(
        host.0 as isize,
        WM_HISTORY_DELETE_FINISHED,
        options.test_pipe.clone(),
    );
    let mut app = App {
        indicator: Indicator::new()?,
        candidates: CandidateWindow::new(history_delete)?,
        mailbox: Arc::clone(&mailbox),
        history_delete_completions,
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
    match options.test_pipe {
        Some(pipe_name) => {
            watch::spawn_test_pipe(pipe_name, move |signal| report(target, &mailbox, signal))
        }
        None => watch::spawn(move |signal| report(target, &mailbox, signal)),
    };

    pump();

    // SAFETY: the pump has returned, so no further message can read this
    // pointer, and `app` is about to go out of scope.
    unsafe {
        SetWindowLongPtrW(host, GWLP_USERDATA, 0);
    }
    drop(app);
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct StartupOptions {
    test_pipe: Option<String>,
}

fn startup_options(
    arguments: impl IntoIterator<Item = String>,
) -> std::result::Result<StartupOptions, String> {
    let mut options = StartupOptions::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == "--test-pipe" {
            let value = arguments
                .next()
                .ok_or_else(|| "--test-pipe requires a named-pipe path".to_owned())?;
            if options
                .test_pipe
                .replace(validate_test_pipe(value)?)
                .is_some()
            {
                return Err("--test-pipe may be supplied only once".to_owned());
            }
        }
        // The production renderer historically has no command-line surface.
        // Preserve harmless ignoring of unrelated launcher arguments; only
        // the explicit test override is interpreted.
    }
    Ok(options)
}

fn validate_test_pipe(value: String) -> std::result::Result<String, String> {
    let suffix = value
        .strip_prefix(TEST_PIPE_PREFIX)
        .ok_or_else(|| format!("--test-pipe must start with {TEST_PIPE_PREFIX:?}"))?;
    if suffix.is_empty()
        || suffix.len() > 160
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "--test-pipe must have a non-empty ASCII alphanumeric, '-' or '_' suffix".to_owned(),
        );
    }
    Ok(value)
}

/// Whether another renderer already holds this session's slot.
///
/// The mutex handle is deliberately leaked: it must live exactly as long as
/// the process, and the process ends by returning from `main` or by the
/// system tearing it down at logoff. Closing it early would let a second
/// renderer in.
fn already_running(test_pipe: Option<&str>) -> Result<bool> {
    let mutex_name = match test_pipe {
        Some(pipe_name) => format!(
            "Local\\SakuraInputRendererTest-{}",
            test_pipe_suffix(pipe_name)
        ),
        None => SINGLE_INSTANCE_NAME.to_owned(),
    };
    let mut wide: Vec<u16> = mutex_name.encode_utf16().collect();
    wide.push(0);
    // SAFETY: `wide` is NUL-terminated and remains live for the call; the
    // returned handle is intentionally leaked for the process lifetime.
    match unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) } {
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

fn test_pipe_suffix(pipe_name: &str) -> &str {
    // `StartupOptions` contains this only after `validate_test_pipe` accepted
    // it, so this cannot redirect a test mutex into the production namespace.
    pipe_name
        .strip_prefix(TEST_PIPE_PREFIX)
        .expect("validated test pipe retains its prefix")
}

/// Creates the hidden watcher host window. It is a real top-level window so
/// ordinary UI-thread messages have a stable owner, but it is never shown.
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
            *slot = Some(*state);
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
                }
                app.candidates.update(&state);
            }
            LRESULT(0)
        }
        WM_HISTORY_DELETE_FINISHED if !app.is_null() => {
            // SAFETY: as in the WM_UI arm, the main window procedure is the
            // sole mutable owner of `app`.
            let app = unsafe { &mut *app };
            while let Ok(HistoryDeleteCompletion(request)) =
                app.history_delete_completions.try_recv()
            {
                app.candidates.history_delete_finished(request);
            }
            LRESULT(0)
        }
        // The engine stopped on purpose, or is gone for good. Either way
        // there is nothing left to render.
        //
        // `WM_CLOSE` and `WM_ENDSESSION` join it because logoff and shutdown
        // have to end the same way.
        WM_ENDED | WM_CLOSE | WM_ENDSESSION | WM_DESTROY => {
            // SAFETY: no arguments; posts `WM_QUIT` to this thread.
            unsafe { PostQuitMessage(0) };
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

    /// The watcher's application messages must stay distinct.
    #[test]
    fn the_application_messages_do_not_collide() {
        let all = [WM_UI, WM_ENDED, WM_HISTORY_DELETE_FINISHED];
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

    #[test]
    fn startup_accepts_only_the_private_test_pipe_override() {
        assert_eq!(startup_options(Vec::new()), Ok(StartupOptions::default()));
        let pipe = format!("{TEST_PIPE_PREFIX}worker-abc_123");
        assert_eq!(
            startup_options(["--test-pipe".to_owned(), pipe.clone()]),
            Ok(StartupOptions {
                test_pipe: Some(pipe)
            })
        );
        assert!(startup_options(["--test-pipe".to_owned()]).is_err());
        assert!(startup_options([
            "--test-pipe".to_owned(),
            r"\\.\pipe\sakura_input_production".to_owned()
        ])
        .is_err());
        assert!(startup_options([
            "--test-pipe".to_owned(),
            format!("{TEST_PIPE_PREFIX}one"),
            "--test-pipe".to_owned(),
            format!("{TEST_PIPE_PREFIX}two"),
        ])
        .is_err());
    }

    #[test]
    fn test_pipe_uses_a_distinct_per_fixture_instance_guard() {
        let pipe = format!("{TEST_PIPE_PREFIX}worker-abc_123");
        assert_eq!(
            format!("Local\\SakuraInputRendererTest-{}", test_pipe_suffix(&pipe)),
            r"Local\SakuraInputRendererTest-worker-abc_123"
        );
    }
}
