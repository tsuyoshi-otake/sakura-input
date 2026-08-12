# 設定画面 User32 E2E シナリオ

設定画面の入力経路を、別プロセスの実 payload と Windows User32 の実デスクトップ入力で検証する。
クリックを `WM_COMMAND` や `SendMessageW(LB_SETCURSEL)` で代用してはいけない。
`SendMessageW` は、クリック後の状態を読むための同期クエリに限って使用する。

## 実行方法

通常のパッケージテストは、対話型デスクトップを必要としない。

```powershell
rtk cargo test -p sakura-settings
```

User32 E2E は明示的に ignored テストを起動する。

```powershell
rtk cargo test -p sakura-settings --test settings_topic_user32 `
  profile_topic_click_shows_only_profile_controls_and_keeps_status_out_of_actions `
  -- --ignored --exact --nocapture

rtk cargo test -p sakura-settings --test settings_topic_user32 `
  input_tree_click_shows_only_selected_conversion_controls `
  -- --ignored --exact --nocapture

rtk cargo test -p sakura-settings --test settings_topic_user32 `
  input_assist_topic_click_shows_only_input_assist_controls `
  -- --ignored --exact --nocapture

rtk cargo test -p sakura-settings --test settings_topic_user32 `
  conversion_category_click_normalizes_to_segment_controls `
  -- --ignored --exact --nocapture

rtk cargo test -p sakura-settings --test settings_topic_user32 `
  dictionary_topic_click_shows_only_the_selected_dictionary_group `
  -- --ignored --exact --nocapture

rtk cargo test -p sakura-settings --test settings_topic_user32 `
  normalizer_controls_are_discoverable_clickable_and_apply_persists_values `
  -- --ignored --exact --nocapture

rtk cargo test -p sakura-settings --test settings_topic_user32 `
  normalizer_reset_is_separate_from_its_group_and_restores_only_normalizer `
  -- --ignored --exact --nocapture

rtk cargo test -p sakura-settings --test settings_topic_user32 `
  input_method_radio_controls_are_physical_and_persist_to_the_engine_config `
  -- --ignored --exact --nocapture

rtk cargo test -p sakura-settings --test settings_topic_user32 `
  learning_and_update_topics_are_discoverable_and_clickable `
  -- --ignored --exact --nocapture
```

複数シナリオをまとめて実行する場合は、前景ウィンドウを奪い合う別の
テストスレッドを作らないため `--test-threads=1` を付ける。

```powershell
rtk cargo test -p sakura-settings --test settings_topic_user32 -- `
  --ignored --nocapture --test-threads=1
```

インストール済み payload も同じ実クリック経路で確認できる。`SAKURA_SETTINGS_E2E_EXE`
を指定した場合だけ Cargo の debug payload を置き換え、設定は一時 `LOCALAPPDATA`
へ隔離する。

```powershell
$env:SAKURA_SETTINGS_E2E_EXE = 'C:\Program Files\Sakura Input\versions\<build>\sakura_settings_payload.exe'
rtk cargo test -p sakura-settings --test settings_topic_user32 -- `
  --ignored --nocapture --test-threads=1
Remove-Item Env:SAKURA_SETTINGS_E2E_EXE
```

テスト内にもUser32デスクトップロックを持つが、Windowsの前景ウィンドウ制御は
テストプロセスのスケジューリングの影響を受けるため、まとめて実行するときは上記の
`--test-threads=1` を必須とする。同じデスクトップのカーソル・前景・Zオーダーを
奪い合わないことを、テストランナー側でも保証する。

テストは一時 `LOCALAPPDATA` を使うため、既存のユーザー設定・更新状態・辞書を読み書きしない。
テスト終了時には起動した payload、カーソル位置、前面ウィンドウを復元し、作成した一時ディレクトリを削除する。

## シナリオ一覧

