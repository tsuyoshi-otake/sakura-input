//! Native Win32 settings control panel.
//!
//! The UI is intentionally a thin frontend over `sakura-settings`' tested
//! library operations. Every button reaches the same transactional path as the
//! CLI; the window never edits a durable file piecemeal.

use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use sakura_core::{
    AppProfile, AppearanceTheme, BracketStyle, CommaMark, ConversionMethod, InputMethod,
    InputSupport, NeuralRerankerScope, Normalizer, NotationStyle, PadShortcut, PeriodMark, Preset,
    PunctuationStyle, ShiftSpaceBehavior, SpaceWidth, SuggestAccept, UserDictionary,
    UserDictionaryEntry, UserPartOfSpeech, Width,
};
use sakura_proto::Mode;
use sakura_reg::user_preferences::{
    self, AiAuth, AiEffort, AiProvider, AiServiceTier, AiStyle, AiTextKey, AiTextPreferences,
};
use sakura_settings::configuration::ConfigurationDocument;
use sakura_settings::formats::DictionaryFormat;
use sakura_settings::user_dictionary::{self, ImportMode};
use sakura_settings::{diagnostics, learning, paths, updater};
use windows::core::{Result as WindowsResult, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, COLORREF, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, FrameRect, GetStockObject,
    GetSysColorBrush, InvalidateRect, SetBkColor, SetBkMode, SetTextColor, UpdateWindow,
    COLOR_WINDOW, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, TRANSPARENT,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::Controls::{
    SetWindowTheme, BST_CHECKED, BST_UNCHECKED, DRAWITEMSTRUCT, HTREEITEM, NMTREEVIEWW,
    ODS_DISABLED, ODS_FOCUS, ODS_HOTLIGHT, ODS_SELECTED, ODT_BUTTON, TVE_EXPAND, TVGN_CARET,
    TVGN_CHILD, TVIF_CHILDREN, TVIF_PARAM, TVIF_TEXT, TVINSERTSTRUCTW, TVINSERTSTRUCTW_0,
    TVITEMEXW_CHILDREN, TVITEMW, TVI_LAST, TVM_EXPAND, TVM_GETNEXTITEM, TVM_INSERTITEMW,
    TVM_SELECTITEM, TVN_SELCHANGEDW, TVS_HASBUTTONS, TVS_HASLINES, TVS_LINESATROOT,
    TVS_SHOWSELALWAYS,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow,
    DispatchMessageW, FindWindowW, GetClientRect, GetMessageW, GetParent, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW,
    IsIconic, LoadCursorW, LoadImageW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    SystemParametersInfoW, TranslateMessage, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX,
    BS_AUTORADIOBUTTON, BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_OWNERDRAW, BS_PUSHBUTTON, BS_TYPEMASK,
    CBN_SELCHANGE, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CW_USEDEFAULT,
    ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_PASSWORD, ES_READONLY, ES_WANTRETURN,
    GWLP_USERDATA, GWL_STYLE, GW_CHILD, GW_ENABLEDPOPUP, GW_HWNDNEXT, GW_OWNER, ICON_BIG,
    ICON_SMALL, IDC_ARROW, IDYES, IMAGE_ICON, LBN_SELCHANGE, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY,
    LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT, LB_SETCURSEL, LR_LOADFROMFILE, MB_ICONERROR,
    MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MSG, SPI_GETHIGHCONTRAST,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_RESTORE,
    SW_SHOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE,
    WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_KEYDOWN, WM_NOTIFY, WM_SETFONT, WM_SETICON,
    WM_SETTINGCHANGE, WM_THEMECHANGED, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_DISABLED, WS_EX_CLIENTEDGE, WS_EX_CONTROLPARENT, WS_GROUP, WS_HSCROLL, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindowVisible, LB_GETCOUNT, LB_GETTEXT, LB_GETTEXTLEN, WM_GETICON,
};

const WINDOW_CLASS: PCWSTR = windows::core::w!("SakuraInputSettingsWindow");
/// The class of the hidden window that owns the settings window.
///
/// It exists for one reason: an unowned top-level window gets a taskbar
/// button, and the settings sheet is a dialog reached from the language bar,
/// not a program the owner started. Giving it an owner takes the button away
/// and leaves its caption, its size and its behaviour exactly as they were.
/// It is a class of its own so that the single-instance `FindWindowW` search
/// still finds the settings window and never this one.
const OWNER_CLASS: PCWSTR = windows::core::w!("SakuraInputSettingsOwner");
const PANEL_CLASS: PCWSTR = windows::core::w!("SakuraInputSettingsPanel");
// The stable bootstrap can launch a new versioned payload on every click, so
// the payload—not the bootstrap—owns the per-session settings singleton.
// `Local\\` keeps separate interactive sessions independent while making all
// versions of the settings UI share one slot for the current session.
const SINGLE_INSTANCE_NAME: PCWSTR = windows::core::w!("Local\\SakuraInputSettings");
const PANEL_COUNT: usize = 5;
const WM_UPDATE_COMPLETE: u32 = WM_APP + 17;
const DARK_SURFACE: COLORREF = rgb(0x35, 0x35, 0x35);
const DARK_INPUT_SURFACE: COLORREF = rgb(0x25, 0x25, 0x25);
const DARK_INK: COLORREF = rgb(0xF5, 0xF3, 0xF1);

/// The settings window keeps both icon sizes alive until the window is gone.
/// `WM_SETICON` does not transfer ownership of either HICON to the window.
#[derive(Debug)]
struct WindowIcons {
    big: windows::Win32::UI::WindowsAndMessaging::HICON,
    small: windows::Win32::UI::WindowsAndMessaging::HICON,
}

impl Drop for WindowIcons {
    fn drop(&mut self) {
        // SAFETY: both handles came from LoadImageW and are destroyed once
        // after the settings window and its message pump have ended.
        unsafe {
            let _ = DestroyIcon(self.big);
            let _ = DestroyIcon(self.small);
        }
    }
}
const DARK_DISABLED_INK: COLORREF = rgb(0xAA, 0xA7, 0xA4);
const DARK_BUTTON: COLORREF = rgb(0x45, 0x45, 0x45);
const DARK_BUTTON_HOVER: COLORREF = rgb(0x4C, 0x46, 0x49);
const DARK_BUTTON_PRESSED: COLORREF = rgb(0x2E, 0x2B, 0x2C);
const DARK_BUTTON_DISABLED: COLORREF = rgb(0x3B, 0x3B, 0x3B);
const DARK_BUTTON_BORDER: COLORREF = rgb(0x68, 0x63, 0x65);
const SAKURA_ACCENT: COLORREF = rgb(0xB7, 0x7C, 0x8C);
const LIGHT_SURFACE: COLORREF = rgb(0xF7, 0xF6, 0xF4);
const LIGHT_INPUT_SURFACE: COLORREF = rgb(0xE8, 0xE5, 0xE2);
// Keep ordinary Light text neutral; Sakura is reserved for focus and selection.
const LIGHT_INK: COLORREF = rgb(0x2F, 0x2F, 0x2F);
const LIGHT_DISABLED_INK: COLORREF = rgb(0x70, 0x70, 0x70);
const LIGHT_BUTTON: COLORREF = rgb(0xF7, 0xF6, 0xF4);
const LIGHT_BUTTON_HOVER: COLORREF = rgb(0xE8, 0xE5, 0xE2);
const LIGHT_BUTTON_PRESSED: COLORREF = rgb(0xD9, 0xD4, 0xD0);
const LIGHT_BUTTON_DISABLED: COLORREF = rgb(0xEE, 0xEC, 0xEA);
const LIGHT_BUTTON_BORDER: COLORREF = rgb(0xBD, 0xB9, 0xB5);
const LIGHT_SAKURA_ACCENT: COLORREF = rgb(0xB2, 0x8D, 0x96);
// This is the measured outer size of a compact Japanese IME property dialog
// at 100% DPI, including the native non-client frame.
const WINDOW_WIDTH: i32 = 629;
const WINDOW_HEIGHT: i32 = 464;
const NAVIGATION_WIDTH: i32 = 161;
const PANEL_LEFT: i32 = 211;
const PANEL_TOP: i32 = 74;
const PANEL_WIDTH: i32 = 385;
const PANEL_HEIGHT: i32 = 300;
const BOTTOM_ACTION_Y: i32 = 384;
// Topic copy ends at y=38.  Start the first framed group on the next 8 px
// rhythm step so a group caption never crowds the page description.
const TOPIC_GROUP_TOP: i32 = 48;
const NORMALIZER_GROUP_HEIGHT: i32 = 192;
const NORMALIZER_RESET_Y: i32 = 264;
const NORMALIZER_RESET_HEIGHT: i32 = 24;
// The profile group frames a list, five rows and its two action buttons. It
// grew by one row when the notation preset landed; the assert below is what
// stops the next row from silently overflowing the panel instead.
const PROFILE_GROUP_HEIGHT: i32 = 216;
const PROFILE_CONTENT_BOTTOM: i32 = 254;
// TreeView does not use the parent WM_CTLCOLOR brush for its item labels. Set
// its documented colors explicitly so dark-mode rows do not retain the native
// white item background behind otherwise dark panels.
const TVM_SETBKCOLOR: u32 = 0x111D; // TV_FIRST + 29
const TVM_SETTEXTCOLOR: u32 = 0x111E; // TV_FIRST + 30
const CLR_NONE: COLORREF = COLORREF(u32::MAX);
// ATOK's measured property rail is about 161 logical px wide with a 12–14 px
// gutter before the right pane. Keep the input TreeView on that same visual
// grid while the flat pages retain the slightly wider topic ListBox.
const INPUT_TREE_LEFT: i32 = 36;
const INPUT_TREE_TOP: i32 = PANEL_TOP + 20;
const INPUT_TREE_WIDTH: i32 = 161;
const INPUT_TREE_HEIGHT: i32 = 282;
// The dictionary page has the densest content. Its last helper baseline must
// remain above the persistent root action row at every theme/DPI.
const DICTIONARY_CONTENT_BOTTOM: i32 = 288;

// Input / conversion topic IDs carried by the native tree item's lParam.
// Every selectable tree row maps to the one right-hand panel it owns.
const INPUT_TOPIC_BASIC: usize = 0;
const INPUT_TOPIC_PROFILE: usize = 1;
const INPUT_TOPIC_INPUT_ASSIST: usize = 2;
const INPUT_TOPIC_SEGMENT: usize = 3;
const INPUT_TOPIC_PREDICTION: usize = 4;
const INPUT_TOPIC_ASSOCIATION: usize = 5;
const INPUT_TOPIC_DISPLAY: usize = 6;
const INPUT_TOPIC_NORMALIZER: usize = 7;
const INPUT_TOPIC_AI_TEXT: usize = 8;
const INPUT_TOPIC_INPUT_REPAIR: usize = 9;
const INPUT_TOPIC_INPUT_SYMBOL: usize = 10;
const TREE_GROUP: usize = usize::MAX;
// Shown after `NotationStyle::ALL`. It is a readout, never a value: choosing
// it writes nothing, and it is what the preset falls back to whenever the
// underlying controls spell a combination no shipped style produces.
const NOTATION_STYLE_CUSTOM_LABEL: &str = "カスタム（個別に設定）";
// Keep the familiar property-sheet hierarchy through 連想変換, but do not
// invent ATOK-only pages or map a label to an unrelated Sakura setting. Each
// leaf owns the panel the user sees on the right; category rows normalize to
// their first leaf so the TreeView highlight and right-hand page agree.
const INPUT_TREE_LABELS: [&str; 13] = [
    "基本",
    "入力補助",
    "AI文章変換",
    "変換補助",
    "文節変換",
    "文字幅・句読点",
    "表示",
    "入力支援",
    "入力誤りの自動修復",
    "英単語・記号置換",
    "推測変換",
    "連想変換",
    "アプリ別の設定",
];

const _: () = assert!(PANEL_TOP + DICTIONARY_CONTENT_BOTTOM < BOTTOM_ACTION_Y);
const _: () = assert!(NORMALIZER_RESET_Y >= TOPIC_GROUP_TOP + NORMALIZER_GROUP_HEIGHT + 8);
const _: () = assert!(NORMALIZER_RESET_Y + NORMALIZER_RESET_HEIGHT <= PANEL_HEIGHT - 8);
const _: () = assert!(TOPIC_GROUP_TOP + PROFILE_GROUP_HEIGHT <= PANEL_HEIGHT - 8);
const _: () = assert!(PROFILE_CONTENT_BOTTOM <= TOPIC_GROUP_TOP + PROFILE_GROUP_HEIGHT);

#[derive(Debug)]
struct GeneralControls {
    basic_panel: HWND,
    profile_panel: HWND,
    input_assist_panel: HWND,
    ai_text_panel: HWND,
    segment_panel: HWND,
    normalizer_panel: HWND,
    prediction_panel: HWND,
    association_panel: HWND,
    display_panel: HWND,
    input_repair_panel: HWND,
    input_symbol_panel: HWND,
    keymap: HWND,
    input_method_romaji: HWND,
    input_method_kana: HWND,
    default_mode: HWND,
    pad_shortcut: HWND,
    input_assist_space_width: HWND,
    input_assist_shift_space: HWND,
    ai_text_key: HWND,
    ai_provider: HWND,
    ai_endpoint: HWND,
    ai_auth: HWND,
    ai_api_key: HWND,
    ai_api_key_status: HWND,
    ai_api_key_clear: HWND,
    ai_style: HWND,
    ai_effort: HWND,
    ai_service_tier: HWND,
    ai_providers: Vec<AiProvider>,
    conversion_assist_method: HWND,
    prediction: HWND,
    suggest: HWND,
    association: HWND,
    appearance: HWND,
    neural_reranker_scope: HWND,
    notation_style: HWND,
    normalizer_alnum: HWND,
    normalizer_number: HWND,
    normalizer_symbol: HWND,
    punctuation_period: HWND,
    punctuation_comma: HWND,
    punctuation_brackets: HWND,
    normalizer_reset: HWND,
    input_support_enabled: HWND,
    input_support_commit_based: HWND,
    input_support_advanced: HWND,
    input_support_vowel_count: HWND,
    input_support_consonant_extra: HWND,
    input_support_n_count: HWND,
    input_support_dakuten_swap: HWND,
    input_support_tsu_sokuon: HWND,
    input_support_wa_wo: HWND,
    input_support_small_u: HWND,
    input_support_fuzzy_proper_nouns: HWND,
    input_support_reset: HWND,
    input_support_english_to_katakana: HWND,
    input_support_period_after_digit: HWND,
    input_support_comma_after_digit: HWND,
    input_support_middle_dot_after_digit: HWND,
    input_support_long_vowel_after_alnum: HWND,
    profile_list: HWND,
    profile_process: HWND,
    profile_mode: HWND,
    profile_prediction: HWND,
    profile_suggest: HWND,
    profile_notation: HWND,
    profile_save: HWND,
    profile_delete: HWND,
}

/// Palette resources that remain valid while the window procedure returns them
/// to controls in `WM_CTLCOLOR*`. Windows owns the stock brushes in
/// high-contrast mode; the normal Light and Dark palette brushes are ours to
/// delete.
#[derive(Debug)]
struct ThemeBrushes {
    surface: HBRUSH,
    input: HBRUSH,
    button: HBRUSH,
    button_hover: HBRUSH,
    button_pressed: HBRUSH,
    button_disabled: HBRUSH,
    button_border: HBRUSH,
    accent: HBRUSH,
}

impl Drop for ThemeBrushes {
    fn drop(&mut self) {
        // SAFETY: these are unselected brushes created by `CreateSolidBrush`
        // and retained only for the lifetime of the settings window.
        unsafe {
            let _ = DeleteObject(self.surface.into());
            let _ = DeleteObject(self.input.into());
            let _ = DeleteObject(self.button.into());
            let _ = DeleteObject(self.button_hover.into());
            let _ = DeleteObject(self.button_pressed.into());
            let _ = DeleteObject(self.button_disabled.into());
            let _ = DeleteObject(self.button_border.into());
            let _ = DeleteObject(self.accent.into());
        }
    }
}

#[derive(Debug)]
struct UiTheme {
    dark: bool,
    high_contrast: bool,
    brushes: Option<ThemeBrushes>,
}

impl UiTheme {
    fn resolve(appearance: AppearanceTheme) -> Self {
        let high_contrast = high_contrast_enabled();
        let dark = !high_contrast
            && match appearance {
                AppearanceTheme::Auto => !windows_apps_use_light_theme(),
                AppearanceTheme::Light => false,
                AppearanceTheme::Dark => true,
            };
        if high_contrast {
            return Self {
                dark,
                high_contrast,
                brushes: None,
            };
        }

        let brushes = if dark {
            [
                DARK_SURFACE,
                DARK_INPUT_SURFACE,
                DARK_BUTTON,
                DARK_BUTTON_HOVER,
                DARK_BUTTON_PRESSED,
                DARK_BUTTON_DISABLED,
                DARK_BUTTON_BORDER,
                SAKURA_ACCENT,
            ]
        } else {
            [
                LIGHT_SURFACE,
                LIGHT_INPUT_SURFACE,
                LIGHT_BUTTON,
                LIGHT_BUTTON_HOVER,
                LIGHT_BUTTON_PRESSED,
                LIGHT_BUTTON_DISABLED,
                LIGHT_BUTTON_BORDER,
                LIGHT_SAKURA_ACCENT,
            ]
        };
        // A GDI allocation failure falls back to system colors rather than
        // leaving a partially themed surface behind.
        // SAFETY: CreateSolidBrush copies only its scalar COLORREF argument.
        let handles = unsafe { brushes.map(|color| CreateSolidBrush(color)) };
        if handles.iter().any(|brush| brush.is_invalid()) {
            // SAFETY: only successfully-created brushes are deleted; every
            // handle is local and unselected at this construction boundary.
            unsafe {
                for brush in handles.into_iter().filter(|brush| !brush.is_invalid()) {
                    let _ = DeleteObject(brush.into());
                }
            }
            return Self {
                dark,
                high_contrast,
                brushes: None,
            };
        }
        Self {
            dark,
            high_contrast,
            brushes: Some(ThemeBrushes {
                surface: handles[0],
                input: handles[1],
                button: handles[2],
                button_hover: handles[3],
                button_pressed: handles[4],
                button_disabled: handles[5],
                button_border: handles[6],
                accent: handles[7],
            }),
        }
    }

    fn surface_brush(&self) -> HBRUSH {
        self.brushes.as_ref().map_or_else(
            // SAFETY: COLOR_WINDOW is a documented system color role.
            || unsafe { GetSysColorBrush(COLOR_WINDOW) },
            |brushes| brushes.surface,
        )
    }

    fn input_brush(&self) -> HBRUSH {
        self.brushes.as_ref().map_or_else(
            // SAFETY: COLOR_WINDOW is a documented system color role.
            || unsafe { GetSysColorBrush(COLOR_WINDOW) },
            |brushes| brushes.input,
        )
    }

    const fn ink(&self) -> COLORREF {
        if self.dark {
            DARK_INK
        } else {
            LIGHT_INK
        }
    }

    const fn disabled_ink(&self) -> COLORREF {
        if self.dark {
            DARK_DISABLED_INK
        } else {
            LIGHT_DISABLED_INK
        }
    }

    fn apply_control_colors(&self, message: u32, dc: HDC, input_surface: bool) -> Option<HBRUSH> {
        if self.high_contrast {
            return None;
        }
        // SAFETY: the device context belongs to the synchronous WM_CTLCOLOR
        // message and is valid for these scalar color updates.
        unsafe {
            let _ = SetTextColor(dc, self.ink());
            let _ = SetBkColor(
                dc,
                if input_surface || matches!(message, WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX) {
                    if self.dark {
                        DARK_INPUT_SURFACE
                    } else {
                        LIGHT_INPUT_SURFACE
                    }
                } else {
                    if self.dark {
                        DARK_SURFACE
                    } else {
                        LIGHT_SURFACE
                    }
                },
            );
        }
        match message {
            WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => Some(self.input_brush()),
            WM_CTLCOLORSTATIC if input_surface => Some(self.input_brush()),
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => Some(self.surface_brush()),
            _ => None,
        }
    }

    fn draw_button(&self, item: &DRAWITEMSTRUCT, default: bool, selected_tab: bool) -> bool {
        if self.high_contrast || item.CtlType != ODT_BUTTON {
            return false;
        }
        let Some(brushes) = &self.brushes else {
            return false;
        };
        let state = item.itemState.0;
        let disabled = state & ODS_DISABLED.0 != 0;
        let pressed = state & ODS_SELECTED.0 != 0;
        let hovered = state & ODS_HOTLIGHT.0 != 0;
        let focused = state & ODS_FOCUS.0 != 0;
        let fill = if disabled {
            brushes.button_disabled
        } else if pressed {
            brushes.button_pressed
        } else if hovered || selected_tab {
            brushes.button_hover
        } else {
            brushes.button
        };
        let frame = if default || focused || selected_tab {
            brushes.accent
        } else {
            brushes.button_border
        };
        let mut text_rect = item.rcItem;
        if pressed {
            text_rect.left += 1;
            text_rect.top += 1;
        }
        let mut text = window_text(item.hwndItem)
            .encode_utf16()
            .collect::<Vec<_>>();
        // SAFETY: WM_DRAWITEM provides a live HDC and item rectangle for this
        // synchronous draw. The window text buffer remains live until DrawTextW
        // returns, and all brushes are held by `self` for the App lifetime.
        unsafe {
            let _ = FillRect(item.hDC, &item.rcItem, fill);
            let _ = FrameRect(item.hDC, &item.rcItem, frame);
            let _ = SetBkMode(item.hDC, TRANSPARENT);
            let _ = SetTextColor(
                item.hDC,
                if disabled {
                    self.disabled_ink()
                } else {
                    self.ink()
                },
            );
            let _ = DrawTextW(
                item.hDC,
                &mut text,
                &mut text_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if focused {
                let mut focus_rect = item.rcItem;
                focus_rect.left += 4;
                focus_rect.top += 4;
                focus_rect.right -= 4;
                focus_rect.bottom -= 4;
                let _ = FrameRect(item.hDC, &focus_rect, brushes.accent);
            }
        }
        true
    }
}

#[derive(Debug)]
struct DictionaryControls {
    entries_panel: HWND,
    io_panel: HWND,
    list: HWND,
    reading: HWND,
    surface: HWND,
    part_of_speech: HWND,
    comment: HWND,
    add: HWND,
    update: HWND,
    delete: HWND,
    path: HWND,
    format: HWND,
    import_mode: HWND,
    import: HWND,
    export: HWND,
}

#[derive(Debug)]
struct LearningControls {
    history_panel: HWND,
    operations_panel: HWND,
    list: HWND,
    export_path: HWND,
    refresh: HWND,
    export: HWND,
    clear: HWND,
}

#[derive(Debug)]
struct DiagnosticsControls {
    text: HWND,
    refresh: HWND,
    clear: HWND,
}

#[derive(Debug)]
struct UpdateControls {
    settings_panel: HWND,
    available_panel: HWND,
    status_panel: HWND,
    enabled: HWND,
    save: HWND,
    check: HWND,
    apply: HWND,
    result: HWND,
}

#[derive(Debug, Clone, Copy)]
enum UpdateOperation {
    Check,
    Apply,
}

#[derive(Debug)]
enum UpdateCompletion {
    Check(updater::UpdateCheckOutcome),
    Apply(updater::UpdateOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseRequest {
    Ok,
    Cancel,
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseDecision {
    Destroy,
    WaitForUpdate,
}

const fn close_decision(_request: CloseRequest, update_in_flight: bool) -> CloseDecision {
    if update_in_flight {
        CloseDecision::WaitForUpdate
    } else {
        CloseDecision::Destroy
    }
}

#[derive(Debug)]
struct App {
    window: HWND,
    panels: [HWND; PANEL_COUNT],
    navigation: [HWND; PANEL_COUNT],
    page_topics: HWND,
    input_tree: HWND,
    status: HWND,
    ok: HWND,
    cancel: HWND,
    apply: HWND,
    general: GeneralControls,
    dictionary_controls: DictionaryControls,
    learning_controls: LearningControls,
    diagnostics_controls: DiagnosticsControls,
    update_controls: UpdateControls,
    configuration_path: PathBuf,
    dictionary_path: PathBuf,
    learning_path: PathBuf,
    diagnostics_path: PathBuf,
    disconnect_path: PathBuf,
    update_preferences_path: PathBuf,
    update_paths: updater::UpdatePaths,
    configuration: ConfigurationDocument,
    dictionary: UserDictionary,
    update_preferences: updater::UpdatePreferences,
    update_in_flight: bool,
    selected_panel: usize,
    dpi: u32,
    theme: UiTheme,
    theme_apply_in_progress: bool,
}

#[derive(Debug)]
struct SettingsInstance(HANDLE);

impl SettingsInstance {
    fn acquire() -> WindowsResult<Option<Self>> {
        // SAFETY: the mutex name is a static, NUL-terminated string and the
        // handle remains owned by this guard until the UI pump exits.
        let handle = unsafe { CreateMutexW(None, false, SINGLE_INSTANCE_NAME)? };
        // `CreateMutexW` succeeds for both a new and an existing named mutex;
        // the last-error value is the only supported way to distinguish them.
        let already_exists = windows::core::Error::from_thread().code()
            == windows::core::HRESULT::from_win32(ERROR_ALREADY_EXISTS.0);
        if already_exists {
            // SAFETY: this process did not acquire the singleton slot, so the
            // temporary handle must be closed before the activation hand-off.
            unsafe {
                let _ = CloseHandle(handle);
            }
            Ok(None)
        } else {
            Ok(Some(Self(handle)))
        }
    }
}

impl Drop for SettingsInstance {
    fn drop(&mut self) {
        // SAFETY: the guard owns this valid mutex handle for its whole drop
        // lifetime; closing it releases the slot after the message pump ends.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn activate_existing_window() -> bool {
    const ACTIVATION_ATTEMPTS: usize = 50;
    const ACTIVATION_WAIT: Duration = Duration::from_millis(10);

    for _ in 0..ACTIVATION_ATTEMPTS {
        // SAFETY: both class and title pointers are static; a successful HWND
        // remains valid for the synchronous activation calls below.
        if let Ok(window) = unsafe { FindWindowW(WINDOW_CLASS, PCWSTR::null()) } {
            // SAFETY: the HWND was returned by User32 and all operations are
            // scalar window-manager calls on that live top-level window.
            unsafe {
                if IsIconic(window).as_bool() {
                    let _ = ShowWindow(window, SW_RESTORE);
                } else {
                    let _ = ShowWindow(window, SW_SHOW);
                }
                let _ = BringWindowToTop(window);
                let _ = SetForegroundWindow(window);
            }
            return true;
        }
        sleep(ACTIVATION_WAIT);
    }
    false
}

pub fn run() -> Result<(), String> {
    let Some(_instance) = SettingsInstance::acquire().map_err(display)? else {
        // A second launch is an activation request, not a second settings
        // document. The first process may still be between mutex creation and
        // window creation, so give its UI thread a short bounded hand-off
        // window before returning.
        let _ = activate_existing_window();
        return Ok(());
    };
    // SAFETY: process DPI awareness is selected before creating any window.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    register_window_class().map_err(display)?;
    let window = create_main_window().map_err(display)?;
    // Read now, while the settings window is alive: after the pump it is gone
    // and the hidden owner would have nothing left to be found through.
    // Destroying an owner destroys what it owns, so this cannot be done from
    // the settings window's own `WM_DESTROY`.
    //
    // SAFETY: a live top-level window; the call only reads a relationship.
    let owner = unsafe { GetWindow(window, GW_OWNER) }.ok();
    let _window_icons = apply_window_icons(window);
    let app = App::new(window)?;
    let app = Box::into_raw(Box::new(app));
    // SAFETY: the boxed state remains alive until the message pump exits. Only
    // this UI thread reads the pointer stored on its own window.
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, app as isize);
        if (*app).update_preferences.enabled {
            if let Err(error) = (*app).start_update(UpdateOperation::Check) {
                (*app).set_status(&format!("自動更新の確認を開始できませんでした: {error}"));
            }
        }
        let _ = ShowWindow(window, SW_SHOW);
        let _ = UpdateWindow(window);
    }
    pump(window);
    // SAFETY: WM_DESTROY has ended the pump, so no later message can access the
    // pointer. Clear user data before reclaiming the box.
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        drop(Box::from_raw(app));
    }
    if let Some(owner) = owner {
        // SAFETY: the hidden owner created alongside the settings window;
        // nothing it owns is left by the time the pump has returned.
        unsafe {
            let _ = DestroyWindow(owner);
        }
    }
    Ok(())
}

pub fn show_fatal_error(message: &str) {
    message_box(None, message, "Sakura Input", MB_OK | MB_ICONERROR);
}

impl App {
    fn new(window: HWND) -> Result<Self, String> {
        let configuration_path = paths::configuration().map_err(display)?;
        let dictionary_path = paths::user_dictionary().map_err(display)?;
        let learning_path = paths::learning().map_err(display)?;
        let diagnostics_path = paths::timeout_diagnostics().map_err(display)?;
        let disconnect_path = paths::disconnect_diagnostics().map_err(display)?;
        let update_preferences_path = paths::update_preferences().map_err(display)?;
        let update_paths = updater::UpdatePaths {
            installer: paths::update_installer().map_err(display)?,
            log: paths::update_log().map_err(display)?,
        };
        let configuration = ConfigurationDocument::load(&configuration_path).map_err(display)?;
        let theme = UiTheme::resolve(configuration.preferences.appearance_theme);
        let dictionary = user_dictionary::load(&dictionary_path).map_err(display)?;
        let update_preferences =
            updater::UpdatePreferences::load(&update_preferences_path).map_err(display)?;

        // Keep the familiar property-sheet shell deliberately compact: the
        // current configuration is first, categories are a stable left rail,
        // and every page owns the framed controls on the right.
        label(window, "現在の設定", 12, 14, 74, 22).map_err(display)?;
        label(
            window,
            "既定の入力設定（アプリ別の設定は各アプリの入力コンテキスト作成時に適用されます）",
            92,
            14,
            510,
            22,
        )
        .map_err(display)?;
        // Native buttons retain their standard keyboard/UIA behavior while the
        // owner-draw dark palette gives the row a compact tab-strip treatment.
        let navigation = [
            button(window, "入力・変換", 15, 42, 120, 26, false).map_err(display)?,
            button(window, "辞書", 136, 42, 120, 26, false).map_err(display)?,
            button(window, "学習", 257, 42, 120, 26, false).map_err(display)?,
            button(window, "診断", 378, 42, 120, 26, false).map_err(display)?,
            button(window, "更新", 499, 42, 120, 26, false).map_err(display)?,
        ];
        label(
            window,
            "設定項目",
            INPUT_TREE_LEFT,
            PANEL_TOP - 2,
            INPUT_TREE_WIDTH,
            18,
        )
        .map_err(display)?;
        let page_topics = listbox(
            window,
            INPUT_TREE_LEFT,
            INPUT_TREE_TOP,
            NAVIGATION_WIDTH,
            INPUT_TREE_HEIGHT,
        )
        .map_err(display)?;
        let input_tree = input_topic_tree(window).map_err(display)?;
        let panels = [
            panel(window).map_err(display)?,
            panel(window).map_err(display)?,
            panel(window).map_err(display)?,
            panel(window).map_err(display)?,
            panel(window).map_err(display)?,
        ];
        let status =
            label(window, "変更は［適用］で保存されます。", 12, 390, 365, 20).map_err(display)?;
        let ok = button(window, "OK", 384, BOTTOM_ACTION_Y, 68, 26, true).map_err(display)?;
        let cancel =
            button(window, "キャンセル", 458, BOTTOM_ACTION_Y, 72, 26, false).map_err(display)?;
        let apply = button(window, "適用", 536, BOTTOM_ACTION_Y, 68, 26, false).map_err(display)?;

        let general = create_general_controls(panels[0]).map_err(display)?;
        let dictionary_controls = create_dictionary_controls(panels[1]).map_err(display)?;
        let learning_controls = create_learning_controls(panels[2]).map_err(display)?;
        let diagnostics_controls = create_diagnostics_controls(panels[3]).map_err(display)?;
        let update_controls = create_update_controls(panels[4]).map_err(display)?;

        let mut app = Self {
            window,
            panels,
            navigation,
            page_topics,
            input_tree,
            status,
            ok,
            cancel,
            apply,
            general,
            dictionary_controls,
            learning_controls,
            diagnostics_controls,
            update_controls,
            configuration_path,
            dictionary_path,
            learning_path,
            diagnostics_path,
            disconnect_path,
            update_preferences_path,
            update_paths,
            configuration,
            dictionary,
            update_preferences,
            update_in_flight: false,
            selected_panel: 0,
            dpi: window_dpi(window),
            theme,
            theme_apply_in_progress: false,
        };
        app.apply_theme();
        app.populate_input_tree();
        app.show_page_controls(0);
        app.populate_general();
        app.populate_dictionary();
        app.refresh_learning()?;
        app.refresh_diagnostics()?;
        app.populate_updates();
        Ok(app)
    }

    fn handle_command(&mut self, source: HWND, notification: u16) -> Result<(), String> {
        if let Some(index) = self.navigation.iter().position(|button| *button == source) {
            self.show_panel(index);
            return Ok(());
        }
        if source == self.apply {
            return self.save_global_settings();
        }
        if source == self.ok {
            self.save_global_settings()?;
            self.request_close(CloseRequest::Ok);
            return Ok(());
        }
        if source == self.cancel {
            // Some pages intentionally perform their own transactional actions
            // immediately. Cancel therefore closes without claiming to roll
            // those completed operations back.
            self.request_close(CloseRequest::Cancel);
            return Ok(());
        }
        if source == self.general.appearance && notification == CBN_SELCHANGE as u16 {
            self.preview_appearance()?;
            return Ok(());
        }
        if source == self.general.ai_provider && notification == CBN_SELCHANGE as u16 {
            let provider = self.selected_ai_provider()?;
            set_text(self.general.ai_endpoint, provider.default_endpoint());
            select_combo(
                self.general.ai_auth,
                enum_index(&AiAuth::ALL, provider.default_auth()),
            );
            self.refresh_ai_provider_controls(provider);
            return Ok(());
        }
        if source == self.general.ai_auth && notification == CBN_SELCHANGE as u16 {
            let provider = self.selected_ai_provider()?;
            self.refresh_ai_provider_controls(provider);
            return Ok(());
        }
        if source == self.general.ai_api_key_clear {
            if confirm("保存済みのAI APIキーを削除しますか？") {
                user_preferences::clear_api_key().map_err(display)?;
                set_text(self.general.ai_api_key, "");
                self.refresh_ai_provider_controls(self.selected_ai_provider()?);
                self.set_status("保存済みのAI APIキーを削除しました。");
            }
            return Ok(());
        }
        if source == self.general.notation_style && notification == CBN_SELCHANGE as u16 {
            self.apply_notation_style()?;
            return Ok(());
        }
        if notification == CBN_SELCHANGE as u16
            && notation_style_inputs(&self.general).contains(&source)
        {
            self.sync_notation_style();
            return Ok(());
        }
        if source == self.general.normalizer_reset {
            self.reset_normalizer_controls();
            return Ok(());
        }
        if source == self.general.input_support_enabled {
            self.sync_input_support_enabled_state();
            return Ok(());
        }
        if source == self.general.input_support_reset {
            self.configuration.preferences.input_support = InputSupport::default();
            self.load_input_support_controls();
            self.set_status("入力支援の初期値に戻しました。");
            return Ok(());
        }
        if source == self.page_topics && notification == LBN_SELCHANGE as u16 {
            self.show_topic_controls(list_index(self.page_topics).unwrap_or(0));
            return Ok(());
        }
        if source == self.general.profile_save {
            return self.save_profile();
        }
        if source == self.general.profile_delete {
            return self.delete_profile();
        }
        if source == self.general.profile_list && notification == LBN_SELCHANGE as u16 {
            self.select_profile();
            return Ok(());
        }
        if source == self.dictionary_controls.list && notification == LBN_SELCHANGE as u16 {
            self.select_dictionary_entry();
            return Ok(());
        }
        if source == self.dictionary_controls.add {
            return self.add_dictionary_entry();
        }
        if source == self.dictionary_controls.update {
            return self.update_dictionary_entry();
        }
        if source == self.dictionary_controls.delete {
            return self.delete_dictionary_entry();
        }
        if source == self.dictionary_controls.import {
            return self.import_dictionary();
        }
        if source == self.dictionary_controls.export {
            return self.export_dictionary();
        }
        if source == self.learning_controls.refresh {
            return self.refresh_learning();
        }
        if source == self.learning_controls.export {
            return self.export_learning();
        }
        if source == self.learning_controls.clear {
            return self.clear_learning();
        }
        if source == self.diagnostics_controls.refresh {
            return self.refresh_diagnostics();
        }
        if source == self.diagnostics_controls.clear {
            diagnostics::clear_timeouts(&self.diagnostics_path).map_err(display)?;
            diagnostics::clear_disconnects(&self.disconnect_path).map_err(display)?;
            self.refresh_diagnostics()?;
            self.set_status("IPC タイムアウトと engine 再接続の診断情報を消去しました。");
        }
        if source == self.update_controls.save {
            return self.save_update_preference();
        }
        if source == self.update_controls.check {
            return self.start_update(UpdateOperation::Check);
        }
        if source == self.update_controls.apply {
            if !confirm(
                "最新の署名済み Sakura Input インストーラーをダウンロード、検証して管理者承認のもと実行しますか？",
            ) {
                self.set_status("更新のインストールを取り消しました。");
                return Ok(());
            }
            return self.start_update(UpdateOperation::Apply);
        }
        Ok(())
    }

    fn show_panel(&mut self, selected: usize) {
        self.show_page_controls(selected);
        // SAFETY: navigation controls are live children. Repainting only exposes
        // the selected-tab visual state; it does not modify focus or command flow.
        unsafe {
            for button in self.navigation {
                let _ = InvalidateRect(Some(button), None, true);
            }
        }
    }

    /// Synchronize the first frame and later tab changes through one visibility
    /// boundary. This avoids relying on a theme change or first user selection
    /// to reveal controls that are already part of the selected page.
    fn show_page_controls(&mut self, selected: usize) {
        for (index, panel) in self.panels.iter().enumerate() {
            // SAFETY: every handle is a live child owned by the main window.
            unsafe {
                let _ = ShowWindow(*panel, if index == selected { SW_SHOW } else { SW_HIDE });
            }
        }
        self.selected_panel = selected;
        self.populate_topic_list(selected);
        // The input/conversion page uses a real hierarchical TreeView that
        // mirrors the ATOK property-sheet rail. Other pages retain the compact
        // topic list because their categories are intentionally flat.
        // SAFETY: the tree and list handles are live children owned by the root.
        unsafe {
            let _ = ShowWindow(
                self.input_tree,
                if selected == 0 { SW_SHOW } else { SW_HIDE },
            );
            let _ = ShowWindow(
                self.page_topics,
                if selected == 0 { SW_HIDE } else { SW_SHOW },
            );
        }
        self.show_topic_controls(0);
        // SAFETY: these controls are permanent children of the main window or
        // the selected panel. Showing them here makes the initial Light frame
        // follow exactly the same path as a later tab/theme interaction.
        unsafe {
            let _ = ShowWindow(self.ok, SW_SHOW);
            let _ = ShowWindow(self.cancel, SW_SHOW);
            let _ = ShowWindow(self.apply, SW_SHOW);
        }
        debug_assert!(selected != 0 || has_visible_style(self.general.basic_panel));
        debug_assert!(has_visible_style(self.apply));
    }

    fn populate_topic_list(&self, selected: usize) {
        reset_list(self.page_topics);
        for topic in topics_for_panel(selected) {
            add_list(self.page_topics, topic);
        }
        select_list(self.page_topics, 0);
    }

    fn populate_input_tree(&self) {
        let basics = insert_input_tree_item(
            self.input_tree,
            Default::default(),
            INPUT_TREE_LABELS[0],
            INPUT_TOPIC_BASIC,
            false,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            Default::default(),
            INPUT_TREE_LABELS[1],
            INPUT_TOPIC_INPUT_ASSIST,
            false,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            Default::default(),
            INPUT_TREE_LABELS[2],
            INPUT_TOPIC_AI_TEXT,
            false,
        );
        let conversion_assist = insert_input_tree_item(
            self.input_tree,
            Default::default(),
            INPUT_TREE_LABELS[3],
            TREE_GROUP,
            true,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            conversion_assist,
            INPUT_TREE_LABELS[4],
            INPUT_TOPIC_SEGMENT,
            false,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            conversion_assist,
            INPUT_TREE_LABELS[5],
            INPUT_TOPIC_NORMALIZER,
            false,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            Default::default(),
            INPUT_TREE_LABELS[6],
            INPUT_TOPIC_DISPLAY,
            false,
        );
        let input_support = insert_input_tree_item(
            self.input_tree,
            Default::default(),
            INPUT_TREE_LABELS[7],
            TREE_GROUP,
            true,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            input_support,
            INPUT_TREE_LABELS[8],
            INPUT_TOPIC_INPUT_REPAIR,
            false,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            input_support,
            INPUT_TREE_LABELS[9],
            INPUT_TOPIC_INPUT_SYMBOL,
            false,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            Default::default(),
            INPUT_TREE_LABELS[10],
            INPUT_TOPIC_PREDICTION,
            false,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            Default::default(),
            INPUT_TREE_LABELS[11],
            INPUT_TOPIC_ASSOCIATION,
            false,
        );
        let _ = insert_input_tree_item(
            self.input_tree,
            Default::default(),
            INPUT_TREE_LABELS[12],
            INPUT_TOPIC_PROFILE,
            false,
        );
        expand_input_tree_item(self.input_tree, conversion_assist);
        expand_input_tree_item(self.input_tree, input_support);
        select_input_tree_item(self.input_tree, basics);
    }

    /// The settings tree is a real navigation control, not a decorative index:
    /// when the user selects a topic, exactly that topic's page remains visible.
    /// Each page owns a bounded set of nested topic panels so switching the left
    /// rail never leaves controls from the previous topic in the tab order.
    fn show_topic_controls(&self, topic: usize) {
        // SAFETY: all handles are live child pages. Toggling visibility preserves
        // control state and command routes without recreating native controls.
        unsafe {
            match self.selected_panel {
                0 => {
                    let _ = ShowWindow(
                        self.general.basic_panel,
                        if topic == INPUT_TOPIC_BASIC {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.input_assist_panel,
                        if topic == INPUT_TOPIC_INPUT_ASSIST {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.ai_text_panel,
                        if topic == INPUT_TOPIC_AI_TEXT {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.profile_panel,
                        if topic == INPUT_TOPIC_PROFILE {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.segment_panel,
                        if topic == INPUT_TOPIC_SEGMENT {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.normalizer_panel,
                        if topic == INPUT_TOPIC_NORMALIZER {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.prediction_panel,
                        if topic == INPUT_TOPIC_PREDICTION {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.association_panel,
                        if topic == INPUT_TOPIC_ASSOCIATION {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.input_repair_panel,
                        if topic == INPUT_TOPIC_INPUT_REPAIR {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.input_symbol_panel,
                        if topic == INPUT_TOPIC_INPUT_SYMBOL {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    let _ = ShowWindow(
                        self.general.display_panel,
                        if topic == INPUT_TOPIC_DISPLAY {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                }
                1 => {
                    let _ = ShowWindow(
                        self.dictionary_controls.entries_panel,
                        if topic == 0 { SW_SHOW } else { SW_HIDE },
                    );
                    let _ = ShowWindow(
                        self.dictionary_controls.io_panel,
                        if topic == 1 { SW_SHOW } else { SW_HIDE },
                    );
                }
                2 => {
                    let _ = ShowWindow(
                        self.learning_controls.history_panel,
                        if topic == 0 { SW_SHOW } else { SW_HIDE },
                    );
                    let _ = ShowWindow(
                        self.learning_controls.operations_panel,
                        if topic == 1 { SW_SHOW } else { SW_HIDE },
                    );
                }
                4 => {
                    let _ = ShowWindow(
                        self.update_controls.settings_panel,
                        if topic == 0 { SW_SHOW } else { SW_HIDE },
                    );
                    let _ = ShowWindow(
                        self.update_controls.available_panel,
                        if topic == 1 { SW_SHOW } else { SW_HIDE },
                    );
                    let _ = ShowWindow(
                        self.update_controls.status_panel,
                        if topic == 2 { SW_SHOW } else { SW_HIDE },
                    );
                }
                _ => {}
            }
        }
    }

    fn populate_general(&mut self) {
        select_combo(
            self.general.keymap,
            match self.configuration.preferences.keymap_preset {
                Preset::MsIme => 0,
                Preset::Atok => 1,
            },
        );
        set_checked(
            self.general.input_method_romaji,
            self.configuration.preferences.input_method == InputMethod::Romaji,
        );
        set_checked(
            self.general.input_method_kana,
            self.configuration.preferences.input_method == InputMethod::Kana,
        );
        select_combo(
            self.general.default_mode,
            mode_index(self.configuration.preferences.default_mode),
        );
        select_combo(
            self.general.pad_shortcut,
            pad_shortcut_index(self.configuration.preferences.pad_shortcut),
        );
        select_combo(
            self.general.conversion_assist_method,
            conversion_method_index(self.configuration.preferences.conversion_method),
        );
        select_combo(
            self.general.input_assist_space_width,
            space_width_index(self.configuration.preferences.space_width),
        );
        select_combo(
            self.general.input_assist_shift_space,
            shift_space_behavior_index(self.configuration.preferences.shift_space_behavior),
        );
        select_combo(
            self.general.ai_text_key,
            ai_text_key_index(user_preferences::read_ai_text_key()),
        );
        let ai = user_preferences::read_ai_text_preferences();
        let provider_index = self
            .general
            .ai_providers
            .iter()
            .position(|candidate| *candidate == ai.provider)
            .unwrap_or(0);
        select_combo(self.general.ai_provider, provider_index);
        set_text(self.general.ai_endpoint, &ai.endpoint);
        select_combo(self.general.ai_auth, enum_index(&AiAuth::ALL, ai.auth));
        set_text(self.general.ai_api_key, "");
        select_combo(self.general.ai_style, enum_index(&AiStyle::ALL, ai.style));
        select_combo(
            self.general.ai_effort,
            enum_index(&AiEffort::ALL, ai.effort),
        );
        select_combo(
            self.general.ai_service_tier,
            enum_index(&AiServiceTier::ALL, ai.service_tier),
        );
        self.refresh_ai_provider_controls(ai.provider);
        set_checked(
            self.general.prediction,
            self.configuration.preferences.prediction_enabled,
        );
        set_checked(
            self.general.association,
            self.configuration.preferences.association_enabled,
        );
        self.load_input_support_controls();
        select_combo(
            self.general.suggest,
            suggest_index(self.configuration.preferences.suggest_accept),
        );
        select_combo(
            self.general.appearance,
            appearance_index(self.configuration.preferences.appearance_theme),
        );
        select_combo(
            self.general.neural_reranker_scope,
            neural_reranker_scope_index(self.configuration.preferences.neural_reranker_scope),
        );
        select_combo(
            self.general.normalizer_alnum,
            width_index(self.configuration.preferences.normalizer.width.alnum),
        );
        select_combo(
            self.general.normalizer_number,
            width_index(self.configuration.preferences.normalizer.width.number),
        );
        select_combo(
            self.general.normalizer_symbol,
            width_index(self.configuration.preferences.normalizer.width.symbol),
        );
        select_combo(
            self.general.punctuation_period,
            punctuation_period_index(self.configuration.preferences.normalizer.punctuation),
        );
        select_combo(
            self.general.punctuation_comma,
            punctuation_comma_index(self.configuration.preferences.normalizer.punctuation),
        );
        select_combo(
            self.general.punctuation_brackets,
            bracket_style_index(self.configuration.preferences.normalizer.brackets),
        );
        self.sync_notation_style();
        self.populate_profile_list();
    }

    fn populate_profile_list(&self) {
        reset_list(self.general.profile_list);
        for profile in &self.configuration.profiles {
            add_list(
                self.general.profile_list,
                &format!(
                    "{}  —  {}、予測{}",
                    profile.process_name,
                    mode_label(profile.default_mode),
                    if profile.prediction_enabled {
                        "あり"
                    } else {
                        "なし"
                    }
                ),
            );
        }
    }

    fn save_global_settings(&mut self) -> Result<(), String> {
        self.configuration.preferences.keymap_preset = match combo_index(self.general.keymap) {
            Some(0) => Preset::MsIme,
            Some(1) => Preset::Atok,
            _ => return Err("キー設定を選択してください。".to_owned()),
        };
        self.configuration.preferences.input_method = input_method_from_checks(
            is_checked(self.general.input_method_romaji),
            is_checked(self.general.input_method_kana),
        )?;
        self.configuration.preferences.default_mode =
            mode_from_index(combo_index(self.general.default_mode))?;
        self.configuration.preferences.pad_shortcut =
            pad_shortcut_from_index(combo_index(self.general.pad_shortcut))?;
        self.configuration.preferences.conversion_method =
            conversion_method_from_index(combo_index(self.general.conversion_assist_method))?;
        self.configuration.preferences.prediction_enabled = is_checked(self.general.prediction);
        self.configuration.preferences.association_enabled = is_checked(self.general.association);
        self.save_input_support_controls();
        self.configuration.preferences.suggest_accept =
            suggest_from_index(combo_index(self.general.suggest))?;
        self.configuration.preferences.appearance_theme =
            appearance_from_index(combo_index(self.general.appearance))?;
        self.configuration.preferences.neural_reranker_scope =
            neural_reranker_scope_from_index(combo_index(self.general.neural_reranker_scope))?;
        self.configuration.preferences.space_width =
            space_width_from_index(combo_index(self.general.input_assist_space_width))?;
        self.configuration.preferences.shift_space_behavior =
            shift_space_behavior_from_index(combo_index(self.general.input_assist_shift_space))?;
        let ai_text_key = ai_text_key_from_index(combo_index(self.general.ai_text_key))?;
        let ai_provider = self.selected_ai_provider()?;
        let ai_endpoint = window_text(self.general.ai_endpoint).trim().to_owned();
        validate_ai_endpoint(ai_provider, &ai_endpoint)?;
        let ai_preferences = AiTextPreferences {
            provider: ai_provider,
            endpoint: if ai_provider == AiProvider::ChatGptCodex {
                String::new()
            } else {
                ai_endpoint
            },
            auth: enum_from_index(&AiAuth::ALL, combo_index(self.general.ai_auth), "認証")?,
            style: enum_from_index(
                &AiStyle::ALL,
                combo_index(self.general.ai_style),
                "変換スタイル",
            )?,
            effort: enum_from_index(
                &AiEffort::ALL,
                combo_index(self.general.ai_effort),
                "Effort",
            )?,
            service_tier: enum_from_index(
                &AiServiceTier::ALL,
                combo_index(self.general.ai_service_tier),
                "Tier",
            )?,
        };
        self.configuration.preferences.normalizer = self.normalizer_from_controls()?;
        self.configuration
            .save(&self.configuration_path)
            .map_err(display)?;
        user_preferences::write_ai_text_key(ai_text_key).map_err(display)?;
        user_preferences::write_ai_text_preferences(&ai_preferences).map_err(display)?;
        let mut api_key = window_text(self.general.ai_api_key);
        if !api_key.trim().is_empty() {
            user_preferences::write_api_key(&api_key).map_err(display)?;
            // The credential API has copied the value. Erase the edit control
            // and this temporary buffer instead of retaining a second copy.
            set_text(self.general.ai_api_key, "");
            // SAFETY: zero is valid UTF-8, and the String is not observed until
            // it is dropped immediately after this block.
            unsafe { api_key.as_bytes_mut() }.fill(0);
            api_key.clear();
        }
        self.refresh_ai_provider_controls(ai_provider);
        self.set_status("既定の設定を保存しました。");
        Ok(())
    }

    fn selected_ai_provider(&self) -> Result<AiProvider, String> {
        self.general
            .ai_providers
            .get(
                combo_index(self.general.ai_provider)
                    .ok_or_else(|| "AIプロバイダーを選択してください。".to_owned())?,
            )
            .copied()
            .ok_or_else(|| "AIプロバイダーの値が不正です。".to_owned())
    }

    fn load_input_support_controls(&self) {
        let support = self.configuration.preferences.input_support;
        set_checked(self.general.input_support_enabled, support.enabled);
        set_checked(
            self.general.input_support_commit_based,
            support.commit_based,
        );
        set_checked(self.general.input_support_advanced, support.advanced);
        set_checked(self.general.input_support_vowel_count, support.vowel_count);
        set_checked(
            self.general.input_support_consonant_extra,
            support.consonant_extra,
        );
        set_checked(self.general.input_support_n_count, support.n_count);
        set_checked(
            self.general.input_support_dakuten_swap,
            support.dakuten_swap,
        );
        set_checked(self.general.input_support_tsu_sokuon, support.tsu_sokuon);
        set_checked(self.general.input_support_wa_wo, support.wa_wo);
        set_checked(self.general.input_support_small_u, support.small_u);
        set_checked(
            self.general.input_support_fuzzy_proper_nouns,
            support.fuzzy_proper_nouns,
        );
        set_checked(
            self.general.input_support_english_to_katakana,
            support.english_to_katakana,
        );
        set_checked(
            self.general.input_support_period_after_digit,
            support.period_after_digit,
        );
        set_checked(
            self.general.input_support_comma_after_digit,
            support.comma_after_digit,
        );
        set_checked(
            self.general.input_support_middle_dot_after_digit,
            support.middle_dot_after_digit,
        );
        set_checked(
            self.general.input_support_long_vowel_after_alnum,
            support.long_vowel_after_alnum,
        );
        self.sync_input_support_enabled_state();
    }

    fn save_input_support_controls(&mut self) {
        let support = InputSupport {
            enabled: is_checked(self.general.input_support_enabled),
            commit_based: is_checked(self.general.input_support_commit_based),
            advanced: is_checked(self.general.input_support_advanced),
            vowel_count: is_checked(self.general.input_support_vowel_count),
            consonant_extra: is_checked(self.general.input_support_consonant_extra),
            n_count: is_checked(self.general.input_support_n_count),
            dakuten_swap: is_checked(self.general.input_support_dakuten_swap),
            tsu_sokuon: is_checked(self.general.input_support_tsu_sokuon),
            wa_wo: is_checked(self.general.input_support_wa_wo),
            small_u: is_checked(self.general.input_support_small_u),
            fuzzy_proper_nouns: is_checked(self.general.input_support_fuzzy_proper_nouns),
            english_to_katakana: is_checked(self.general.input_support_english_to_katakana),
            period_after_digit: is_checked(self.general.input_support_period_after_digit),
            comma_after_digit: is_checked(self.general.input_support_comma_after_digit),
            middle_dot_after_digit: is_checked(self.general.input_support_middle_dot_after_digit),
            long_vowel_after_alnum: is_checked(self.general.input_support_long_vowel_after_alnum),
        };
        self.configuration.preferences.input_support = support;
    }

    fn sync_input_support_enabled_state(&self) {
        let enabled = is_checked(self.general.input_support_enabled);
        // SAFETY: every HWND belongs to this live settings window.
        unsafe {
            for window in [
                self.general.input_support_commit_based,
                self.general.input_support_advanced,
                self.general.input_support_vowel_count,
                self.general.input_support_consonant_extra,
                self.general.input_support_n_count,
                self.general.input_support_dakuten_swap,
                self.general.input_support_tsu_sokuon,
                self.general.input_support_wa_wo,
                self.general.input_support_small_u,
                self.general.input_support_fuzzy_proper_nouns,
                self.general.input_support_english_to_katakana,
                self.general.input_support_period_after_digit,
                self.general.input_support_comma_after_digit,
                self.general.input_support_middle_dot_after_digit,
                self.general.input_support_long_vowel_after_alnum,
            ] {
                let _ = EnableWindow(window, enabled);
            }
        }
    }

    fn refresh_ai_provider_controls(&self, provider: AiProvider) {
        let subscription = provider == AiProvider::ChatGptCodex;
        let auth = enum_from_index(&AiAuth::ALL, combo_index(self.general.ai_auth), "認証")
            .unwrap_or_default();
        // SAFETY: every HWND belongs to this live settings window.
        unsafe {
            let _ = EnableWindow(self.general.ai_endpoint, !subscription);
            let _ = EnableWindow(self.general.ai_auth, !subscription);
            let _ = EnableWindow(
                self.general.ai_api_key,
                !subscription && auth != AiAuth::None,
            );
            let _ = EnableWindow(
                self.general.ai_api_key_clear,
                !subscription && user_preferences::api_key_is_saved(),
            );
            let _ = EnableWindow(self.general.ai_service_tier, !subscription);
        }
        let status = if subscription {
            if codex_cli_available() {
                "Codex CLIのChatGPTログインを使用します（APIキー不要）。"
            } else {
                "Codex CLIが見つかりません。インストール後に利用できます。"
            }
        } else if auth == AiAuth::None {
            "この設定ではAPIキーを送信しません。"
        } else if user_preferences::api_key_is_saved() {
            "APIキーはWindows資格情報マネージャーに保存済みです。"
        } else {
            "APIキーは未設定です。空欄のまま保存しても既存値は変更しません。"
        };
        set_text(self.general.ai_api_key_status, status);
    }

    /// Reads the six 文字幅・句読点 controls back into a `Normalizer`.
    ///
    /// Shared by save and by the notation preset's readout, so the value that
    /// gets written and the value the preset reports can never disagree.
    fn normalizer_from_controls(&self) -> Result<Normalizer, String> {
        let mut normalizer = self.configuration.preferences.normalizer;
        normalizer.width.alnum = width_from_index(combo_index(self.general.normalizer_alnum))?;
        normalizer.width.number = width_from_index(combo_index(self.general.normalizer_number))?;
        normalizer.width.symbol = width_from_index(combo_index(self.general.normalizer_symbol))?;
        normalizer.punctuation = punctuation_from_indices(
            combo_index(self.general.punctuation_period),
            combo_index(self.general.punctuation_comma),
        )?;
        normalizer.brackets =
            bracket_style_from_index(combo_index(self.general.punctuation_brackets))?;
        Ok(normalizer)
    }

    /// The style the seven controls currently spell, or `None` for a mix no
    /// shipped style produces.
    fn notation_style_from_controls(&self) -> Option<NotationStyle> {
        let mut probe = self.configuration.preferences;
        probe.normalizer = self.normalizer_from_controls().ok()?;
        probe.space_width =
            space_width_from_index(combo_index(self.general.input_assist_space_width)).ok()?;
        NotationStyle::of(&probe)
    }

    /// Moves the preset combo to match the controls.
    ///
    /// Deliberately silent. This runs on every edit of those seven controls,
    /// so a status line here would keep overwriting whatever the user was
    /// reading — including the message `apply_notation_style` just set.
    fn sync_notation_style(&self) {
        select_combo(
            self.general.notation_style,
            notation_style_index(self.notation_style_from_controls()),
        );
    }

    /// Writes the picked style into the seven controls it owns.
    ///
    /// Nothing is persisted here: like every other control on the page these
    /// are staged until 適用. One of the seven — space width — lives on the
    /// 入力補助 page, which the user is not looking at, so the status line
    /// names it rather than letting the preset change a page out of sight.
    fn apply_notation_style(&mut self) -> Result<(), String> {
        let Some(style) = notation_style_from_index(combo_index(self.general.notation_style))?
        else {
            // `カスタム` is a readout, not a value. Put the combo back where
            // the controls say it belongs instead of writing anything.
            self.sync_notation_style();
            return Ok(());
        };
        let mut staged = self.configuration.preferences;
        style.apply_to(&mut staged);
        select_combo(
            self.general.normalizer_alnum,
            width_index(staged.normalizer.width.alnum),
        );
        select_combo(
            self.general.normalizer_number,
            width_index(staged.normalizer.width.number),
        );
        select_combo(
            self.general.normalizer_symbol,
            width_index(staged.normalizer.width.symbol),
        );
        select_combo(
            self.general.punctuation_period,
            punctuation_period_index(staged.normalizer.punctuation),
        );
        select_combo(
            self.general.punctuation_comma,
            punctuation_comma_index(staged.normalizer.punctuation),
        );
        select_combo(
            self.general.punctuation_brackets,
            bracket_style_index(staged.normalizer.brackets),
        );
        select_combo(
            self.general.input_assist_space_width,
            space_width_index(staged.space_width),
        );
        self.set_status(&format!(
            "表記スタイルを「{}」にしました。入力補助のスペース幅も「{}」になります。保存するには適用を押してください。",
            style.label(),
            space_width_label(staged.space_width),
        ));
        Ok(())
    }

    fn reset_normalizer_controls(&mut self) {
        let defaults = sakura_core::Preferences::default();
        select_combo(
            self.general.normalizer_alnum,
            width_index(defaults.normalizer.width.alnum),
        );
        select_combo(
            self.general.normalizer_number,
            width_index(defaults.normalizer.width.number),
        );
        select_combo(
            self.general.normalizer_symbol,
            width_index(defaults.normalizer.width.symbol),
        );
        select_combo(
            self.general.punctuation_period,
            punctuation_period_index(defaults.normalizer.punctuation),
        );
        select_combo(
            self.general.punctuation_comma,
            punctuation_comma_index(defaults.normalizer.punctuation),
        );
        select_combo(
            self.general.punctuation_brackets,
            bracket_style_index(defaults.normalizer.brackets),
        );
        // Reset owns this page only. Space width lives on 入力補助 and is left
        // alone, so the preset may well land on `カスタム` here — which is the
        // truthful answer, not a bug.
        self.sync_notation_style();
        self.set_status(
            "文字幅・句読点の設定を初期値に戻しました。保存するには適用を押してください。",
        );
    }

    fn preview_appearance(&mut self) -> Result<(), String> {
        let appearance = appearance_from_index(combo_index(self.general.appearance))?;
        self.theme = UiTheme::resolve(appearance);
        self.apply_theme();
        self.set_status(&format!(
            "表示をプレビューしています: {}",
            appearance_label(appearance)
        ));
        Ok(())
    }

    fn refresh_auto_appearance(&mut self) {
        if self.theme_apply_in_progress {
            return;
        }
        let Ok(appearance) = appearance_from_index(combo_index(self.general.appearance)) else {
            return;
        };
        if appearance == AppearanceTheme::Auto
            || high_contrast_enabled() != self.theme.high_contrast
        {
            self.theme = UiTheme::resolve(appearance);
            self.apply_theme();
        }
    }

    /// Reflow the fixed property-sheet grid after Windows moves it to a
    /// monitor with a different DPI.  The dialog deliberately remains a
    /// compact, non-resizable property sheet; every child therefore needs the
    /// same scale transition as the outer rectangle or Japanese labels and the
    /// bottom action row drift apart.
    fn apply_dpi_change(&mut self, new_dpi: u32, suggested: Option<RECT>) {
        let new_dpi = new_dpi.max(96);
        let old_dpi = self.dpi.max(96);
        let target = suggested.or_else(|| scaled_window_rect(self.window, old_dpi, new_dpi));

        if let Some(rect) = target {
            // SAFETY: the root HWND is owned by this UI thread and the
            // suggested rectangle is copied from the synchronous User32
            // notification (or derived from its current rectangle).
            unsafe {
                let _ = SetWindowPos(
                    self.window,
                    None,
                    rect.left,
                    rect.top,
                    rect.right.saturating_sub(rect.left),
                    rect.bottom.saturating_sub(rect.top),
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
            }
        }

        if old_dpi != new_dpi {
            scale_window_children(self.window, old_dpi, new_dpi);
            refresh_native_fonts(self.window);
            self.dpi = new_dpi;
        }
        self.apply_theme();
    }

    fn apply_theme(&mut self) {
        // SetWindowTheme may synchronously send WM_THEMECHANGED.  That nested
        // notification observes the active application and deliberately
        // no-ops, rather than recursively reapplying the same palette.
        if self.theme_apply_in_progress {
            return;
        }
        self.theme_apply_in_progress = true;
        apply_title_bar_theme(self.window, self.theme.dark && !self.theme.high_contrast);
        apply_common_control_theme(self.window, self.theme.dark && !self.theme.high_contrast);
        apply_input_tree_theme(self.input_tree, &self.theme);
        self.apply_button_styles();
        // SAFETY: the top-level window and each child remain live for the App
        // lifetime. Invalidating requests repaint only; it transfers no state.
        unsafe {
            let _ = InvalidateRect(Some(self.window), None, true);
            invalidate_child_windows(self.window);
        }
        self.theme_apply_in_progress = false;
    }

    fn is_readonly_input(&self, window: HWND) -> bool {
        window == self.diagnostics_controls.text || window == self.update_controls.result
    }

    fn buttons(&self) -> [HWND; 23] {
        [
            self.navigation[0],
            self.navigation[1],
            self.navigation[2],
            self.navigation[3],
            self.navigation[4],
            self.general.profile_save,
            self.general.profile_delete,
            self.dictionary_controls.add,
            self.dictionary_controls.update,
            self.dictionary_controls.delete,
            self.dictionary_controls.import,
            self.dictionary_controls.export,
            self.learning_controls.refresh,
            self.learning_controls.export,
            self.learning_controls.clear,
            self.diagnostics_controls.refresh,
            self.diagnostics_controls.clear,
            self.update_controls.save,
            self.update_controls.check,
            self.update_controls.apply,
            self.ok,
            self.cancel,
            self.apply,
        ]
    }

    fn apply_button_styles(&self) {
        let owner_draw = !self.theme.high_contrast;
        for button in self.buttons() {
            let default = button == self.ok;
            let type_style = button_type_style(owner_draw, default);
            // SAFETY: all controls were created on this UI thread. Changing only
            // the documented button type preserves their HWND, text, tab order,
            // command notification, and UI Automation provider. SetWindowPos
            // requests the documented non-client/style refresh after the change.
            unsafe {
                let existing = GetWindowLongPtrW(button, GWL_STYLE) as u32;
                let updated = (existing & !(BS_TYPEMASK as u32)) | type_style;
                if existing != updated {
                    SetWindowLongPtrW(button, GWL_STYLE, updated as isize);
                    let _ = SetWindowPos(
                        button,
                        None,
                        0,
                        0,
                        0,
                        0,
                        SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
        }
    }

    fn draw_button(&self, item: &DRAWITEMSTRUCT) -> bool {
        let default = item.hwndItem == self.ok;
        let selected_tab = self
            .navigation
            .get(self.selected_panel)
            .is_some_and(|button| *button == item.hwndItem);
        self.theme.draw_button(item, default, selected_tab)
    }

    fn select_profile(&self) {
        let Some(index) = list_index(self.general.profile_list) else {
            return;
        };
        let Some(profile) = self.configuration.profiles.get(index) else {
            return;
        };
        set_text(self.general.profile_process, &profile.process_name);
        select_combo(self.general.profile_mode, mode_index(profile.default_mode));
        set_checked(self.general.profile_prediction, profile.prediction_enabled);
        select_combo(
            self.general.profile_suggest,
            suggest_index(profile.suggest_accept),
        );
        select_combo(
            self.general.profile_notation,
            notation_style_index(NotationStyle::of_normalizer(&profile.normalizer)),
        );
    }

    fn save_profile(&mut self) -> Result<(), String> {
        let process_name = required_text(self.general.profile_process, "実行ファイル名")?;
        // `カスタム` is a readout, so it cannot be *chosen* as a value here:
        // it means "leave the stored normalizer as it is", which for a new
        // profile is the global one. Only a named style overwrites.
        let inherited = self
            .configuration
            .profiles
            .iter()
            .find(|profile| profile.matches(&process_name))
            .map_or(self.configuration.preferences.normalizer, |profile| {
                profile.normalizer
            });
        let normalizer = notation_style_from_index(combo_index(self.general.profile_notation))?
            .map_or(inherited, NotationStyle::normalizer);
        self.configuration
            .upsert_profile(AppProfile {
                process_name: process_name.clone(),
                default_mode: mode_from_index(combo_index(self.general.profile_mode))?,
                normalizer,
                prediction_enabled: is_checked(self.general.profile_prediction),
                suggest_accept: suggest_from_index(combo_index(self.general.profile_suggest))?,
            })
            .map_err(display)?;
        self.configuration
            .save(&self.configuration_path)
            .map_err(display)?;
        self.populate_profile_list();
        self.set_status(&format!("アプリ別設定を保存しました: {process_name}"));
        Ok(())
    }

    fn delete_profile(&mut self) -> Result<(), String> {
        let process_name = required_text(self.general.profile_process, "実行ファイル名")?;
        self.configuration
            .remove_profile(&process_name)
            .map_err(display)?;
        self.configuration
            .save(&self.configuration_path)
            .map_err(display)?;
        self.populate_profile_list();
        set_text(self.general.profile_process, "");
        self.set_status(&format!("アプリ別設定を削除しました: {process_name}"));
        Ok(())
    }

    fn populate_dictionary(&self) {
        reset_list(self.dictionary_controls.list);
        for entry in self.dictionary.entries() {
            add_list(
                self.dictionary_controls.list,
                &format!(
                    "{}  →  {}  [{}]",
                    entry.reading,
                    entry.surface,
                    entry.part_of_speech.spec().label
                ),
            );
        }
    }

    fn select_dictionary_entry(&self) {
        let Some(index) = list_index(self.dictionary_controls.list) else {
            return;
        };
        let Some(entry) = self.dictionary.entry(index) else {
            return;
        };
        set_text(self.dictionary_controls.reading, &entry.reading);
        set_text(self.dictionary_controls.surface, &entry.surface);
        set_text(self.dictionary_controls.comment, &entry.comment);
        select_combo(
            self.dictionary_controls.part_of_speech,
            UserPartOfSpeech::ALL
                .iter()
                .position(|pos| *pos == entry.part_of_speech)
                .unwrap_or(0),
        );
    }

    fn dictionary_entry_from_controls(&self) -> Result<UserDictionaryEntry, String> {
        let index = combo_index(self.dictionary_controls.part_of_speech)
            .ok_or_else(|| "品詞を選択してください。".to_owned())?;
        let part_of_speech = UserPartOfSpeech::ALL
            .get(index)
            .copied()
            .ok_or_else(|| "選択した品詞が不正です。".to_owned())?;
        Ok(UserDictionaryEntry {
            reading: required_text(self.dictionary_controls.reading, "読み")?,
            surface: required_text(self.dictionary_controls.surface, "単語")?,
            part_of_speech,
            comment: window_text(self.dictionary_controls.comment),
        })
    }

    fn add_dictionary_entry(&mut self) -> Result<(), String> {
        let entry = self.dictionary_entry_from_controls()?;
        self.dictionary = user_dictionary::add(&self.dictionary_path, entry).map_err(display)?;
        self.populate_dictionary();
        self.set_status(&format!(
            "ユーザー辞書は {} 件です。",
            self.dictionary.len()
        ));
        Ok(())
    }

    fn update_dictionary_entry(&mut self) -> Result<(), String> {
        let index = list_index(self.dictionary_controls.list)
            .ok_or_else(|| "変更する辞書項目を選択してください。".to_owned())?;
        let original = self
            .dictionary
            .entry(index)
            .cloned()
            .ok_or_else(|| "選択した辞書項目は存在しません。".to_owned())?;
        let replacement = self.dictionary_entry_from_controls()?;
        self.dictionary = user_dictionary::update(
            &self.dictionary_path,
            &original.reading,
            &original.surface,
            replacement,
        )
        .map_err(display)?;
        self.populate_dictionary();
        self.set_status("ユーザー辞書の項目を更新しました。");
        Ok(())
    }

    fn delete_dictionary_entry(&mut self) -> Result<(), String> {
        let index = list_index(self.dictionary_controls.list)
            .ok_or_else(|| "削除する辞書項目を選択してください。".to_owned())?;
        let entry = self
            .dictionary
            .entry(index)
            .cloned()
            .ok_or_else(|| "選択した辞書項目は存在しません。".to_owned())?;
        self.dictionary =
            user_dictionary::delete(&self.dictionary_path, &entry.reading, &entry.surface)
                .map_err(display)?;
        self.populate_dictionary();
        self.set_status("ユーザー辞書の項目を削除しました。");
        Ok(())
    }

    fn import_dictionary(&mut self) -> Result<(), String> {
        let source = PathBuf::from(required_text(
            self.dictionary_controls.path,
            "読み込みファイル",
        )?);
        let format = dictionary_format(combo_index(self.dictionary_controls.format), true)?;
        let mode = match combo_index(self.dictionary_controls.import_mode) {
            Some(0) => ImportMode::Merge,
            Some(1) => {
                if !confirm("既存のSakura Inputユーザー辞書を、このファイルで置き換えますか？")
                {
                    self.set_status("ユーザー辞書の置き換えを取り消しました。");
                    return Ok(());
                }
                ImportMode::Replace
            }
            _ => return Err("追加または置き換えを選択してください。".to_owned()),
        };
        let bytes = std::fs::read(&source).map_err(display)?;
        let report = user_dictionary::import(&self.dictionary_path, &bytes, format, mode)
            .map_err(display)?;
        self.dictionary = user_dictionary::load(&self.dictionary_path).map_err(display)?;
        self.populate_dictionary();
        self.set_status(&format!(
            "{}形式の辞書を{}件読み込みました（合計{}件）。",
            report.format.name(),
            report.imported,
            report.total
        ));
        Ok(())
    }

    fn export_dictionary(&self) -> Result<(), String> {
        let destination =
            PathBuf::from(required_text(self.dictionary_controls.path, "書き出し先")?);
        let format = dictionary_format(combo_index(self.dictionary_controls.format), false)?
            .ok_or_else(|| "書き出し形式を選択してください。".to_owned())?;
        let count = user_dictionary::export(&self.dictionary_path, &destination, format)
            .map_err(display)?;
        self.set_status(&format!(
            "{}件の辞書を {} に書き出しました。",
            count,
            destination.display()
        ));
        Ok(())
    }

    fn refresh_learning(&self) -> Result<(), String> {
        let snapshot = learning::view(&self.learning_path).map_err(display)?;
        reset_list(self.learning_controls.list);
        for record in snapshot.records.iter().rev().take(2_000) {
            add_list(
                self.learning_controls.list,
                &format!(
                    "#{}  {} → {}  ({} / {})",
                    record.sequence,
                    record.reading,
                    record.surface.replace(['\r', '\n'], " "),
                    record.left_context,
                    record.right_context
                ),
            );
        }
        self.set_status(&format!(
            "学習履歴: {} 件を確認しました（末尾の無効データ: {} bytes）。",
            snapshot.records.len(),
            snapshot.ignored_tail_bytes
        ));
        Ok(())
    }

    fn export_learning(&self) -> Result<(), String> {
        let destination = PathBuf::from(required_text(
            self.learning_controls.export_path,
            "書き出し先",
        )?);
        let count = learning::export(&self.learning_path, &destination).map_err(display)?;
        self.set_status(&format!(
            "学習履歴を{}件、{}に書き出しました。",
            count,
            destination.display()
        ));
        Ok(())
    }

    fn clear_learning(&self) -> Result<(), String> {
        if !confirm("学習した変換履歴と予測履歴をすべて消去しますか？") {
            self.set_status("学習履歴の消去を取り消しました。");
            return Ok(());
        }
        let route = learning::clear(&self.learning_path).map_err(display)?;
        self.refresh_learning()?;
        self.set_status(match route {
            learning::ClearRoute::LiveEngine => "実行中の engine を通じて学習履歴を消去しました。",
            learning::ClearRoute::Offline { .. } => {
                "engine が停止中のため、保存済みの学習履歴を消去しました。"
            }
        });
        Ok(())
    }

    fn refresh_diagnostics(&self) -> Result<(), String> {
        let timeouts = diagnostics::load_timeouts(&self.diagnostics_path).map_err(display)?;
        let resets = diagnostics::load_disconnects(&self.disconnect_path).map_err(display)?;
        // Both reports go in the same box, but as two separate tables. A reset
        // is usually the correct response to a host lifecycle event, so summing
        // them with the timeouts would read as a fault count and be wrong.
        let report = format!(
            "{}\n{}",
            diagnostics::render_text(&timeouts),
            diagnostics::render_disconnects_text(&resets)
        );
        set_text(
            self.diagnostics_controls.text,
            &report.replace('\n', "\r\n"),
        );
        self.set_status(&format!(
            "IPC タイムアウトの記録: {} 件、engine 再接続の記録: {} 件",
            timeouts.valid_events, resets.valid_events
        ));
        Ok(())
    }

    fn populate_updates(&self) {
        set_checked(
            self.update_controls.enabled,
            self.update_preferences.enabled,
        );
        set_text(
            self.update_controls.result,
            &format!(
                "インストール済みバージョン: {}\r\n自動更新の確認: {}\r\n\r\n無効の間は更新を確認しません。",
                updater::current_version(),
                if self.update_preferences.enabled {
                    "有効"
                } else {
                    "無効"
                }
            ),
        );
    }

    fn save_update_preference(&mut self) -> Result<(), String> {
        let was_enabled = self.update_preferences.enabled;
        let enabled = is_checked(self.update_controls.enabled);
        updater::UpdatePreferences { enabled }
            .save(&self.update_preferences_path)
            .map_err(display)?;
        self.update_preferences.enabled = enabled;
        self.populate_updates();
        self.set_status(if enabled {
            "自動更新の確認を有効にしました。"
        } else {
            "自動更新の確認を無効にしました。"
        });
        if enabled && !was_enabled {
            self.start_update(UpdateOperation::Check)?;
        }
        Ok(())
    }

    fn start_update(&mut self, operation: UpdateOperation) -> Result<(), String> {
        if self.update_in_flight {
            return Err("更新処理を実行中です。完了するまでお待ちください。".to_owned());
        }
        let window_value = self.window.0 as usize;
        let enabled = self.update_preferences.enabled;
        let update_paths = self.update_paths.clone();
        std::thread::Builder::new()
            .name("sakura-update-worker".to_owned())
            .spawn(move || {
                let window = HWND(window_value as *mut c_void);
                let completion = std::panic::catch_unwind(|| match operation {
                    UpdateOperation::Check => UpdateCompletion::Check(updater::check_real(enabled)),
                    UpdateOperation::Apply => {
                        UpdateCompletion::Apply(updater::apply_real(enabled, &update_paths))
                    }
                })
                .unwrap_or_else(|_| {
                    let failure = updater::UpdateFailure {
                        stage: updater::UpdateStage::Worker,
                        message: "更新処理が予期せず終了しました。".to_owned(),
                    };
                    match operation {
                        UpdateOperation::Check => {
                            UpdateCompletion::Check(updater::UpdateCheckOutcome::Failed(failure))
                        }
                        UpdateOperation::Apply => {
                            UpdateCompletion::Apply(updater::UpdateOutcome::Failed {
                                version: None,
                                failure,
                            })
                        }
                    }
                });
                let pointer = Box::into_raw(Box::new(completion));
                // SAFETY: ownership of `pointer` transfers to the UI thread if
                // the post succeeds. A failed post reclaims it here.
                if unsafe {
                    PostMessageW(
                        Some(window),
                        WM_UPDATE_COMPLETE,
                        WPARAM(pointer as usize),
                        LPARAM(0),
                    )
                }
                .is_err()
                {
                    // SAFETY: PostMessage failed, so no other thread can own or
                    // observe this allocation.
                    unsafe {
                        drop(Box::from_raw(pointer));
                    }
                }
            })
            .map_err(|error| format!("更新処理を開始できませんでした: {error}"))?;
        self.update_in_flight = true;
        let message = match operation {
            UpdateOperation::Check => "署名済みリリースの情報を確認しています…",
            UpdateOperation::Apply => {
                "署名済みインストーラーをダウンロードして検証しています。終了しないでください…"
            }
        };
        set_text(self.update_controls.result, message);
        self.set_status(message);
        Ok(())
    }

    fn finish_update(&mut self, completion: UpdateCompletion) {
        self.update_in_flight = false;
        let available_version = match &completion {
            UpdateCompletion::Check(updater::UpdateCheckOutcome::Available(manifest)) => {
                Some(manifest.version)
            }
            _ => None,
        };
        let (message, failed) = match completion {
            UpdateCompletion::Check(outcome) => {
                let failed = matches!(outcome, updater::UpdateCheckOutcome::Failed(_));
                (Self::describe_update_check(&outcome), failed)
            }
            UpdateCompletion::Apply(outcome) => {
                let failed = outcome.is_failure();
                (Self::describe_update(&outcome), failed)
            }
        };
        set_text(self.update_controls.result, &message.replace('\n', "\r\n"));
        self.set_status(&message.replace(['\r', '\n'], " "));
        if failed {
            message_box(
                Some(self.window),
                &message,
                "Sakura Input の更新",
                MB_OK | MB_ICONERROR,
            );
        }
        if let Some(version) = available_version {
            let prompt = Self::update_available_prompt(version);
            if message_box(
                Some(self.window),
                &prompt,
                "Sakura Input の更新",
                MB_YESNO | MB_ICONINFORMATION,
            ) == IDYES
            {
                if let Err(error) = self.start_update(UpdateOperation::Apply) {
                    let message = format!("更新のインストールを開始できませんでした: {error}");
                    self.set_status(&message);
                    message_box(
                        Some(self.window),
                        &message,
                        "Sakura Input の更新",
                        MB_OK | MB_ICONERROR,
                    );
                }
            } else {
                self.set_status(&format!("Sakura Input {version} の更新を保留しました。"));
            }
        }
    }

    fn update_stage_label(stage: updater::UpdateStage) -> &'static str {
        match stage {
            updater::UpdateStage::Worker => "更新処理",
            updater::UpdateStage::ManifestDownload => "更新情報の取得",
            updater::UpdateStage::ManifestValidation => "更新情報の検証",
            updater::UpdateStage::InstallerPreparation => "インストーラーの準備",
            updater::UpdateStage::InstallerDownload => "インストーラーの取得",
            updater::UpdateStage::InstallerSize => "インストーラーのサイズ確認",
            updater::UpdateStage::InstallerHash => "インストーラーのSHA-256確認",
            updater::UpdateStage::SignatureVerification => "インストーラーの署名確認",
            updater::UpdateStage::InstallerLaunch => "インストーラーの起動",
            updater::UpdateStage::InstallerExit => "インストーラーの終了",
        }
    }

    fn describe_update_failure(failure: &updater::UpdateFailure) -> String {
        format!(
            "{}に失敗しました: {}",
            Self::update_stage_label(failure.stage),
            failure.message
        )
    }

    fn describe_update_check(outcome: &updater::UpdateCheckOutcome) -> String {
        match outcome {
            updater::UpdateCheckOutcome::Disabled => {
                "自動更新は無効です（ネットワーク通信は行いません）。".to_owned()
            }
            updater::UpdateCheckOutcome::UpToDate { current, latest } => {
                format!("Sakura Input は最新です（現在: {current}、確認した最新版: {latest}）。")
            }
            updater::UpdateCheckOutcome::Available(manifest) => {
                format!(
                    "Sakura Input {version} の更新が利用できます。",
                    version = manifest.version
                )
            }
            updater::UpdateCheckOutcome::Failed(failure) => Self::describe_update_failure(failure),
        }
    }

    fn update_available_prompt(version: updater::Version) -> String {
        format!(
            "Sakura Input {version} の更新が利用できます。\r\n\r\n現在のバージョン: {}\r\n更新後のバージョン: {version}\r\n\r\n今すぐダウンロードして、検証後にインストールしますか？\r\n管理者権限の確認が表示されます。",
            updater::current_version()
        )
    }

    fn describe_update(outcome: &updater::UpdateOutcome) -> String {
        match outcome {
            updater::UpdateOutcome::Disabled => {
                "自動更新は無効です（ネットワーク通信は行いません）。".to_owned()
            }
            updater::UpdateOutcome::UpToDate { current, latest } => {
                format!("Sakura Input は最新です（現在: {current}、確認した最新版: {latest}）。")
            }
            updater::UpdateOutcome::Installed { version } => {
                format!("Sakura Input {version} をインストールしました。")
            }
            updater::UpdateOutcome::RestartRequired { version } => format!(
                "Sakura Input {version} をインストールしました。古いファイルの整理のためWindowsの再起動が必要です。"
            ),
            updater::UpdateOutcome::TimedOutStillRunning { version } => format!(
                "Sakura Input {version} のインストーラーはまだ実行中です（30分の待機上限に達しました）。"
            ),
            updater::UpdateOutcome::Failed { failure, .. } => Self::describe_update_failure(failure),
        }
    }

    /// Applies the one close policy for the caption close control and the
    /// bottom buttons. An in-flight update owns the terminal result, so every
    /// request deliberately leaves the window alive until `finish_update`.
    fn request_close(&self, request: CloseRequest) -> CloseDecision {
        match close_decision(request, self.update_in_flight) {
            CloseDecision::WaitForUpdate => {
                message_box(
                    Some(self.window),
                    "更新処理を実行中です。終了結果が表示されるまでプロパティを閉じないでください。",
                    "Sakura Input の更新",
                    MB_OK | MB_ICONWARNING,
                );
                CloseDecision::WaitForUpdate
            }
            CloseDecision::Destroy => {
                // SAFETY: this is the live top-level window on its owning UI
                // thread. All close routes reach this one destruction boundary.
                unsafe {
                    let _ = DestroyWindow(self.window);
                }
                CloseDecision::Destroy
            }
        }
    }

    fn set_status(&self, message: &str) {
        set_text(self.status, &compact_status(message));
    }
}

/// The bottom-left status slot is deliberately one line beside the persistent
/// action buttons. Keep detail such as file paths in the operation page or an
/// error dialog instead of allowing a success notification to cover buttons.
fn compact_status(message: &str) -> String {
    const MAX_CHARS: usize = 26;
    let single_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = single_line.chars();
    let visible = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{visible}…")
    } else {
        visible
    }
}

fn window_dpi(window: HWND) -> u32 {
    // SAFETY: callers pass a live window owned by this UI thread. Windows
    // documents zero as the failure value, so retain the logical baseline.
    let dpi = unsafe { GetDpiForWindow(window) };
    if dpi == 0 {
        96
    } else {
        dpi
    }
}

fn scale_dpi_value(value: i32, from: u32, to: u32) -> i32 {
    let from = i64::from(from.max(1));
    let to = i64::from(to.max(1));
    let value = i64::from(value);
    value
        .saturating_mul(to)
        .saturating_add(if value >= 0 { from / 2 } else { -(from / 2) })
        .checked_div(from)
        .unwrap_or(value)
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn suggested_rect(value: LPARAM) -> Option<RECT> {
    let pointer = value.0 as *const RECT;
    // SAFETY: Windows supplies a readable suggested rectangle for
    // WM_DPICHANGED. A null LPARAM is treated as a malformed notification.
    (!pointer.is_null()).then(|| unsafe { *pointer })
}

fn scaled_window_rect(window: HWND, from: u32, to: u32) -> Option<RECT> {
    let mut rect = RECT::default();
    // SAFETY: the window is live and `rect` is writable storage for User32.
    unsafe { GetWindowRect(window, &mut rect) }.ok()?;
    let width = scale_dpi_value(rect.right.saturating_sub(rect.left), from, to);
    let height = scale_dpi_value(rect.bottom.saturating_sub(rect.top), from, to);
    Some(RECT {
        left: rect.left,
        top: rect.top,
        right: rect.left.saturating_add(width),
        bottom: rect.top.saturating_add(height),
    })
}

fn child_client_rect(parent: HWND, child: HWND) -> Option<RECT> {
    let mut screen = RECT::default();
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: both handles are descendants of the live settings root, and the
    // output structures outlive the synchronous User32 calls.
    unsafe {
        GetWindowRect(child, &mut screen).ok()?;
        if !ClientToScreen(parent, &mut origin).as_bool() {
            return None;
        }
    }
    Some(RECT {
        left: screen.left.saturating_sub(origin.x),
        top: screen.top.saturating_sub(origin.y),
        right: screen.right.saturating_sub(origin.x),
        bottom: screen.bottom.saturating_sub(origin.y),
    })
}

fn scale_window_children(parent: HWND, from: u32, to: u32) {
    // SAFETY: this traversal runs on the settings UI thread. We cache the
    // next sibling before moving the current child so SetWindowPos cannot
    // invalidate the iteration order.
    unsafe {
        let Ok(mut child) = GetWindow(parent, GW_CHILD) else {
            return;
        };
        while !child.0.is_null() {
            let next = GetWindow(child, GW_HWNDNEXT).ok();
            if let Some(rect) = child_client_rect(parent, child) {
                let x = scale_dpi_value(rect.left, from, to);
                let y = scale_dpi_value(rect.top, from, to);
                let width = scale_dpi_value(rect.right.saturating_sub(rect.left), from, to);
                let height = scale_dpi_value(rect.bottom.saturating_sub(rect.top), from, to);
                let _ = SetWindowPos(
                    child,
                    None,
                    x,
                    y,
                    width.max(1),
                    height.max(1),
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                scale_window_children(child, from, to);
            }
            let Some(next) = next else { break };
            child = next;
        }
    }
}

fn refresh_native_fonts(window: HWND) {
    // SAFETY: DEFAULT_GUI_FONT is a process-lifetime stock object. Every
    // descendant borrows the handle through WM_SETFONT and never owns it.
    unsafe {
        let font = GetStockObject(DEFAULT_GUI_FONT);
        let Ok(mut child) = GetWindow(window, GW_CHILD) else {
            return;
        };
        while !child.0.is_null() {
            let _ = SendMessageW(
                child,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            refresh_native_fonts(child);
            let Ok(next) = GetWindow(child, GW_HWNDNEXT) else {
                return;
            };
            child = next;
        }
    }
}

const fn button_type_style(owner_draw: bool, default: bool) -> u32 {
    if owner_draw {
        BS_OWNERDRAW as u32
    } else if default {
        BS_DEFPUSHBUTTON as u32
    } else {
        BS_PUSHBUTTON as u32
    }
}

/// The hidden owner's procedure: it has nothing of its own to do.
///
/// It exists because a window class needs a procedure and the `windows` crate
/// exposes `DefWindowProcW` as a Rust function rather than a `"system"` one.
unsafe extern "system" fn owner_window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: every message is delegated to the system exactly once, with the
    // original scalar payloads.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn register_window_class() -> WindowsResult<()> {
    // SAFETY: the class name and procedure are static; stock objects are
    // process-owned and remain valid for the window class lifetime.
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_procedure),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: WINDOW_CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
        let panel_class = WNDCLASSW {
            lpfnWndProc: Some(panel_window_procedure),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // Panel colors are supplied by WM_ERASEBKGND so the brush can
            // follow a runtime appearance preview instead of remaining fixed.
            lpszClassName: PANEL_CLASS,
            ..Default::default()
        };
        RegisterClassW(&panel_class);
        let owner_class = WNDCLASSW {
            lpfnWndProc: Some(owner_window_procedure),
            lpszClassName: OWNER_CLASS,
            ..Default::default()
        };
        RegisterClassW(&owner_class);
    }
    Ok(())
}

fn icon_asset_path() -> Option<PathBuf> {
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

fn load_icon(path: &Path, size: i32) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: the path is NUL-terminated and remains alive for this call;
    // LoadImageW copies the icon resource into a process-owned HICON.
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
    Some(windows::Win32::UI::WindowsAndMessaging::HICON(handle.0))
}

fn apply_window_icons(window: HWND) -> Option<WindowIcons> {
    let path = icon_asset_path()?;
    let big = load_icon(&path, 32)?;
    let Some(small) = load_icon(&path, 16) else {
        // SAFETY: `big` was returned by LoadImageW and has not been handed
        // to the window because the small icon could not be loaded.
        unsafe {
            let _ = DestroyIcon(big);
        }
        return None;
    };
    let icons = WindowIcons { big, small };
    // SAFETY: the window is owned by this UI thread; WM_SETICON only stores
    // the handles, while `icons` retains ownership until the pump ends.
    unsafe {
        let _ = SendMessageW(
            window,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(icons.big.0 as isize)),
        );
        let _ = SendMessageW(
            window,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(icons.small.0 as isize)),
        );
    }
    Some(icons)
}

fn create_main_window() -> WindowsResult<HWND> {
    // Never shown, never sized, never painted: the whole job of this window is
    // to be an owner, which is what keeps the settings sheet out of the
    // taskbar. `WS_EX_TOOLWINDOW` would have done it too, but it would also
    // have shrunk the caption of a window the owner reads labels in.
    //
    // SAFETY: the class is registered above and the call is synchronous.
    let owner = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            OWNER_CLASS,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )?
    };
    // SAFETY: the class has just been registered and all text pointers outlive
    // this synchronous call.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            WINDOW_CLASS,
            windows::core::w!("Sakura Input プロパティ"),
            // Match the fixed-size property-sheet behavior: controls are laid
            // out against the measured ATOK-like client grid, so a resize must
            // not create clipped Japanese labels or overlapping action rows.
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            Some(owner),
            None,
            None,
            None,
        )
    }
}

fn panel(parent: HWND) -> WindowsResult<HWND> {
    topic_panel(parent, PANEL_LEFT, PANEL_TOP, PANEL_WIDTH, PANEL_HEIGHT)
}

fn topic_panel(parent: HWND, left: i32, top: i32, width: i32, height: i32) -> WindowsResult<HWND> {
    control(
        parent,
        PANEL_CLASS,
        "",
        WS_CHILD | WS_VISIBLE,
        WS_EX_CONTROLPARENT,
        left,
        top,
        width,
        height,
    )
}

fn create_general_controls(parent: HWND) -> WindowsResult<GeneralControls> {
    let basic_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let profile_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let input_assist_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let ai_text_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let segment_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let normalizer_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let prediction_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let association_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let input_repair_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let input_symbol_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let display_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;

    let parent = basic_panel;
    label(parent, "基本設定", 4, 2, 190, 20)?;
    label(parent, "入力方法の基本設定を行います。", 4, 20, 358, 18)?;
    group_box(parent, "既定の入力", 0, TOPIC_GROUP_TOP, PANEL_WIDTH, 166)?;
    label(parent, "キー設定", 12, 72, 92, 22)?;
    let keymap = combo(parent, 104, 68, 126, 150)?;
    add_combo(keymap, "Microsoft IME 互換");
    add_combo(keymap, "ATOK 互換");
    label(parent, "入力方法", 12, 102, 92, 22)?;
    let input_method_romaji = radio(
        parent,
        input_method_label(InputMethod::Romaji),
        104,
        99,
        100,
        22,
        true,
    )?;
    let input_method_kana = radio(
        parent,
        input_method_label(InputMethod::Kana),
        204,
        99,
        88,
        22,
        false,
    )?;
    label(parent, "文字種", 12, 130, 92, 22)?;
    let default_mode = combo(parent, 104, 126, 155, 150)?;
    for mode in Mode::ALL {
        add_combo(default_mode, mode_label(mode));
    }
    label(parent, "Sakura Pad", 12, 160, 92, 22)?;
    let pad_shortcut = combo(parent, 104, 156, 180, 120)?;
    for shortcut in PadShortcut::ALL {
        add_combo(pad_shortcut, pad_shortcut_label(shortcut));
    }
    let parent = input_assist_panel;
    label(parent, "入力補助", 4, 2, 190, 20)?;
    label(
        parent,
        "スペースキーで入力する空白文字を設定します。",
        4,
        20,
        358,
        18,
    )?;
    group_box(parent, "空白文字", 0, TOPIC_GROUP_TOP, PANEL_WIDTH, 96)?;
    label(parent, "スペースキー", 12, 72, 96, 20)?;
    let input_assist_space_width = combo(parent, 116, 68, 160, 120)?;
    for space_width in SpaceWidth::ALL {
        add_combo(input_assist_space_width, space_width_label(space_width));
    }
    label(parent, "Shift+スペース", 12, 100, 96, 20)?;
    let input_assist_shift_space = combo(parent, 116, 96, 160, 120)?;
    add_combo(input_assist_shift_space, "スペースの逆");
    add_combo(input_assist_shift_space, "常に全角");
    add_combo(input_assist_shift_space, "常に半角");

    label(parent, "文章変換キー", 12, 132, 96, 20)?;
    let ai_text_key = combo(parent, 116, 128, 190, 120)?;
    add_combo(ai_text_key, "変換（Spaceの右・既定）");
    add_combo(ai_text_key, "Caps Lock");
    add_combo(ai_text_key, "使わない");

    let parent = ai_text_panel;
    label(parent, "AI文章変換", 4, 2, 190, 20)?;
    label(
        parent,
        "明示操作した文章だけをGPT-5.6 Lunaへ送信します。",
        4,
        20,
        358,
        18,
    )?;
    label(parent, "プロバイダー", 12, 56, 92, 20)?;
    let ai_provider = combo(parent, 110, 52, 220, 150)?;
    let ai_providers = available_ai_providers();
    for provider in &ai_providers {
        add_combo(ai_provider, ai_provider_label(*provider));
    }
    label(parent, "モデル", 12, 84, 92, 20)?;
    let _ai_model = edit(parent, sakura_ai_proto::MODEL, 110, 80, 220, 24, true)?;
    label(parent, "Endpoint", 12, 112, 92, 20)?;
    let ai_endpoint = edit(parent, "", 110, 108, 260, 24, false)?;
    label(parent, "認証", 12, 140, 92, 20)?;
    let ai_auth = combo(parent, 110, 136, 130, 120)?;
    for auth in AiAuth::ALL {
        add_combo(ai_auth, ai_auth_label(auth));
    }
    label(parent, "APIキー", 12, 168, 92, 20)?;
    let ai_api_key = password_edit(parent, 110, 164, 190, 24)?;
    let ai_api_key_clear = button(parent, "削除", 306, 164, 64, 24, false)?;
    let ai_api_key_status = label(parent, "", 110, 190, 260, 18)?;
    label(parent, "変換スタイル", 12, 220, 92, 20)?;
    let ai_style = combo(parent, 110, 216, 190, 180)?;
    for style in AiStyle::ALL {
        add_combo(ai_style, ai_style_label(style));
    }
    label(parent, "Effort", 12, 248, 92, 20)?;
    let ai_effort = combo(parent, 110, 244, 130, 180)?;
    for effort in AiEffort::ALL {
        add_combo(ai_effort, ai_effort_label(effort));
    }
    label(parent, "Tier", 250, 248, 36, 20)?;
    let ai_service_tier = combo(parent, 286, 244, 84, 120)?;
    for tier in AiServiceTier::ALL {
        add_combo(ai_service_tier, ai_service_tier_label(tier));
    }

    let parent = prediction_panel;
    label(parent, "推測変換", 4, 2, 190, 20)?;
    label(
        parent,
        "入力中に候補を自動表示し、確定方法を選べます。",
        4,
        20,
        358,
        18,
    )?;
    group_box(parent, "推測候補", 0, TOPIC_GROUP_TOP, PANEL_WIDTH, 112)?;
    let prediction = checkbox(parent, "予測入力を使う", 12, 70, 180, 24)?;
    label(parent, "候補の確定", 12, 104, 92, 22)?;
    let suggest = combo(parent, 104, 100, 126, 120)?;
    add_combo(suggest, "Tab");
    add_combo(suggest, "Shift+Enter");
    add_combo(suggest, "使わない");

    let parent = segment_panel;
    label(parent, "文節変換", 4, 2, 190, 20)?;
    label(
        parent,
        "変換単位と sakura-rerank による候補の並べ替えを設定します。",
        4,
        20,
        358,
        18,
    )?;
    group_box(parent, "変換方法", 0, TOPIC_GROUP_TOP, PANEL_WIDTH, 80)?;
    label(parent, "変換単位", 12, 76, 92, 22)?;
    let conversion_assist_method = combo(parent, 104, 72, 155, 150)?;
    for method in ConversionMethod::ALL {
        add_combo(conversion_assist_method, conversion_method_label(method));
    }
    group_box(parent, "AI候補の並べ替え", 0, 144, PANEL_WIDTH, 100)?;
    label(parent, "sakura-rerank の適用範囲", 12, 172, 168, 20)?;
    let neural_reranker_scope = combo(parent, 184, 168, 168, 120)?;
    for scope in NeuralRerankerScope::ALL {
        add_combo(neural_reranker_scope, neural_reranker_scope_label(scope));
    }
    label(
        parent,
        "文節変換を基本とし、sakura-rerank は候補の並べ替えだけに使用します。",
        12,
        204,
        358,
        32,
    )?;

    let parent = normalizer_panel;
    label(parent, "文字幅・句読点", 4, 2, 190, 20)?;
    label(
        parent,
        "英数字・記号・句読点の表示形式を設定します。",
        4,
        20,
        358,
        18,
    )?;
    group_box(
        parent,
        "入力・変換",
        0,
        TOPIC_GROUP_TOP,
        PANEL_WIDTH,
        NORMALIZER_GROUP_HEIGHT,
    )?;
    // The preset sits above the six leaf controls it writes, because it is a
    // shortcut for them rather than a seventh independent setting: picking a
    // style fills the rows below in, and editing any row below moves the
    // preset to `カスタム`. Nothing is stored under the preset's own name.
    label(parent, "表記スタイル", 12, 76, 76, 22)?;
    let notation_style = combo(parent, 92, 72, 236, 150)?;
    for style in NotationStyle::ALL {
        add_combo(notation_style, style.label());
    }
    add_combo(notation_style, NOTATION_STYLE_CUSTOM_LABEL);
    label(parent, "英字", 12, 104, 42, 22)?;
    let normalizer_alnum = combo(parent, 54, 100, 108, 120)?;
    for width in [Width::Half, Width::Full, Width::FollowMode] {
        add_combo(normalizer_alnum, width_label(width));
    }
    label(parent, "数字", 178, 104, 42, 22)?;
    let normalizer_number = combo(parent, 220, 100, 108, 120)?;
    for width in [Width::Half, Width::Full, Width::FollowMode] {
        add_combo(normalizer_number, width_label(width));
    }
    label(parent, "句点", 12, 132, 42, 22)?;
    let punctuation_period = combo(parent, 54, 128, 108, 120)?;
    for mark in PeriodMark::ALL {
        add_combo(punctuation_period, period_mark_label(mark));
    }
    label(parent, "読点", 178, 132, 42, 22)?;
    let punctuation_comma = combo(parent, 220, 128, 108, 120)?;
    for mark in CommaMark::ALL {
        add_combo(punctuation_comma, comma_mark_label(mark));
    }
    label(parent, "記号", 12, 160, 42, 22)?;
    let normalizer_symbol = combo(parent, 54, 156, 108, 120)?;
    for width in [Width::Half, Width::Full, Width::FollowMode] {
        add_combo(normalizer_symbol, width_label(width));
    }
    label(parent, "括弧", 178, 160, 42, 22)?;
    let punctuation_brackets = combo(parent, 220, 156, 108, 120)?;
    for bracket in BracketStyle::ALL {
        add_combo(punctuation_brackets, bracket_style_label(bracket));
    }
    let normalizer_reset = button(
        parent,
        "初期値に戻す",
        260,
        NORMALIZER_RESET_Y,
        116,
        NORMALIZER_RESET_HEIGHT,
        false,
    )?;

    let parent = association_panel;
    label(parent, "連想変換", 4, 2, 190, 20)?;
    label(
        parent,
        "文節のつながりを使った候補を表示します。",
        4,
        20,
        358,
        18,
    )?;
    group_box(parent, "連想変換", 0, TOPIC_GROUP_TOP, PANEL_WIDTH, 92)?;
    let association = checkbox(parent, "連想変換を使う", 12, 70, 180, 24)?;
    label(
        parent,
        "連想候補は文節変換とは別に表示されます。",
        12,
        102,
        358,
        20,
    )?;

    let parent = input_repair_panel;
    label(parent, "入力誤りの自動修復", 4, 2, 220, 20)?;
    label(
        parent,
        "ローマ字／カナ入力のミスを変換時に修正します。",
        4,
        20,
        358,
        18,
    )?;
    let input_support_enabled = checkbox(parent, "入力支援を有効にする", 12, 48, 200, 22)?;
    let input_support_commit_based = checkbox(parent, "確定内容に応じて修正する", 12, 74, 180, 20)?;
    let input_support_advanced = checkbox(parent, "高度な自動修復を行う", 200, 74, 170, 20)?;
    let input_support_vowel_count = checkbox(parent, "母音の過不足", 12, 98, 180, 20)?;
    let input_support_consonant_extra = checkbox(parent, "子音の超過", 200, 98, 170, 20)?;
    let input_support_n_count = checkbox(parent, "Ｎの過不足", 12, 122, 180, 20)?;
    let input_support_dakuten_swap = checkbox(parent, "゛／゜の誤り", 200, 122, 170, 20)?;
    let input_support_tsu_sokuon = checkbox(parent, "つ→っ", 12, 146, 180, 20)?;
    let input_support_wa_wo = checkbox(parent, "わ→を", 200, 146, 170, 20)?;
    let input_support_small_u = checkbox(parent, "ぅ→う", 12, 170, 180, 20)?;
    let input_support_fuzzy_proper_nouns =
        checkbox(parent, "あいまいな固有名詞", 200, 170, 170, 20)?;
    let input_support_reset = button(parent, "初期値に戻す", 260, 250, 116, 24, false)?;

    let parent = input_symbol_panel;
    label(parent, "英単語・記号置換", 4, 2, 220, 20)?;
    label(
        parent,
        "英単語のつづりと、数字・英数字直後の記号を置換します。",
        4,
        20,
        358,
        18,
    )?;
    let input_support_english_to_katakana = checkbox(
        parent,
        "英単語のつづりをカタカナ語に変換する",
        12,
        56,
        340,
        22,
    )?;
    group_box(parent, "長音・句読点の自動置換", 0, 92, PANEL_WIDTH, 140)?;
    let input_support_period_after_digit =
        checkbox(parent, "句点（。）→ピリオド（．）", 12, 120, 340, 20)?;
    let input_support_comma_after_digit =
        checkbox(parent, "読点（、）→カンマ（，）", 12, 144, 340, 20)?;
    let input_support_middle_dot_after_digit =
        checkbox(parent, "中黒（・）→スラッシュ（／）", 12, 168, 340, 20)?;
    let input_support_long_vowel_after_alnum =
        checkbox(parent, "長音（ー）→マイナス（－）", 12, 192, 340, 20)?;

    let parent = display_panel;
    label(parent, "表示", 4, 2, 190, 20)?;
    label(
        parent,
        "候補ウィンドウと設定画面の外観を選びます。",
        4,
        20,
        358,
        18,
    )?;
    group_box(parent, "外観", 0, TOPIC_GROUP_TOP, PANEL_WIDTH, 116)?;
    label(parent, "テーマ", 12, 76, 72, 22)?;
    let appearance = combo(parent, 104, 72, 160, 120)?;
    for theme in AppearanceTheme::ALL {
        add_combo(appearance, appearance_label(theme));
    }
    label(
        parent,
        "ハイ コントラスト設定が有効な場合は、Windows の配色を使用します。",
        12,
        110,
        358,
        34,
    )?;

    let parent = profile_panel;
    label(parent, "アプリ別の設定", 4, 2, 190, 20)?;
    label(parent, "アプリごとの入力方法を設定します。", 4, 20, 358, 18)?;
    group_box(
        parent,
        "アプリ別の設定",
        0,
        TOPIC_GROUP_TOP,
        PANEL_WIDTH,
        PROFILE_GROUP_HEIGHT,
    )?;
    let profile_list = listbox(parent, 12, 68, 150, 154)?;
    label(parent, "実行ファイル名", 170, 68, 86, 20)?;
    let profile_process = edit(parent, "", 260, 64, 117, 24, false)?;
    label(parent, "既定の入力モード", 170, 94, 86, 20)?;
    let profile_mode = combo(parent, 260, 90, 117, 160)?;
    for mode in Mode::ALL {
        add_combo(profile_mode, mode_label(mode));
    }
    let profile_prediction = checkbox(parent, "予測入力を使う", 170, 116, 116, 22)?;
    label(parent, "候補の確定", 170, 140, 86, 20)?;
    let profile_suggest = combo(parent, 260, 136, 117, 120)?;
    add_combo(profile_suggest, "Tab");
    add_combo(profile_suggest, "Shift+Enter");
    add_combo(profile_suggest, "使わない");
    // A profile stores a `Normalizer` but no space width, so the per-app form
    // of the setting is the preset alone — the six leaf controls of the
    // 文字幅・句読点 page are deliberately not duplicated here.
    label(parent, "表記スタイル", 170, 166, 86, 20)?;
    let profile_notation = combo(parent, 260, 162, 117, 150)?;
    for style in NotationStyle::ALL {
        add_combo(profile_notation, style.label());
    }
    add_combo(profile_notation, NOTATION_STYLE_CUSTOM_LABEL);
    select_combo(profile_mode, mode_index(Mode::Hiragana));
    select_combo(profile_suggest, suggest_index(SuggestAccept::Tab));
    select_combo(profile_notation, notation_style_index(None));
    let profile_save = button(parent, "追加／更新", 170, 192, 100, 22, false)?;
    let profile_delete = button(parent, "削除", 278, 192, 99, 22, false)?;
    label(
        parent,
        "例: code.exe　設定はアプリが入力コンテキストを作成したときに適用されます。",
        12,
        240,
        358,
        14,
    )?;
    Ok(GeneralControls {
        basic_panel,
        profile_panel,
        input_assist_panel,
        ai_text_panel,
        segment_panel,
        normalizer_panel,
        prediction_panel,
        association_panel,
        display_panel,
        input_repair_panel,
        input_symbol_panel,
        keymap,
        input_method_romaji,
        input_method_kana,
        default_mode,
        pad_shortcut,
        input_assist_space_width,
        input_assist_shift_space,
        ai_text_key,
        ai_provider,
        ai_endpoint,
        ai_auth,
        ai_api_key,
        ai_api_key_status,
        ai_api_key_clear,
        ai_style,
        ai_effort,
        ai_service_tier,
        ai_providers,
        conversion_assist_method,
        prediction,
        suggest,
        association,
        appearance,
        neural_reranker_scope,
        notation_style,
        normalizer_alnum,
        normalizer_number,
        normalizer_symbol,
        punctuation_period,
        punctuation_comma,
        punctuation_brackets,
        normalizer_reset,
        input_support_enabled,
        input_support_commit_based,
        input_support_advanced,
        input_support_vowel_count,
        input_support_consonant_extra,
        input_support_n_count,
        input_support_dakuten_swap,
        input_support_tsu_sokuon,
        input_support_wa_wo,
        input_support_small_u,
        input_support_fuzzy_proper_nouns,
        input_support_reset,
        input_support_english_to_katakana,
        input_support_period_after_digit,
        input_support_comma_after_digit,
        input_support_middle_dot_after_digit,
        input_support_long_vowel_after_alnum,
        profile_list,
        profile_process,
        profile_mode,
        profile_prediction,
        profile_suggest,
        profile_notation,
        profile_save,
        profile_delete,
    })
}

fn create_dictionary_controls(parent: HWND) -> WindowsResult<DictionaryControls> {
    let entries_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let io_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    label(parent, "辞書", 4, 2, 190, 20)?;
    label(
        parent,
        "ユーザー辞書の登録・入出力を行います。",
        4,
        20,
        358,
        18,
    )?;
    let parent = entries_panel;
    group_box(parent, "ユーザー辞書", 0, 38, PANEL_WIDTH, 160)?;
    let list = listbox(parent, 12, 58, 148, 132)?;
    label(parent, "読み", 160, 58, 60, 20)?;
    let reading = edit(parent, "", 220, 54, 153, 24, false)?;
    label(parent, "単語", 160, 84, 60, 20)?;
    let surface = edit(parent, "", 220, 80, 153, 24, false)?;
    label(parent, "品詞", 160, 110, 60, 20)?;
    let part_of_speech = combo(parent, 220, 106, 153, 200)?;
    for pos in UserPartOfSpeech::ALL {
        add_combo(
            part_of_speech,
            &format!("{} — {}", pos.spec().name, pos.spec().label),
        );
    }
    select_combo(part_of_speech, 0);
    label(parent, "コメント", 160, 136, 60, 20)?;
    let comment = edit(parent, "", 220, 132, 153, 24, false)?;
    let add = button(parent, "追加", 160, 162, 68, 22, false)?;
    let update = button(parent, "更新", 234, 162, 68, 22, false)?;
    let delete = button(parent, "削除", 308, 162, 65, 22, false)?;

    let parent = io_panel;
    group_box(parent, "辞書ファイル", 0, 38, PANEL_WIDTH, 78)?;
    label(parent, "ファイル", 12, 58, 56, 20)?;
    let path = edit(parent, "", 68, 54, 230, 24, false)?;
    label(parent, "形式", 306, 58, 38, 20)?;
    let format = combo(parent, 326, 54, 50, 160)?;
    add_combo(format, "自動");
    for value in DictionaryFormat::ALL {
        add_combo(format, value.name());
    }
    select_combo(format, 0);
    let import_mode = combo(parent, 12, 82, 128, 100)?;
    add_combo(import_mode, "追加して登録");
    add_combo(import_mode, "すべて置き換え");
    select_combo(import_mode, 0);
    let import = button(parent, "インポート", 150, 82, 112, 20, false)?;
    let export = button(parent, "エクスポート", 270, 82, 106, 20, false)?;
    label(
        parent,
        "MS-IME／ATOK は UTF-16LE、Sakura／Mozc は UTF-8 です。未対応の項目は登録しません。",
        12,
        142,
        358,
        14,
    )?;
    Ok(DictionaryControls {
        entries_panel,
        io_panel,
        list,
        reading,
        surface,
        part_of_speech,
        comment,
        add,
        update,
        delete,
        path,
        format,
        import_mode,
        import,
        export,
    })
}

fn create_learning_controls(parent: HWND) -> WindowsResult<LearningControls> {
    let history_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let operations_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    label(parent, "学習", 4, 2, 190, 20)?;
    label(
        parent,
        "確定済みの学習履歴を新しい順に表示します。",
        4,
        20,
        358,
        18,
    )?;
    let parent = history_panel;
    group_box(parent, "学習履歴", 0, 40, PANEL_WIDTH, 190)?;
    let list = listbox(parent, 12, 62, 358, 150)?;
    let parent = operations_panel;
    group_box(parent, "操作", 0, 40, PANEL_WIDTH, 80)?;
    let refresh = button(parent, "最新の状態に更新", 12, 62, 122, 22, false)?;
    label(parent, "書き出し先", 144, 64, 62, 20)?;
    let export_path = edit(parent, "", 188, 60, 110, 24, false)?;
    let export = button(parent, "TSV 出力", 306, 60, 70, 22, false)?;
    let clear = button(parent, "学習を消去", 12, 90, 122, 22, false)?;
    label(
        parent,
        "消去は実行中の engine に送信します。完了した消去・出力は［キャンセル］では戻せません。",
        144,
        90,
        258,
        22,
    )?;
    Ok(LearningControls {
        history_panel,
        operations_panel,
        list,
        export_path,
        refresh,
        export,
        clear,
    })
}

fn create_diagnostics_controls(parent: HWND) -> WindowsResult<DiagnosticsControls> {
    label(parent, "詳細設定・診断", 4, 2, 190, 20)?;
    label(
        parent,
        "通信のタイムアウトと整合性の状態を確認します。",
        4,
        20,
        358,
        18,
    )?;
    group_box(parent, "診断情報", 0, 40, PANEL_WIDTH, 210)?;
    let text = multiline_readonly(parent, 12, 62, 358, 168)?;
    group_box(parent, "操作", 0, 236, PANEL_WIDTH, 64)?;
    let refresh = button(parent, "最新の状態に更新", 12, 258, 122, 22, false)?;
    let clear = button(parent, "カウンターを消去", 144, 258, 130, 22, false)?;
    label(
        parent,
        "診断ログは 1 MiB に制限され、例外的なタイムアウト時だけ記録されます。",
        12,
        282,
        358,
        12,
    )?;
    Ok(DiagnosticsControls {
        text,
        refresh,
        clear,
    })
}

fn create_update_controls(parent: HWND) -> WindowsResult<UpdateControls> {
    let settings_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let available_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    let status_panel = topic_panel(parent, 0, 0, PANEL_WIDTH, PANEL_HEIGHT)?;
    label(parent, "更新", 4, 2, 190, 20)?;
    label(
        parent,
        "署名済みのリリース確認とインストールを管理します。",
        4,
        20,
        358,
        18,
    )?;
    let parent = settings_panel;
    group_box(parent, "更新の確認", 0, 40, PANEL_WIDTH, 86)?;
    let enabled = checkbox(parent, "設定画面の起動時に更新を確認する", 12, 64, 260, 24)?;
    let save = button(parent, "設定を保存", 260, 64, 116, 24, false)?;
    let parent = available_panel;
    group_box(parent, "利用可能な更新", 0, 40, PANEL_WIDTH, 92)?;
    let check = button(parent, "今すぐ確認", 12, 64, 106, 24, false)?;
    let apply = button(
        parent,
        "ダウンロードして検証・インストール",
        128,
        64,
        202,
        24,
        false,
    )?;
    label(
        parent,
        "更新確認は任意です。取得前に配布先、サイズ、SHA-256 と Authenticode 署名を確認します。",
        12,
        96,
        358,
        24,
    )?;
    let parent = status_panel;
    group_box(parent, "更新の状態", 0, 40, PANEL_WIDTH, 78)?;
    let result = multiline_readonly(parent, 12, 62, 358, 40)?;
    label(
        parent,
        "結果は成功・再起動が必要・タイムアウト・失敗を区別して表示します。",
        12,
        106,
        358,
        12,
    )?;
    Ok(UpdateControls {
        settings_panel,
        available_panel,
        status_panel,
        enabled,
        save,
        check,
        apply,
        result,
    })
}

fn label(parent: HWND, text: &str, x: i32, y: i32, width: i32, height: i32) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("STATIC"),
        text,
        WS_CHILD | WS_VISIBLE,
        WINDOW_EX_STYLE::default(),
        x,
        y,
        width,
        height,
    )
}

/// A native group box gives the compact pages a familiar property-dialog
/// hierarchy without creating a custom accessibility surface or a focus stop.
fn group_box(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("BUTTON"),
        text,
        // Group boxes are visual containers, never command targets.  Marking
        // them disabled keeps User32 hit-testing on the native controls they
        // surround (ComboBox/ListBox/Button) while preserving the standard
        // property-sheet frame and its UIA grouping semantics.
        WS_CHILD | WS_VISIBLE | WS_DISABLED | WINDOW_STYLE(BS_GROUPBOX as u32),
        WINDOW_EX_STYLE::default(),
        x,
        y,
        width,
        height,
    )
}

fn button(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    default: bool,
) -> WindowsResult<HWND> {
    let button_style = if default { BS_DEFPUSHBUTTON } else { 0 };
    control(
        parent,
        windows::core::w!("BUTTON"),
        text,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(button_style as u32),
        WINDOW_EX_STYLE::default(),
        x,
        y,
        width,
        height,
    )
}

fn checkbox(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("BUTTON"),
        text,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
        WINDOW_EX_STYLE::default(),
        x,
        y,
        width,
        height,
    )
}

fn radio(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    first_in_group: bool,
) -> WindowsResult<HWND> {
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTORADIOBUTTON as u32);
    if first_in_group {
        style |= WS_GROUP;
    }
    control(
        parent,
        windows::core::w!("BUTTON"),
        text,
        style,
        WINDOW_EX_STYLE::default(),
        x,
        y,
        width,
        height,
    )
}

fn combo(parent: HWND, x: i32, y: i32, width: i32, height: i32) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("COMBOBOX"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
        WS_EX_CLIENTEDGE,
        x,
        y,
        width,
        height,
    )
}

fn edit(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    readonly: bool,
) -> WindowsResult<HWND> {
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
    if readonly {
        style |= WINDOW_STYLE(ES_READONLY as u32);
    }
    control(
        parent,
        windows::core::w!("EDIT"),
        text,
        style,
        WS_EX_CLIENTEDGE,
        x,
        y,
        width,
        height,
    )
}

fn multiline_readonly(
    parent: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("EDIT"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WS_VSCROLL
            | WS_HSCROLL
            | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN | ES_READONLY) as u32),
        WS_EX_CLIENTEDGE,
        x,
        y,
        width,
        height,
    )
}

fn listbox(parent: HWND, x: i32, y: i32, width: i32, height: i32) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("LISTBOX"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WS_VSCROLL
            | WS_HSCROLL
            | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32),
        WS_EX_CLIENTEDGE,
        x,
        y,
        width,
        height,
    )
}

fn input_topic_tree(parent: HWND) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("SysTreeView32"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WS_VSCROLL
            | WINDOW_STYLE(TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_SHOWSELALWAYS),
        WS_EX_CLIENTEDGE,
        INPUT_TREE_LEFT,
        INPUT_TREE_TOP,
        INPUT_TREE_WIDTH,
        INPUT_TREE_HEIGHT,
    )
}

fn insert_input_tree_item(
    tree: HWND,
    parent: HTREEITEM,
    title: &str,
    topic: usize,
    has_children: bool,
) -> HTREEITEM {
    let mut title = to_wide(title);
    let mask = if has_children {
        TVIF_TEXT | TVIF_PARAM | TVIF_CHILDREN
    } else {
        TVIF_TEXT | TVIF_PARAM
    };
    let item = TVITEMW {
        mask,
        state: Default::default(),
        stateMask: Default::default(),
        pszText: PWSTR(title.as_mut_ptr()),
        cchTextMax: title.len() as i32,
        cChildren: TVITEMEXW_CHILDREN(if has_children { 1 } else { 0 }),
        lParam: LPARAM(topic as isize),
        ..Default::default()
    };
    let insert = TVINSERTSTRUCTW {
        hParent: parent,
        hInsertAfter: TVI_LAST,
        // SAFETY: the union is initialized with the documented TVITEMW arm.
        Anonymous: TVINSERTSTRUCTW_0 { item },
    };
    // SAFETY: both UTF-16 storage and the insertion structure remain live for
    // this synchronous message; the tree copies the title into its own item.
    let result = unsafe {
        SendMessageW(
            tree,
            TVM_INSERTITEMW,
            Some(WPARAM(0)),
            Some(LPARAM(
                (&insert as *const TVINSERTSTRUCTW).cast::<c_void>() as isize
            )),
        )
    };
    HTREEITEM(result.0)
}

fn expand_input_tree_item(tree: HWND, item: HTREEITEM) {
    // SAFETY: `item` was returned by the live TreeView in this process.
    unsafe {
        let _ = SendMessageW(
            tree,
            TVM_EXPAND,
            Some(WPARAM(TVE_EXPAND.0 as usize)),
            Some(LPARAM(item.0)),
        );
    }
}

fn select_input_tree_item(tree: HWND, item: HTREEITEM) {
    // SAFETY: `item` was returned by the live TreeView in this process.
    unsafe {
        let _ = SendMessageW(
            tree,
            TVM_SELECTITEM,
            Some(WPARAM(TVGN_CARET as usize)),
            Some(LPARAM(item.0)),
        );
    }
}

fn first_input_tree_child(tree: HWND, parent: HTREEITEM) -> Option<HTREEITEM> {
    // SAFETY: `parent` belongs to the live TreeView.  The query carries only
    // scalar item handles and returns the first direct child, if it exists.
    let item = unsafe {
        HTREEITEM(
            SendMessageW(
                tree,
                TVM_GETNEXTITEM,
                Some(WPARAM(TVGN_CHILD as usize)),
                Some(LPARAM(parent.0)),
            )
            .0,
        )
    };
    (item.0 != 0).then_some(item)
}

#[allow(clippy::too_many_arguments)]
fn control(
    parent: HWND,
    class_name: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    extended_style: WINDOW_EX_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> WindowsResult<HWND> {
    let text = to_wide(text);
    // SAFETY: class names are static, the title buffer outlives this call, and
    // the returned child is owned by `parent`.
    let window = unsafe {
        CreateWindowExW(
            extended_style,
            class_name,
            PCWSTR(text.as_ptr()),
            style,
            x,
            y,
            width,
            height,
            Some(parent),
            None,
            None,
            None,
        )?
    };
    // SAFETY: DEFAULT_GUI_FONT is a process-lifetime stock object. WM_SETFONT
    // borrows the handle and does not transfer ownership.
    unsafe {
        let font = GetStockObject(DEFAULT_GUI_FONT);
        SendMessageW(
            window,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    Ok(window)
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

fn set_text(window: HWND, value: &str) {
    let wide = to_wide(value);
    // SAFETY: `wide` is NUL-terminated and remains alive for the call.
    unsafe {
        let _ = SetWindowTextW(window, PCWSTR(wide.as_ptr()));
    }
}

fn window_text(window: HWND) -> String {
    // SAFETY: `window` is a live control. The allocated buffer is one unit
    // larger than the length returned immediately before the read.
    unsafe {
        let length = GetWindowTextLengthW(window).max(0) as usize;
        let mut buffer = vec![0u16; length.saturating_add(1)];
        let written = GetWindowTextW(window, &mut buffer).max(0) as usize;
        String::from_utf16_lossy(&buffer[..written])
    }
}

fn has_visible_style(window: HWND) -> bool {
    // SAFETY: callers pass a live control HWND; this is a scalar style read.
    unsafe { (GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_VISIBLE.0) != 0 }
}

fn required_text(window: HWND, label: &str) -> Result<String, String> {
    let value = window_text(window);
    if value.trim().is_empty() {
        Err(format!("{label}を入力してください。"))
    } else {
        Ok(value)
    }
}

fn add_combo(window: HWND, value: &str) {
    send_string(window, CB_ADDSTRING, value);
}

fn add_list(window: HWND, value: &str) {
    send_string(window, LB_ADDSTRING, value);
}

fn send_string(window: HWND, message: u32, value: &str) {
    let wide = to_wide(value);
    // SAFETY: the message consumes the NUL-terminated text synchronously.
    unsafe {
        SendMessageW(
            window,
            message,
            Some(WPARAM(0)),
            Some(LPARAM(wide.as_ptr() as isize)),
        );
    }
}

fn select_combo(window: HWND, index: usize) {
    // SAFETY: the combobox validates the requested index itself.
    unsafe {
        SendMessageW(window, CB_SETCURSEL, Some(WPARAM(index)), Some(LPARAM(0)));
    }
}

fn combo_index(window: HWND) -> Option<usize> {
    // SAFETY: this is a scalar query with no pointer arguments.
    let result = unsafe { SendMessageW(window, CB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 };
    usize::try_from(result).ok()
}

fn list_index(window: HWND) -> Option<usize> {
    // SAFETY: this is a scalar query with no pointer arguments.
    let result = unsafe { SendMessageW(window, LB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 };
    usize::try_from(result).ok()
}

#[cfg(test)]
fn list_count(window: HWND) -> Option<usize> {
    // SAFETY: this is a scalar query with no pointer arguments.
    let result = unsafe { SendMessageW(window, LB_GETCOUNT, Some(WPARAM(0)), Some(LPARAM(0))).0 };
    usize::try_from(result).ok()
}

#[cfg(test)]
fn list_text(window: HWND, index: usize) -> Option<String> {
    // SAFETY: this is a scalar length query. The list box owns the requested
    // string until the later synchronous `LB_GETTEXT` call completes.
    let length =
        unsafe { SendMessageW(window, LB_GETTEXTLEN, Some(WPARAM(index)), Some(LPARAM(0))).0 };
    let length = usize::try_from(length).ok()?;
    let mut buffer = vec![0u16; length.saturating_add(1)];
    // SAFETY: the mutable UTF-16 buffer has room for the reported item plus a
    // NUL terminator and remains live for the synchronous copy.
    let copied = unsafe {
        SendMessageW(
            window,
            LB_GETTEXT,
            Some(WPARAM(index)),
            Some(LPARAM(buffer.as_mut_ptr() as isize)),
        )
        .0
    };
    let copied = usize::try_from(copied).ok()?;
    String::from_utf16(&buffer[..copied]).ok()
}

fn reset_list(window: HWND) {
    // SAFETY: the control owns the strings it is asked to release.
    unsafe {
        SendMessageW(window, LB_RESETCONTENT, Some(WPARAM(0)), Some(LPARAM(0)));
    }
}

fn select_list(window: HWND, index: usize) {
    // SAFETY: the list box validates the requested index itself.
    unsafe {
        SendMessageW(window, LB_SETCURSEL, Some(WPARAM(index)), Some(LPARAM(0)));
    }
}

const fn topics_for_panel(selected: usize) -> &'static [&'static str] {
    match selected {
        0 => &["基本設定", "アプリ別の設定"],
        1 => &["登録単語", "辞書ファイルの入出力"],
        2 => &["学習履歴", "操作"],
        3 => &["診断情報"],
        4 => &["更新の設定", "利用可能な更新", "更新の状態"],
        _ => &[],
    }
}

fn set_checked(window: HWND, checked: bool) {
    let state = if checked { BST_CHECKED } else { BST_UNCHECKED };
    // SAFETY: this is the documented scalar BM_SETCHECK payload.
    unsafe {
        SendMessageW(
            window,
            BM_SETCHECK,
            Some(WPARAM(state.0 as usize)),
            Some(LPARAM(0)),
        );
    }
}

fn is_checked(window: HWND) -> bool {
    // SAFETY: this is a scalar query with no pointer arguments.
    unsafe {
        SendMessageW(window, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0
            == BST_CHECKED.0 as isize
    }
}

fn dictionary_format(
    selection: Option<usize>,
    allow_auto: bool,
) -> Result<Option<DictionaryFormat>, String> {
    match selection {
        Some(0) if allow_auto => Ok(None),
        Some(0) => Err("自動判別は読み込み時のみ利用できます。".to_owned()),
        Some(index) => DictionaryFormat::ALL
            .get(index - 1)
            .copied()
            .map(Some)
            .ok_or_else(|| "選択した辞書形式が不正です。".to_owned()),
        None => Err("辞書形式を選択してください。".to_owned()),
    }
}

fn mode_index(mode: Mode) -> usize {
    Mode::ALL
        .iter()
        .position(|candidate| *candidate == mode)
        .unwrap_or(1)
}

fn conversion_method_index(method: ConversionMethod) -> usize {
    ConversionMethod::ALL
        .iter()
        .position(|candidate| *candidate == method)
        .unwrap_or(0)
}

fn conversion_method_from_index(index: Option<usize>) -> Result<ConversionMethod, String> {
    ConversionMethod::ALL
        .get(index.ok_or_else(|| "変換方法を選択してください。".to_owned())?)
        .copied()
        .ok_or_else(|| "変換方法が不正です。".to_owned())
}

const fn conversion_method_label(method: ConversionMethod) -> &'static str {
    match method {
        ConversionMethod::MultiSegment => "連文節変換",
        ConversionMethod::SingleSegment => "単文節変換",
    }
}

/// UI-only wording keeps the Japanese property sheet independent from the
/// stable command-line renderer used by scripts and automation.
const fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Direct => "直接入力",
        Mode::Hiragana => "ひらがな",
        Mode::Katakana => "全角カタカナ",
        Mode::HalfKatakana => "半角カタカナ",
        Mode::FullAlnum => "全角英数",
        Mode::HalfAlnum => "半角英数",
    }
}

const fn input_method_label(method: InputMethod) -> &'static str {
    match method {
        InputMethod::Romaji => "ローマ字入力",
        InputMethod::Kana => "カナ入力",
    }
}

fn input_method_from_checks(romaji: bool, kana: bool) -> Result<InputMethod, String> {
    match (romaji, kana) {
        (true, false) => Ok(InputMethod::Romaji),
        (false, true) => Ok(InputMethod::Kana),
        _ => Err("入力方法を1つ選択してください。".to_owned()),
    }
}

fn width_index(value: Width) -> usize {
    [Width::Half, Width::Full, Width::FollowMode]
        .into_iter()
        .position(|candidate| candidate == value)
        .unwrap_or(0)
}

fn width_from_index(index: Option<usize>) -> Result<Width, String> {
    [Width::Half, Width::Full, Width::FollowMode]
        .into_iter()
        .nth(index.ok_or_else(|| "英字・数字・記号の幅を選択してください。".to_owned())?)
        .ok_or_else(|| "英字・数字・記号の幅が不正です。".to_owned())
}

// The notation preset names this value in its status line, so the label has
// to come from one place or the message and the combo can drift apart.
fn space_width_label(value: SpaceWidth) -> &'static str {
    match value {
        SpaceWidth::SameAsInput => "入力文字種と同じ",
        SpaceWidth::Full => "常に全角",
        SpaceWidth::Half => "常に半角",
    }
}

fn space_width_index(value: SpaceWidth) -> usize {
    SpaceWidth::ALL
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn space_width_from_index(index: Option<usize>) -> Result<SpaceWidth, String> {
    SpaceWidth::ALL
        .get(index.ok_or_else(|| "空白文字の幅を選択してください。".to_owned())?)
        .copied()
        .ok_or_else(|| "空白文字の幅が不正です。".to_owned())
}

fn shift_space_behavior_index(value: ShiftSpaceBehavior) -> usize {
    ShiftSpaceBehavior::ALL
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn shift_space_behavior_from_index(index: Option<usize>) -> Result<ShiftSpaceBehavior, String> {
    ShiftSpaceBehavior::ALL
        .get(index.ok_or_else(|| "Shift+スペースの動作を選択してください。".to_owned())?)
        .copied()
        .ok_or_else(|| "Shift+スペースの動作が不正です。".to_owned())
}

fn password_edit(parent: HWND, x: i32, y: i32, width: i32, height: i32) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("EDIT"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE((ES_AUTOHSCROLL | ES_PASSWORD) as u32),
        WS_EX_CLIENTEDGE,
        x,
        y,
        width,
        height,
    )
}

fn ai_text_key_index(value: AiTextKey) -> usize {
    AiTextKey::ALL
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn ai_text_key_from_index(index: Option<usize>) -> Result<AiTextKey, String> {
    AiTextKey::ALL
        .get(index.ok_or_else(|| "文章変換キーを選択してください。".to_owned())?)
        .copied()
        .ok_or_else(|| "文章変換キーの値が不正です。".to_owned())
}

fn available_ai_providers() -> Vec<AiProvider> {
    let persisted = user_preferences::read_ai_text_preferences().provider;
    AiProvider::ALL
        .into_iter()
        .filter(|provider| {
            *provider != AiProvider::ChatGptCodex
                || codex_cli_available()
                || persisted == AiProvider::ChatGptCodex
        })
        .collect()
}

fn codex_cli_available() -> bool {
    ["codex.exe", "codex.cmd"].into_iter().any(|name| {
        Command::new("where.exe")
            .arg(name)
            .output()
            .map(|output| output.status.success() && !output.stdout.is_empty())
            .unwrap_or(false)
    })
}

fn enum_index<T: Copy + PartialEq>(values: &[T], value: T) -> usize {
    values
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn enum_from_index<T: Copy>(values: &[T], index: Option<usize>, label: &str) -> Result<T, String> {
    values
        .get(index.ok_or_else(|| format!("{label}を選択してください。"))?)
        .copied()
        .ok_or_else(|| format!("{label}の値が不正です。"))
}

const fn ai_provider_label(value: AiProvider) -> &'static str {
    match value {
        AiProvider::OpenAi => "OpenAI",
        AiProvider::AzureOpenAi => "Azure OpenAI",
        AiProvider::AwsBedrock => "AWS Bedrock",
        AiProvider::Cloudflare => "Cloudflare",
        AiProvider::Custom => "カスタム（Responses互換）",
        AiProvider::ChatGptCodex => "ChatGPT Subscription（Codex CLI）",
    }
}

const fn ai_auth_label(value: AiAuth) -> &'static str {
    match value {
        AiAuth::Bearer => "Bearer",
        AiAuth::ApiKey => "api-key",
        AiAuth::None => "なし",
    }
}

const fn ai_style_label(value: AiStyle) -> &'static str {
    match value {
        AiStyle::Spoken => "話し言葉",
        AiStyle::Polite => "丁寧語",
        AiStyle::Business => "ビジネス",
        AiStyle::Government => "公文書",
        AiStyle::Technical => "技術文書",
        AiStyle::Academic => "論文",
        AiStyle::Contract => "契約",
        AiStyle::Novel => "小説",
        AiStyle::Social => "SNS",
    }
}

const fn ai_effort_label(value: AiEffort) -> &'static str {
    match value {
        AiEffort::ProviderDefault => "プロバイダー既定",
        AiEffort::None => "none",
        AiEffort::Low => "low",
        AiEffort::Medium => "medium",
        AiEffort::High => "high",
        AiEffort::XHigh => "xhigh",
        AiEffort::Max => "max",
    }
}

const fn ai_service_tier_label(value: AiServiceTier) -> &'static str {
    match value {
        AiServiceTier::ProviderDefault => "既定",
        AiServiceTier::Priority => "Priority",
    }
}

