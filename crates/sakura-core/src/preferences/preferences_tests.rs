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
            punctuation: PunctuationStyle::COMMA_PERIOD,
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

    let enabled =
        parse_preferences("[input]\npad-shortcut = \"double-ctrl\"\n").expect("known pad shortcut");
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
        PunctuationStyle::COMMA_KUTEN
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
    let terminal =
        resolve_context_preferences(parsed.preferences, &parsed.profiles, "windowsterminal.EXE");
    assert_eq!(terminal.default_mode, Mode::Direct);
    assert!(terminal.prediction_enabled);
    assert_eq!(terminal.suggest_accept, SuggestAccept::ShiftEnter);
    assert_eq!(terminal.normalizer.width.alnum, Width::Full);
    assert_eq!(
        terminal.normalizer.punctuation,
        PunctuationStyle::COMMA_PERIOD
    );
    let custom = resolve_context_preferences(parsed.preferences, &parsed.profiles, "CUSTOM.EXE");
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
        PunctuationStyle::MIXED
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
    let missing =
        parse_preferences("[input]\nkeymap-preset = \"ms-ime\"\n").expect("missing input method");
    assert_eq!(missing.preferences.input_method, InputMethod::Romaji);
    let unknown =
        parse_preferences("[input]\ninput-method = \"future\"\n").expect("unknown input method");
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
    let missing =
        parse_preferences("[input]\nkeymap-preset = \"ms-ime\"\n").expect("missing default mode");
    assert_eq!(missing.preferences.default_mode, Mode::Hiragana);
    let unknown =
        parse_preferences("[input]\ndefault-mode = \"future\"\n").expect("unknown default mode");
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

    let malformed = parse_preferences("[input]\nneural-reranker-scope = [\"long-text-only\"]\n")
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
    let missing =
        parse_preferences("[meta]\nformat-version = \"4\"\n").expect("missing optional setting");
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

#[test]
fn punctuation_style_names_round_trip_all_nine_combinations() {
    // Every point in the comma x period cross product must survive a
    // name -> style -> name trip, independent of which nine strings the
    // config format happens to spell each combination with.
    for style in PunctuationStyle::ALL {
        let name = punctuation_name(style);
        assert_eq!(
            parse_punctuation(name),
            Some(style),
            "name {name:?} did not parse back to {style:?}"
        );
    }
    // The four combinations that predate the comma/period split must
    // keep serializing to exactly their original names: that is what
    // lets a config file written years ago still round-trip today.
    assert_eq!(
        punctuation_name(PunctuationStyle::KUTEN_TOUTEN),
        "kuten-touten"
    );
    assert_eq!(
        punctuation_name(PunctuationStyle::COMMA_PERIOD),
        "comma-period"
    );
    assert_eq!(punctuation_name(PunctuationStyle::MIXED), "mixed");
    assert_eq!(
        punctuation_name(PunctuationStyle::COMMA_KUTEN),
        "comma-kuten"
    );
}

#[test]
fn punctuation_regular_scheme_aliases_match_their_legacy_names() {
    // The four legacy combinations also accept the same regular
    // "<comma>-<period>" scheme the five newer combinations use, so a
    // reader only has to learn one naming rule even though the writer
    // still prefers the old spelling for these four.
    let aliases = [
        ("touten-kuten", "kuten-touten"),
        ("full-comma-full-period", "comma-period"),
        ("touten-full-period", "mixed"),
        ("full-comma-kuten", "comma-kuten"),
    ];
    for (alias, legacy) in aliases {
        let parsed_alias = parse_punctuation(alias);
        assert_eq!(
            parsed_alias,
            parse_punctuation(legacy),
            "alias {alias:?} should parse the same as {legacy:?}"
        );
        // But the alias is never what gets written back out: the writer
        // always prefers the legacy irregular name for these four.
        let style = parsed_alias.expect("alias is one of the nine canonical styles");
        assert_eq!(punctuation_name(style), legacy);
    }
}

