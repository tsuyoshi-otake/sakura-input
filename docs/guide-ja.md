# Sakura Input ユーザーガイド

このガイドは Sakura Input 1.0 のインストール後の操作、設定、データ管理、更新、障害時の確認方法を説明します。

## 1. 対応範囲

- Windows 11 build 22000 以降
- x64 プロセッサ、AVX 対応 CPU
- x64 の TSF 対応デスクトップアプリ
- 日本語（ja-JP）言語プロファイル

32 bit ホスト、Windows 10、ARM64 ネイティブ実行はサポート対象外です。パスワード入力など、ホストが機密入力スコープを通知したフィールドでは予測・学習を行いません。

## 2. 入力と変換

### 2.1 入力モード

既定はひらがなです。`半角/全角`（US 配列は `Alt`+`` ` ``）で IME を切り替えます。`無変換` は、ひらがな・カタカナ系のモードを切り替えます。

Windows 11 では、Sakura Input が編集可能な入力欄のキャレットを受け取っている間だけ、タスクバーの入力モード表示に`あ`または`A`を表示します。フォーカスを外すと表示も消えるため、常駐する通知領域アイコンにはなりません。右クリックでは「入力モード」（ひらがな／カタカナ／英数／直接入力）、「変更前の入力モードに戻す」、「日本語入力をオン／オフ」を選べます。パスワードなどの機密入力欄、種類を判定できない欄、または変換中は、誤って文書を変更しないようメニューからの切り替えを無効にします。

### 2.2 変換と候補

ローマ字を入力し、`Space` または `変換` で変換を開始します。続けて押すと次候補、`Shift`+`Space` で前候補へ移動します。`PageDown`／`PageUp` は候補ページ、`1`～`9` は表示ページ内の候補を直接選択します。`Enter` で確定、`Esc` または候補表示中の `Backspace` で変換前の読みへ戻ります。

ひらがなモードで最初の英字を`Shift`を押しながら入力すると、かなへ変えず英語用のcompositionを開始します。以降は`Shift`を離しても、そのcompositionを確定または取消するまで英語入力として扱います。`Space`または`変換`で辞書の正規表記を選びます。例: `Shift`+`C`の後に`laude` → `Claude`／`Claude Code`、`Shift`+`O`の後に`penai` → `OpenAI`、`Shift`+`P`の後に`ytorch` → `PyTorch`。該当語がないときは変換せずビープし、`Enter`で入力した英字列をそのまま確定できます。この一時入力で、次の入力モードが英数へ変わることはありません。

文節は `←`／`→` で移動し、`Shift`+`←`／`Shift`+`→` で縮小・拡大します。

### 2.3 予測

入力中に候補が表示された場合、`Tab`／`Shift`+`Tab` または `↓`／`↑` で選び、`Enter` で確定します。候補へフォーカスせず最上位を確定するには `Shift`+`Enter` を使います。端末向けアプリプロファイルでは、シェル補完を妨げないよう予測受理キーを無効にできます。

### 2.3.1 用語説明（Issue #28）

出典のある説明を持つ選択候補では、候補ポップアップの一定幅の詳細ペインに説明全文を折り返して表示します。候補一覧には長い説明を重複表示せず、`履歴`などの短い状態注記だけを残します。説明は右側、左側、下側の順に配置を試み、モニターの作業領域に物理的に収まらない場合だけ末尾を省略します。候補に説明がない場合や、複合候補など辞書の正確な項目へ解決できない場合は表示しません。

辞書原文の長さは表示制限で切り詰めませんが、画面とrendererへの送信には切り詰め状態を明示したプレビューを使います。別名、関連語、類似語、反対語は、出典に明示されたものだけを最大3語ずつ示します。推測した関係語は表示しません。スクリーンリーダーには選択候補の説明を公開し、プレビューが省略されている場合はそのことを伝えます。固定したsmile-chatとJapanese WordNet 1.1由来の説明を収録し、曖昧な語義や未収録語では詳細自体を表示しません。

### 2.4 文字種変換

| 文字種 | キー | 代替キー |
|---|---|---|
| ひらがな | `F6` | `Ctrl`+`U` |
| カタカナ | `F7` | `Ctrl`+`I` |
| 半角カタカナ | `F8` | `Ctrl`+`O` |
| 全角英数 | `F9` | `Ctrl`+`P` |
| 半角英数 | `F10` | `Ctrl`+`T` |

### 2.5 再変換と確定取消

アプリで文字列を選択し、未入力状態で `変換` を押すと再変換を要求します。アプリ側が TSF 再変換を実装していない場合は利用できません。確定直後の `Ctrl`+`Backspace` は確定を取り消します。取消可能期間を過ぎた場合はアプリへ通常のキーとして渡されます。

## 3. 設定画面

インストール先の `sakura_settings.exe` を引数なしで起動します。

- 一般: `ms-ime`／`atok` キープリセット、予測の有効・無効、予測受理キー
- アプリ別: 実行ファイル名ごとの初期モード、予測、受理キー
- ユーザー辞書: 登録、編集、削除、インポート、エクスポート
- 学習・診断: 学習統計、エクスポート、消去、IPC 診断
- 更新: 明示的な opt-in と手動更新

設定は保存後に新しいセッションへ反映されます。壊れた設定は部分適用せず、最後に検証済みの設定または安全な既定値を使います。

## 4. コマンドライン設定

`sakura_settings.exe help` で完全な構文を表示します。主な例:

```text
sakura_settings.exe config show
sakura_settings.exe config set keymap ms-ime
sakura_settings.exe config set prediction on
sakura_settings.exe config set space-width same-as-input
sakura_settings.exe config set shift-space opposite
sakura_settings.exe profile set WindowsTerminal.exe hiragana off disabled
sakura_settings.exe dictionary add さくら "Sakura Input" proper-noun project
sakura_settings.exe dictionary import user.txt auto merge
sakura_settings.exe dictionary export backup.txt mozc
sakura_settings.exe learning export learning.tsv
sakura_settings.exe diagnostics show tsv
```

通常の空白幅は `same-as-input`、`full`、`half` から選べます。Shift+スペースは
`opposite`、`full`、`half` から選べます。いずれも変換中の Space 操作ではなく、
アイドル時に確定する空白文字だけを対象にします。

ユーザー辞書の自動判定は Sakura、MS-IME、ATOK、Mozc 形式を対象にします。置換インポートは既存辞書を入れ替えるため、先にエクスポートして退避してください。書き込みは一時ファイルを検証してから原子的に置換し、失敗時に半分だけ更新された状態を残しません。

## 5. データの場所とバックアップ

ユーザーデータは `%LOCALAPPDATA%\SakuraInput` 以下です。

| パス | 内容 |
|---|---|
| `config\config.toml` | 一般設定とアプリ別設定 |
| `userdict\user.tsv` | ユーザー辞書 |
| `learning\log.bin` | 学習ログ |
| `diagnostics\ipc-timeouts.bin` | 内容を含まない IPC タイムアウト統計 |
| `logs\engine.log`、`engine.log.1` | サイズ制限付きライフサイクルログ |
| `logs\logon.status` | サインイン時の自己修復結果 |
| `dumps\` | 最大 5 個のローカル WER minidump |
| `update\` | 更新 opt-in、検証中 installer、install log |

バックアップは設定画面または CLI の辞書・学習エクスポートを優先してください。エンジン実行中に `log.bin` を直接コピーするより、整合したスナップショットを取得できます。

## 6. 更新

自動更新は既定で無効です。有効化後も、バックグラウンド常駐サービスではなく設定アプリを開いた時だけ確認します。

```text
sakura_settings.exe update status
sakura_settings.exe update enable
sakura_settings.exe update apply
sakura_settings.exe update disable
```

更新処理は HTTPS 配布元を allowlist で制限し、manifest の厳密な形式、installer の宣言サイズ、SHA-256、Authenticode 署名を確認します。いずれかが一致しなければ installer を削除して実行しません。新しい runtime は `Program Files\Sakura Input\versions\<version>-<build-id>` に先にコピーされ、COM 登録を新しい DLL に切り替えてから旧世代を解放します。通常のサイレント更新の終了コードは `0` で、再起動は要求しません。旧 DLL が使用中なら、その versioned ディレクトリをログオン時の SYSTEM メンテナンスタスクが後で削除します。削除できない場合は次回ログオンへ残します。

## 7. 診断と復旧

### 入力方式が見つからない

一度サインアウトして再度サインインしてください。`sakura_logon.exe` はログオンタスクとユーザープロファイル登録を毎回確認し、Windows 機能更新で失われた場合に自己修復します。別の `Sakura Input Maintenance\Payload Cleanup` タスクが SYSTEM 権限で旧 payload の削除を再試行します。`%LOCALAPPDATA%\SakuraInput\logs\logon.status` の非ゼロ bit は、タスク、プロファイル、エンジン、renderer のどの終端が失敗したかを示します。

### 変換できない

アクティブな versioned ディレクトリ内の `dict\system.dic` が存在することを確認します。エンジンを再起動するにはサインアウト／サインインが最も安全です。開発用の `SAKURA_DICTIONARY` 環境変数が設定されていると、同梱辞書ではなくそのパスが優先されます。

### タイムアウトを調べる

`sakura_settings.exe diagnostics show text` を実行します。統計には入力文字列を含めません。消去は `diagnostics clear` です。`engine.log` も内容ではなく起動、停止、rotation、dump prune などのイベントだけを記録します。

### クラッシュ dump

WER minidump はローカルにだけ保存され、自動送信されません。メモリ内容を含み得るため、共有前に機密情報として扱ってください。保持数は 5 に制限され、エンジン起動時にも超過分を削除します。

## 8. アンインストール

通常のアンインストールは設定、辞書、学習、診断を保持します。完全削除が必要な場合のみ、管理者ターミナルからアンインストーラーへ `/PURGE=1` を渡します。登録解除に失敗した状態で DLL を削除すると Windows の入力機能を壊す可能性があるため、アンインストーラーはその場合に停止します。エラーの指示に従って登録解除を修復してから再実行してください。

## 9. ライセンス

インストール先の `docs` と `licenses` に、本体 MIT License、Third-party notices、Mozc 辞書の混合ライセンス notice、smile-chat public glossary の MIT notice を同梱します。

### 任意のローカル長文再順位付けとプライバシー

Issue #32 の実験的 neural reranker は、自作の `Sakura-Rerank-Tiny-v1` をRust workerでローカル実行します。旧DeBERTa Tinyのruntimeとinstaller経路は除去済みです。Sakuraモデル、worker、ONNX Runtimeは通常installerに同梱され、既定では長い読みを `Space` で通常変換するときだけ、既存の辞書N-best候補を最大6件までlistwiseに再順位付けします。これは候補生成や `Tab` の推測候補ではありません。engineは `sakura_neural_worker.exe` を必要なときだけローカル子プロセスとして起動し、`neural/sakura-rerank-tiny-v1/` の `model.onnx` と `manifest.json` を使用します。workerは同じディレクトリのONNX Runtime DLLを動的に読み込みます。TSF DLLやengine本体へONNX Runtimeやモデルを読み込みません。

この worker はネットワークへ送信しません。engine が分類済み `Normal` scope の通常変換の読みからローカルで作った候補 snapshot だけを、同一マシン内の IPC で渡します。Password、URL、Email、Digits、未知または未分類 scope、直接入力、`test_only` 入力、短い読み、候補が 1 件だけの場合は worker を起動せず、入力内容も送信しません。通常の診断ログへ入力文字列を記録せず、自動アップロードもしません。

変換のキー経路は worker の応答を待ちません。worker／モデルの欠落、起動失敗、異常応答、timeout、または一致しない古い結果では、従来のローカル順位のまま表示します。候補表示後に遅い結果で順序を変更することはありません。明示的な学習、完全一致 cache、ユーザー辞書の優先順位はモデルより上です。

`15 MiB` の private working-set 予算は TSF/engine 本体の予算であり、任意 worker のモデル、ONNX Runtime、working setは含みません。Sakuraモデルのworking setとcold/warm latencyは別途計測します。自作モデルはMITライセンスで配布承認済みですが、既存評価ではGate A未通過・final holdout未使用です。これは品質合格を示すものではありません。設定の neural reranker scope を `off` にすると無効化でき、`all-normal-conversions` にすると短い読みを含む通常変換へ広げられます。
