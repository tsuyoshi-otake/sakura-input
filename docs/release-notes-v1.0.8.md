# Sakura Input 1.0.8

`きのうしょうかい` が `昨日紹介` になる同音複合を直し、IT 複合語 `機能紹介` をトップ1にします。単独の `きのう` → `昨日` と、`昨日以外` のような日付複合は変えていません。

## 変更

- 変換優先 overlay に IT 複合語を登録し、既存の高いコストを下げました（#62）。

  - `きのうしょうかい` → `機能紹介`（glossary に無かったので新規）
  - `きのうようけん` → `機能要件`
  - `きのうこんぽーねんと` → `機能コンポーネント`

  `昨日` の word_cost 1100 は維持します。`昨日紹介` は 2 番目以降の候補として残します。

## 同梱辞書

- `system.dic` 94,702,656 bytes
- SHA-256 `1d8dada979963b363009b53beb778829db7e3fed708ebc06db1b368e8da2c0a0`
- 1.0.7 のカテゴリ辞書へ conversion-priorities overlay を載せ、同一入力の 2 回コンパイルで SHA-256 が一致することを確認しています。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
