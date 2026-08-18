# Sakura Input 1.0.15

Cursor / Electron で予測中の Space のあと、変換候補はできるのに一覧が出ない不具合を直します（Issue #69）。

## 変更

- 予測表示中の Space / `変換` は読みを確定せず、辞書変換を開始します。Chromium が下線付きの読みを確定しません。
- 予測リストが出ているときの Space は、変換を compact（選択中の1行）にせず、変換候補のページを表示します。
- Cursor が同じ Space を別の入力欄にも送っても、表示中の候補ポップアップを消しません。
- 開発者履歴は package version と、インストール済みなら `versions/<version>-<build-id>` を記録します。
- Cursor で「にほんご」「へんかん」＋ Space の変換候補一覧が出ることを、1.0.15 再インストール後に確認しました。

## 同梱辞書

- `system.dic` 94,493,048 bytes
- SHA-256 `f0741b6c7426cc4217e56419a79733a2336fa9e994b60cd264eeb5ec9291b9cb`

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
