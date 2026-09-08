# 高負荷時の入力・変換整合性（#148）

Baseline HEAD: `5eb40e3807c2997270b7361f6ca0c63bf09743d1` (main, v1.0.38, clean worktree)
Program status: **Phase 0 IN PROGRESS**. 下記は静的なコード読取りの結果であり、
実測でも原因確定でもない。実測は Phase 1 以降で追記する。

## 1. 目的

高負荷（CPU / メモリ / ディスク I/O / OS scheduling）の下で、入力文字・preedit・
変換状態・候補選択を失わず、重複させず、順序を壊さず、古い結果を適用しない。
遅延の許容と整合性の非許容を分けて扱う。

## 2. 物理キー1打の経路（TSF DLL 側）

すべてホストアプリの UI／キーストロークスレッド上で動く。この経路は worker thread を
起こさない。`sakura_ipc::Client::call_until` は**同じスレッドを named pipe 上でブロック**する。

### 2.1 段階表

| # | 段階 | 実装 | thread | block | deadline | IPC | COM 再入 | file I/O | alloc | lock |
|---|---|---|---|---|---|---|---|---|---|---|
| 0 | deadline scope 確立 | `text_service.rs:6809` / `6814` / `6845` → `callback_deadline.rs:18` | host UI | no | **`KEY_BUDGET` 50 ms を親として確立** | no | no | no | no | no（thread-local `Cell`） |
| 1 | Win32 → `KeyInput` 変換 | `key_handler.rs:87-238` | host UI | no | 継承 | no | no | no | stack 8×u16 | no |
| 2 | conversion seat 判定 | `text_service.rs:5238-5245`, `conversion_key.rs:256-276` | host UI | **yes** | 継承 | no | no | no | no | **process-global `Mutex<LiveCompositionTable>`** |
| 3 | unactionable / no-context 短絡 | `text_service.rs:4162`, `5246-5263` | host UI | no | 継承 | no | no | no | no | no |
| 4 | AI トリガ捕捉（該当時のみ） | `text_service.rs:5279-5302`, `edit_session.rs:112-141` | host UI | **yes** | 継承 | 条件付き | **yes** (`TF_ES_SYNC\|TF_ES_READ`) | no | no | no |
| 5 | Probe（`OnTestKeyDown`） | `text_service.rs:5308-5340` → `engine.rs:485-502` → `engine.rs:792-866` | host UI | **yes** | `min(親, +50ms)` | **yes** `ProbeKey` | yes（scope 読取） | no | frame buf | no |
| 6 | real-callback fencing | `text_service.rs:5345-5407`, `2086-2133` | host UI | no | 継承 | no | 条件付き（`EndComposition`） | no | no | `RefCell` |
| 7 | 隣接性証明 + scope 公開 | `text_service.rs:2777-2811`, `2892-2899` → `engine.rs:509-572` | host UI | **yes** | `min(親, +50ms)` | **yes** `ResetDocumentContext` / `SetInputScope`（+ 必要なら `Revert` resync） | **yes** sync read | no | no | `RefCell` |
| 8 | admission + 予約 | `text_service.rs:5437-5449`, `write_coordinator.rs:339-366` | host UI | no | 継承 | UI のみ (`UI_BUDGET`) | no | no | **`VecDeque` 伸長** | `RefCell` |
| 9 | **本命 `SendKey`** | `text_service.rs:2837` → `engine.rs:235-241` → `engine.rs:792-866` | host UI | **yes** | `min(親, +50ms)` | **yes** `SendKey`（未接続なら先に `Hello`+`CreateSession`、`RECONNECT_BUDGET`） | no | no | frame buf | no |
| 10 | 応答の処理 | `text_service.rs:5464-5587` | host UI | 条件付き | 継承 | 条件付き `UndoCommit` | **yes** sync read | no | no | `RefCell` |
| 11 | document 反映 | `text_service.rs:5663-5838` → `edit_session.rs:147-175` → `apply_queued_write` `text_service.rs:5875+` | host UI（sync）／後続 callback（async） | **yes**（sync 時） | 継承（sync）／**なし**（async 完了時） | 条件付き `UndoCommit` (`text_service.rs:6280`) | **yes** `TF_ES_SYNC` → 拒否時 `TF_ES_ASYNC` フォールバック | no | plan の `String`/`Vec` | `RefCell` |
| 12 | return | `text_service.rs:5520`, `callback_deadline.rs:27` | host UI | no | scope 破棄 | no | no | no | no | no |

