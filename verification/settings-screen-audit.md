# 設定画面の調査（2026-09-26）

対象: `codex/release-2.0.6-integrated`、HEAD `f55e1d5`。
既存の履歴修正 #265 / #266 は保持。今回、設定の製品コードは変更していない。

この文書は修正前の調査記録。後続の承認に基づく #267 / #145 の修正と検証は
`verification/settings-save-and-update-fixes.md` を参照。

## 確認した不備

以下は現行コードの状態遷移と保存順序から確認したもの。
障害注入や一連の物理クリックによる再現テストはまだ実施していない。

### 1. 入力支援のリセットが別ページの保存に混ざる

- 起点: `src/ui.rs:932` の「初期値に戻す」は、画面の値だけでなく
  `self.configuration.preferences.input_support` を直接既定値へ変更する。
- `save_profile` (`ui.rs:2036`) と `delete_profile` (`ui.rs:2068`) は、
  この同じ `ConfigurationDocument` 全体を保存する。
- そのため、保存済みの入力支援が既定値と異なる状態で、リセット →
  アプリ別設定を追加／更新 → キャンセル、と操作すると、全体の「適用」を
  押していなくても入力支援のリセットまで保存される。
- アプリ別設定そのものが即時保存される仕様は問題ではない。
  別の責務である未適用のグローバル設定が同時に保存されることが問題。
- 同じ共有状態には `save_global_settings` の検証途中までの値も入るため、
  保存エラー後のアプリ別設定の保存も確認対象になる。
- 修正方向: 保存済み設定と編集中の値の境界を守り、入力支援のリセットは
  コントロールへだけ反映する。プロフィール保存時の他項目不変を回帰検証する。

### 2. 「適用」が途中で失敗しても一部が保存済みになる

- `save_global_settings` (`ui.rs:1570-1585`) は TOML → キー設定の
  レジストリ値 → AI設定のレジストリ値 → APIキー、の順で保存する。
- 後段が失敗しても前段を戻さず、`ui.rs:4391` は一般的なエラーを表示する。
  何が保存済みかは表示されない。
- 明確な入力例は APIキーの長さ制限。`sakura-user-prefs/src/lib.rs:390`
  の 2,048 byte 制限は、前段の書き込み後に判定される。
- 個々の TOML 保存には原子的置換があるが、複数保存先をまとめる契約はない。
  既存の個別ストアの安全性と、画面全体の部分成功は区別する必要がある。
- 修正方向: 書き込み前に全入力を検証し、ストアごとの失敗に対して
  保存済み範囲と再試行方法を明示する。全体の原子性を約束する場合は別途設計する。

### 3. 自動更新確認の結果が設定操作を遮る

- 起動時に `UpdateOperation::Check` を開始する (`ui.rs:740`)。
  初期値は有効 (`updater.rs:85`)。
- 自動／手動の起点を保持せず、`finish_update` (`ui.rs:2427-2448`) は
  確認失敗と更新発見をモーダルの MessageBox で通知する。
- 設定を編集している途中でも、更新確認の結果によって操作が遮られる。
- 既存 Issue #145 と一致: https://github.com/tsuyoshi-otake/sakura-input/issues/145
- 修正方向: 自動確認の結果は更新ページ等に表示し、手動確認とインストール確認を区別する。

## 検証記録

Cargo は `CARGO_HTTP_CHECK_REVOKE=false`、既存 TKW と
`ci/run-test-quiet.ps1` を使用。テストは隔離した LOCALAPPDATA で実行した。

| 基準 | Verify | Expect / 結果 |
| --- | --- | --- |
| 現行ソースで起動可能 | `cargo build -p sakura-settings --bin sakura_settings_payload` | exit 0、成功 |
| 初期表示を取得可能 | `cargo test -p sakura-settings --test settings_topic_user32 visual::capture_initial_compact_settings -- --ignored --exact --test-threads=1` | 1 passed / 0 failed |
| 既存UI契約を維持 | `cargo test -p sakura-settings --bin sakura_settings_payload ui::tests:: -- --test-threads=1` | 26 passed / 0 failed |
| 検証プロセスを終了 | `ci/check-process-clean.ps1` と Cargo / rustc / 設定プロセス一覧 | 残留なし |

TKW 保存済み証拠:

- capture: request `8ed9e2c2f08c4cf281160868c805858e`、run `a3aafb11e7461fdf629480525c12dbb7`、既定 store。
- UI unit: request `f58fe23262e74824baf6e5da8b7f6d23`、run `c768c4ff787425c5167f2ad44e8990c5`、store `~/tmp/sakura-settings-audit-20260926/TKW/runs`。
- 初期画面と矩形: `~/tmp/sakura-settings-audit-20260926/screenshots/basic.bmp` / `basic.txt`。

画面操作ツールのアプリ／ウィンドウ一覧には検証用設定ウィンドウが現れなかった。
手動起動した検証プロセスのみ終了し、既存のネイティブキャプチャテストを使用した。
取得した初期画面は 640×480、96 DPI。全ページ・高DPI・物理クリックの網羅検証ではない。
Sakura Pad のコンボがこの画像では空欄に見えるが、選択値の実測と原因特定は未実施。
画像だけで機能不良と断定せず、上記3件とは分けて追加確認対象とする。

上記27テストの成功は、今回の3件が無いことの証明にはならない。
リセットとプロフィール保存の組合せ、保存途中の障害、自動確認の非モーダル性を
直接保証するテストは今回実行した範囲にない。

## 調査範囲・設計への影響

- A. 範囲: 設定 `ui.rs` のコマンド／入力反映／保存／更新完了、`ui/pages.rs`、
  `configuration.rs`、`storage.rs`、`paths.rs`、updater の設定読み込み、
  `ui_tests.rs`、User32 fixture / visual capture、ユーザー設定の保存API、
  engine 設定監視の公開契約。関連仕様は README、設定のUI検証文書、
  renderer/settings の現行分析、Issue #144 / #145 / #146。
- B. 拡大理由: Apply が10トピックと3種類の保存先を横断しているため、
  UIだけで判断できず保存APIと完全スナップショットの境界まで確認した。
  委譲先の契約マップも含む。既存のUIモジュールを分割・再構成していない。
- C. 所有者: 編集中のUI状態は `App`、TOMLの公開単位は
  `ConfigurationDocument`、AI設定と資格情報の保存は `sakura-user-prefs`。
  所有者、依存方向、外部契約、製品動作への変更はない。
- D. 次回IRV: この文書から原因と回帰シナリオへ進める。
  コード境界は変えていないため、構造的なIRV削減は未達。
  修正時には即時保存とApply待ちの区別、3保存先の部分成功を理解する必要がある。

テスト隔離の注意: LOCALAPPDATA の差し替えは TOML 等のファイルだけを隔離する。
AI設定の HKCU と Credential Manager は同じ利用者の領域のままなので、
この調査ではそれらへ書き込む実画面の「適用」やキー削除を実行していない。
