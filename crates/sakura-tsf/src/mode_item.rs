//! The focused TSF input-mode item and its original Sakura menu model.
//!
//! Windows 11 exposes a third-party IME's current mode through the one
//! `GUID_LBI_INPUTMODE` language-bar item. This module owns the small,
//! re-entrancy-safe state behind that item: visibility, the `あ`/`A` glyph,
//! the menu's enabled state, and the language-bar update sink. It does not
//! talk to the engine or TSF document contexts; `text_service` owns those
//! boundaries and passes this module an already-resolved snapshot.

use std::cell::{Cell, RefCell};
use std::ffi::{c_void, OsString};
use std::mem::size_of;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use sakura_proto::Mode;
use windows::Win32::Foundation::{COLORREF, ERROR_SUCCESS, E_FAIL, HMODULE, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GetDC,
    PatBlt, ReleaseDC, SelectObject, SetBkMode, SetTextColor, BLACKNESS, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_SEMIBOLD,
    HBITMAP, HDC, HFONT, NONANTIALIASED_QUALITY, OUT_TT_PRECIS, TRANSPARENT, WHITENESS,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::System::Ole::{
    CONNECT_E_ADVISELIMIT, CONNECT_E_CANNOTCONNECT, CONNECT_E_NOCONNECTION,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::TextServices::{
    ITfLangBarItemSink, ITfMenu, TF_LBI_ICON, TF_LBI_STATUS, TF_LBI_STATUS_HIDDEN, TF_LBI_TEXT,
    TF_LBI_TOOLTIP, TF_LBMENUF_GRAYED, TF_LBMENUF_RADIOCHECKED, TF_LBMENUF_SEPARATOR,
    TF_LBMENUF_SUBMENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON, SM_CYSMICON,
};
use windows_core::{w, Error, Result, PCWSTR};

#[cfg(test)]
use windows::Win32::Graphics::Gdi::GetPixel;
#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo};

/// Item ids are local to one [`ITfMenu`] invocation. Keep them stable so a
/// future new menu entry cannot silently change what an old id means.
pub const MENU_RESTORE_MODE: u32 = 1;
pub const MENU_INPUT_MODE: u32 = 2;
pub const MENU_IME_TOGGLE: u32 = 3;
pub const MENU_SETTINGS: u32 = 4;
pub const MENU_MODE_HIRAGANA: u32 = 10;
pub const MENU_MODE_KATAKANA: u32 = 11;
pub const MENU_MODE_HALF_KATAKANA: u32 = 12;
pub const MENU_MODE_FULL_ALNUM: u32 = 13;
pub const MENU_MODE_HALF_ALNUM: u32 = 14;
pub const MENU_MODE_DIRECT: u32 = 15;

const SINK_COOKIE: u32 = 1;

/// A selected menu operation. The text service validates the focus generation
/// and asks the active engine session to execute it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    RestoreMode,
    SetMode(Mode),
    ToggleIme,
    OpenSettings,
}

/// The caller-owned state used to build a menu at the instant TSF requests it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub visible: bool,
    pub mode: Option<Mode>,
    pub can_change: bool,
    pub can_restore: bool,
}

/// Mutable state shared by COM callbacks. Scalars live in `Cell` so focus loss
/// can hide the item even if TSF re-enters while a sink callback is in flight.
/// The one COM reference remains behind a short-lived `RefCell` borrow and is
/// always cloned before invoking TSF again.
#[derive(Debug, Default)]
pub struct ModeItemState {
    visible: Cell<bool>,
    mode: Cell<Option<Mode>>,
    can_change: Cell<bool>,
    can_restore: Cell<bool>,
    pending_update: Cell<u32>,
    sink: RefCell<Option<ITfLangBarItemSink>>,
}

