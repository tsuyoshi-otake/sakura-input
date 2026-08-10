# Sakura Input

Sakura Input は、Windows 11 x64 向けの日本語入力システムです。Microsoft IME に近い既定キー操作を保ちながら、IT 用語辞書、文脈変換、予測入力、学習、アプリ別設定、MS-IME／ATOK／Mozc 形式のユーザー辞書入出力を備えます。

> 対応環境は Windows 11（build 22000 以降）、x64、AVX + SSSE3 対応 CPU です。32 bit ホスト用 DLL と ARM64 ネイティブ版は提供しません。

## インストール

1. GitHub Releases の `sakura_setup.exe` を取得します。
2. ファイルのプロパティにある「デジタル署名」、または PowerShell の `Get-AuthenticodeSignature .\sakura_setup.exe` で署名が有効であることを確認します。署名がない、無効、または発行元が想定と異なるファイルは実行しないでください。
3. インストーラーを実行します。管理者権限はマシン全体の TSF 登録に使われ、入力方式の有効化はサインイン中のユーザーとして実行されます。
4. Windows の入力方式切り替え（`Win`+`Space`）から「Sakura Input」を選びます。反映されない場合は一度サインアウトして再度サインインしてください。

アップグレード時の TSF DLL、エンジン、renderer、設定 payload、辞書は、`Program Files\Sakura Input\versions\<version>-<build-id>` に新しい世代としてコピーされます。コピー完了後に COM 登録の参照先だけを切り替えるため、ホストアプリが旧 DLL を読み込んでいても通常更新に Windows の再起動は不要です。使用中の旧世代はロックが解けるまで残りますが、登録解除済みです。管理者権限の隠しメンテナンスタスクがログオンごとに旧世代の削除を再試行するため、UAC を毎回表示せずに後片付けできます。

タスクバーの入力モード表示は、現在のモードに応じて `あ`／`ア`／`ｱ`／`Ａ`／`A`を表示します。直接入力は半角英数と区別できる斜線付き`A`です。文字はタスクバーのテーマとDPIに合わせた透過表示で、右クリックすると入力モードの変更や「Sakura Input の設定」を開くメニューが表示されます。

## 基本操作

既定の `ms-ime` プリセットでは、主な操作は次のとおりです。

| 操作 | キー |
|---|---|
| IME のオン／オフ | `半角/全角` または `Alt`+`` ` `` |
| 変換開始／次候補 | `Space` または `変換` |
| 前候補 | `Shift`+`Space` |
| 確定 | `Enter` |
| 取消 | `Esc` |
| 予測候補の移動 | `Tab`／`Shift`+`Tab`、`↓`／`↑` |
| 無変換（未入力） | ひらがな → 全角カタカナ → 半角カタカナ → ひらがなの永続切替 |
| 無変換（入力中） | 入力済み文字列を全角カタカナへ一時変換。続けて押すと半角カタカナ |
| ひらがな／カタカナ／半角カナ | `F6`／`F7`／`F8` |
| 全角／半角英数 | `F9`／`F10` |
| 再変換 | 未入力状態で `変換` |
| 確定取消 | 確定直後に `Ctrl`+`Backspace` |

入力中の`Tab`は推測候補を選択し、`Enter`で確定します。先頭候補が入力済み文字列と同じ場合は、最初の`Tab`で見た目が変わらないことがあります。候補表示中は`1`～`9`で候補を直接選択でき、Microsoft IMEプリセットの変換中`Tab`は候補表を展開します。ATOKプリセットでは変換中の`Tab`／`Shift`+`Tab`が候補グループの次／前移動になります。入力中の`無変換`は現在の文字列だけを変換し、確定後に入力モードを変更しません。完全なキー一覧と設定・辞書・診断の説明は [日本語ユーザーガイド](docs/guide-ja.md) を参照してください。

ひらがなモードで最初の英字を`Shift`を押しながら入力すると、以降は`Shift`を離しても英語用のcompositionになります。`Space`で正規表記へ変換でき、たとえば`Shift`+`C`の後に`laude`と入力して`Claude`／`Claude Code`、`Shift`+`O`の後に`penai`と入力して`OpenAI`を選べます。辞書にない語はかなへ誤変換せず、入力した英字のまま`Enter`で確定できます。

## 設定

`sakura_settings.exe` を引数なしで起動すると設定画面が開きます。コマンドライン操作も提供しています。

```text
sakura_settings.exe config show
sakura_settings.exe config set keymap atok
sakura_settings.exe dictionary import user.txt auto merge
sakura_settings.exe diagnostics show text
sakura_settings.exe update status
```

自動更新は既定で無効です。設定画面または `sakura_settings.exe update enable` で明示的に有効化した場合だけ、設定アプリ起動時に更新を確認します。インストーラーは HTTPS で取得し、固定された配布元、サイズ、SHA-256、Authenticode 署名をすべて検証してから実行します。設定の root 実行ファイルは安定ランチャーで、実体は現在の versioned payload から起動します。

## 開発者モード（入力・変換履歴）

UI/UX開発や入力状態の再現に使う、明示的なオプトイン機能です。既定では無効で、次の設定を行った場合だけ履歴を保存します。

```text
sakura_settings.exe config set developer-mode on

