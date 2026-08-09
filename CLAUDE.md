# Sakura Input 引き継ぎ指示書

このファイルは、別セッションでSakura Inputの作業を再開するAI／開発者向けの引き継ぎメモです。まずこのファイルと`README.md`を読み、必要に応じて`DESIGN.md`、`PLAN.md`、ユーザーが提示した`AGENTS.md`相当の指示を確認してください。

## 最優先タスク：VS Codeで文字入力中に落ちる問題（調査未完了）

### ユーザー依頼と必須条件

- VS CodeでSakura Inputを使って文字入力している最中、まれにVS Codeごと落ちる問題の安全性を改善する。
- この調査・実装では、グローバル導入済みの`$sol-advisor:orchestration`を必ず使用し、エージェントを起動する前にプラグインの役割プリフライトを実行する。
- 主セッションはGPT-5.6 Solを使う。reasoning effortの`max`は必須条件にせず、Sol Advisor 0.2.1の要件に従う。
- 独立レビューはSol Advisor 0.2.1既定の`sol_advisor_sol_reviewer`（GPT-5.6 Sol／high／requested read-only）を使う。per-spawnのmodel／reasoning overrideは行わない。
- 2026-08-02のこの調査では、まだコード変更、再現試験、修正後テストを実施していない。下記は静的調査から得た仮説であり、クラッシュ原因として確定していない。

### Sol Advisorプリフライトと実行状況

現在使用する0.2.1について、完全に読んだ指示ファイルは次の2つ。

- `C:\Users\developer\.codex\plugins\cache\sol-advisor\sol-advisor\0.2.1\skills\orchestration\SKILL.md`
- `C:\Users\developer\.codex\plugins\cache\sol-advisor\sol-advisor\0.2.1\skills\orchestration\references\role-contracts.md`

Windowsでは素の`sh`がWSLへ入りWindowsパスを解決できなかった。役割exactnessチェックにはGit Bashを明示し、次のコマンドで3役すべて`exact`を確認した。

```powershell
rtk proxy powershell -NoProfile -Command "& 'C:\Program Files\Git\bin\bash.exe' -lc 'sh /c/Users/developer/.codex/plugins/cache/sol-advisor/sol-advisor/0.2.1/scripts/install-agents.sh --check'"
```

確認済みの役割は次のとおり。

- `sol_advisor_luna_implementer`
- `sol_advisor_terra_implementer`
- `sol_advisor_sol_reviewer`

コミットメント境界の相談としてSol reviewerを1回起動したが、回答前に引き継ぎ依頼へ切り替わったため停止した。thread IDは`019fc05d-4a91-7b42-ba24-89d8ecf5e7a7`で、再利用せず次セッションで新しい独立コンテキストを起動すること。レビュー判定（`PROCEED`／`CHANGE`／`STOP`）は得られていない。

そのreviewerは実行時検査で次を確認した。

- role：`sol_advisor_sol_reviewer`
- model：`gpt-5.6-sol`
- effort：`max`
- sandbox：`danger-full-access`
- permission profile：`disabled`

これは0.2.0を使った過去の実行時観測であり、今後のreviewerに`max`を要求する根拠にはしない。以後は0.2.1既定のSol／highを使用する。

つまりモデルとeffortはユーザー指定どおりだったが、read-onlyはプロンプト上の行動制約だけで、OS／ランタイムによる強制read-onlyではなかった。次回も起動直後に次の検査を行い、同じ結果なら「強制read-onlyではない」残余リスクを隠さないこと。起動前後で差分を記録し、レビュー後に書き込みがないことも検証する。

```powershell
rtk proxy powershell -NoProfile -Command "& 'C:\Program Files\Git\bin\bash.exe' -lc 'sh /c/Users/developer/.codex/plugins/cache/sol-advisor/sol-advisor/0.2.1/scripts/inspect-agent-runtime.sh <thread-id>'"
```

### 現時点の技術的な観察（未確定）

主に次を読んだ。

- `crates/sakura-tsf/src/text_service.rs`
- `crates/sakura-tsf/src/edit_session.rs`
- `crates/sakura-tsf/src/composition.rs`
- `crates/sakura-tsf/src/candidate_ui.rs`
- `crates/sakura-tsf/src/engine.rs`
- `crates/sakura-tsf/Cargo.toml`

最も疑わしい境界は、TSFの同期edit sessionが拒否された場合に非同期へフォールバックする経路と、先行して更新される内部composition状態の整合性である。

