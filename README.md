# Sakura Input

Sakura Input は、コード、技術文書、Issue、レビュー、チャット、ターミナルを行き来するITエンジニアを主対象にした、Windows 11 x64向けの日本語入力システムです。

> コードと日本語の間を、自然につなぐIME。

製品方針は「ITエンジニア・ファースト」であり、「IT専用」ではありません。常用漢字や一般語を自然に入力できることを品質の土台にし、その上でIT・技術用語、製品名、略語、正確な英字の大文字小文字、英数字混在、バージョン、コード識別子、Markdown、技術文章を優先して最適化します。IT向け機能のために一般日本語の候補を失わせたり、パス、URL、コマンド、識別子を不用意に全角化・文章変換したりしないことを開発原則とします。

Microsoft IMEに近い既定キー操作を保ちながら、IT用語辞書、文脈変換、予測入力、学習、アプリ別設定、MS-IME／ATOK／Mozc形式のユーザー辞書入出力を備えます。一般日本語の基本品質とIT入力の表記精度を別々に評価し、片方の改善によるもう片方の回帰を許容しません。

> 対応環境は Windows 11（build 22000 以降）、x64、AVX + SSSE3 対応 CPU です。32 bit ホスト用 DLL と ARM64 ネイティブ版は提供しません。

## インストール

1. GitHub Releases の `sakura_setup.exe` を取得します。
2. ReleaseページのSHA-256と、PowerShellの`(Get-FileHash .\sakura_setup.exe -Algorithm SHA256).Hash`が一致することを確認します。
3. 署名済みリリースでは、ファイルのプロパティにある「デジタル署名」または`Get-AuthenticodeSignature .\sakura_setup.exe`でも署名が有効か確認します。owner承認の未署名リリースは、リリースノートにその旨を明記し、手動インストールだけを案内します。
4. インストーラーを実行します。管理者権限はマシン全体の TSF 登録に使われ、入力方式の有効化はサインイン中のユーザーとして実行されます。
5. Windows の入力方式切り替え（`Win`+`Space`）から「Sakura Input」を選びます。反映されない場合は一度サインアウトして再度サインインしてください。

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

入力中の`Tab`は推測候補を選択し、`Enter`で確定します。先頭に入力済み文字列と同じ候補がある場合、最初の`Tab`はそれを飛ばして最初の見た目が変わる候補へ進みます。候補表示中は候補行のクリックまたは`1`～`9`で直接確定でき、Microsoft IMEプリセットの変換中`Tab`は候補表を展開します。ATOKプリセットでは変換中の`Tab`／`Shift`+`Tab`が候補グループの次／前移動になります。入力中の`無変換`は現在の文字列だけを変換し、確定後に入力モードを変更しません。完全なキー一覧と設定・辞書・診断の説明は [日本語ユーザーガイド](docs/guide-ja.md) を参照してください。

### 候補ウィンドウ（Issue #27、自動検証・通常light実画面確認済み）

候補ウィンドウは、キャレットのそばに出る控えめなSakura独自の表示です。通常表示は候補番号、候補文字列、`履歴`などの短い状態注記を整然と並べ、候補文字列を最も読み取りやすい要素にします。辞書の長い説明は一覧へ重複表示せず、選択候補の詳細ペインだけに表示します。選択中の候補だけを淡い桜色の細いレールで示し、注記、候補種別、ページ番号は補助情報として静かに表示します。変換中の展開表示と入力中のコンパクト表示は、既存の候補・選択・ページ操作をそのまま表すだけで、候補の意味やキー操作を変えません。