fn validate_ai_endpoint(provider: AiProvider, endpoint: &str) -> Result<(), String> {
    if provider == AiProvider::ChatGptCodex {
        return Ok(());
    }
    if endpoint.is_empty() {
        return Err("Endpointを入力してください。".to_owned());
    }
    if endpoint.chars().any(char::is_control)
        || endpoint.chars().any(char::is_whitespace)
        || endpoint.contains(['?', '#', '@'])
    {
        return Err("Endpointに使用できない文字が含まれています。".to_owned());
    }
    if let Some(rest) = endpoint.strip_prefix("https://") {
        return (!rest.is_empty() && !rest.starts_with('/'))
            .then_some(())
            .ok_or_else(|| "Endpointのホスト名を入力してください。".to_owned());
    }
    if let Some(rest) = endpoint.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or_default();
        let host = if authority.starts_with('[') {
            authority
                .split_once(']')
                .map(|(host, _)| format!("{host}]"))
                .unwrap_or_default()
        } else {
            authority.split(':').next().unwrap_or_default().to_owned()
        };
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
            return Ok(());
        }
    }
    Err("EndpointはHTTPSにしてください（HTTPはlocalhostのみ許可）。".to_owned())
}

const fn width_label(value: Width) -> &'static str {
    match value {
        Width::Half => "半角",
        Width::Full => "全角",
        Width::FollowMode => "入力モードに合わせる",
    }
}

