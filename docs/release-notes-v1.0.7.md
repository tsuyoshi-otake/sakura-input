# Sakura Input 1.0.7

1.0.6 以降の入力修正と、候補ポップアップが入力中の文字列を覆う問題への対応です。Sakura-Rerank-Tiny-v1 は 1.0.6 と同一です。同梱辞書は smile-chat glossary の MIT スナップショットをリポジトリへ固定したあとの成果物です。

## 変更

- 変換候補の辞書詳細が高いときに、不透明な候補ウィンドウ全体が入力中の文字列を覆う問題を修正しました（#47）。

  候補一覧はキャレットのすぐ近くに置けていても、右側の詳細ペインが一覧より高いと、HWND の外接矩形が一覧の下まで白く伸びて入力行に被さることがありました。詳細ペイン単体がキャレットを外していても、ウィンドウ全体が一覧と同じ隙間を保てない配置は捨て、入力行を残します。キャレット位置を上下 5px 刻み、さらに ±5px ずらした総当たりで確認しています。

- 開発者モードの入力履歴を、エンジン再起動なしで有効化できるようにしました（`84efb5d`）。
- smile-chat の MIT glossary スナップショットを `third_party/smile-chat-public` へ同梱し、インストーラーと release CI が private-repository token を要求しないようにしました（`a5f8ef4`）。

## 同梱辞書

- `system.dic` 94,761,496 bytes
- SHA-256 `c5be5a51d88c39f7f968081dc5c5853a7b22dd5ef8733d7fdae0585594489aaf`
- 1.0.6 の 39,349,040 bytes から更新しています。vendored glossary と curated overlay を含む通常の既定ビルドです。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