### 2.2 1打あたりの同期 IPC 往復（最悪ケース）

段階 5・7・9・10・11 はいずれも `Client::call_until` で engine を同期待ちする。
`1d10296`（#134）以降、これらは**すべて同一の 50 ms を分け合う**（`callback_deadline::limit`
が `min(親 deadline, now + budget)` を返す）。したがって resync が遅ければ本命の
`SendKey` に残る時間が減る、という**時間の奪い合いが構造として残っている**。

| 呼び出し | Request | budget | `DeadlineExpired`（未送信） | `Timeout`（送信済み・遅い） |
|---|---|---|---|---|
| `engine.rs:797` | `SendKey` / `ProbeKey` / `Commit` / `Reconvert` / `CommitCandidate` | `KEY_BUDGET` | link 同期済なら `Answer::Rejected`（link 維持、何も送っていない）／既に desync なら `Unavailable` | `Answer::Unavailable`。`MayMutate` なら `link.desynchronized = true` |
| `engine.rs:1003` / `1021` | `Hello` + `CreateSession` | `RECONNECT_BUDGET` 50 ms を2往復で共有 | `open()` → `None` | 同左。`blocked_until` に `RETRY_INTERVAL` 2 s |
| `engine.rs:941` | `Revert`（resync） | 親の残り | `drop_link()` | `drop_link()` |
| `engine.rs:520` | `SetInputScope` | `KEY_BUDGET` | `false` → キーはホストへ返す（fail-closed） | `false`、`desynchronized = true` |
| `engine.rs:553` | `ResetDocumentContext` | `KEY_BUDGET` | `false` → `disconnect(DocumentContextReset)` | 同左 |
| `engine.rs:699` | `UndoCommit` | `KEY_BUDGET` | `false` → `disconnect(UndoCommitSettleFailed)` | 同左 |

**timeout 時に document がロールバックされることはない。** 既に反映済みのテキストは
残り、変わるのは「次の呼び出しが先に `Revert` で resync するか」だけである。

### 2.3 `CallbackDeadline` を持たない入口

親 deadline を確立するのは `OnTestKeyDown` (`6809`) / `OnKeyDown` (`6814`) /
`OnPreservedKey` (`6845`) の3つだけである。以下は**親を持たず、内部の各 IPC が独立に
満額の budget を取る**。

- `OnTestKeyUp` / `OnKeyUp` (`6820-6839`) — engine に触らないので現状は無害。
- `OnCompositionTerminated` (`6868-6890`) — `ask_to_finalize()` から `Engine::commit()` へ到達し得る。
- `OnLayoutChange`、候補 UI callback、`DEFERRED_WORK_MESSAGE` の遅延ハンドラ
  （`OnKeyDown` が return した後に `TF_ES_ASYNC` の書き込みを実行する経路）。
- `OnActivated` / focus / deactivation callback。
- AI job の start / poll / cancel（timer 経路）。

**#148 の観点**: 非同期で document を触る唯一の経路（`DEFERRED_WORK_MESSAGE`）が
親 deadline の外にある。ここは Phase 5 の Pending/ACK 設計が所有権を明示すべき境界である。

### 2.4 write journal が満杯のときの終端

`WriteCoordinator::reserve` (`write_coordinator.rs:339-366`) は
`operations.len() >= capacity || has_reservation()` で `AdmissionError::Full` を返す。
`text_service.rs:2166-2180` がこれを COM `E_UNEXPECTED` に写し、`handle_key_input`
(`5442-5447`) が `host_eaten(owner, key, false)` として**キーをホストへ返す**。
先行する `can_admit_write_for_context` (`5421`, `5439`) が通常はここへ到達する前に
同じ出口を取る。

→ **journal 満杯でキーが黙って消えることは、静的には無い。** ただし
「ホストへ返す」ことが利用者から見て正しいかは別問題であり（変換中に生の文字が
入る可能性）、圧力下での実挙動は Phase 3 の counterexample で固定する。

別の 16 スロット境界として `terminal_capacity` (`write_coordinator.rs:203`) の
`TerminalRecord` リングがあり、満杯時に**最古の監査レコードを黙って捨てる**
(`record_terminal`, `755-760`)。これは権威ある状態ではなく診断履歴なので設計どおりだが、
**#148 の「exactly-once terminal を証明する」目的には不足**する（証拠が消える）。