fn punctuation_period_index(value: PunctuationStyle) -> usize {
    PeriodMark::ALL
        .iter()
        .position(|candidate| *candidate == value.period)
        .unwrap_or(0)
}

fn punctuation_comma_index(value: PunctuationStyle) -> usize {
    CommaMark::ALL
        .iter()
        .position(|candidate| *candidate == value.comma)
        .unwrap_or(0)
}

fn period_mark_from_index(index: Option<usize>) -> Result<PeriodMark, String> {
    index
        .and_then(|index| PeriodMark::ALL.get(index).copied())
        .ok_or_else(|| "句点の形式を選択してください。".to_owned())
}

fn comma_mark_from_index(index: Option<usize>) -> Result<CommaMark, String> {
    index
        .and_then(|index| CommaMark::ALL.get(index).copied())
        .ok_or_else(|| "読点の形式を選択してください。".to_owned())
}

fn punctuation_from_indices(
    period_index: Option<usize>,
    comma_index: Option<usize>,
) -> Result<PunctuationStyle, String> {
    let period = period_mark_from_index(period_index)?;
    let comma = comma_mark_from_index(comma_index)?;
    Ok(PunctuationStyle::new(comma, period))
}

const fn period_mark_label(value: PeriodMark) -> &'static str {
    match value {
        PeriodMark::Kuten => "。",
        PeriodMark::FullWidth => "．（全角）",
        PeriodMark::HalfWidth => ".（半角）",
    }
}

