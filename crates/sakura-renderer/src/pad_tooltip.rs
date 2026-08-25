//! Words for the pad's wordless controls.
//!
//! Most of the bar is drawn icons. An icon is a guess the reader makes, and
//! the owner reported making the wrong one: the control that copies a memo as
//! Markdown was read as something else entirely. The drawing was changed for
//! that, but a picture can only narrow the guess — the sentence has to be
//! available somewhere, and hovering is where Windows users look for it.
//!
//! These are not the accessible names. Every button already carries real
//! window text, which is what UI Automation reads out and what a screen
//! reader announces. A tooltip is the longer form for the pointer:
//! `このメモを削除` rather than `削除`. Both exist because they answer to
//! different readers.
//!
//! `TTF_SUBCLASS` is what makes this a few calls instead of a message pump:
//! the tooltip control subclasses each button and takes the mouse messages it
//! needs, so nothing in the pad's own procedure has to relay them.
//!
//! Which control says what is not decided here — that belongs beside the
//! control ids, in [`crate::pad`]. This module knows only how to carry a
//! string.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::core::PWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, TOOLTIPS_CLASS, TTF_IDISHWND,
    TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETMAXTIPWIDTH, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SendMessageW, SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, WINDOW_EX_STYLE, WINDOW_STYLE, WS_POPUP,
};

/// How wide a tip may get before it wraps, in pixels.
///
/// Sending this at all is what turns wrapping on; the value only decides
/// where. These are short phrases, so the number is a guard against one of
/// them becoming a single line across the monitor rather than a layout.
const MAX_TIP_WIDTH: i32 = 320;

/// The pad's one tooltip control and the strings it was given.
///
/// The strings are owned here because `TTTOOLINFOW` keeps the pointer rather
/// than the text: the control reads it again on every hover, so a temporary
/// would be read after it was gone.
#[derive(Debug)]
pub(crate) struct Tooltips {
    window: HWND,
    texts: Vec<Vec<u16>>,
}

impl Tooltips {
    /// Makes the control that will carry every tip on `owner`.
    ///
    /// `None` if the common control cannot be created, which costs the pad its
    /// hover text and nothing else.
    pub(crate) fn new(owner: HWND) -> Option<Self> {
        let request = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        // SAFETY: the request is fully initialized and lives across the call.
        unsafe {
            let _ = InitCommonControlsEx(&request);
        }
        // SAFETY: the class is registered by the call above, and the owner is
        // the live pad window.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                TOOLTIPS_CLASS,
                None,
                WINDOW_STYLE(WS_POPUP.0 | TTS_ALWAYSTIP | TTS_NOPREFIX),
                0,
                0,
                0,
                0,
                Some(owner),
                None,
                None,
                None,
            )
            .ok()?
        };
        // SAFETY: the window was created above and is live for this object.
        unsafe {
            let _ = SetWindowPos(
                window,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            SendMessageW(
                window,
                TTM_SETMAXTIPWIDTH,
                Some(WPARAM(0)),
                Some(LPARAM(MAX_TIP_WIDTH as isize)),
            );
        }
        Some(Self {
            window,
            texts: Vec::new(),
        })
    }

    /// Gives `control` the hover text `text`.
    ///
    /// `owner` has to be the control's parent: the tool is registered by
    /// window handle, and the control asks the parent where that handle is.
    pub(crate) fn attach(&mut self, owner: HWND, control: HWND, text: &str) {
        if self.window.is_invalid() || control.is_invalid() {
            return;
        }
        let mut wide: Vec<u16> = OsStr::new(text).encode_wide().collect();
        wide.push(0);
        self.texts.push(wide);
        let stored = match self.texts.last_mut() {
            Some(text) => text.as_mut_ptr(),
            None => return,
        };
        let info = TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            // By handle rather than by rectangle: the pad moves its controls
            // on every resize and every DPI change, and a rectangle would
            // have to be re-registered each time one of those happened.
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd: owner,
            uId: control.0 as usize,
            lpszText: PWSTR(stored),
            ..Default::default()
        };
        // SAFETY: `info` outlives the call, and the text it points at is owned
        // by `self.texts` for as long as this object — which is as long as the
        // window the tips are attached to.
        unsafe {
            SendMessageW(
                self.window,
                TTM_ADDTOOLW,
                Some(WPARAM(0)),
                Some(LPARAM(&info as *const TTTOOLINFOW as isize)),
            );
        }
    }
}

impl Drop for Tooltips {
    fn drop(&mut self) {
        if self.window.is_invalid() {
            return;
        }
        // SAFETY: the control belongs to this object and is destroyed on the
        // thread that made it. Destroying the pad window first already takes
        // this one with it, in which case the handle is stale and the call
        // fails — which is why the result is discarded rather than checked.
        unsafe {
            let _ = DestroyWindow(self.window);
        }
    }
}
