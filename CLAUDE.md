# Sakura Input 引き継ぎ指示書

## リリース署名に関するowner判断（2026-08-22）

- ownerは、コード署名証明書が未設定でもSakura Inputの正式リリースを公開してよいと明示承認した。GitHub Actionsの`release` environmentに署名secretがないことを、リリースのblockerにしない。
- 未署名のinstallerを署名済みと表現してはいけない。リリースノートと導入案内にはAuthenticode未署名であること、GitHub ReleaseのSHA-256を照合して手動インストールすることを明記する。
- updater側の`WinVerifyTrust` fail-closed検証は弱めない。未署名リリースの自動取得・実行は拒否される設計を維持し、未署名版は手動インストール対象として扱う。
- 署名secretが3点すべて揃っている場合は従来どおり署名・検証してよい。部分設定は曖昧な成果物を作らずCIを失敗させる。

## Issue #58 GPT-5.6 Luna文章変換・選択文字列校正

- AI文章変換は明示操作だけで起動する。共通トリガーは既定のJIS `変換`、設定可能な`Caps Lock`、無効の3択で、preeditを優先し、次にホストの非空選択範囲を対象とする。対象がない場合は既存キー動作へ戻す。校正はlanguage-barメニューから選択文字列に対してだけ起動し、composition中は拒否する。
- exact modelは`gpt-5.6-luna`、wire APIはResponsesのみ。OpenAI、Azure OpenAI、AWS Bedrock、Cloudflare、Customは設定済みResponses互換Endpointへ送る。ChatGPT SubscriptionはCodex CLI検出時だけ設定候補にし、既存ログインを使う。sourceは匿名stdinだけで渡し、argv、環境変数、ファイル名、persistent sessionへ入れない。CLI、login、model、API失敗は可視エラーで終端し、fallbackしない。
- APIキーはWindows Credential Managerだけに保存し、registry、ログ、argv、環境変数へ保存しない。engineから`crates/sakura-ai-worker`へ上限付きbinary protocolの匿名stdinで渡し、送信後の一時バッファをzeroizeする。WinHTTP、JSON、Codex CLI起動はTSF DLLとengineから分離したworkerだけが所有する。
- `crates/sakura-engine/src/ai_text.rs`は全体で同時1 job、owner/session identity、repeat latch、同一operation/sourceのcooldown、deadline、detached cancellationを管理する。workerをcancelしてもprocess終了まではcapacityを解放しない。TSFは50 ms timerでpollし、focus/context/scope/source/rangeの一致を再検証してからだけ適用する。
- selected-text resultは捕捉したexact `ITfRange`を使い、write cookie取得後にも元文字列を再読する。preedit resultは既存write journalへ入れ、journalのApplied終端後だけ開発履歴へAppliedとして記録する。focus loss、deactivate、host edit、stale callback、timeout、malformed/errorは文書を変更せず明示終端する。
- `Normal`と確定分類されたscopeだけを許可する。Password、URL、Email、Digits、unknown、classification failure、`test_only`はworker起動前と履歴入口の双方でfail closedにする。開発者履歴は既存DPAPI・保持上限を使い、operation/status/model/provider/style、bounded source/result、content-free error code、latency、attempts、token metricsを記録する。
- 仕様・探索結果・未検証範囲は`verification/ai-text-verification.md`、TLA+モデルは`verification/tla/AiTextLifecycle.tla`にある。モデルは実装コードから独立した状態名と遷移で、Safety/Liveness/deadlock/reachabilityを3構成で検査する。探索上限を実装保証と混同しない。
- このIssue #58ではownerがSolAdvisorを使用しないよう明示している。古いVS Codeクラッシュ引き継ぎ節にあるSolAdvisor必須記述を、この機能の実装・検証へ適用しない。

## タスクバー入力モードasset（Issue #26）

- `crates/sakura-tsf/assets/mode-indicator`には、全6モードの16px／32px・dark／light用premultiplied BGRA assetがある。第三者製品のassetを含めたり、その解析内容を公開文書へ記載したりしない。
- assetはSakura Input独自のYu Gothic UI Semibold字形で、`scripts/generate-mode-indicator-assets.ps1`から再生成する。実行時フォント描画へ戻さない。
- `crates/sakura-tsf/src/mode_item.rs`はDPI境界でassetを選び、top-down 32-bit DIBからHICONを生成する。中間bitmapは必ず削除し、返却HICONの所有権はTSFへ渡す。
- 直接入力は斜線付き`A`、半角英数は通常の`A`であり、同じassetへ統合しない。全モード・サイズ・テーマの透過、alpha、premultiplication、一意性、HICON生成を総当たりテストする。