const fn comma_mark_label(value: CommaMark) -> &'static str {
    match value {
        CommaMark::Touten => "、",
        CommaMark::FullWidth => "，（全角）",
        CommaMark::HalfWidth => ",（半角）",
    }
}

/// The seven controls the notation preset reads and writes.
///
/// Six live on the 文字幅・句読点 page next to the preset; the seventh, space
/// width, lives on 入力補助. These are what actually get saved — the preset
/// itself has no config key and is derived from them on every edit.
fn notation_style_inputs(general: &GeneralControls) -> [HWND; 7] {
    [
        general.normalizer_alnum,
        general.normalizer_number,
        general.normalizer_symbol,
        general.punctuation_period,
        general.punctuation_comma,
        general.punctuation_brackets,
        general.input_assist_space_width,
    ]
}

/// `None` selects the trailing `カスタム` row, which is why this cannot reuse
/// `enum_index`: the combo has one more item than `NotationStyle::ALL`.
fn notation_style_index(value: Option<NotationStyle>) -> usize {
    value.map_or(NotationStyle::ALL.len(), |style| {
        enum_index(&NotationStyle::ALL, style)
    })
}

/// `Ok(None)` is the `カスタム` row — a legitimate selection to *read back*,
/// but not a value to write. Anything past it is a malformed index.
fn notation_style_from_index(index: Option<usize>) -> Result<Option<NotationStyle>, String> {
    let index = index.ok_or_else(|| "表記スタイルを選択してください。".to_owned())?;
    match NotationStyle::ALL.get(index) {
        Some(style) => Ok(Some(*style)),
        None if index == NotationStyle::ALL.len() => Ok(None),
        None => Err("表記スタイルが不正です。".to_owned()),
    }
}

