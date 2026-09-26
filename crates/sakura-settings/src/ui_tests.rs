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
fn ai_style_labels_follow_the_persisted_combo_order() {
    let labels: Vec<_> = AiStyle::ALL.into_iter().map(ai_style_label).collect();
    assert_eq!(
        labels,
        [
            "話し言葉",
            "丁寧語",
            "ビジネス",
            "公文書",
            "技術文書",
            "論文",
            "契約",
            "小説",
            "SNS",
            "英語",
        ]
    );
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
    assert_eq!(LIGHT_INPUT_SURFACE, rgb(0xFC, 0xFB, 0xFA));
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
    let _desktop = native_test_guard();
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
    let _desktop = native_test_guard();
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
