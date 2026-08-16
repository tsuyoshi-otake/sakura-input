# Sakura Input 1.0.10

ATOK 風の入力支援（入力誤りの自動修復、英単語→カタカナ、文脈依存の記号置換）を入れ、レビューで見つかった SPELLING_CORRECTION ゲート漏れと commit ヒントの誤適用（Issue #63）を直したリリースです。

## 変更

- 入力支援を既定オンで追加しました。設定の「入力支援」から全体オフ、および誤り修復／英単語・記号置換の個別スイッチを変更できます。
- 機微スコープ（Password / URL / Email / Digits）と未分類入力では、規則修復・記号置換・SPELLING_CORRECTION を起動しません。
- 文節の区切り直し後は、同じ読みの repair を suppress します。
- Issue #63: conversion / prediction で SPELLING_CORRECTION 入場条件を統一し、候補列挙・予算計上の前に除外します。
- Issue #63: commit 履歴ヒントはクエリ全文 `[0, reading.len())` にだけ載せ、中間 start への貼り付けをやめました。
- Issue #63: 単一文節格子は実際に追加した exact 辺数で repair 枠を差し引きます。

## 同梱辞書

- `system.dic` 94,705,232 bytes
- SHA-256 `130bc2929b355bf38c3326ae30acd644cf58bec7c756de173174b8badc5b1efc`

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
