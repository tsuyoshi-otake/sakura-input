# Issue #412: 活用形の基本形説明

2026-10-28。基本形の説明がある生成活用形について、基本形の意味として
明示して表示する。否定・条件・時制を基本形の肯定的な意味に置き換えない。
関連語は継承しない。既存の活用形説明は優先し、候補やコストは変更しない。

## 対応と計数

既存generatorから全基本形が生成しうる `(reading, surface)` を調べる。
生成元が複数の exact identity に対応する場合は、生成時の先勝順序を
根拠にせず保留する。入力overlayを同じgeneratorで再生成して全行の
identity・cost・flagsと照合し、不一致はビルドを失敗させる。final entry
も同じidentity・cost・flagsであること、基本形のfinal entryと説明が
存在することを確認する。説明は既存の詳細の統合後に加える。

| 対象 | 件数 |
| --- | ---: |
| 生成活用形 | 166,300 |
| 新しく説明を付与 | 19,664 |
| 複数の基本形に対応するため保留 | 2,689 |
| 既存の説明を維持 | 95 |
| 基本形の説明がないため保留 | 143,852 |
| 基本形不在・final変更・対応なし | 0 |

最終辞書は661,428 entries、説明は31,335 → 50,999。これは全体の
7.71%であり、未説明は610,429 entries。元からMozcに存在する活用形や
基本形に説明がない生成形は、この処理だけでは埋まらない。

## 検証結果

- 既定入力の2-pass辞書ビルド: PASS。比較した全中間成果物、14カテゴリ、
  辞書画像と詳細レポートのSHA-256が各passで一致。
- 追加前後の14カテゴリと詳細以外の全image table: バイト一致。
  詳細tableのDIDXは31,335 → 50,999。入力・候補・コスト・接続・
  単漢字tableは不変。
- 基本形の一意性、否定形と条件形の表記、既存詳細の優先、基本形不在、
  stale overlay、final entryの改変: `dictc-core` の回帰テストがPASS。
  `cargo test --locked -p dictc-core -p dictc` もPASS。
- 実辞書による `issue_94_`、`issue_348_`、`issue_390_`、`issue_392_`
  順位テスト: 追加前後ともPASS。
- 最終辞書の `かかない / 書かない` は「「書く」の活用形。基本形の意味：
  文字や文章を記すこと。」を返し、追加前の同一entryには説明がない。
- report helperは旧レポートの受理、正しいcountの一致、不正な型、
  count欠落とoverflowの拒否を検証。final reportの50,999件も再照合した。
- `cargo fmt --all -- --check`、対象3 packageの全target clippy
  (`-D warnings`)、`git diff --check`: PASS。
- `cargo test --locked --workspace --features
  'sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture,sakura-conversion-eval/learning-replay,sakura-conversion-eval/accuracy-gate'`
  を `ci/run-test-quiet.ps1` 経由で実行: PASS。終了後の
  `ci/check-process-clean.ps1`: 残存なし。
- `ci/check-dependency-rules.ps1` と `ci/check-facade.ps1`: exit 0。前者は
  既存の未実装ルールをPENDING、移行中の境界をWARNとして報告した。

## 測定

同一マシンでreleaseビルドを使い、追加前後を順に実行した。変換は各
500 warmupsと5,000 samples。起動は31 samples、詳細検索は101 warmed
samples。1組の比較なので計測ばらつきと原因は区別できない。

| 測定 | 追加前 p50 / p99 | 追加後 p50 / p99 |
| --- | --- | --- |
| 通常変換 | 0.836 / 1.475 ms | 0.827 / 1.419 ms |
| cross-commit bridgeあり | 1.391 / 2.320 ms | 1.376 / 2.229 ms |
| 辞書open・検証 | 46.471 / 55.297 ms | 46.356 / 51.413 ms |
| 既存「書く」詳細検索・プレビュー | 0.1 / 0.1 μs | 0.1 / 0.1 μs |
| 新規「書かない」詳細検索・プレビュー | 説明なし | 0.1 / 0.2 μs |

辞書は38,829,508 → 39,489,620 bytes（+660,112 bytes）。辞書openは
OSキャッシュを含み、表示描画・実アプリでのキー入力・冷間起動は測定外。
そのため、今回の測定だけで体感の無変化や将来の大量追加時の性能は
保証できない。標準 `detail-coverage.tsv` はLLMと継承追加前のtarget選択用。

追加前 SHA-256: `fb823ab4c2981c57fe15aa44adaeae9f8a0a8844226a04d28fbda50de6abf514`

追加後 SHA-256: `b9cca42209fb15d59ac2953fa5cc02562014b28346390f550eef2dfbadbb5d5b`
