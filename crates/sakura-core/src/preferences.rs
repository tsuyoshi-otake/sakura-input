//! Versioned user preferences built on Sakura's deliberately small TOML subset.
//!
//! Unknown and missing fields are ignored/defaulted so a newer settings tool
//! cannot brick an older engine. Parsing still rejects structurally malformed
//! TOML: silently guessing where a broken quote ended would be less safe than
//! retaining the last known-good configuration at the file-loading layer.

use crate::config::{self, Document, ParseError};
use crate::keymap::Preset;
use crate::width::{BracketStyle, Normalizer, PunctuationStyle, Width, WidthPolicy};
use sakura_proto::{AppearanceTheme, Mode, PadShortcut};

// The appearance section is optional, so adding its theme key remains
// compatible with v4 readers and does not require a format-version bump.
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

/// Selects how ordinary kana input is interpreted before conversion.
///
/// `Romaji` keeps the shipped romaji table path. `Kana` accepts the kana
/// character reported by the active Windows keyboard layout directly, which
/// is the same boundary used by the TSF key translator. The setting is
/// optional in the v4 document and therefore remains backwards compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMethod {
    #[default]
    Romaji,
    Kana,
}

impl InputMethod {
    pub const ALL: [Self; 2] = [Self::Romaji, Self::Kana];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Romaji => "romaji",
            Self::Kana => "kana",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "romaji" => Some(Self::Romaji),
            "kana" => Some(Self::Kana),
            _ => None,
        }
    }
}

/// Selects the segmentation contract used for ordinary conversion.
///
/// `MultiSegment` is the existing Viterbi/N-best path and may expose several
/// bunsetsu segments. `SingleSegment` asks the converter to build only paths
/// that cover the entire reading with one segment; it is not a presentation
/// toggle and therefore cannot silently discard trailing segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConversionMethod {
    #[default]
    MultiSegment,
    SingleSegment,
}

impl ConversionMethod {
    pub const ALL: [Self; 2] = [Self::MultiSegment, Self::SingleSegment];

    pub const fn name(self) -> &'static str {
        match self {
            Self::MultiSegment => "multi-segment",
            Self::SingleSegment => "single-segment",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "multi-segment" => Some(Self::MultiSegment),
            "single-segment" => Some(Self::SingleSegment),
            _ => None,
        }
    }
}

/// Selects the width emitted for an idle Space key in an ordinary IME mode.
/// This is separate from `WidthPolicy::symbol` so punctuation and other
/// symbols do not change when a user only changes spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpaceWidth {
    /// Full-width in Japanese/full-width modes, half-width in half-width mode.
    #[default]
    SameAsInput,
    Full,
    Half,
}

impl SpaceWidth {
    pub const ALL: [Self; 3] = [Self::SameAsInput, Self::Full, Self::Half];

    pub const fn name(self) -> &'static str {
        match self {
            Self::SameAsInput => "same-as-input",
            Self::Full => "full",
            Self::Half => "half",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "same-as-input" => Some(Self::SameAsInput),
            "full" => Some(Self::Full),
            "half" => Some(Self::Half),
            _ => None,
        }
    }

    pub const fn is_full(self, mode: Mode) -> bool {
        match self {
            Self::SameAsInput => !matches!(mode, Mode::HalfAlnum | Mode::Direct),
            Self::Full => true,
            Self::Half => false,
        }
    }
}

/// Selects how Shift+Space modifies the base space width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShiftSpaceBehavior {
    /// Emit the opposite of the configured ordinary Space width.
    #[default]
    Opposite,
    Full,
    Half,
}

impl ShiftSpaceBehavior {
    pub const ALL: [Self; 3] = [Self::Opposite, Self::Full, Self::Half];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Opposite => "opposite",
            Self::Full => "full",
            Self::Half => "half",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "opposite" => Some(Self::Opposite),
            "full" => Some(Self::Full),
            "half" => Some(Self::Half),
            _ => None,
        }
    }

    pub const fn is_full(self, base_is_full: bool) -> bool {
        match self {
            Self::Opposite => !base_is_full,
            Self::Full => true,
            Self::Half => false,
        }
    }
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

