//! Reading a preferences file. Unknown and malformed values fall back to the
//! default for that field; only structurally broken TOML is an error.

use crate::config::{self, Document, ParseError};
use crate::keymap::Preset;
use crate::width::{
    BracketStyle, CommaMark, Normalizer, PeriodMark, PunctuationStyle, Width, WidthPolicy,
};
use sakura_values::{AppearanceTheme, Mode, PadShortcut};

use super::{default_app_profiles, is_valid_profile_process_name, AppProfile};
use super::{
    ConversionMethod, InputMethod, InputSupport, NeuralRerankerScope, Preferences,
    ShiftSpaceBehavior, SpaceWidth, SuggestAccept,
};
use super::{CONFIG_FORMAT_VERSION, LEGACY_CONFIG_FORMAT_VERSION, PREVIOUS_CONFIG_FORMAT_VERSION};

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
    if let Some(input_method) =
        text(&document, input_section, "input-method").and_then(InputMethod::from_name)
    {
        preferences.input_method = input_method;
    }
    if let Some(method) =
        text(&document, input_section, "conversion-method").and_then(ConversionMethod::from_name)
    {
        preferences.conversion_method = method;
    }
    if let Some(mode) = text(&document, input_section, "default-mode").and_then(parse_mode) {
        preferences.default_mode = mode;
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
    if let Some(enabled) =
        text(&document, input_section, "association-enabled").and_then(parse_bool)
    {
        preferences.association_enabled = enabled;
    }
    parse_input_support(&document, &mut preferences.input_support);
    if let Some(value) = document.section(input_section).and_then(|entries| {
        entries
            .iter()
            .find(|entry| entry.key == "neural-reranker-scope")
    }) {
        // Omission retains the pre-setting long-text behavior. In contrast, a
        // value the current build cannot validate is an explicit request it
        // must not guess at, so disable reranking rather than broadening it.
        preferences.neural_reranker_scope = value
            .value
            .as_text()
            .and_then(NeuralRerankerScope::from_name)
            .unwrap_or(NeuralRerankerScope::Off);
    }
    if let Some(enabled) = text(&document, input_section, "developer-mode").and_then(parse_bool) {
        preferences.developer_mode = enabled;
    }
    if let Some(space_width) =
        text(&document, input_section, "space-width").and_then(SpaceWidth::from_name)
    {
        preferences.space_width = space_width;
    }
    if let Some(behavior) =
        text(&document, input_section, "shift-space").and_then(ShiftSpaceBehavior::from_name)
    {
        preferences.shift_space_behavior = behavior;
    }
    if let Some(theme) = text(&document, "appearance", "theme").and_then(AppearanceTheme::from_name)
    {
        preferences.appearance_theme = theme;
    }
    if let Some(value) = document
        .section(input_section)
        .and_then(|entries| entries.iter().find(|entry| entry.key == "pad-shortcut"))
    {
        // Missing and unknown shortcut values are deliberately bounded to
        // Disabled. A structurally malformed document still returns the
        // parser error above, allowing the watcher to retain its last-good
        // complete configuration snapshot.
        preferences.pad_shortcut = value
            .value
            .as_text()
            .and_then(PadShortcut::from_name)
            .unwrap_or(PadShortcut::Disabled);
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
    let brackets = text(&document, "width", "brackets")
        .or_else(|| text(&document, "punctuation", "brackets"))
        .and_then(BracketStyle::from_name)
        .unwrap_or_default();
    preferences.normalizer = Normalizer {
        width,
        punctuation,
        brackets,
    };

    let profiles = parse_app_profiles(&document, preferences);

    Ok(ParsedPreferences {
        source_version,
        preferences,
        profiles,
    })
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
        if let Some(value) = text(document, section, "brackets").and_then(BracketStyle::from_name) {
            profile.normalizer.brackets = value;
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

fn parse_input_support(document: &Document, support: &mut InputSupport) {
    const KEYS: [&str; 16] = [
        "enabled",
        "commit-based",
        "advanced",
        "vowel-count",
        "consonant-extra",
        "n-count",
        "dakuten-swap",
        "tsu-sokuon",
        "wa-wo",
        "small-u",
        "fuzzy-proper-nouns",
        "english-to-katakana",
        "period-after-digit",
        "comma-after-digit",
        "middle-dot-after-digit",
        "long-vowel-after-alnum",
    ];
    for key in KEYS {
        if let Some(value) = text(document, "input-support", key).and_then(parse_bool) {
            let _ = support.set_flag(key, value);
        }
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" | "on" | "yes" => Some(true),
        "false" | "off" | "no" => Some(false),
        _ => None,
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

fn parse_width(value: &str) -> Option<Width> {
    match value {
        "half" => Some(Width::Half),
        "full" => Some(Width::Full),
        "follow-mode" => Some(Width::FollowMode),
        _ => None,
    }
}

// The comma and period roles are independent settings (`width::CommaMark`,
// `width::PeriodMark`), so the nine names below are the full cross product,
// not four hand-picked combinations. Four of the nine predate that split and
// keep their original irregular names — "kuten-touten", "comma-period",
// "mixed", "comma-kuten" — so config files written before the split still
// parse, and `punctuation_name` still emits exactly what it always did. The
// other five are new and get the regular "<comma>-<period>" scheme. The four
// legacy combinations also accept that regular form as an alias on read
// ("touten-kuten", "full-comma-full-period", "touten-full-period",
// "full-comma-kuten"), so a reader learning the vocabulary only has to learn
// one scheme, even though the writer still prefers the old spelling for
// those four. Unknown values return `None`: the caller falls back to the
// default rather than guessing at a convention this build does not know.
pub(super) fn parse_punctuation(value: &str) -> Option<PunctuationStyle> {
    match value {
        "kuten-touten" | "touten-kuten" => {
            Some(PunctuationStyle::new(CommaMark::Touten, PeriodMark::Kuten))
        }
        "comma-period" | "full-comma-full-period" => Some(PunctuationStyle::new(
            CommaMark::FullWidth,
            PeriodMark::FullWidth,
        )),
        "mixed" | "touten-full-period" => Some(PunctuationStyle::new(
            CommaMark::Touten,
            PeriodMark::FullWidth,
        )),
        "comma-kuten" | "full-comma-kuten" => Some(PunctuationStyle::new(
            CommaMark::FullWidth,
            PeriodMark::Kuten,
        )),
        "touten-half-period" => Some(PunctuationStyle::new(
            CommaMark::Touten,
            PeriodMark::HalfWidth,
        )),
        "full-comma-half-period" => Some(PunctuationStyle::new(
            CommaMark::FullWidth,
            PeriodMark::HalfWidth,
        )),
        "half-comma-kuten" => Some(PunctuationStyle::new(
            CommaMark::HalfWidth,
            PeriodMark::Kuten,
        )),
        "half-comma-full-period" => Some(PunctuationStyle::new(
            CommaMark::HalfWidth,
            PeriodMark::FullWidth,
        )),
        "half-comma-half-period" => Some(PunctuationStyle::new(
            CommaMark::HalfWidth,
            PeriodMark::HalfWidth,
        )),
        _ => None,
    }
}
