//! Writing a preferences file in the current canonical format.

use crate::width::{BracketStyle, CommaMark, PeriodMark, PunctuationStyle, Width};
use sakura_values::Mode;

use super::{
    default_app_profiles, is_valid_profile_process_name, AppProfile, Preferences,
    CONFIG_FORMAT_VERSION,
};

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

const fn bool_name(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

pub(super) const fn mode_name(value: Mode) -> &'static str {
    match value {
        Mode::Direct => "direct",
        Mode::Hiragana => "hiragana",
        Mode::Katakana => "katakana",
        Mode::HalfKatakana => "half-katakana",
        Mode::FullAlnum => "full-alnum",
        Mode::HalfAlnum => "half-alnum",
    }
}

const fn width_name(value: Width) -> &'static str {
    match value {
        Width::Half => "half",
        Width::Full => "full",
        Width::FollowMode => "follow-mode",
    }
}

// Always writes one of the nine canonical names, never one of
// `parse_punctuation`'s regular-scheme aliases — in particular the four
// legacy combinations keep emitting their original irregular name so
// existing config files, and the round-trip tests below, see the exact same
// bytes come back out.
pub(super) const fn punctuation_name(value: PunctuationStyle) -> &'static str {
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