/// Controls which ordinary conversions may use the optional local neural
/// reranker. The reranker itself still fails closed when its isolated runtime
/// or artifact is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NeuralRerankerScope {
    /// Never invoke the optional reranker.
    Off,
    /// Preserve the established behavior for configurations that predate this
    /// setting: rerank only long normal conversions.
    #[default]
    LongTextOnly,
    /// Allow every classified normal conversion to be considered for reranking.
    AllNormalConversions,
}

impl NeuralRerankerScope {
    pub const ALL: [Self; 3] = [Self::Off, Self::LongTextOnly, Self::AllNormalConversions];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::LongTextOnly => "long-text-only",
            Self::AllNormalConversions => "all-normal-conversions",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "off" => Some(Self::Off),
            "long-text-only" => Some(Self::LongTextOnly),
            "all-normal-conversions" => Some(Self::AllNormalConversions),
            _ => None,
        }
    }
}

/// ATOK-style input assistance: typo repair at conversion time, English-to-
/// katakana spelling recovery, and contextual punctuation/long-vowel swaps.
///
/// Every flag defaults on so a missing `[input-support]` section matches the
/// ATOK-like factory defaults chosen for this feature. The master `enabled`
/// gate turns the whole sheet off without clearing the individual choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputSupport {
    pub enabled: bool,
    pub commit_based: bool,
    pub advanced: bool,
    pub vowel_count: bool,
    pub consonant_extra: bool,
    pub n_count: bool,
    pub dakuten_swap: bool,
    pub tsu_sokuon: bool,
    pub wa_wo: bool,
    pub small_u: bool,
    pub fuzzy_proper_nouns: bool,
    pub english_to_katakana: bool,
    pub period_after_digit: bool,
    pub comma_after_digit: bool,
    pub middle_dot_after_digit: bool,
    pub long_vowel_after_alnum: bool,
}

impl Default for InputSupport {
    fn default() -> Self {
        Self {
            enabled: true,
            commit_based: true,
            advanced: true,
            vowel_count: true,
            consonant_extra: true,
            n_count: true,
            dakuten_swap: true,
            tsu_sokuon: true,
            wa_wo: true,
            small_u: true,
            fuzzy_proper_nouns: true,
            english_to_katakana: true,
            period_after_digit: true,
            comma_after_digit: true,
            middle_dot_after_digit: true,
            long_vowel_after_alnum: true,
        }
    }
}

impl InputSupport {
    /// Effective gate used on every conversion and keystroke path.
    pub const fn is_active(self) -> bool {
        self.enabled
    }

    /// Shared SPELLING_CORRECTION admission used by conversion and prediction.
    ///
    /// Issue #63: active master + fuzzy proper nouns + not suppressed/sensitive.
    pub const fn allows_spelling_correction(self, skip_input_repair: bool) -> bool {
        self.is_active() && self.fuzzy_proper_nouns && !skip_input_repair
    }

    pub fn set_flag(&mut self, key: &str, value: bool) -> bool {
        match key {
            "enabled" => self.enabled = value,
            "commit-based" => self.commit_based = value,
            "advanced" => self.advanced = value,
            "vowel-count" => self.vowel_count = value,
            "consonant-extra" => self.consonant_extra = value,
            "n-count" => self.n_count = value,
            "dakuten-swap" => self.dakuten_swap = value,
            "tsu-sokuon" => self.tsu_sokuon = value,
            "wa-wo" => self.wa_wo = value,
            "small-u" => self.small_u = value,
            "fuzzy-proper-nouns" => self.fuzzy_proper_nouns = value,
            "english-to-katakana" => self.english_to_katakana = value,
            "period-after-digit" => self.period_after_digit = value,
            "comma-after-digit" => self.comma_after_digit = value,
            "middle-dot-after-digit" => self.middle_dot_after_digit = value,
            "long-vowel-after-alnum" => self.long_vowel_after_alnum = value,
            _ => return false,
        }
        true
    }

