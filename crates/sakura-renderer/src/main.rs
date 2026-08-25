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
mod pad;
mod pad_caption;
mod pad_gesture;
mod pad_icon;
mod pad_list;
mod pad_rail;
mod pad_storage;
mod pad_tooltip;
mod raw_input;
mod theme;
mod watch;

use std::ffi::c_void;
#[cfg(debug_assertions)]
use std::fs::OpenOptions;
#[cfg(debug_assertions)]
use std::io::Write;
use std::sync::{mpsc::Receiver, Arc, Mutex};

use sakura_proto::{AppearanceTheme, Mode, PadShortcut, UiState};
use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageTime, GetMessageW,
    GetWindowLongPtrW, KillTimer, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer,
    SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, MSG, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_ENDSESSION, WM_INPUT, WM_INPUT_DEVICE_CHANGE, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};

use candidate::CandidateWindow;
use indicator::Indicator;
use pad::PadWindow;
use pad_gesture::GestureResult;
use raw_input::RawInputOwner;
use watch::{CandidateCommitCompletion, HistoryDeleteCompletion, Signal};

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

/// A bounded delete attempt reached a terminal outcome. Failed or negative
/// outcomes release duplicate-click suppression; successful removal still
/// waits for the next authoritative UI revision.
const WM_HISTORY_DELETE_FINISHED: u32 = WM_APP + 4;
const WM_CANDIDATE_COMMIT_FINISHED: u32 = WM_APP + 5;
/// A raw-input gesture never opens the pad from inside WM_INPUT.  The host
/// posts this deferred message so the complete USER32 packet has returned and
/// normal message ordering/focus rules remain observable.
const WM_PAD_TRIGGER: u32 = WM_APP + 6;
/// A short UI-thread timer gives the pure gesture reducer an explicit timeout
/// even when no further keyboard packet arrives after the first tap.
const PAD_GESTURE_TIMER: usize = 0x5342;

#[cfg(debug_assertions)]
fn pad_debug(event: &str) {
    let Some(path) = std::env::var_os("SAKURA_PAD_DEBUG_LOG") else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{event}");
    }
}

#[cfg(not(debug_assertions))]
fn pad_debug(_: &str) {}

