# Sakura Input 1.0.14

予測候補の表示中に Space を押すと、読みは変換されるのに変換候補の一覧が出ない不具合を直します。

## 変更

- 予測リストが出ているときの Space / `変換` は、変換を compact（選択中の1行）にせず、変換候補のページをそのまま表示します。
- 予測がない通常の変換は、これまでどおり compact から始まります。Tab で展開、変換中の Space で次候補です。
- 1.0.13 の「読みを確定しない」修正はそのままです。

## 同梱辞書

- `system.dic` 94,493,048 bytes
- SHA-256 `f0741b6c7426cc4217e56419a79733a2336fa9e994b60cd264eeb5ec9291b9cb`

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