配色は light／dark で低コントラストの暖色系ニュートラルを使いますが、Windowsのハイコントラストではシステムの色とコントラストを優先します。候補ウィンドウは入力フォーカスを奪いません。候補行の左クリックだけを確定操作として受け付け、行外と詳細ペインのクリックはホストへ通します。クリックは表示revision、候補index、所有session、入力欄とフォーカスを再検証してからTSFのedit sessionで適用するため、古い候補や別の入力欄へは反映しません。スクリーンリーダーにはUI Automationで候補を公開し、独自ウィンドウを表示できない環境ではTSFの候補データ経路を利用します。表示はrendererの通常のWin32ポップアップをGDIで描く実装で、候補用の画像assetを再生成したり、入力モード表示用assetを流用したりしません。Issue #27の検証記録では、mode-indicator assetに差分がないことも確認済みです。レイアウト、DPI、フォーカス維持、UI Automationの自動検証に加え、最新版再インストール後の通常light実画面で、候補本文、右側annotation列、淡い選択面と桜色rail、予測footer、`1–9/9`ページ表示を確認済みです。dark／Windowsハイコントラストの実画面確認だけが残っています。

### 候補の用語説明（Issue #28）

選択中の候補に、出典のある辞書説明があるときだけ、同じSakura候補ポップアップ内に説明を補助表示します。説明は一定幅の詳細ペイン内で全文を折り返し、モニターの作業領域に物理的に収まらない場合だけ末尾を省略します。候補一覧の基準矩形と列位置は選択移動で変えません。画面端では候補の右側、左側、下側の順に配置を試み、収まらなければ表示しません。説明がない候補、複合候補、または辞書の厳密なentry ordinalへ解決できない候補は、推測せず説明なしとして扱います。

辞書は原文を長さ上限なしで保持しますが、rendererへ渡すのは明示的に切り詰め状態を伴う安全なプレビューだけです。別名、関連語、類似語、反対語はmanifestに固定された明示データだけを表示し、実行時に表記や意味から推測しません。UI Automationにも選択候補の説明を公開し、プレビューである場合はその状態を伝えます。固定したsmile-chatとJapanese WordNet 1.1のfull-source構成では36,606件を再現できます。通常の既定ビルドでは、Sakuraのcurated sourceとIssue #30の審査済み000010 releaseを加え、472,825 entries中29,229 exact-entry detailsを2-passで確認しました。000010は242語を審査対象とし、236語を承認（246 exact-entry details）、6語を保留しました。承認レコードは全件に関連語があり、類似語43件・反対語16件も明示的に保持します。説明を持たないentry、曖昧な語義、未審査draftには詳細を表示しません。

ひらがなモードで最初の英字を`Shift`を押しながら入力すると、以降は`Shift`を離しても英語用のcompositionになります。英語用composition中の`Space`は半角の単語区切りを入力し、`変換`で辞書の正規表記を選べます。たとえば`Shift`+`C`の後に`laude`と入力して`変換`を押すと`Claude`／`Claude Code`、`Shift`+`O`の後に`penai`と入力すると`OpenAI`を選べます。辞書にない語はかなへ誤変換せず、入力した英字のまま`Enter`で確定できます。

## 設定

`sakura_settings.exe` を引数なしで起動すると設定画面が開きます。コマンドライン操作も提供しています。

```text
sakura_settings.exe config show
sakura_settings.exe config set keymap atok
sakura_settings.exe config set space-width half
sakura_settings.exe config set shift-space opposite
sakura_settings.exe dictionary import user.txt auto merge
sakura_settings.exe diagnostics show text
sakura_settings.exe update status
```

`space-width` は通常の空白キー（入力文字種と同じ／常に全角／常に半角）、
`shift-space` は Shift+スペース（スペースの逆／常に全角／常に半角）を設定します。
変換中の Space は候補・文節操作を優先し、これらのアイドル時空白設定で上書きしません。

### GPT-5.6 Lunaによる文章変換・校正

設定画面の「AI文章変換」で、送信先、Endpoint、認証方式、APIキー、変換スタイル、Effort、Tier、文章変換キーを設定できます。使用モデルは`gpt-5.6-luna`固定、API方式はResponsesのみです。文章変換キーの既定値はSpace右側の`変換`で、`Caps Lock`または無効にも変更できます。入力中はSakuraのpreeditを、入力がなければホストアプリの選択文字列を変換します。対象文字列がない場合は、`変換`の再変換や`Caps Lock`の入力モード切替といった従来動作を保ちます。校正はタスクバーのSakura Inputメニューにある「選択中文字列を校正」から明示的に実行します。