sakura_settings.exe history show
sakura_settings.exe history export <file>
sakura_settings.exe history stats
sakura_settings.exe history clear

sakura_settings.exe config set developer-mode off
```

設定ファイルの値と、すでに起動中のengineで履歴サービスが動いているかは別の状態です。`config set developer-mode`は`active`、`restart-required-to-enable`、`restart-required-to-disable`、または`will-enable-at-next-engine-start`を表示します。設定変更が起動中のengineへ反映されていない場合は、次のengine起動から有効または無効になります。

履歴は `%LOCALAPPDATA%\SakuraInput\history\input.bin` に、現在のWindowsユーザー向けDPAPIで暗号化して保存されます。実際のキーコード、文字、修飾キー、リピート、入力状態・モードの変化、表示前後のpreedit、commit、削除、アクション、変換時のreading・surface・前後の文脈を記録します。`history export` は開発用のTSVへ出力します。

Password、URL、Email、Digitsの入力スコープ、未分類・未知のスコープ、テスト専用入力は保存しません。履歴は30日または64 MiBを超えないよう整理され、入力処理を止めない有界キューを使用します。`history stats`は、engine接続状態、履歴サービスの実稼働状態、キュー落ち、保存失敗、未分類・機密・テスト専用を理由に除外した件数を表示します。除外件数は内容を含まない実行中engine単位の集計で、engine再起動時にリセットされます。

## プライバシーと診断

変換中の文字列や辞書内容を診断ログへ記録・送信しません。`%LOCALAPPDATA%\SakuraInput` に、サイズ制限付きのライフサイクルログ、IPC タイムアウト統計、最大 5 個のローカル WER minidump を保持します。自動アップロードはありません。詳細と消去手順はユーザーガイドを参照してください。

### 任意のローカル長文再順位付け

Issue #24 の任意 neural reranker は、Rust worker、engine の非同期統合、固定artifact生成、実 ONNX Runtime/model IPC E2E、opt-in installer buildまで実装・確認済みです。既定installerはまだ同梱しないrollout境界であり、順位品質とcold/warm latency、private working setの受け入れ計測は未完了です。同梱した場合、長い読みの通常の `Space` 変換でのみ、既存の辞書 N-best 候補をローカルの DeBERTa V2 tiny Japanese モデルで補助的に再順位付けできます。これは候補生成器や `Tab` の推測候補ではなく、既存の最大 6 件の変換候補に対する任意の処理です。worker またはモデルが存在しない、起動・IPC・推論が失敗する、または結果が期限内に ready でないときは、入力を待たずに従来のローカル順位で変換します。

この機能は Rust 製の `sakura_neural_worker.exe` を `sakura_engine.exe` と同じディレクトリから遅延起動し、`neural/deberta-v2-tiny-japanese-char-wwm/` の `model.onnx`、`vocab.txt`、および `manifest.json` を使います。worker は同じディレクトリの ONNX Runtime DLL を動的に読み込みます。TSF DLL と engine 本体に ML runtime やモデルを読み込ませません。worker との通信は同一マシン内の標準入出力 IPC だけで、クラウド送信、入力内容の診断ログへの記録、自動アップロードは行いません。

manifest は固定 model/revision、ONNX opset/runtime、Basic + character tokenizer（`do_lower_case=false`）、artifact のサイズと SHA-256 を Rust worker 起動時に厳密検証します。推論量は候補最大 6 件、sequence 最大 128 token、mask row 最大 48 に制限し、上限超過時は従来順位へ戻します。

engine は分類済みの `Normal` scope にある通常変換の読みをローカルで候補 snapshot にし、worker へは再順位付けに必要な候補 snapshot だけを渡します。Password、URL、Email、Digits、未知または未分類の scope、直接入力、`test_only` 入力、短い読み、候補が 1 件だけの場合は除外されます。候補 UI を表示した後には、遅い worker 結果で順位を並べ替えません。モデルの結果は session、composition generation、reading、candidate set が完全一致する場合だけ利用し、明示的な学習・完全一致 cache・ユーザー辞書の優先順位を上書きしません。

`15 MiB` の private working-set 予算は TSF/engine 本体の予算です。任意 worker のプロセス working setとcold/warm latencyはこの予算に含めず、受け入れ計測も未完了です。2026-08-10のx64 release artifactはworker 0.39 MiB、ONNX Runtime DLL 15.08 MiB、model 40.37 MiB、neural同梱installer 55.45 MiBでした（toolchain/buildごとに再計測が必要です）。配布 artifact の再生成には [scripts/export-neural-model.py](scripts/export-neural-model.py) と [scripts/build-neural-reranker.ps1](scripts/build-neural-reranker.ps1) を使用し、固定 revision と生成 manifest の SHA-256 記録を検証してください。

## アンインストール

Windows の「インストールされているアプリ」からアンインストールします。ユーザー辞書、学習、設定、診断は既定で保持されます。すべて削除する場合は、アンインストーラーを `/PURGE=1` 付きで明示的に実行してください。TSF 登録解除に失敗した場合、入力機能を壊さないためアンインストールはファイル削除前に停止します。

## ライセンス

プログラム本体は [MIT License](LICENSE) です。辞書由来データと任意のニューラル再順位付け artifact のライセンス・出典は [Third-party notices](THIRD_PARTY_NOTICES.md)、[Mozc 辞書 notice](THIRD_PARTY_LICENSES/mozc-dictionary.txt)、[smile-chat public glossary notice](THIRD_PARTY_LICENSES/smile-chat-public-MIT.txt)、[Kyoto University NLP model notice](THIRD_PARTY_LICENSES/ku-nlp-deberta-v2-tiny-japanese-char-wwm.txt) に記載しています。変換済みの ONNX model artifact は CC BY-SA 4.0 の条件に従います。該当 artifact と notice はインストーラーにも同梱されます。

## 開発者向け

設計上の制約は [DESIGN.md](DESIGN.md)、フェーズと合格基準は [PLAN.md](PLAN.md)、別セッションへの作業引き継ぎは [CLAUDE.md](CLAUDE.md) を参照してください。通常の検証は `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` です。全フェーズの厳格判定は `scripts/verify-all-phases.ps1`、個別判定は `scripts/verify-phase0.ps1`～`verify-phase5.ps1` を使います。手動・dogfood・互換性・段階更新の記録例は `scripts/templates/` にあり、テンプレートをコピーしただけでは合格にならず、担当者・日時・実ファイルの SHA-256 が検証されます。

辞書は `scripts/build-dictionary.ps1` が pinned source から決定論的に生成し、`.dic` はリポジトリへコミットしません。リリース用の本体辞書を生成するときは、14カテゴリの入力ディレクトリを `-SystemCategoryDirectory` で指定します。`build-installer.ps1` は14カテゴリのmanifestがない辞書を拒否します。`-EngineeringOnly` はローカル実装の反復用であり、CI、実ホスト、経過日数、72時間 fuzz、実署名、公開済み Release の代替にはなりません。