#[test]
fn punctuation_config_accepts_the_all_ascii_variant() {
    let parsed = parse_preferences(
        r#"
[meta]
format-version = "4"
[width]
punctuation = "half-comma-half-period"
"#,
    )
    .expect("parse all-ASCII punctuation");
    assert_eq!(
        parsed.preferences.normalizer.punctuation,
        PunctuationStyle::ASCII
    );
    let serialized = serialize_preferences(parsed.preferences);
    assert!(serialized.contains("punctuation = \"half-comma-half-period\""));
}

#[test]
fn notation_style_standard_matches_preferences_default() {
    // `Preferences` derives `PartialEq`/`Eq` (see its struct definition
    // above), so every field can be checked in one assertion instead of
    // listing the seven `Standard` touches by hand.
    let mut preferences = Preferences::default();
    NotationStyle::Standard.apply_to(&mut preferences);
    assert_eq!(preferences, Preferences::default());
}

#[test]
fn notation_style_round_trips_through_apply_and_of() {
    for style in NotationStyle::ALL {
        let mut preferences = Preferences::default();
        style.apply_to(&mut preferences);
        assert_eq!(NotationStyle::of(&preferences), Some(style), "{style:?}");
    }
}

#[test]
fn notation_style_of_returns_none_when_any_field_is_perturbed() {
    for style in NotationStyle::ALL {
        let mut baseline = Preferences::default();
        style.apply_to(&mut baseline);
        assert_eq!(NotationStyle::of(&baseline), Some(style));

        // Every style pins these four channels to the same value (`Half`
        // width, `Corner` brackets), so nudging any one of them away
        // from that value cannot land on a different style either: the
        // replacement is safe regardless of which style is under test.
        let mut alnum = baseline;
        alnum.normalizer.width.alnum = Width::Full;
        assert_eq!(NotationStyle::of(&alnum), None, "{style:?} alnum");

        let mut number = baseline;
        number.normalizer.width.number = Width::Full;
        assert_eq!(NotationStyle::of(&number), None, "{style:?} number");

        let mut symbol = baseline;
        symbol.normalizer.width.symbol = Width::Full;
        assert_eq!(NotationStyle::of(&symbol), None, "{style:?} symbol");

        let mut brackets = baseline;
        brackets.normalizer.brackets = BracketStyle::Square;
        assert_eq!(NotationStyle::of(&brackets), None, "{style:?} brackets");

        // `Full` space width is likewise never used by any style (only
        // `SameAsInput` and `Half` are), so it too is a safe replacement
        // no matter which style's baseline this perturbs.
        let mut space_width = baseline;
        space_width.space_width = SpaceWidth::Full;
        assert_eq!(
            NotationStyle::of(&space_width),
            None,
            "{style:?} space_width"
        );

        // Comma and period are the two leaves that actually vary between
        // styles, so a careless replacement could reconstruct a
        // different style's exact combination by accident. Each pair
        // below is chosen so the resulting (comma, period) combination
        // is not the punctuation any style requires, independent of what
        // the other six fields happen to be.
        let (comma_replacement, period_replacement) = match style {
            NotationStyle::Standard => (CommaMark::HalfWidth, PeriodMark::FullWidth),
            NotationStyle::TechnicalPaper => (CommaMark::Touten, PeriodMark::Kuten),
            NotationStyle::Academic => (CommaMark::Touten, PeriodMark::HalfWidth),
            NotationStyle::Official => (CommaMark::HalfWidth, PeriodMark::HalfWidth),
        };
        let baseline_punctuation = baseline.normalizer.punctuation;

        let mut comma = baseline;
        comma.normalizer.punctuation =
            PunctuationStyle::new(comma_replacement, baseline_punctuation.period);
        assert_eq!(NotationStyle::of(&comma), None, "{style:?} comma");

        let mut period = baseline;
        period.normalizer.punctuation =
            PunctuationStyle::new(baseline_punctuation.comma, period_replacement);
        assert_eq!(NotationStyle::of(&period), None, "{style:?} period");
    }
}

