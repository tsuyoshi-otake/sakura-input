# Issue #412: 既存の活用形への説明継承

2026-10-28。既存Mozc entryについて、全基本形から生成可能な読み・
表記を照合し、品詞・活用型・活用形が適合する親が一つのexact identity
に定まる場合だけ、その基本形の説明を「基本形の意味」として付与する。
既存説明は維持し、元entryと最終entryのID・cost・flagsの不一致や親の
不在、複数親、親の説明なしはそれぞれ保留する。関係語は継承しない。

| 項目 | 追加前 | 追加後 |
| --- | ---: | ---: |
| 全 entry | 661,428 | 661,428 |
| 説明付き entry | 53,567 | 58,703 |
| 説明なし | 607,861 | 602,725 |
| 辞書画像サイズ | 39,584,492 bytes | 39,710,068 bytes |

native source 459,787件中、対応候補あり31,688件。内訳は追加5,136、
複数親3、最終entry変更29、既存説明239、基本形の説明なし26,281。
対応なしは428,099。基準の30%には198,429件が必要で、残り139,726件。
追加前に説明のなかった native entry「歩き」は「「歩く」の活用形。
基本形の意味：足を交互に動かして進む。」を返した。

## 検証

- 既定の2-pass buildで全成果物一致。14カテゴリと、DIDX・DREC・
  DREL・DTOF・DTXT以外の全 image table は追加前後でバイト一致。
  追加前の53,567詳細はordinalごとに説明、表示説明、関連語が一致。
- 両画像で `issue_94_`、`issue_348_`、`issue_390_`、`issue_392_`
  の順位テストが成功。native親の一意性、entry変更、既存詳細保持の
  focused test、`cargo test --locked -p dictc-core -p dictc`、対象3
  packageのclippy、format、report helper testsも成功。
- generatedとnativeの両import件数は、既存形式を受け入れる検証helperで
  照合し、missing、型不正、overflow、不整合を拒否。終了後に
  `ci/check-process-clean.ps1` で対象プロセスなしを確認。
- SHA-256: 追加前 `042cf52f4d0f8e94f2898fb688d79ee9939cce7cf97afc000202a7565e11934f`、
  追加後 `10192b74e612d7ce605cfc9daa695aef14acda7bb57b5580b4c2da99e10bf22f`。

## 速度

同一マシンの release build で追加前後を順に測定。通常変換と bridge は
各500 warmups・5,000 samples、画像openと4種類の候補検索は31 samples、
詳細検索は101 warmed samples。各1回の比較なのでばらつきと変更による
差は区別できない。

| 対象 | 追加前 p50 / p99 | 追加後 p50 / p99 |
| --- | --- | --- |
| 通常変換 | 0.877 / 1.535 ms | 0.832 / 1.446 ms |
| cross-commit bridge あり | 1.463 / 2.423 ms | 1.392 / 2.278 ms |
| 4種類の候補検索・表示 | 1.145 / 1.801 ms | 1.197 / 1.913 ms |
| 辞書open・検証 | 55.038 / 77.283 ms | 51.844 / 63.305 ms |
| 既存「食べない」詳細検索・preview | 0.1 / 0.3 μs | 0.1 / 0.2 μs |

openにはOS cacheを含む。実アプリのキー入力・画面描画・冷間起動は
測定していない。説明付き割合は約8.88%で、30%には未到達。