### 2.5 既存の sequence / generation / revision

| フィールド | 型 | 所有 | 意味 |
|---|---|---|---|
| `WriteCoordinator::activation` | `u64` | journal | activate/deactivate で増加 |
| `WriteCoordinator::focus` | `u64` | journal | activate/deactivate/focus 変更で増加 |
| `WriteCoordinator::context_generation` | `u64` | journal | context 変更ごと。`ContextId` の ABA 対策 |
| `committed_revision` / `tail_revision` | `u64` | journal | 反映済み／投機的先頭 |
| `OperationId` | `u64` | journal | 予約ごとの単調 id |
| `Epoch{activation,focus,context,context_generation,revision}` | struct | `Ticket`/`UiLease` | `validate_callback` (`482-510`) が5種の stale を判定 |
| `Ticket{id,epoch,result_revision}` | struct | in-flight write | |
| `UiLease{...}` | struct | 完了 write | 古い write の UI が新しいものを上書きするのを防ぐ |
| `SessionId` | `u64` | engine session | wire を渡る |
| `RequestId` | `u64` | IPC client | **per-call**。物理キーを跨がない |
| `CompositionFlight{id,lifecycle}` | struct | composition handle | |
| `ClaimToken{instance,generation}` | struct | live composition table | stale な非同期完了が claim を復活させない |
| `RecoveryToken` | `u64` | `TextService` | engine unavailable 時の finalizer 権威 |
| `instance_id` | `u64` | `TextService` | dual-TSF の兄弟識別 |

**欠落**: `key_seq`（物理キーの単調番号）が存在しない。`RequestId` は per-call であり、
`SendKey { session, key }` (`sakura-proto/src/message.rs:90`) は順序識別子を持たない。
`document_revision` / `composition_generation` は TSF 側の概念で **wire を渡らない**。

### 2.6 engine session を捨てる経路（`disconnect(reason)`）

`disconnect` (`text_service.rs:2823-2830`) は **IPC ではない**。診断を記録し、
隣接性キャッシュを消し、`self.engine` を `Engine::new()` で置き換える（=パイプを閉じる。
明示的な `DeleteSession` RPC は無い）。

理由コードと呼び出し元（`text_service.rs`）:

| 行 | reason | 契機 |
|---|---|---|
| 1601 | `UndoTerminalizationDeferred` | journal borrow 再入で undo 結果が不明 |
| 1635 | `UndoTerminalizationSettled` | 1回の bounded retry が `disconnect_required` |
| 2130 | `WriteContextObservationFailed` | context 置換だが cancel 対象が無い |
| 2249 | `CancelledStateTerminalized` | 汎用終端 (`reset_engine=true`) |
| 2340 | `CancelledWritesSettled` | 完了が空で `reset_engine` 要求 |
| 2551 | `ServiceDetached` | deactivation |
| 2770 | `DocumentContextReset` | `ResetDocumentContext` 失敗 |
| 3158 | `UndoCommitSettleFailed` | `UndoCommit` の失敗/timeout/期限切れ |
| 3924 | `DocumentAccessRevisionMismatch` | 調停で stale revision |
| 3930 / 7546 | `DocumentAccessUndoTerminalized` | 文書アクセス中に undo 終端 |
| 3939 | `DocumentAccessReconciled` | 調停後の無条件 reset |
| 4836 | `CandidateCommitPollFailed` | 候補クリック poll 失敗 |
| 5399 | `KeyContextAuthorityLost` | `ReplaceAndApply` 内の `observe_write_context` 失敗 |
| 5517 | `KeyPredecessorFailed` | engine が進んだ後の `submit_output` 失敗 |
| 5550 / 7191 | `EngineUnavailableRecovery` | 投機 tail を cancel した後の無条件 reset |
| 5932 | `CompositionProjectionAbandoned` | COM 呼び出し前に状態が乖離 |
| 6062 | `StaleQueuedWrite` | 非同期 write の flight が lifecycle callback に retire された |
| 6148 | `QueuedWriteTerminalFailure` | 非同期 write の終端が適用できない |
| 6287 | `AppliedWriteUnacknowledged` | 文書は変えたが engine が undo を ack しない |
| 6697 / 6724 | `ReconversionUnavailable` / `ReconversionFailed` | 再変換 |

## 3. 物理キー1打の経路（`sakura_engine.exe` 側）

### 3.1 スレッドモデル

