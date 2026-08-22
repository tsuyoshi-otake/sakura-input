# Issue #79 同音異義語の辞書詳細・評価検証

検証日: 2026-08-22

## 目的と境界

ユーザー提示の同音異義語を正規化し、最終辞書に存在する候補へ、選択時に意味を確認できる exact-entry detail を追加した。候補の追加、候補コスト、順位、分かち書きは変更しない。複合語は評価用観測データとして保存し、合成された複合候補へ構成語の detail を誤って継承させない。

detail-only source は、最終候補の reading、surface、left/right ID、word/prediction cost、flags がすべて一致する場合だけ detail を付ける。一致しない行、重複 identity、壊れた入力は fail closed とする。curated detail が存在する pair は、release manifest と target manifest の整合性検証後に旧 identity の LLM detail を抑止する。

## 正規化結果

| 区分 | 件数 | 扱い |
| --- | ---: | --- |
| 提示された一意な `(reading, surface)` | 1,568 | 全件を監査 |
| 明示除外 | 34 | 読み不一致、同音でない表記、入力誤りなど |
| 通常の既定辞書に存在 | 1,372 | 全 pair に detail あり |
| 14カテゴリのリリース辞書に存在 | 1,496 | 全 pair に detail あり |
| リリース辞書にも不在 | 72 | 候補や順位を新設せず hold |
| 最終状態で未被覆 | 0 | 両構成とも pair 単位・exact identity 単位でゼロ |

新規の `data/curated-homophone-details.tsv` は、両構成で共通する 980 pair、1,163 exact identities を保持する。`data/curated-homophone-system-details.tsv` は14カテゴリ構成で追加される301 pair、319 exact identitiesだけを保持し、`-SystemCategoryDirectory` 指定時に条件付きで取り込む。これにより通常の小構成でも存在しない identity を黙認せず、exact match の fail-closed を維持する。最終辞書に複数 identity がある `昨日`、`季節`、`紹介` なども ordinal 単位で監査した。保留と除外は `verification/homophone-detail-holds.tsv` に理由付きで固定した。

## 優先語と複合語

- 優先評価は 10 readings、23 surfaces。`決済/決裁`、`保証/補償/保障`、`制作/製作/政策`、`規定/規程/既定`、`改定/改訂`、`自立/自律`、`補足/捕捉`、`要件/用件`、`対象/対照`、`機能/昨日` を `eval/corpus/behavioral/homophone-details/fixture.tsv` に固定した。
- 複合語は 158 candidate-observation cases、330 submitted surfaces と、左右文脈で正解が変わる 2 context-required cases を `eval/corpus/behavioral/homophone-compounds/fixture.tsv` に固定した。
- 複合語の候補列は無条件の Top-1 正解ではない。現段階では順位調整を行わず、将来の文脈評価用観測データとして扱う。
- unit test で、単語 `機能` は exact entry detail を持つ一方、2 segment で合成した `機能テスト` は exact entry ordinal を持たず、構成語 detail を継承しないことを確認した。

## 候補集合・順位の不変性

detail 追加前後の全14カテゴリ辞書について、candidate を構成する先頭7列（reading、surface、left/right ID、word/prediction cost、flags）を集合比較した。

- 今回の detail-only 最終版と直前候補版: missing 0、added 0。
- 着手時 baseline と最終版: missing 0、added 1。唯一の追加は `しゅうじ -> 週次` で、同時進行中の既存 `data/conversion-priorities.tsv` 変更由来。Issue #79 の detail-only source は category overlay へ渡していない。

したがって、Issue #79 による候補追加、候補削除、コスト・flags の変更はない。

## 辞書ビルド結果

| Build | Entries | Details | Bytes | SHA-256 |
| --- | ---: | ---: | ---: | --- |
| baseline | 604,586 | 29,712 | 46,642,816 | `493e2dd143f25c6f1cc6f34f3540e2e3c06a88aba9653e3b41ad270ee05c917c` |
| default final | 604,587 | 30,873 | 46,732,992 | `6c935f5eabced8e44b2351e8634ac05fe45e01b8765b687fbc76c9628958aef8` |
| 14-category release final | 1,399,245 | 39,237 | 81,801,048 | `9344aa01cf2196f83089686a1bb867b01c8fb5780cb9c115bde36ffefb10734e` |

両 final build は成功した。14-category release final の独立 repeat は entries、details、bytes、dictionary hash、全 report/artifact hash が一致し、`deterministic_repeat: true` となった。release の curated detail import は2,443 input records、2,442 emitted、既存 detail による抑止1件。review済み000010 releaseは236 recordsを検証し、149 unique terms / 157 exact detailsを出力した。