## Issue #27 Sakura候補ポップアップ（自動検証・通常light実画面確認済み）

- 候補表示はrenderer所有のWin32ポップアップであり、non-activating、click-through、キャレット追従、DPI対応、UI Automation公開を維持する。engine／TSFが持つ候補順、選択、ページ、候補種別の意味をrendererが変更してはいけない。
- Sakura独自の見た目は、low-contrastの暖色系neutral、Yu Gothic UI、28 logical px行、候補番号／文字列／注記の列、260–480 logical pxのcontent-aware幅、控えめな種別・ページfooter、passiveなページ位置rail、選択行のmuted sakura 2 logical px railである。候補文字列を主階層とし、注記・ページ情報は補助階層にする。
- light／dark paletteに加え、Windows high-contrastではsystem roleを使う。UI-less hostには`ITfUIElement`候補データ経路を保ち、popupの可視性に依存させない。compact／expanded表示は既存のengine semanticsとキー操作を維持し、新しい候補定義や操作を追加しない。
- 実装は通常のWin32 popupをGDI（`CreateFontW`／`DrawTextW`／brush）で描く。layered windowやDirectWriteを使う設計として説明しない。これはrendererの描画境界だけの変更で、候補用raster assetの再生成は不要であり、Issue #26のmode-indicator assetを変更・流用しない。
- unit testと実renderer processのintegration testにより、compact／expanded semantics、260–480 logical px幅、DPI変更、non-activation、キャレット追従、ページ、数字選択、UI Automation公開は自動検証済み。最新版再インストール後の通常light実画面スクリーンショットでは、候補本文、右側annotation列、淡い選択面と桜色rail、予測footer、`1–9/9`ページ表示を目視確認済み。Issue #27の検証記録ではmode-indicator assetに差分がないことも確認済み。残る受け入れ作業はdark／Windows high-contrastの実画面確認だけである。

## Issue #28 選択候補の辞書詳細（実装・実データ検証済み）

- 詳細はSakura独自の候補ポップアップと同一HWNDで、選択中の候補1件だけに補助表示する。候補の順序、選択、ページ、入力フォーカス、click-through、GDI描画境界を変えない。候補一覧はページ全体から幅を決め、選択移動で基準矩形や列位置を動かさない。辞書説明は一覧annotationへ重複させず、一定幅の詳細ペインで全文を折り返す。モニター作業領域に物理的に収まらない場合だけ末尾を省略する。配置は右、左、下の順に試し、収まらなければ表示しない。
- 辞書の原文説明は表示用上限を持たない。一方、wireへはUTF-8安全なプレビューと明示的なtruncated flagだけを送り、暗黙に切り詰めない。UIAは選択候補の詳細を公開し、プレビューが省略されていることを保持する。候補全件へ詳細を複製しない。
- detailは最終ENTR tableのexact entry ordinalで結び、surface文字列だけで照合しない。複合候補、ordinal不一致、旧辞書、壊れたoptional table、空または不正なdetailはfail-closedでdetailなしにする。
- 別名／関連語／類似語／反対語はmanifest固定済みの直接データだけであり、実行時の推測、表記類似、埋め込み類似、カテゴリ、推移的探索で補わない。表示は各種最大3語。固定したsmile-chatとJapanese WordNet 1.1を統合したfull-source構成は36,606 source-backed details。通常の既定ビルドは、curated sourceとIssue #30のreview済み000010 releaseを合わせ、472,825 entries中29,229 exact-entry detailsを生成する。000010の承認236語は246 exact-entry detailsとなり、全レコードに関連語、合計43件の類似語、16件の反対語がある。説明なし・同形異義・多義で一意に解決できないentryと未審査draftはdetailなしにする。
- 固定seed／総当たり型テストでUnicode・絵文字・wire frame境界、ordinal collision、compound omission、relationの自己参照／重複／循環、UIA、DPI、画面端を確認済み。full-source実データの既存検証値は93,001,395 bytes、SHA-256 `f8894a485c6e2ae98d499a74dc72dad74b2f6260f40cc4a00c5f4c86765c5a2f`。000010を含む通常の既定ビルドは2-pass決定性、39,349,040 bytes、SHA-256 `b7d08643395181f6d214866f9bb98646de366dc71caa15320effe774bc4c1d90`を2026-08-14に確認した。両構成の件数・hashを混同しない。以前記録していた38,456,565 bytes、SHA-256 `6d34364b...`は60eb263（bunsetsu boundary table、2026-08-12）より前の値であり、`data/dictionary-build.report.json`もその時点で更新されていなかった。サイズ増はboundary table由来で、Issue #48の一桁数字校正は内容hashだけを変える（HEAD overlayだけで作ったbaselineも39,349,040 bytes）。

