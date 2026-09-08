//! Native forms grouped by settings task. Values and commands belong to App.
use super::presentation::{BoxRect as R, Presentation, TextRole};
use super::*;

pub(super) fn create_general_controls(
    parent: HWND,
    layout: &mut Presentation,
) -> WindowsResult<GeneralControls> {
    let mut p = layout.page(
        parent,
        "基本設定",
        "いつもの入力方法と、使い慣れたキー操作を選びます。",
        368,
    )?;
    let basic_panel = p.window();
    p.section("既定の入力", 100)?;
    let keymap = p.row_combo("キー設定", 140)?;
    add_combo(keymap, "Microsoft IME 互換");
    add_combo(keymap, "ATOK 互換");
    p.row_label("入力方法", 192)?;
    let input_method_romaji = p.radio(
        input_method_label(InputMethod::Romaji),
        R::new(248, 192, 156, 34),
        true,
    )?;
    let input_method_kana = p.radio(
        input_method_label(InputMethod::Kana),
        R::new(412, 192, 140, 34),
        false,
    )?;
    let default_mode = p.row_combo("文字種", 244)?;
    for mode in Mode::ALL {
        add_combo(default_mode, mode_label(mode));
    }
    let pad_shortcut = p.row_combo("Sakura Pad", 296)?;
    for value in PadShortcut::ALL {
        add_combo(pad_shortcut, pad_shortcut_label(value));
    }

    let mut p = layout.page(
        parent,
        "入力補助",
        "空白文字の入力と、AI文章変換を呼び出すキーを設定します。",
        324,
    )?;
    let input_assist_panel = p.window();
    let input_assist_space_width = p.row_combo("スペースキー", 108)?;
    for value in SpaceWidth::ALL {
        add_combo(input_assist_space_width, space_width_label(value));
    }
    let input_assist_shift_space = p.row_combo("Shift+スペース", 160)?;
    for value in ["スペースの逆", "常に全角", "常に半角"] {
        add_combo(input_assist_shift_space, value);
    }
    let ai_text_key = p.row_combo("文章変換キー", 240)?;
    for value in ["変換（Spaceの右・既定）", "Caps Lock", "使わない"] {
        add_combo(ai_text_key, value);
    }

    let mut p = layout.page(
        parent,
        "AI文章変換",
        "明示操作した文章だけをGPT-5.6 Lunaへ送信します。",
        720,
    )?;
    let ai_text_panel = p.window();
    p.section("接続", 100)?;
    let ai_provider = p.row_combo("プロバイダー", 140)?;
    let ai_providers = available_ai_providers();
    for value in &ai_providers {
        add_combo(ai_provider, ai_provider_label(*value));
    }
    p.row_label("モデル", 192)?;
    p.edit(sakura_ai_proto::MODEL, R::new(248, 192, 304, 34), true)?;
    p.text("Endpoint", R::new(0, 244, 552, 24), TextRole::Body)?;
    let ai_endpoint = p.edit("", R::new(0, 276, 552, 34), false)?;
    let ai_auth = p.row_combo("認証", 332)?;
    for value in AiAuth::ALL {
        add_combo(ai_auth, ai_auth_label(value));
    }
    p.row_label("APIキー", 384)?;
    let ai_api_key = p.password(R::new(248, 384, 216, 34))?;
    let ai_api_key_clear = p.button("削除", R::new(476, 384, 76, 34))?;
    let ai_api_key_status = p.helper("", 432)?;
    p.section("文章の仕上がり", 500)?;
    let ai_style = p.row_combo("変換スタイル", 540)?;
    for value in AiStyle::ALL {
        add_combo(ai_style, ai_style_label(value));
    }
    let ai_effort = p.row_combo("Effort", 592)?;
    for value in AiEffort::ALL {
        add_combo(ai_effort, ai_effort_label(value));
    }
    let ai_service_tier = p.row_combo("Tier", 644)?;
    for value in AiServiceTier::ALL {
        add_combo(ai_service_tier, ai_service_tier_label(value));
    }

    let mut p = layout.page(
        parent,
        "推測変換",
        "入力中に候補を自動表示し、確定方法を選べます。",
        260,
    )?;
    let prediction_panel = p.window();
    let prediction = p.checkbox("予測入力を使う", R::new(0, 108, 552, 34))?;
    let suggest = p.row_combo("候補の確定", 176)?;
    for value in ["Tab", "Shift+Enter", "使わない"] {
        add_combo(suggest, value);
    }

    let mut p = layout.page(
        parent,
        "文節変換",
        "変換単位と、sakura-rerankによる候補の並べ替えを設定します。",
        384,
    )?;
    let segment_panel = p.window();
    let conversion_assist_method = p.row_combo("変換単位", 108)?;
    for value in ConversionMethod::ALL {
        add_combo(conversion_assist_method, conversion_method_label(value));
    }
    p.section("AI候補の並べ替え", 200)?;
    let neural_reranker_scope = p.row_combo("sakura-rerankの適用範囲", 240)?;
    for value in NeuralRerankerScope::ALL {
        add_combo(neural_reranker_scope, neural_reranker_scope_label(value));
    }
    p.helper(
        "文節変換を基本とし、sakura-rerankは候補の並べ替えだけに使用します。",
        300,
    )?;

    let mut p = layout.page(
        parent,
        "文字幅・句読点",
        "表記スタイルを選んだ後、各項目を個別に調整できます。",
        584,
    )?;
    let normalizer_panel = p.window();
    let notation_style = p.row_combo("表記スタイル", 108)?;
    for value in NotationStyle::ALL {
        add_combo(notation_style, value.label());
    }
    add_combo(notation_style, NOTATION_STYLE_CUSTOM_LABEL);
    let normalizer_alnum = p.row_combo("英字", 180)?;
    let normalizer_number = p.row_combo("数字", 232)?;
    let punctuation_period = p.row_combo("句点", 284)?;
    let punctuation_comma = p.row_combo("読点", 336)?;
    let normalizer_symbol = p.row_combo("記号", 388)?;
    let punctuation_brackets = p.row_combo("括弧", 440)?;
    for control in [normalizer_alnum, normalizer_number, normalizer_symbol] {
        for value in [Width::Half, Width::Full, Width::FollowMode] {
            add_combo(control, width_label(value));
        }
    }
    for value in PeriodMark::ALL {
        add_combo(punctuation_period, period_mark_label(value));
    }
    for value in CommaMark::ALL {
        add_combo(punctuation_comma, comma_mark_label(value));
    }
    for value in BracketStyle::ALL {
        add_combo(punctuation_brackets, bracket_style_label(value));
    }
    let normalizer_reset = p.button("初期値に戻す", R::new(408, 516, 144, 34))?;

    let mut p = layout.page(
        parent,
        "連想変換",
        "文節のつながりを使った候補を表示します。",
        260,
    )?;
    let association_panel = p.window();
    let association = p.checkbox("連想変換を使う", R::new(0, 108, 552, 34))?;
    p.helper("連想候補は文節変換とは別に表示されます。", 164)?;

    let mut p = layout.page(
        parent,
        "入力誤りの自動修復",
        "ローマ字／カナ入力のミスを変換時に修正します。",
        504,
    )?;
    let input_repair_panel = p.window();
    let input_support_enabled = p.checkbox("入力支援を有効にする", R::new(0, 108, 552, 34))?;
    let input_support_commit_based =
        p.checkbox("確定内容に応じて修正する", R::new(0, 168, 276, 34))?;
    let input_support_advanced = p.checkbox("高度な自動修復を行う", R::new(288, 168, 264, 34))?;
    let input_support_vowel_count = p.checkbox("母音の過不足", R::new(0, 220, 276, 34))?;
    let input_support_consonant_extra = p.checkbox("子音の超過", R::new(288, 220, 264, 34))?;
    let input_support_n_count = p.checkbox("Ｎの過不足", R::new(0, 272, 276, 34))?;
    let input_support_dakuten_swap = p.checkbox("゛／゜の誤り", R::new(288, 272, 264, 34))?;
    let input_support_tsu_sokuon = p.checkbox("つ→っ", R::new(0, 324, 276, 34))?;
    let input_support_wa_wo = p.checkbox("わ→を", R::new(288, 324, 264, 34))?;
    let input_support_small_u = p.checkbox("ぅ→う", R::new(0, 376, 276, 34))?;
    let input_support_fuzzy_proper_nouns =
        p.checkbox("あいまいな固有名詞", R::new(288, 376, 264, 34))?;
    let input_support_reset = p.button("初期値に戻す", R::new(408, 444, 144, 34))?;

    let mut p = layout.page(
        parent,
        "英単語・記号置換",
        "英単語のつづりと、数字・英数字直後の記号を置換します。",
        480,
    )?;
    let input_symbol_panel = p.window();
    let input_support_english_to_katakana = p.checkbox(
        "英単語のつづりをカタカナ語に変換する",
        R::new(0, 108, 552, 34),
    )?;
    p.section("長音・句読点の自動置換", 192)?;
    let input_support_period_after_digit =
        p.checkbox("句点（。）→ピリオド（．）", R::new(0, 232, 552, 34))?;
    let input_support_comma_after_digit =
        p.checkbox("読点（、）→カンマ（，）", R::new(0, 284, 552, 34))?;
    let input_support_middle_dot_after_digit =
        p.checkbox("中黒（・）→スラッシュ（／）", R::new(0, 336, 552, 34))?;
    let input_support_long_vowel_after_alnum =
        p.checkbox("長音（ー）→マイナス（－）", R::new(0, 388, 552, 34))?;

    let mut p = layout.page(
        parent,
        "表示",
        "候補ウィンドウと設定画面の外観を選びます。",
        288,
    )?;
    let display_panel = p.window();
    let appearance = p.row_combo("テーマ", 108)?;
    for value in AppearanceTheme::ALL {
        add_combo(appearance, appearance_label(value));
    }
    p.helper(
        "ハイ コントラスト設定が有効な場合は、Windowsの配色を使用します。",
        176,
    )?;

    let mut p = layout.page(
        parent,
        "アプリ別の設定",
        "アプリごとに入力モードや表記スタイルを切り替えます。",
        636,
    )?;
    let profile_panel = p.window();
    let profile_list = p.list(R::new(0, 108, 188, 420))?;
    p.text("実行ファイル名", R::new(212, 108, 340, 24), TextRole::Body)?;
    let profile_process = p.edit("", R::new(212, 140, 340, 34), false)?;
    p.text(
        "既定の入力モード",
        R::new(212, 192, 340, 24),
        TextRole::Body,
    )?;
    let profile_mode = p.combo(212, 224, 340)?;
    for value in Mode::ALL {
        add_combo(profile_mode, mode_label(value));
    }
    let profile_prediction = p.checkbox("予測入力を使う", R::new(212, 276, 340, 34))?;
    p.text("候補の確定", R::new(212, 328, 340, 24), TextRole::Body)?;
    let profile_suggest = p.combo(212, 360, 340)?;
    for value in ["Tab", "Shift+Enter", "使わない"] {
        add_combo(profile_suggest, value);
    }
    p.text("表記スタイル", R::new(212, 412, 340, 24), TextRole::Body)?;
    let profile_notation = p.combo(212, 444, 340)?;
    for value in NotationStyle::ALL {
        add_combo(profile_notation, value.label());
    }
    add_combo(profile_notation, NOTATION_STYLE_CUSTOM_LABEL);
    select_combo(profile_mode, mode_index(Mode::Hiragana));
    select_combo(profile_suggest, suggest_index(SuggestAccept::Tab));
    select_combo(profile_notation, notation_style_index(None));
    let profile_save = p.button("追加／更新", R::new(212, 504, 164, 34))?;
    let profile_delete = p.button("削除", R::new(388, 504, 164, 34))?;
    p.helper(
        "例: code.exe　設定はアプリが入力コンテキストを作成したときに適用されます。",
        568,
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

pub(super) fn create_dictionary_controls(
    parent: HWND,
    layout: &mut Presentation,
) -> WindowsResult<DictionaryControls> {
    let mut p = layout.page(
        parent,
        "ユーザー辞書",
        "よく使う単語を登録し、変換候補に追加します。",
        548,
    )?;
    let entries_panel = p.window();
    let list = p.list(R::new(0, 108, 188, 368))?;
    p.text("読み", R::new(212, 108, 340, 24), TextRole::Body)?;
    let reading = p.edit("", R::new(212, 140, 340, 34), false)?;
    p.text("単語", R::new(212, 192, 340, 24), TextRole::Body)?;
    let surface = p.edit("", R::new(212, 224, 340, 34), false)?;
    p.text("品詞", R::new(212, 276, 340, 24), TextRole::Body)?;
    let part_of_speech = p.combo(212, 308, 340)?;
    for pos in UserPartOfSpeech::ALL {
        add_combo(
            part_of_speech,
            &format!("{} — {}", pos.spec().name, pos.spec().label),
        );
    }
    select_combo(part_of_speech, 0);
    p.text("コメント", R::new(212, 360, 340, 24), TextRole::Body)?;
    let comment = p.edit("", R::new(212, 392, 340, 34), false)?;
    let add = p.button("追加", R::new(212, 452, 104, 34))?;
    let update = p.button("更新", R::new(330, 452, 104, 34))?;
    let delete = p.button("削除", R::new(448, 452, 104, 34))?;

    let mut p = layout.page(
        parent,
        "辞書ファイル",
        "辞書を読み込む、またはファイルに書き出します。",
        428,
    )?;
    let io_panel = p.window();
    p.text("ファイル", R::new(0, 108, 552, 24), TextRole::Body)?;
    let path = p.edit("", R::new(0, 140, 552, 34), false)?;
    let format = p.row_combo("形式", 200)?;
    add_combo(format, "自動");
    for value in DictionaryFormat::ALL {
        add_combo(format, value.name());
    }
    select_combo(format, 0);
    let import_mode = p.row_combo("読み込み方法", 252)?;
    add_combo(import_mode, "追加して登録");
    add_combo(import_mode, "すべて置き換え");
    select_combo(import_mode, 0);
    let import = p.button("インポート", R::new(248, 312, 146, 34))?;
    let export = p.button("エクスポート", R::new(406, 312, 146, 34))?;
    p.helper(
        "MS-IME／ATOKはUTF-16LE、Sakura／MozcはUTF-8です。未対応の項目は登録しません。",
        368,
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

pub(super) fn create_learning_controls(
    parent: HWND,
    layout: &mut Presentation,
) -> WindowsResult<LearningControls> {
    let mut p = layout.page(
        parent,
        "学習履歴",
        "確定済みの学習履歴を新しい順に表示します。",
        460,
    )?;
    let history_panel = p.window();
    let list = p.list(R::new(0, 108, 552, 320))?;
    let mut p = layout.page(
        parent,
        "学習の操作",
        "学習履歴の更新、書き出し、消去を行います。",
        500,
    )?;
    let operations_panel = p.window();
    let refresh = p.button("最新の状態に更新", R::new(0, 108, 188, 34))?;
    p.text("書き出し先", R::new(0, 188, 552, 24), TextRole::Body)?;
    let export_path = p.edit("", R::new(0, 220, 552, 34), false)?;
    let export = p.button("TSV 出力", R::new(408, 272, 144, 34))?;
    p.section("学習の消去", 348)?;
    p.helper("完了した消去・出力は［キャンセル］では戻せません。", 388)?;
    let clear = p.button("学習を消去", R::new(0, 440, 144, 34))?;
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

pub(super) fn create_diagnostics_controls(
    parent: HWND,
    layout: &mut Presentation,
) -> WindowsResult<DiagnosticsControls> {
    let mut p = layout.page(
        parent,
        "詳細設定・診断",
        "通信のタイムアウトと整合性の状態を確認します。",
        552,
    )?;
    let text = p.multiline(R::new(0, 108, 552, 300))?;
    let refresh = p.button("最新の状態に更新", R::new(0, 432, 188, 34))?;
    let clear = p.button("カウンターを消去", R::new(204, 432, 188, 34))?;
    p.helper(
        "診断ログは1 MiBに制限され、例外的なタイムアウト時だけ記録されます。",
        492,
    )?;
    Ok(DiagnosticsControls {
        text,
        refresh,
        clear,
    })
}

pub(super) fn create_update_controls(
    parent: HWND,
    layout: &mut Presentation,
) -> WindowsResult<UpdateControls> {
    let mut p = layout.page(
        parent,
        "更新の確認",
        "設定画面を開いたときの更新確認を設定します。",
        288,
    )?;
    let settings_panel = p.window();
    let enabled = p.checkbox("設定画面の起動時に更新を確認する", R::new(0, 108, 552, 34))?;
    let save = p.button("設定を保存", R::new(0, 176, 144, 34))?;
    let mut p = layout.page(
        parent,
        "利用可能な更新",
        "新しいリリースを確認し、検証してインストールします。",
        348,
    )?;
    let available_panel = p.window();
    let check = p.button("今すぐ確認", R::new(0, 108, 144, 34))?;
    let apply = p.button(
        "ダウンロードして検証・インストール",
        R::new(0, 176, 360, 34),
    )?;
    p.helper(
        "取得前に配布先、サイズ、SHA-256とAuthenticode署名を確認します。",
        244,
    )?;
    let mut p = layout.page(
        parent,
        "更新の状態",
        "更新確認とインストールの結果を表示します。",
        380,
    )?;
    let status_panel = p.window();
    let result = p.multiline(R::new(0, 108, 552, 160))?;
    p.helper(
        "成功・再起動が必要・タイムアウト・失敗を区別して表示します。",
        300,
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
