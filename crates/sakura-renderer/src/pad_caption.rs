//! The pad's own title bar: what is in it, and the color of it.
//!
//! The design draws a title bar of its own — thirty-eight logical pixels of
//! custom chrome carrying its own minimize, maximize and close. The pad keeps
//! the real one instead. Snap layouts, the maximize flyout, the system menu,
//! every screen reader's idea of what a window is, and high contrast all come
//! out of the real bar for free, and a redrawn one either re-implements them
//! or quietly loses them.
//!
//! What is left is to stop the real bar looking like it belongs to some other
//! program. Two things do that, and neither costs any of the above: no icon
//! in the corner, because the pad is summoned by a gesture rather than picked
//! out of a row of windows and has nothing to identify itself against; and
//! DWM's caption attributes, which paint the system's own bar in the
//! palette's colors so the window reads as one surface from its top edge
//! down.
//!
//! Under Windows high contrast the color is handed straight back to the
//! system. A program tinting its own caption is exactly what that setting
//! exists to stop.

use std::mem::size_of;

use sakura_proto::AppearanceTheme;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_CLOAK,
    DWMWA_COLOR_DEFAULT, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SendMessageW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, ICON_BIG,
    ICON_SMALL, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_SETICON,
    WS_EX_DLGMODALFRAME,
};

use crate::theme::{high_contrast_enabled, palette, resolves_dark};

/// Takes the icon out of the left end of the title bar.
///
/// A window class with no icon of its own is drawn with Windows' placeholder,
/// which is the one part of the window that says it belongs to some other
/// program, and giving the window the product's icon only trades a wrong
/// picture for a redundant one. `WS_EX_DLGMODALFRAME` is what removes the
/// corner altogether; the two null icons are what stop Windows filling it
/// back in. The system menu is untouched — it is still on Alt+Space and on a
/// right-click of the bar.
pub(crate) fn hide_icon(window: HWND) {
    // SAFETY: the window belongs to this thread, and a null handle is the
    // documented way to say the window has no icon of its own.
    unsafe {
        for which in [ICON_BIG, ICON_SMALL] {
            let _ = SendMessageW(
                window,
                WM_SETICON,
                Some(WPARAM(which as usize)),
                Some(LPARAM(0)),
            );
        }
    }
    // SAFETY: reads and writes the window's own extended style, then asks for
    // the frame to be recalculated with it. The window is not on screen yet
    // when this runs, so nothing flickers.
    unsafe {
        let style = GetWindowLongPtrW(window, GWL_EXSTYLE);
        SetWindowLongPtrW(window, GWL_EXSTYLE, style | WS_EX_DLGMODALFRAME.0 as isize);
        let _ = SetWindowPos(
            window,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// Paints the system title bar in the palette `theme` resolves to.
///
/// Every attribute here is best effort: Windows 10 does not know the three
/// color ones and rejects them, which leaves the system-themed bar — the same
/// outcome as never having asked.
pub(crate) fn dress(window: HWND, theme: AppearanceTheme) {
    let colors = (!high_contrast_enabled()).then(|| palette(theme));
    let dark = colors.is_some() && resolves_dark(theme);
    let value = i32::from(dark);
    // SAFETY: the window is live and DWM copies the four bytes during this
    // synchronous call.
    unsafe {
        let _ = DwmSetWindowAttribute(
            window,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&value as *const i32).cast(),
            size_of::<i32>() as u32,
        );
    }
    // `DWMWA_COLOR_DEFAULT` gives the bar back to Windows, which is what high
    // contrast asks for: the system's caption roles rather than ours.
    let (caption, text, border) = match colors {
        Some(colors) => (colors.surface, colors.ink, colors.border),
        None => (
            COLORREF(DWMWA_COLOR_DEFAULT),
            COLORREF(DWMWA_COLOR_DEFAULT),
            COLORREF(DWMWA_COLOR_DEFAULT),
        ),
    };
    for (attribute, color) in [
        (DWMWA_CAPTION_COLOR, caption),
        (DWMWA_TEXT_COLOR, text),
        (DWMWA_BORDER_COLOR, border),
    ] {
        set_color(window, attribute, color);
    }
}

/// Hides `window` from the compositor without hiding it from Windows.
///
/// Between `ShowWindow` and the first `WM_PAINT` a window's surface holds
/// whatever the compositor last had for it, and a window of child controls
/// paints in pieces as each child gets its turn. Cloaked, none of that
/// reaches the screen: the window is shown, painted to completion, and only
/// then uncloaked, so its first visible frame is a finished one.
///
/// A failure leaves the window uncloaked, which is exactly the behaviour
/// this replaces.
pub(crate) fn cloak(window: HWND, hidden: bool) {
    let value = i32::from(hidden);
    // SAFETY: the window is live and the attribute takes the four bytes of a
    // BOOL, which is what is being handed to it.
    unsafe {
        let _ = DwmSetWindowAttribute(
            window,
            DWMWA_CLOAK,
            (&value as *const i32).cast(),
            size_of::<i32>() as u32,
        );
    }
}

fn set_color(window: HWND, attribute: DWMWINDOWATTRIBUTE, color: COLORREF) {
    // SAFETY: as above; a `COLORREF` is the four bytes this attribute takes.
    unsafe {
        let _ = DwmSetWindowAttribute(
            window,
            attribute,
            (&color as *const COLORREF).cast(),
            size_of::<COLORREF>() as u32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The corner is emptied by an extended style, so the style has to be the
    /// one Windows reads for it.
    #[test]
    fn the_frame_style_that_empties_the_corner_is_the_documented_one() {
        assert_eq!(WS_EX_DLGMODALFRAME.0, 0x0000_0001);
    }
}
