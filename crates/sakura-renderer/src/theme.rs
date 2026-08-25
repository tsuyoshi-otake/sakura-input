//! The renderer's shared appearance vocabulary.
//!
//! Every Sakura-owned surface — the candidate popup and Sakura Pad — resolves
//! its colors here, so the two cannot drift into two products that merely
//! resemble each other. The values are the settled restrained warm neutrals
//! with one muted sakura accent; a surface that wants a new color is asking
//! the wrong question.
//!
//! Windows high contrast deliberately precedes an explicit light/dark
//! preference: accessibility system roles stay authoritative everywhere.
//!
//! The GDI helpers live here for the same reason. Both surfaces draw with
//! `CreateFontW`/`DrawTextW`/solid brushes, and a second private copy of
//! "fill this rectangle" is how two windows start rounding DPI differently.

use std::ffi::c_void;
use std::mem::size_of;

use sakura_proto::AppearanceTheme;
use windows::core::w;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetSysColor, SelectObject,
    SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_3DSHADOW, COLOR_GRAYTEXT,
    COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, DEFAULT_CHARSET,
    DEFAULT_PITCH, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    FF_DONTCARE, FW_NORMAL, HBRUSH, HDC, HFONT, HGDIOBJ, OUT_TT_PRECIS, SYS_COLOR_INDEX,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

/// The 96-DPI measurements every Sakura surface shares.
///
/// These are the ones that make two windows look like one product: how far
/// text sits from an edge, how far two things sit from each other, how wide
/// the selection rail is, and the two type sizes. Surface-specific
/// measurements — a candidate row's height, a memo row's height — stay with
/// the surface that owns them.
pub(crate) const PADDING_96: i32 = 8;
pub(crate) const GAP_96: i32 = 8;
pub(crate) const RAIL_WIDTH_96: i32 = 2;
pub(crate) const BODY_FONT_96: i32 = 16;
pub(crate) const SUPPORT_FONT_96: i32 = 13;

/// The complete set of roles a Sakura surface may paint with.
///
/// It is intentionally small. `rail` is the only saturated color in the
/// product and marks exactly one thing at a time — the current selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    pub(crate) surface: COLORREF,
    pub(crate) ink: COLORREF,
    pub(crate) annotation: COLORREF,
    pub(crate) selected: COLORREF,
    pub(crate) selected_ink: COLORREF,
    pub(crate) rail: COLORREF,
    pub(crate) border: COLORREF,
    pub(crate) action: COLORREF,
    /// The one control a surface may offer that cannot be undone by pressing
    /// it again. Muted enough to sit in the same warm register as the rest,
    /// distinct enough that it is never pressed by mistake.
    pub(crate) danger: COLORREF,
    /// What is written on, as against what the program is made of. A surface
    /// that holds the user's own words separates itself from the chrome
    /// around it by a shade — lighter than `surface` in light, darker in
    /// dark — so the writing area reads as paper rather than as another
    /// panel.
    pub(crate) paper: COLORREF,
    /// The ruled squares behind `paper`, already flattened against it.
    ///
    /// `None` under Windows high contrast: a decorative texture laid over a
    /// system background is exactly what that setting asks a program not to
    /// draw.
    pub(crate) grid: Option<COLORREF>,
}

pub(crate) fn palette(theme: AppearanceTheme) -> Palette {
    if high_contrast_enabled() {
        high_contrast_palette()
    } else {
        resolve_palette(theme, false, windows_apps_use_light_theme())
    }
}

/// Resolves the palette from inputs that can be tested without reading global
/// Windows state. High contrast deliberately precedes an explicit preference:
/// accessibility system roles must remain authoritative for every Sakura-owned
/// surface.
pub(crate) fn resolve_palette(
    theme: AppearanceTheme,
    high_contrast: bool,
    apps_use_light_theme: bool,
) -> Palette {
    if high_contrast {
        high_contrast_palette()
    } else {
        match theme {
            AppearanceTheme::Auto if apps_use_light_theme => light_palette(),
            AppearanceTheme::Auto | AppearanceTheme::Dark => dark_palette(),
            AppearanceTheme::Light => light_palette(),
        }
    }
}

