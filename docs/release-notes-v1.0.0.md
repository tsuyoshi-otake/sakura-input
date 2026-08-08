# Sakura Input 1.0.0

Sakura Input の最初の安定版です。

## 主な機能

- Windows 11 x64 向け TSF 日本語入力
- Microsoft IME／ATOK キープリセットとローマ字テーブル
- pinned Mozc 辞書と IT 用語 overlay による変換・予測
- 文節変換、再変換、確定取消、アプリ別プロファイル
- ユーザー辞書の Sakura／MS-IME／ATOK／Mozc 入出力
- bounded learning、診断、WER dump、ログオン自己修復
- 明示的 opt-in の署名・hash 検証付き updater

## 対応環境

Windows 11 build 22000 以降、x64、AVX 対応 CPU。32 bit ホスト用 DLL と ARM64 ネイティブ版は含みません。

## インストール前の確認

`sakura_setup.exe` の Authenticode 署名が有効で、想定した発行元であることを確認してください。アップグレードは versioned payload の side-by-side 切替で行われるため、通常の終了コードは `0` で、TSF DLL が使用中でも Windows の再起動は要求されません。ロックされた旧世代は、SYSTEM のログオン時メンテナンスタスクが後で削除を再試行します。

詳細は同梱の `README-ja.md` と `guide-ja.md` を参照してください。
