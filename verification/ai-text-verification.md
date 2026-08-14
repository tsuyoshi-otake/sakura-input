# GPT-5.6 Luna文章変換の検証・残存リスク監査

検証日: 2026-08-15  
対象: Issue #58、`gpt-5.6-luna`固定の文章変換・選択文字列校正

## 独立テストオラクル

期待値は実装の関数や分岐を写さず、次の外部契約から定義した。

1. 許可された要求は、明示操作、確定済みNormal scope、非空かつ上限内のsource、空いているglobal capacityをすべて満たす。
2. 同一owner/session/jobだけが結果の取得・取消を所有し、全結果は一度だけ終端する。
3. host textを書き換えられるのは、成功結果、同一focus/context/range/source/scope、未失効lifecycleが同時に成立するときだけである。
4. wireはexact model、Responses、`store:false`、上限付きUTF-8を要求し、失敗statusにresult textが同居してはならない。
5. cancel後もworker process終了まではglobal capacityを保持し、重複操作・キーrepeatは新規要求を作らない。

具体例テストは空、ちょうど上限、上限+1、不正magic/version/UTF-8、成功、各失敗status、owner/session不一致、cancel、cooldown、capacityを固定期待値で検査する。PBT相当の決定的総当たりでは、operation/provider/auth/style/effort/tierの直積2,268通りを独立したwire decoderで復号し、列挙値と境界を照合した。

## API結合と重要状態遷移

ローカルfake Responses serverを実worker境界へ接続し、POST path、Bearer header、exact model、`store:false`、source body、usageの復号を検査した。429は1回だけ再試行して成功しattempts=2、400は再試行せず終端する。malformed、empty、oversized output、missing final message、CLI failure分類、timeout/cancel、duplicate/capacity、owner/session不一致、stale selection/focus/lifecycleはunit/integrationまたはTLCで検査した。

順序逆転はjob identityとsource/range再照合により、古い結果を適用しない。失敗後のcapacity回復と、取消済みworkerが終了するまでcapacityを保持する遷移を分けている。永続化境界は既存DPAPI履歴のtest dependencyを使い、AI terminal recordとrequest/attempt/token aggregateのencode/decodeを検査した。

## C2（条件・分岐網羅）

nightly Rustのbranch instrumentationを使い、対象6 packageを実測した。全対象package合計は2,156/4,682 branches、46.05%である。AI機能に近い主要ファイルは次のとおり。

| 対象 | covered / branches | C2 |
|---|---:|---:|
| AI binary protocol | 24 / 28 | 85.71% |
| engine AI lifecycle | 40 / 74 | 54.05% |
| Responses/WinHTTP boundary | 50 / 90 | 55.56% |
| Codex CLI boundary | 14 / 24 | 58.33% |
| user preferences/Credential boundary | 3 / 14 | 21.43% |
| TSF text service全体 | 178 / 862 | 20.65% |

TSFはCOM host依存分岐を大量に含むため、20.65%を「AI経路の網羅率」とはみなさない。未到達の主要領域は実Codex child process、実WinHTTP timeout、Credential ManagerのOS失敗、実アプリのCOM selection/edit-session拒否、UI menu描画失敗である。測定artifactは`.codex/goal-loop/luna-text-tools/coverage-final.json`。

## ミューテーションテスト

独立wire oracle追加前のAI protocolは58 mutants中32 caught、21 missed、5 unviableだった。境界・列挙直積・失敗/result排他のoracleを追加後、58中53 caught、5 unviable、viable mutant検出率100%となった。unviableは型またはコンパイル制約で成立しない変異であり、検出成功へ算入していない。

engine lifecycleの追加sampleはbaseline後5 mutantsを実行し5 caughtだったが、全mutant実行は時間上限で終了した。したがってengine全体のmutation scoreは主張しない。最終protocol artifactは`.codex/goal-loop/luna-text-tools/mutants-ai-proto-final`、engine partial artifactは同`mutants-engine-final`にある。

## TLA+ / TLC

`verification/tla/AiTextLifecycle.tla`は実装の型・関数を参照せず、actor、latch、global job、detached capacity、deadline、focus/scope/source、terminal result、applyを状態として定義する。Safety（最大1 job、fresh resultだけをapply、terminalの再適用禁止）、Liveness（公平なpoll下でaccepted requestが終端）、deadlock freedom、success/failure/timeout/cancel/recoveryの到達可能性を検査した。