fn bracket_style_index(value: BracketStyle) -> usize {
    BracketStyle::ALL
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn bracket_style_from_index(index: Option<usize>) -> Result<BracketStyle, String> {
    BracketStyle::ALL
        .get(index.ok_or_else(|| "括弧の形式を選択してください。".to_owned())?)
        .copied()
        .ok_or_else(|| "括弧の形式が不正です。".to_owned())
}

const fn bracket_style_label(value: BracketStyle) -> &'static str {
    match value {
        BracketStyle::Corner => "「」",
        BracketStyle::Square => "［］",
    }
}

fn mode_from_index(index: Option<usize>) -> Result<Mode, String> {
    index
        .and_then(|index| Mode::ALL.get(index).copied())
        .ok_or_else(|| "既定の入力モードを選択してください。".to_owned())
}

fn suggest_index(value: SuggestAccept) -> usize {
    SuggestAccept::ALL
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn suggest_from_index(index: Option<usize>) -> Result<SuggestAccept, String> {
    index
        .and_then(|index| SuggestAccept::ALL.get(index).copied())
        .ok_or_else(|| "候補の確定キーを選択してください。".to_owned())
}

fn neural_reranker_scope_index(value: NeuralRerankerScope) -> usize {
    NeuralRerankerScope::ALL
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn neural_reranker_scope_from_index(index: Option<usize>) -> Result<NeuralRerankerScope, String> {
    index
        .and_then(|index| NeuralRerankerScope::ALL.get(index).copied())
        .ok_or_else(|| "AI候補の並べ替え方法を選択してください。".to_owned())
}

const fn neural_reranker_scope_label(value: NeuralRerankerScope) -> &'static str {
    match value {
        NeuralRerankerScope::Off => "使用しない",
        NeuralRerankerScope::LongTextOnly => "長い変換のみ",
        NeuralRerankerScope::AllNormalConversions => "通常の変換すべて",
    }
}

fn appearance_index(value: AppearanceTheme) -> usize {
    AppearanceTheme::ALL
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn appearance_from_index(index: Option<usize>) -> Result<AppearanceTheme, String> {
    index
        .and_then(|index| AppearanceTheme::ALL.get(index).copied())
        .ok_or_else(|| "表示を［自動］［ライト］［ダーク］から選択してください。".to_owned())
}

const fn appearance_label(value: AppearanceTheme) -> &'static str {
    match value {
        AppearanceTheme::Auto => "自動（Windows に合わせる）",
        AppearanceTheme::Light => "ライト",
        AppearanceTheme::Dark => "ダーク",
    }
}

fn pad_shortcut_index(value: PadShortcut) -> usize {
    PadShortcut::ALL
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn pad_shortcut_from_index(index: Option<usize>) -> Result<PadShortcut, String> {
    index
        .and_then(|index| PadShortcut::ALL.get(index).copied())
        .ok_or_else(|| "Sakura Padのショートカットを選択してください。".to_owned())
}

const fn pad_shortcut_label(value: PadShortcut) -> &'static str {
    match value {
        PadShortcut::Disabled => "使わない",
        PadShortcut::DoubleCtrl => "Ctrlを2回",
    }
}

fn windows_apps_use_light_theme() -> bool {
    let mut value = 1u32;
    let mut bytes = size_of::<u32>() as u32;
    // SAFETY: the registry names are static NUL-terminated strings, and the
    // value buffer has the exact REG_DWORD size supplied through `bytes`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            windows::core::w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast::<c_void>()),
            Some(&mut bytes),
        )
    };
    // A missing or unreadable preference must retain a legible UI. Windows
    // itself defaults applications to light when this value is unavailable.
    if status == windows::Win32::Foundation::ERROR_SUCCESS && bytes == size_of::<u32>() as u32 {
        value != 0
    } else {
        true
    }
}