/// Whether `theme` resolves to the dark palette on this machine.
///
/// Asked of the inputs rather than of the resulting colors: a palette is a set
/// of colors, and deciding from them whether they are "dark" is a guess that
/// can disagree with the choice that produced them.
pub(crate) fn resolves_dark(theme: AppearanceTheme) -> bool {
    resolve_dark(theme, windows_apps_use_light_theme())
}

/// The testable half of `resolves_dark`, taking the system state as an input.
pub(crate) fn resolve_dark(theme: AppearanceTheme, apps_use_light_theme: bool) -> bool {
    match theme {
        AppearanceTheme::Auto => !apps_use_light_theme,
        AppearanceTheme::Dark => true,
        AppearanceTheme::Light => false,
    }
}

pub(crate) fn windows_apps_use_light_theme() -> bool {
    let mut value = 1_u32;
    let mut bytes = size_of::<u32>() as u32;
    // SAFETY: the registry names are static NUL-terminated strings, and the
    // value buffer has the exact REG_DWORD size supplied through `bytes`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast::<c_void>()),
            Some(&mut bytes),
        )
    };
    // Missing, malformed, or unreadable values must preserve a legible
    // surface. Windows applications conventionally default to light.
    if status == windows::Win32::Foundation::ERROR_SUCCESS && bytes == size_of::<u32>() as u32 {
        value != 0
    } else {
        true
    }
}

pub(crate) fn high_contrast_palette() -> Palette {
    Palette {
        surface: system_color(COLOR_WINDOW),
        ink: system_color(COLOR_WINDOWTEXT),
        annotation: system_color(COLOR_GRAYTEXT),
        selected: system_color(COLOR_HIGHLIGHT),
        selected_ink: system_color(COLOR_HIGHLIGHTTEXT),
        rail: system_color(COLOR_HIGHLIGHT),
        border: system_color(COLOR_3DSHADOW),
        action: system_color(COLOR_WINDOWTEXT),
        // High contrast themes carry no destructive role, and inventing one
        // would be the only unreadable color on the surface.
        danger: system_color(COLOR_WINDOWTEXT),
        paper: system_color(COLOR_WINDOW),
        grid: None,
    }
}

pub(crate) fn high_contrast_enabled() -> bool {
    let mut high_contrast = HIGHCONTRASTW {
        cbSize: core::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: Windows fills the supplied initialized HIGHCONTRASTW structure.
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            high_contrast.cbSize,
            Some((&mut high_contrast as *mut HIGHCONTRASTW).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && (high_contrast.dwFlags.0 & HCF_HIGHCONTRASTON.0) != 0
    }
}

pub(crate) fn light_palette() -> Palette {
    Palette {
        surface: rgb(0xF7, 0xF6, 0xF4),
        ink: rgb(0x2F, 0x2F, 0x2F),
        annotation: rgb(0x70, 0x70, 0x70),
        selected: rgb(0xE8, 0xE5, 0xE2),
        selected_ink: rgb(0x2F, 0x2F, 0x2F),
        rail: rgb(0xB2, 0x8D, 0x96),
        border: rgb(0xBD, 0xB9, 0xB5),
        action: rgb(0x89, 0x72, 0x77),
        danger: rgb(0xA5, 0x4B, 0x42),
        paper: rgb(0xFF, 0xFD, 0xFB),
        // `selected` at just over a quarter opacity over `paper`: present as
        // a texture, gone as a pattern the eye has to read past.
        grid: Some(rgb(0xF9, 0xF6, 0xF4)),
    }
}

pub(crate) fn dark_palette() -> Palette {
    Palette {
        surface: rgb(0x35, 0x35, 0x35),
        ink: rgb(0xF2, 0xF0, 0xEE),
        annotation: rgb(0xC9, 0xC4, 0xC0),
        selected: rgb(0x25, 0x25, 0x25),
        selected_ink: rgb(0xFF, 0xFD, 0xFB),
        rail: rgb(0xD7, 0xA6, 0xB1),
        border: rgb(0x72, 0x6E, 0x6B),
        action: rgb(0xE7, 0xC1, 0xC8),
        danger: rgb(0xE8, 0x9C, 0x93),
        paper: rgb(0x25, 0x25, 0x25),
        grid: Some(rgb(0x2F, 0x2D, 0x2D)),
    }
}

pub(crate) const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(u32::from_le_bytes([red, green, blue, 0]))
}

