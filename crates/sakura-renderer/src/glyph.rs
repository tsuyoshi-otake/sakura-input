//! Drawing あ / A, and turning it into a tray icon.
//!
//! There is no icon resource to load. The indicator has to show the mode as
//! a *character*, and there is one per mode (DESIGN 3's mode set), so
//! shipped `.ico` files would be a bitmap apiece saying something the font
//! already knows how to draw. Drawing it means the glyph is always the right
//! shape at whatever size Windows asks for, including the 24×24 tray icon on
//! a 150% display, which a 16×16 resource would not be.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, DrawTextW, SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FW_SEMIBOLD, HDC, HFONT, OUT_TT_PRECIS, TRANSPARENT,
};

use sakura_proto::Mode;

/// What the indicator shows for a mode.
///
/// Matches what Windows' own IME indicator shows, because a user switching
/// to this IME should not have to learn a new alphabet of symbols: あ for
/// the kana modes, A for the alphanumeric ones. Katakana and half-width
/// katakana share あ with hiragana here — the focused TSF input-mode item
/// carries the precise full mode name. `Direct`
/// joins the A side for the same reason Windows shows A for it: nothing the
/// user types is being transformed.
pub fn label(mode: Mode) -> &'static str {
    match mode {
        Mode::Hiragana | Mode::Katakana | Mode::HalfKatakana => "あ",
        Mode::Direct | Mode::HalfAlnum | Mode::FullAlnum => "A",
    }
}

/// A mode as a small integer, for the places Win32 only carries one.
///
/// Two of them: the window word the indicator paints from, and the
/// `WPARAM` the watcher thread posts to the UI thread. Both are `isize`
/// with no type information, so the mapping lives here once rather than
/// being written out twice and drifting.
///
/// Every mode maps to a non-zero code, which is what lets zero mean "no
/// mode": it is what an unset `GWLP_USERDATA` reads as, and what the
/// watcher posts when the engine reports `mode: None`. That is the only
/// reason for the `+ 1`: the protocol's own discriminants start at zero,
/// which is the one value this encoding has to keep free.
pub fn code(mode: Mode) -> isize {
    mode as isize + 1
}

/// The inverse of [`code`], rejecting anything it did not produce.
///
/// Searches [`Mode::ALL`] rather than matching on the codes, so a mode added
/// to the protocol decodes here the day it is added. A hand-written match
/// would keep compiling and drop the new mode on the floor, and the symptom
/// — one mode whose indicator never appears — is a long way from the cause.
pub fn from_code(stored: isize) -> Option<Mode> {
    Mode::ALL.into_iter().find(|mode| code(*mode) == stored)
}

/// Draws `text` centred in `rect` on `dc`, in a font sized to the rect.
///
/// The font is created and destroyed per call. This runs on a mode change
/// and on a repaint, never in a loop, and caching an `HFONT` across DPI
/// changes is how an indicator ends up crisp on one monitor and blurry on
/// the next.
pub fn draw_centered(dc: HDC, rect: &RECT, text: &str, color: COLORREF) {
    let height = rect.bottom - rect.top;
    let font = font_of_height((height * 2) / 3);
    // SAFETY: `dc` is a live device context and `font` a live font; the
    // previous object is restored below before the font is deleted, which is
    // what makes the delete legal.
    let previous = unsafe { SelectObject(dc, font.into()) };

    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut area = *rect;
    // SAFETY: `wide` is a live buffer whose length is passed to `DrawTextW`
    // through the slice, and `area` outlives the call.
    unsafe {
        let _ = SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            &mut wide,
            &mut area,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(dc, previous);
        let _ = DeleteObject(font.into());
    }
}

/// A UI font at the given pixel height.
///
/// Face name empty so Windows picks the system UI font, which is the one
/// with the Japanese coverage on a Japanese system and the right fallback
/// chain everywhere else. Naming a face here is how you get tofu on a
/// machine that does not have it.
fn font_of_height(height: i32) -> HFONT {
    // SAFETY: every argument is a plain value; the empty name is a valid
    // NUL-terminated wide string.
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0).into(),
            PCWSTR::null(),
        )
    }
}

/// The window a mode indicator should appear beside: the foreground one.
pub fn foreground() -> Option<HWND> {
    // SAFETY: no arguments; a null result means no foreground window.
    let hwnd = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    (!hwnd.is_invalid()).then_some(hwnd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_a_glyph() {
        for mode in Mode::ALL {
            assert!(!label(mode).is_empty());
        }
    }

    /// Every mode has to survive the trip through an `isize`, and none may
    /// encode as zero — the value an unset window word reads as, and the
    /// one the watcher posts to mean "no mode". A mode colliding with it
    /// would paint a glyph on a window that has never been shown.
    #[test]
    fn modes_survive_the_trip_through_an_isize() {
        for mode in Mode::ALL {
            let stored = code(mode);
            assert_ne!(stored, 0, "{mode:?} collides with the no-mode code");
            assert_eq!(from_code(stored), Some(mode));
        }
    }

    #[test]
    fn no_mode_and_nonsense_decode_to_nothing() {
        assert_eq!(from_code(0), None);
        assert_eq!(from_code(-1), None);
        assert_eq!(from_code(9_999), None);
    }

    /// `Direct` is a mode like any other and has to be drawable. It reached
    /// this module only because the compiler insisted, and a mode the
    /// indicator cannot show is a mode the user switches into and gets no
    /// feedback from.
    #[test]
    fn direct_input_is_a_mode_like_any_other() {
        assert_eq!(label(Mode::Direct), "A");
        assert_eq!(from_code(code(Mode::Direct)), Some(Mode::Direct));
    }

    /// The alphanumeric modes must not show あ, and the kana modes must not
    /// show A. Getting this backwards would be a bug the user reads as the
    /// IME being in the wrong mode.
    #[test]
    fn the_glyph_matches_the_family_of_the_mode() {
        assert_eq!(label(Mode::Hiragana), "あ");
        assert_eq!(label(Mode::Katakana), "あ");
        assert_eq!(label(Mode::HalfKatakana), "あ");
        assert_eq!(label(Mode::HalfAlnum), "A");
        assert_eq!(label(Mode::FullAlnum), "A");
        assert_eq!(label(Mode::Direct), "A");
    }
}