変換スタイルは、話し言葉、丁寧語、ビジネス、公文書、技術文書、論文、契約、小説、SNS、英語から選択できます。英語は、日本語などの入力を自然な英語へ翻訳し、すでに英語の入力は意味を保ったまま自然な英語へ整えます。OpenAI、Azure OpenAI、AWS Bedrock、Cloudflare、Customの各プリセットでは、指定したResponses互換Endpointへ接続します。APIキーは平文設定ではなく、現在のWindowsユーザーのCredential Managerへ保存します。インストール完了時にも設定画面を開けますが、保存時に疎通確認は行わず、APIキー欄が空なら既存のキーを上書きしません。

利用可能なCodex CLIが見つかった場合だけ「ChatGPT Subscription（Codex CLI）」も候補に表示します。この方式は別途インストール・ログイン済みのCodex CLIが必要で、APIキーは使いません。対象文字列は標準入力だけで渡し、コマンドライン引数、環境変数、ファイル名には含めません。CLI未検出、未ログイン、モデル利用不可、APIエラー、タイムアウトなどは別モデルへ切り替えず、タスクバーのメニューとツールチップに直近のエラーを表示します。

AI処理は明示操作時だけ開始し、同時に1件までです。キーの押しっぱなし、連打、同一内容の短時間再送では重複リクエストを作りません。結果待ちの間にフォーカス、選択範囲、元文字列、入力スコープが変わった場合は結果を適用しません。Password、URL、Email、Digits、未知・未分類の入力欄とテスト専用入力では送信しません。開発者モードでは、暗号化された入力履歴へ結果、状態、プロバイダー、スタイル、遅延、試行回数、取得できたトークン数を記録し、`history stats`でAIリクエスト回数とトークン合計を確認できます。

自動更新の確認は既定で有効です。設定画面または `sakura_settings.exe update disable` で明示的に無効化できます。無効化していない場合、設定アプリ起動時にGitHub Releasesの更新を確認します。利用可能な更新があれば確認ダイアログを表示し、同意した場合だけインストーラーを取得・検証・実行します。更新チャンネルは、Authenticode と Sakura 固有の detached application signature を別々に検証します。Authenticode 署名済みリリースは従来どおり `WinVerifyTrust` を通過する必要があります。owner 承認の Authenticode 未署名リリースでも、canonical `release-manifest-v2.txt` と `release-manifest-v2.sig` が Sakura の固定公開鍵で検証でき、`WinVerifyTrust` が正確に `TRUST_E_NOSIGNATURE` を返す場合だけ自動更新できます。公開鍵、trust epoch、release sequence、鍵の rotation／recovery、Authenticode 判定表は [update-signing v2 contract](verification/update-signing-v2.md) に固定しています。v1.0.33 は旧 updater からの手動 bridge であり、v2 対応 updater の導入後に自動更新を開始します。インストーラーは HTTPS で取得し、固定された配布元、サイズ、SHA-256、署名ポリシーをすべて検証してから実行します。設定の root 実行ファイルは安定ランチャーで、実体は現在の versioned payload から起動します。

## Sakura Pad（ローカルメモ）

Sakura Pad は、入力中の思いつきや貼り付けた断片を置いておくための、Sakura Input 内蔵のメモ帳です。既定では無効で、設定画面の「Sakura Pad ショートカット」または次のコマンドで明示的に有効化した場合だけ、`Ctrl` の2回叩き（左右どちらか同じ側・同じデバイスで、それぞれ 500 ms 以内）で開きます。

```text
sakura_settings.exe config set pad-shortcut double-ctrl
sakura_settings.exe config set pad-shortcut disabled
```