fn high_contrast_enabled() -> bool {
    let mut high_contrast = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: Windows fills the initialized structure; no strings are read.
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

fn apply_title_bar_theme(window: HWND, dark: bool) {
    let value = i32::from(dark);
    // SAFETY: the window is live and DWM copies the four-byte BOOL during this
    // synchronous call. Older Windows can reject this documented attribute;
    // in that case the native title bar remains system themed.
    unsafe {
        let _ = DwmSetWindowAttribute(
            window,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&value as *const i32).cast(),
            size_of::<i32>() as u32,
        );
    }
}

fn apply_common_control_theme(window: HWND, dark: bool) {
    let requested_theme = if dark {
        windows::core::w!("DarkMode_Explorer")
    } else {
        PCWSTR::null()
    };
    // SAFETY: the tree is owned by this UI thread. `SetWindowTheme` is a
    // best-effort request for common-control hover, pressed, focus, disabled,
    // selection, and read-only rendering; a rejection leaves the documented
    // system control theme in place while WM_CTLCOLOR keeps text legible.
    unsafe {
        let _ = SetWindowTheme(window, requested_theme, PCWSTR::null());
        let Ok(mut child) = GetWindow(window, GW_CHILD) else {
            return;
        };
        while !child.0.is_null() {
            apply_common_control_theme(child, dark);
            let Ok(next) = GetWindow(child, GW_HWNDNEXT) else {
                return;
            };
            child = next;
        }
    }
}