## Issue #30 重要辞書のSakura作成説明

- 対象はIT・技術用語、外来語・カタカナ語、略語・英数字、専門用語を優先する。全辞書を件数だけで埋める目標は持たず、語義が一意で実用性の高い語を選ぶ。
- `data/llm-detail-targets/<batch>`のcommitted target manifestが全入力hashとexact dictionary identityを固定し、`data/llm-details/releases/<batch>`のrelease manifestが審査済みJSONLを固定する。draftは直接importできず、release directoryと対応target directoryを両方指定しない限り`dictc`へ入らない。
- 現在の通常ビルド対象は000010。既定辞書だけから作った242 targetsのうち236語を承認、6語（始め、監督、命令、告知、提言、標記）を保留し、承認語は246 exact-entry detailsとして入る。候補段階の「終わり」は多義・複数identityのためtarget作成前に保留した。承認レコードは全件に少なくとも1つの型付き関係語を持つ。レビューはユーザー指定によりsubagentを使わず、同一モデルの別prompt工程で実施したもので、独立モデル審査とは表現しない。000004以前のreleaseは履歴として残るが通常ビルドへ重ねてimportしない。既存detailと同じNFC正規化済み(surface, reading) pair、曖昧語義、辞書identity不一致、改ざん、未知schemaはfail closedで除外する。

## Issue #32 Sakura-Rerank-Tiny-v1 統合（1.0.5同梱準備中）

旧DeBERTa Tinyのruntimeとinstaller opt-in経路はowner指示で除去した。現在の任意rerankerは、自作の `Sakura-Rerank-Tiny-v1-research-prototype` だけを対象とする。2026-08-14にownerがMITライセンスでの配布と通常installerへの同梱を明示承認したため、1.0.5ではモデル、worker、ONNX Runtimeを同梱し、既定の`LongTextOnly` scopeで有効にする。既存設定が明示的に`off`なら上書きしない。研究時点のGate A未通過とfinal holdout未使用は変更せず、品質gate合格とは表現しない。

### 現在の実装境界

- TSF、engine、workerはRust。workerは `crates/sakura-neural-worker` の `sakura_neural_worker.exe` で、ONNX Runtime DLLとモデルをプロセス境界の外部artifactとして動的に読む。
- モデル配置は `neural/sakura-rerank-tiny-v1/{model.onnx,manifest.json}`。manifestはモデルcontract、研究manifest SHA-256、Gate状態、final-holdout非使用、MITライセンス、配布承認、model size/SHA-256を厳密検証する。
- protocol v1を維持し、候補表記、local cost、fingerprintだけを渡す。context/reading tensorはゼロ、利用可能featureは正規化local cost、候補順、surface長だけである。モデルscoreを完全なlistwise選択信号として扱い、local costへ二重加算しない。
- 追跡するFP32 artifactと研究manifestは `models/sakura-rerank-tiny-v1/` に置く。`scripts/build-sakura-reranker.ps1` が固定hashのONNX Runtime 1.28.0と合わせてrelease stagingを生成し、`scripts/stage-sakura-rerank.ps1` は既存directoryを上書きしない。

### プライバシーとフォールバックの不変条件

- cloud 送信はしない。worker が受け取るのは、表示前の候補スナップショットだけであり、本文・入力履歴・ユーザー辞書・学習ストアを渡さない。
- `Password`、`URL`、`Email`、`Digits`、未分類または未知の scope、`test_only` 入力は除外する。worker 不在、runtime/model/manifest の不備、検証失敗、timeout、異常応答では元順位を保持して fail closed する。
- UI は worker を待たず、候補を表示した後に非同期で並べ替えない。TSF/engine 本体の 15 MiB 予算と任意 worker のメモリは別境界であり、worker の実測値は未測定として扱う。

### 再開時の検証と未解決事項

- model-free unit test、実FP32 ONNXのprobeと2候補protocol v1 IPCは確認する。engineは候補fingerprintと順序を完全照合し、最大finite scoreを選択し、同点では元の先頭順を維持する。
- missing/malformed/stale/late/timeout/failed結果は必ず既存local rankingへfail closedする。候補表示後の並べ替え、学習・exact cache・ユーザー辞書優先順位の上書きは禁止する。
- Gate A/B、Windows CPU batch-one 10,000回以上のcold/warm latency、private working setは未完了の別工程であり、実モデルIPC成功やinstaller同梱を品質gate合格と表現しない。

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
