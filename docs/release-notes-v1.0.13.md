# Sakura Input 1.0.13

予測候補が出ているときの Space が変換候補を出さず、読み（「よそく」など）を確定してしまう不具合を直します。

## 変更

- 予測候補の表示中も Space / `変換` は辞書変換を開始します。Chromium が下線付きの読みを確定しません。
- 予測ポップアップの layout、またはキュー済み候補が持っている live な文書へ変換結果を書き込みます。
- 予測が一覧表示のときも、Tab でフォーカスしているときも、Space は読みの確定ではなく変換候補を出します。
- TLC で、予測表示中の Space がホスト確定に到達しないことと、Predicting の Space が Convert であることを確認しました。

## 同梱辞書

- `system.dic` 94,493,048 bytes
- SHA-256 `f0741b6c7426cc4217e56419a79733a2336fa9e994b60cd264eeb5ec9291b9cb`

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