fn apply_input_tree_theme(tree: HWND, theme: &UiTheme) {
    let (background, text) = if theme.high_contrast {
        (CLR_NONE, CLR_NONE)
    } else if theme.dark {
        (DARK_INPUT_SURFACE, DARK_INK)
    } else {
        (LIGHT_INPUT_SURFACE, LIGHT_INK)
    };
    // SAFETY: `tree` is the live TreeView owned by the settings UI thread. The
    // messages carry only scalar COLORREF values and repaint synchronously.
    unsafe {
        let _ = SendMessageW(
            tree,
            TVM_SETBKCOLOR,
            Some(WPARAM(0)),
            Some(LPARAM(background.0 as isize)),
        );
        let _ = SendMessageW(
            tree,
            TVM_SETTEXTCOLOR,
            Some(WPARAM(0)),
            Some(LPARAM(text.0 as isize)),
        );
    }
}

unsafe fn invalidate_child_windows(window: HWND) {
    // SAFETY: caller holds the UI-thread ownership of this live child tree.
    unsafe {
        let Ok(mut child) = GetWindow(window, GW_CHILD) else {
            return;
        };
        while !child.0.is_null() {
            let _ = InvalidateRect(Some(child), None, true);
            invalidate_child_windows(child);
            let Ok(next) = GetWindow(child, GW_HWNDNEXT) else {
                return;
            };
            child = next;
        }
    }
}

const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(u32::from_le_bytes([red, green, blue, 0]))
}

fn confirm(message: &str) -> bool {
    message_box(None, message, "Sakura Input", MB_YESNO | MB_ICONWARNING) == IDYES
}