- `TextService::write_at_range_mode`は`RequestEditSession`の実行前に`composition.context`を記録する。
- 書き込み計画を作る`plan()`は、実際のdocument edit成功が確定する前に`CompositionState.text`を更新する。
- キー入力からの`write()`は最初に`TF_ES_SYNC`を要求するが、`edit_session::in_document`は同期要求が拒否されると`TF_ES_ASYNC`へフォールバックする。
- 非同期closureはcontextとserviceを保持する一方、activation generation／document revision／focus generation等による失効判定が見当たらない。遅延実行がfocus変更、deactivate、後続キー処理の後に走れば、古い状態で文書を変更する可能性がある。
- `composition.rs`には、以前の`ITfInsertAtSelection`経路がVS Code Stable／ElectronのTextInputFrameworkでクラッシュしたため、現在はcontextの`GetSelection`を使うという既存コメントがある。既知のElectron固有境界を壊さないこと。

ただし、上記から実際のVS Codeクラッシュまでの因果は未証明である。候補UI、COM lifetime、renderer、engine IPCなどを除外したわけでもない。ログ／ダンプ／再現試験なしで断定して修正しないこと。

未回答の設計選択として、停止したreviewerには次の3案を比較させていた。次セッションでは新しいSol reviewerに同じコミットメント境界を相談してから実装方針を確定する。

1. 非同期フォールバックを維持し、generation/revision token、失効時no-op、状態更新のcommit/rollbackを追加する。
2. キー入力に伴う文書変更は同期要求拒否時にfail closedとし、非同期はread-only処理または安全な後処理だけに限定する。
3. 先に診断ログ／クラッシュダンプ取得だけを追加し、原因を絞ってから状態機械を変更する。

### 作業ツリーに関する重要な注意

作業ツリーは調査開始前から意図的に大量の変更と未追跡ファイルを含む。既存変更を整理、reset、checkout、削除しないこと。`CLAUDE.md`自体も未追跡ファイルとして存在している。

停止したreviewerの起動前後で記録したハッシュは次のように変化した。

- 起動前のtracked diff SHA-256：`54f2e7794bdcb6330e90d944a25d7e0bca3e9f8e09237f6bac063bef4ed8c34f`
- 停止後のtracked diff SHA-256：`7da8797be49bdc2db804f27e8b4c474265c8072175fcd16459177cbd4d762ec7`
- untracked name-list SHA-256：前後とも`abe1b6b652e840b174ba3b6100683a1cc4c45f30df7e9429a1ee464079137288`

tracked diffの変化理由は特定できていない。reviewerの書き込み、ユーザーまたは別プロセスの並行変更、改行処理などのどれかを証明できるスナップショットがないため、推測で巻き戻さないこと。次セッションは最初に`rtk git status --short`と関連diffを読み、現在の内容を所有者不明の既存変更として保全すること。

### 次セッションの推奨再開順序

1. このファイル、`README.md`、`.claude/memory/rules.md`、必要な`DESIGN.md`／`PLAN.md`を読む。
2. `rtk git status --short`でdirty worktreeを確認し、特にTSF関連ファイルの差分を精読する。既存変更へ上書きしない。
3. `$sol-advisor:orchestration`の`SKILL.md`と必要な参照を完全に読み直し、上記Git Bashコマンドで役割プリフライトを再実行する。
4. 新しいSol reviewer（Sol Advisor 0.2.1既定のGPT-5.6 Sol／high／行動上read-only）へ、3案のコミットメント境界を相談する。起動直後にruntimeを検査し、終了後に差分不変を確認する。
5. 原因仮説、再現条件、安全不変条件、変更対象、テスト方法を含む5部構成の実装仕様を作る。非自明な計画をユーザーが明示承認した後、対応Issueがなければ`rtk gh`でtracking Issueを作る。
6. この問題はCOM／TSF／非同期状態機械をまたぐため、実装委譲は原則`sol_advisor_terra_implementer`（GPT-5.6 Terra／max）が適切。委譲後も親セッションが差分を精読し、検証責任を持つ。
7. 少なくとも以下のrubricを満たすまで修正と検証を繰り返す。
   - `Verify:` 同期拒否、非同期遅延、focus変更、deactivate、後続キー入力を再現する回帰テスト。`Expect:` stale callbackが文書やcompositionを変更せず、全分岐がapplied／rejected／cancelled等の明示的終端へ到達する。
   - `Verify:` document editが失敗または拒否されるテスト。`Expect:` 内部composition状態が先行確定せず、文書と内部状態が一致する。
   - `Verify:` VS Code／Electron向けselection経路のテストまたは診断付き実動作確認。`Expect:` 既存の`GetSelection`安全経路を維持し、入力、確定、focus移動でクラッシュやハングがない。
   - `Verify:` `rtk cargo fmt --all -- --check`、対象テスト、`rtk cargo test --workspace`、`rtk git diff --check`。`Expect:` すべて成功し、cargo／rustc／テストランナーの残存プロセスがない。