/// The windows the main thread owns, reached from the window procedure
/// through the host window's user data.
struct App {
    indicator: Indicator,
    candidates: CandidateWindow,
    /// Created lazily so an optional memo/storage failure cannot take down
    /// candidate UI or the mode indicator.
    pad: Option<PadWindow>,
    pad_theme: AppearanceTheme,
    raw_input: RawInputOwner,
    pad_shortcut: PadShortcut,
    pad_config_generation: u64,
    /// The mode and theme of the previous UI state. The engine retains the
    /// mode on every revision — candidate updates included — so the
    /// indicator must compare against this to show only on an actual
    /// change, or it would sit re-triggered beside the caret for as long as
    /// the user types. The theme is part of the key because a system
    /// light/dark switch must repaint an indicator the mode alone would
    /// leave stale.
    shown_indicator: Option<(Mode, AppearanceTheme)>,
    /// A latest-value mailbox shared with the blocking watcher. Multiple
    /// engine revisions can coalesce while the UI thread is busy painting.
    mailbox: Arc<Mutex<Option<UiState>>>,
    history_delete_completions: Receiver<HistoryDeleteCompletion>,
    candidate_commit_completions: Receiver<CandidateCommitCompletion>,
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
    let mut raw_input = RawInputOwner::new(host, 0);
    // Preferences default to Disabled.  The first generation-stamped UiState
    // may enable the Ctrl double-tap gesture; until then the hidden host has
    // no raw-input registration and therefore cannot observe keyboard input.
    raw_input.set_enabled(false)?;
    let mailbox = Arc::new(Mutex::new(None));
    let (history_delete, history_delete_completions) = watch::spawn_history_deleter(
        host.0 as isize,
        WM_HISTORY_DELETE_FINISHED,
        options.test_pipe.clone(),
    );
    let (candidate_commit, candidate_commit_completions) = watch::spawn_candidate_committer(
        host.0 as isize,
        WM_CANDIDATE_COMMIT_FINISHED,
        options.test_pipe.clone(),
    );
    let mut app = App {
        indicator: Indicator::new()?,
        candidates: CandidateWindow::new(history_delete, candidate_commit)?,
        pad: None,
        pad_theme: AppearanceTheme::Auto,
        raw_input,
        pad_shortcut: PadShortcut::Disabled,
        pad_config_generation: 0,
        shown_indicator: None,
        mailbox: Arc::clone(&mailbox),
        history_delete_completions,
        candidate_commit_completions,
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

/// Whether a fresh UI state means the mode indicator should appear.
///
/// Only an actual change of mode or theme does. The engine bumps its UI
/// revision for every candidate update while typing and each of those
/// states carries the unchanged current mode, so showing on every state
/// would keep the indicator permanently beside the text the user is trying
/// to read. A theme flip with the same mode still shows: the indicator may
/// be on screen in the old palette and must repaint.
fn indicator_change_shows(
    shown: Option<(Mode, AppearanceTheme)>,
    next: Option<(Mode, AppearanceTheme)>,
) -> bool {
    next.is_some() && next != shown
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
            // The Pad is the only renderer window with focusable children, so
            // it is the only one that needs the dialog keyboard. Every other
            // window sees the message untouched.
            if pad::dialog_navigation(&message) {
                continue;
            }
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
                let next = state.mode.map(|mode| (mode, state.appearance_theme));
                app.pad_theme = state.appearance_theme;
                if let Some(pad) = app.pad.as_mut() {
                    pad.set_theme(state.appearance_theme);
                }
                if state.pad_shortcut != app.pad_shortcut {
                    // Wrapping still produces a distinct generation at the
                    // u64 boundary (max -> 0), so a configuration change
                    // cannot leave a pending gesture alive forever.
                    app.pad_config_generation = app.pad_config_generation.wrapping_add(1);
                    let _ = app
                        .raw_input
                        .set_generation(app.pad_config_generation, message_time_ms());
                    let enabled = matches!(state.pad_shortcut, PadShortcut::DoubleCtrl);
                    if !enabled {
                        // Disabling is fail-closed even if a removal call
                        // reports an OS error: no new timer event is allowed
                        // to keep a half gesture alive.
                        // SAFETY: the timer belongs to this host window, which
                        // is live for the whole message.
                        unsafe {
                            let _ = KillTimer(Some(window), PAD_GESTURE_TIMER);
                        }
                    }
                    // Record the desired generation even if USER32 rejects
                    // registration. This avoids retrying on every unrelated
                    // UiState heartbeat; a real config change or restart is
                    // the next bounded retry opportunity.
                    let registration = app.raw_input.set_enabled(enabled);
                    pad_debug(if registration.is_ok() {
                        if enabled {
                            "registration:enabled"
                        } else {
                            "registration:disabled"
                        }
                    } else {
                        "registration:failed"
                    });
                    app.pad_shortcut = state.pad_shortcut;
                }
                // The candidate popup places itself first so the indicator
                // can avoid the rectangle it actually took — including the
                // flip above the composition near the bottom of the screen.
                app.candidates.update(&state);
                if let (Some((mode, theme)), true) =
                    (next, indicator_change_shows(app.shown_indicator, next))
                {
                    let shown =
                        app.indicator
                            .show(mode, theme, &state, app.candidates.popup_rect());
                    if shown {
                        app.shown_indicator = next;
                    } else {
                        // Do not consume the mode change until the first
                        // authoritative caret placement has arrived.  A
                        // later state with the same mode must still be able
                        // to perform the initial paint.
                        app.shown_indicator = None;
                    }
                } else {
                    // No new mode to announce, but the caret and the popup
                    // this update just moved are exactly what a bar already
                    // on screen was placed against. Repositioning keeps that
                    // placement true; it cannot show or re-linger the bar.
                    app.indicator
                        .reposition_if_visible(&state, app.candidates.popup_rect());
                    app.shown_indicator = next;
                }
            }
            LRESULT(0)
        }
        WM_HISTORY_DELETE_FINISHED if !app.is_null() => {
            // SAFETY: as in the WM_UI arm, the main window procedure is the
            // sole mutable owner of `app`.
            let app = unsafe { &mut *app };
            while let Ok(HistoryDeleteCompletion { request, removed }) =
                app.history_delete_completions.try_recv()
            {
                app.candidates.history_delete_finished(request, removed);
            }
            LRESULT(0)
        }
        WM_CANDIDATE_COMMIT_FINISHED if !app.is_null() => {
            // SAFETY: the renderer window procedure is the sole mutable owner.
            let app = unsafe { &mut *app };
            while let Ok(CandidateCommitCompletion { request, queued }) =
                app.candidate_commit_completions.try_recv()
            {
                app.candidates.candidate_commit_finished(request, queued);
            }
            LRESULT(0)
        }
        WM_INPUT if !app.is_null() => {
            // SAFETY: the host procedure owns `app`; packet parsing is
            // bounded in RawInputOwner and returns only a reduced gesture
            // result, never keyboard contents.
            let app = unsafe { &mut *app };
            match app.raw_input.handle_wm_input(l, message_time_ms()) {
                // SAFETY: the timers and the deferred message all target
                // this host window, which is live for the whole message.
                GestureResult::Waiting => unsafe {
                    pad_debug("input:waiting");
                    // One-shot in practice: any next packet resets it, and
                    // WM_TIMER kills it before evaluating the timeout.
                    let _ = SetTimer(Some(window), PAD_GESTURE_TIMER, 501, None);
                },
                // SAFETY: as above.
                GestureResult::Trigger => unsafe {
                    pad_debug("input:trigger");
                    let _ = KillTimer(Some(window), PAD_GESTURE_TIMER);
                    // Defer activation until USER32 has completed WM_INPUT.
                    let _ = PostMessageW(Some(window), WM_PAD_TRIGGER, WPARAM(0), LPARAM(0));
                },
                // SAFETY: as above.
                GestureResult::Terminated(_) => unsafe {
                    pad_debug("input:terminated");
                    let _ = KillTimer(Some(window), PAD_GESTURE_TIMER);
                },
            }
            // WM_INPUT has a documented default cleanup path.  Returning
            // through DefWindowProc after reduction keeps that ownership
            // contract while still preventing packet contents from escaping.
            // SAFETY: the default handler receives the arguments this
            // procedure was given, unmodified.
            unsafe { DefWindowProcW(window, message, w, l) }
        }
        WM_INPUT_DEVICE_CHANGE if !app.is_null() => {
            if w.0 as u32 == windows::Win32::UI::WindowsAndMessaging::GIDC_REMOVAL {
                // SAFETY: the host procedure owns `app` and the null case is
                // excluded by the match guard.
                let app = unsafe { &mut *app };
                let _ = app
                    .raw_input
                    .device_removed(l.0 as usize as u64, message_time_ms());
                // SAFETY: the timer belongs to this host window.
                unsafe {
                    let _ = KillTimer(Some(window), PAD_GESTURE_TIMER);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if !app.is_null() && w.0 == PAD_GESTURE_TIMER => {
            // SAFETY: the host procedure owns `app` and the null case is
            // excluded by the match guard.
            let app = unsafe { &mut *app };
            // SAFETY: the timer belongs to this host window.
            unsafe {
                let _ = KillTimer(Some(window), PAD_GESTURE_TIMER);
            }
            let _ = app.raw_input.timeout(message_time_ms());
            LRESULT(0)
        }
        WM_PAD_TRIGGER if !app.is_null() => {
            // SAFETY: the host procedure owns `app` and the null case is
            // excluded by the match guard.
            let app = unsafe { &mut *app };
            if app.pad.is_none() {
                if let Ok(mut pad) = PadWindow::new(window) {
                    pad_debug("pad:created");
                    pad.set_theme(app.pad_theme);
                    app.pad = Some(pad);
                } else {
                    pad_debug("pad:create-failed");
                }
            }
            if let Some(pad) = app.pad.as_ref() {
                pad_debug("pad:show");
                pad.show_or_focus();
                #[cfg(debug_assertions)]
                pad_debug(if pad.is_visible() {
                    "pad:visible"
                } else {
                    "pad:hidden"
                });
            }
            LRESULT(0)
        }
        // The engine stopped on purpose, or is gone for good. Either way
        // there is nothing left to render.
        //
        // `WM_CLOSE` and `WM_ENDSESSION` join it because logoff and shutdown
        // have to end the same way.
        WM_ENDED | WM_CLOSE | WM_ENDSESSION | WM_DESTROY => {
            if !app.is_null() {
                // SAFETY: the main thread owns both registrations and can
                // unregister before the hidden host is torn down.
                let app = unsafe { &mut *app };
                let _ = app.raw_input.shutdown(message_time_ms());
                let _ = app.raw_input.unregister();
                // SAFETY: the timer belongs to this host window.
                unsafe {
                    let _ = KillTimer(Some(window), PAD_GESTURE_TIMER);
                }
                if let Some(pad) = app.pad.as_ref() {
                    pad.hide();
                }
            }
            // SAFETY: no arguments; posts `WM_QUIT` to this thread.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: the default handler is where every unhandled message
        // must go, with the arguments this procedure was given.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

fn message_time_ms() -> u64 {
    // USER32 stamps input when it enters the queue. Using that stamp prevents
    // a stalled UI thread from compressing two old packets into a false
    // double tap. A 32-bit wrap may reject one gesture, never create one.
    // SAFETY: no arguments, and the result is a plain value read from this
    // thread's message state.
    unsafe { GetMessageTime() as u32 as u64 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_proto::Mode;

    /// The watcher's application messages must stay distinct.
    #[test]
    fn the_application_messages_do_not_collide() {
        let all = [
            WM_UI,
            WM_ENDED,
            WM_HISTORY_DELETE_FINISHED,
            WM_CANDIDATE_COMMIT_FINISHED,
            WM_PAD_TRIGGER,
        ];
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

    /// The indicator appears on a change of mode or theme and only on a
    /// change: a candidate update that repeats the current pair must not
    /// re-trigger it, or it never leaves the screen while the user types.
    #[test]
    fn the_indicator_shows_on_mode_or_theme_changes_and_not_on_repeats() {
        let hira = |theme| Some((Mode::Hiragana, theme));
        let kata = |theme| Some((Mode::Katakana, theme));
        assert!(indicator_change_shows(None, hira(AppearanceTheme::Light)));
        assert!(indicator_change_shows(
            hira(AppearanceTheme::Light),
            kata(AppearanceTheme::Light)
        ));
        assert!(
            indicator_change_shows(hira(AppearanceTheme::Light), hira(AppearanceTheme::Dark)),
            "a system theme switch must repaint an indicator in the stale palette"
        );
        assert!(!indicator_change_shows(
            hira(AppearanceTheme::Light),
            hira(AppearanceTheme::Light)
        ));
        assert!(!indicator_change_shows(hira(AppearanceTheme::Light), None));
        assert!(!indicator_change_shows(None, None));
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
