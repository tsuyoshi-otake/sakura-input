//! The preference model: every setting's type, its closed set of values,
//! and the defaults a fresh install starts from. Reading and writing the
//! config file live in `parse` and `serialize`.

use crate::keymap::Preset;
use crate::width::{BracketStyle, Normalizer, PunctuationStyle, Width, WidthPolicy};
use sakura_values::{AppearanceTheme, Mode, PadShortcut};

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
    pub(super) const fn payload(self) -> (Normalizer, SpaceWidth) {
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