impl ModeItemState {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            visible: self.visible.get(),
            mode: self.mode.get(),
            can_change: self.can_change.get(),
            can_restore: self.can_restore.get(),
        }
    }

    /// Replaces the externally visible state and notifies the language bar
    /// after every interior borrow has ended. A missing or temporarily
    /// re-entrant sink leaves a pending notification for the next safe call.
    pub fn update(&self, visible: bool, mode: Option<Mode>, can_change: bool, can_restore: bool) {
        let visible = visible && mode.is_some();
        let mut flags = 0;
        if self.visible.replace(visible) != visible {
            flags |= TF_LBI_STATUS;
        }
        if self.mode.replace(mode) != mode {
            flags |= TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP;
        }
        // Menu items are generated only when opened, so their enabled state
        // needs no asynchronous redraw. Keeping it here still makes a
        // concurrent focus/scope change fail closed at selection time.
        self.can_change.set(can_change);
        self.can_restore.set(can_restore);
        self.queue_update(flags);
    }

    pub fn hide(&self) {
        self.update(false, None, false, false);
    }

    pub fn status(&self) -> u32 {
        if self.visible.get() {
            0
        } else {
            TF_LBI_STATUS_HIDDEN
        }
    }

    pub fn advise_sink(&self, sink: ITfLangBarItemSink) -> Result<u32> {
        let mut slot = self
            .sink
            .try_borrow_mut()
            .map_err(|_| Error::from_hresult(E_FAIL))?;
        if slot.is_some() {
            return Err(Error::from_hresult(CONNECT_E_ADVISELIMIT));
        }
        *slot = Some(sink);
        drop(slot);
        self.flush_update();
        Ok(SINK_COOKIE)
    }

    pub fn unadvise_sink(&self, cookie: u32) -> Result<()> {
        if cookie != SINK_COOKIE {
            return Err(Error::from_hresult(CONNECT_E_NOCONNECTION));
        }
        let mut slot = self
            .sink
            .try_borrow_mut()
            .map_err(|_| Error::from_hresult(E_FAIL))?;
        if slot.take().is_none() {
            return Err(Error::from_hresult(CONNECT_E_NOCONNECTION));
        }
        Ok(())
    }

    /// Detach owns final cleanup. A re-entrant borrowed slot is left for the
    /// corresponding TSF `UnadviseSink`; visibility has already been cleared
    /// through `Cell`, so it cannot leave a stale visible status behind.
    pub fn reset(&self) {
        self.visible.set(false);
        self.mode.set(None);
        self.can_change.set(false);
        self.can_restore.set(false);
        self.pending_update.set(0);
        if let Ok(mut slot) = self.sink.try_borrow_mut() {
            let _ = slot.take();
        }
    }

    fn queue_update(&self, flags: u32) {
        if flags == 0 {
            return;
        }
        self.pending_update.set(self.pending_update.get() | flags);
        self.flush_update();
    }

    fn flush_update(&self) {
        let flags = self.pending_update.get();
        if flags == 0 {
            return;
        }
        let sink = match self.sink.try_borrow() {
            Ok(slot) => slot.clone(),
            Err(_) => return,
        };
        let Some(sink) = sink else {
            return;
        };
        self.pending_update.set(0);
        // SAFETY: this is the sink TSF supplied through `ITfSource`; the COM
        // reference is cloned above and no `RefCell` borrow remains across the
        // re-entrant callback.
        if unsafe { sink.OnUpdate(flags) }.is_err() {
            self.pending_update.set(self.pending_update.get() | flags);
        }
    }
}

/// Maps a menu id back to the one engine operation it is allowed to perform.
pub const fn menu_command(id: u32) -> Option<MenuCommand> {
    match id {
        MENU_RESTORE_MODE => Some(MenuCommand::RestoreMode),
        MENU_IME_TOGGLE => Some(MenuCommand::ToggleIme),
        MENU_SETTINGS => Some(MenuCommand::OpenSettings),
        MENU_MODE_HIRAGANA => Some(MenuCommand::SetMode(Mode::Hiragana)),
        MENU_MODE_KATAKANA => Some(MenuCommand::SetMode(Mode::Katakana)),
        MENU_MODE_HALF_KATAKANA => Some(MenuCommand::SetMode(Mode::HalfKatakana)),
        MENU_MODE_FULL_ALNUM => Some(MenuCommand::SetMode(Mode::FullAlnum)),
        MENU_MODE_HALF_ALNUM => Some(MenuCommand::SetMode(Mode::HalfAlnum)),
        MENU_MODE_DIRECT => Some(MenuCommand::SetMode(Mode::Direct)),
        _ => None,
    }
}

