# Sakura Input エージェント指向リファクタリング計画

- 基準コミット：`a370477`（origin/main、2026-09-12、v1.0.39、PR #151 merge）
- 追跡 Issue：[#152](https://github.com/tsuyoshi-otake/sakura-input/issues/152)
- 根拠資料：`docs/architecture/analysis/{engine,core-proto,tsf,contracts-dictc-tools,docs-verification-ci,renderer-settings}.md`（HEAD a370477 に対する静的解析 6 本。本書の行番号・件数はすべてそこから引用）
- 本書の位置づけ：**計画書**。コード変更は含まない。各ステップは「1 PR ＝ エージェント 1 セッションで読解・変更・検証が完結する粒度」に切ってある。

---

## 0. 要旨（この 1 画面だけ読めば方針が分かる）

**目的**：1 つの Issue を直すとき、エージェントが読むコード・文書・テストを最小化する。Clean Architecture の形に合わせることが目的ではない。

**現状の要点**（詳細は §2）

| 観点 | 事実 |
|---|---|
| god file | 16 ファイルが 1,900 行超。最大は `crates/sakura-engine/src/dispatch.rs` 16,850 行（production 6,409＋テスト 10,396）、`crates/sakura-tsf/src/text_service.rs` 9,192 行 |
| テスト同居 | 大きいファイルの 30〜60% がインラインテスト。`cargo test -p sakura-renderer --lib` は `lib.rs` が無く存在しない |
| 依存の逆流 | `sakura-core → sakura-proto`（18 参照、すべて値型）、`sakura-settings → sakura-engine`（2 ファイル形式を読むためだけ）、`renderer::indicator → candidate`、`pad ⇄ pad_rail` |
| 隠れた結合 | reranker protocol が 2 crate に手書き重複（`neural-worker/src/protocol.rs:3-8`、`engine/src/long_conversion.rs:28-29`）。`TextService` の `RefCell` フィールドが 40 箇所から borrow |
| 正当性ゲート | `cargo test --workspace` 1 本（`ci.yml:80`、記録上 6m58s）。DLL 1 MiB ゲートも TLC 実行も CI に無い |
| 文書 | Issue 1 件あたり必読散文の中央値 ~170 KB。`CLAUDE.md` 40,118 B のうち約 10 KB が停止中調査、コード行番号引用 5/5 が stale |
| 形式検証 | TLA+ 10 モデル／48 cfg あるが Rust との対応は散文のみ、`traceability.json` は自己申告 STALE |

**方針**（詳細は §3）

1. **テストを本体から出す**（sibling `*_tests.rs`）。挙動不変・最小リスクで、読解量を 40〜60% 減らす。最初にやる。
2. **葉 crate を 5 つ切る**（`sakura-values`、`sakura-store`、`sakura-rerank-proto`、`sakura-oracles`、`sakura-context`）。依存の逆流を全て消し、`rg` 1 行で検査できる方向規則にする。
3. **god file を「変更理由」で分割**する。行数ではなく Issue の型で切る。分割後の各ディレクトリに「この Issue 型はここ」の対応表を置く。
4. **crate 単位の CI ゲート**（paths filter matrix、DLL サイズ、TLC、verification⇄Rust 対応検査）を足し、モジュールを独立に検証可能にする。
5. **文書を「契約」「判断」「履歴」に分け**、`CLAUDE.md` を 8 KB 以下、crate ごとに README ≤4 KB を置く。
6. **形式検証境界を実ディレクトリにする**：`sakura-tsf/src/session/` に `use windows` を禁止し、TLA+ モデルと 1:1 対応する `correspondence.json` を CI で検査する。

**順序**：Phase 0（ゲート整備）→ 1（テスト分離）→ 2（葉 crate）→ 3（core/proto/ipc/reg/dictc 分割）→ 4（engine）→ 5（renderer/settings）→ 6（TSF）。Phase 7（文書・verification）は Phase 0 から並走。TSF の write journal／candidate teardown に触る 3 工程は **#7 のクラッシュ証拠が取れるまで着手しない**（§6）。

**規模**：PR 約 70 本。Phase 1〜2 だけで読解セットの削減幅の 6 割が得られる（§5）。

---

## 1. 目的と評価軸

12 原則を、PR ごとに機械検査できる指標へ落とす。以後の各ステップの `Verify:` はこの表の指標を使う。

| 原則 | 指標（測り方） | 現状 | 目標 |
|---|---|---|---|
| High Cohesion | 1 ファイルの「変更理由」数（直近 150 commit で同一ファイルを触った Issue の種類数） | `dispatch.rs` 8 種、`conversion.rs` 8 種、`input_history.rs` 13 種 | ファイルあたり ≤2 種 |
| Low Coupling | crate 間 `use` 辺の数、モジュール内 `RefCell`/`Cell` 共有フィールドの borrow 箇所数 | `text_service.rs` の `deferred` 40 箇所 | 共有フィールドは 1 owner モジュールに閉じる |
| Single Responsibility | 1 関数の行数と分岐数 | `apply_action` 431 行／45 arm、`serve` 365 行 | 関数 ≤120 行、分岐は表引き |
| One-way Dependency | `rg` で検査する禁止 import 規則（§3.3）の違反件数 | 逆流 4 系統、循環 3 組 | 違反 0（CI で検査） |
| Change Locality | Issue 1 件で触る crate 数・ファイル数（過去 Issue を再演） | #109 で 3 crate、#57 で 8 ファイル | 1 crate、≤3 ファイル |
| Build/Test Isolation | `cargo test -p <crate> --lib` が単独で意味を持つ crate の数 | 14/15（renderer に lib 無し）、CI は workspace 1 本 | 15/15、変更 crate だけ走る matrix |
| Explicit Contracts | 契約ごとの単一文書と単一 owner crate | 18 契約中 2 件が文書なし、reranker protocol が 2 箇所 | `docs/contracts/` に 1 契約 1 文書、owner crate 1 つ |
| Small Agent Context | Issue 型別の必読セット（行数・bytes、§5） | 中央値 ~170 KB 散文 + 数千〜2 万行 | 散文 ≤25 KB、コード ≤1,500 行 |
| Independently Testable | 純粋ロジックが Win32/COM なしで走るテスト数 | TSF は `composition.rs` の 23 本のみ模範 | `session/` 全体、renderer `layout`、settings `model` |
| Independently Refactorable | 公開 API 面（`pub` item 数）と `pub(crate)` 化率 | core facade 113 再輸出、~28 未使用 | 未使用再輸出 0、crate 境界の `pub` は README に列挙 |
| Formal Verification Boundary | `correspondence.json` の Rust シンボル解決率 | 0%（`traceability.json` STALE） | 100%、CI で検査 |
| 誤認しにくい構造 | Issue 型 → ディレクトリ対応表（§3.7）の一意性 | 無し | 全 Issue 型に 1 ディレクトリ |

---

## 2. 現状の構造的問題（証拠付き）

### 2.1 workspace

`Cargo.toml` members 16（crates 15＋`tools/ime-eval`）。`tools/candidate-snapshot`・`tools/candidate-sweep` は独自 `[workspace]` で、`ci/dep-policy.ps1` の `Get-WorkspaceCrateName`（:131〜）の監査対象外。プロセス境界は 4 つ（TSF DLL、engine、renderer、settings）＋ worker 2 つ。

### 2.2 god file 一覧

| ファイル | 総行数 | production | 同居する変更理由 | 出典 |
|---|---:|---:|---|---|
| `sakura-engine/src/dispatch.rs` | 16,850 | 6,409 | キー→Action、edit、変換、候補、確定、render、mode、テスト 168 本 | engine §3 |
| `sakura-tsf/src/text_service.rs` | 9,192 | 6,939 | 10 COM interface（vtable 572 行 :6367-6938）＋調整 4,808 行＋fake engine 5 種 | tsf §3 |
| `sakura-core/src/conversion.rs` | 7,405 | 5,009 | lattice/Viterbi、候補組立、ranking、特殊候補、evidence(#108)、repair、bridge、raw repair | core §3 |
| `sakura-settings/src/ui.rs` | 4,999 | — | `App` 33 field、`GeneralControls` 68 HWND、`window_procedure` 322 行、enum 写像 :3397-3853 | renderer-settings §4 |
| `sakura-renderer/src/pad.rs` | 3,752 | — | `layout` :243-662（純粋 420 行）、`pad_procedure` :2833（295 行）、GDI | renderer-settings §3 |
| `sakura-engine/src/input_history.rs` | 3,632 | 1,909 | 13 変更理由（record、codec、DPAPI、queue、compaction、stats、scope filter…） | engine §3 |
| `sakura-renderer/src/candidate.rs` | 3,432 | — | 純粋 layout／placement と GDI paint（seam は綺麗）、`text_width_96` :1846 と `theme::text_width` :285 が二重権威 | renderer-settings §3 |
| `sakura-core/src/dictionary.rs` | 2,671 | — | `image_format` :20-103、`Dictionary::parse` 249 行、validate ~500 行、LOUDS、lookup、detail | core §3 |
| `sakura-tsf/src/engine.rs` | 2,572 | — | pipe transport ＋ request wrapper ＋ recovery | tsf §3 |
| `sakura-settings/src/updater.rs` | 2,422 | — | 4 trait 背後に flow/http/digest/authenticode/installer | renderer-settings §4 |
| `sakura-settings/src/update_trust.rs` | — | — | 契約 :118-790 と状態 :792-1162 が同居（#150 の修正箇所） | renderer-settings §4 |
| `dictc/src/lib.rs` | 1,987 | 1,987 | インラインテスト 0、直近 150 commit 中 18 で変更（最高 churn）、19% が 1 binary のため | contracts §4 |
| `sakura-engine/src/server.rs` | — | — | `serve` 365 行（:1519-1883）、`Server` constructor 12 個 | engine §3 |
| `sakura-ipc/src/security.rs` | 1,350 | — | admission（:88-141）と server trust #104（:151-320, 453-513）が同居 | contracts §3 |
| `sakura-engine/src/session.rs` | — | — | `Session` ~148 field／92 accessor | engine §3 |

### 2.3 依存の方向違反・循環

| 違反 | 証拠 | 影響 |
|---|---|---|
| `sakura-core → sakura-proto` | 18 参照、すべて `FixedStr/FixedVec/Overflow`、`Mode/KeyCode/KeyInput/Modifiers/AppearanceTheme/PadShortcut`、上限定数 3 つ。wire 型は 0 | core を触ると proto の codec テストまで読解圏に入る |
| `sakura-settings → sakura-engine` | `settings/src/learning.rs:6`、`input_history.rs:8`。engine の record/codec を読むためだけ | settings の CLI 変更で engine 全体が build 対象 |
| `renderer::indicator → candidate` | `indicator.rs:57,76,261,573`（定数 3 つと `monitor_work_area`） | indicator の Issue で candidate.rs 3,432 行が読解圏 |
| `renderer::pad ⇄ pad_rail` | `pad_rail.rs:351` の `dpi_of` 1 関数 | 循環 |
| `tsf::exports ⇄ class_factory` | tsf §2 | 循環 |
| テスト専用循環 | `core/preferences.rs:1609,1612 → input_repair`、`core/simd.rs:1761,1899 → width` | 分割時の障害 |
| `settings/ui/{pages,tabs,presentation}` | #144 が `use super::*` を 3 本残した | 分割したのに境界が無い |

### 2.4 隠れた結合

- **reranker protocol の二重定義**：`SKNR`/`SKNS` v1 magic（`0x524e_4b53`/`0x534e_4b53`）が `neural-worker/src/protocol.rs:3-8` と `engine/src/long_conversion.rs:28-29` に手書き。所有 crate なし。一方 `sakura-neural-proto`（`SCV1`）は休眠中で、名前が実態と逆。
- **`TextService` の共有フィールド**：`RefCell` 11／`Cell` 22。`deferred` 40 箇所、`writes`・`engine` 各 29 箇所、`composition` 27 箇所から borrow。再入排他は 5 つの裸 `Cell` bit（`text_service.rs:1000-1024`、`candidate_operation_active` :1010 ほか）に分散。`use` 辺には現れない結合で、#7 のハザード面そのもの。
- **`sakura-reg` の 4 変更理由**：COM 登録、installer maintenance、VS Code 診断、Credential Manager（`user_preferences.rs:345-436`）。`lib.rs:1-9` の「input path では動かない」は `tsf/text_service.rs:68` で既に偽。
- **engine の dev-deps**：`dictc`（112 call site、すべて in-memory `dictc::compile`）と `ime-eval`（engine src からは未使用、`tests/pipe_round_trip.rs:36-37` のみ）。engine のテストを走らせるだけで dictc 全体が build 対象。
- **休眠コード**：engine の #34 系 4 モジュール（`context_baseline/evaluation/intelligence`＋α、1,846 行）は常に compile され、oracle 12 モジュールも `lib.rs:66` で常時 compile。

### 2.5 テストの位置と CI ゲート

- `src/` 直下のテスト専用モジュール：engine 12（`*_tests.rs`、`*_oracle.rs`）、tsf `engine_recovery_tests.rs`（432 行）。production と同列に並ぶため、エージェントが変更対象を誤認する。
- `#[ignore]` 95 本、CI が opt-in するのは 4 本。settings の統合テスト 28 本中 27 本が ignore。
- 正当性ゲートは `cargo test --workspace` 1 本（`ci.yml:80`）。crate 単位のジョブが無い（#89）。DLL 1 MiB 上限の検査は未起動の `scripts/verify-phase1.ps1:258` にしか無い。TLC は `verify-space-key-dispatch-tlc.ps1 -SelfTest`（`ci.yml:76`）のみで、モデル自体は 1 つも走らない。

### 2.6 文書と形式検証

- `CLAUDE.md` 40,118 B。§最優先タスク 10,147 B は 2026-08-02 の停止中調査。コード引用 5/5 が stale（`dispatch.rs:1039` → 実際 `:3435`、`keymap.rs:1295`、「496 passed / 12 ignored」→ 現在 1,988 tests、build ID `1f5ca43e59d305b6`）。`apply_patch`（Codex 専用）を必須にし、`:103` で SolAdvisor 必須・`:42` で禁止と矛盾。
- `DESIGN.md` 100,119 B。crate README 0/15。`AGENTS.md` 不在（`C:/Codes/CLAUDE.md` は `@AGENTS.md` を期待）。
- `verification/` 187 ファイル 705,796 B。flat な `<topic>.md` 23 本＋`-results.json` 15 本＋機能ディレクトリ 8 本＋`tla/` 59 ファイルが混在。`.gitignore:24` が `/verification/*/tlc/` を無視しつつ 3 つが tracked。
- 機微スコープ規則（Password/URL/Email/Digits 除外）が 10 箇所に再掲。
- `verification/tsf-probe-host-insert/tla-record.md:24` が存在しない `Link::desynchronized` を引用（実名 `Engine::is_desynchronized`、`tsf/src/engine.rs:812`）。

### 2.7 Issue 1 件あたりの読解セット（現状）

| Issue 型 | 例 | 必読コード（行） | 必読散文（bytes） |
|---|---|---:|---:|
| `無変換` モード循環 | #51 | ~19,600 | ~170,000 |
| 候補順序／ranking | #94, #108 | ~20,900 | ~170,000 |
| 再入 candidate UI クラッシュ | #7 | ~9,700 | ~127,000 |
| stale composition | #57 | ~8,230 | ~127,000 |
| Dual-TSF キー調停 | #69 | ~7,400 | ~127,000 |
| 句読点候補 | #99 | ~11,300 | ~170,000 |
| 辞書イメージ形式 | #109 | ~5,020 | ~215,000 |
| 開発者履歴 | #113–#132 | ~7,000 | ~265,000 |
| 更新 trust state | #150 | ~2,990 | ~215,000 |

散文 ~85,770 B（`CLAUDE.md`＋`.claude/memory/rules.md`）はどの Issue でも無条件に読まれる。

---

## 3. 目標構造

### 3.1 crate 図（依存は上→下のみ）

```
                 ┌──────────────────────────────────────────┐
 process 境界    │ sakura-tsf (dll)  sakura-engine  sakura-renderer  sakura-settings │
                 │ sakura-neural-worker  sakura-ai-worker  sakura-regtool  sakura-logon │
                 └──────────────────────────────────────────┘
                     │          │            │              │
 契約/転送      sakura-proto  sakura-ipc  sakura-rerank-proto  sakura-ai-proto  sakura-context-proto(旧 neural-proto)
                     │          │
 領域            sakura-core  sakura-store  sakura-context  sakura-reg(識別子のみ)  sakura-user-prefs  sakura-install-maintenance
                     │          │
 葉              sakura-values (FixedStr/FixedVec/Overflow, capacity, Mode/KeyCode/KeyInput/Modifiers, AppearanceTheme/PadShortcut, Fingerprint)
 dev only        sakura-oracles (engine の TLA+ oracle 12 モジュール)   dictc-core / dictc(bin)   tools/*
```

新規 crate と優先度：

| crate | 中身 | 由来 | 優先 |
|---|---|---|---|
| `sakura-values` | `fixed.rs`（FixedStr/FixedVec/Overflow）、`capacity.rs`（上限定数）、`input.rs`（Mode/KeyCode/KeyInput/Modifiers）、`appearance.rs`（AppearanceTheme/PadShortcut）、`fingerprint.rs`（候補 fingerprint） | core §7 の `sakura-values` と contracts §6 の `sakura-limits` を統合。名前は `sakura-values` に統一 | 必須 |
| `sakura-store` | learning／input_history の record・codec・DPAPI・snapshot reader | engine §7、renderer-settings §5、contracts §6 の `sakura-stores` を統合。名前は `sakura-store` に統一 | 必須 |
| `sakura-rerank-proto` | `SKNR`/`SKNS` v1（magic、frame、bounds） | contracts §6 | 必須 |
| `sakura-oracles` | engine の `*_oracle.rs` 12 モジュール（dev-dependency のみ） | engine §7 | 推奨 |
| `sakura-context` | #34 休眠 4 モジュール 1,846 行 | engine §7 | 推奨 |
| `sakura-context-proto` | `sakura-neural-proto`（SCV1）の改名 | contracts §6 | 任意 |
| `sakura-user-prefs` | Credential Manager＋user preference（reg から） | contracts §6 | 推奨 |
| `sakura-install-maintenance` | maintenance／launcher／vscode_diagnostics（reg から） | contracts §6 | 推奨 |
| `dictc-core` | dictc lib の compile／image writer。`dictc` は bin だけの薄い crate に | contracts §6 | 推奨 |
| `sakura-diagnostics` | ipc の `debug_trace.rs`／`diagnostics.rs`（pipe と無関係） | contracts §6 | 任意 |

### 3.2 crate ごとの目標モジュールツリー

規約：ディレクトリ名＝業務語。`mod.rs` は再輸出と 20 行以内の説明のみ。テストは `<name>_tests.rs` を同階層に置き、`#[cfg(test)] #[path = "<name>_tests.rs"] mod tests;` で繋ぐ。`util`／`misc`／`common` は禁止。

#### sakura-core

```
src/
  lib.rs                 (facade: 未使用再輸出を削除し ≤85 item)
  conversion/{options,input,result,evidence,repair_plan,bridge,raw_repair}.rs
  conversion/search/     (lattice, viterbi)
  conversion/candidates/ (assembly, dedup)
  conversion/ranking/    (cost, tie_break, #94/#108 がここ)
  conversion/synthesis/  (numerals, calendar, punctuation → #99 はここ)
  dictionary/{format,parse,validate,louds,lookup,detail}.rs   (format.rs = image_format の唯一定義、#109 はここ)
  width/{policy,punctuation,chars,normalize}.rs, width/scan/ (simd)
  romaji/{table,fsm,replay,completion}.rs                      (#56 はここ)
  keymap/{vocabulary,config}.rs
  preferences/{model,parse,serialize,profiles}.rs
  editing.rs  text.rs  user_dictionary.rs  input_repair.rs  cpu.rs
```

#### sakura-proto

```
src/
  lib.rs      (PROTOCOL_VERSION: u16 = 22 はここに残す。値は変えない)
  message/{tags,header,request,response,ui_state}.rs
  codec.rs    (encode/decode)
  wire.rs     (owner 判断：values に降ろすか proto に残すか。§7 D3)
```

#### sakura-engine

```
src/
  lib.rs main.rs
  ipc/        (pipe accept、admission 呼び出し)
  request/    (Request → 内部 command 変換)
  keys/       (Action ごとの handler。apply_action は表引きの dispatcher ≤120 行。mode_kana_cycle.rs が #51 の場所)
  edit/       (preedit 編集)
  conversion/ (core::conversion 呼び出しと segment 状態)
  candidates/ (候補一覧、ページ、選択)
  predict/    (prediction, prediction_snapshot)
  commit/     (確定と学習通知)
  render/     (ui.rs の描画状態組立)
  state/      (Session と mode。**上位モジュールを import しない**)
  services/   (EngineServices builder、learning/history/ai_text/long_conversion の起動)
  runtime/    (server.rs の serve を accept/dispatch/shutdown に分割)
  fault_injection.rs  timing.rs  event_log.rs
```

#### sakura-tsf

```
src/
  lib.rs
  com/        (text_service.rs ≤1,300 行 = vtable と委譲のみ、class_factory, exports, live_objects,
               edit_session, display_attributes, reconversion, candidate_element)
  session/    (SessionController, key_arbitration, write_journal, write_pipeline, recovery_fence,
               callback_deadline, candidate_board, deferred_work, layout_lease, input_scope, ai_text)
              **`use windows` 禁止 = 形式検証境界。TLA+ 4 モデルと 1:1**
  host/       (composition, candidate_ui, language_bar, mode_assets, key_translation, wait_cursor,
               engine/{mod,link,requests}.rs)
  diagnostics/ring.rs
```

#### sakura-renderer

```
src/
  lib.rs (新設。main.rs は起動だけ)  theme.rs  screen.rs (dpi_of, monitor_work_area の唯一の場所)
  candidate/{mod,layout,placement,paint,overlay,window}.rs
  indicator/{mod,placement,paint}.rs
  pad/{mod,layout,list,rail,caption,gesture,icon,tooltip,storage,window}.rs
  input/ (raw_input)  watch/  accessibility.rs  glyph.rs
```

#### sakura-settings

```
src/
  lib.rs main.rs bootstrap.rs paths.rs storage.rs
  config/     (configuration, formats)
  history/    (sakura-store 経由。engine への依存なし)
  engine/     (engine_admin, engine_faults, engine_timing = engine admin 契約の client)
  update/{contract,trust_state,flow,http,digest,authenticode,installer}.rs  (#150 は trust_state.rs だけ)
  ui/{mod(≤600),presentation(葉),theme,tabs,model}.rs, ui/topics/<15 topic>.rs
  cli/{parse,run,render}.rs
```

#### sakura-ipc／sakura-reg／dictc

```
sakura-ipc/src/  transport.rs client.rs security/{admission,server_trust}.rs  (debug_trace/diagnostics は sakura-diagnostics へ)
sakura-reg/src/  guids.rs registry.rs com_server.rs module.rs profile.rs wide.rs  (識別子と登録だけ)
dictc-core/src/  compile/ image/ overlay/ detail/   dictc/src/bin/*.rs (11 bin。dictc lib を import しない 3 bin は tools/ へ)
```

### 3.3 `rg` で検査できる依存規則

`ci/check-dependency-rules.ps1` を新設し、以下を毎 PR で実行する。全行 **空出力が合格**。

| # | 規則 | コマンド |
|---|---|---|
| R1 | core は proto を知らない | `rg -n 'sakura_proto' crates/sakura-core/src` |
| R2 | settings は engine を知らない | `rg -n 'sakura_engine' crates/sakura-settings/src` |
| R3 | TSF session は Win32/COM を知らない | `rg -n 'use windows\|windows::' crates/sakura-tsf/src/session/` |
| R4 | engine state は上位を知らない | `rg -n 'use crate::(keys\|commit\|render\|ipc\|request\|services\|runtime)' crates/sakura-engine/src/state/` |
| R5 | renderer indicator は candidate を知らない | `rg -n 'crate::candidate' crates/sakura-renderer/src/indicator/` |
| R6 | pad_rail は pad を知らない | `rg -n 'crate::pad::' crates/sakura-renderer/src/pad/rail.rs` |
| R7 | settings ui presentation は葉 | `rg -n 'use (super\|crate)::' crates/sakura-settings/src/ui/presentation.rs` |
| R8 | rerank magic は 1 箇所 | `rg -n '0x524[eE]_4[bB]53' crates --glob '!**/tests/**'` → **1 件のみ**（`sakura-rerank-proto`。`neural-worker/tests/real_model_e2e.rs:9` の複製は除外） |
| R9 | test-only module は src に置かない | `rg -l --files-with-matches '^#!\[cfg\(test\)\]' crates/*/src` → `*_tests.rs` 以外 0 |
| R10 | 未使用 facade 再輸出なし | `cargo doc`＋`rg` は不可のため、`ci/check-facade.ps1` が `pub use` 各 item の被参照を `rg` で数える |
| R11 | tools workspace も dep-policy 対象 | `ci/dep-policy.ps1` が `tools/*/Cargo.lock` も読む |

### 3.4 テスト配置規約

1. インラインテストは **300 行を超えたら** sibling `*_tests.rs` に出す（300 行以下は同居可。行数だけで割らない原則に従う）。
2. テストしか含まないモジュール（oracle、`*_tests.rs`）は `src/` に置かず、`tests/` か `sakura-oracles` crate に置く。
3. `#[ignore]` は理由を属性文字列に書く（`#[ignore = "needs desktop session"]`）。CI の scheduled job で `--ignored` を走らせる（§3.6）。
4. fake／fixture は `tests/support/` または `<crate>/src/testing.rs`（`#[cfg(any(test, feature = "test-support"))]`）に 1 箇所。TSF の fake pipe engine 5 種はここに集約。

### 3.5 文書配置

```
AGENTS.md                (≤1 KB。読む順序と禁止事項だけ)
CLAUDE.md                (≤8 KB。テスト出力規約、owner 判断へのリンク、Issue型→ディレクトリ表)
DESIGN.md                (≤40 KB。不変条件と境界だけ。詳細は docs/architecture/ へ)
docs/architecture/{README,conversion,ui,packaging,agent-refactor-plan}.md
docs/contracts/{input-scope,ipc-v1,write-journal,candidate-ui,developer-history,update-trust,
                neural-rerank,ai-text,dependencies,test-output,config-format,engine-admin}.md
docs/decisions/          (owner 判断、append-only、1 判断 1 ファイル、日付プレフィックス)
docs/history/{releases,issues,research}/   (release-notes-*.md 36 本、停止中調査、研究ノート)
docs/runbooks/           (vscode-crash-diagnostics-runbook.md など手順書)
crates/<crate>/README.md (≤4 KB。責務、公開 API、この crate に来る Issue 型、禁止依存、テストコマンド)
```

機微スコープ規則は `docs/contracts/input-scope.md` を唯一の定義とし、他 9 箇所はリンクに置換する。

### 3.6 verification 再編と correspondence

```
verification/README.md
verification/<crate>/<feature>/
  model.tla  <feature>-<cfg>.cfg  record.md  results.json
  correspondence.json   {"model":"...","invariants":[...],
                         "rust":[{"symbol":"session::key_arbitration::admit","path":"crates/sakura-tsf/src/session/key_arbitration.rs"}],
                         "tests":["crates/sakura-tsf/src/session/key_arbitration_tests.rs"],
                         "verified_at":"a370477"}
  coverage/ mutants/ pbt/   (存在するものだけ)
verification/_historical/   (revalidation-*.md、issue-141-tdd.md など)
```

`ci/check-verification-correspondence.ps1`：各 `correspondence.json` の `path` が存在し、`symbol` の末尾識別子が `rg -n 'fn <ident>\|struct <ident>\|enum <ident>' <path>` で 1 件以上見つかることを検査。`-SelfTest` を持つ。187 ファイルは移動のみで内容不変（`git log --follow` を保つ）。

### 3.7 Issue 型 → 最初に開くディレクトリ（CLAUDE.md に載せる表）

| Issue 型 | ディレクトリ | 契約文書 | verification |
|---|---|---|---|
| キー動作・モード（`無変換`、Shift、Space） | `sakura-engine/src/keys/` | — | `verification/sakura-engine/{space-key-dispatch,shift-latin}` |
| 変換候補の順序・欠落 | `sakura-core/src/conversion/ranking/` | — | — |
| 句読点・数字・日付候補 | `sakura-core/src/conversion/synthesis/` | — | — |
| 辞書イメージ形式 | `sakura-core/src/dictionary/format.rs` ＋ `dictc-core/src/image/` | `docs/contracts/dictionary-image.md` | — |
| 候補ポップアップ表示 | `sakura-renderer/src/candidate/` | `docs/contracts/candidate-ui.md` | — |
| TSF クラッシュ・再入・stale | `sakura-tsf/src/session/` | `docs/contracts/write-journal.md` | `verification/sakura-tsf/*` |
| 開発者履歴・学習ストア | `sakura-store/` | `docs/contracts/developer-history.md` | `verification/sakura-store/*` |
| 更新・署名・trust | `sakura-settings/src/update/` | `docs/contracts/update-trust.md` | `verification/sakura-settings/update-signing-v2` |
| 設定 UI | `sakura-settings/src/ui/topics/` | — | — |
| reranker／AI text | `sakura-rerank-proto/`、`sakura-engine/src/services/` | `docs/contracts/{neural-rerank,ai-text}.md` | `verification/sakura-engine/ai-text` |
| CI／テスト規約 | `ci/`、`docs/contracts/test-output.md` | — | — |

---

## 4. 移行計画

各ステップの形式：**目的 → 変更 → Verify: → Expect:**。`Verify:` の cargo test は必ず `./ci/run-test-quiet.ps1 -Name '<name>' -Command { ... }` 経由。全ステップ共通の前提：`cargo fmt --all -- --check`、`git diff --check`、`./ci/check-process-clean.ps1` が成功。

規模の目安：S = 1 PR・エージェント 1 セッション（30〜90 分）、M = 1〜2 PR・半日、L = 2〜4 PR・1 日超。

### Phase 0：ゲート整備（コード移動なし、5 PR）

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 0.1 | DLL サイズゲート | `ci/check-dll-size.ps1`（`(Get-Item target/release/sakura_tsf.dll).Length -le 1048576`）を `ci.yml` の release build 後に追加 | `pwsh ./ci/check-dll-size.ps1 -SelfTest`；CI 実行 | SelfTest が超過ダミーで失敗・正常で成功。本番 DLL が閾値以下 | S |
| 0.2 | crate 単位テストの基準値記録 | 各 crate の `cargo test -p <crate> --lib` 件数を `ci/test-baseline.json` に記録（renderer は `--bin sakura_renderer`） | `pwsh ./ci/record-test-baseline.ps1` | 15 crate 分の件数が JSON に出る。合計 1,988 | S |
| 0.3 | 依存規則スクリプト | `ci/check-dependency-rules.ps1`（§3.3 の R1〜R9。現状違反する規則は `-Advisory` で警告のみ） | `pwsh ./ci/check-dependency-rules.ps1` | R8・R9・R11 以外は今は違反を列挙し exit 0。Phase 完了ごとに `-Advisory` を外す | S |
| 0.4 | 形式検証 workflow（非ブロッキング） | `.github/workflows/formal-verification.yml`：tla2tools.jar を SHA-256 固定で取得、10 モデル × 48 cfg を matrix、`continue-on-error: true`、1 cfg 20 分 timeout、`verify-space-key-dispatch-tlc.ps1` の process lifecycle を流用 | workflow_dispatch で実行 | 48 job が完走。既知の `-bug` cfg は反例を出して赤、それ以外は緑 | M |
| 0.5 | `AGENTS.md` と `CLAUDE.md` の縮約 | `AGENTS.md` 新設（≤1 KB）、`CLAUDE.md` から §最優先タスク（10,147 B）を `docs/history/issues/vscode-crash-investigation-20260802.md` へ移動、stale 行番号 5 箇所を修正、`apply_patch` 記述を削除、SolAdvisor 矛盾を owner 判断 D1 へ | `wc -c CLAUDE.md AGENTS.md`；`rg -n 'dispatch.rs:1039\|keymap.rs:1295\|496 passed\|apply_patch' CLAUDE.md` | ≤8,192 B／≤1,024 B。rg 空 | S（owner 承認要） |

### Phase 1：テスト分離（挙動不変、11 PR）

原則：production 行を 1 行も変えない。`git diff --stat` で production ファイルは削除行のみ。

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 1.1 | `dispatch.rs` のテスト分離 | 6455〜16850 行を `dispatch_tests.rs` へ。`record_conversion_lookup_for_test()`（:1927/:2017）は `#[cfg(test)] pub(crate)` のまま | `wc -l crates/sakura-engine/src/dispatch.rs`；`cargo test -p sakura-engine --lib dispatch` | 6,455 行。168 tests pass、件数不変 | S |
| 1.2 | engine 他の大モジュール | `input_history`、`server`、`session`、`prediction`、`learning` を同様に | `cargo test -p sakura-engine --lib` | 件数が 0.2 の基準と一致 | S |
| 1.3 | core | `conversion.rs`（5026〜7405）、`dictionary.rs`、`width.rs`、`romaji.rs`、`keymap.rs`、`preferences.rs`、`simd.rs` | `cargo test -p sakura-core --lib` | 件数不変。`ci/check-simd-assembly.ps1` 成功 | S |
| 1.4 | テスト専用循環の解消 | `preferences.rs:1609,1612`・`simd.rs:1761,1899` の参照を fixture 側へ移す | `rg -n 'input_repair' crates/sakura-core/src/preferences_tests.rs` など | production 側から循環参照が消える | S |
| 1.5 | tsf | `text_service.rs` の 2,253 テスト行と fake engine 5 種を `text_service_tests.rs`＋`testing/fake_engine.rs` へ；`engine_recovery_tests.rs` を `tests/` へ | `cargo test -p sakura-tsf --lib` | ~191 tests pass。`src/` に `*_tests.rs` 以外のテスト専用ファイル 0 | S |
| 1.6 | renderer `lib.rs` 新設 | `main.rs:28-42` の 16 mod 宣言を `lib.rs` へ、`main.rs` は `sakura_renderer::run()` 呼び出しだけ | `cargo test -p sakura-renderer --lib` | 約 150 tests が `--lib` で走る（従来は `--bin` 経由） | S |
| 1.7 | renderer / settings | `candidate.rs`、`pad.rs`、`ui.rs`、`updater.rs`、`update_trust.rs` のテスト分離 | 各 `cargo test -p <crate> --lib` | 件数不変 | S |
| 1.8 | engine の test-only module 退避 | `*_tests.rs` 12 本を `tests/` または対応 sibling へ、`*_oracle.rs` は 2.4 まで残置 | `ls crates/sakura-engine/src` | `src/` に `_tests.rs` は sibling 規約のものだけ | S |
| 1.9 | `#[ignore]` に理由付与 | 95 本すべて `#[ignore = "..."]` | `rg -n '#\[ignore\]$' crates` | 空 | S |
| 1.10 | scheduled desktop job | `.github/workflows/desktop-tests.yml`（self-hosted or windows-latest、`--ignored` 実行、weekly＋dispatch） | workflow_dispatch | settings 27 本・renderer 実 process テストが走り、結果が artifact に残る | M（owner: runner） |
| 1.11 | R9 を blocking に | 0.3 の `-Advisory` から R9 を外す | `pwsh ./ci/check-dependency-rules.ps1` | R9 違反 0 | S |

### Phase 2：葉 crate 抽出（依存の逆流を消す、8 PR）

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 2.1 | `sakura-values` | proto から `FixedStr/FixedVec/Overflow`、上限定数、`Mode/KeyCode/KeyInput/Modifiers/AppearanceTheme/PadShortcut` を移動。proto は `pub use sakura_values::*` で再輸出（下流無変更）。`Fingerprint` は 2.3 で追加 | R1；`cargo test -p sakura-proto`（roundtrip/robustness/zero_alloc）；`rg -n 'PROTOCOL_VERSION: u16 = 22' crates/sakura-proto/src/lib.rs` | R1 空。codec テスト全 pass。1 件 | M（owner D3: wire.rs） |
| 2.2 | `sakura-store` | `engine/src/input_history.rs` の record・codec・DPAPI・queue（`crate::` 参照ゼロの部分）と `learning.rs` の store 部分を移動。settings は `sakura-store` を参照 | R2；`cargo test -p sakura-store`；`cargo test -p sakura-settings`；既存 DPAPI ファイルの読み戻しテスト | R2 空。`history show/export/stats` の統合テスト pass、`input.bin` 形式不変 | M |
| 2.3 | `sakura-rerank-proto` | `neural-worker/src/protocol.rs:3-8` と `engine/src/long_conversion.rs:28-29` を 1 crate に。`Fingerprint` を values へ | R8；`cargo test -p sakura-neural-worker`；2 候補 protocol v1 IPC テスト | R8 が 1 件。worker と engine が同じ定義を使う | S |
| 2.4 | `sakura-oracles` | engine の `*_oracle.rs` 12 モジュールを dev-dependency crate へ。`lib.rs:66` の常時 compile を削除。cargo-mutants 設定を repoint | `cargo build -p sakura-engine --release` の後 `rg -n 'oracle' target/release/deps/*.d`；`cargo test -p sakura-engine` | release binary に oracle シンボル無し。テスト件数不変 | S |
| 2.5 | `sakura-context` | #34 休眠 4 モジュール 1,846 行を feature `context` 付き別 crate へ | `cargo build -p sakura-engine`；`cargo build -p sakura-engine --features context` | 既定 build から 1,846 行が消える。feature 付きで従来どおり | S |
| 2.6 | engine dev-deps 整理 | `ime-eval` dev-dep を削除（`tests/pipe_round_trip.rs:36-37` は ime-eval 側の integration test へ）、`dictc` は feature `dictc-fixtures` 背後 | `cargo tree -p sakura-engine -e dev`；`cargo test -p sakura-engine` | ime-eval が出ない。テスト pass | S |
| 2.7 | `sakura-context-proto` 改名 | `sakura-neural-proto` → `sakura-context-proto`（`SCV1` は不変） | `rg -n 'sakura_neural_proto\|sakura-neural-proto' .` | 空 | S |
| 2.8 | R1・R2・R8 を blocking に | 0.3 の `-Advisory` から外す | `pwsh ./ci/check-dependency-rules.ps1` | 違反 0 | S |

### Phase 3：core／proto／ipc／reg／dictc の分割（10 PR）

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 3.1 | `conversion/` 分割 | §3.2 の 8 サブモジュール。公開 API（`convert`, `ConversionOptions`, `Candidate`）は `conversion/mod.rs` で維持 | `cargo test -p sakura-core --lib conversion`；`cargo bench`（あれば）；`tools/ime-eval` の固定コーパス比較 | テスト件数不変。ime-eval の recall／top-1 が bit 一致 | L |
| 3.2 | `dictionary/` 分割 | `format.rs` を唯一定義に。`dictc/tests/image.rs` の適合テストを core 側にも複製（reader 視点） | `cargo test -p sakura-core --lib dictionary`；辞書再ビルド | 通常ビルド辞書が 39,349,040 bytes、SHA-256 `b7d08643…` で一致 | M |
| 3.3 | `width/`＋simd | `width/scan/` に simd を収容 | `pwsh ./ci/check-simd-assembly.ps1`；`cargo test -p sakura-core --features simd-assembly-audit` | 成功 | M |
| 3.4 | `romaji/`、`keymap/`、`preferences/` | §3.2 どおり | `cargo test -p sakura-core --lib` | 件数不変 | M |
| 3.5 | facade 剪定 | 未使用再輸出 ~28 を削除 | `cargo build --workspace`；`ci/check-facade.ps1` | build 成功、未使用 0 | S（owner D4） |
| 3.6 | `proto/message/` 分割 | tags/header/request/response/ui_state | `cargo test -p sakura-proto` | codec テスト全 pass、`PROTOCOL_VERSION` 22 | S |
| 3.7 | `ipc/security/` 分割 | admission（:88-141）と server_trust（:151-320, 453-513）を分離 | `cargo test -p sakura-ipc` | #104 のテストが `server_trust_tests.rs` に閉じる | S |
| 3.8 | `sakura-reg` 分割 | Credential Manager→`sakura-user-prefs`、maintenance/launcher/vscode_diagnostics→`sakura-install-maintenance`、`lib.rs:1-9` の偽記述を修正 | `cargo test -p sakura-reg -p sakura-user-prefs -p sakura-install-maintenance`；`ci/dep-policy.ps1` | pass。TSF DLL サイズ不変（0.1） | M |
| 3.9 | `dictc-core`＋thin `dictc` | lib を `dictc-core` へ、dictc を import しない 3 bin（2,323 行）を `tools/` へ | 辞書 2-pass 再ビルド | 39,349,040 bytes／SHA-256 `b7d08643…` 一致 | M |
| 3.10 | tools workspace 統合 | `candidate-sweep` を workspace member に、`candidate-snapshot` は dep-policy が `Cargo.lock` を読む | `pwsh ./ci/dep-policy.ps1` | tools の lock も監査される（R11） | S |

### Phase 4：engine の分割（12 PR）

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 4.1 | `input_history` 分割 | 13 変更理由を `history/{scope_filter,queue,compaction,stats,service}.rs` へ（record/codec は 2.2 で store 済み） | `cargo test -p sakura-engine --lib history`；TLA+ `DeveloperHistory` correspondence | 件数不変。correspondence 解決 100% | M |
| 4.2 | `render/` | `ui.rs` の描画状態組立を移動 | `cargo test -p sakura-engine --lib render` | 不変 | S |
| 4.3 | `edit/` | preedit 編集 | 同上 | 不変 | S |
| 4.4 | `keys/` と `apply_action` 分割 | 45 arm を `keys/<action>.rs` に 1 handler 1 ファイル。`apply_action` は `match` → handler 呼び出しのみ ≤120 行。`ModeKanaCycle`（:3435）は `keys/mode_kana_cycle.rs` | `cargo test -p sakura-engine --lib keys`；`wc -l crates/sakura-engine/src/keys/mod.rs`；`無変換` 回帰テスト（CLAUDE.md の必須遷移 5 項目） | 168 tests pass。≤120 行。5 遷移 pass | L |
| 4.5 | `candidates/`、`commit/`、`predict/` | 同様 | `cargo test -p sakura-engine --lib` | 不変 | M |
| 4.6 | `state/` | `Session` と mode を `state/` へ。R4 を blocking に | R4；`cargo test -p sakura-engine --lib state` | R4 空 | M |
| 4.7 | `services/` builder | `Dispatcher` constructor 8 個・`Server` constructor 12 個を `EngineServices::builder()` 1 系統に | `rg -n 'pub fn new\|pub fn with_' crates/sakura-engine/src/runtime/ crates/sakura-engine/src/services/` | constructor が builder 1 系統＋`Default` のみ | M |
| 4.8 | `runtime/` と `serve` 分割 | `serve`（:1519-1883）を accept／dispatch／shutdown に。#148 の経路が 1 ファイルに | `cargo test -p sakura-engine`；`tests/pipe_round_trip.rs` | 件数不変。#148 再演で読むファイル ≤3 | M |
| 4.9 | `Session` 分割 | 148 field を `state/{mode,composition,conversion,candidates,prediction}.rs` に | `cargo test -p sakura-engine` | 不変 | L（owner D5） |
| 4.10 | verification 対応更新 | `verification/sakura-engine/*/correspondence.json` を新モジュールパスに | `pwsh ./ci/check-verification-correspondence.ps1` | 解決 100% | S |
| 4.11 | engine README | `crates/sakura-engine/README.md` ≤4 KB | `wc -c` | ≤4,096 B | S |
| 4.12 | R4 blocking＋file budget 報告 | `ci/report-file-budget.ps1`（1,500 行超の production ファイルを警告、非ブロッキング） | 実行 | engine で 1,500 行超が 0 | S |

### Phase 5：renderer／settings の分割（12 PR）

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 5.1 | renderer 循環解消 | `screen.rs` に `dpi_of`／`monitor_work_area`／共有定数 3 つ。`pad_rail.rs:351`、`indicator.rs:57,76,261,573` を repoint。R5・R6 blocking | R5・R6；`cargo test -p sakura-renderer --lib` | 空。不変 | S |
| 5.2 | `candidate/` 分割 | layout／placement（純粋）と paint／overlay／window（GDI）。`text_width_96`（:1846）を `theme::text_width`（:285）に統一 | `cargo test -p sakura-renderer --lib candidate`（33 tests）；実 renderer 統合テスト（scheduled） | 33 pass。260–480 logical px 幅テスト pass | M |
| 5.3 | `pad/` 分割 | `layout`（:243-662）を `pad/layout.rs`、`pad_procedure`（:2833）を `pad/window.rs` | `cargo test -p sakura-renderer --lib pad`（14 layout tests） | pass | M |
| 5.4 | `indicator/`、`input/`、`watch/` | 同様 | `cargo test -p sakura-renderer --lib` | 不変 | S |
| 5.5 | settings `update/` 分割 | `update_trust.rs` を `contract.rs`（:118-790）と `trust_state.rs`（:792-1162）に | `pwsh ./ci/test-update-signing-v2.ps1`；`cargo test -p sakura-settings --lib update`（9 tests＋#150 回帰 `a_trust_state_below_the_embedded_floor_is_treated_as_absent_and_rebuilt`） | 全 pass | M |
| 5.6 | `updater.rs` 分割 | flow/http/digest/authenticode/installer（4 trait は維持） | `cargo test -p sakura-settings --lib update`（16 tests） | pass | M |
| 5.7 | `ui/model.rs` | `App` 33 field の純粋状態と enum 写像（:3397-3853）を `model.rs` へ（12 tests） | `cargo test -p sakura-settings --lib ui::model` | pass | M |
| 5.8 | `ui/presentation.rs` を葉に | `use super::*` 3 本を除去。R7 blocking | R7 | 空 | S |
| 5.9 | topic registry | `ui/topics/` に 15 topic、`GeneralControls` 68 HWND を topic ごとに | 17 本の ignored desktop test（`tab_focus_order_skips_hidden_topics_and_ends_at_actions` 含む）を scheduled job で | 全 pass | L（owner D6） |
| 5.10 | topics を 1 つずつ移動 | 1 PR 1〜3 topic | 同上 | 同上 | M×5 |
| 5.11 | `cli/` 分割 | parse/run/render | `cargo test -p sakura-settings --lib cli` | 不変 | S |
| 5.12 | `config/`、`history/`、`engine/` | §3.2 どおり | `cargo test -p sakura-settings` | 不変 | S |

### Phase 6：TSF の分割（11 PR、後半 3 本は証拠ゲート付き）

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 6.1 | `com/live_objects.rs` | COM オブジェクト生存管理を切り出し、`exports ⇄ class_factory` 循環を解消 | `cargo test -p sakura-tsf --lib`；0.1 DLL サイズ；`regsvr32` 相当の統合テスト | ~191 pass。≤1 MiB | S |
| 6.2 | `session/reentry_latches.rs` | 5 つの裸 `Cell` bit（:1000-1024）を 1 型に。API は `try_enter(kind) -> Option<Guard>` | `cargo test -p sakura-tsf --lib session::reentry` | 5 latch の総当たりテスト pass。`rg -n 'Cell<bool>' crates/sakura-tsf/src/com/text_service.rs` 空 | S |
| 6.3 | `session/layout_lease.rs`＋`host/layout` | queue_layout の所有権を session に | 同上 | 不変 | S |
| 6.4 | `session/deferred_work.rs` | `deferred: RefCell<DeferredState>` 40 箇所を 1 モジュールに閉じる | `rg -n '\.deferred\.' crates/sakura-tsf/src/com/` | 空（com/ からは `session.deferred()` 経由のみ） | M |
| 6.5 | `host/engine/{mod,link,requests}.rs` | `engine.rs` 2,572 行を transport／request wrapper／recovery に。`tla-record.md:24` を `Engine::is_desynchronized` に修正 | `cargo test -p sakura-tsf --lib host::engine`；correspondence | pass。解決 100% | M |
| 6.6 | `session/key_arbitration.rs` | Dual-TSF キー調停を純粋モジュールに。TLC `DualTsfPhysicalKeyArbitration` 8 cfg を correspondence で紐付け | `cargo test -p sakura-tsf --lib session::key_arbitration`；TLC 8 cfg | 状態数 283/87/9、遷移 5/4 が記録と一致 | M |
| 6.7 | `session/` の R3 を blocking に | `use windows` 0 | R3 | 空 | S |
| **証拠ゲート** | #7 のクラッシュダンプまたは diagnostic ring 捕捉が Issue に添付されるまで 6.8〜6.10 を開始しない（§6） | | | | |
| 6.8 | `session/write_journal.rs`＋`write_pipeline.rs` | `write_coordinator.rs`（validate_callback :482）と `apply_queued_write`（:5875、292 行）を session に | `cargo test -p sakura-tsf --lib session::write`；TLC `TsfProbeHostInsert`／`WriteJournalAuthority` 相当 | pass。correspondence 100% | L |
| 6.9 | `session/candidate_board.rs` | `DualTsfCandidateBoard` 7 cfg と 1:1。`GuardForeignCandidateEnd`／`RestoreCurrentPlacement` の owner 判断 D7 後 | `cargo test -p sakura-tsf --lib session::candidate_board`；TLC 7 cfg | pass | L |
| 6.10 | `com/text_service.rs` 残余分割 | ≤1,300 行（vtable 572＋委譲） | `wc -l crates/sakura-tsf/src/com/text_service.rs`；DLL サイズ；実機 smoke（メモ帳／VS Code で入力・確定・focus 移動） | ≤1,300。≤1 MiB。クラッシュ・ハング無し | L |
| 6.11 | TSF README＋`docs/contracts/write-journal.md` | | `wc -c` | ≤4 KB | S |

### Phase 7：文書・verification・CI（Phase 0 から並走、13 PR）

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 7.1 | `docs/decisions/` 抽出 | CLAUDE.md の owner 判断 4 件（テスト出力 #111、IT エンジニア・ファースト、リリース署名、trust state #150）を 1 件 1 ファイルに。CLAUDE.md はリンク | `wc -c CLAUDE.md` | 段階的に ≤8 KB | S（owner D1） |
| 7.2 | `docs/history/` へ退避 | release-notes 36 本、research、停止中調査 | `ls docs` | `docs/` 直下は ≤10 ファイル | S |
| 7.3 | crate README 15 本 | テンプレ：責務／公開 API／Issue 型／禁止依存／テストコマンド | `for c in crates/*; wc -c $c/README.md` | 15/15、各 ≤4,096 B | M |
| 7.4 | DESIGN.md 分割 | 不変条件だけ残し、詳細を `docs/architecture/{conversion,ui,packaging}.md` へ | `wc -c DESIGN.md` | ≤40,960 B | M（owner D2） |
| 7.5 | `docs/contracts/` 12 本 | §3.5。config-format と engine-admin は新規執筆 | `ls docs/contracts \| wc -l`；`rg -c 'Password.*URL.*Email.*Digits' --glob '!docs/contracts/input-scope.md' .` | 12。機微スコープ規則の再掲 0 | M |
| 7.6 | verification 再編 | §3.6。`git mv` のみ、内容不変。`.gitignore:24` と tracked 3 件の矛盾を解消 | `git log --follow` が繋がる；`find verification -type f \| wc -l` | 187 ファイル | M |
| 7.7 | `correspondence.json`＋検査 | 10 モデル分。`ci/check-verification-correspondence.ps1 -SelfTest` | 実行 | 解決 100%、SelfTest pass | M |
| 7.8 | crate 単位 CI matrix | `ci/changed-crates.ps1` が `git diff --name-only origin/main...HEAD` から影響 crate（依存下流含む）を JSON matrix に。`ci.yml` は matrix job＋週次 full | PR で 1 crate だけ触る | その crate と下流だけ `cargo test -p` が走る。所要 ≤3 分 | M（#89） |
| 7.9 | 0.4 の TLC workflow をブロッキングに | `continue-on-error` 除去 | 実行 | 緑 | S（owner D8） |
| 7.10 | scripts 整理・`rtk` 残滓除去 | `scripts/` を build／verify／release に分け、`rtk` 参照を削除 | `rg -n 'rtk' scripts ci tests` | 空 | S |
| 7.11 | `rules.md` 分割 | `.claude/memory/rules.md` 45,652 B を topic 別に（削除せず移動） | `wc -c .claude/memory/rules.md` | ≤16 KB＋`rules/<topic>.md` | S（owner D9） |
| 7.12 | Issue 型→ディレクトリ表 | §3.7 を CLAUDE.md に | `rg -n 'Issue 型' CLAUDE.md` | 1 件 | S |
| 7.13 | `docs/architecture/README.md` | crate 図（§3.1）と R1〜R11 を 1 ページに | `wc -c docs/architecture/README.md`；`grep -c "^| R[0-9]* |" docs/architecture/README.md` | ≤8,192 B。R1〜R11 の 11 行 | S |

---

## 5. 読解セットの before → after

行数＝Issue 修正時に必ず読む production コード。散文＝必読文書 bytes。after は各解析レポートの見積り。

| Issue 型 | 例 | コード before | コード after | 散文 before | 散文 after | 主に効く Phase |
|---|---|---:|---:|---:|---:|---|
| `無変換` モード循環 | #51 | 19,600 | 900 | 170,000 | 20,000 | 1.1、4.4 |
| 候補順序／ranking | #94, #108 | 20,900 | 3,050 | 170,000 | 20,000 | 1.3、3.1 |
| 句読点候補 | #99 | 11,279 | 450 | 170,000 | 20,000 | 3.1 |
| 辞書イメージ形式 | #109 | 5,020 | 1,150＋writer | 215,000 | 22,000 | 3.2、3.9 |
| romaji | #56 | 2,225 | 1,130 | 170,000 | 20,000 | 3.4 |
| 履歴 compaction | #113 系 | 7,000 | 950 | 265,000 | 20,000 | 2.2、4.1 |
| engine #148 経路 | #148 | 21,100 | 1,290 | 170,000 | 20,000 | 4.8 |
| 再入 candidate UI | #7 | 9,700 | 910 | 127,000 | 22,000 | 6.2、6.9 |
| stale composition | #57 | 8,230 | 1,460 | 127,000 | 22,000 | 6.4、6.8 |
| Dual-TSF キー調停 | #69 | 7,400 | 540 | 127,000 | 22,000 | 6.6 |
| 候補ポップアップ | #47, #142 | 3,730 | 700 | 127,000 | 22,000 | 5.2 |
| pad | #92 | 5,570 | 1,300 | 127,000 | 22,000 | 5.3 |
| 更新 trust state | #150 | 2,990 | 800 | 215,000 | 22,000 | 5.5 |
| 設定 UI | #146 | 5,900 | 800 | 127,000 | 22,000 | 5.7〜5.10 |
| reranker／server trust | #88, #104 | 2,100 | 730 | 170,000 | 20,000 | 2.3、3.7 |
| CI／プロセス | #111 系 | — | — | 215,000 | 22,000 | 7.x |

Phase 1（テスト分離）だけで engine・core・tsf の行数が 40〜60% 減る。Phase 2 で crate 越しの読解（proto codec、engine の store）が消える。残りの削減はモジュール分割による。

---

## 6. リスク管理と証拠ゲート

| リスク | 対策 |
|---|---|
| TSF の write journal／candidate teardown を触ってクラッシュを悪化させる | **6.8〜6.10 は #7 のクラッシュダンプまたは diagnostic ring 捕捉が Issue に添付されるまで開始しない**。`verification/revalidation-20260905.md` が T3/T4 を HYPOTHESIS と分類している以上、証拠なしに状態機械を動かさない（CLAUDE.md の 2026-08-13 訂正と同じ方針） |
| DLL が 1 MiB を超える | 0.1 のゲートを Phase 1 の前に入れる。generic 化・trait object 化は DLL では避け、`opt-level="z"` の下で計測する |
| テスト件数の無言減少 | 0.2 の基準 JSON と毎 PR 比較。減ったら理由を PR 本文に |
| 辞書イメージの意図せぬ差分 | 3.2・3.9 で 39,349,040 bytes／SHA-256 `b7d08643395181f6d214866f9bb98646de366dc71caa15320effe774bc4c1d90` を再現 |
| `PROTOCOL_VERSION` の暗黙変更 | 2.1・3.6 で `= 22` を rg 固定。wire 形式変更はこの計画の範囲外 |
| 分割で pub 面が広がる | `pub(crate)` を既定、crate 境界の `pub` は README に列挙（3.5、4.11、6.11、7.3） |
| `git blame` の断絶 | 分割は `git mv` → 編集の 2 commit に分け、`--follow` が繋がるようにする |
| 大 PR による review 不能 | 1 PR ＝ 1 ステップ。production 行の変更を伴う PR は diff ≤1,500 行 |
| `#[ignore]` テストの黙殺 | 1.10 の scheduled job を Phase 5 の前に稼働させる |
| CI 時間の増加 | 7.8 の matrix で通常 PR は ≤3 分。full は週次と release |

---

## 7. Owner 判断が必要な項目

| ID | 判断 | 影響ステップ | 既定案 |
|---|---|---|---|
| D1 | CLAUDE.md の §最優先タスク（VS Code 調査）と SolAdvisor 記述をどう扱うか（履歴へ退避か削除か） | 0.5、7.1 | `docs/history/issues/` へ退避、CLAUDE.md からは 3 行のリンクに |
| D2 | DESIGN.md 100 KB をどこまで分割するか | 7.4 | 不変条件と境界だけ残し ≤40 KB |
| D3 | `wire.rs`（proto の低レベル codec）を `sakura-values` に降ろすか proto に残すか | 2.1 | proto に残す（values は値型のみ） |
| D4 | core facade の未使用再輸出 ~28 を削除してよいか | 3.5 | 削除（外部利用者なし） |
| D5 | `Session` 148 field の分割を行うか | 4.9 | 行う。ただし Phase 4 の最後、単独 PR |
| D6 | settings topic registry のため 17 本の desktop test を走らせる runner | 1.10、5.9 | self-hosted Windows runner を週次 |
| D7 | `GuardForeignCandidateEnd`／`RestoreCurrentPlacement` の扱い（6.9 の前提） | 6.9 | #7 証拠取得後に決める |
| D8 | TLC workflow をブロッキングにするか | 7.9 | Phase 3 完了後にブロッキング |
| D9 | `.claude/memory/rules.md` 45 KB の分割 | 7.11 | topic 別に移動、削除しない |
| D10 | `session/` を独立 crate（`sakura-tsf-session`）にするか、ディレクトリのままか | 6.7 | Phase 6 完了まではディレクトリ＋R3。安定後に crate 化を再検討 |
| D11 | 実行時の候補上限（`research-wide-candidates` feature）を残すか | 3.1 | 残す（研究用） |

---

## 8. 命名の統一

解析レポート間で名前が揺れていたものは以下に統一する。

| 統一名 | レポート内の別名 |
|---|---|
| `sakura-values` | `sakura-limits`（contracts §6） |
| `sakura-store` | `sakura-stores`（contracts §6） |
| `sakura-context-proto` | `sakura-neural-proto`（現行名） |
| `session/`（tsf） | 「純粋状態機械層」（tsf §7） |
| `<name>_tests.rs` | 「sibling test file」 |

---

## 9. 付録

### 9.1 共通 Verify コマンド

```powershell
cargo fmt --all -- --check
./ci/run-test-quiet.ps1 -Name '<crate> tests' -Command { cargo test -p <crate> }
git diff --check
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
./ci/dep-policy.ps1
./ci/check-dependency-rules.ps1
./ci/check-dll-size.ps1
./ci/check-verification-correspondence.ps1
```

ローカルでは cargo の前に `CARGO_HTTP_CHECK_REVOKE=false` を付ける。

### 9.2 PR テンプレ（各ステップ共通）

```
Step: <Phase>.<n> <title>  (refs #152)
Reads: <このステップで読んだファイル、行数合計>
Changes: production <±行>, tests <±行>, moved <ファイル数>
Verify: <実行したコマンドと結果 1 行ずつ>
Expect: <観測値> (baseline: <値>)
Rules now blocking: <R#>
```

### 9.3 解析レポートとの対応

| 本書の節 | レポート |
|---|---|
| §2.2 engine 行、§4 Phase 4 | `analysis/engine.md` §3, §7, §9 |
| §2.3 core→proto、§4 Phase 3 | `analysis/core-proto.md` §2, §7, §9 |
| §2.4 TextService、§4 Phase 6 | `analysis/tsf.md` §3, §5, §7, §9 |
| §2.4 reranker、§4 3.7〜3.10 | `analysis/contracts-dictc-tools.md` §3, §6, §8 |
| §2.6、§3.5〜3.6、Phase 7 | `analysis/docs-verification-ci.md` §2, §6, §8 |
| §2.2 renderer/settings、Phase 5 | `analysis/renderer-settings.md` §3, §4, §7, §9 |
