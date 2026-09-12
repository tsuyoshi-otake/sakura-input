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