/// Builds Sakura's menu from the current snapshot. This follows the useful
/// hierarchy of familiar IMEs (a focused input-mode submenu and a one-shot
/// restore), while intentionally using Sakura labels and only operations the
/// engine can carry out safely.
pub fn populate_menu(menu: &ITfMenu, state: Snapshot) -> Result<()> {
    let mut unused = None;
    let restore_flags = if state.can_restore && state.can_change {
        0
    } else {
        TF_LBMENUF_GRAYED
    };
    add(
        menu,
        MENU_RESTORE_MODE,
        restore_flags,
        "変更前の入力モードに戻す",
        &mut unused,
    )?;
    add(menu, 0, TF_LBMENUF_SEPARATOR, "", &mut unused)?;

    let mut mode_menu = None;
    add(
        menu,
        MENU_INPUT_MODE,
        TF_LBMENUF_SUBMENU,
        "入力モード",
        &mut mode_menu,
    )?;
    let Some(mode_menu) = mode_menu else {
        return Err(Error::from_hresult(E_FAIL));
    };

    add_mode(
        &mode_menu,
        MENU_MODE_HIRAGANA,
        Mode::Hiragana,
        "ひらがな",
        state,
    )?;
    add_mode(
        &mode_menu,
        MENU_MODE_KATAKANA,
        Mode::Katakana,
        "全角カタカナ",
        state,
    )?;
    add_mode(
        &mode_menu,
        MENU_MODE_HALF_KATAKANA,
        Mode::HalfKatakana,
        "半角カタカナ",
        state,
    )?;
    add(&mode_menu, 0, TF_LBMENUF_SEPARATOR, "", &mut unused)?;
    add_mode(
        &mode_menu,
        MENU_MODE_FULL_ALNUM,
        Mode::FullAlnum,
        "全角英数",
        state,
    )?;
    add_mode(
        &mode_menu,
        MENU_MODE_HALF_ALNUM,
        Mode::HalfAlnum,
        "半角英数",
        state,
    )?;
    add_mode(
        &mode_menu,
        MENU_MODE_DIRECT,
        Mode::Direct,
        "直接入力",
        state,
    )?;

    add(menu, 0, TF_LBMENUF_SEPARATOR, "", &mut unused)?;
    let ime_label = if state.mode == Some(Mode::Direct) {
        "日本語入力をオン"
    } else {
        "日本語入力をオフ"
    };
    let toggle_flags = if state.can_change {
        0
    } else {
        TF_LBMENUF_GRAYED
    };
    add(menu, MENU_IME_TOGGLE, toggle_flags, ime_label, &mut unused)?;
    add(menu, 0, TF_LBMENUF_SEPARATOR, "", &mut unused)?;
    add(menu, MENU_SETTINGS, 0, "Sakura Input の設定", &mut unused)
}

fn add_mode(menu: &ITfMenu, id: u32, mode: Mode, label: &str, state: Snapshot) -> Result<()> {
    let mut flags = if state.mode == Some(mode) {
        TF_LBMENUF_RADIOCHECKED
    } else {
        0
    };
    if !state.can_change {
        flags |= TF_LBMENUF_GRAYED;
    }
    let mut unused = None;
    add(menu, id, flags, label, &mut unused)
}

fn add(
    menu: &ITfMenu,
    id: u32,
    flags: u32,
    label: &str,
    submenu: &mut Option<ITfMenu>,
) -> Result<()> {
    let label: Vec<u16> = label.encode_utf16().collect();
    // SAFETY: the temporary UTF-16 buffer and out-pointer stay live for the
    // duration of the call. The system owns any returned submenu interface.
    unsafe {
        menu.AddMenuItem(
            id,
            flags,
            HBITMAP::default(),
            HBITMAP::default(),
            &label,
            submenu,
        )
    }
}

/// The concise status glyph. The tooltip contains the precise full mode name.
pub const fn label(mode: Mode) -> &'static str {
    match mode {
        Mode::Hiragana | Mode::Katakana | Mode::HalfKatakana => "あ",
        Mode::Direct | Mode::HalfAlnum | Mode::FullAlnum => "A",
    }
}

