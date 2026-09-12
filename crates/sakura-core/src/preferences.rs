//! Versioned user preferences built on Sakura's deliberately small TOML subset.
//!
//! Unknown and missing fields are ignored/defaulted so a newer settings tool
//! cannot brick an older engine. Parsing still rejects structurally malformed
//! TOML: silently guessing where a broken quote ended would be less safe than
//! retaining the last known-good configuration at the file-loading layer.

use crate::config::{self, Document, ParseError};
use crate::keymap::Preset;
use crate::width::{
    BracketStyle, CommaMark, Normalizer, PeriodMark, PunctuationStyle, Width, WidthPolicy,
};
use sakura_values::{AppearanceTheme, Mode, PadShortcut};

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

/// A named preset over [`Preferences`]'s notation-related fields:
/// alphanumeric/number/symbol width, punctuation, brackets, and space width.
///
/// This is a pure derived/applied grouping, not a stored preference: it adds
/// no config-file key of its own and is never read or written by the parser
/// or serializer. The settings UI uses [`NotationStyle::apply_to`] to set
/// several scattered fields in one step, and [`NotationStyle::of`] to report
/// which preset (if any) the current combination still matches, so it can
/// fall back to showing "custom" once a user edits one of the underlying
/// fields directly.
///
/// Every style spells out all seven values it pins, even the three width
/// channels and the bracket style that happen to be identical across all
/// four styles today. That repetition is deliberate: a style is a complete,
/// self-contained declaration of what it requires, so a later change to
/// `WidthPolicy`'s or [`Preferences`]'s own defaults cannot silently change
/// what an existing style means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotationStyle {
    /// 標準（日本語） — ordinary Japanese prose: half-width alnum/number/
    /// symbol, the traditional `、。` punctuation, corner brackets, and a
    /// space that follows the current input width. Equal to
    /// [`Preferences::default`] on these seven fields.
    #[default]
    Standard,
    /// 日本語技術論文（半角句読点） — half-width `,.` punctuation for prose
    /// that will be typeset from plain text (Markdown, LaTeX, code
    /// comments), with half-width spaces to keep monospaced alignment
    /// predictable.
    TechnicalPaper,
    /// 学術（全角コンマ・ピリオド） — the JIS / 学術論文 convention of
    /// full-width `，．` punctuation.
    Academic,
    /// 公用文 — the 公用文 convention of full-width comma with a Japanese
    /// period, `，。`.
    Official,
}

impl NotationStyle {
    /// All styles, in declaration order.
    pub const ALL: [Self; 4] = [
        Self::Standard,
        Self::TechnicalPaper,
        Self::Academic,
        Self::Official,
    ];

    /// The seven leaf values this style pins, grouped the way [`Preferences`]
    /// itself groups them: the three [`WidthPolicy`] channels plus
    /// punctuation and brackets live inside [`Normalizer`], and
    /// [`SpaceWidth`] sits alongside it.
    ///
    /// Each arm below is written out in full rather than sharing one
    /// `WidthPolicy`/`BracketStyle` value across styles, for the same reason
    /// the type doc comment gives: nothing here should be able to change two
    /// styles at once by accident.
    const fn payload(self) -> (Normalizer, SpaceWidth) {
        match self {
            Self::Standard => (
                Normalizer {
                    width: WidthPolicy {
                        alnum: Width::Half,
                        number: Width::Half,
                        symbol: Width::Half,
                    },
                    punctuation: PunctuationStyle::KUTEN_TOUTEN,
                    brackets: BracketStyle::Corner,
                },
                SpaceWidth::SameAsInput,
            ),
            Self::TechnicalPaper => (
                Normalizer {
                    width: WidthPolicy {
                        alnum: Width::Half,
                        number: Width::Half,
                        symbol: Width::Half,
                    },
                    punctuation: PunctuationStyle::ASCII,
                    brackets: BracketStyle::Corner,
                },
                SpaceWidth::Half,
            ),
            Self::Academic => (
                Normalizer {
                    width: WidthPolicy {
                        alnum: Width::Half,
                        number: Width::Half,
                        symbol: Width::Half,
                    },
                    punctuation: PunctuationStyle::COMMA_PERIOD,
                    brackets: BracketStyle::Corner,
                },
                SpaceWidth::Half,
            ),
            Self::Official => (
                Normalizer {
                    width: WidthPolicy {
                        alnum: Width::Half,
                        number: Width::Half,
                        symbol: Width::Half,
                    },
                    punctuation: PunctuationStyle::COMMA_KUTEN,
                    brackets: BracketStyle::Corner,
                },
                SpaceWidth::SameAsInput,
            ),
        }
    }