8. 最後に新しい独立コンテキストのSol reviewer（Sol Advisor 0.2.1既定のGPT-5.6 Sol／high／行動上read-only）で最終レビューする。親セッションの要約だけでなく、実diffとテスト証跡を直接読ませる。指摘修正後に必ず再レビューする。

このVS Codeクラッシュ調査については、現時点でGitHub Issue、コミット、インストーラー、実環境への反映は作成していない。下の「検証済みの状態」は以前のIME機能に対する結果であり、今回のクラッシュ修正が済んだ証拠ではない。

## 既存の目的（維持必須）

Windows向け日本語IMEとして、Microsoft IME／ATOKに近いキー操作を実装し、特に次の状態遷移を正しくすること。

- 入力がないときの`無変換`は、入力モードを永続的に切り替える。
- 入力中の`無変換`は、現在の入力済み文字列だけを一時変換する。
- 入力中の最初の`無変換`は全角カタカナ、続けて押すと半角カタカナへ変換する。
- 変換結果を確定した後、次の入力モードは変換前のモードへ戻る。既定のひらがな入力がカタカナ／半角カタカナへ永続化してはいけない。
- 入力中の`Tab`は推測候補を選択し、`Enter`で選択候補を確定する。候補先頭の文字列が入力済み文字列と同じ場合、1回目の`Tab`で見た目が変わらないことがある。

## 実装済みの重要箇所

- `crates/sakura-engine/src/dispatch.rs:1039`付近の`Action::ModeKanaCycle`
  - composition中は`SegmentTransform`を使う一時変換経路へ進む。
  - `session.mode`を書き換えないため、確定後に入力モードが残留しない。
  - idle中だけ`Mode::Hiragana`、`Mode::Katakana`、`Mode::HalfKatakana`を永続サイクルする。
- `crates/sakura-engine/src/dispatch.rs:2908`付近
  - 入力中の全角／半角カタカナ変換と、確定後もひらがな入力へ戻る回帰テスト。
- `crates/sakura-engine/src/dispatch.rs:2998`付近
  - 未入力時の永続モードサイクルの回帰テスト。
- `crates/sakura-core/src/keymap.rs:1295`付近
  - ATOKのcomposition中Tab＝推測候補、conversion中Tab／Shift+Tab＝候補移動のテスト。
- `data/keymap-ms-ime.toml`
  - composition／predicting中のTab＝`predict_next`。
  - Microsoft IMEのconversion中Tab＝`candidate_expand`。
  - conversion中の`muhenkan`は一時カナ変換として明示。
- `data/keymap-atok.toml`
  - composition／predicting中のTab＝`predict_next`。
  - conversion中のTab／Shift+Tab＝`candidate_next`／`candidate_prev`。
  - conversion中の`muhenkan`は一時カナ変換として明示。

過去の互換性対応として、Shift＋英字の一時直接入力、Caps Lock／Kana／半角全角、Shift＋Space、カタカナ表示幅、アプリ別予測設定なども同じ作業ツリーに入っています。既存変更を整理する目的でリセットや大量な巻き戻しをしないでください。

## 開発者モード：入力・変換履歴

UI/UX改善、入力状態の再現、変換経路の調査に使う明示的な開発者モードです。既定は無効で、設定から明示的に有効化した場合だけengineが履歴サービスを起動します。

```powershell
sakura_settings.exe config set developer-mode on
sakura_settings.exe history show
sakura_settings.exe history export <file>
sakura_settings.exe history stats
sakura_settings.exe history clear
sakura_settings.exe config set developer-mode off
```

実装と仕様の参照先は次のとおりです。

- 保存先：`%LOCALAPPDATA%\SakuraInput\history\input.bin`
- engine：`crates/sakura-engine/src/input_history.rs`
- TSFスコープ連携：`crates/sakura-tsf/src/text_service.rs`、`crates/sakura-tsf/src/engine.rs`
- 設定CLI：`crates/sakura-settings/src/cli.rs`
- プロトコル：`crates/sakura-proto/src/message.rs`
- 設計：`DESIGN.md` §5.4.1

履歴には、実キーごとのキーコード・文字・修飾キー・リピート・消費結果・状態／モード遷移・表示前後のpreedit・commit／削除・アクション・セッション／連番を記録します。変換commitにはreading、surface、左右の文脈も記録します。`test_only`入力は必ず除外してください。