pub(crate) fn system_color(role: SYS_COLOR_INDEX) -> COLORREF {
    // SAFETY: callers pass documented system color roles.
    COLORREF(unsafe { GetSysColor(role) })
}

/// Converts a 96-DPI design token to physical pixels, rounding to nearest so a
/// one-pixel rule does not vanish at 150%.
pub(crate) fn scaled(value: i32, dpi: u32) -> i32 {
    value.saturating_mul(dpi as i32).saturating_add(48) / 96
}

pub(crate) fn fill_color(dc: HDC, rect: &RECT, color: COLORREF) {
    // SAFETY: the brush is owned by this call, remains live for `FillRect`, and
    // is deleted on the same path before returning.
    unsafe {
        let brush: HBRUSH = CreateSolidBrush(color);
        if !brush.is_invalid() {
            let _ = FillRect(dc, rect, brush);
            let _ = DeleteObject(brush.into());
        }
    }
}

pub(crate) fn select_font(dc: HDC, font: HFONT) -> Option<HGDIOBJ> {
    if font.is_invalid() {
        return None;
    }
    // SAFETY: the font is a live object owned by this paint frame. A NULL or
    // HGDI_ERROR result means the DC keeps its previously selected valid font.
    let previous = unsafe { SelectObject(dc, font.into()) };
    (!previous.0.is_null() && (previous.0 as isize) != -1).then_some(previous)
}

pub(crate) fn text(
    dc: HDC,
    value: &str,
    mut rect: RECT,
    color: COLORREF,
    alignment: DRAW_TEXT_FORMAT,
) {
    let Some(mut wide) = drawable_utf16(value) else {
        return;
    };
    // SAFETY: the non-empty UTF-16 buffer and rectangle outlive the call.
    unsafe {
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            alignment | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    }
}

/// How wide `value` draws in the DC's current font.
///
/// For the one place a surface centers a drawn shape and a word together and
/// so has to know where the word starts.
pub(crate) fn text_width(dc: HDC, value: &str) -> i32 {
    let Some(mut wide) = drawable_utf16(value) else {
        return 0;
    };
    let mut rect = RECT::default();
    // SAFETY: the non-empty UTF-16 buffer and rectangle outlive the call, and
    // `DT_CALCRECT` measures into `rect` instead of drawing.
    unsafe {
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
        );
    }
    rect.right.saturating_sub(rect.left)
}

/// `DrawTextW` may still inspect its buffer pointer when its length is zero.
/// Rust's empty `Vec` uses a dangling non-null sentinel, so empty strings never
/// cross this FFI boundary.
pub(crate) fn drawable_utf16(value: &str) -> Option<Vec<u16>> {
    (!value.is_empty()).then(|| value.encode_utf16().collect())
}

pub(crate) fn font(height: i32) -> HFONT {
    font_weighted(height, FW_NORMAL.0 as i32)
}