ウィンドウはメモ一覧と編集面の2ペインで、クライアント幅 520 logical px を境に形が変わります。520 px 以上では左に一覧・右に編集面を並べ、520 px 未満では一覧と編集面を `≡` で切り替える単一ペインになります。一覧には検索と並べ替え、下部バーには新規作成、並べ替え、同期、コピー、削除があります。編集の停止から少ししてから自動保存し、保存状態は編集面の見出し行に表示します。一覧と編集面のスクロールバーは、それぞれのペインの地色に合わせた Sakura 独自の細いレールで、ホイールとドラッグの両方で操作できます。

メモは `%LOCALAPPDATA%\SakuraInput\pad\memo.bin` に、現在の Windows ユーザー向け DPAPI で暗号化して保存します。最大 200 件、タイトル 256 UTF-16 単位、本文 65,536 UTF-16 単位です。保存は一時ファイルへ書き出してから公開し、検証済みバックアップを1世代保持します。読めない既存データは上書きせず、偽の「保存済み」を表示しません。

Pad の内容は IME の入力履歴、学習、ユーザー辞書、AI 文章変換のいずれにも渡しません。`Ctrl` 2回叩きの検出に使う Raw Input は、キーの文字やスキャンコードを保持せず、ジェスチャー判定に必要な最小限のイベントだけに落としてから状態機械へ渡します。

**GitHub 同期はこのリリースには含みません。** 下部バーの同期ボタンは「GitHub 未設定」と表示するだけで、通信は行いません。メモは端末内にとどまります。

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

履歴は `%LOCALAPPDATA%\SakuraInput\history\input.bin` に、現在のWindowsユーザー向けDPAPIで暗号化して保存されます。実際のキーコード、文字、修飾キー、リピート、入力状態・モードの変化、表示前後のpreedit、commit、削除、アクション、変換時のreading・surface・前後の文脈を記録します。履歴サービス起動時に package version と（インストール済みなら）`versions/<version>-<build-id>` の release label も記録するので、`history show` / `history export` / `history stats` でどのビルドのログか分かります。`history export` は開発用のTSVへ出力します。

Password、URL、Email、Digitsの入力スコープ、未分類・未知のスコープ、テスト専用入力は保存しません。履歴は30日または64 MiBを超えないよう整理され、入力処理を止めない有界キューを使用します。`history stats`は、engine接続状態、履歴サービスの実稼働状態、履歴ファイルに残っている package version / release label、キュー落ち、保存失敗、未分類・機密・テスト専用を理由に除外した件数を表示します。除外件数は内容を含まない実行中engine単位の集計で、engine再起動時にリセットされます。

## プライバシーと診断

変換中の文字列や辞書内容を診断ログへ記録・送信しません。`%LOCALAPPDATA%\SakuraInput` に、サイズ制限付きのライフサイクルログ、IPC タイムアウト統計、最大 5 個のローカル WER minidump を保持します。自動アップロードはありません。詳細と消去手順はユーザーガイドを参照してください。

### 任意のローカル長文再順位付け

Issue #32 の実験的 neural reranker は、自作の `Sakura-Rerank-Tiny-v1` を Rust worker からローカル実行します。旧 DeBERTa Tiny の実装と配布経路は除去済みです。Sakuraモデル、worker、ONNX Runtimeは通常installerに同梱し、既定では長い読みの通常変換だけに適用します。候補生成器や `Tab` の推測候補ではなく、既存の最大6件の辞書N-best候補だけをlistwiseに再順位付けします。workerまたはモデルが存在しない、manifest不一致、起動・IPC・推論が失敗する、または結果が期限内にreadyでないときは、入力を待たずに従来のローカル順位で変換します。

この機能は Rust 製の `sakura_neural_worker.exe` を `sakura_engine.exe` と同じディレクトリから遅延起動し、`neural/sakura-rerank-tiny-v1/` の `model.onnx` と `manifest.json` を使います。workerは同じディレクトリのONNX Runtime DLLを動的に読み込みます。TSF DLLとengine本体にML runtimeやモデルを読み込ませません。workerとの通信は同一マシン内の標準入出力IPCだけで、クラウド送信、入力内容の診断ログへの記録、自動アップロードは行いません。

