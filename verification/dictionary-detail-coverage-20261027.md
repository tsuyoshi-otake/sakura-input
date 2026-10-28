# 全辞書説明: 記号の最初の追加

2026-10-27。全エントリ100%を長期目標とする作業の最初の一部。
人名・活用形を含む残りの説明と、それらを安全に作成する方法は未完成。

## 検証基準

1. Verify: `cargo test --locked -p dictc-core --test curated_details` を
   `ci/run-test-quiet.ps1` 経由で実行。Expect: 元の候補を変更せず厳密に
   説明を紐付ける契約と、チェックイン済み説明の重複・空欄検査が通る。
   Result: PASS。各テスト実行後、`ci/check-process-clean.ps1` は残存なし。
2. Verify: `scripts/build-dictionary.ps1 -SkipDeterminismCheck` で既定の
   pinned inputs から再生成し、report と image の detail 数を比較。
   Expect: 既存66万エントリを保持し、指定した34件が追加される。
   Result: 661,428 entries、31,301 → 31,335 details。追加した34個の
   identityはすべてcoverageに現れ、既存identityの削除は0。残り630,093。
   2-pass決定性は今回未実施。
3. Verify: 前後のカテゴリ辞書と image directory の各 table の SHA-256 を
   比較。Expect: 候補、コスト、接続、検索に関わる table がすべて同じ。
   Result: 全14カテゴリが一致。差分は DIDX、DREC、DREL、DTOF、DTXT
   の詳細 table のみ。これらは詳細追加により再編成される。実辞書を使う
   `shipped_dictionary_ranking` の `issue_94_` 同音語順位テストも、
   追加前・追加後の両方でPASS。
4. Verify: `cargo test --locked --release -p sakura-engine --features
   dev-fixtures --test shipped_dictionary_ranking
   issue_83_cross_commit_bridge_release_percentiles -- --ignored --nocapture
   --test-threads=1` を各辞書に `SAKURA_SYSTEM_DIC` を向けて、
   `ci/run-test-quiet.ps1` 経由で実行。Expect: 両方が20 msのp99条件を満たす。
   Result: どちらもPASS。500 warmups、5,000 samples。下表を参照。
5. Verify: `cargo fmt --all -- --check`、`cargo clippy --locked -p dictc-core
   --test curated_details -- -D warnings`、`git diff --check`。
   Expect: すべて成功。Result: PASS。

| 測定 | 追加前 p50 / p99 | 追加後 p50 / p99 |
| --- | --- | --- |
| 通常変換 | 0.824 / 1.440 ms | 0.833 / 1.469 ms |
| cross-commit bridgeあり | 1.368 / 2.253 ms | 1.382 / 2.332 ms |
| 最大長のbridge入力 | 0.867 / 1.423 ms | 0.870 / 1.490 ms |

ベンチマークは順番に各1回実行した結果であり、ばらつきと因果関係は
分離できない。候補選択後の説明検索・UI描画、起動時の辞書検証、
大量追加後のメモリ・ファイルサイズへの影響は測っていない。

追加前 image SHA-256: `ea23b788b548341daed219900339164c58f511f38764ab1cb429e0b77833e84c`。
追加後 image SHA-256: `fb823ab4c2981c57fe15aa44adaeae9f8a0a8844226a04d28fbda50de6abf514`。
サイズは38,827,092 → 38,829,508 bytes（+2,416 bytes）。

`detail-coverage.tsv` は既存のtarget選択用で、LLM releaseのimport前に
書かれる。最終image全体の説明率を直接示すものではない。