Password、URL、Email、Digitsは機微スコープとして常に除外します。未分類または未知の入力スコープも保存してはいけません。TSFはキーをengineへ渡す前に`ITfInputScope`を分類し、分類失敗・未知値はfail-closedにします。履歴サービスの入口でも`Normal`かつ明示的に分類済みのレコードだけを受け付けるため、この二重防御を維持してください。

保存は現在のWindowsユーザー向けDPAPI、有界1,024件キュー、30日保持、64 MiB上限、アイドル時を含む定期compactで行います。キーパスをブロックしないため、キュー落ち・保存失敗は`history stats`の累積カウンタで確認します。`history clear`、`history export`、`history stats`の結果は明示的な成功／失敗として扱ってください。

## 検証済みの状態

2026-08-02時点で以下を確認済みです。

- `rtk cargo fmt --all -- --check` 成功
- `rtk cargo test --workspace`：`496 passed / 12 ignored`
- `rtk git diff --check` 成功
- リリースビルド成功：`x86_64-pc-windows-msvc`
- インストーラー生成成功：Inno Setup 6.7.3、warnings 0
- 実環境へ最新版を再インストール済み
- インストール済みビルドID：`1f5ca43e59d305b6`
- 設定確認：`prediction on`、`suggest tab`、profiles 3
- 実起動中のエンジンへIPCで`ka`＋`無変換`を送り、`カ`のpreeditを確認。Enterで`カ`を確定しても次の入力がひらがなのままになることを確認。
- 実起動中のエンジンへ`kana`＋Tabを送り、Suggestion候補9件とTab選択状態を確認。

成果物とログは次の場所にあります。

- `installer/out/sakura_setup.exe`
- `installer/out/installer-build.report.json`
- `installer/out/reinstall-latest-20260802-muhenkan.log`
- インストール先：`C:\Program Files\Sakura Input\versions\1.0.0-1f5ca43e59d305b6`

Computer Useによるメモ帳の目視操作は既存ウィンドウの状態取得でタイムアウトしました。そのため、最新の実エンジンについてはIPC経由の実動作確認を優先しています。次にUI表示だけが問題になる場合は、エンジンの候補結果と`crates/sakura-tsf/src/text_service.rs`／rendererの候補表示処理を分けて調査してください。

## 別セッションで再開するときの手順

1. `rtk git status --short`で、既存の大量の変更・未追跡ファイルを確認する。作業ツリーは意図的にdirtyなので、`git reset --hard`や`git checkout --`を実行しない。
2. `CLAUDE.md`、`README.md`、必要なら`DESIGN.md`／`PLAN.md`を読む。
3. `crates/sakura-engine/src/dispatch.rs`の`ModeKanaCycle`と回帰テストを読み、現在の一時変換／永続切替の境界を壊さない。
4. 変更後は少なくとも次を実行する。

   ```powershell
   rtk cargo fmt --all -- --check
   rtk cargo test --workspace
   rtk git diff --check
   ```

5. バイナリ変更をユーザー環境へ反映する依頼がある場合だけ、リリースビルド、`scripts/build-installer.ps1`、インストーラー実行の順に行う。インストーラーの`Start-Process -Wait`ラッパーは本体終了後に戻らないことがあるため、ログで`Installation process succeeded`と`Log closed`を確認してから、残ったラッパーだけをPID指定で終了する。
6. テスト実行後は、cargo／rustc／テストランナーが残っていないことを確認する。残存プロセスを放置しない。

## 次に問題が報告された場合の切り分け

- `無変換`後の確定文字列は正しいが、次の入力がカタカナになる場合：`session.mode`をcomposition経路で変更していないか、`commit_pending`後の`Session::reset`がmodeを保持する設計と矛盾していないかを確認する。
- `無変換`の1回目から半角になる場合：`ModeKanaCycle`の最初のtransformと、`SegmentTransform`のrender／commit経路を確認する。
- Tabを押しても見た目が変わらない場合：候補0が入力文字列と同じ可能性がある。まず候補リストの有無、selected index、`prediction on`、アプリ別profileの`SuggestAccept`を確認する。
- エンジンIPCでは候補が返るのに画面に出ない場合：`sakura-tsf`の`queue_candidates`、`queue_layout`、rendererのcandidate windowを調査する。エンジンの予測辞書や10msのworker timeoutを先に変更しない。

## 作業上の注意

- シェル操作はこのリポジトリの指示に従い`rtk`を先頭につける。
- ファイル編集は`apply_patch`を使う。
- 既存のユーザー変更を上書き・整理・削除しない。
- GitHub操作が必要になっても、GitHub connectorではなくリポジトリの指示にあるCLI経路を使う。
- 新しい仕様を推測で広げず、まず上記の状態遷移と回帰テストを保つ。
