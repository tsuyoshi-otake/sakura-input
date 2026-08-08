//! Versioned user preferences built on Sakura's deliberately small TOML subset.
//!
//! Unknown and missing fields are ignored/defaulted so a newer settings tool
//! cannot brick an older engine. Parsing still rejects structurally malformed
//! TOML: silently guessing where a broken quote ended would be less safe than
//! retaining the last known-good configuration at the file-loading layer.

use crate::config::{self, Document, ParseError};
use crate::keymap::Preset;
use crate::width::{Normalizer, PunctuationStyle, Width, WidthPolicy};
use sakura_proto::Mode;

pub const CONFIG_FORMAT_VERSION: u16 = 4;
pub const PREVIOUS_CONFIG_FORMAT_VERSION: u16 = 3;
const LEGACY_CONFIG_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SuggestAccept {
    #[default]
    Tab,
    ShiftEnter,
    Disabled,
}

impl SuggestAccept {
    pub const ALL: [Self; 3] = [Self::Tab, Self::ShiftEnter, Self::Disabled];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Tab => "tab",
            Self::ShiftEnter => "shift-enter",
            Self::Disabled => "disabled",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "tab" => Some(Self::Tab),
            "shift-enter" => Some(Self::ShiftEnter),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    pub keymap_preset: Preset,
    pub normalizer: Normalizer,
    pub prediction_enabled: bool,
    pub suggest_accept: SuggestAccept,
    /// Enables the explicitly opt-in developer interaction history. The
    /// engine keeps this separate from ordinary learning so a normal install
    /// never records raw key events.
    pub developer_mode: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            keymap_preset: Preset::MsIme,
            normalizer: Normalizer::default(),
            prediction_enabled: true,
            suggest_accept: SuggestAccept::Tab,
            developer_mode: false,
        }
    }
}

/// Fully resolved defaults for one host executable.
///
/// Profiles are resolved while loading configuration rather than on the key
/// path. A newly created context copies these four small values and thereafter
/// owns its mode, so refocusing an editor can never silently overwrite an
/// explicit mode change made by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppProfile {
    pub process_name: String,
    pub default_mode: Mode,
    pub normalizer: Normalizer,
    pub prediction_enabled: bool,
    pub suggest_accept: SuggestAccept,
}

impl AppProfile {
    fn inherited(process_name: &str, preferences: Preferences) -> Self {
        Self {
            process_name: process_name.to_owned(),
            default_mode: Mode::Hiragana,
            normalizer: preferences.normalizer,
            prediction_enabled: preferences.prediction_enabled,
            suggest_accept: preferences.suggest_accept,
        }
    }

    pub fn matches(&self, process_name: &str) -> bool {
        self.process_name.eq_ignore_ascii_case(process_name)
    }
}

/// The values copied into a newly created editing context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPreferences {
    pub default_mode: Mode,
    pub normalizer: Normalizer,
    pub prediction_enabled: bool,
    pub suggest_accept: SuggestAccept,
}

/// Shipped profiles protect shell/IDE Tab completion before a user has opened
/// settings. User sections with the same process name override these values.
pub fn default_app_profiles(preferences: Preferences) -> Vec<AppProfile> {
    ["WindowsTerminal.exe", "Code.exe", "devenv.exe"]
        .into_iter()
        .map(|process_name| {
            let mut profile = AppProfile::inherited(process_name, preferences);
            // Start shell and IDE contexts with the IME genuinely off. Using
            // HalfAlnum here made the first half-width/full-width press switch
            // to Direct instead of Japanese; Direct makes that same first
            // press enter Hiragana, matching the system IME convention while
            // leaving ordinary shortcuts and typing entirely to the host.
            profile.default_mode = Mode::Direct;
            profile.prediction_enabled = false;
            profile.suggest_accept = SuggestAccept::Disabled;
            profile
        })
        .collect()
}

