# write_coordinator TLA+モデル

`crates/sakura-tsf/src/write_coordinator.rs`（revision 0e766fd、1.0.3、SHA-256 `0f485885d6d172f568e5430ce9bf59c084bebbc9c38c9fb2de5488dbe135b31e`）の抽象プロトコルモデルです。Issue #52の形式検証で使用したもので、activation／focus世代、context識別、committed／tail revision、有界journal（Reserved→Ready→Requested）、ticket発行・検証、UI lease発行・採用をモデル化しています。

deadnessはtrace層のoracleです。epochイベント（activate、deactivate、focus変更、context置換、revision bump）は`KillTickets`／`KillLeases`で発行済みticket・leaseをすべてdeadにし、安全性性質は「dead ticketは検証を通らない」「dead leaseは採用されない」を要求します。

## 実行方法

TLA+ tools（tla2tools.jar、TLC2 2.19）とJavaが必要です。この開発機ではグローバルに導入済みで、`tlc`コマンドが使えます（`%LOCALAPPDATA%\Programs\TLAplus\tlc.cmd`）。手動で実行する場合：

```powershell
# 全invariant（NoStaleAdoptを含む）— lease epoch欠陥により違反traceが出る（想定どおり）
tlc -config WriteCoordinator_all.cfg WriteCoordinator.tla

# 安全性invariantのみ（NoStaleAdoptを除く）— 完走してPASSする
tlc -config WriteCoordinator_safe.cfg WriteCoordinator.tla
```

bounds：`Contexts = {1, 2}`、`Cap = 2`、`MaxRev = 2`、`MaxGen = 2`、`MaxId = 3`、`CONSTRAINT Constraint`。

## 記録済みのTLC結果（2026-08-14、TLC2 2.19、OpenJDK 11）

- `WriteCoordinator_all.cfg`：`NoStaleAdopt`違反。11ステップの反例trace（distinct states 2,920で検出）。context A→B→Aの往復後、`ObserveReplace`が`committed_revision`を0へ戻すのにactivation／focus世代を進めないため、旧context Aのdead leaseが`Adopt`の比較を通る。Issue #52のF2と同型。
- `WriteCoordinator_safe.cfg`：全invariant PASS。distinct states 893,961、max depth 32で状態空間を完走。

## モデルの既知の限界（Issue #52レビューで確定）

このモデルは実装の`complete_applied`／`reject`境界を検証していません。`CompleteApplied`／`RejectHead`は`"req"`位相と`ValidateTicket`を前提条件にしており、実装（`complete_applied`はticketを検証せず`finish_head`へ進む。`cancel_from`の非Cancelledガードは`Reserved`しか遮断しない）より強い抽象です。したがって`TicketSafety`のPASSは、Ready頭部が要求なしにApplied／Rejected終端できる欠陥面（Issue #52のF1とその一般化）をカバーしません。F1はRust側bounded checkerのprobeで検出されたものです。

また`validate_ui_lease`相当のactionはなく、TLCが検出するのはdead leaseの採用（F2相当）だけです。F3（dead leaseの検証通過）とF4（同revision新旧leaseの再採用）はモデル外で、Rust checkerが検出しています。

検証の全容・発見F1〜F4・訂正記録はIssue #52を参照してください。