pub(crate) fn font_weighted(height: i32, weight: i32) -> HFONT {
    // SAFETY: all arguments are plain values and the face name is static.
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0).into(),
            w!("Yu Gothic UI"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_palettes_are_exact_and_distinct() {
        assert_eq!(light_palette().surface, rgb(0xF7, 0xF6, 0xF4));
        assert_eq!(light_palette().ink, rgb(0x2F, 0x2F, 0x2F));
        assert_eq!(light_palette().annotation, rgb(0x70, 0x70, 0x70));
        assert_eq!(light_palette().selected, rgb(0xE8, 0xE5, 0xE2));
        assert_eq!(light_palette().selected_ink, rgb(0x2F, 0x2F, 0x2F));
        assert_eq!(light_palette().rail, rgb(0xB2, 0x8D, 0x96));
        assert_eq!(light_palette().border, rgb(0xBD, 0xB9, 0xB5));
        assert_eq!(light_palette().action, rgb(0x89, 0x72, 0x77));
        assert_eq!(light_palette().paper, rgb(0xFF, 0xFD, 0xFB));
        assert_eq!(light_palette().grid, Some(rgb(0xF9, 0xF6, 0xF4)));
        assert_eq!(dark_palette().surface, rgb(0x35, 0x35, 0x35));
        assert_eq!(dark_palette().ink, rgb(0xF2, 0xF0, 0xEE));
        assert_eq!(dark_palette().annotation, rgb(0xC9, 0xC4, 0xC0));
        assert_eq!(dark_palette().selected, rgb(0x25, 0x25, 0x25));
        assert_eq!(dark_palette().rail, rgb(0xD7, 0xA6, 0xB1));
        assert_eq!(dark_palette().border, rgb(0x72, 0x6E, 0x6B));
        assert_eq!(dark_palette().action, rgb(0xE7, 0xC1, 0xC8));
        assert_eq!(dark_palette().paper, rgb(0x25, 0x25, 0x25));
        assert_eq!(dark_palette().grid, Some(rgb(0x2F, 0x2D, 0x2D)));
        assert!(
            high_contrast_palette().grid.is_none(),
            "high contrast asks for system roles, not texture"
        );
        assert_ne!(light_palette(), dark_palette());
    }

    #[test]
    fn explicit_preferences_select_their_palette_regardless_of_the_system_theme() {
        for apps_use_light_theme in [false, true] {
            assert_eq!(
                resolve_palette(AppearanceTheme::Light, false, apps_use_light_theme),
                light_palette()
            );
            assert_eq!(
                resolve_palette(AppearanceTheme::Dark, false, apps_use_light_theme),
                dark_palette()
            );
        }
    }

    #[test]
    fn auto_follows_the_system_theme_and_high_contrast_outranks_every_preference() {
        assert_eq!(
            resolve_palette(AppearanceTheme::Auto, false, true),
            light_palette()
        );
        assert_eq!(
            resolve_palette(AppearanceTheme::Auto, false, false),
            dark_palette()
        );
        for theme in [
            AppearanceTheme::Auto,
            AppearanceTheme::Light,
            AppearanceTheme::Dark,
        ] {
            for apps_use_light_theme in [false, true] {
                assert_eq!(
                    resolve_palette(theme, true, apps_use_light_theme),
                    high_contrast_palette()
                );
            }
        }
    }

    #[test]
    fn scaling_rounds_to_nearest_so_hairlines_survive_fractional_dpi() {
        assert_eq!(scaled(1, 96), 1);
        assert_eq!(scaled(1, 144), 2);
        assert_eq!(scaled(1, 192), 2);
        assert_eq!(scaled(28, 96), 28);
        assert_eq!(scaled(28, 144), 42);
        assert_eq!(scaled(28, 192), 56);
    }

    /// Anything asking "is this window dark?" has to get the same answer as
    /// the palette it will be painted with, or the title bar ends up light
    /// over a dark window.
    #[test]
    fn dark_resolution_agrees_with_the_palette_it_picks() {
        for theme in [
            AppearanceTheme::Auto,
            AppearanceTheme::Light,
            AppearanceTheme::Dark,
        ] {
            for apps_use_light_theme in [false, true] {
                assert_eq!(
                    resolve_dark(theme, apps_use_light_theme),
                    resolve_palette(theme, false, apps_use_light_theme) == dark_palette(),
                    "{theme:?} with apps_use_light_theme={apps_use_light_theme}"
                );
            }
        }
    }
}