pub fn resolve_context_preferences(
    preferences: Preferences,
    profiles: &[AppProfile],
    process_name: &str,
) -> ContextPreferences {
    if let Some(profile) = profiles
        .iter()
        .find(|profile| profile.matches(process_name))
    {
        return ContextPreferences {
            default_mode: profile.default_mode,
            normalizer: profile.normalizer,
            prediction_enabled: profile.prediction_enabled,
            suggest_accept: profile.suggest_accept,
        };
    }
    ContextPreferences {
        default_mode: Mode::Hiragana,
        normalizer: preferences.normalizer,
        prediction_enabled: preferences.prediction_enabled,
        suggest_accept: preferences.suggest_accept,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPreferences {
    /// Version declared by the source. Missing/malformed values are treated as
    /// v1, whose known fields are still recoverable.
    pub source_version: u16,
    pub preferences: Preferences,
    pub profiles: Vec<AppProfile>,
}

impl ParsedPreferences {
    pub const fn needs_upgrade(&self) -> bool {
        self.source_version != CONFIG_FORMAT_VERSION
    }
}

pub fn parse_preferences(source: &str) -> Result<ParsedPreferences, ParseError> {
    let document = config::parse(source)?;
    let source_version = text(&document, "meta", "format-version")
        .or_else(|| text(&document, "settings", "format-version"))
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(PREVIOUS_CONFIG_FORMAT_VERSION);

    let mut preferences = Preferences::default();
    let input_section = if source_version <= LEGACY_CONFIG_FORMAT_VERSION {
        "settings"
    } else {
        "input"
    };
    if let Some(preset) = text(&document, input_section, "keymap-preset")
        .or_else(|| text(&document, input_section, "keymap"))
        .and_then(Preset::from_name)
    {
        preferences.keymap_preset = preset;
    }
    if let Some(enabled) = text(&document, input_section, "prediction-enabled")
        .or_else(|| text(&document, input_section, "prediction"))
        .and_then(parse_bool)
    {
        preferences.prediction_enabled = enabled;
    }
    if let Some(accept) =
        text(&document, input_section, "suggest-accept").and_then(SuggestAccept::from_name)
    {
        preferences.suggest_accept = accept;
    }
    if let Some(enabled) = text(&document, input_section, "developer-mode").and_then(parse_bool) {
        preferences.developer_mode = enabled;
    }

    let mut width = WidthPolicy::default();
    if let Some(value) = text(&document, "width", "alnum").and_then(parse_width) {
        width.alnum = value;
    }
    if let Some(value) = text(&document, "width", "number").and_then(parse_width) {
        width.number = value;
    }
    if let Some(value) = text(&document, "width", "symbol").and_then(parse_width) {
        width.symbol = value;
    }
    let punctuation = text(&document, "width", "punctuation")
        .and_then(parse_punctuation)
        .unwrap_or_default();
    preferences.normalizer = Normalizer { width, punctuation };

    let profiles = parse_app_profiles(&document, preferences);

    Ok(ParsedPreferences {
        source_version,
        preferences,
        profiles,
    })
}

/// Emits the current canonical format. Values are a quoted TOML subset so the
/// same small parser serves shipped key maps and user preferences.
pub fn serialize_preferences(preferences: Preferences) -> String {
    serialize_preferences_with_profiles(preferences, &default_app_profiles(preferences))
}

/// Emits global preferences and every resolved per-application profile.
pub fn serialize_preferences_with_profiles(
    preferences: Preferences,
    profiles: &[AppProfile],
) -> String {
    let mut output = format!(
        "[meta]\nformat-version = \"{}\"\n\n[input]\nkeymap-preset = \"{}\"\nprediction-enabled = \"{}\"\nsuggest-accept = \"{}\"\ndeveloper-mode = \"{}\"\n\n[width]\nalnum = \"{}\"\nnumber = \"{}\"\nsymbol = \"{}\"\npunctuation = \"{}\"\n",
        CONFIG_FORMAT_VERSION,
        preferences.keymap_preset.name(),
        bool_name(preferences.prediction_enabled),
        preferences.suggest_accept.name(),
        bool_name(preferences.developer_mode),
        width_name(preferences.normalizer.width.alnum),
        width_name(preferences.normalizer.width.number),
        width_name(preferences.normalizer.width.symbol),
        punctuation_name(preferences.normalizer.punctuation),
    );
    for profile in profiles {
        if !is_valid_profile_process_name(&profile.process_name) {
            continue;
        }
        output.push_str(&format!(
            "\n[profile.{}]\ndefault-mode = \"{}\"\nprediction-enabled = \"{}\"\nsuggest-accept = \"{}\"\nalnum = \"{}\"\nnumber = \"{}\"\nsymbol = \"{}\"\npunctuation = \"{}\"\n",
            profile.process_name,
            mode_name(profile.default_mode),
            bool_name(profile.prediction_enabled),
            profile.suggest_accept.name(),
            width_name(profile.normalizer.width.alnum),
            width_name(profile.normalizer.width.number),
            width_name(profile.normalizer.width.symbol),
            punctuation_name(profile.normalizer.punctuation),
        ));
    }
    output
}

pub fn is_valid_profile_process_name(process_name: &str) -> bool {
    !process_name.is_empty()
        && process_name.len() <= 128
        && process_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn parse_app_profiles(document: &Document, preferences: Preferences) -> Vec<AppProfile> {
    let mut profiles = default_app_profiles(preferences);
    for section in document.section_names() {
        let Some(process_name) = section.strip_prefix("profile.") else {
            continue;
        };
        if !is_valid_profile_process_name(process_name) {
            continue;
        }
        let existing = profiles
            .iter()
            .position(|profile| profile.matches(process_name));
        let mut profile = existing
            .and_then(|index| profiles.get(index).cloned())
            .unwrap_or_else(|| AppProfile::inherited(process_name, preferences));
        profile.process_name = process_name.to_owned();
        if let Some(mode) = text(document, section, "default-mode").and_then(parse_mode) {
            profile.default_mode = mode;
        }
        if let Some(enabled) = text(document, section, "prediction-enabled")
            .or_else(|| text(document, section, "suggest-enabled"))
            .and_then(parse_bool)
        {
            profile.prediction_enabled = enabled;
        }
        if let Some(accept) =
            text(document, section, "suggest-accept").and_then(SuggestAccept::from_name)
        {
            profile.suggest_accept = accept;
        }
        if let Some(value) = text(document, section, "alnum").and_then(parse_width) {
            profile.normalizer.width.alnum = value;
        }
        if let Some(value) = text(document, section, "number").and_then(parse_width) {
            profile.normalizer.width.number = value;
        }
        if let Some(value) = text(document, section, "symbol").and_then(parse_width) {
            profile.normalizer.width.symbol = value;
        }
        if let Some(value) = text(document, section, "punctuation").and_then(parse_punctuation) {
            profile.normalizer.punctuation = value;
        }
        if let Some(index) = existing {
            profiles[index] = profile;
        } else {
            profiles.push(profile);
        }
    }
    profiles
}

fn text<'a>(document: &'a Document, section: &str, key: &str) -> Option<&'a str> {
    document
        .section(section)?
        .iter()
        .find(|entry| entry.key == key)?
        .value
        .as_text()
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" | "on" | "yes" => Some(true),
        "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

const fn bool_name(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn parse_mode(value: &str) -> Option<Mode> {
    match value {
        "direct" => Some(Mode::Direct),
        "hiragana" => Some(Mode::Hiragana),
        "katakana" => Some(Mode::Katakana),
        "half-katakana" => Some(Mode::HalfKatakana),
        "full-alnum" => Some(Mode::FullAlnum),
        "half-alnum" => Some(Mode::HalfAlnum),
        _ => None,
    }
}

const fn mode_name(value: Mode) -> &'static str {
    match value {
        Mode::Direct => "direct",
        Mode::Hiragana => "hiragana",
        Mode::Katakana => "katakana",
        Mode::HalfKatakana => "half-katakana",
        Mode::FullAlnum => "full-alnum",
        Mode::HalfAlnum => "half-alnum",
    }
}

fn parse_width(value: &str) -> Option<Width> {
    match value {
        "half" => Some(Width::Half),
        "full" => Some(Width::Full),
        "follow-mode" => Some(Width::FollowMode),
        _ => None,
    }
}

const fn width_name(value: Width) -> &'static str {
    match value {
        Width::Half => "half",
        Width::Full => "full",
        Width::FollowMode => "follow-mode",
    }
}

fn parse_punctuation(value: &str) -> Option<PunctuationStyle> {
    match value {
        "kuten-touten" => Some(PunctuationStyle::KutenTouten),
        "comma-period" => Some(PunctuationStyle::CommaPeriod),
        "mixed" => Some(PunctuationStyle::Mixed),
        _ => None,
    }
}

const fn punctuation_name(value: PunctuationStyle) -> &'static str {
    match value {
        PunctuationStyle::KutenTouten => "kuten-touten",
        PunctuationStyle::CommaPeriod => "comma-period",
        PunctuationStyle::Mixed => "mixed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_format_roundtrips_every_setting() {
        let preferences = Preferences {
            keymap_preset: Preset::Atok,
            normalizer: Normalizer {
                width: WidthPolicy {
                    alnum: Width::Full,
                    number: Width::FollowMode,
                    symbol: Width::Half,
                },
                punctuation: PunctuationStyle::CommaPeriod,
            },
            prediction_enabled: false,
            suggest_accept: SuggestAccept::ShiftEnter,
            developer_mode: true,
        };
        let parsed = parse_preferences(&serialize_preferences(preferences)).expect("parse");
        assert_eq!(parsed.source_version, CONFIG_FORMAT_VERSION);
        assert_eq!(parsed.preferences, preferences);
        assert!(!parsed.needs_upgrade());
    }

    #[test]
    fn shipped_terminal_and_ide_profiles_start_off_and_protect_tab_completion() {
        let preferences = Preferences::default();
        let profiles = default_app_profiles(preferences);
        for process_name in ["WindowsTerminal.exe", "code.EXE", "DEVENV.exe"] {
            let resolved = resolve_context_preferences(preferences, &profiles, process_name);
            assert_eq!(resolved.default_mode, Mode::Direct);
            assert!(!resolved.prediction_enabled);
            assert_eq!(resolved.suggest_accept, SuggestAccept::Disabled);
        }
        let ordinary = resolve_context_preferences(preferences, &profiles, "notepad.exe");
        assert_eq!(ordinary.default_mode, Mode::Hiragana);
        assert!(ordinary.prediction_enabled);
        assert_eq!(ordinary.suggest_accept, SuggestAccept::Tab);
    }

    #[test]
    fn profile_sections_override_builtins_and_roundtrip() {
        let parsed = parse_preferences(
            r#"
[meta]
format-version = "3"
[input]
prediction-enabled = "true"
[profile.WindowsTerminal.exe]
default-mode = "direct"
prediction-enabled = "true"
suggest-accept = "shift-enter"
alnum = "full"
punctuation = "comma-period"
[profile.custom.exe]
default-mode = "katakana"
suggest-enabled = "false"
"#,
        )
        .expect("profiles");
        let terminal = resolve_context_preferences(
            parsed.preferences,
            &parsed.profiles,
            "windowsterminal.EXE",
        );
        assert_eq!(terminal.default_mode, Mode::Direct);
        assert!(terminal.prediction_enabled);
        assert_eq!(terminal.suggest_accept, SuggestAccept::ShiftEnter);
        assert_eq!(terminal.normalizer.width.alnum, Width::Full);
        assert_eq!(
            terminal.normalizer.punctuation,
            PunctuationStyle::CommaPeriod
        );
        let custom =
            resolve_context_preferences(parsed.preferences, &parsed.profiles, "CUSTOM.EXE");
        assert_eq!(custom.default_mode, Mode::Katakana);
        assert!(!custom.prediction_enabled);

        let serialized = serialize_preferences_with_profiles(parsed.preferences, &parsed.profiles);
        let reparsed = parse_preferences(&serialized).expect("roundtrip");
        assert_eq!(reparsed.profiles, parsed.profiles);
    }

    #[test]
    fn previous_format_upgrades_known_fields_without_data_loss() {
        let previous = r#"
[settings]
format-version = "1"
keymap = "atok"
prediction = "false"
future-setting = "preserved-by-defaulting"

[width]
alnum = "full"
number = "half"
symbol = "follow-mode"
punctuation = "mixed"
"#;
        let parsed = parse_preferences(previous).expect("previous format");
        assert!(parsed.needs_upgrade());
        assert_eq!(parsed.preferences.keymap_preset, Preset::Atok);
        assert!(!parsed.preferences.prediction_enabled);
        assert_eq!(parsed.preferences.normalizer.width.alnum, Width::Full);
        assert_eq!(
            parsed.preferences.normalizer.width.symbol,
            Width::FollowMode
        );
        assert_eq!(
            parsed.preferences.normalizer.punctuation,
            PunctuationStyle::Mixed
        );

        let upgraded =
            parse_preferences(&serialize_preferences(parsed.preferences)).expect("upgraded format");
        assert_eq!(upgraded.source_version, CONFIG_FORMAT_VERSION);
        assert_eq!(upgraded.preferences, parsed.preferences);
    }

    #[test]
    fn missing_unknown_and_malformed_known_values_default_independently() {
        let parsed = parse_preferences(
            r#"
[meta]
format-version = "99"
[input]
keymap-preset = "future-map"
prediction-enabled = "maybe"
future-field = "ignored"
[future-section]
anything = "ignored"
"#,
        )
        .expect("forward-compatible document");
        assert_eq!(parsed.source_version, 99);
        assert_eq!(parsed.preferences, Preferences::default());
        assert!(parsed.needs_upgrade());
    }
}
