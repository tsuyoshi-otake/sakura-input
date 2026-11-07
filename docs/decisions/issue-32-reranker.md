## Issue #32 Sakura-Rerank-Tiny-v1 統合（1.0.5同梱準備中）

旧DeBERTa Tinyのruntimeとinstaller opt-in経路はowner指示で除去した。現在の任意rerankerは、自作の `Sakura-Rerank-Tiny-v1-research-prototype` だけを対象とする。2026-08-14にownerがMITライセンスでの配布と通常installerへの同梱を明示承認したため、1.0.5ではモデル、worker、ONNX Runtimeを同梱し、既定の`LongTextOnly` scopeで有効にする。既存設定が明示的に`off`なら上書きしない。研究時点のGate A未通過とfinal holdout未使用は変更せず、品質gate合格とは表現しない。

### Issue #440: #29 蒸留モデルと確信度ゲートへの差し替え

- 同梱モデルを `Sakura-Rerank-Tiny-v1-issue29-distilled`（sakura-rerank Issue #29 の最終student、FP32 SHA-256 `a58ebea1…`、model contract v2）へ替える。研究manifestは #29 report（`reports/issue-29-distill-gate.json`、v0.1.0-research.3、SHA-256 `42fbc5af…`）のバイト複製である。配置directory名 `neural/sakura-rerank-tiny-v1/` は変えない。
- 判断の根拠は sakura-rerank の dev 参照（1,788行、Gate A未通過のAI教師ラベル、独立Testではない）。製品と同じ入力に揃えると、出荷中v1は正解1,630・誤置換14、#29＋ゲートは正解1,611・誤置換3だった（片方だけ正解36対17、exact McNemar p=0.013）。精度向上ではなく、誤置換を減らす安全側への交換として扱う。v1の保護選択はdevで再現できないため、v1の実際の誤置換はこれより少ない可能性がある。
- 選択規則: 保護evidence groupの規則でモデル候補を選んだ後、ローカル選択（`preferred`）とのlogit差がf64で1.7を超えるときだけ置き換える。閾値は `sakura_rerank_proto::Selection::LOCAL_OVERRIDE_MARGIN` が唯一の定義で、runtime manifestの `selection` が一致しなければworkerはfail closedする。
- MIT配布・installer同梱・既定 `LongTextOnly` での有効化は、このモデルについて、v1の承認を流用せず、ownerが2026-11-07に改めて承認した。

### 現在の実装境界

- TSF、engine、workerはRust。workerは `crates/sakura-neural-worker` の `sakura_neural_worker.exe` で、ONNX Runtime DLLとモデルをプロセス境界の外部artifactとして動的に読む。
- モデル配置は `neural/sakura-rerank-tiny-v1/{model.onnx,manifest.json}`。manifestはモデルcontract、研究manifest SHA-256、Gate状態、final-holdout非使用、MITライセンス、配布承認、model size/SHA-256を厳密検証する。
- protocol v1を維持し、候補表記、local cost、fingerprintだけを渡す。context/reading tensorはゼロ、利用可能featureは正規化local cost、候補順、surface長、cost gap（#440から）だけである。モデルscoreはlocal costへ二重加算せず、保護evidence groupの規則と確信度ゲートを通した選択信号として使う。
- 追跡するFP32 artifactと研究manifestは `models/sakura-rerank-tiny-v1/` に置く。`scripts/build-sakura-reranker.ps1` が固定hashのONNX Runtime 1.28.0と合わせてrelease stagingを生成し、`scripts/stage-sakura-rerank.ps1` は既存directoryを上書きしない。

### プライバシーとフォールバックの不変条件

- cloud 送信はしない。worker が受け取るのは、表示前の候補スナップショットだけであり、本文・入力履歴・ユーザー辞書・学習ストアを渡さない。
- `Password`、`URL`、`Email`、`Digits`、未分類または未知の scope、`test_only` 入力は除外する。worker 不在、runtime/model/manifest の不備、検証失敗、timeout、異常応答では元順位を保持して fail closed する。
- UI は worker を待たず、候補を表示した後に非同期で並べ替えない。TSF/engine 本体の 15 MiB 予算と任意 worker のメモリは別境界であり、worker の実測値は未測定として扱う。

### 再開時の検証と未解決事項

- model-free unit test、実FP32 ONNXのprobeと2候補protocol v1 IPCは確認する。engineは候補fingerprintと順序を完全照合し、最強evidence group内の最大finite scoreを選び（同点は元の先頭順）、ローカル選択との差が1.7以下ならローカル選択を保持する。
- missing/malformed/stale/late/timeout/failed結果は必ず既存local rankingへfail closedする。候補表示後の並べ替え、学習・exact cache・ユーザー辞書優先順位の上書きは禁止する。
- Gate A/B、Windows CPU batch-one 10,000回以上のcold/warm latency、private working setは未完了の別工程であり、実モデルIPC成功やinstaller同梱を品質gate合格と表現しない。

このファイルは、別セッションでSakura Inputの作業を再開するAI／開発者向けの引き継ぎメモです。まずこのファイルと`README.md`を読み、必要に応じて`DESIGN.md`、`PLAN.md`、ユーザーが提示した`AGENTS.md`相当の指示を確認してください。