fn message_box(
    parent: Option<HWND>,
    message: &str,
    title: &str,
    style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    let message = to_wide(message);
    let title = to_wide(title);
    // SAFETY: both buffers are NUL-terminated and outlive the synchronous call.
    unsafe {
        MessageBoxW(
            parent,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
        )
    }
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn pump(window: HWND) {
    let mut message = MSG::default();
    // SAFETY: `message` outlives each call. A zero or negative return ends the
    // loop so a failed GetMessage cannot become a busy retry.
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            // A focused ComboBox/ListBox owns its keyboard messages, so an
            // Escape keydown would otherwise never reach the top-level window
            // procedure.  Convert it to the same WM_CLOSE route before
            // IsDialogMessageW can consume it; WM_CLOSE then applies the
            // update-in-flight guard and Cancel semantics in one place.
            if message.message == WM_KEYDOWN && message.wParam.0 as u32 == 0x1b {
                // A native ComboBox owns a separate enabled popup while its
                // list is open. Let that popup consume its first Escape so it
                // closes the list; only a bare property-sheet Escape reaches
                // the top-level Cancel route.
                let popup_is_open = GetWindow(window, GW_ENABLEDPOPUP)
                    .ok()
                    .is_some_and(|popup| popup != window);
                if !popup_is_open {
                    // SAFETY: `window` is the live top-level HWND owned by this
                    // message pump; the posted scalar message has no borrowed data.
                    let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
                    continue;
                }
            }
            // Treat the settings window as a dialog-style keyboard container.
            // WS_EX_CONTROLPARENT on each page lets this walk into the visible
            // custom panel, so Tab/Shift+Tab traverse its native WS_TABSTOP
            // controls instead of being dispatched as unhandled key messages.
            if IsDialogMessageW(window, &message).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe fn app_for_panel(panel: HWND) -> Option<*mut App> {
    // SAFETY: a topic panel can be nested inside its category panel. Walk to
    // the one top-level settings window before reading its UI-thread-owned
    // state. Construction and teardown have no App pointer and retain native
    // system painting.
    unsafe {
        let mut root = GetParent(panel).ok()?;
        while let Ok(parent) = GetParent(root) {
            root = parent;
        }
        let pointer = GetWindowLongPtrW(root, GWLP_USERDATA) as *mut App;
        (!pointer.is_null()).then_some(pointer)
    }
}

unsafe extern "system" fn panel_window_procedure(
    panel: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => {
            // SAFETY: this procedure owns the live panel and only reads root UI state.
            let Some(pointer) = (unsafe { app_for_panel(panel) }) else {
                // SAFETY: unhandled construction/teardown painting is native-owned.
                return unsafe { DefWindowProcW(panel, message, wparam, lparam) };
            };
            let mut rect = RECT::default();
            // SAFETY: the panel and WM_ERASEBKGND device context remain live
            // through the synchronous fill; the root owns the returned brush.
            unsafe {
                let _ = GetClientRect(panel, &mut rect);
                let _ = FillRect(
                    HDC(wparam.0 as *mut c_void),
                    &rect,
                    (&*pointer).theme.surface_brush(),
                );
            }
            LRESULT(1)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            // SAFETY: this procedure owns the live panel and only reads root UI state.
            let Some(pointer) = (unsafe { app_for_panel(panel) }) else {
                // SAFETY: unhandled construction/teardown painting is native-owned.
                return unsafe { DefWindowProcW(panel, message, wparam, lparam) };
            };
            // SAFETY: the panel shares the root's UI thread and WM_CTLCOLOR is
            // synchronous. The child HWND in LPARAM is valid for this call.
            let app = unsafe { &*pointer };
            if let Some(brush) = app.theme.apply_control_colors(
                message,
                HDC(wparam.0 as *mut c_void),
                app.is_readonly_input(HWND(lparam.0 as *mut c_void)),
            ) {
                return LRESULT(brush.0 as isize);
            }
            // SAFETY: forwards untouched scalar message arguments once.
            unsafe { DefWindowProcW(panel, message, wparam, lparam) }
        }
        WM_COMMAND => {
            // Child commands target their immediate panel parent. Forward them
            // synchronously to the root, which owns the App state and already
            // defines every command's terminal success/error handling.
            // SAFETY: this panel has a live root parent for its normal lifetime.
            let Ok(root) = (unsafe { GetParent(panel) }) else {
                // SAFETY: construction/teardown uses default native command handling.
                return unsafe { DefWindowProcW(panel, message, wparam, lparam) };
            };
            // SAFETY: command parameters are scalar values forwarded synchronously.
            unsafe { SendMessageW(root, WM_COMMAND, Some(wparam), Some(lparam)) }
        }
        WM_DRAWITEM => {
            // Owner-draw controls send their documented DRAWITEMSTRUCT to the
            // immediate panel parent. The root owns palette and selected-tab
            // state, so forward the borrowed pointer synchronously only.
            // SAFETY: this panel has a live root parent for its normal lifetime.
            let Ok(root) = (unsafe { GetParent(panel) }) else {
                // SAFETY: construction/teardown uses default native drawing.
                return unsafe { DefWindowProcW(panel, message, wparam, lparam) };
            };
            // SAFETY: WM_DRAWITEM's structure is valid for the synchronous send.
            unsafe { SendMessageW(root, WM_DRAWITEM, Some(wparam), Some(lparam)) }
        }
        _ => {
            // SAFETY: forwards untouched scalar message arguments once.
            unsafe { DefWindowProcW(panel, message, wparam, lparam) }
        }
    }
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => {
            // SAFETY: user data is zero only while the top-level window is
            // being constructed or destroyed; otherwise it is the live App.
            // SAFETY: user data is either zero during construction or the live
            // App pointer installed before the window is shown.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if pointer.is_null() {
                // SAFETY: forwards untouched scalar message arguments once.
                return unsafe { DefWindowProcW(window, message, wparam, lparam) };
            }
            let mut rect = RECT::default();
            // SAFETY: WM_ERASEBKGND supplies a live HDC and this window is live.
            unsafe {
                let _ = GetClientRect(window, &mut rect);
                let _ = FillRect(
                    HDC(wparam.0 as *mut c_void),
                    &rect,
                    (&*pointer).theme.surface_brush(),
                );
            }
            LRESULT(1)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            // SAFETY: the pointer follows the same UI-thread lifetime contract
            // as the command handler. WM_CTLCOLOR is synchronous, so the HDC
            // in WPARAM remains valid while its palette is selected.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if !pointer.is_null() {
                // SAFETY: the root stores this App pointer on its owning UI thread.
                let app = unsafe { &*pointer };
                if let Some(brush) = app.theme.apply_control_colors(
                    message,
                    HDC(wparam.0 as *mut c_void),
                    app.is_readonly_input(HWND(lparam.0 as *mut c_void)),
                ) {
                    return LRESULT(brush.0 as isize);
                }
            }
            // SAFETY: forwards untouched scalar message arguments once.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_UPDATE_COMPLETE => {
            let completion = wparam.0 as *mut UpdateCompletion;
            if completion.is_null() {
                return LRESULT(0);
            }
            // SAFETY: the worker transfers exactly one Box allocation in
            // WPARAM after a successful PostMessageW.
            let completion = unsafe { Box::from_raw(completion) };
            // SAFETY: user data is either zero during construction/destruction
            // or the live Box<App> installed by `run`.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if !pointer.is_null() {
                // SAFETY: update completions are handled only on the owning UI
                // thread and the App box outlives the message pump.
                unsafe { &mut *pointer }.finish_update(*completion);
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            // SAFETY: WM_DRAWITEM supplies a valid DRAWITEMSTRUCT for this
            // synchronous message, or a null value that is safely ignored.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            let item = lparam.0 as *const DRAWITEMSTRUCT;
            if !pointer.is_null() && !item.is_null() {
                // SAFETY: the root stores this App pointer on its owning UI
                // thread; the draw structure remains live for this call only.
                if unsafe { &*pointer }.draw_button(unsafe { &*item }) {
                    return LRESULT(1);
                }
            }
            // SAFETY: native controls retain the fallback for non-dark drawing.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_NOTIFY => {
            // TreeView selection is a WM_NOTIFY payload rather than a
            // WM_COMMAND. Only the live input tree is allowed to change the
            // visible right-hand topic panel.
            // SAFETY: user data is either zero during construction or the live
            // App pointer installed before the window is shown.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            let notification = lparam.0 as *const NMTREEVIEWW;
            if !pointer.is_null() && !notification.is_null() {
                // SAFETY: TreeView keeps the NMTREEVIEWW payload live for this
                // synchronous notification; the root owns the App pointer.
                let notification = unsafe { &*notification };
                // SAFETY: the pointer is non-null and remains owned by this UI
                // thread for the duration of this synchronous notification.
                if notification.hdr.hwndFrom == unsafe { (*pointer).input_tree }
                    && notification.hdr.code == TVN_SELCHANGEDW
                {
                    let topic = notification.itemNew.lParam.0;
                    if topic as usize == TREE_GROUP {
                        // Category nodes do not own a right-hand page.  Keep
                        // the left selection and the visible heading in sync by
                        // immediately normalizing a category click to its first
                        // real settings child (for example 入力支援 → 入力誤りの自動修復).
                        // SAFETY: `pointer` is the live App owned by this UI
                        // thread for the duration of this notification.
                        let input_tree = unsafe { (*pointer).input_tree };
                        if let Some(child) =
                            first_input_tree_child(input_tree, notification.itemNew.hItem)
                        {
                            select_input_tree_item(input_tree, child);
                        }
                    } else if topic >= 0 {
                        // SAFETY: this UI thread owns the App for the lifetime
                        // of the message pump.
                        unsafe { &mut *pointer }.show_topic_controls(topic as usize);
                    }
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            // SAFETY: user data is either zero during window construction or
            // the `Box<App>` installed before the window is shown.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if !pointer.is_null() {
                let source = HWND(lparam.0 as *mut c_void);
                let notification = ((wparam.0 >> 16) & 0xffff) as u16;
                // SAFETY: only this UI thread handles commands and the box
                // outlives the message pump.
                let app = unsafe { &mut *pointer };
                if let Err(error) = app.handle_command(source, notification) {
                    app.set_status(&format!("エラー: {error}"));
                    message_box(
                        Some(window),
                        &error,
                        "Sakura Input プロパティ",
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // The root owns every child in the property-sheet grid. Handling
            // the notification here keeps the ATOK-like rail, right pane, and
            // persistent action row on one scale instead of leaving a 100%
            // child layout inside a 150% window.
            // SAFETY: user data is either zero during construction or the live
            // App pointer installed before the window is shown.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if !pointer.is_null() {
                let new_dpi = (wparam.0 as u32 & 0xffff).max(96);
                // SAFETY: the suggested RECT is owned by User32 for the
                // duration of this synchronous notification; the handler
                // copies it before returning.
                unsafe { &mut *pointer }.apply_dpi_change(new_dpi, suggested_rect(lparam));
                return LRESULT(0);
            }
            // SAFETY: preserve default construction-time handling when no App
            // state has been installed yet.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            // SAFETY: the App pointer is owned by this thread and remains live
            // until the message pump ends. Auto re-reads the Windows app-theme
            // preference; high-contrast changes always supersede a user choice.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if !pointer.is_null() {
                // SAFETY: the root stores this App pointer on its owning UI thread.
                unsafe { &mut *pointer }.refresh_auto_appearance();
            }
            // SAFETY: forwards untouched scalar message arguments once.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_KEYDOWN if wparam.0 as u32 == 0x1b => {
            // Escape is a property-sheet cancel affordance even when focus is
            // on a custom panel.  Route it through the same close policy as
            // the caption and bottom Cancel button so an in-flight update
            // cannot be abandoned accidentally.
            // SAFETY: this reads the App pointer owned by the live top-level
            // window; construction and teardown keep it null outside that
            // lifetime.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if !pointer.is_null() {
                // SAFETY: the App pointer belongs to this UI thread and lives
                // until the message pump exits.
                unsafe { &*pointer }.request_close(CloseRequest::Cancel);
                return LRESULT(0);
            }
            // SAFETY: construction has no live App state; preserve native
            // handling for that narrow branch.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_CLOSE => {
            // SAFETY: user data follows the same lifetime contract as in the
            // command and completion handlers above.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if !pointer.is_null() {
                // SAFETY: the non-null pointer is the App installed in this
                // window's GWLP_USERDATA and remains live until WM_NCDESTROY.
                unsafe { &*pointer }.request_close(CloseRequest::Window);
                return LRESULT(0);
            }
            // SAFETY: construction/teardown has no live App state to guard;
            // preserve the native close terminal state in that narrow branch.
            unsafe {
                let _ = DestroyWindow(window);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends the one message pump owned by this thread.
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: unhandled messages are delegated to the system exactly
            // once, with the original scalar payloads.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_mappings_cover_every_mode_suggest_binding_and_dictionary_format() {
        for mode in Mode::ALL {
            assert_eq!(mode_from_index(Some(mode_index(mode))), Ok(mode));
        }
        for binding in SuggestAccept::ALL {
            assert_eq!(
                suggest_from_index(Some(suggest_index(binding))),
                Ok(binding)
            );
        }
        for key in AiTextKey::ALL {
            assert_eq!(
                ai_text_key_from_index(Some(ai_text_key_index(key))),
                Ok(key)
            );
        }
        for scope in NeuralRerankerScope::ALL {
            assert_eq!(
                neural_reranker_scope_from_index(Some(neural_reranker_scope_index(scope))),
                Ok(scope)
            );
        }
        for (index, format) in DictionaryFormat::ALL.into_iter().enumerate() {
            assert_eq!(dictionary_format(Some(index + 1), false), Ok(Some(format)));
        }
        assert_eq!(dictionary_format(Some(0), true), Ok(None));
        assert!(dictionary_format(Some(0), false).is_err());
        assert!(neural_reranker_scope_from_index(None).is_err());
        assert!(neural_reranker_scope_from_index(Some(NeuralRerankerScope::ALL.len())).is_err());
    }

    #[test]
    fn input_method_controls_use_japanese_labels_and_one_selection() {
        assert_eq!(input_method_label(InputMethod::Romaji), "ローマ字入力");
        assert_eq!(input_method_label(InputMethod::Kana), "カナ入力");
        assert_eq!(
            input_method_from_checks(true, false),
            Ok(InputMethod::Romaji)
        );
        assert_eq!(input_method_from_checks(false, true), Ok(InputMethod::Kana));
        assert!(input_method_from_checks(false, false).is_err());
        assert!(input_method_from_checks(true, true).is_err());
    }

    #[test]
    fn normalizer_mappings_cover_width_and_punctuation_choices() {
        for width in [Width::Half, Width::Full, Width::FollowMode] {
            assert_eq!(width_from_index(Some(width_index(width))), Ok(width));
        }
        for punctuation in PunctuationStyle::ALL {
            assert_eq!(
                punctuation_from_indices(
                    Some(punctuation_period_index(punctuation)),
                    Some(punctuation_comma_index(punctuation)),
                ),
                Ok(punctuation)
            );
        }
        assert!(width_from_index(None).is_err());
        assert!(punctuation_from_indices(Some(0), None).is_err());
        assert_eq!(width_label(Width::FollowMode), "入力モードに合わせる");
        // period index 1 = ．, comma index 0 = 、
        assert_eq!(
            punctuation_from_indices(Some(1), Some(0)),
            Ok(PunctuationStyle::MIXED)
        );
        // period index 2 = ., comma index 2 = ,
        assert_eq!(
            punctuation_from_indices(Some(2), Some(2)),
            Ok(PunctuationStyle::ASCII)
        );
        assert!(punctuation_from_indices(Some(3), Some(0)).is_err());
        assert!(punctuation_from_indices(Some(0), Some(3)).is_err());
    }

    #[test]
    fn notation_style_indices_round_trip_and_reserve_the_custom_row() {
        // The combo carries one more row than `NotationStyle::ALL`, so the
        // two directions have to agree about which row that extra one is.
        for style in NotationStyle::ALL {
            assert_eq!(
                notation_style_from_index(Some(notation_style_index(Some(style)))),
                Ok(Some(style)),
                "{style:?}"
            );
        }
        assert_eq!(notation_style_index(None), NotationStyle::ALL.len());
        assert_eq!(
            notation_style_from_index(Some(NotationStyle::ALL.len())),
            Ok(None),
            "the trailing row reads back as `no style`, not as an error"
        );
        assert!(notation_style_from_index(Some(NotationStyle::ALL.len() + 1)).is_err());
        assert!(notation_style_from_index(None).is_err());
    }

    #[test]
    fn space_width_labels_are_shared_between_the_combo_and_the_preset_status() {
        // `apply_notation_style` names the space width it changed on a page
        // the reader is not looking at. That sentence and the combo row it
        // refers to have to be the same string.
        for space_width in SpaceWidth::ALL {
            assert_eq!(
                space_width_from_index(Some(space_width_index(space_width))),
                Ok(space_width)
            );
        }
        assert_eq!(
            space_width_label(SpaceWidth::SameAsInput),
            "入力文字種と同じ"
        );
        assert_eq!(space_width_label(SpaceWidth::Full), "常に全角");
        assert_eq!(space_width_label(SpaceWidth::Half), "常に半角");
    }

    #[test]
    fn notation_style_labels_are_distinct_and_never_collide_with_the_custom_row() {
        // The combo is keyed by row order, but a reader picks by label. Two
        // styles reading the same, or one reading `カスタム`, would make the
        // control unusable without failing anything else.
        for (index, style) in NotationStyle::ALL.into_iter().enumerate() {
            assert!(!style.label().is_empty(), "{style:?}");
            assert_ne!(style.label(), NOTATION_STYLE_CUSTOM_LABEL, "{style:?}");
            for other in NotationStyle::ALL.into_iter().skip(index + 1) {
                assert_ne!(style.label(), other.label(), "{style:?} vs {other:?}");
            }
        }
    }

    #[test]
    fn punctuation_combo_labels_are_japanese_in_canonical_order() {
        let period_labels: Vec<_> = PeriodMark::ALL.into_iter().map(period_mark_label).collect();
        assert_eq!(period_labels, ["。", "．（全角）", ".（半角）"]);
        let comma_labels: Vec<_> = CommaMark::ALL.into_iter().map(comma_mark_label).collect();
        assert_eq!(comma_labels, ["、", "，（全角）", ",（半角）"]);
    }

    #[test]
    fn neural_reranker_scope_labels_are_japanese_in_canonical_combo_order() {
        let labels: Vec<_> = NeuralRerankerScope::ALL
            .into_iter()
            .map(neural_reranker_scope_label)
            .collect();
        assert_eq!(labels, ["使用しない", "長い変換のみ", "通常の変換すべて"]);
    }

    #[test]
    fn appearance_mapping_covers_auto_light_and_dark_without_invalid_fallbacks() {
        assert_eq!(AppearanceTheme::ALL.len(), 3);
        for appearance in AppearanceTheme::ALL {
            assert_eq!(
                appearance_from_index(Some(appearance_index(appearance))),
                Ok(appearance)
            );
        }
        assert!(appearance_from_index(None).is_err());
        assert!(appearance_from_index(Some(AppearanceTheme::ALL.len())).is_err());
    }

    #[test]
    fn appearance_labels_are_japanese_in_canonical_combo_order() {
        let labels: Vec<_> = AppearanceTheme::ALL
            .into_iter()
            .map(appearance_label)
            .collect();
        assert_eq!(labels, ["自動（Windows に合わせる）", "ライト", "ダーク"]);
    }

    #[test]
    fn pad_shortcut_mapping_is_bounded_and_japanese() {
        for shortcut in PadShortcut::ALL {
            assert_eq!(
                pad_shortcut_from_index(Some(pad_shortcut_index(shortcut))),
                Ok(shortcut)
            );
        }
        assert!(pad_shortcut_from_index(None).is_err());
        assert!(pad_shortcut_from_index(Some(PadShortcut::ALL.len())).is_err());
        let labels: Vec<_> = PadShortcut::ALL
            .into_iter()
            .map(pad_shortcut_label)
            .collect();
        assert_eq!(labels, ["使わない", "Ctrlを2回"]);
    }

    #[test]
    fn update_status_is_japanese_at_the_settings_presentation_boundary() {
        assert_eq!(
            App::describe_update_check(&updater::UpdateCheckOutcome::Disabled),
            "自動更新は無効です（ネットワーク通信は行いません）。"
        );
        let failure = updater::UpdateFailure {
            stage: updater::UpdateStage::InstallerHash,
            message: "digest mismatch".to_owned(),
        };
        let failure_text =
            App::describe_update_check(&updater::UpdateCheckOutcome::Failed(failure.clone()));
        assert!(failure_text.starts_with("インストーラーのSHA-256確認に失敗しました: "));
        assert!(failure_text.ends_with("digest mismatch"));
        let installed = App::describe_update(&updater::UpdateOutcome::Installed {
            version: updater::Version {
                major: 1,
                minor: 2,
                patch: 3,
            },
        });
        assert_eq!(installed, "Sakura Input 1.2.3 をインストールしました。");
    }

    #[test]
    fn update_available_prompt_names_current_and_new_versions() {
        let prompt = App::update_available_prompt(updater::Version {
            major: 9,
            minor: 8,
            patch: 7,
        });
        assert!(prompt.contains(&format!("現在のバージョン: {}", updater::current_version())));
        assert!(prompt.contains("更新後のバージョン: 9.8.7"));
        assert!(prompt.contains("管理者権限の確認が表示されます。"));
    }

    #[test]
    fn mode_labels_are_japanese_and_cover_every_mode() {
        let labels: Vec<_> = Mode::ALL.into_iter().map(mode_label).collect();
        assert_eq!(labels.len(), Mode::ALL.len());
        assert_eq!(labels[mode_index(Mode::Hiragana)], "ひらがな");
        assert_eq!(labels[mode_index(Mode::FullAlnum)], "全角英数");
    }

    #[test]
    fn compact_property_sheet_geometry_keeps_the_navigation_and_actions_separate() {
        assert_eq!((WINDOW_WIDTH, WINDOW_HEIGHT), (629, 464));
        assert_eq!(PANEL_LEFT, INPUT_TREE_LEFT + INPUT_TREE_WIDTH + 14);
        let outer_width = std::hint::black_box(WINDOW_WIDTH);
        let tree_left = std::hint::black_box(INPUT_TREE_LEFT);
        let tree_width = std::hint::black_box(INPUT_TREE_WIDTH);
        let tree_top = std::hint::black_box(INPUT_TREE_TOP);
        let tree_height = std::hint::black_box(INPUT_TREE_HEIGHT);
        let action_row = std::hint::black_box(380);
        assert!(PANEL_LEFT + PANEL_WIDTH < outer_width);
        assert!(tree_left + tree_width < PANEL_LEFT);
        assert!(tree_top + tree_height <= action_row);
    }

    #[test]
    fn dictionary_content_stays_above_the_persistent_action_row() {
        let content_bottom = std::hint::black_box(PANEL_TOP + DICTIONARY_CONTENT_BOTTOM);
        let action_row = std::hint::black_box(BOTTOM_ACTION_Y);
        assert!(content_bottom < action_row);
    }

    #[test]
    fn every_close_route_waits_for_an_in_flight_update_then_closes_after_completion() {
        for request in [CloseRequest::Ok, CloseRequest::Cancel, CloseRequest::Window] {
            assert_eq!(
                close_decision(request, true),
                CloseDecision::WaitForUpdate,
                "{request:?} must keep the settings window open while an update owns its result"
            );
            assert_eq!(
                close_decision(request, false),
                CloseDecision::Destroy,
                "{request:?} must close normally after the update terminal state"
            );
        }
    }

    #[test]
    fn light_settings_palette_matches_the_candidate_popup_roles() {
        assert_eq!(LIGHT_SURFACE, rgb(0xF7, 0xF6, 0xF4));
        assert_eq!(LIGHT_INK, rgb(0x2F, 0x2F, 0x2F));
        assert_eq!(LIGHT_DISABLED_INK, rgb(0x70, 0x70, 0x70));
        assert_eq!(LIGHT_INPUT_SURFACE, rgb(0xE8, 0xE5, 0xE2));
        assert_eq!(LIGHT_BUTTON_BORDER, rgb(0xBD, 0xB9, 0xB5));
        assert_eq!(LIGHT_SAKURA_ACCENT, rgb(0xB2, 0x8D, 0x96));
    }

    #[test]
    fn high_contrast_theme_leaves_colors_to_system_roles() {
        let theme = UiTheme {
            dark: false,
            high_contrast: true,
            brushes: None,
        };
        assert!(theme.brushes.is_none());
        assert!(theme
            .apply_control_colors(WM_CTLCOLORSTATIC, HDC::default(), false)
            .is_none());
    }

    #[test]
    fn input_assist_space_controls_have_stable_orders() {
        assert_eq!(space_width_index(SpaceWidth::SameAsInput), 0);
        assert_eq!(space_width_index(SpaceWidth::Full), 1);
        assert_eq!(space_width_index(SpaceWidth::Half), 2);
        assert_eq!(shift_space_behavior_index(ShiftSpaceBehavior::Opposite), 0);
        assert_eq!(shift_space_behavior_index(ShiftSpaceBehavior::Full), 1);
        assert_eq!(shift_space_behavior_index(ShiftSpaceBehavior::Half), 2);
        assert_eq!(space_width_from_index(Some(0)), Ok(SpaceWidth::SameAsInput));
        assert_eq!(
            shift_space_behavior_from_index(Some(0)),
            Ok(ShiftSpaceBehavior::Opposite)
        );
        assert_eq!(width_from_index(Some(0)), Ok(Width::Half));
        assert_eq!(width_from_index(Some(1)), Ok(Width::Full));
        assert_eq!(width_from_index(Some(2)), Ok(Width::FollowMode));
    }

    #[test]
    fn light_initial_frame_shows_only_the_selected_input_topic() {
        register_window_class().expect("settings window class registers");
        let window = create_main_window().expect("settings root window creates");
        let mut app = Box::new(App::new(window).expect("settings controls create"));
        app.configuration.preferences.appearance_theme = AppearanceTheme::Light;
        select_combo(
            app.general.appearance,
            appearance_index(AppearanceTheme::Light),
        );
        app.theme = UiTheme::resolve(AppearanceTheme::Light);
        app.apply_theme();
        app.show_page_controls(0);
        let app = Box::into_raw(app);
        // SAFETY: the boxed App outlives this window's synchronous first show
        // and is removed only after the root has been destroyed.
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, app as isize);
            let _ = ShowWindow(window, SW_SHOW);
            let _ = UpdateWindow(window);
            assert!(!IsWindowVisible((&*app).general.appearance).as_bool());
            assert!(IsWindowVisible((&*app).input_tree).as_bool());
            assert!(!IsWindowVisible((&*app).page_topics).as_bool());
            assert!(IsWindowVisible((&*app).general.basic_panel).as_bool());
            assert!(IsWindowVisible((&*app).apply).as_bool());
            assert_eq!(list_count((&*app).page_topics), Some(2));
            assert_eq!(
                list_text((&*app).page_topics, 0).as_deref(),
                Some("基本設定")
            );
            assert_eq!(
                list_text((&*app).page_topics, 1).as_deref(),
                Some("アプリ別の設定")
            );
            assert_eq!(list_index((&*app).page_topics), Some(0));
            assert_ne!(
                GetWindowLongPtrW((&*app).general.appearance, GWL_STYLE) as u32 & WS_VISIBLE.0,
                0
            );
            assert_ne!(
                GetWindowLongPtrW((&*app).apply, GWL_STYLE) as u32 & WS_VISIBLE.0,
                0
            );
            (*app).show_topic_controls(1);
            assert!(!IsWindowVisible((&*app).general.basic_panel).as_bool());
            assert!(!IsWindowVisible((&*app).general.input_assist_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.profile_panel).as_bool());
            assert!(!IsWindowVisible((&*app).general.appearance).as_bool());
            assert!(IsWindowVisible((&*app).general.profile_list).as_bool());
            (*app).show_topic_controls(INPUT_TOPIC_SEGMENT);
            assert!(!IsWindowVisible((&*app).general.profile_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.segment_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.neural_reranker_scope).as_bool());
            assert!(!IsWindowVisible((&*app).general.basic_panel).as_bool());
            assert!(!IsWindowVisible((&*app).general.normalizer_panel).as_bool());
            (*app).show_topic_controls(INPUT_TOPIC_NORMALIZER);
            assert!(!IsWindowVisible((&*app).general.segment_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.normalizer_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.normalizer_reset).as_bool());
            assert!(!IsWindowVisible((&*app).general.neural_reranker_scope).as_bool());
            (*app).show_topic_controls(INPUT_TOPIC_INPUT_ASSIST);
            assert!(!IsWindowVisible((&*app).general.basic_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.input_assist_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.input_assist_space_width).as_bool());
            assert!(IsWindowVisible((&*app).general.input_assist_shift_space).as_bool());
            assert!(!IsWindowVisible((&*app).general.segment_panel).as_bool());
            (*app).show_topic_controls(INPUT_TOPIC_PREDICTION);
            assert!(IsWindowVisible((&*app).general.prediction_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.prediction).as_bool());
            assert!(IsWindowVisible((&*app).general.suggest).as_bool());
            assert!(!IsWindowVisible((&*app).general.segment_panel).as_bool());
            (*app).show_topic_controls(INPUT_TOPIC_DISPLAY);
            assert!(IsWindowVisible((&*app).general.display_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.appearance).as_bool());
            assert!(!IsWindowVisible((&*app).general.prediction_panel).as_bool());
            (*app).show_topic_controls(INPUT_TOPIC_ASSOCIATION);
            assert!(!IsWindowVisible((&*app).general.profile_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.association_panel).as_bool());
            assert!(IsWindowVisible((&*app).general.association).as_bool());
            assert!(!IsWindowVisible((&*app).general.neural_reranker_scope).as_bool());
            assert!(!IsWindowVisible((&*app).general.display_panel).as_bool());
            let _ = DestroyWindow(window);
            drop(Box::from_raw(app));
        }
    }

    #[test]
    fn settings_window_uses_the_sakura_input_icon() {
        register_window_class().expect("settings window class registers");
        let window = create_main_window().expect("settings root window creates");
        let icons = apply_window_icons(window).expect("Sakura Input icon asset loads");
        // SAFETY: `window` is the live HWND created immediately above; these
        // messages only query the icon handles currently associated with it.
        let (big, small) = unsafe {
            (
                SendMessageW(window, WM_GETICON, Some(WPARAM(ICON_BIG as usize)), None).0,
                SendMessageW(window, WM_GETICON, Some(WPARAM(ICON_SMALL as usize)), None).0,
            )
        };
        // The window must be destroyed before WindowIcons drops its handles;
        // WM_SETICON borrows them rather than transferring ownership.
        // SAFETY: this test owns the live top-level window and destroys it once.
        unsafe {
            let _ = DestroyWindow(window);
        }
        drop(icons);
        assert_ne!(big, 0, "settings window has no large icon");
        assert_ne!(small, 0, "settings window has no small icon");
    }

    #[test]
    fn every_tab_has_its_expected_japanese_settings_topics() {
        assert_eq!(topics_for_panel(0), ["基本設定", "アプリ別の設定"]);
        assert_eq!(topics_for_panel(1), ["登録単語", "辞書ファイルの入出力"]);
        assert_eq!(topics_for_panel(2), ["学習履歴", "操作"]);
        assert_eq!(topics_for_panel(3), ["診断情報"]);
        assert_eq!(
            topics_for_panel(4),
            ["更新の設定", "利用可能な更新", "更新の状態"]
        );
        assert!(topics_for_panel(PANEL_COUNT).is_empty());
    }

    #[test]
    fn input_tree_lists_only_real_sakura_topics_through_association() {
        assert_eq!(
            INPUT_TREE_LABELS,
            [
                "基本",
                "入力補助",
                "AI文章変換",
                "変換補助",
                "文節変換",
                "文字幅・句読点",
                "表示",
                "入力支援",
                "入力誤りの自動修復",
                "英単語・記号置換",
                "推測変換",
                "連想変換",
                "アプリ別の設定",
            ]
        );
        assert!(
            INPUT_TREE_LABELS
                .iter()
                .position(|label| *label == "連想変換")
                .expect("association topic")
                > INPUT_TREE_LABELS
                    .iter()
                    .position(|label| *label == "文節変換")
                    .expect("segment topic")
        );
        assert_eq!(
            INPUT_TREE_LABELS
                .iter()
                .position(|label| *label == "入力誤りの自動修復"),
            Some(8)
        );
    }

    #[test]
    fn status_text_is_single_line_and_never_exceeds_the_reserved_slot() {
        assert_eq!(
            compact_status("既定の設定を保存しました。"),
            "既定の設定を保存しました。"
        );
        let compact = compact_status(
            "既定の設定を保存しました: C:\\Users\\developer\\AppData\\Local\\SakuraInput\\config\\config.toml",
        );
        assert!(compact.chars().count() <= 27);
        assert!(compact.ends_with('…'));
        assert!(!compact.contains('\r'));
        assert!(!compact.contains('\n'));
    }

    #[test]
    fn rgb_uses_win32_colorref_channel_order() {
        assert_eq!(rgb(0x35, 0x35, 0x35), DARK_SURFACE);
        assert_eq!(rgb(0x25, 0x25, 0x25), DARK_INPUT_SURFACE);
        assert_eq!(rgb(0xF5, 0xF3, 0xF1), DARK_INK);
        assert_eq!(rgb(0xB7, 0x7C, 0x8C), SAKURA_ACCENT);
    }

    #[test]
    fn button_style_restores_native_semantics_outside_dark_owner_draw() {
        assert_eq!(button_type_style(true, false), BS_OWNERDRAW as u32);
        assert_eq!(button_type_style(true, true), BS_OWNERDRAW as u32);
        assert_eq!(button_type_style(false, false), BS_PUSHBUTTON as u32);
        assert_eq!(button_type_style(false, true), BS_DEFPUSHBUTTON as u32);
    }
}
