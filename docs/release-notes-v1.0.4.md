# Sakura Input 1.0.4

1.0.3 に対する保守リリースです。通常のインストール構成での入力動作・UI・変換エンジン・辞書・プロトコルは 1.0.3 と同じで、同梱辞書もバイト単位で同一です。変更は研究用ニューラル再順位付け経路の内部入れ替えだけです。

## 変更

- 研究用ニューラル再順位付け経路を Sakura-Rerank-Tiny-v1 系へ入れ替えました（#32）。

  従来の DeBERTa Tiny（`ku-nlp/deberta-v2-tiny-japanese-char-wwm`）を使う worker 実装、ビルド・エクスポートスクリプト、インストーラーの opt-in payload 節を削除しました。worker は、hash 固定の research-only manifest（未知フィールド拒否、`status = "research_only_gate_a_failed"`、配布未承認・final holdout 未使用フラグの強制）を通過した Sakura-Rerank-Tiny-v1 research prototype だけを受理します。

  engine 側の選択は、辞書 cost との合成をやめ、モデルの listwise スコアを直接使う方式へ変更しました。従来の合成方式は local cost を二重に数えて Top-1 を悪化させることが固定データセットで測定されています。スコア数・候補集合 fingerprint・候補ごとの fingerprint・スコアの有限性のいずれかが一致しない場合は、従来どおり元のローカル順位を維持します（fail closed）。

  品質評価の Gate A を通過しておらず、モデル artifact の再配布も承認されていないため、このリリースにモデルは同梱されません。既定では従来どおり無効で、production の既定値・設定 UI・protocol v1・非同期で候補順位を変えない境界・機微スコープ除外は変わりません。

## 同梱辞書

- `system.dic` 39,349,040 bytes
- SHA-256 `b7d08643395181f6d214866f9bb98646de366dc71caa15320effe774bc4c1d90`
- 1.0.3 と同一です。この変更は辞書に触れていません。

## 対応環境

Windows 11 build 22000 以降、x64、AVX 対応 CPU。32 bit ホスト用 DLL と ARM64 ネイティブ版は含みません。

## インストール前の確認

このリリースの `sakura_setup.exe` は 1.0.0〜1.0.3 と同じくローカルビルドで、Authenticode 署名を含みません。署名の検証はできないため、ダウンロード元と SHA-256 を確認したうえでインストールしてください。署名が必要な場合は、署名証明書の secret を登録したうえで Actions の署名付きリリースワークフローを使ってください。

アップグレードは versioned payload の side-by-side 切替で行われるため、通常の終了コードは `0` で、TSF DLL が使用中でも Windows の再起動は要求されません。ロックされた旧世代は、SYSTEM のログオン時メンテナンスタスクが後で削除を再試行します。

詳細は同梱の `README-ja.md` と `guide-ja.md` を参照してください。
