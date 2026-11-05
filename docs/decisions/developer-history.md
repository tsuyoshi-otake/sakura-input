## 開発者モード：入力・変換履歴

UI/UX改善、入力状態の再現、変換経路の調査に使う明示的な開発者モードです。既定は無効で、設定から明示的に有効化した場合だけengineが履歴サービスを起動します。

履歴の初期化は専用ワーカーで行います。既存ファイルの検証中もengineは入力を処理し、`history stats`は`active=false`を返します。初期化が完了するまでの入力は後から記録しません。初期化中に無効化した場合、その世代を有効なサービスとして公開せず終了します。初期化の失敗は入力ごとに再試行せず、明示的な無効化→有効化で再試行します。engine終了時は初期化と履歴ライターの終了を待つため、初期化途中の終了時間はファイル検証時間に依存します。

`history stats` の `history-package-version` は保存済みの最後の起動記録です。現在の収集状態は `history-service-active`、保存データの新しさは `history-newest-timestamp-ms` で確認してください。`history-records` は保持期間内の件数、`history-ignored-tail-bytes` は読取り時に検証できなかった末尾バイト数です。サービス未接続時の失敗・記録落ちカウンタは既定の0なので、0だけで正常とは判断できません（#434）。

`config set developer-mode on` は、設定を保存した後、最大180秒の確認枠で有効化を待ちます（無効化の確認枠は3秒、進行中のIPCには既存の通信期限が適用されます）。確認は直列で行い、100msから最大約1.1秒へ待機間隔を広げます。長い履歴の検証中に初期化を再起動することはありません。確認できない場合は `developer-history unconfirmed` と非0の終了コードを返し、設定は保存済みであることと `history stats` で確認する手順を案内します。未確認だけでは、初期化中か初期化失敗かを区別できません。

既存の `.writer.lock` は、誰も保持していなくても初期化を拒否する安全契約です。自動削除・自動再利用はしません。収集停止を復旧するときは、開発者モードを無効にして既存ライターの終了を確認し、暗号化履歴を保全・照合してから、通常ファイルで内容が空、かつ排他的な所有者がいないロックだけを退避します。再有効化後は初期化完了と新しい保存記録を確認します。ロックが残った原因の解明や電源断の保証は、この手順による復旧とは別です。

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
- 起動・有効化・終了の所有者：`crates/sakura-engine/src/history_runtime.rs`
- TSFスコープ連携：`crates/sakura-tsf/src/text_service.rs`、`crates/sakura-tsf/src/engine.rs`
- 設定CLI：`crates/sakura-settings/src/cli.rs`
- プロトコル：`crates/sakura-proto/src/message/`（`request.rs`・`response.rs`・`ui_state.rs`）
- 設計：`DESIGN.md` §5.4.1

履歴には、実キーごとのキーコード・文字・修飾キー・リピート・消費結果・状態／モード遷移・表示前後のpreedit・commit／削除・アクション・セッション／連番を記録します。変換commitにはreading、surface、左右の文脈も記録します。`test_only`入力は必ず除外してください。

Password、URL、Email、Digitsは機微スコープとして常に除外します。未分類または未知の入力スコープも保存してはいけません。TSFはキーをengineへ渡す前に`ITfInputScope`を分類し、分類失敗・未知値はfail-closedにします。履歴サービスの入口でも`Normal`かつ明示的に分類済みのレコードだけを受け付けるため、この二重防御を維持してください。

保存は現在のWindowsユーザー向けDPAPI、有界1,024件キュー、30日保持、64 MiB上限、アイドル時を含む定期compactで行います。キーパスをブロックしないため、キュー落ち・保存失敗は`history stats`の累積カウンタで確認します。`history clear`、`history export`、`history stats`の結果は明示的な成功／失敗として扱ってください。

履歴の追記途中で保存に失敗した場合、ライターは書き込み前のファイル長と位置を復元してから後続レコードを受け付けます。復元にも失敗した場合は、追記と定期compactを停止し、保存失敗カウンタとFlush／終了時のエラーで通知します。成功した`history clear`、または無効化→有効化後のファイル検証に成功した場合に記録を再開します。失敗したClearでは停止状態を解除しません。復元に成功しても、失敗した記録があることは成功したClearまでFlush／終了結果に残します。検証は[部分追記の復旧記録](../../verification/history-append-recovery.md)を参照してください。

Clear自身が元の書き込みハンドルを手放した後で失敗した場合も、追記と定期compactを停止します。不完全なヘッダーを通常の追記で開き直してはいけません。engine経由とオフラインのどちらも、空の履歴ヘッダーを書き、OSへの同期が成功してからClear成功を返します。消去前の状態へ戻す原子性や電源断への保証は含みません。検証は[Clear失敗の記録](../../verification/history-clear-failure.md)を参照してください。

開発者モードを無効にした直後の`history clear`（#417）：無効化したサービスの書き込みスレッドは、リクエストから見えなくなった後も、`history_runtime.rs`のワーカーが停止を完了するまで履歴ストアのロックを保持します。サービスが接続されていないClearはパスを直接指定して消去するため、engineは自プロセスの履歴世代の初期化と停止が終わるまで最大1.2秒待ちます。待ち切れない場合は`Busy`を返し、履歴は変更しません（数秒後に再実行してください）。

待つだけでは足りません。待機を終えてからClearを実行するまでの間に開発者モードが切り替わると、接続済みのサービスが停止して書き込みスレッドへ届かなくなるか、新しい世代がストアのロックを取ってパス指定のClearと衝突し、どちらも`Internal`で失敗します。そのためengineは、待機を終えた時点の履歴世代を`LifecycleHold`として保持し、Clearのディスパッチが終わるまで、次の世代の初期化も、現在のサービスの停止も行いません。Clearは保持を取った時点で公開されていたサービス（なければパス指定）に対して実行します。設定の反映は保持に待たされず、リクエストから見えるサービスはすぐ切り替わります。後ろへ回るのはワーカーの初期化と停止だけで、保持の長さはClear1回分です。