    /// The Japanese label shown in the settings UI.
    ///
    /// Styles have no config-file token of their own — see the type doc
    /// comment — so unlike this file's other small enums there is no paired
    /// `name`/`from_name`: `label` is deliberately the only string accessor.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Standard => "標準（日本語）",
            Self::TechnicalPaper => "日本語技術論文（半角句読点）",
            Self::Academic => "学術（全角コンマ・ピリオド）",
            Self::Official => "公用文",
        }
    }

    /// Writes this style's seven pinned values into `preferences`. Every
    /// other field is left exactly as the caller passed it in.
    pub fn apply_to(self, preferences: &mut Preferences) {
        let (normalizer, space_width) = self.payload();
        preferences.normalizer = normalizer;
        preferences.space_width = space_width;
    }

    /// The style whose seven values all match `preferences`, or `None` if
    /// the current combination is a custom mix no shipped style produces.
    ///
    /// The four styles are constructed to be pairwise distinct on these
    /// seven fields (`notation_style_payloads_are_pairwise_distinct` below
    /// checks this), so at most one `ALL` entry can ever match.
    pub fn of(preferences: &Preferences) -> Option<Self> {
        let current = (preferences.normalizer, preferences.space_width);
        Self::ALL
            .into_iter()
            .find(|style| style.payload() == current)
    }

    /// Just the [`Normalizer`] half of the payload.
    ///
    /// An [`AppProfile`] carries a normalizer but no space width, so the
    /// per-application form of this setting can only pin five of the seven
    /// values. Splitting the accessor keeps that honest: the profile path
    /// cannot silently reach a field the profile does not store.
    pub const fn normalizer(self) -> Normalizer {
        self.payload().0
    }

    /// The style whose [`Normalizer`] matches, for the per-application form
    /// that has no space width to compare.
    ///
    /// Dropping space width from the comparison is only sound while the four
    /// normalizers stay pairwise distinct on their own —
    /// `notation_style_normalizers_are_pairwise_distinct` below checks that,
    /// because two styles differing *only* in space width would make this
    /// return an arbitrary one of them.
    pub fn of_normalizer(normalizer: &Normalizer) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|style| style.normalizer() == *normalizer)
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
fn parse_punctuation(value: &str) -> Option<PunctuationStyle> {
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

// Always writes one of the nine canonical names, never one of
// `parse_punctuation`'s regular-scheme aliases — in particular the four
// legacy combinations keep emitting their original irregular name so
// existing config files, and the round-trip tests below, see the exact same
// bytes come back out.
const fn punctuation_name(value: PunctuationStyle) -> &'static str {
    match (value.comma, value.period) {
        (CommaMark::Touten, PeriodMark::Kuten) => "kuten-touten",
        (CommaMark::FullWidth, PeriodMark::FullWidth) => "comma-period",
        (CommaMark::Touten, PeriodMark::FullWidth) => "mixed",
        (CommaMark::FullWidth, PeriodMark::Kuten) => "comma-kuten",
        (CommaMark::Touten, PeriodMark::HalfWidth) => "touten-half-period",
        (CommaMark::FullWidth, PeriodMark::HalfWidth) => "full-comma-half-period",
        (CommaMark::HalfWidth, PeriodMark::Kuten) => "half-comma-kuten",
        (CommaMark::HalfWidth, PeriodMark::FullWidth) => "half-comma-full-period",
        (CommaMark::HalfWidth, PeriodMark::HalfWidth) => "half-comma-half-period",
    }
}

const fn brackets_name(value: BracketStyle) -> &'static str {
    value.name()
}

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;