pub const fn description(mode: Mode) -> &'static str {
    match mode {
        Mode::Direct => "直接入力",
        Mode::Hiragana => "ひらがな",
        Mode::Katakana => "全角カタカナ",
        Mode::HalfKatakana => "半角カタカナ",
        Mode::HalfAlnum => "半角英数",
        Mode::FullAlnum => "全角英数",
    }
}

/// Creates a caller-owned, DPI-sized `あ`/`A` icon. TSF explicitly assigns
/// `DestroyIcon` ownership to the language bar after `GetIcon` returns, so no
/// icon handle is cached in the in-process service.
pub fn icon_for(mode: Mode) -> Result<HICON> {
    // SAFETY: the metrics have no input pointers and return a scalar size.
    let size = unsafe {
        SIZE {
            cx: GetSystemMetrics(SM_CXSMICON).max(16),
            cy: GetSystemMetrics(SM_CYSMICON).max(16),
        }
    };
    // SAFETY: a null HWND requests the screen DC; it is released below on all
    // paths because `Canvas::compose` returns before the explicit release only
    // through its `Result`, which is captured first.
    let screen = unsafe { GetDC(None) };
    let result = Canvas::new(screen, size).compose(size, label(mode));
    // SAFETY: `screen` came from `GetDC(None)` above.
    unsafe { ReleaseDC(None, screen) };
    result
}

struct Canvas {
    memory: HDC,
    color: HBITMAP,
    mask: HBITMAP,
}

impl Canvas {
    fn new(screen: HDC, size: SIZE) -> Self {
        // SAFETY: a live screen DC is sufficient for all three compatible GDI
        // objects; `Drop` releases each one exactly once.
        unsafe {
            Self {
                memory: CreateCompatibleDC(Some(screen)),
                // Keep both planes one-bit. Modern Windows 11 taskbars apply
                // TF_LBI_STYLE_TEXTCOLORICON to black pixels in hbmColor, but
                // do not recolour a legacy mask-only monochrome HICON.
                color: CreateBitmap(size.cx, size.cy, 1, 1, None),
                mask: CreateBitmap(size.cx, size.cy, 1, 1, None),
            }
        }
    }

    fn compose(&self, size: SIZE, text: &str) -> Result<HICON> {
        // The AND mask below decides which pixels are visible. Windows 11 does
        // not consistently apply TF_LBI_STYLE_TEXTCOLORICON to third-party TSF
        // items, so choose the same foreground family as the adjacent language
        // indicator: black for a light system theme, white for a dark one.
        // SAFETY: `color` is live and is restored before any other bitmap is
        // selected or the canvas is released.
        let previous_color = unsafe { SelectObject(self.memory, self.color.into()) };
        // SAFETY: the monochrome colour bitmap is selected into this live DC.
        unsafe {
            let _ = PatBlt(
                self.memory,
                0,
                0,
                size.cx,
                size.cy,
                glyph_plane_rop(system_uses_light_theme()),
            );
            SelectObject(self.memory, previous_color);
        }

        // SAFETY: `mask` is selected only for the duration of drawing and is
        // restored before the canvas drops it.
        let previous = unsafe { SelectObject(self.memory, self.mask.into()) };
        let rect = RECT {
            left: 0,
            top: 0,
            right: size.cx,
            bottom: size.cy,
        };
        // SAFETY: the monochrome bitmap is selected into this live memory DC.
        // AND=1 leaves the taskbar pixel unchanged (transparent), while AND=0
        // exposes the matching black pixel in the colour plane. Initializing
        // the whole mask explicitly avoids depending on allocation contents.
        unsafe {
            let _ = PatBlt(self.memory, 0, 0, size.cx, size.cy, WHITENESS);
        }
        draw_centered(self.memory, &rect, text, COLORREF(0));
        // SAFETY: restores the previous GDI object before `mask` is released.
        unsafe { SelectObject(self.memory, previous) };

        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: self.mask,
            hbmColor: self.color,
        };
        // SAFETY: both monochrome planes are live and unselected. Windows
        // copies them into the returned caller-owned icon.
        unsafe { CreateIconIndirect(&info) }
    }
}

const fn glyph_plane_rop(system_uses_light_theme: bool) -> windows::Win32::Graphics::Gdi::ROP_CODE {
    if system_uses_light_theme {
        BLACKNESS
    } else {
        WHITENESS
    }
}