    pub fn flag(self, key: &str) -> Option<bool> {
        Some(match key {
            "enabled" => self.enabled,
            "commit-based" => self.commit_based,
            "advanced" => self.advanced,
            "vowel-count" => self.vowel_count,
            "consonant-extra" => self.consonant_extra,
            "n-count" => self.n_count,
            "dakuten-swap" => self.dakuten_swap,
            "tsu-sokuon" => self.tsu_sokuon,
            "wa-wo" => self.wa_wo,
            "small-u" => self.small_u,
            "fuzzy-proper-nouns" => self.fuzzy_proper_nouns,
            "english-to-katakana" => self.english_to_katakana,
            "period-after-digit" => self.period_after_digit,
            "comma-after-digit" => self.comma_after_digit,
            "middle-dot-after-digit" => self.middle_dot_after_digit,
            "long-vowel-after-alnum" => self.long_vowel_after_alnum,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    pub keymap_preset: Preset,
    /// Whether ordinary kana keys go through the romaji FSM or are accepted
    /// directly from the active keyboard layout.
    pub input_method: InputMethod,
    /// Segmentation contract used by new conversion contexts.
    pub conversion_method: ConversionMethod,
    /// Character type used when a new ordinary input context starts.
    ///
    /// Application profiles may override this value; this global setting is
    /// the fallback for every host without a matching profile.
    pub default_mode: Mode,
    pub normalizer: Normalizer,
    /// Width of an idle Space key in ordinary input modes.
    pub space_width: SpaceWidth,
    /// Width policy applied to Shift+Space in ordinary input modes.
    pub shift_space_behavior: ShiftSpaceBehavior,
    pub prediction_enabled: bool,
    pub suggest_accept: SuggestAccept,
    /// Enables the bounded grammar/context pass used by associative conversion.
    /// This is a preference rather than a new candidate source: the converter
    /// still owns candidate order and simply receives the previous segment's
    /// right-connection class when the option is enabled.
    pub association_enabled: bool,
    /// ATOK-style input assistance (typo repair, English spelling, punctuation).
    pub input_support: InputSupport,
    /// Scope for the optional, local neural conversion reranker.
    pub neural_reranker_scope: NeuralRerankerScope,
    /// Appearance selection shared by Sakura-owned settings and renderer UI.
    pub appearance_theme: AppearanceTheme,
    /// Keyboard shortcut used to show or focus Sakura Pad. This is a global
    /// renderer preference and is intentionally not part of an app profile.
    pub pad_shortcut: PadShortcut,
    /// Enables the explicitly opt-in developer interaction history. The
    /// engine keeps this separate from ordinary learning so a normal install
    /// never records raw key events.
    pub developer_mode: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            keymap_preset: Preset::MsIme,
            input_method: InputMethod::Romaji,
            conversion_method: ConversionMethod::MultiSegment,
            default_mode: Mode::Hiragana,
            normalizer: Normalizer::default(),
            space_width: SpaceWidth::SameAsInput,
            shift_space_behavior: ShiftSpaceBehavior::Opposite,
            prediction_enabled: true,
            suggest_accept: SuggestAccept::Tab,
            association_enabled: true,
            input_support: InputSupport::default(),
            neural_reranker_scope: NeuralRerankerScope::LongTextOnly,
            appearance_theme: AppearanceTheme::Auto,
            pad_shortcut: PadShortcut::Disabled,
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
            default_mode: preferences.default_mode,
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
    pub input_method: InputMethod,
    pub conversion_method: ConversionMethod,
    pub normalizer: Normalizer,
    pub space_width: SpaceWidth,
    pub shift_space_behavior: ShiftSpaceBehavior,
    pub prediction_enabled: bool,
    pub suggest_accept: SuggestAccept,
    pub association_enabled: bool,
    pub input_support: InputSupport,
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
            input_method: preferences.input_method,
            conversion_method: preferences.conversion_method,
            normalizer: profile.normalizer,
            space_width: preferences.space_width,
            shift_space_behavior: preferences.shift_space_behavior,
            prediction_enabled: profile.prediction_enabled,
            suggest_accept: profile.suggest_accept,
            association_enabled: preferences.association_enabled,
            input_support: preferences.input_support,
        };
    }
    ContextPreferences {
        default_mode: preferences.default_mode,
        input_method: preferences.input_method,
        conversion_method: preferences.conversion_method,
        normalizer: preferences.normalizer,
        space_width: preferences.space_width,
        shift_space_behavior: preferences.shift_space_behavior,
        prediction_enabled: preferences.prediction_enabled,
        suggest_accept: preferences.suggest_accept,
        association_enabled: preferences.association_enabled,
        input_support: preferences.input_support,
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
    let support = preferences.input_support;
    let mut output = format!(
        "[meta]\nformat-version = \"{}\"\n\n[input]\nkeymap-preset = \"{}\"\ninput-method = \"{}\"\nconversion-method = \"{}\"\ndefault-mode = \"{}\"\nprediction-enabled = \"{}\"\nsuggest-accept = \"{}\"\nassociation-enabled = \"{}\"\nneural-reranker-scope = \"{}\"\ndeveloper-mode = \"{}\"\nspace-width = \"{}\"\nshift-space = \"{}\"\n\n[input-support]\nenabled = \"{}\"\ncommit-based = \"{}\"\nadvanced = \"{}\"\nvowel-count = \"{}\"\nconsonant-extra = \"{}\"\nn-count = \"{}\"\ndakuten-swap = \"{}\"\ntsu-sokuon = \"{}\"\nwa-wo = \"{}\"\nsmall-u = \"{}\"\nfuzzy-proper-nouns = \"{}\"\nenglish-to-katakana = \"{}\"\nperiod-after-digit = \"{}\"\ncomma-after-digit = \"{}\"\nmiddle-dot-after-digit = \"{}\"\nlong-vowel-after-alnum = \"{}\"\n\n[appearance]\ntheme = \"{}\"\n\n[width]\nalnum = \"{}\"\nnumber = \"{}\"\nsymbol = \"{}\"\npunctuation = \"{}\"\nbrackets = \"{}\"\n",
        CONFIG_FORMAT_VERSION,
        preferences.keymap_preset.name(),
        preferences.input_method.name(),
        preferences.conversion_method.name(),
        mode_name(preferences.default_mode),
        bool_name(preferences.prediction_enabled),
        preferences.suggest_accept.name(),
        bool_name(preferences.association_enabled),
        preferences.neural_reranker_scope.name(),
        bool_name(preferences.developer_mode),
        preferences.space_width.name(),
        preferences.shift_space_behavior.name(),
        bool_name(support.enabled),
        bool_name(support.commit_based),
        bool_name(support.advanced),
        bool_name(support.vowel_count),
        bool_name(support.consonant_extra),
        bool_name(support.n_count),
        bool_name(support.dakuten_swap),
        bool_name(support.tsu_sokuon),
        bool_name(support.wa_wo),
        bool_name(support.small_u),
        bool_name(support.fuzzy_proper_nouns),
        bool_name(support.english_to_katakana),
        bool_name(support.period_after_digit),
        bool_name(support.comma_after_digit),
        bool_name(support.middle_dot_after_digit),
        bool_name(support.long_vowel_after_alnum),
        preferences.appearance_theme.name(),
        width_name(preferences.normalizer.width.alnum),
        width_name(preferences.normalizer.width.number),
        width_name(preferences.normalizer.width.symbol),
        punctuation_name(preferences.normalizer.punctuation),
        brackets_name(preferences.normalizer.brackets),
    );
    // Keep the optional Pad setting in the v4 input section without changing
    // the existing format version or making older readers depend on it.
    output = output.replacen(
        "\n\n[input-support]",
        &format!(
            "\npad-shortcut = \"{}\"\n\n[input-support]",
            preferences.pad_shortcut.name()
        ),
        1,
    );
    for profile in profiles {
        if !is_valid_profile_process_name(&profile.process_name) {
            continue;
        }
        output.push_str(&format!(
            "\n[profile.{}]\ndefault-mode = \"{}\"\nprediction-enabled = \"{}\"\nsuggest-accept = \"{}\"\nalnum = \"{}\"\nnumber = \"{}\"\nsymbol = \"{}\"\npunctuation = \"{}\"\nbrackets = \"{}\"\n",
            profile.process_name,
            mode_name(profile.default_mode),
            bool_name(profile.prediction_enabled),
            profile.suggest_accept.name(),
            width_name(profile.normalizer.width.alnum),
            width_name(profile.normalizer.width.number),
            width_name(profile.normalizer.width.symbol),
            punctuation_name(profile.normalizer.punctuation),
            brackets_name(profile.normalizer.brackets),
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
        "comma-kuten" => Some(PunctuationStyle::CommaKuten),
        _ => None,
    }
}

const fn punctuation_name(value: PunctuationStyle) -> &'static str {
    match value {
        PunctuationStyle::KutenTouten => "kuten-touten",
        PunctuationStyle::CommaPeriod => "comma-period",
        PunctuationStyle::Mixed => "mixed",
        PunctuationStyle::CommaKuten => "comma-kuten",
    }
}

const fn brackets_name(value: BracketStyle) -> &'static str {
    value.name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::EntryFlags;

    #[test]
    fn current_format_roundtrips_every_setting() {
        let preferences = Preferences {
            keymap_preset: Preset::Atok,
            input_method: InputMethod::Kana,
            default_mode: Mode::Katakana,
            conversion_method: ConversionMethod::SingleSegment,
            normalizer: Normalizer {
                width: WidthPolicy {
                    alnum: Width::Full,
                    number: Width::FollowMode,
                    symbol: Width::Half,
                },
                punctuation: PunctuationStyle::CommaPeriod,
                brackets: BracketStyle::Square,
            },
            space_width: SpaceWidth::Full,
            shift_space_behavior: ShiftSpaceBehavior::Half,
            prediction_enabled: false,
            suggest_accept: SuggestAccept::ShiftEnter,
            association_enabled: false,
            input_support: InputSupport {
                enabled: false,
                commit_based: false,
                advanced: true,
                vowel_count: false,
                consonant_extra: true,
                n_count: false,
                dakuten_swap: true,
                tsu_sokuon: false,
                wa_wo: true,
                small_u: false,
                fuzzy_proper_nouns: true,
                english_to_katakana: false,
                period_after_digit: true,
                comma_after_digit: false,
                middle_dot_after_digit: true,
                long_vowel_after_alnum: false,
            },
            neural_reranker_scope: NeuralRerankerScope::AllNormalConversions,
            appearance_theme: AppearanceTheme::Dark,
            pad_shortcut: PadShortcut::DoubleCtrl,
            developer_mode: true,
        };
        let parsed = parse_preferences(&serialize_preferences(preferences)).expect("parse");
        assert_eq!(parsed.source_version, CONFIG_FORMAT_VERSION);
        assert_eq!(parsed.preferences, preferences);
        assert!(!parsed.needs_upgrade());
    }

    #[test]
    fn pad_shortcut_is_optional_and_unknown_values_fail_closed() {
        let defaults = Preferences::default();
        assert_eq!(defaults.pad_shortcut, PadShortcut::Disabled);
        let missing = parse_preferences("[meta]\nformat-version = \"4\"\n")
            .expect("missing optional pad shortcut");
        assert_eq!(missing.preferences.pad_shortcut, PadShortcut::Disabled);

        let enabled = parse_preferences("[input]\npad-shortcut = \"double-ctrl\"\n")
            .expect("known pad shortcut");
        assert_eq!(enabled.preferences.pad_shortcut, PadShortcut::DoubleCtrl);

        let unknown = parse_preferences("[input]\npad-shortcut = \"future\"\n")
            .expect("unknown enum is still structurally valid");
        assert_eq!(unknown.preferences.pad_shortcut, PadShortcut::Disabled);
        let malformed_value = parse_preferences("[input]\npad-shortcut = [\"double-ctrl\"]\n")
            .expect("known key with wrong value shape");
        assert_eq!(
            malformed_value.preferences.pad_shortcut,
            PadShortcut::Disabled
        );

        let serialized = serialize_preferences(Preferences {
            pad_shortcut: PadShortcut::DoubleCtrl,
            ..defaults
        });
        assert!(serialized.contains("pad-shortcut = \"double-ctrl\""));
        assert_eq!(
            parse_preferences(&serialized)
                .expect("serialized pad shortcut")
                .preferences
                .pad_shortcut,
            PadShortcut::DoubleCtrl
        );
    }

    #[test]
    fn punctuation_config_accepts_the_independent_comma_kuten_variant() {
        let parsed = parse_preferences(
            r#"
[meta]
format-version = "4"
[width]
punctuation = "comma-kuten"
"#,
        )
        .expect("parse independent punctuation");
        assert_eq!(
            parsed.preferences.normalizer.punctuation,
            PunctuationStyle::CommaKuten
        );
        let serialized = serialize_preferences(parsed.preferences);
        assert!(serialized.contains("punctuation = \"comma-kuten\""));
    }

    #[test]
    fn space_preferences_roundtrip_and_resolve_without_touching_symbol_width() {
        let parsed = parse_preferences(
            r#"
[meta]
format-version = "4"
[input]
space-width = "half"
shift-space = "full"
[width]
symbol = "full"
"#,
        )
        .expect("space preferences");
        assert_eq!(parsed.preferences.space_width, SpaceWidth::Half);
        assert_eq!(
            parsed.preferences.shift_space_behavior,
            ShiftSpaceBehavior::Full
        );
        assert_eq!(parsed.preferences.normalizer.width.symbol, Width::Full);
        let serialized = serialize_preferences(parsed.preferences);
        assert!(serialized.contains("space-width = \"half\""));
        assert!(serialized.contains("shift-space = \"full\""));
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
neural-reranker-scope = "future-scope"
future-field = "ignored"
[appearance]
theme = "future-theme"
[future-section]
anything = "ignored"
"#,
        )
        .expect("forward-compatible document");
        assert_eq!(parsed.source_version, 99);
        assert_eq!(
            parsed.preferences,
            Preferences {
                neural_reranker_scope: NeuralRerankerScope::Off,
                ..Preferences::default()
            }
        );
        assert!(parsed.needs_upgrade());
    }

    #[test]
    fn appearance_theme_names_roundtrip_exhaustively() {
        for theme in AppearanceTheme::ALL {
            assert_eq!(AppearanceTheme::from_name(theme.name()), Some(theme));
        }
        assert_eq!(AppearanceTheme::from_name("system"), None);
    }

    #[test]
    fn appearance_theme_roundtrips_all_variants() {
        for appearance_theme in AppearanceTheme::ALL {
            let preferences = Preferences {
                appearance_theme,
                ..Preferences::default()
            };
            let serialized = serialize_preferences(preferences);
            assert!(serialized.contains(&format!(
                "[appearance]\ntheme = \"{}\"",
                appearance_theme.name()
            )));
            let parsed = parse_preferences(&serialized).expect("parse");
            assert_eq!(parsed.preferences.appearance_theme, appearance_theme);
        }
    }

    #[test]
    fn old_or_unknown_appearance_theme_fails_closed_to_auto() {
        let absent = parse_preferences("[settings]\nformat-version = \"1\"\n").expect("old");
        assert_eq!(absent.source_version, 1);
        assert_eq!(absent.preferences.appearance_theme, AppearanceTheme::Auto);

        let unknown = parse_preferences("[appearance]\ntheme = \"system\"\n").expect("unknown");
        assert_eq!(unknown.preferences.appearance_theme, AppearanceTheme::Auto);
    }

    #[test]
    fn input_method_roundtrips_and_missing_values_keep_romaji_default() {
        for input_method in InputMethod::ALL {
            assert_eq!(
                InputMethod::from_name(input_method.name()),
                Some(input_method)
            );
            let serialized = serialize_preferences(Preferences {
                input_method,
                ..Preferences::default()
            });
            assert!(serialized.contains(&format!("input-method = \"{}\"", input_method.name())));
            let parsed = parse_preferences(&serialized).expect("input method roundtrip");
            assert_eq!(parsed.preferences.input_method, input_method);
        }
        let missing = parse_preferences("[input]\nkeymap-preset = \"ms-ime\"\n")
            .expect("missing input method");
        assert_eq!(missing.preferences.input_method, InputMethod::Romaji);
        let unknown = parse_preferences("[input]\ninput-method = \"future\"\n")
            .expect("unknown input method");
        assert_eq!(unknown.preferences.input_method, InputMethod::Romaji);
    }

    #[test]
    fn default_mode_roundtrips_and_missing_or_unknown_values_keep_hiragana() {
        for mode in [
            Mode::Direct,
            Mode::Hiragana,
            Mode::Katakana,
            Mode::HalfKatakana,
            Mode::FullAlnum,
            Mode::HalfAlnum,
        ] {
            let serialized = serialize_preferences(Preferences {
                default_mode: mode,
                ..Preferences::default()
            });
            assert!(serialized.contains(&format!("default-mode = \"{}\"", mode_name(mode))));
            let parsed = parse_preferences(&serialized).expect("default mode roundtrip");
            assert_eq!(parsed.preferences.default_mode, mode);
        }
        let missing = parse_preferences("[input]\nkeymap-preset = \"ms-ime\"\n")
            .expect("missing default mode");
        assert_eq!(missing.preferences.default_mode, Mode::Hiragana);
        let unknown = parse_preferences("[input]\ndefault-mode = \"future\"\n")
            .expect("unknown default mode");
        assert_eq!(unknown.preferences.default_mode, Mode::Hiragana);
    }

    #[test]
    fn global_default_mode_is_used_for_hosts_without_a_profile() {
        let preferences = Preferences {
            default_mode: Mode::HalfKatakana,
            ..Preferences::default()
        };
        let resolved = resolve_context_preferences(preferences, &[], "notepad.exe");
        assert_eq!(resolved.default_mode, Mode::HalfKatakana);
        let profiles = default_app_profiles(preferences);
        assert_eq!(
            resolve_context_preferences(preferences, &profiles, "notepad.exe").default_mode,
            Mode::HalfKatakana
        );
        assert_eq!(
            resolve_context_preferences(preferences, &profiles, "Code.exe").default_mode,
            Mode::Direct
        );
    }

    #[test]
    fn neural_reranker_scope_names_roundtrip_exhaustively() {
        for scope in NeuralRerankerScope::ALL {
            assert_eq!(NeuralRerankerScope::from_name(scope.name()), Some(scope));
        }
        assert_eq!(NeuralRerankerScope::from_name("all"), None);
    }

    #[test]
    fn neural_reranker_scope_roundtrips_all_variants() {
        for neural_reranker_scope in NeuralRerankerScope::ALL {
            let preferences = Preferences {
                neural_reranker_scope,
                ..Preferences::default()
            };
            let serialized = serialize_preferences(preferences);
            assert!(serialized.contains(&format!(
                "neural-reranker-scope = \"{}\"",
                neural_reranker_scope.name()
            )));
            let parsed = parse_preferences(&serialized).expect("parse");
            assert_eq!(
                parsed.preferences.neural_reranker_scope,
                neural_reranker_scope
            );
        }
    }

    #[test]
    fn missing_scope_preserves_long_text_only_but_explicit_unknown_values_fail_closed() {
        let missing = parse_preferences(
            "[meta]\nformat-version = \"4\"\n\n[input]\nprediction-enabled = \"true\"\n",
        )
        .expect("existing configuration");
        assert_eq!(
            missing.preferences.neural_reranker_scope,
            NeuralRerankerScope::LongTextOnly
        );

        let unknown = parse_preferences("[input]\nneural-reranker-scope = \"future-scope\"\n")
            .expect("unknown scope");
        assert_eq!(
            unknown.preferences.neural_reranker_scope,
            NeuralRerankerScope::Off
        );

        let malformed =
            parse_preferences("[input]\nneural-reranker-scope = [\"long-text-only\"]\n")
                .expect("list value is structurally valid but not a scope");
        assert_eq!(
            malformed.preferences.neural_reranker_scope,
            NeuralRerankerScope::Off
        );
    }

    #[test]
    fn association_conversion_setting_roundtrips_and_defaults_on() {
        let enabled = Preferences::default();
        assert!(enabled.association_enabled);
        let disabled = Preferences {
            association_enabled: false,
            ..enabled
        };
        let serialized = serialize_preferences(disabled);
        assert!(serialized.contains("association-enabled = \"false\""));
        assert!(
            !parse_preferences(&serialized)
                .expect("parse")
                .preferences
                .association_enabled
        );
        let missing = parse_preferences("[meta]\nformat-version = \"4\"\n")
            .expect("missing optional setting");
        assert!(missing.preferences.association_enabled);
    }

    #[test]
    fn input_support_defaults_on_and_roundtrips_individual_flags() {
        let defaults = Preferences::default();
        assert!(defaults.input_support.enabled);
        assert!(defaults.input_support.vowel_count);
        assert!(defaults.input_support.english_to_katakana);
        let missing =
            parse_preferences("[meta]\nformat-version = \"4\"\n").expect("missing input-support");
        assert_eq!(missing.preferences.input_support, InputSupport::default());

        let support = InputSupport {
            enabled: false,
            n_count: false,
            period_after_digit: false,
            ..InputSupport::default()
        };
        let preferences = Preferences {
            input_support: support,
            ..Preferences::default()
        };
        let serialized = serialize_preferences(preferences);
        assert!(serialized.contains("[input-support]"));
        assert!(serialized.contains("enabled = \"false\""));
        assert!(serialized.contains("n-count = \"false\""));
        assert!(serialized.contains("period-after-digit = \"false\""));
        let parsed = parse_preferences(&serialized).expect("roundtrip");
        assert_eq!(parsed.preferences.input_support, support);
    }

    #[test]
    fn spelling_correction_admission_matches_issue_63_contract() {
        // Policy matrix from Issue #63.
        let cases = [
            (false, false, true, false),
            (true, true, true, false),
            (true, false, false, false),
            (true, false, true, true),
        ];
        for (active, skip, fuzzy, expect_spelling) in cases {
            let support = InputSupport {
                enabled: active,
                fuzzy_proper_nouns: fuzzy,
                ..InputSupport::default()
            };
            assert_eq!(
                support.allows_spelling_correction(skip),
                expect_spelling,
                "active={active} skip={skip} fuzzy={fuzzy}"
            );
            assert_eq!(
                crate::allows_system_entry(support, skip, EntryFlags::SPELLING_CORRECTION),
                expect_spelling
            );
            assert!(crate::allows_system_entry(support, skip, EntryFlags::IT));
        }
    }
}
