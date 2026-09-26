# 設定の保存境界と自動更新通知の修正

2026-09-26 / Issue #267、#145。
修正前の調査は `settings-screen-audit.md`。既存の履歴修正 #265 / #266 は保持した。

## 変更した動作

1. 入力支援のリセットは編集コントロールにだけ反映する。全体の適用前に
   プロフィールを保存・削除しても、未適用の入力支援や他ページの値を保存しない。
   全体の入力検証もコピー上で行い、検証失敗で保存済みスナップショットを変えない。
   プロフィール保存・削除自身の失敗でも、保存済みスナップショットを維持する。
2. APIキーの既存長さ規則を公開の副作用なし検証APIへまとめ、他の設定を
   書き込む前に検証する。空欄は既存キーを保持する。
   保存は4段階（TOML、文章変換キー、AI設定、必要な場合のみAPIキー）。
   失敗時は成功済み、結果を確認できない段階、未実行の段階を明示し、
   キャンセルで成功済み分は戻らないことと、原因解消後の再適用を案内する。
3. 更新確認の起点を完了まで保持する。自動確認は成功・失敗・更新ありの
   いずれも結果表示だけとし、モーダル・ページ移動・インストールを起動しない。
   手動確認の更新発見はインストール確認、手動の失敗はエラー表示を維持する。
   同時1ジョブ、署名検証、インストールの同意境界は変更していない。

3保存先全体を原子的にする変更ではない。先行する保存が成功した後のI/O失敗は
依然として部分成功になる。失敗した段階も内部で一部書き込み済みの可能性があるため、
「未保存」と断定せず「保存を確認できない」と表示する。

## 検証基準と結果

| 基準 | Verify | Expect / 結果 |
| --- | --- | --- |
| リセットとプロフィール保存の分離 | `ui::save_tests::reset_then_profile_save_keeps_unapplied_global_preferences`、削除版 | プロフィールだけ永続化。成功 |
| 入力検証と保存済み状態の分離 | `failed_global_validation_does_not_leak_into_profile_save`、`oversized_api_key_prevents_every_write_and_keeps_the_saved_snapshot` | 不正Endpoint/APIキーでは書き込み0、後続プロフィール保存にも混入なし。成功 |
| プロフィールの保存失敗 | `failed_profile_save_does_not_change_the_saved_snapshot`、削除版 | 失敗した編集を保存済み状態へ入れない。成功 |
| 部分成功と再試行 | `each_store_failure_reports_committed_uncertain_and_unattempted_settings` | 4段階それぞれで停止、表示範囲一致、後段未実行、プロフィール保存後の再試行成功。成功 |
| APIキー空欄と明示適用 | `blank_api_key_is_preserved_and_reset_is_published_only_by_apply` | 資格情報書き込みなし、リセットはApplyで保存。成功 |
| キー検証の境界 | `sakura-user-prefs` のAPIキー検証4テスト | 空白、trim後2048 byte、上限超過、UTF-8のbyte長を検証。成功 |
| 自動更新の操作非中断 | `automatic_completion_keeps_native_edit_focus_values_and_page` | オフライン/タイムアウト/更新あり/最新/無効の完了を実Win32コントロールに注入。編集中の値・フォーカス・ページを維持、結果を保持。成功 |
| 更新の手動操作・重複抑止 | `ui::update_tests` の他4テスト | 手動更新発見は確認要求、失敗はエラー。同時実行中30回の開始要求はすべて拒否し既存結果を維持。成功 |
| ネイティブの破棄操作 | ignored `escape_cancel_discards_unapplied_preferences` | 実キーボード経路でEscape終了し未適用値を保存しない。1 passed |
| 回帰 | workspaceの規定feature付き全テスト | 2,007 passed / 0 failed / 97 ignored、114結果グループ |
| 静的解析・書式 | workspace all-targets Clippy `-D warnings`、`cargo fmt --all -- --check` | 成功 |
| 終了 | `ci/check-process-clean.ps1`、runnerのプロセス列挙 | リポジトリのテストプロセス残留なし |

最初の3回帰テストは修正前にすべて失敗し、期待値と異なる実ファイル／
メモリ状態を確認した。修正後は新規保存テスト8件、更新テスト5件、キー検証4件が成功。
対象パッケージのみの試験も114 passed / 0 failed / 21 ignored。
全体試験後の最後の変更は、入力検証テスト自身も偽の保存先を使うようにしたもの。
その単独再実行と最終fmt checkも成功した。

