# Sakura Input 1.0.11

候補の横に出ていた `[calibration]` や `[company]` のような開発用タグを止めます。変換順位は 1.0.10 のままです。

## 変更

- 変換優先 overlay と curated-terms の注記列を空にしました。この列は候補注記としてユーザーに見えるため、開発用メモは `#` コメントへ移します。
- 手編集 TSV が `[` で始まる注記を持つと辞書コンパイルを拒否します。
- 詳細へ移したあと残った一覧注記は画像へ入れません。カテゴリ辞書に焼き付いていたタグも落ちます。

## 同梱辞書

- `system.dic` 94,493,048 bytes
- SHA-256 `f0741b6c7426cc4217e56419a79733a2336fa9e994b60cd264eeb5ec9291b9cb`

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