## 検証手順と結果

1. `scripts/build-dictionary.ps1 -SystemCategoryDirectory C:\Users\developer\tmp\atok36-analysis-20260809\system-lexicon-v1 -OutputDirectory C:\Users\developer\tmp\sakura-input-issue79-release-20260822-3`
   - Expect: 2回の生成物が一致し、report が `deterministic_repeat: true`。
   - Result: PASS。1,399,245 entries、39,237 details、81,801,048 bytes。通常構成も `-SkipDeterminismCheck` で別途生成し、604,587 entries、30,873 detailsで成功。
2. `C:\Users\developer\tmp\issue79-audit.ps1` を final `detail-coverage.tsv`、全カテゴリ辞書、review済み000010 releaseへ実行。
   - Expect: accepted present pair とその全 exact identities の未被覆が0。
   - Result: PASS。14カテゴリ版は1,496 / 1,496 pairs、通常版は1,372 / 1,372 pairs。いずれも uncovered pair 0、uncovered exact identity 0。
3. `C:\Users\developer\tmp\issue79-compare-categories.ps1` で候補先頭7列を比較。
   - Expect: Issue #79 の detail-only 差分が0。
   - Result: PASS。最終版と直前候補版は missing 0 / added 0。
4. `cargo test -p dictc --test curated_details --test homophone_compounds`
   - Expect: exact attachment、fail-closed、データ件数、評価fixture、複合語非継承が成功。
   - Result: PASS。11 passed / 0 failed。
5. `cargo fmt --all -- --check`、`cargo test --workspace`、`git diff --check`
   - Expect: 全コマンド成功し、テストランナーの残存プロセスなし。
   - Result: `fmt` と `diff --check` は PASS。`cargo test --workspace` は Issue #79 のテストを含む全通過後、既存の実engine統合テスト `a_real_engine_serves_a_real_client_over_an_owned_private_pipe` 1件だけが FAIL。現在の既存・staged engine実装は Shift英字中の Space を半角空白として保持する一方、この既存test helperは旧仕様の辞書変換を期待しており、単独再実行でも `expected ["Claude", "Claude Code"], got "CLAUDE "` を再現した。最初のpanicによるlock poisoningで同runの後続6件も連鎖失敗した。
   - Isolation: 上記1件を `--skip` した `cargo test -p sakura-engine` は残り全件成功し、`cargo test --workspace --exclude sakura-engine` も成功した。この既存不整合は Issue #79 のdictc/data変更とは依存経路がなく、本Issueでは変更していない。

## リリースと再インストール

- release workspace build と Inno Setup 6.7.3 による installer build は成功し、warning は0件。生成物は version `1.0.18`、build ID `3ccd617ea7c8ba4c`、29,387,903 bytes、SHA-256 `472c3a7bdcf488252c261c0a8aa858740d9c24b9dbfdcd51f65c8448316672c1`。
- silent reinstall log で `Installation process succeeded.`、profile enable exit code 0、`Need to restart Windows? No`、`Log closed.` を確認した。
- 導入先 `C:\Program Files\Sakura Input\versions\1.0.18-3ccd617ea7c8ba4c` の report 対象24 payloadを再ハッシュし、bytes／SHA-256が24 / 24一致した。導入済み辞書は81,801,048 bytes、SHA-256 `9344aa01cf2196f83089686a1bb867b01c8fb5780cb9c115bde36ffefb10734e`。
- COM登録 `HKLM\Software\Classes\CLSID\{C18F44DE-39E0-4B16-8D28-D5DE35BB11BC}\InprocServer32` は新しい `sakura_tsf.dll` を指し、実行中のrenderer／engineも同じbuild IDだった。旧build、installer、cargo、rustc、Inno Setup、対象test runnerの残存プロセスは0件。
- 再インストール後に `cargo fmt --all -- --check`、Issue #79のdictc回帰テスト11件、`git diff --check` を再実行し、すべて成功した。

## 残る範囲

14カテゴリのリリース辞書にも存在しない72 pairは、意味だけを追加できないため hold とした。通常の既定辞書だけで見ると不在は196 pairである。これらを候補として追加する場合は、読み、品詞ID、コスト、順位への影響を別Issueで審査する。複合語の文脈別Top-1品質も、文脈対応の評価・ランキング機構を導入する別工程とする。