通常Cargoテストは `ci/run-test-quiet.ps1` とTKWを使用し、各実行に期限を指定。
`CARGO_HTTP_CHECK_REVOKE=false` を設定した。試験時LOCALAPPDATAは
`~/tmp/sakura-settings-fix-tests`。新しい保存試験は、TOMLだけ実際に一時保存し、
HKCUとCredential Managerは注入ストアへ置き換える。利用中の認証情報は変更しない。
ネイティブウィンドウ試験同士はMutexで直列化してフォーカス競合を防ぐ。

## 保存済み実行証拠

TKW storeは、Clippyを除き `~/tmp/sakura-settings-fix-tests/TKW/runs`。

| 実行 | request | run |
| --- | --- | --- |
| 修正前3失敗 | `19920e3b0f4547298a6cde6f25bacb45` | `6a03e080b0096d35d8d349785e175443` |
| 保存8テスト | `e0e081d6a88f46a0ab475e5e0cd56518` | `b050a0718b4d14a2ebb601b48e4eb2f4` |
| 対象パッケージ | `2bf439601e2344bd9d0ac26e8d4fcfe6` | `f2d1f4993ebfe29e7a4e27013b51b798` |
| workspace | `8f5c730d2bc2497ca0ad33e60c9f83f2` | `12e7a41488b937b326fc9d5febb74155` |
| Escape E2E | `1ef667c8a0314f92988896c6efb04fd7` | `ed12b6dcf36c0f303d4e6d3c20d4ab10` |
| 最終単独回帰 | `ce7ba7dd5f05478a8a01e733bf53be0e` | requestで取得可能 |
| 最終fmt check | `013a2776f05c4517a2b52a5681e31d3a` | requestで取得可能 |
| Clippy（既定store） | `f6e35dc3bb60459a825d3a2552f6ae0e` | requestで取得可能 |

全文の回帰結果は `~/tmp/sakura-settings-fix-tests/workspace-tests.txt` と
`targeted-tests.txt`。初回の修正後ビルドでは、更新テストがbinから見えない
テスト専用APIを参照してコンパイルに失敗した。表示用fixtureの値へ直し、
以降の全体Clippy・テストで解消を確認した。

## 範囲・所有者・IRV

- A. 変更範囲: `ui.rs` の編集値収集／入力支援リセット／プロフィール保存／
  更新起点と完了、`ui/save.rs` の保存順序・部分成功、対応するsave/updateテスト、
  既存 `ui_tests.rs` のネイティブ試験直列化、`sakura-user-prefs` のキー検証、README。
  外部契約の確認に `configuration.rs`、保存API、更新結果型と既存仕様を参照した。
- B. 拡大: グローバル設定の混入と同じ状態所有問題がプロフィール保存失敗にも
  あったため、追加・削除とも成功時だけスナップショットを更新した。
  保存先3種類を実ユーザーのストアへ書かずに障害注入するため、保存境界を
  独立させた。広範なUI分割やエンジン改造はしていない。
- C. 所有者: 編集中の値はWin32コントロール、保存済みTOMLはAppの
  `ConfigurationDocument`。`PreparedSave` は検証後の1回分を所有し、
  `SettingsStore` は段階ごとの成功／不確定を契約とする。APIキー制限は
  `sakura-user-prefs::validate_api_key` が単独で所有する。更新起点はジョブが
  完了まで所有し、表示の判断だけを純粋関数へ分けた。外部依存の追加なし。
- D. 次回IRV: 保存順序、部分成功の表示、障害注入は `ui/save.rs` と
  `ui/save_tests.rs` に集まり、キーの長さ確認にCredential Managerの内部を
  読む必要がなくなった。更新通知は起点・表示判断と `ui/update_tests.rs` を
  起点に追える。UIへの配線確認には引き続き `ui.rs` が必要。
  既存IRVベンチマークに設定専用ケースはなく、量的削減は主張しない。

依存検査R1/R2/R8/R9/R12/R13と公開facade 63項目は成功。
R3/R4/R6は将来構造のためPENDING、R5/R7は既存WARN。
IRV比較も既存HISTORY-STOREとCIでWARN（6,992対2,672 LOC、5,630対5,034 LOC）。
無条件ガイドは11,298 byteで24,576 byteの予算内。新規の境界違反はない。

## 残る検証範囲

署名付き更新の実ネットワーク取得、インストーラー起動、UAC、実Credential
Managerの書き込み失敗はこの試験では発生させていない。既存の更新処理と
資格情報保存APIを維持し、その手前のUI判断・保存順序を検証した。
この検証時点では未コミット・未リリース。利用中のインストールへの反映は行っていない。
