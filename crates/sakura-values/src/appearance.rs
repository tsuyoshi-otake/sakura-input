/// Controls whether Sakura-owned UI follows the system appearance or uses an
/// explicit light or dark palette.
///
/// This is part of the renderer protocol because the engine is the authority
/// that loads the user-wide preference while the renderer owns the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum AppearanceTheme {
    #[default]
    Auto = 0,
    Light = 1,
    Dark = 2,
}

impl AppearanceTheme {
    /// All supported appearance themes, in declaration order.
    pub const ALL: [AppearanceTheme; 3] = [
        AppearanceTheme::Auto,
        AppearanceTheme::Light,
        AppearanceTheme::Dark,
    ];

    /// The canonical name used in preference files and settings surfaces.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Parses the canonical preference-file name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// Selects the explicit shortcut used to show or focus Sakura Pad.
///
/// The renderer receives this value as part of `UiState`.  Keeping
/// the enum in this shared crate makes the configuration authority and the
/// renderer share one bounded, strict representation instead of parsing the
/// user configuration at the UI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum PadShortcut {
    /// Sakura Pad has no keyboard shortcut.
    #[default]
    Disabled = 0,
    /// Press either Control key twice in succession.
    DoubleCtrl = 1,
}

impl PadShortcut {
    /// All supported shortcuts, in the canonical settings order.
    pub const ALL: [Self; 2] = [Self::Disabled, Self::DoubleCtrl];

    /// The canonical name used in preference files and settings surfaces.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::DoubleCtrl => "double-ctrl",
        }
    }

    /// Parses the canonical preference-file name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "disabled" => Some(Self::Disabled),
            "double-ctrl" => Some(Self::DoubleCtrl),
            _ => None,
        }
    }
}