| ID | シナリオ | 入力 | 合格条件 | 状態 |
| --- | --- | --- | --- | --- |
| SET-U32-001 | payload 起動と HWND 公開 | payload を別プロセスで起動 | 同一 PID の可視トップレベル HWND が 5 秒以内に得られる | 実装済み |
| SET-U32-002 | 初期トピック | なし | `SysTreeView32` が可視で、Sakuraの入力・変換階層の `基本` が選択され、基本パネルだけが可視 | 実装済み |
| SET-U32-003 | アプリ別設定への実クリック | TreeViewの文字列列を `SetCursorPos`、`SendInput(LBUTTONDOWN/UP)` で走査 | アプリ別の設定を選ぶと基本パネルが非表示、アプリ別パネルだけが可視 | 実装済み |
| SET-U32-004 | 基本設定への復帰クリック | TreeView先頭行へ `SetCursorPos`、`SendInput(LBUTTONDOWN/UP)` | 基本を選ぶとアプリ別パネルが非表示、基本パネルだけが可視 | 実装済み |
| SET-U32-005 | 前面性の維持 | 各クリック後に `GetForegroundWindow` | 設定画面が前面のままで、クリックが別ウィンドウへ流れない | 実装済み |
| SET-U32-006 | 適用ボタンの実クリック | `適用` の矩形中央へ `SendInput(LBUTTONDOWN/UP)` | 保存成功メッセージが表示される | 実装済み |
| SET-U32-007 | ステータスと操作列の分離 | ステータス／適用の `GetWindowRect` | ステータスの右端が適用ボタン左端以内で、下端も操作列内に収まる | 実装済み |
| SET-U32-008 | 失敗時の bounded cleanup | テストを正常終了・panic の両経路で終了 | 起動した payload が残らず、カーソルと前面ウィンドウが復元される | 実装済み |
| SET-U32-009 | 文字幅・句読点の初期値復元 | `文字幅・句読点` → `初期値に戻す` の矩形中央へ `SendInput(LBUTTONDOWN/UP)` | 文字幅・句読点トピックのステータスに初期値復元が表示され、連想変換などの兄弟設定へ作用しない | 実装済み |
| SET-U32-010 | 辞書トピックの実クリック | `辞書` → `辞書ファイルの入出力` → `登録単語` を物理クリック | 選択した項目のネスト済みパネルだけが可視 | 実装済み |
| SET-U32-011 | 連想変換への実クリック | Sakuraの入力階層TreeViewの行をUser32で走査し、`連想変換` を選択 | 連想変換のチェックボックスと説明だけが可視で、基本・文節変換・アプリ別パネルは非表示。Tinyの適用範囲は文節変換ページで設定する | 実装済み |
| SET-U32-012 | 文字幅・句読点のコントロール認識 | 選択ページの6つのComboBox（英字・数字・句点・読点・記号・括弧）をHWND・クラス・可視矩形で列挙し、幅／句点／読点／括弧の選択項目をComboLBoxへ物理クリック | 別ページのコントロールを触らず、`Width::Full`、独立した句点・読点選択（`PunctuationStyle::CommaPeriod`）、括弧スタイル（`BracketStyle::Square`）がApply後の隔離configへ保存される | 実装済み |
| SET-U32-013 | インストール済み payload の同一経路 | `SAKURA_SETTINGS_E2E_EXE` で Program Files 配下の payload を指定し、SET-U32-003/010/011/012 を実行 | debug payload と同じ HWND認識・物理クリック・隔離保存の結果になる | payload再インストール・debugとのSHA-256一致を確認済み。installed payloadの実画面クリックは未実行 |
| SET-U32-014 | 入力方法の実クリック | `ローマ字入力`／`カナ入力`を物理クリックしてApply | ラジオ選択が排他的に切り替わり、`input-method = "kana"`が保存される。初期値復元は当該ページだけに作用する | 実装済み |
| SET-U32-015 | 文字種の実クリック | 基本設定の`文字種`コンボを物理クリックして全角カタカナをApply | `default-mode = "katakana"`が保存され、別ページの初期値復元によって変更されない | 実装済み |
| SET-U32-016 | 変換方法の実クリック | `変換補助`のカテゴリを物理クリックして最初の葉`文節変換`へ正規化し、`変換方法`コンボを物理クリックして単文節変換をApply | `conversion-method = "single-segment"`が保存され、左の選択項目と右ペインがずれない。engine境界では次のsessionの候補が1文節になる | 実装済み（UI→config；engine境界は `pipe_round_trip` の設定テスト） |

## 追加する回帰シナリオ

以下は設定画面の機能拡張時に同じ User32 E2E ファイルへ追加する。未実装の機能を現行テストの合格条件に混ぜない。

