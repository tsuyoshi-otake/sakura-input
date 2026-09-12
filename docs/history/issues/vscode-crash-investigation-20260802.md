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
powershell -NoProfile -Command "& 'C:\Program Files\Git\bin\bash.exe' -lc 'sh /c/Users/developer/.codex/plugins/cache/sol-advisor/sol-advisor/0.2.1/scripts/install-agents.sh --check'"
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
powershell -NoProfile -Command "& 'C:\Program Files\Git\bin\bash.exe' -lc 'sh /c/Users/developer/.codex/plugins/cache/sol-advisor/sol-advisor/0.2.1/scripts/inspect-agent-runtime.sh <thread-id>'"
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
- 【2026-08-13訂正】以前ここに「非同期closureに失効判定が見当たらない」と書いていたが、現行コードと一致しない。非同期フォールバックの書き込みは`crates/sakura-tsf/src/write_coordinator.rs`のticket／epoch journalを経由し、`validate_callback`（同ファイル469行付近）がdocument・UIアクセス前にDeactivated／ActivationChanged／FocusChanged／ContextReplaced／RevisionMismatch／StaleCallbackを検証して失効callbackを拒否する。stale callback仮説を前提にした調査・修正を再開しないこと。クラッシュ原因は依然未特定であり、再開時はログ／ダンプ取得から始める。
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

tracked diffの変化理由は特定できていない。reviewerの書き込み、ユーザーまたは別プロセスの並行変更、改行処理などのどれかを証明できるスナップショットがないため、推測で巻き戻さないこと。次セッションは最初に`git status --short`と関連diffを読み、現在の内容を所有者不明の既存変更として保全すること。

### 次セッションの推奨再開順序

1. このファイル、`README.md`、`.claude/memory/rules.md`、必要な`DESIGN.md`／`PLAN.md`を読む。
2. `git status --short`でdirty worktreeを確認し、特にTSF関連ファイルの差分を精読する。既存変更へ上書きしない。
3. `$sol-advisor:orchestration`の`SKILL.md`と必要な参照を完全に読み直し、上記Git Bashコマンドで役割プリフライトを再実行する。
4. 新しいSol reviewer（Sol Advisor 0.2.1既定のGPT-5.6 Sol／high／行動上read-only）へ、3案のコミットメント境界を相談する。起動直後にruntimeを検査し、終了後に差分不変を確認する。
5. 原因仮説、再現条件、安全不変条件、変更対象、テスト方法を含む5部構成の実装仕様を作る。非自明な計画をユーザーが明示承認した後、対応Issueがなければ`gh`でtracking Issueを作る。
6. この問題はCOM／TSF／非同期状態機械をまたぐため、実装委譲は原則`sol_advisor_terra_implementer`（GPT-5.6 Terra／max）が適切。委譲後も親セッションが差分を精読し、検証責任を持つ。
7. 少なくとも以下のrubricを満たすまで修正と検証を繰り返す。
   - `Verify:` 同期拒否、非同期遅延、focus変更、deactivate、後続キー入力を再現する回帰テスト。`Expect:` stale callbackが文書やcompositionを変更せず、全分岐がapplied／rejected／cancelled等の明示的終端へ到達する。
   - `Verify:` document editが失敗または拒否されるテスト。`Expect:` 内部composition状態が先行確定せず、文書と内部状態が一致する。
   - `Verify:` VS Code／Electron向けselection経路のテストまたは診断付き実動作確認。`Expect:` 既存の`GetSelection`安全経路を維持し、入力、確定、focus移動でクラッシュやハングがない。
   - `Verify:` `cargo fmt --all -- --check`、対象テスト、`cargo test --workspace`、`git diff --check`。`Expect:` すべて成功し、cargo／rustc／テストランナーの残存プロセスがない。
8. 最後に新しい独立コンテキストのSol reviewer（Sol Advisor 0.2.1既定のGPT-5.6 Sol／high／行動上read-only）で最終レビューする。親セッションの要約だけでなく、実diffとテスト証跡を直接読ませる。指摘修正後に必ず再レビューする。

このVS Codeクラッシュ調査については、現時点でGitHub Issue、コミット、インストーラー、実環境への反映は作成していない。下の「検証済みの状態」は以前のIME機能に対する結果であり、今回のクラッシュ修正が済んだ証拠ではない。