`server.rs:1-24` は「pipe instance ごとに1スレッド、接続間で何も共有しない、
keystroke path は lock を1つも取らない」と述べている。`Dispatcher` / `Session` /
`SessionTable` については**その通り**である。しかし key path は
process-wide の共有サービスを経由し、**そこでは lock を取る**（§3.3）。

| スレッド | 数 | 実装 |
|---|---|---|
| `sakura-pipe`（pipe instance） | Data 最大 64 / Renderer 4 / Control 4 | `transport.rs:56`、`server.rs:769-770`、`server.rs:887-889`。stack 160 KiB（`server.rs:788`）。1接続 = 1 `Dispatcher` + 1 `Buffers` |
| learning maintenance | 1 | `learning.rs:1294`、`MAINTENANCE_INTERVAL = 5s`（`learning.rs:51`） |
| prediction worker | 1 | `prediction.rs` |
| long conversion（neural） | 1 + 子プロセス | `long_conversion.rs` |
| input-history writer | 1 | `input_history.rs:757`、bounded `sync_channel`（`QUEUE_CAPACITY = 1024`, `learning.rs` 相当は `input_history.rs:40`） |
| main | 1 | `server.rs:727` |

キー要求を処理するのは、その接続を accept した pipe instance スレッド自身である。
通常のキー打鍵で別スレッドへ渡るのは prediction 要求（10 ms 上限で**待つ**）と
long conversion 要求（待たない）だけ。

### 3.2 `SendKey` 1件の同期呼び出し鎖

| # | 段階 | 実装 | lock | I/O | alloc |
|---|---|---|---|---|---|
| 1 | frame receive | blocking `ReadFile` | no | pipe | no |
| 2 | endpoint / request matrix | `server.rs:1446-1488` | no | no | no |
| 3 | `configuration_snapshot()` | `server.rs:230-237`、呼び出しは `server.rs:1724` | **RwLock read**（process-wide） | no | clone |
| 4 | **`runtime_services()`** | `server.rs:255-...`、呼び出しは `server.rs:1725` | **`Mutex<DynamicRuntimes>`（process-wide、全リクエストが通る、`server.rs:223`/`:267`）** | 初回のみ **worker 起動 / 子プロセス spawn / `InputHistoryService::open`** | yes |
| 5 | `dispatch()` → `send_key` | `dispatch.rs:1098-1289` | no | no | no |
| 6 | session 取得 | O(64) 線形走査 | no | no | no |
| 7 | 履歴 before snapshot（developer mode 時） | `dispatch.rs:1179-1201` | no | no | `Session` clone + render |
| 8 | `apply_key` → 変換 | `dictionary.rs:263-318`, `367-427` | **`try_lock`**（`CONVERSION_SLOTS = 2`, `dictionary.rs:34`。busy なら即 `ConvertFailure::Busy`） | **mmap page fault の可能性** | no |
| 9 | learning 読み（`preference` 等） | `learning.rs:927-1030` | **blocking `state.lock()`** | no | no |
| 10 | learning 書き（`learn`） | `learning.rs:888-914` → `Log::append` `learning.rs:640-665` | **blocking `state.lock()` を保持したまま `write_all` ×3** | **file write**（`sync_data` は無し） | no |
| 11 | long conversion schedule | `long_conversion.rs` | 短時間の `Mutex` | no | no |
| 12 | prediction | `prediction.rs:394-430` | **`Condvar::wait_timeout` で最大 10 ms 待つ（設計どおり）** | no | no |
| 13 | 履歴 record | `input_history.rs:851-872` | no（`try_send`、容量 1024、溢れは counter） | no（writer thread が行う） | `String` 複数 |
| 14 | **debug trace** | `server.rs:1756-1759` → `debug_trace::emit` | no | **`var_os` + `create_dir_all` + `open` + `metadata` + `write` を毎キー** | `PathBuf` / `String` |
| 15 | encode + reply write | `server.rs:1760-1775`（blocking `WriteFile`） | no | pipe | no |
| 16 | UI publish | `server.rs:1779-1795` | 共有状態 | no | no |

無限ループは無い。`SessionTable` は 64、converter slot は 2、
`collect_commit_repair_readings` は 8 件で自己制限。

### 3.3 key path で 10 ms を超え得る箇所

