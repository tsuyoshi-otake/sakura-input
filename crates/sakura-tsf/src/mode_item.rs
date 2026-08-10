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
use windows::Win32::Foundation::{ERROR_SUCCESS, E_FAIL, HMODULE};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
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
    CreateIconIndirect, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};
use windows_core::{w, Error, Result, PCWSTR};

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
        Mode::Hiragana => "あ",
        Mode::Katakana => "ア",
        Mode::HalfKatakana => "ｱ",
        Mode::FullAlnum => "Ａ",
        Mode::HalfAlnum | Mode::Direct => "A",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetSize {
    Px16,
    Px32,
}

impl AssetSize {
    const fn edge(self) -> i32 {
        match self {
            Self::Px16 => 16,
            Self::Px32 => 32,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct IconAsset {
    size: AssetSize,
    pixels: &'static [u8],
}

const fn asset_size_for_metric(metric: i32) -> AssetSize {
    if metric >= 24 {
        AssetSize::Px32
    } else {
        AssetSize::Px16
    }
}

fn asset_for(mode: Mode, size: AssetSize, light_theme: bool) -> IconAsset {
    let pixels = match (mode, size, light_theme) {
        (Mode::Hiragana, AssetSize::Px16, true) => {
            include_bytes!("../assets/mode-indicator/hiragana-16-light.bgra").as_slice()
        }
        (Mode::Hiragana, AssetSize::Px16, false) => {
            include_bytes!("../assets/mode-indicator/hiragana-16-dark.bgra").as_slice()
        }
        (Mode::Hiragana, AssetSize::Px32, true) => {
            include_bytes!("../assets/mode-indicator/hiragana-32-light.bgra").as_slice()
        }
        (Mode::Hiragana, AssetSize::Px32, false) => {
            include_bytes!("../assets/mode-indicator/hiragana-32-dark.bgra").as_slice()
        }
        (Mode::Katakana, AssetSize::Px16, true) => {
            include_bytes!("../assets/mode-indicator/katakana-16-light.bgra").as_slice()
        }
        (Mode::Katakana, AssetSize::Px16, false) => {
            include_bytes!("../assets/mode-indicator/katakana-16-dark.bgra").as_slice()
        }
        (Mode::Katakana, AssetSize::Px32, true) => {
            include_bytes!("../assets/mode-indicator/katakana-32-light.bgra").as_slice()
        }
        (Mode::Katakana, AssetSize::Px32, false) => {
            include_bytes!("../assets/mode-indicator/katakana-32-dark.bgra").as_slice()
        }
        (Mode::HalfKatakana, AssetSize::Px16, true) => {
            include_bytes!("../assets/mode-indicator/half-katakana-16-light.bgra").as_slice()
        }
        (Mode::HalfKatakana, AssetSize::Px16, false) => {
            include_bytes!("../assets/mode-indicator/half-katakana-16-dark.bgra").as_slice()
        }
        (Mode::HalfKatakana, AssetSize::Px32, true) => {
            include_bytes!("../assets/mode-indicator/half-katakana-32-light.bgra").as_slice()
        }
        (Mode::HalfKatakana, AssetSize::Px32, false) => {
            include_bytes!("../assets/mode-indicator/half-katakana-32-dark.bgra").as_slice()
        }
        (Mode::FullAlnum, AssetSize::Px16, true) => {
            include_bytes!("../assets/mode-indicator/full-alnum-16-light.bgra").as_slice()
        }
        (Mode::FullAlnum, AssetSize::Px16, false) => {
            include_bytes!("../assets/mode-indicator/full-alnum-16-dark.bgra").as_slice()
        }
        (Mode::FullAlnum, AssetSize::Px32, true) => {
            include_bytes!("../assets/mode-indicator/full-alnum-32-light.bgra").as_slice()
        }
        (Mode::FullAlnum, AssetSize::Px32, false) => {
            include_bytes!("../assets/mode-indicator/full-alnum-32-dark.bgra").as_slice()
        }
        (Mode::HalfAlnum, AssetSize::Px16, true) => {
            include_bytes!("../assets/mode-indicator/half-alnum-16-light.bgra").as_slice()
        }
        (Mode::HalfAlnum, AssetSize::Px16, false) => {
            include_bytes!("../assets/mode-indicator/half-alnum-16-dark.bgra").as_slice()
        }
        (Mode::HalfAlnum, AssetSize::Px32, true) => {
            include_bytes!("../assets/mode-indicator/half-alnum-32-light.bgra").as_slice()
        }
        (Mode::HalfAlnum, AssetSize::Px32, false) => {
            include_bytes!("../assets/mode-indicator/half-alnum-32-dark.bgra").as_slice()
        }
        (Mode::Direct, AssetSize::Px16, true) => {
            include_bytes!("../assets/mode-indicator/direct-16-light.bgra").as_slice()
        }
        (Mode::Direct, AssetSize::Px16, false) => {
            include_bytes!("../assets/mode-indicator/direct-16-dark.bgra").as_slice()
        }
        (Mode::Direct, AssetSize::Px32, true) => {
            include_bytes!("../assets/mode-indicator/direct-32-light.bgra").as_slice()
        }
        (Mode::Direct, AssetSize::Px32, false) => {
            include_bytes!("../assets/mode-indicator/direct-32-dark.bgra").as_slice()
        }
    };
    IconAsset { size, pixels }
}

/// Creates a caller-owned icon from one original, DPI-specific ARGB asset. TSF
/// assigns `DestroyIcon` ownership to the language bar after `GetIcon` returns,
/// so the in-process service never caches a host-owned handle.
pub fn icon_for(mode: Mode) -> Result<HICON> {
    // SAFETY: the metric has no input pointers and returns one scalar size.
    let size = asset_size_for_metric(unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16));
    create_icon(asset_for(mode, size, system_uses_light_theme()))
}

fn create_icon(asset: IconAsset) -> Result<HICON> {
    let edge = asset.size.edge();
    if asset.pixels.len() != edge as usize * edge as usize * 4 {
        return Err(Error::from_hresult(E_FAIL));
    }

    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: edge,
            // A negative height makes the DIB top-down, matching asset order.
            biHeight: -edge,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: asset.pixels.len() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut::<c_void>();
    // SAFETY: `info` is a valid top-down 32-bit DIB descriptor and `bits` is a
    // writable out-pointer. A null section asks GDI for process-owned storage.
    let color = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
    if bits.is_null() {
        // SAFETY: `color` was created immediately above and is not selected.
        unsafe {
            let _ = DeleteObject(color.into());
        }
        return Err(Error::from_hresult(E_FAIL));
    }
    // SAFETY: the DIB allocation is exactly `width * height * 4` bytes and the
    // embedded asset was length-checked above. The ranges cannot overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(asset.pixels.as_ptr(), bits.cast::<u8>(), asset.pixels.len());
    }

    let mask_stride = (edge as usize).div_ceil(16) * 2;
    let mask_bits = vec![0u8; mask_stride * edge as usize];
    // SAFETY: `mask_bits` contains the word-aligned monochrome scanlines that
    // `CreateBitmap` requires and remains live for the duration of the call.
    let mask = unsafe { CreateBitmap(edge, edge, 1, 1, Some(mask_bits.as_ptr().cast::<c_void>())) };
    if mask.is_invalid() {
        // SAFETY: `color` is still live and unselected.
        unsafe {
            let _ = DeleteObject(color.into());
        }
        return Err(Error::from_thread());
    }

    let icon_info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    // SAFETY: both bitmaps are live and unselected. CreateIconIndirect copies
    // their pixels, after which this function owns and releases both handles.
    let icon = unsafe { CreateIconIndirect(&icon_info) };
    // SAFETY: CreateIconIndirect has completed, both bitmap handles are still
    // owned here, and neither is selected into a device context.
    unsafe {
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
    }
    icon
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
    let module_units = buffer
        .get(..written)
        .ok_or_else(|| Error::from_hresult(E_FAIL))?;
    let module_path = PathBuf::from(OsString::from_wide(module_units));
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
            for other in Mode::ALL.iter().skip(index + 1) {
                assert_ne!(description(*mode), description(*other));
            }
        }
    }

    #[test]
    fn every_mode_theme_and_size_is_a_valid_argb_asset() {
        for mode in Mode::ALL {
            for size in [AssetSize::Px16, AssetSize::Px32] {
                let edge = size.edge() as usize;
                for light_theme in [false, true] {
                    let asset = asset_for(mode, size, light_theme);
                    assert_eq!(asset.pixels.len(), edge * edge * 4);

                    let alpha: Vec<u8> = asset
                        .pixels
                        .chunks_exact(4)
                        .map(|pixel| pixel.last().copied().unwrap_or_default())
                        .collect();
                    for corner in [0, edge - 1, edge * (edge - 1), edge * edge - 1] {
                        assert_eq!(
                            alpha.get(corner).copied(),
                            Some(0),
                            "{mode:?} {size:?} corner"
                        );
                    }
                    assert!(alpha.contains(&0));
                    assert!(alpha.iter().any(|value| *value > 0));
                    assert!(alpha.iter().any(|value| (1..255).contains(value)));

                    for pixel in asset.pixels.chunks_exact(4) {
                        let blue = pixel.first().copied().unwrap_or_default();
                        let green = pixel.get(1).copied().unwrap_or_default();
                        let red = pixel.get(2).copied().unwrap_or_default();
                        let alpha = pixel.last().copied().unwrap_or_default();
                        assert!(blue <= alpha && green <= alpha && red <= alpha);
                        if light_theme {
                            assert!(blue <= 28 && green <= 28 && red <= 28);
                        } else {
                            assert_eq!((blue, green, red), (alpha, alpha, alpha));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn theme_variants_share_alpha_but_contrast_and_modes_stay_distinct() {
        for size in [AssetSize::Px16, AssetSize::Px32] {
            let mut mode_alpha = Vec::new();
            for mode in Mode::ALL {
                let dark = asset_for(mode, size, false);
                let light = asset_for(mode, size, true);
                let dark_alpha: Vec<u8> = dark
                    .pixels
                    .chunks_exact(4)
                    .map(|pixel| pixel.last().copied().unwrap_or_default())
                    .collect();
                let light_alpha: Vec<u8> = light
                    .pixels
                    .chunks_exact(4)
                    .map(|pixel| pixel.last().copied().unwrap_or_default())
                    .collect();
                assert_eq!(dark_alpha, light_alpha);
                assert_ne!(dark.pixels, light.pixels);
                assert!(mode_alpha.iter().all(|other| other != &dark_alpha));
                mode_alpha.push(dark_alpha);
            }
        }
    }

    #[test]
    fn every_asset_constructs_an_owned_hicon() -> Result<()> {
        for mode in Mode::ALL {
            for size in [AssetSize::Px16, AssetSize::Px32] {
                for light_theme in [false, true] {
                    let icon = create_icon(asset_for(mode, size, light_theme))?;
                    let mut info = ICONINFO::default();
                    // SAFETY: `icon` is live and `info` is a writable output.
                    unsafe { GetIconInfo(icon, &mut info)? };
                    assert!(!info.hbmColor.is_invalid());
                    assert!(!info.hbmMask.is_invalid());
                    // SAFETY: GetIconInfo returns caller-owned bitmap copies;
                    // the original icon is caller-owned as well.
                    unsafe {
                        let _ = DeleteObject(info.hbmColor.into());
                        let _ = DeleteObject(info.hbmMask.into());
                        let _ = DestroyIcon(icon);
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn direct_input_has_a_dedicated_asset() {
        for size in [AssetSize::Px16, AssetSize::Px32] {
            assert_ne!(
                asset_for(Mode::Direct, size, false).pixels,
                asset_for(Mode::HalfAlnum, size, false).pixels
            );
        }
    }

    #[test]
    fn metric_selects_nearest_authored_size() {
        for metric in 0..24 {
            assert_eq!(asset_size_for_metric(metric), AssetSize::Px16);
        }
        for metric in 24..=64 {
            assert_eq!(asset_size_for_metric(metric), AssetSize::Px32);
        }
    }

    #[test]
    fn visibility_is_fail_closed_without_a_mode() {
        let item = ModeItemState::default();
        item.update(true, None, true, false);
        assert!(!item.snapshot().visible);
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
