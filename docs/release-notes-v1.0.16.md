# Sakura Input 1.0.16

Cursor / Electron の Dual TSF で、変換一覧が出たあとの Space が次候補にならず、ひらがな確定と空白が入る不具合を直します（Issue #69）。

## 変更

- 同じプロセスの idle な `TextService` は、兄弟が読みを持っているあいだ物理 Space / `変換` をホストへ返しません。消費だけして engine と文書と候補 UI には触れません。
- 読みが無いときの idle Space は、これまでどおりホストへ届きます。owner teardown や context 差し替えで Space を飲み続けません。
- 予測一覧の履歴補完が、入力中の読みより長いとき、Space はその履歴の読みで変換します（`に` の履歴が `日本語` なら `にほんご` を変換する）。
- 郵便番号や都道府県付きフル住所の地名層を辞書から外し、`来て` / `書いて` / `行って` のような融合活用をビルド時に足します。
- 独立 TLA+ モデル `DualTsfPhysicalKeyArbitration` で、二値所有の反例と三値の safety を TLC 2.19 で確認しています。

## 同梱辞書

- `system.dic` 81,666,712 bytes
- SHA-256 `f09f8bf4ebf6e21d170123672ddbb8c7a5f450571807a3ba938e42497c723b80`

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
