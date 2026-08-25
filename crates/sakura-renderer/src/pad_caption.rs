//! The pad's own title bar: the icon in it, and the color of it.
//!
//! The design draws a title bar of its own — thirty-eight logical pixels of
//! custom chrome carrying its own minimize, maximize and close. The pad keeps
//! the real one instead. Snap layouts, the maximize flyout, the system menu,
//! every screen reader's idea of what a window is, and high contrast all come
//! out of the real bar for free, and a redrawn one either re-implements them
//! or quietly loses them.
//!
//! What is left is to stop the real bar looking like it belongs to some other
//! program. Two things do that, and neither costs any of the above: a real
//! icon, because a window class with none is given a placeholder; and DWM's
//! caption attributes, which paint the system's own bar in the palette's
//! colors so the window reads as one surface from its top edge down.
//!
//! Under Windows high contrast both are handed straight back to the system.
//! A program tinting its own caption is exactly what that setting exists to
//! stop.

use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use sakura_proto::AppearanceTheme;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_CLOAK,
    DWMWA_COLOR_DEFAULT, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
};
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, LoadImageW, SendMessageW, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON,
    LR_LOADFROMFILE, SM_CXICON, SM_CXSMICON, SYSTEM_METRICS_INDEX, WM_SETICON,
};

use crate::theme::{high_contrast_enabled, palette, resolves_dark, scaled};

/// The two icons the window is showing.
///
/// `WM_SETICON` borrows rather than takes ownership, so these have to outlive
/// the window showing them — which is why they are owned by the pad's state
/// and not by the function that loaded them.
#[derive(Debug)]
pub(crate) struct CaptionIcons {
    big: HICON,
    small: HICON,
}

impl Drop for CaptionIcons {
    fn drop(&mut self) {
        for icon in [self.big, self.small] {
            if !icon.is_invalid() {
                // SAFETY: both came from `LoadImageW` below, and by the time
                // this runs the window has either been destroyed or been
                // handed replacements.
                unsafe {
                    let _ = DestroyIcon(icon);
                }
            }
        }
    }
}

/// Gives `window` the product's icon at the sizes this display asks for.
///
/// The caller keeps the result for as long as the window lives. Dropping it
/// while the window is still showing the icons would leave the caption
/// drawing from freed handles.
pub(crate) fn icons(window: HWND, dpi: u32) -> Option<CaptionIcons> {
    let path = asset_path()?;
    let big = load(&path, metric(SM_CXICON, dpi, 32))?;
    let Some(small) = load(&path, metric(SM_CXSMICON, dpi, 16)) else {
        // SAFETY: `big` was loaded a moment ago and was never handed to the
        // window, because a window with one of the two is worse than a window
        // with neither: the missing one falls back to the placeholder.
        unsafe {
            let _ = DestroyIcon(big);
        }
        return None;
    };
    // SAFETY: the window belongs to this thread and `WM_SETICON` only stores
    // the handles; `CaptionIcons` keeps ownership.
    unsafe {
        let _ = SendMessageW(
            window,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(big.0 as isize)),
        );
        let _ = SendMessageW(
            window,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(small.0 as isize)),
        );
    }
    Some(CaptionIcons { big, small })
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

/// The size Windows wants an icon to be on this display, or the scaled
/// 96-DPI size where the metric is unavailable.
fn metric(index: SYSTEM_METRICS_INDEX, dpi: u32, fallback_96: i32) -> i32 {
    // SAFETY: reads one documented system metric for a DPI value.
    let reported = unsafe { GetSystemMetricsForDpi(index, dpi) };
    if reported > 0 {
        reported
    } else {
        scaled(fallback_96, dpi).max(1)
    }
}

/// Where the shipped icon is: beside the executable in an installed layout,
/// and in the repository's assets when running from a build tree.
fn asset_path() -> Option<PathBuf> {
    let mut candidates = Vec::with_capacity(2);
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            candidates.push(parent.join("sakura-input.ico"));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
            .join("sakura-input-icon")
            .join("sakura-input.ico"),
    );
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn load(path: &Path, size: i32) -> Option<HICON> {
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: the path is NUL-terminated and stays alive for the call;
    // `LoadImageW` copies the resource into a process-owned handle.
    let handle = unsafe {
        LoadImageW(
            None,
            PCWSTR(path.as_ptr()),
            IMAGE_ICON,
            size,
            size,
            LR_LOADFROMFILE,
        )
    }
    .ok()?;
    Some(HICON(handle.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The icon has to be findable from wherever the renderer runs, or the
    /// caption falls back to the placeholder that started all this.
    #[test]
    fn the_product_icon_is_on_disk() {
        let path = asset_path().expect("the shipped icon was not found from this build");
        assert!(path.is_file(), "{} is not a file", path.display());
    }

    /// A metric that a display cannot answer still has to produce a size an
    /// icon can be loaded at.
    #[test]
    fn an_unavailable_metric_still_yields_a_scaled_size() {
        for dpi in [96, 120, 144, 192, 240] {
            // 0 is not a metric index Windows answers, so this exercises the
            // fallback without depending on the session having a display.
            assert_eq!(
                metric(SYSTEM_METRICS_INDEX(i32::MAX), dpi, 16),
                scaled(16, dpi).max(1)
            );
            assert!(metric(SM_CXSMICON, dpi, 16) > 0);
        }
    }
}
