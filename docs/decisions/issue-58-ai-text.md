## Issue #58 GPT-5.6 Luna文章変換・選択文字列校正

- AI文章変換は明示操作だけで起動する。共通トリガーは既定のJIS `変換`、設定可能な`Caps Lock`、無効の3択で、preeditを優先し、次にホストの非空選択範囲を対象とする。対象がない場合は既存キー動作へ戻す。校正はlanguage-barメニューから選択文字列に対してだけ起動し、composition中は拒否する。
- exact modelは`gpt-5.6-luna`、wire APIはResponsesのみ。OpenAI、Azure OpenAI、AWS Bedrock、Cloudflare、Customは設定済みResponses互換Endpointへ送る。ChatGPT SubscriptionはCodex CLI検出時だけ設定候補にし、既存ログインを使う。sourceは匿名stdinだけで渡し、argv、環境変数、ファイル名、persistent sessionへ入れない。CLI、login、model、API失敗は可視エラーで終端し、fallbackしない。
- APIキーはWindows Credential Managerだけに保存し、registry、ログ、argv、環境変数へ保存しない。engineから`crates/sakura-ai-worker`へ上限付きbinary protocolの匿名stdinで渡し、送信後の一時バッファをzeroizeする。WinHTTP、JSON、Codex CLI起動はTSF DLLとengineから分離したworkerだけが所有する。
- `crates/sakura-engine/src/ai_text.rs`は全体で同時1 job、owner/session identity、repeat latch、同一operation/sourceのcooldown、deadline、detached cancellationを管理する。workerをcancelしてもprocess終了まではcapacityを解放しない。TSFは50 ms timerでpollし、focus/context/scope/source/rangeの一致を再検証してからだけ適用する。
- selected-text resultは捕捉したexact `ITfRange`を使い、write cookie取得後にも元文字列を再読する。preedit resultは既存write journalへ入れ、journalのApplied終端後だけ開発履歴へAppliedとして記録する。focus loss、deactivate、host edit、stale callback、timeout、malformed/errorは文書を変更せず明示終端する。
- `Normal`と確定分類されたscopeだけを許可する。Password、URL、Email、Digits、unknown、classification failure、`test_only`はworker起動前と履歴入口の双方でfail closedにする。開発者履歴は既存DPAPI・保持上限を使い、operation/status/model/provider/style、bounded source/result、content-free error code、latency、attempts、token metricsを記録する。
- 仕様・探索結果・未検証範囲は`verification/ai-text-verification.md`、TLA+モデルは`verification/tla/AiTextLifecycle.tla`にある。モデルは実装コードから独立した状態名と遷移で、Safety/Liveness/deadlock/reachabilityを3構成で検査する。探索上限を実装保証と混同しない。
- このIssue #58ではownerがSolAdvisorを使用しないよう明示している。古いVS Codeクラッシュ引き継ぎ節にあるSolAdvisor必須記述を、この機能の実装・検証へ適用しない。