| ID | シナリオ | 合格条件 | 状態 |
| --- | --- | --- | --- |
| SET-U32-101 | 辞書ページのトピック切替 | 選択した項目に対応する辞書パネルだけが可視 | 実装済み |
| SET-U32-102 | 学習ページのトピック切替 | 選択した項目に対応する学習パネルだけが可視 | 実装済み |
| SET-U32-103 | 更新ページのトピック切替 | 選択した項目に対応する更新パネルだけが可視 | 実装済み |
| SET-U32-104 | Tab フォーカス順 | タブ列 → 左トピック → 表示中の右ペイン → OK → キャンセル → 適用の順で移動し、非表示 HWND はスキップ | 実装済み（`tab_focus_order_skips_hidden_topics_and_ends_at_actions`） |
| SET-U32-105 | Esc / キャンセル | `Esc` で閉じ、未適用の外観・入力設定を保存しない | 実装済み（`escape_cancel_discards_unapplied_preferences`） |
| SET-U32-106 | OK / 適用の永続化 | User32 で変更・適用・再起動し、設定ファイルと次回表示が一致 | 実装済み（`apply_persists_preferences_across_a_user32_relaunch`） |
| SET-U32-107 | Light / Dark / Auto | コンボ選択後に候補ポップアップと設定画面の配色が同じテーマへ反映 | 分割検証済み（設定側 `theme_combo_updates_input_tree_colors_through_user32`、候補側 `appearance_switch_repaints_a_visible_candidate_popup`。同一テストでの跨プロセス結合は未実装） |
| SET-U32-108 | DPI 変更 | `WM_DPICHANGED` 後も日本語ラベル、左ペイン、操作列が切れず重ならない | 実装済み（`dpi_change_reflows_atok_property_sheet_grid_without_clipping`。150%相当のUser32通知で再配置し、元DPIへ復帰） |
| SET-U32-109 | High Contrast | OS のシステム配色を優先し、桜色だけに依存せず選択状態が判別できる | 実装済み（`UiTheme` の system-role 分岐を unit 検証。OS のアクセシビリティ設定をテスト中に変更する実E2Eは安全上未実施） |
| SET-U32-110 | 更新中のクローズ | 更新中に閉じようとしても処理を破棄せず、完了後に明示的な終端へ到達 | 実装済み（OK／キャンセル／閉じるの共通終端を unit 検証。実更新配布を伴うE2Eは未実施） |
| SET-U32-111 | 入力・変換の実挙動 | 幅・句読点・入力方法を User32 で変更して適用し、次の入力セッションで変換。設定ファイル、エンジンの `Normalizer`／`InputMethod`、表示／確定文字列が同じ値になる | UI→config は SET-U32-012/014、config→実エンジンは `pipe_round_trip` の width/punctuation/input-method E2E で検証。単一テストでの跨プロセス結合は未実装 |
| SET-U32-112 | 入力ツリーの選択ページ分離 | `文節変換`、`推測変換`、`連想変換`、`表示`を順に物理クリック | 選択した右ペインだけが可視で、前のページの操作可能な HWND が残らない | 実装済み |
| SET-U32-113 | 入力補助ページの実クリック | Sakuraの入力階層TreeViewの`入力補助`を `SetCursorPos` + `SendInput(LBUTTONDOWN/UP)` で選択し、可視なComboBoxとラベルを列挙 | 基本のキー設定・入力方法・文字種、および変換補助の変換方法は表示・操作できず、Space と Shift+Space の2つだけが可視 | 実装済み（`input_assist_topic_click_shows_only_input_assist_controls`） |
| SET-U32-114 | 入力補助設定の実挙動 | `入力補助`を物理選択し、Space と Shift+Space の2つのComboBoxをネイティブポップアップの行クリックで変更してApply | 選択中は入力補助ページだけが可視で、隔離configへ空白規則だけが保存される。キー設定・文字種・変換方法は既定値のまま | 実装済み（`input_assist_topic_click_shows_only_input_assist_controls`） |
| SET-U32-115 | 変換補助カテゴリの実挙動 | `変換補助`を物理選択し、最初の葉`文節変換`で変換方法をネイティブComboBoxの行クリックで変更してApply | カテゴリ自身の空ページを出さず、`文節変換`だけが可視。既存Preferencesの変換方法へ保存される | 実装済み（`conversion_category_click_normalizes_to_segment_controls`） |

