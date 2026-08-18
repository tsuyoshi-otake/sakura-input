# Sakura Input 1.0.12

Cursor / Chromium で変換中の Space が読み末尾の空白になる不具合を直します（Issue #68）。

## 変更

- 入力中の Space / `変換` は、エンジン Probe や辞書変換の前に IME がキーを所有します。Probe が 50ms を超えても Chromium が空白を挿入しません。
- Probe は辞書変換を走らせません。Probe の timeout で live session を捨てません。
- Electron への Space 二重配送を fence し、別 `ITfContext` でも live composition を維持します。
- Cursor で「にほんごにゅうりょくのてすと」＋ Space が変換されることを再インストール後に確認しました。

## 同梱辞書

- `system.dic` 94,493,048 bytes
- SHA-256 `f0741b6c7426cc4217e56419a79733a2336fa9e994b60cd264eeb5ec9291b9cb`

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