manifestは固定モデル名、contract version、ONNX opset/runtime、研究manifestのSHA-256、Gate A失敗・final holdout未使用、MITライセンスと配布承認、artifactのサイズとSHA-256をworker起動時に厳密検証します。入力はprotocol v1で既に渡していた候補表記とlocal costだけです。最大6候補、表記32 Unicode scalar、固定feature 6次元に制限し、上限超過時は従来順位へ戻します。

engine は分類済みの `Normal` scope にある通常変換の読みをローカルで候補 snapshot にし、worker へは再順位付けに必要な候補 snapshot だけを渡します。Password、URL、Email、Digits、未知または未分類の scope、直接入力、`test_only` 入力、短い読み、候補が 1 件だけの場合は除外されます。候補 UI を表示した後には、遅い worker 結果で順位を並べ替えません。モデルの結果は session、composition generation、reading、candidate set が完全一致する場合だけ利用し、明示的な学習・完全一致 cache・ユーザー辞書の優先順位を上書きしません。

`15 MiB`のprivate working-set予算はTSF/engine本体の予算です。任意workerのprocess working setとcold/warm latencyはこの予算に含めず、Sakuraモデルについて別途計測します。モデルは自作物としてMITライセンスで配布承認済みです。一方、既存の評価記録ではGate A未通過で、final holdoutも未使用のため、品質合格を示すものではありません。リリース用payloadは `scripts/build-sakura-reranker.ps1` がモデル、研究manifest、worker、ONNX Runtimeを固定hashで検証して生成します。

## アンインストール

Windows の「インストールされているアプリ」からアンインストールします。ユーザー辞書、学習、設定、診断は既定で保持されます。すべて削除する場合は、アンインストーラーを `/PURGE=1` 付きで明示的に実行してください。TSF 登録解除に失敗した場合、入力機能を壊さないためアンインストールはファイル削除前に停止します。

## ライセンス

プログラム本体と自作の `Sakura-Rerank-Tiny-v1` は [MIT License](LICENSE) です。配布する辞書由来データとONNX Runtimeのライセンス・出典は [Third-party notices](THIRD_PARTY_NOTICES.md)、[Mozc 辞書 notice](THIRD_PARTY_LICENSES/mozc-dictionary.txt)、[smile-chat public glossary notice](THIRD_PARTY_LICENSES/smile-chat-public-MIT.txt) に記載しています。ONNX Runtimeの原文ライセンスとthird-party noticesは、固定した公式archiveからinstallerの `licenses` directoryへコピーします。

## 開発者向け

設計上の制約は [DESIGN.md](DESIGN.md)、フェーズと合格基準は [PLAN.md](PLAN.md)、別セッションへの作業引き継ぎは [CLAUDE.md](CLAUDE.md) を参照してください。通常の検証は `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`./ci/run-test-quiet.ps1 -Name 'workspace tests' -Command { cargo test --workspace }` です。テストは成功時にPASS 1行だけを表示し、失敗時は保存していた通常ログを全量表示します。全フェーズの厳格判定は `scripts/verify-all-phases.ps1`、個別判定は `scripts/verify-phase0.ps1`～`verify-phase5.ps1` を使います。手動・dogfood・互換性・段階更新の記録例は `scripts/templates/` にあり、テンプレートをコピーしただけでは合格にならず、担当者・日時・実ファイルの SHA-256 が検証されます。

辞書は `scripts/build-dictionary.ps1` が pinned source とSakuraのcurated layerから14カテゴリを決定論的に生成し、`.dic` はリポジトリへコミットしません。外部のカテゴリ辞書を追加する場合だけ `-SystemCategoryDirectory` を指定し、そのmanifestとライセンス宣言を厳格に検証します。`build-installer.ps1` は生成レポートに正規14カテゴリが完全・重複なしで記録されていない辞書を拒否します。`-EngineeringOnly` はローカル実装の反復用であり、CI、実ホスト、経過日数、72時間 fuzz、実署名、公開済み Release の代替にはなりません。
