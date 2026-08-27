# Sakura Input 1.0.31

AI文章変換の変換スタイルに「英語」を追加します。日本語などの入力を自然な英語へ翻訳し、すでに英語の入力は意味、事実、固有名詞、コード、数値、改行、意図を保ったまま自然な英語へ整えます（#58）。

## 主な変更

### 変換スタイル「英語」

設定画面の「入力・変換」→「AI文章変換」→「変換スタイル」で「英語」を選べるようになりました。文章変換キーを押すと、Sakura Inputの入力中文字列を優先し、入力中でなければホストアプリの選択文字列を英語へ変換します。対象がなければ既存のキー動作へ戻ります。

OpenAI、Azure OpenAI、AWS Bedrock、Cloudflare、CustomのResponses互換Endpointと、ChatGPT Subscription（Codex CLI）の両経路に同じ英語変換の意味を渡します。Codex CLI経路では、従来の日本語出力指定が英語スタイルと競合しないよう、英語変換時だけ英語出力を明示します。

校正は変換スタイルを適用しません。選択中文字列の言語を保ったまま、綴り、文法、助詞、句読点、明らかな誤字を修正します。

## 互換性と安全性

- 既存9スタイルのRegistry値とworker wire値は変更していません。「英語」はそれぞれ末尾の新しい値として追加しました。
- AI文章変換は従来どおり明示操作時だけ起動します。Password、URL、Email、Digits、未知・未分類の入力欄とテスト専用入力は送信しません。
- 使用モデルは`gpt-5.6-luna`固定、API方式はResponsesのみです。別モデルや別APIへfallbackしません。
- APIキーはWindows Credential Managerだけに保存し、対象文字列やキーをargv、環境変数、ログへ入れない境界を維持します。
- 通常の日本語変換、候補、キー操作、辞書には変更ありません。

## 検証

- workspace全体のformat、Clippy（`-D warnings`）、`cargo test --workspace`に成功しました。
- Registry値、worker wire値、engine mapping、設定画面の表示順を全列挙で検証しました。
- OpenAI ResponsesとCodex CLIの両経路で、英語変換指示と出力言語制約を回帰テストしました。
- 校正が変換スタイルに関係なく元の言語を維持することを回帰テストしました。
- 実providerへの文章送信はリリース検証では行っていません。通信要求はfake APIと構築済みpromptの検査で確認しています。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このReleaseはowner承認済みのAuthenticode未署名版です。GitHub Releaseの`sakura_setup.exe`を取得し、同じReleaseにある`release-manifest.txt`のSHA-256とPowerShellの`(Get-FileHash .\sakura_setup.exe -Algorithm SHA256).Hash`が一致することを確認してから手動で実行してください。

未署名installerは自動更新のAuthenticode検証を通らないため、自動取得・実行されません。updaterのfail-closed動作は維持されます。

アップグレードはversioned payloadのside-by-side切替で行われます。新しいpayloadの検証に失敗した場合は、TSF登録を新しい版へ切り替えずに中止します。