| # | 箇所 | 上限 | 予算の有無 |
|---|---|---|---|
| 1 | prediction mailbox 待ち | 10 ms | **設計どおり予算あり**（`PREDICTION_TIMEOUT`, `dispatch.rs:85`） |
| 2 | learning compaction が mutex を保持 | 無し | `maintain()` は `try_lock` で取得（`learning.rs:1295`）した後、`compact_state`（`learning.rs:2073-2168`）で `sync_data` + 最大 16 MiB read + 全 replay + write + rename ×2 + reopen を**保持したまま**実行する。他スレッドの key path は blocking `.lock()` で全期間待つ |
| 3 | `forget_prediction_exact`（**renderer 経由**）が同じ mutex で全ログ書き換え | 無し | `learning.rs:1039-1277`。blocking `.lock()`（`:1047-1049`）を取り、`sync_data` + read + replay + temp write + 2 phase publish を保持 |
| 4 | `clear()` / 履歴クリア | 無し | `learning.rs:1340-1342`、同型 |
| 5 | cold dictionary の hard fault | 無し | `dictionary.rs:457-506` の mmap。`try_lock` した converter slot 内で発生し得る（仮説 H-B） |
| 6 | **`runtime_services` の初回起動が key スレッド上** | 無し | `server.rs:255-...`（仮説 H-E） |
| 7 | **debug trace のファイル I/O が毎キー** | 無し | `debug_trace.rs:66-128`（仮説 H-F） |
| 8 | 遅いホストへの blocking `WriteFile` | 無し | `server.rs:1775`。設計上は自スレッドのみに閉じる |

learning の blocking `.lock()` は 16 箇所（`learning.rs` の `.lock()` 全件）。
非ブロッキングは `maintain()` の `try_lock` 1箇所だけである。
つまり **maintenance がロックを取れた瞬間から、compaction 完了までの全期間、
全接続のキー処理が learning 参照でブロックし得る**。

### 3.4 protocol の既存 sequence / generation（engine 側）

| フィールド | 型 | 発行者 | 用途 |
|---|---|---|---|
| `RequestId` | `u64` | **client**（接続ごと） | frame header の stale-response 相関 |
| `SessionId` | `u64` | server、単調・再利用なし | session 同一性 |
| `Revision` | `u64` | server、engine 生存中単調 | `WatchUi` / 候補 capability |
| `LearningService.generation` | `AtomicU64` | process 内のみ | prediction cache 無効化。**wire を渡らない** |
| `session.prediction_generation` | — | process 内のみ | prediction cache / long conversion の key |

**server 発行の per-key sequence は無い。** wire を渡る generation は `Revision` だけで、
これは UI 状態用であり key transaction 用ではない。§8 の `KeyTransaction` に必要な
`key_seq` / `document_revision` / `composition_generation` は、
**現状どれも protocol に存在しない**。

### 3.5 既存の遅延注入 hook

**production の key path へ時間遅延を注入する仕組みは存在しない。** 近いものは:

- `server.rs:1926-1927` `tests::BEFORE_OUTPUT`（`#[cfg(test)]` の任意 closure、`Reply::Output` の encode 直前 `server.rs:1749-1754` で同期実行）
- `input_history.rs:1986` `tests::BEFORE_ENQUEUE`（`enqueue()` 先頭 `input_history.rs:851`）
- `server.rs:1928-1929` `tests::FAIL_PIPE_SPAWN_AFTER`（遅延ではなく失敗注入、`server.rs:982`）
- `learning.rs` `ForgetFaultPoint`（I/O 失敗注入。`#[cfg(not(test))]` では no-op）

`BEFORE_OUTPUT` は closure なので **sleep を入れれば「mutation 後・reply 前」位置の
遅延注入としてそのまま使える**。ただし `#[cfg(test)]` かつ同一プロセス内であり、
**別プロセスの実 engine へは注入できない**。#148 の stress harness は
`crates/sakura-engine/tests/common/mod.rs` の `Engine::spawn_isolated()` が起動する
**実行ファイル**を相手にするため、release ビルドにも残る明示的な注入点が別途必要になる。

## 4. 未確定仮説

`.claude/memory/rules.md` と `verification/revalidation-20260905.md` の T4 に従い、
以下は **仮説** として扱う。ETW 実測前に原因と断定しない。

- **H-A** engine の 50 ms 同期応答依存（#102 / #134）
- **H-B** 45.4 MiB 辞書 mapping の trim と再フォールト（#107）。hard fault 比率は未測定。
- **H-C** 複数ホスト同時の配置通信（#142）
- **H-D** learning mutex の構造的危険（R1）。実機データでは 1777 ms を説明しない。