fn system_uses_light_theme() -> bool {
    let mut value = 1u32;
    let mut bytes = size_of::<u32>() as u32;
    // SAFETY: both names are static NUL-terminated strings, and `value` plus
    // `bytes` are writable for the exact REG_DWORD size supplied to the API.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast::<c_void>()),
            Some(&mut bytes),
        )
    };
    if status == ERROR_SUCCESS && bytes == size_of::<u32>() as u32 {
        value != 0
    } else {
        // Windows 11 defaults to the light system theme when the preference is
        // absent. Matching that default is safer than an invisible white glyph.
        true
    }
}

/// Opens the stable install-root settings bootstrap rather than a versioned
/// payload. The bootstrap resolves the currently registered version, so a menu
/// hosted by an older process still opens the newly installed settings UI.
pub fn open_settings() -> Result<()> {
    let settings = settings_path_from_loaded_module()?;
    Command::new(settings)
        .spawn()
        .map(|_| ())
        .map_err(|_| Error::from_hresult(E_FAIL))
}

fn settings_path_from_loaded_module() -> Result<PathBuf> {
    let mut module = HMODULE::default();
    // SAFETY: with FROM_ADDRESS, Windows interprets this stable function
    // address as an address inside the containing module, not as a string.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(settings_path_from_loaded_module as *const () as *const u16),
            &mut module,
        )?;
    }
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: `module` identifies this loaded DLL and `buffer` is writable for
    // the length supplied by the slice binding.
    let written = unsafe { GetModuleFileNameW(Some(module), &mut buffer) } as usize;
    if written == 0 || written >= buffer.len() {
        return Err(Error::from_thread());
    }
    let module_path = PathBuf::from(OsString::from_wide(&buffer[..written]));
    settings_path_from_module_path(&module_path).ok_or_else(|| Error::from_hresult(E_FAIL))
}

fn settings_path_from_module_path(module_path: &Path) -> Option<PathBuf> {
    if !module_path
        .file_name()?
        .to_string_lossy()
        .eq_ignore_ascii_case("sakura_tsf.dll")
    {
        return None;
    }
    let version_dir = module_path.parent()?;
    let versions_dir = version_dir.parent()?;
    if !versions_dir
        .file_name()?
        .to_string_lossy()
        .eq_ignore_ascii_case("versions")
    {
        return None;
    }
    Some(versions_dir.parent()?.join("sakura_settings.exe"))
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: every handle came from `Canvas::new` and is released once.
        unsafe {
            let _ = DeleteObject(self.color.into());
            let _ = DeleteObject(self.mask.into());
            let _ = DeleteDC(self.memory);
        }
    }
}

fn draw_centered(dc: HDC, rect: &RECT, text: &str, color: COLORREF) {
    let font = font_of_height(((rect.bottom - rect.top) * 4) / 5);
    // SAFETY: the previous font is restored before the temporary font is
    // deleted. `wide` remains valid for the call.
    let previous = unsafe { SelectObject(dc, font.into()) };
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut area = *rect;
    // SAFETY: the caller supplied a live DC and rectangle; the temporary font
    // and UTF-16 buffer remain valid until the previous font is restored.
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

fn font_of_height(height: i32) -> HFONT {
    // SAFETY: Yu Gothic UI is the Japanese Windows UI face used beside this
    // item by the taskbar language indicator. It is part of supported Windows
    // 11, and the explicit face keeps あ/A visually aligned with its 日本 text.
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
            NONANTIALIASED_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0).into(),
            w!("Yu Gothic UI"),
        )
    }
}