| SET-U32-116 | 空白キーの実挙動 | `入力補助`で空白文字を「入力文字種と同じ／常に全角／常に半角」、Shift+スペースを「スペースの逆／常に全角／常に半角」から物理選択してApplyし、別の実エンジンへ Space と Shift+Space を送る | 設定ファイルへ2値が保存され、次のidle入力で通常SpaceとShift+Spaceがそれぞれ指定幅のコミットになり、変換中のSpace（候補表示）は従来の文節変換を維持する | 実装済み（UIの入力補助E2E＋engineの`idle_space_and_shift_space_follow_the_configured_width_policy`） |
| SET-U32-117 | 予測／Tiny適用範囲のライブ反映 | `推測変換`で予測入力、`文節変換`でTiny適用範囲（長文のみ／すべての通常変換／無効）を変更してApply | 隔離configへ保存され、実行中engineの次の入力境界でPredictionRuntime／LongConversionRuntimeが遅延起動または切り離される。既存の候補順・プライバシー除外・失敗時baseline保持は不変 | 実装済み（UIの保存・User32選択、engine `optional_prediction_worker_follows_a_live_configuration_change`／runtime configuration tests。UI→別engineを一つにした結合E2Eは未実装） |
| SET-U32-118 | 枠とリセット操作の視覚的分離 | `文字幅・句読点`を物理選択し、`入力・変換`グループ枠と`初期値に戻す`の`GetWindowRect`を確認してからボタン中央を `SendInput` でクリック | リセットの上端がグループ枠の下端より8 px以上下にあり、ボタン中央の`WindowFromPoint`はリセットボタン。リセットは文字幅・句読点だけを既定値へ戻す | 実装済み（`normalizer_reset_is_separate_from_its_group_and_restores_only_normalizer`） |
| SET-U32-119 | 説明と推測候補枠の視覚的分離 | `推測変換`を物理選択し、説明Staticと`推測候補`グループ枠の`GetWindowRect`を確認 | グループ枠の上端が説明の下端より8 px以上下にあり、タイトルや説明と重ならない | 実装済み（`input_tree_click_shows_only_selected_conversion_controls`） |
| SET-U32-120 | カテゴリ選択の葉への正規化 | `変換補助`／`入力支援`のカテゴリ行をそれぞれ物理クリックし、TreeViewの`TVGN_CARET`と先頭の子Itemを照合 | カテゴリの空ページを表示せず、TreeViewの選択はそれぞれ`文節変換`／`推測変換`の葉へ移り、右ペインはその葉だけを表示する | 実装済み（`conversion_category_click_normalizes_to_segment_controls`、`input_support_topic_click_shows_prediction_assistance_controls`） |

## 判定の原則

- 物理ポインタ経路の証拠は `SetCursorPos` の read-back と `SendInput` の挿入数 2 件で構成する。
- 状態の証拠は HWND の可視性、TreeView/ListBoxの選択状態、前面 HWND、矩形を User32 から再取得して判定する。
- TreeViewは対象Itemの矩形を、fixtureプロセス内の一時`RECT`を使う`TVM_GETITEMRECT`で取得する。辞書などのListBoxでは `LB_GETITEMRECT` と `ClientToScreen` を使う。固定スクリーン座標や行の総当たりクリックを使わない。
- 待機はすべて 5 秒以内の bounded poll とし、失敗時には payload を強制終了しても子プロセスが残らないことを確認する。
- テストは実装していない機能を暗黙に合格扱いしない。シナリオ表の「追加する回帰シナリオ」は未実装として扱う。

## UI後段の実エンジン境界

設定payloadとエンジンは別プロセスなので、UIの物理クリックと、保存済み設定を読み直した実エンジンの変換を同じテストプロセスへ混ぜない。
UI側は SET-U32-012 で隔離configへの書き込みを証明し、エンジン側は次の private pipe E2E で同じ形式を読み込み、実際の composition／commit を確認する。

```powershell
rtk cargo test -p sakura-engine --test pipe_round_trip `
  a_running_engine_applies_saved_width_punctuation_and_brackets_to_real_output `
  -- --exact --nocapture

rtk cargo test -p sakura-engine --test pipe_round_trip `
  a_running_engine_applies_saved_input_method_to_real_output `
  -- --exact --nocapture

rtk cargo test -p sakura-engine --test pipe_round_trip `
  a_running_engine_applies_saved_default_character_type_to_new_sessions `
  -- --exact --nocapture
```

`a_running_engine_applies_saved_input_method_to_real_output` は、同じ ASCII
レイアウト文字を送り、Romaji の `あ` ではなく Kana 設定時の `a` が
composition に現れることを、設定 watcher と別プロセスの private pipe 越しに確認する。

`a_running_engine_applies_saved_default_character_type_to_new_sessions` は、保存後に
作成した新しい入力セッションが `Mode::Katakana` で開始し、`ka` の実変換結果が
`カ` になることを確認する。既存セッションのモードを勝手にリセットしないことも
同時に維持する。

このエンジン境界テストは `LOCALAPPDATA`、pipe、child PID を隔離する。したがって、ATOKの既存ウィンドウやインストール済みSakura singletonを対象にしない。