以下 2 件は Phase 0 の静的読取りで新たに判明した。**コード形状として確認済み・実測は未実施**。
原因と断定しない。

### H-E: `Shared::dynamic_runtimes` の process-wide mutex が全リクエストの前段にある

`server.rs:1725` は **すべてのリクエスト**について `runtime_services()` を呼び、
その中で `self.dynamic_runtimes.lock()`（`server.rs:267`）を取る。これは
Data / Renderer / Control の**全 pipe instance スレッドが共有する1本の mutex**である。
`server.rs:1739-1740` のコメント自身が
「Configuration/runtime locks may have delayed this request since the first check」
と認めており、この経路が要求を遅延させ得ることは設計上想定されている。

保持者が長い作業をした場合、待っていた全スレッドが**その完了と同時に解放される**。
ロック内で初回だけ実行され得るのは次の3つで、いずれも無予算である。

- developer-mode 初回有効化: `InputHistoryService::open`（#102 の実測で 14.8 MB のストアに対し復号1パスだけで **14.8 秒**）
- prediction 初回有効化: `PredictionRuntime::start_with_learning`（worker thread 起動）
- neural reranker 初回有効化: `LongConversionRuntime::discover`（**子プロセス spawn**）

**#107 が観測した「`ui-placement` の burst が tids=4 で +0 ms（別々の4スレッドが同一ミリ秒）」
「計算のない処理まで巻き込む」という形は、共有 mutex の一斉解放と整合する。**
H-B を採る前に、この経路を排除する必要がある。

検証方法（Phase 2–4）:
`dynamic_runtimes` の取得待ち時間を content-free に計測し、burst 時刻と突き合わせる。
ETW の Context Switch / ReadyThread で、同一 lock を待っていたスレッドが同時に Ready に
なっているかを確認する。

### H-F: developer mode が key path へ毎キー 4 回以上のファイル syscall を追加する

`debug_trace::emit`（`debug_trace.rs:66-73`）は毎回:

1. `std::env::var_os("LOCALAPPDATA")` + `PathBuf` join（**アロケーション**）
2. `fs::create_dir_all(parent)`（`debug_trace.rs:82`）
3. `OpenOptions::new().create(true).append(true).open(path)`（`debug_trace.rs:107`）
4. `file.metadata()?.len()`（`debug_trace.rs:108`）
5. `file.write(...)`（`debug_trace.rs:120`）

を実行する。ハンドルもパスもキャッシュしない。これが
`server.rs:1756-1759` から **`Reply::Output` を返す全キーの encode 直前**
（= client の 50 ms 予算の内側）で走る。有効化は環境変数だけでなく、
**developer-mode 入力履歴が開いた時点で暗黙に `set_enabled(true)`**
される（`server.rs:341`）。

**交絡としての重要性**: #102 / #107 / #141 / #142 の診断データは、いずれも
developer mode を有効にして収集されている。したがってそれらの timeout 記録は
**計測器自身が key path へ追加したファイル I/O を含んだ窓**を測っている。
I/O 圧力下ではこの寄与が支配的になり得る。

**これは既存 Issue の観測値を無効にするものではない。** ただし
「developer mode 有効時の key path」と「通常利用時の key path」を
別物として扱う必要がある。#148 の stress harness は両方を測る。

検証方法（Phase 1–2）:
`emit_at` の1回あたりコストを idle / I/O 圧力下で直接計測し、
developer mode on/off で key round trip の p50/p95/p99 を比較する。

## 5. 実施済み / 未実施

| 項目 | 状態 |
|---|---|
| Issue 整理・親 Issue 作成（#148） | 実施済み |
| baseline HEAD 固定 | 実施済み |
| TSF 側キー経路マップ | 実施済み（静的読取り、§2） |
| engine 側キー経路マップ | 実施済み（静的読取り、§3） |
| correlation ID / timing 計装 | **未実施** |
| deterministic delay injection | **未実施** |
| stress harness | **未実施** |
| ETW 因果確定 | **未実施** |
| PBT / mutation / TLC | **未実施** |
| 実ホスト E2E | **未実施** |

本文書の §2 / §3 はすべて静的コード読取りであり、
**実行時計測・ETW・stress は一切実施していない**。
H-A〜H-F はいずれも仮説であり、原因確定ではない。