/// Returns the `ITfSource` error expected for an unsupported sink interface.
pub fn cannot_connect() -> Error {
    Error::from_hresult(CONNECT_E_CANNOTCONNECT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_a_status_glyph_and_distinct_name() {
        for mode in Mode::ALL {
            assert!(!label(mode).is_empty());
            assert!(!description(mode).is_empty());
        }
        for (index, mode) in Mode::ALL.iter().enumerate() {
            for other in &Mode::ALL[index + 1..] {
                assert_ne!(description(*mode), description(*other));
            }
        }
    }

    #[test]
    fn mode_icon_is_a_transparent_monochrome_glyph() -> Result<()> {
        let icon = icon_for(Mode::Hiragana)?;
        let mut info = ICONINFO::default();
        // SAFETY: `icon` is live and `info` is a writable output structure.
        unsafe { GetIconInfo(icon, &mut info)? };

        let size = unsafe {
            SIZE {
                cx: GetSystemMetrics(SM_CXSMICON).max(16),
                cy: GetSystemMetrics(SM_CYSMICON).max(16),
            }
        };
        // The transparent mask contains the background and the glyph's opaque
        // pixels; the one-bit colour plane lets TSF apply its theme text colour.
        let has_theme_color_plane = !info.hbmColor.is_invalid();
        let dc = unsafe { CreateCompatibleDC(None) };
        let previous = unsafe { SelectObject(dc, info.hbmMask.into()) };
        let corners_are_transparent = [(0, 0), (size.cx - 1, 0), (0, size.cy - 1)]
            .into_iter()
            .all(|(x, y)| unsafe { GetPixel(dc, x, y).0 } == 0x00ff_ffff);
        let mut glyph_pixels = 0usize;
        for y in 0..size.cy {
            for x in 0..size.cx {
                if unsafe { GetPixel(dc, x, y).0 } == 0 {
                    glyph_pixels += 1;
                }
            }
        }
        // SAFETY: restore the original DC object, then release every copied
        // handle returned by GetIconInfo and finally the caller-owned icon.
        unsafe {
            SelectObject(dc, previous);
            let _ = DeleteDC(dc);
            let _ = DeleteObject(info.hbmMask.into());
            if !info.hbmColor.is_invalid() {
                let _ = DeleteObject(info.hbmColor.into());
            }
            let _ = DestroyIcon(icon);
        }

        assert!(
            has_theme_color_plane,
            "Windows 11 needs a black colour plane for theme recolouring"
        );
        assert!(
            corners_are_transparent,
            "the taskbar background must remain visible around the glyph"
        );
        assert!(
            glyph_pixels > 0,
            "the monochrome mask must contain the glyph"
        );
        assert!(
            glyph_pixels < (size.cx * size.cy) as usize,
            "the glyph must not fill the entire icon"
        );
        Ok(())
    }

    #[test]
    fn glyph_plane_contrasts_with_both_system_themes() {
        assert_eq!(glyph_plane_rop(true), BLACKNESS);
        assert_eq!(glyph_plane_rop(false), WHITENESS);
    }

    #[test]
    fn visibility_is_fail_closed_without_a_mode() {
        let item = ModeItemState::default();
        item.update(true, None, true, false);
        assert_eq!(item.snapshot().visible, false);
        assert_eq!(item.status(), TF_LBI_STATUS_HIDDEN);
    }

    #[test]
    fn menu_ids_have_one_supported_meaning() {
        assert_eq!(
            menu_command(MENU_RESTORE_MODE),
            Some(MenuCommand::RestoreMode)
        );
        assert_eq!(
            menu_command(MENU_MODE_HALF_KATAKANA),
            Some(MenuCommand::SetMode(Mode::HalfKatakana))
        );
        assert_eq!(menu_command(MENU_IME_TOGGLE), Some(MenuCommand::ToggleIme));
        assert_eq!(menu_command(MENU_SETTINGS), Some(MenuCommand::OpenSettings));
        assert_eq!(menu_command(999), None);
    }

    #[test]
    fn settings_bootstrap_is_derived_only_from_the_versioned_tsf_layout() {
        let module =
            Path::new(r"C:\Program Files\Sakura Input\versions\1.0.0-build\sakura_tsf.dll");
        assert_eq!(
            settings_path_from_module_path(module),
            Some(PathBuf::from(
                r"C:\Program Files\Sakura Input\sakura_settings.exe"
            ))
        );
        assert_eq!(
            settings_path_from_module_path(Path::new(r"C:\temp\sakura_tsf.dll")),
            None
        );
        assert_eq!(
            settings_path_from_module_path(Path::new(
                r"C:\Program Files\Sakura Input\versions\1.0.0-build\other.dll"
            )),
            None
        );
    }
}
