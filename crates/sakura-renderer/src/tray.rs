//! The notification-area icon: the mode at rest.
//!
//! The floating indicator answers "what did I just switch to?" and then
//! gets out of the way. This answers "what mode am I in?" at any moment,
//! which is the question a user asks after coming back to the machine —
//! and it is the only part of the renderer that is visible when nothing has
//! happened recently.
//!
//! # Surviving an Explorer restart
//!
//! When Explorer restarts — a crash, or a shell upgrade — every icon in the
//! notification area is gone and no `Shell_NotifyIconW` call reports it.
//! The shell broadcasts `TaskbarCreated` to every top-level window instead,
//! and adding the icon again on that message is the only way back. Missing
//! it costs the user their mode indicator for the rest of the session, with
//! no way to get it back short of signing out.

use windows::core::Result;
use windows::Win32::Foundation::{COLORREF, HWND, SIZE};
use windows::Win32::Graphics::Gdi::{GetSysColor, COLOR_WINDOW, COLOR_WINDOWTEXT};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

use sakura_proto::Mode;

use crate::glyph;

/// Identifies this icon within the owning window. Any value; the window has
/// one icon, and the pair (window, id) is what the shell keys on.
const ICON_ID: u32 = 1;

/// The tray icon's edge in pixels.
///
/// `GetSystemMetrics(SM_CXSMICON)` would be the DPI-correct answer for the
/// current monitor, but the notification area lives on one taskbar whose
/// DPI can differ from the window this process happens to own. Drawing at
/// 32 and letting the shell scale down is sharper on every configuration
/// than drawing at 16 and letting it scale up.
const ICON_EDGE: i32 = 32;

/// The notification-area icon, removed when dropped.
#[derive(Debug)]
pub struct Tray {
    owner: HWND,
    /// The icon currently handed to the shell. Kept so it can be destroyed
    /// after the *next* one is installed: destroying it first leaves the
    /// shell holding a dead handle for the moment in between.
    current: Option<HICON>,
    /// What was last shown, so an Explorer restart can restore it rather
    /// than reverting to a placeholder.
    mode: Option<Mode>,
    /// Whether the shell is currently holding an icon for us. `add` and
    /// `modify` are different calls and using the wrong one silently does
    /// nothing.
    added: bool,
}

impl Tray {
    /// Creates a tray attached to `owner`, showing nothing yet.
    ///
    /// The icon is not added until there is a mode to show. A tray icon
    /// that appears at logon saying nothing in particular is clutter; one
    /// that appears when the IME first does something is information.
    pub fn new(owner: HWND) -> Self {
        Tray {
            owner,
            current: None,
            mode: None,
            added: false,
        }
    }

    /// Shows `mode`, adding the icon if this is the first one.
    pub fn set(&mut self, mode: Mode) -> Result<()> {
        self.mode = Some(mode);
        self.install(mode)
    }

    /// Puts the icon back after an Explorer restart.
    ///
    /// Does nothing if there was never an icon to restore, which is the
    /// case if the shell restarted before the user first changed mode.
    pub fn restore(&mut self) -> Result<()> {
        let Some(mode) = self.mode else {
            return Ok(());
        };
        // The shell has forgotten us, whatever we last believed.
        self.added = false;
        self.install(mode)
    }

    fn install(&mut self, mode: Mode) -> Result<()> {
        // The system colours rather than fixed ones, so the icon stays
        // legible when the user switches to a dark theme. Read per install,
        // which is also what makes a theme change take effect at the next
        // mode change instead of at the next sign-in.
        //
        // SAFETY: both are documented colour indices, which is the whole of
        // this call's contract.
        let (ink, paper) = unsafe {
            (
                COLORREF(GetSysColor(COLOR_WINDOWTEXT)),
                COLORREF(GetSysColor(COLOR_WINDOW)),
            )
        };
        let icon = glyph::tray_icon(
            SIZE {
                cx: ICON_EDGE,
                cy: ICON_EDGE,
            },
            glyph::label(mode),
            ink,
            paper,
        )?;

        let mut data = self.data();
        data.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
        data.hIcon = icon;
        write_tip(&mut data.szTip, glyph::description(mode));

        let message = if self.added { NIM_MODIFY } else { NIM_ADD };
        // SAFETY: `data` is fully initialized above, including the size and
        // the owning window, and outlives the call.
        let sent = unsafe { Shell_NotifyIconW(message, &data) }.as_bool();

        if !sent {
            // SAFETY: the shell did not take it, so this process still owns
            // it and nothing else can be holding it.
            unsafe {
                let _ = DestroyIcon(icon);
            }
            // Not an error worth failing startup over: the notification
            // area can refuse an add while the shell is still coming up,
            // and the next mode change tries again.
            return Ok(());
        }

        self.added = true;
        // Only now is the previous icon certainly unused by the shell.
        if let Some(previous) = self.current.replace(icon) {
            // SAFETY: replaced in the shell by the call above, so nothing
            // else refers to it.
            unsafe {
                let _ = DestroyIcon(previous);
            }
        }
        Ok(())
    }

    /// The identifying half of a `NOTIFYICONDATAW`: which icon, on which
    /// window. Every call needs it and none of it ever varies.
    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.owner,
            uID: ICON_ID,
            uCallbackMessage: crate::WM_TRAY,
            ..Default::default()
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.added {
            let data = self.data();
            // SAFETY: `data` names an icon this process added and outlives
            // the call.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
        }
        if let Some(icon) = self.current.take() {
            // SAFETY: removed from the shell above, so nothing refers to it.
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

/// Copies `text` into a fixed tooltip buffer, NUL-terminated.
///
/// Truncated rather than rejected if it does not fit. The tooltips are mode
/// names and fit with room to spare; the bound is here because the field is
/// a fixed array and an unterminated one is read past its end by the shell.
fn write_tip(tip: &mut [u16; 128], text: &str) {
    let mut written = 0;
    for unit in text.encode_utf16() {
        if written + 1 >= tip.len() {
            break;
        }
        tip[written] = unit;
        written += 1;
    }
    tip[written] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tip_is_copied_and_terminated() {
        let mut tip = [0xFFFFu16; 128];
        write_tip(&mut tip, "ひらがな");
        let text: Vec<u16> = "ひらがな".encode_utf16().collect();
        assert_eq!(&tip[..text.len()], &text[..]);
        assert_eq!(tip[text.len()], 0);
    }

    /// The buffer is fixed and the shell reads until a NUL, so a tip that
    /// does not fit must still end.
    #[test]
    fn an_oversized_tip_is_truncated_and_still_terminated() {
        let mut tip = [0xFFFFu16; 128];
        let long = "あ".repeat(500);
        write_tip(&mut tip, &long);
        assert_eq!(*tip.last().unwrap(), 0);
        assert!(
            tip[..tip.len() - 1].iter().all(|&u| u != 0),
            "the terminator should be at the end, not early"
        );
    }

    /// Every mode's tooltip must fit, or the thing the tray exists to
    /// distinguish — the three kana modes, which share one glyph — gets
    /// truncated to the same string.
    #[test]
    fn every_modes_tooltip_fits_whole() {
        for mode in Mode::ALL {
            let text = glyph::description(mode);
            let units = text.encode_utf16().count();
            // `<`, not `<=`: the terminator needs a slot of its own.
            assert!(
                units < 128,
                "{mode:?}'s tooltip needs {units} units and would be truncated"
            );
        }
    }
}