| config | 主な境界 | generated states | distinct states | depth | 結果 |
|---|---|---:|---:|---:|---|
| small | actors=2、通常deadline | 498,067 | 96,552 | 19 | error/deadlockなし |
| boundary | actors=2、deadline=0 | 383,353 | 72,920 | 15 | error/deadlockなし |
| concurrent | actors=3、競合・順序逆転 | 2,015,093 | 380,840 | 16 | error/deadlockなし |

探索は各cfgの`MaxRevision`、`MaxRequests`、`MaxPresses`と有限actor数に制限される。公平性は`PollTerminal`へ仮定しており、OS scheduler、WinHTTP、Codex CLIが現実に必ず進行する証明ではない。無限文字列、hash collision、unbounded retry、process crash内部、COM reference lifetime、複数Windows sessionはモデル化していない。

## 敵対的対応監査

| 重要条件 | 具体例/PBT | TLC | 実装境界 | 判定 |
|---|---|---|---|---|
| global max 1 / duplicate / repeat | lifecycle examples | `AtMostOneJob`、latch | engine service、TSF key-up latch | 自動検証済み |
| focus/source/range変更 | identity examples | fresh-source/focus safety | TSF再読・exact range | 純粋遷移済み、実host未確認 |
| retry/429/400/recovery | fake server | failure/recovery reachability | worker bounded retry | 自動検証済み |
| timeout/cancel/detached worker | state examples | deadline/cancel/liveness | engine capacity ownership、Job object | mock/TLC済み、実slow processは未確認 |
| protocol境界・enum・失敗排他 | 2,268直積、境界例 | abstract terminal | bounded binary protocol | mutation 100% viable caught |
| API key秘匿 | request/debug examples | 非モデル化 | Credential Manager、stdin、zeroize | 静的/unit済み、OS障害注入なし |
| history除外・metrics | encode/decode examples | scope safety | encrypted history入口 | 自動検証済み |
| selected-text COM apply | source identity oracle | stale/reordered safety | eventual write cookieで再読 | 実アプリ結合未確認 |

## 未検証・仮定・残存リスク

- 有料OpenAI API、ChatGPT Subscriptionへの実送信は行っていない。exact modelのアカウント可用性、provider固有のResponses互換性、Codex CLIの将来のJSONL形式変更は本検証外である。
- Azure/AWS/Cloudflare presetは、ユーザーが指定するEndpointがOpenAI Responses互換で、Bearerまたは`api-key`認証であることを仮定する。AWS SigV4などprovider固有署名は実装していない。
- VS Code、Word、メモ帳等で実際に選択して変換・校正する目視/COM結合試験、IME再インストール試験は未実施である。host固有のselection clone/edit-session挙動が最大の残存リスクである。
- C2が低いOS/COM/Credential分岐、engine lifecycle全体の未完了mutation run、TLCの有限境界外は欠陥検出力の空白として残る。
- developer historyはsource/resultを保存する明示的opt-in機能である。DPAPIと既存除外を維持するが、開発者モード利用者はexport先の管理責任を持つ。

以上から、pure protocolとbounded request contractの欠陥検出力は高い。非同期安全性は例/PBT/TLCが同じ不変条件を別表現で検査している。一方、実host COM結合と実provider疎通は未検証であり、リリース前dogfoodの優先対象と評価する。

## Repository acceptance

- `cargo fmt --all -- --check`: 成功
- 変更対象crateのClippy（all targets、TSF/engineはlib）: 警告ゼロ
- `cargo test --workspace`: 1,125 passed / 43 ignored
- `cargo build --workspace --release`: 成功（`sakura_ai_worker.exe`を含む）
- Inno Setup 6.7.3: `sakura_setup.exe`生成成功、AI workerの圧縮・監査成功
- test/build/TLC/Java process survivor: なし

workspace全体の`clippy --all-targets -D warnings`は、変更していない`shift_ascii_space_tests.rs`の`needless_range_loop`と`engine_recovery_tests.rs`の`indexing_slicing`で失敗する。Issue #58の変更対象では同じ条件を通過しており、既存lintを便乗修正しなかった。