#[test]
fn notation_style_apply_to_only_touches_its_seven_fields() {
    // Every field below is deliberately non-default, including
    // `normalizer`/`space_width` (which `apply_to` is expected to
    // overwrite): the point is to prove the other thirteen survive.
    let custom = Preferences {
        keymap_preset: Preset::Atok,
        input_method: InputMethod::Kana,
        default_mode: Mode::Katakana,
        conversion_method: ConversionMethod::SingleSegment,
        normalizer: Normalizer::default(),
        space_width: SpaceWidth::Full,
        shift_space_behavior: ShiftSpaceBehavior::Half,
        prediction_enabled: false,
        suggest_accept: SuggestAccept::ShiftEnter,
        association_enabled: false,
        input_support: InputSupport {
            enabled: false,
            ..InputSupport::default()
        },
        neural_reranker_scope: NeuralRerankerScope::AllNormalConversions,
        appearance_theme: AppearanceTheme::Dark,
        pad_shortcut: PadShortcut::DoubleCtrl,
        developer_mode: true,
    };

    for style in NotationStyle::ALL {
        let mut preferences = custom;
        style.apply_to(&mut preferences);

        assert_eq!(preferences.keymap_preset, custom.keymap_preset, "{style:?}");
        assert_eq!(preferences.input_method, custom.input_method, "{style:?}");
        assert_eq!(
            preferences.conversion_method, custom.conversion_method,
            "{style:?}"
        );
        assert_eq!(preferences.default_mode, custom.default_mode, "{style:?}");
        assert_eq!(
            preferences.shift_space_behavior, custom.shift_space_behavior,
            "{style:?}"
        );
        assert_eq!(
            preferences.prediction_enabled, custom.prediction_enabled,
            "{style:?}"
        );
        assert_eq!(
            preferences.suggest_accept, custom.suggest_accept,
            "{style:?}"
        );
        assert_eq!(
            preferences.association_enabled, custom.association_enabled,
            "{style:?}"
        );
        assert_eq!(preferences.input_support, custom.input_support, "{style:?}");
        assert_eq!(
            preferences.neural_reranker_scope, custom.neural_reranker_scope,
            "{style:?}"
        );
        assert_eq!(
            preferences.appearance_theme, custom.appearance_theme,
            "{style:?}"
        );
        assert_eq!(preferences.pad_shortcut, custom.pad_shortcut, "{style:?}");
        assert_eq!(
            preferences.developer_mode, custom.developer_mode,
            "{style:?}"
        );
    }
}

#[test]
fn notation_style_technical_paper_pins_ascii_punctuation_and_half_space() {
    let mut preferences = Preferences::default();
    NotationStyle::TechnicalPaper.apply_to(&mut preferences);
    assert_eq!(preferences.normalizer.punctuation, PunctuationStyle::ASCII);
    assert_eq!(preferences.space_width, SpaceWidth::Half);
}

#[test]
fn notation_style_payloads_are_pairwise_distinct() {
    for (index, style) in NotationStyle::ALL.into_iter().enumerate() {
        for other in NotationStyle::ALL.into_iter().skip(index + 1) {
            assert_ne!(style.payload(), other.payload(), "duplicate payload");
        }
    }
}

#[test]
fn notation_style_normalizers_are_pairwise_distinct() {
    // `of_normalizer` compares five of the seven values because an
    // `AppProfile` stores no space width. That is only unambiguous while
    // no two styles are separated by space width alone; if a fifth style
    // ever were, this fails here rather than silently resolving to
    // whichever one `ALL` happens to list first.
    for (index, style) in NotationStyle::ALL.into_iter().enumerate() {
        assert_eq!(
            NotationStyle::of_normalizer(&style.normalizer()),
            Some(style),
            "{style:?}"
        );
        for other in NotationStyle::ALL.into_iter().skip(index + 1) {
            assert_ne!(
                style.normalizer(),
                other.normalizer(),
                "{style:?} and {other:?} share a normalizer"
            );
        }
    }

    // A mix no style produces stays a custom mix.
    let mut custom = NotationStyle::TechnicalPaper.normalizer();
    custom.brackets = BracketStyle::Square;
    assert_eq!(NotationStyle::of_normalizer(&custom), None);
}
