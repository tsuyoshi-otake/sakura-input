# Tiny / 文節変換 600件評価（2026-08-12 再評価）

> Historical evidence: this report measured the former DeBERTa Tiny payload.
> That runtime and installer path were removed under Issue #32; the commands
> below are retained for provenance and are not current build instructions.

## 結論

この再評価では、Tiny の候補再順位付けは文節変換の Top-1 を改善しなかった。両モードとも正解は 544/600（90.67%）で、勝敗は 0 勝 / 0 敗 / 600 引き分けだった。候補生成側は Recall@6 が 600/600（100%）、MRR@6 が 0.950 だった。したがって、現時点で「Tiny の方が良い」とは言えず、候補生成と文節変換を土台にし、Tiny は設定で任意に有効化する再順位付けとして扱う。

## 入力と再現性

- corpus: `corpus/neural-eval-communication-draft.tsv`
- 件数: 600（チャット 200、メール 200、一般 200）
- SHA-256: `db66bd68559ea434c37760c4aa13313da0701fae869323faffc625145e60d087`
- 辞書: `artifacts/release/system.dic`
- worker: `artifacts/release/sakura_neural_worker.exe`
- model: `artifacts/release/neural/deberta-v2-tiny-japanese-char-wwm`
- reports: `C:\Users\developer\tmp\sakura-neural-eval-communication-long-v3.json`, `C:\Users\developer\tmp\sakura-neural-eval-communication-all-normal-v3.json`
- acceptance flag: `acceptance_eligible: true` because both runs satisfy the 600-row/chat-200/email-200 gate. The corpus is still an authored draft, not an independently reviewed model-selection holdout.

実行コマンド（Windows）:

```powershell
rtk cargo run -p dictc --bin neural-eval -- --dictionary artifacts\release\system.dic --corpus corpus\neural-eval-communication-draft.tsv --worker artifacts\release\sakura_neural_worker.exe --model-dir artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm --mode long --report C:\Users\developer\tmp\sakura-neural-eval-communication-long-v3.json
rtk cargo run -p dictc --bin neural-eval -- --dictionary artifacts\release\system.dic --corpus corpus\neural-eval-communication-draft.tsv --worker artifacts\release\sakura_neural_worker.exe --model-dir artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm --mode all-normal --report C:\Users\developer\tmp\sakura-neural-eval-communication-all-normal-v3.json
```

## 結果

| 条件 | 正解 Top-1 | Tiny 適用 | fallback | 勝 / 敗 / 引き分け |
| --- | ---: | ---: | ---: | ---: |
| 文節変換 + Tiny（長文のみ） | 544/600 | 191 | 409 | 0 / 0 / 600 |
| 通常の変換 + Tiny（全 Normal） | 544/600 | 192 | 408 | 0 / 0 / 600 |

候補生成は両条件で共通し、Recall@6 は 600/600、MRR@6 は 0.950 だった。今回の再評価では両モードとも全600行がスコープ上は適格で、worker が status 2 を返した行（long 409、all-normal 408）は元の文節変換順位へ fail-closed で戻っている。

スライス別の正解数（両条件で同じ）:

| スライス | 文節変換 Top-1 | Tiny 後 Top-1 |
| --- | ---: | ---: |
| チャット | 185/200 | 185/200 |
| メール | 170/200 | 170/200 |
| 一般 | 189/200 | 189/200 |

スライス別の候補生成指標は、チャットが Recall@6 200/200・MRR@6 0.956、メールが 200/200・0.922、一般が 200/200・0.973 だった。

## 解釈上の注意

これは通信文面を追加した回帰用 draft corpus であり、独立した人手レビュー済みのモデル選定用 holdout ではない。worker が status 2 を返した行は元の文節変換順位へ fail-closed で戻っているため、適用件数だけを品質向上と解釈してはいけない。今回の測定から確実に言えるのは「この 600 行では Tiny が順位を変えず、改善の証拠が得られなかった」までである。MRR は候補生成の順位を表すもので、Tiny の品質向上を意味しない。

また、Tiny は現在候補を生成する変換エンジンではなく、文節変換が作った最大 6 候補を再順位付けする worker である。「Tiny だけで変換する」設定は製品挙動として提供していない。
