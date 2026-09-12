# Sakura Input エージェント指向リファクタリング計画

- 基準コミット：`a370477`（origin/main、2026-09-12、v1.0.39、PR #151 merge）
- 追跡 Issue：[#152](https://github.com/tsuyoshi-otake/sakura-input/issues/152)
- 根拠資料：`docs/architecture/analysis/{engine,core-proto,tsf,contracts-dictc-tools,docs-verification-ci,renderer-settings}.md`（HEAD a370477 に対する静的解析 6 本。本書の行番号・件数はすべてそこから引用）
- 本書の位置づけ：**計画書**。コード変更は含まない。各ステップは「エージェント 1 セッションで読解・変更・検証が完結する粒度」に切ってある。PR との対応は §4 冒頭の境界原則に従う（1 ステップ ＝ 1 PR は機械的規則にしない）。
- 中心 KPI：**IRV（Issue Reading Volume、§1.1）**。1 Issue を安全に変更するためにエージェントが読むコード・テスト・契約・文書・設定の総量を、Phase 0 で計測し（`verification/irv/baseline.json`）、全 PR で回帰検査し、Phase ごとに目標値まで下げる。
- 追跡 PR：[#153](https://github.com/tsuyoshi-otake/sakura-input/pull/153)

---

## 0. 要旨（この 1 画面だけ読めば方針が分かる）

**目的**：1 つの Issue を直すとき、エージェントが読むコード・文書・テストを最小化する。Clean Architecture の形に合わせることが目的ではない。判定基準は **IRV（Issue Reading Volume）**：代表 Issue 形 10 種について「読み込む量（Physical）」と「理解が要る量（Semantic）」を別々に測り、両方を下げる（§1.1）。

**IRV の現状**（Phase 0 基準値、§1.1.1）：`無変換` 系 Issue で 21,664 行を読むが理解が要るのは 900 行。候補順序系 26,842 行／3,050 行。TSF 再入系 13,543 行／910 行。無条件に読む文書 85,770 B（予算 24,576 B）。

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

0. **ゲートを先に置く**（Phase 0）：main を ruleset で保護して PR 必須にし、依存規則・crate 別テスト・workspace テスト・IRV 回帰・DLL サイズを必須チェックにする。IRV 基準値をここで固定する。
1. **テストを本体から出す**（sibling `*_tests.rs`）。挙動不変で、Phase 1 の 6 読解集合では Physical IRV が約 16〜55% 減った（#173 訂正実測）。最初にやる。**Semantic IRV はこれでは下がらない**（§1.1）。
2. **葉 crate を 5 つ切る**（`sakura-values`、`sakura-store`、`sakura-rerank-proto`、`sakura-oracles`、`sakura-context-research`）。依存の逆流を全て消し、Cargo metadata と Rust-aware architecture check で検査できる方向規則にする。各 crate は憲章（Purpose／Owns／Must not own／Allowed dependencies／Allowed consumers／Public API budget、§3.1.1）を持ち、「どこにも置けないものを置く場所」にしない。
3. **god file を「変更理由」で分割**する。行数ではなく Issue の型で切る。分割後の各ディレクトリに「この Issue 型はここ」の対応表を置く。
4. **crate 単位の CI ゲート**（paths filter matrix、DLL サイズ、TLC、verification⇄Rust 対応検査）を足し、モジュールを独立に検証可能にする。
5. **文書を「契約」「判断」「履歴」に分け**、`CLAUDE.md` を 8 KB 以下、crate ごとに README ≤4 KB を置く。crate 責務・依存規則・不変条件・所有・契約・移行規則は Phase 0〜2 で前倒しし、新 crate を作る PR は README と依存規則を同梱する（Phase 7 は残りの整理だけ）。
6. **形式検証境界を実ディレクトリにする**：`sakura-tsf/src/session/` に `use windows` を禁止し、TLA+ モデルと 1:1 対応する `correspondence.json` を CI で検査する。

**順序**：Phase 0（ゲート整備）→ 1（テスト分離）→ 2（葉 crate）→ 3（core/proto/ipc/reg/dictc 分割）→ 4（engine）→ 5（renderer/settings）→ 6（TSF）。Phase 7（文書・verification）は Phase 0 から並走。TSF の write journal／candidate teardown に触る 3 工程は **#7 のクラッシュ証拠が取れるまで着手しない**（§6）。

**規模**：ステップ 93。PR はステップと 1:1 ではない（§4 冒頭の境界原則）。Phase 1〜2 だけで Physical IRV の削減幅の 6 割が得られる（§5）。各 Phase の受け入れ基準は IRV 目標値で書く（§4.1）。最終目標は代表 10 ベンチマークで **Physical ≤1,200 行、Semantic ≤1,000 行**（例外 2 件は §4.1 に理由付きで明記）。

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
| Build/Test Isolation | `cargo metadata --no-deps` が列挙する workspace package／target ごとのテスト基準と、package 単位の単独実行可否 | 現行 target 構成と件数を Phase 0.2 で採取、CI は workspace 1 本 | metadata 上の全 package／testable target を欠落なく記録し、変更 package だけ走る matrix |
| Explicit Contracts | 契約ごとの単一文書と単一 owner crate | 複数契約が文書なし、reranker protocol が 2 箇所 | `docs/contracts/` に §3.5 の完全 inventory ごとに 1 契約 1 文書、owner crate 1 つ |
| Small Agent Context | Issue 型別の必読セット（行数・bytes、§5） | 中央値 ~170 KB 散文 + 数千〜2 万行 | 散文 ≤25 KB、コード ≤1,500 行 |
| Independently Testable | 純粋ロジックが Win32/COM なしで走るテスト数 | TSF は `composition.rs` の 23 本のみ模範 | `session/` 全体、renderer `layout`、settings `model` |
| Independently Refactorable | 公開 API 面（`pub` item 数）と `pub(crate)` 化率 | core facade 113 再輸出、~28 未使用 | 未使用再輸出 0、crate 境界の `pub` は README に列挙 |
| Formal Verification Boundary | `correspondence.json` の Rust シンボル解決率 | 0%（`traceability.json` STALE） | 100%、CI で検査 |
| 誤認しにくい構造 | Issue 型 → ディレクトリ対応表（§3.7）の一意性 | 無し | 全 Issue 型に 1 ディレクトリ |


### 1.1 中心 KPI：IRV（Issue Reading Volume）

本計画の成否は次の 1 問で判定する。**「ある Issue を渡されたエージェントが、その Issue と無関係なコードを読まず、狭い責務境界の中だけで安全に理解・変更・検証できるようになったか」**。これを測る Architecture Fitness Function として IRV を定義し、Phase 0 で基準値を取り、以後の全 PR で回帰を検査する。§1 の 12 指標は IRV を下げるための手段であり、IRV が下がらない分割は成果と見なさない。

**定義**：IRV(b) ＝ 代表 Issue 形 b を安全に変更するためにエージェントが読む必要のある資料の総量。次の 6 カテゴリの合計。

| カテゴリ | 含むもの | 単位 |
|---|---|---|
| Production Code | 変更対象と、その挙動を決める呼び出し元・呼び出し先 | LOC・bytes・ファイル数 |
| Test Code | 変更で壊れうる既存テストと、追加すべきテストの置き場 | 同上 |
| Contract / Protocol | wire 形式、keymap、TOML／JSON schema、`PROTOCOL_VERSION` | 同上 |
| Documentation | Issue 型別の契約文書と、**無条件に読む文書**（`CLAUDE.md`、`.claude/memory/rules.md`） | bytes（LOC も記録） |
| Build / CI / Config | `Cargo.toml`、workflow、`ci/*.ps1` のうち変更経路に関わるもの | 同上 |
| Verification / Formal Model | 対応する TLA+ モデル、cfg、record | 同上 |

単位は当面 LOC・bytes・ファイル数。トークン数は `scripts/measure-irv.ps1` に tokenizer を差し込めば同じ JSON に列を足せる（`physical.tokens`）。

**Physical IRV と Semantic IRV**

| | 定義 | 測り方 | 下がる操作 |
|---|---|---|---|
| Physical IRV | 機械的に読み込まれる量（`Read` されるファイルの総 LOC・bytes） | `measure-irv.ps1` が自動計測 | テスト分離、ファイル分割、依存の切断 |
| Semantic IRV | 正しく変更するために**理解しなければならない**量（型・不変条件・状態遷移・呼び出し経路） | 解析レポート §9 の必読範囲をエージェントまたは人が確認し、`semantic.ranges` に行範囲で記録 | 変更理由ごとの責務分割、依存方向の一方向化、契約の単一化 |

**原則：ファイルを分割しただけでは Semantic IRV は下がらない。** インラインテストを sibling に移す Phase 1 は Physical IRV だけを下げる（KEY-MODE の読解集合は Phase 1 前後で 22,251→10,082 LOC。単独の `dispatch.rs` の行数ではない。#173 の訂正計測）。`apply_action` 431 行を 45 ファイルに割っても、45 ファイルを全部読まないと `無変換` を直せないなら Semantic IRV は同じである。Semantic IRV が下がるのは、Issue 型ごとに「ここだけ読めばよい」入口と契約が定まり、境界の外を読まなくても安全だと言える構造になったときだけ。各 Phase の受け入れ基準（§4）は両方を別々に持つ。

**測定の 2 層**

| 層 | 誰が | 何を | 位置づけ |
|---|---|---|---|
| 自動近似 | `scripts/measure-irv.ps1`（CI） | `benchmarks.json` に固定したファイル集合の LOC／bytes／ファイル数、インラインテスト行数、無条件文書 bytes | 毎 PR の回帰ゲート。**近似**であり、依存の切断はファイル集合の更新（＝ベンチマーク定義の PR）で反映する |
| 検証済み semantic 集合 | 解析レポート §9 を書いたエージェント、または Phase 完了時のレビュア | `semantic.ranges`（file・start・end・why）と `estimated_loc` | Phase 受け入れ時に更新。自動化しない。行範囲は `sed -n` で実在を確認する |

#### 1.1.1 IRV ベンチマークスイート

`verification/irv/benchmarks.json` に固定 ID で定義する。ID は変更しない（履歴比較のため）。ファイル集合は構造変更のたびに更新するが、**Issue 形（issue_shape）と entry point の意味は変えない**。

| ID | Issue 形（例） | entry point | critical |
|---|---|---|---|
| `IRV-ENGINE-KEY-MODE` | `無変換`／モード循環／キー→Action（#51） | `dispatch.rs::apply_action`（`Action::ModeKanaCycle`） | ✔ |
| `IRV-ENGINE-CANDIDATE` | 候補順序／ranking／evidence（#94、#108） | `core/conversion.rs` ranking ＋ `engine/candidate_projection.rs` | ✔ |
| `IRV-TSF-REENTRANCY` | 再入 candidate UI クラッシュ／teardown（#7） | `tsf/text_service.rs` の deferred／latch | ✔ |
| `IRV-TSF-DUAL-KEY` | Dual-TSF 物理キー調停（#69） | `tsf/key_handler.rs` | ✔ |
| `IRV-CORE-CONVERSION` | 句読点・数字・日付候補合成（#99） | `core/conversion.rs` synthesis | |
| `IRV-DICTIONARY-FORMAT` | 辞書イメージ形式／reader-writer 適合（#109） | `core/dictionary.rs::image_format` | |
| `IRV-HISTORY-STORE` | 開発者履歴 store／compaction／stats（#113〜#132） | `engine/input_history.rs` | |
| `IRV-RENDERER-POPUP` | 候補ポップアップ配置（#47、#142） | `renderer/candidate.rs` | |
| `IRV-CI` | CI／テスト runner／プロセス規約（#111） | `.github/workflows/ci.yml` | |
| `IRV-DOCS` | 無条件に読む文書（全 Issue） | `CLAUDE.md` | ✔ |

critical ＝ 過去 1 年で最も Issue が集中し、かつ状態機械を含む（誤読がクラッシュに直結する）4 系統と、全 Issue に乗る文書負荷。

**Phase 0 基準値（歴史的な v1 出力）**（`verification/irv/baseline.json`、コミット 97705a5 のツリーを対象に計測。`LOC` は `wc -l` 相当、`Semantic` は解析レポート §9 の必読見積り）。**除テスト列は #173 の集計不具合の影響を受けており、目標値の根拠に使わない。** 最初の `#[cfg(test)]` 以降すべてを除外すると、途中の helper より後の production も消えてしまう。Physical LOC と回帰判定は影響を受けない。元 baseline は保存し、訂正した Phase 1 前後の別計測を [`inline-accounting.md`](../../verification/irv/inline-accounting.md) と [`phase1-corrected.json`](../../verification/irv/phase1-corrected.json) に記録する。

| ID | Physical LOC | 除テスト LOC | bytes | ファイル数 | Semantic LOC |
|---|---:|---:|---:|---:|---:|
| `IRV-ENGINE-KEY-MODE` | 21,664 | 4,927 | 845,745 | 5 | 900 |
| `IRV-ENGINE-CANDIDATE` | 26,842 | 7,446 | 1,059,791 | 4 | 3,050 |
| `IRV-TSF-REENTRANCY` | 13,543 | 8,858 | 554,795 | 8 | 910 |
| `IRV-TSF-DUAL-KEY` | 13,015 | 8,009 | 531,060 | 6 | 540 |
| `IRV-CORE-CONVERSION` | 10,355 | 6,392 | 418,312 | 4 | 450 |
| `IRV-DICTIONARY-FORMAT` | 5,306 | 4,821 | 207,843 | 4 | 1,150 |
| `IRV-HISTORY-STORE` | 7,797 | 4,380 | 329,935 | 15 | 950 |
| `IRV-RENDERER-POPUP` | 5,631 | 1,691 | 213,180 | 4 | 700 |
| `IRV-CI` | 1,749 | 1,749 | 76,614 | 10 | 600 |
| `IRV-DOCS` | 1,775 | 1,775 | 125,847 | 2 | — |
| 無条件文書（全ベンチマークに加算） | — | — | **85,770** | 2 | — |

読み方：`IRV-ENGINE-KEY-MODE` は「`無変換` を直すために 21,664 行を Read するが、うち 16,737 行はインラインテストで、本当に理解が要るのは 900 行」。Physical と Semantic の差 24 倍が、この計画で消す無駄である。

#### 1.1.2 IRV 回帰ゲート（Phase 0 で導入、全 PR で実行）

`scripts/measure-irv.ps1 -Compare verification/irv/baseline.json` を CI job `irv-regression` として必須チェックにする。判定：

| 条件 | 判定 | 根拠 |
|---|---|---|
| 依存境界違反（§3.3 の R1〜R12 のうち blocking 化済みのもの） | **FAIL** | 逆流 1 本で下流 crate 全体が読解集合に戻る。IRV の増分では捕まえられない構造回帰 |
| 無条件文書（`CLAUDE.md`＋`rules.md`）が予算 24,576 B を超過 | WARN mode では基準から増加した場合だけ **WARN**、非増加なら note／PASS（現状 85,770 B）。0.5／7.11 のうち後に完了する工程の PR で予算内を検証して CI を切り替え、その次の PR から、増加の有無にかかわらず予算超過を **FAIL** | 全 Issue に乗る固定費。8 KiB＋16 KiB は §3.5 の上限。D14 として 2026-09-13 に owner が判断を委任し、この切替時点に決定 |
| 代表ベンチマークの Physical LOC が基準比 **+10%** | **WARN**（PR 本文に理由必須） | ファイル集合を固定した LOC は決定的な計測であり、+10% の増加はノイズでなく読解負荷の増加。理由を説明する閾値として維持する |
| critical ベンチマークの Physical LOC が基準比 **+25%** | **FAIL** | 4,000 LOC の集合に 1,000 LOC のモジュールを再併合したのと同じ規模。critical 4 系統では誤読がクラッシュに直結する |
| `benchmarks.json` のファイル集合が実在しない（移動忘れ） | **FAIL** | 計測不能を静かに 0 にしない |

基準値は **Phase ごとに更新**する（Phase 完了 PR で `-Out` を再実行し、受け入れ基準を満たしたことを同じ PR で示す）。途中 PR は直前の基準値と比較するので、IRV は Phase 内で単調非増加になる。しきい値の変更は `benchmarks.json` の `thresholds.rationale` の書き換えを伴う PR でのみ行う。

`-Compare` は FAIL で exit 1 を返す。CI ではパイプで受けず（`| tail` は exit code を隠す）、直接実行する。
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
 領域            sakura-core  sakura-store  sakura-context-research(feature 限定)  sakura-reg(識別子のみ)  sakura-user-prefs  sakura-install-maintenance
                     │          │
 葉              sakura-values (FixedStr/FixedVec/Overflow, capacity, Mode/KeyCode/KeyInput/Modifiers, AppearanceTheme/PadShortcut, Fingerprint)
 dev only        sakura-oracles (engine の TLA+ oracle 12 モジュール)   dictc-core / dictc(bin)   tools/*
```

新規 crate と優先度：

| crate | 中身 | 由来 | 優先 |
|---|---|---|---|
| `sakura-values` | `fixed.rs`（FixedStr/FixedVec/Overflow）、`capacity.rs`（上限定数）、`input.rs`（Mode/KeyCode/KeyInput/Modifiers）、`appearance.rs`（AppearanceTheme/PadShortcut）、`scope.rs`（InputScope）、`ai_text.rs`（AiTextOperation/AiTextStatus）、`fingerprint.rs`（候補 fingerprint）。**wire codec は持たない**（proto 側が `impl Wire for` で実装、§4 Phase 2） | core §7 の `sakura-values` と contracts §6 の `sakura-limits` を統合。名前は `sakura-values` に統一 | 必須 |
| `sakura-store` | learning／input_history の **format（record layout・上限）／codec／persistence（ファイル配置・保持・compaction の純関数）／crypto（DPAPI を trait の背後に）**。writer thread・queue は engine に残す | engine §7、renderer-settings §5、contracts §6 の `sakura-stores` を統合。名前は `sakura-store` に統一 | 必須 |
| `sakura-rerank-proto` | `SKNR`/`SKNS` v1（magic、frame、bounds） | contracts §6 | 必須 |
| `sakura-oracles` | engine の `*_oracle.rs` 12 モジュール（dev-dependency のみ） | engine §7 | 推奨 |
| `sakura-context-research` | #34 休眠 4 モジュール 1,846 行。名前に `research` を付け、feature `context-research` でしか engine に入らない。`sakura-context` という広い名前は採らない。1 年間 feature が使われなかったことを確認した場合だけ、別の削除 PR で廃止する（期限だけで自動削除しない） | engine §7 | 決定済み（D13、2026-09-13 owner 委任） |
| `sakura-context-proto` | `sakura-neural-proto`（SCV1）の改名 | contracts §6 | 任意 |
| `sakura-user-prefs` | Credential Manager＋user preference（reg から） | contracts §6 | 推奨 |
| `sakura-install-maintenance` | maintenance／launcher／vscode_diagnostics（reg から） | contracts §6 | 推奨 |
| `dictc-core` | dictc lib の compile／image writer。`dictc` は bin だけの薄い crate に | contracts §6 | 推奨 |
| `sakura-diagnostics` | ipc の `debug_trace.rs`／`diagnostics.rs`（pipe と無関係） | contracts §6 | 任意 |

### 3.1.1 新 crate の憲章（junk drawer 化の防止）

新 crate は例外なく次の 6 項目を `crates/<crate>/README.md` の見出しとして持つ。**Must not own** に書かれたものを入れる PR は、憲章を改訂する PR を先に通さない限り merge しない。Public API budget は `pub` item 数の上限で、超えたら分割か憲章改訂。`ci/check-dependency-rules.ps1` は README の `Allowed dependencies` と `Cargo.toml` の `[dependencies]` が一致することも検査する（R14 として 2.8 で追加）。

| 項目 | `sakura-values` | `sakura-store` | `sakura-rerank-proto` | `sakura-oracles` | `sakura-context-research` |
|---|---|---|---|---|---|
| **Purpose** | 全 crate が共有する値型と上限定数の唯一の定義 | 学習／入力履歴の永続レコードの形式・codec・保存規則を engine と settings で 1 定義にする | reranker worker との protocol v1（`SKNR`／`SKNS`）の唯一の定義 | engine の TLA+ oracle（参照実装）を production から隔離する | #34 の休眠 context 研究コードを既定 build から外し、所在だけ残す |
| **Owns** | `FixedStr`／`FixedVec`／`Overflow`、上限定数（`MAX_CANDIDATES`、`MAX_PREEDIT_BYTES`、`MAX_SEGMENTS` ほか）、`Mode`／`KeyCode`／`KeyInput`／`Modifiers`、`AppearanceTheme`／`PadShortcut`、`InputScope`、`AiTextOperation`／`AiTextStatus`、`Fingerprint` | `input_history::format`（record layout、version、tag 0／1／2 の `HistoryScope`、`MAX_RECORD_BYTES`）、`input_history::codec`（record の encode・decode）、`persistence`（`input.bin` 配置、atomic write、30 日保持、64 MiB 上限、compaction の純関数）、`crypto`（`Sealer` trait と `DpapiSealer`） | magic、frame header、bounds、request／response の encode・decode、version 定数 | `*_oracle.rs` 12 モジュール（現 `engine/src`）と oracle 用 fixture | 現 engine の休眠 4 モジュール 1,846 行のみ |
| **Must not own** | wire codec（`Reader`／`Sink`／`encode`／`decode`）、`PROTOCOL_VERSION`、message／header、I/O、Windows API、ロジック（変換・keymap 解決） | writer thread・queue・`InputHistoryService`（engine）、`ScopeClass` 分類（engine）、CLI 表示（settings）、engine identity／session id（値として受け取るだけ）、`sakura_proto` の型 | ONNX、model manifest 検証、timeout／fallback 方針、候補 scoring の意味 | production から参照されるもの、`#[cfg(test)]` 外で使われる item | 既定 build から参照されるもの、runtime IPC、新規研究コード（新しい研究は新 Issue で新 crate） |
| **Allowed dependencies** | なし（`std` のみ） | `sakura-values`；target-specific `windows`（`persistence/` は `Win32_Foundation`／`Win32_Storage_FileSystem`、2.2c の `crypto/` は feature `dpapi`） | `sakura-values` | `sakura-engine`（dev）、`sakura-values`、`proptest` | `sakura-values`、`sakura-core` |
| **Allowed consumers** | 全 crate | `sakura-engine`、`sakura-settings`、`tools/*` | `sakura-engine`、`sakura-neural-worker` | `sakura-engine`（dev-dependency のみ）、cargo-mutants | `sakura-engine`（feature `context-research` のみ） |
| **Public API budget** | `pub` 型 ≤25、`pub fn` は値の構築・変換のみ（I/O 0） | `pub` 型 ≤12、`pub fn` ≤23（2.2b の 6 入口と 2.2c の `Sealer::seal`／`open` を含む） | `pub` item ≤8 | `pub mod` 12、`pub fn` は oracle 入口のみ | `pub` item ≤6 |
| **検査** | R12、`cargo tree -p sakura-values -e normal` | R13 | R8 | `rg -n 'oracle' target/release/deps/*.d` 空 | `rg -n 'sakura_context_research' crates/sakura-engine/src` が `#[cfg(feature)]` 配下のみ |

Phase 3 以降で分割される既存 crate（`sakura-user-prefs`、`sakura-install-maintenance`、`dictc-core`、`sakura-diagnostics`、`sakura-context-proto`）も同じ 6 項目を README に持つ（各ステップの PR に同梱）。「どこにも置けないものを置く場所」が要ると感じたら、それは境界の誤りであり、crate を増やすのではなく §3.1 の図を直す Issue を立てる。

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
  wire.rs     (proto に残す。values は値型のみ。§7 D3)
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
  input/ (raw_input)  events.rs (candidate/watch 共通 DTO の葉)  watch/  accessibility.rs  glyph.rs
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

### 3.3 自動検査できる依存規則

`ci/check-dependency-rules.ps1` を新設し、以下を毎 PR で実行する。全行 **空出力が合格**。

| # | 規則 | コマンド |
|---|---|---|
| R1 | core は proto を知らない | `cargo metadata --format-version 1` の resolve graph で `sakura-core → sakura-proto` が 0、かつ `rg -n 'sakura_proto' crates/sakura-core/src` が空 |
| R2 | settings は engine を知らない | `cargo metadata --format-version 1` の resolve graph で `sakura-settings → sakura-engine` が 0、かつ `rg -n 'sakura_engine' crates/sakura-settings/src` が空 |
| R3 | TSF session は Win32/COM を知らない | `rg -n 'use windows\|windows::' crates/sakura-tsf/src/session/` が空。Rust-aware module edge 検査は `host/engine → session/callback_deadline` だけを明示例外とし、他の `host → session` を拒否 |
| R4 | engine state は上位を知らない | `rg -n 'use crate::(keys\|commit\|render\|ipc\|request\|services\|runtime)' crates/sakura-engine/src/state/` |
| R5 | renderer の描画境界は逆向き依存を持たない | Rust-aware module edge 検査で `indicator → candidate`、`candidate → accessibility`、`candidate → watch` が 0。共通 request DTO は葉 `events.rs`、accessibility/watch との composition は `main` が所有 |
| R6 | pad_rail は pad を知らない | `rg -n 'crate::pad::' crates/sakura-renderer/src/pad/rail.rs` |
| R7 | settings ui presentation は葉 | `rg -n 'use (super\|crate)::' crates/sakura-settings/src/ui/presentation.rs` |
| R8 | reranker protocol は 1 owner | `sakura-rerank-proto` だけが request magic `0x524e_4b53`、response magic `0x534e_4b53`、version、`MAX_FRAME`、`MAX_CANDIDATES`、`MAX_CANDIDATE_BYTES` を定義し、`cargo metadata` で engine と neural-worker の両方が同 crate に依存する。`ci/check-dependency-rules.ps1` は定数名と両 magic 値の重複を production source 全体で拒否する（固定 wire bytes を検証する `neural-worker/tests/real_model_e2e.rs` の test fixture だけ許可） |
| R9 | test-only module の置き場 | `src/` では production module から `#[cfg(test)] #[path = "<name>_tests.rs"] mod tests;` で結ぶ sibling `*_tests.rs` と、明示した `testing.rs` だけ許可する。oracle と process／cross-module 専用 test は `sakura-oracles` または `tests/`。検査は許可名以外の `#![cfg(test)]` 専用 source を拒否する |
| R10 | 未使用 facade 再輸出なし | `ci/check-facade.ps1` が Cargo metadata の全 workspace package／target と supported API/docs を対象に、Rust-aware symbol resolver で `pub use` item の caller を解決する。コメント・文字列・同名別 symbol は caller に数えない |
| R11 | tools workspace も dep-policy 対象 | `ci/dep-policy.ps1` が `tools/*/Cargo.lock` も読む |
| R12 | values は wire を知らない | `rg -n 'sakura_proto\|wire::\|Reader\|Sink\|fn encode\|fn decode' crates/sakura-values/src`；`cargo tree -p sakura-values -e normal` が `sakura-values` 1 行のみ |
| R13 | store は runtime を持たない | Cargo metadata で normal direct dependency は `sakura-values`（2.2c 以降は `windows` も可）だけ。source scan は `std::thread`／`std::sync::mpsc`／`mpsc::`／`sakura_engine`／`sakura_proto`／`sakura_ipc`／session module path を拒否する。durable record の値フィールド `session` 自体は許可する |
| R14 | README の Allowed dependencies と `Cargo.toml` が一致 | `ci/check-dependency-rules.ps1 -Charters` が各 `crates/*/README.md` の `## Allowed dependencies` 節と `[dependencies]` を突き合わせる（2.8 で追加） |

### 3.4 テスト配置規約

1. インラインテストは **300 行を超えたら** sibling `*_tests.rs` に出す（300 行以下は同居可。行数だけで割らない原則に従う）。
2. private API の unit test を収める sibling `*_tests.rs` は `src/` に置いてよい。oracle は `sakura-oracles`、process／cross-module test は `tests/` に置く。
3. `#[ignore]` は理由を属性文字列に書く（`#[ignore = "needs desktop session"]`）。CI の scheduled job で `--ignored` を走らせる（§3.6）。
4. fake／fixture は `tests/support/` または `<crate>/src/testing.rs`（`#[cfg(any(test, feature = "test-support"))]`）に 1 箇所。TSF の fake pipe engine 5 種はここに集約。

### 3.5 文書配置

```
AGENTS.md                (≤1 KB。読む順序と禁止事項だけ)
CLAUDE.md                (≤8 KB。テスト出力規約、owner 判断へのリンク、Issue型→ディレクトリ表)
DESIGN.md                (≤40 KB。不変条件と境界だけ。詳細は docs/architecture/ へ)
docs/architecture/{README,conversion,ui,packaging,agent-refactor-plan}.md
docs/contracts/{input-scope,ipc-v1,ipc-pipe-security,ipc-server-trust,ipc-client-deadline,
                ipc-diagnostics,debug-trace,callback-deadline,write-journal,candidate-ui,
                dictionary-image,developer-history,learning-store,update-trust,neural-rerank,
                ai-text,context-protocol,registry-layout,keymap-format,config-format,
                engine-admin,dependencies,test-output}.md
docs/decisions/          (owner 判断、append-only、1 判断 1 ファイル、日付プレフィックス)
docs/history/{releases,issues,research}/   (release-notes-*.md 36 本、停止中調査、研究ノート)
docs/runbooks/           (vscode-crash-diagnostics-runbook.md など手順書)
crates/<crate>/README.md (≤4 KB。責務、公開 API、この crate に来る Issue 型、禁止依存、テストコマンド)
```

上の 23 件を契約ファイルの完全 inventory とする。件数を gate に直書きせず、`docs/contracts/README.md` の一意な filename／owner crate 集合とこの inventory の集合一致を検査する。機微スコープ規則は `docs/contracts/input-scope.md` を唯一の定義とし、他 9 箇所はリンクに置換する。

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

`ci/check-verification-correspondence.ps1`：各 `correspondence.json` の `path` が存在することに加え、`tools/architecture-check` の Rust parser が crate module tree と AST を辿り、module-qualified `symbol` が指定 path の item として **exactly one** 解決されることを検査する。コメント、文字列、別 module の同名 item、0 件、複数件は失敗する。`-SelfTest` は各失敗型と正常系を持つ。既存 artifact は `git mv` で履歴を保ち、新設する `verification/README.md` と各 `correspondence.json` は移動件数と別に検査する。

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

規模の目安：S = エージェント 1 セッション（30〜90 分）、M = 半日、L = 1 日超。

**PR 境界の原則**（「1 ステップ ＝ 1 PR」を機械的規則にしない）

1. **1 PR ＝ 1 変更理由 ＝ 1 つのレビュー可能な意味単位。** レビュアが「なぜこの diff か」を 1 文で言えない PR は分ける。
2. **構造変更・挙動変更・契約変更を同じ PR に混ぜない。** 構造 PR（`git mv`、分割、crate 新設）は `git diff -M --stat` で rename として見え、production の意味差分は 0。挙動変更 PR は構造を動かさない。契約変更（wire、ファイル形式、公開 API、`PROTOCOL_VERSION`）はこの計画の範囲外で、必要なら別 Issue。
3. **PR 自身の IRV を小さく保つ。** レビュアが読む production diff ≤1,500 行、読解対象は 1 crate ＋ その README ＋ 対応する契約文書 1 本まで。1 ステップがこれを超えるなら分ける（2.2a〜2.2d、5.10 のように）。逆に、同じ変更理由の機械的な小変更（≤50 行の rename が数本）は 1 PR にまとめてよい。
4. **新 crate を作る PR は文書を同梱する。** `crates/<crate>/README.md`（§3.1.1 の 6 見出し）、§3.3 への依存規則追加、`verification/irv/benchmarks.json` のファイル集合更新を同じ PR に含める。後回しにした README は書かれない。
5. **各 PR 本文は §9.2 のテンプレ**（Reads／Changes／Verify／Expect／IRV before→after）。

#### 4.1 Phase ごとの受け入れ基準（IRV 目標値）

Physical は `measure-irv.ps1` の `physical.loc`、Semantic は Phase 完了レビューで更新する `semantic.estimated_loc`。Phase 2 以降の根拠は §5 の after 列（各解析レポート §9 の必読見積り）。Phase 1 は #173 の訂正計測（`758c93f`→`ed712b4`）に基づき、挙動不変の分離後の実測を 100 LOC 単位で切り上げる（余裕 0〜99 LOC）。旧 `除テスト LOC` から導いた値は撤回する。計測元・scanner・manifest の hash と差分理由は `verification/irv/phase1-corrected.json`／`inline-accounting.md` に固定し、semantic 目標と後続 Phase の目標は変えない。

| Phase | ベンチマーク | Physical 目標 | Semantic 目標 | 根拠 |
|---|---|---:|---:|---|
| 0 | 全部 | 基準値を記録（変化なし） | 記録のみ | ゲートと基準値の固定が目的 |
| 1 | `IRV-ENGINE-KEY-MODE` | ≤10,100 | 900（不変） | 訂正実測 22,251→10,082、余裕 18 |
| 1 | `IRV-ENGINE-CANDIDATE` | ≤13,700 | 3,050（不変） | 訂正実測 27,531→13,630、余裕 70 |
| 1 | `IRV-TSF-REENTRANCY` | ≤11,700 | 910（不変） | 訂正実測 13,859→11,610、余裕 90 |
| 1 | `IRV-TSF-DUAL-KEY` | ≤11,400 | 540（不変） | 訂正実測 13,624→11,375、余裕 25 |
| 1 | `IRV-CORE-CONVERSION` | ≤7,600 | 450（不変） | 訂正実測 11,044→7,511、余裕 89 |
| 1 | `IRV-RENDERER-POPUP` | ≤4,600 | 700（不変） | 訂正実測 5,939→4,581、余裕 19 |
| 2 | `IRV-HISTORY-STORE` | ≤3,000、ファイル数 ≤8 | ≤950 | settings が engine の 3,632 行ファイルを読まなくなる。store 4 分割で読む範囲が 1 モジュールに |
| 2 | `IRV-CORE-CONVERSION` | proto をファイル集合から除く | 450 | R1 blocking |
| 3 | `IRV-CORE-CONVERSION` | ≤1,200 | ≤450 | §5：11,279→450 |
| 3 | `IRV-DICTIONARY-FORMAT` | ≤1,500 | ≤1,150 | §5：5,020→1,150＋writer |
| 3 | `IRV-ENGINE-CANDIDATE` | ≤4,000 | ≤3,050 | ranking 8 サブモジュールのうち Issue 型で 2〜3 個 |
| 4 | `IRV-ENGINE-KEY-MODE` | ≤1,200 | ≤900 | §5：19,600→900 |
| 4 | `IRV-HISTORY-STORE` | ≤1,200 | ≤950 | §5：7,000→950 |
| 4 | `IRV-ENGINE-CANDIDATE` | ≤3,500 | ≤3,050 | candidate_projection／prediction が `candidates/` に閉じる |
| 5 | `IRV-RENDERER-POPUP` | ≤1,200 | ≤700 | §5：3,730→700 |
| 6 | `IRV-TSF-REENTRANCY` | ≤1,200 | ≤910 | §5：9,700→910 |
| 6 | `IRV-TSF-DUAL-KEY` | ≤1,000 | ≤540 | §5：7,400→540 |
| 7（並走） | `IRV-DOCS`／無条件文書 | 無条件文書 ≤24,576 B、Issue 型別契約 ≤22,000 B | — | §3.5 の上限 |
| 7（並走） | `IRV-CI` | ≤1,200 | ≤600 | ci.yml＋変更対象 ps1 1 本 |

最終目標：10 ベンチマークすべてで **Physical ≤1,200 行、Semantic ≤1,000 行**。例外は 2 件で、`IRV-ENGINE-CANDIDATE`（Semantic 3,050：ranking は cost・learning・dedup・evidence を同時に理解しないと安全に変えられない。Physical ≤3,500）と `IRV-DICTIONARY-FORMAT`（Semantic 1,150：形式仕様・reader・writer を必ず一緒に読む契約。Physical ≤1,500）。この 2 件を無理に 1,000 以下へ割ると「分割しただけで Semantic が下がらない」状態になるため、目標にしない。

Phase 完了 PR は `measure-irv.ps1 -Out verification/irv/baseline.json` を再実行して基準値を更新し、上表の該当行を PR 本文に before→after で貼る。目標未達なら Phase を閉じない。

### Phase 0：ゲート整備（コード移動なし、9 ステップ）

目的：**main を保護し、以後の全 PR が同じゲートを通る状態を作る**。現状（2026-09-12 に `gh api` で確認）：リポジトリは public、`main` に branch protection なし（`/branches/main/protection` が 404）、ruleset なし（`/rulesets` が `[]`）。push 権限のある利用者が PR を経ず直接 push できる。2026-09-13 の読み取り確認では、現在の認証に `permissions.admin=true` があり、ruleset は引き続き `[]`。これは Phase 0.6 で適用できる権限があることの確認であり、適用済みという意味ではない。

merge gate の構成（0.6 で必須チェックにする job 名）：

| job | 内容 | 由来 |
|---|---|---|
| `fmt` | `cargo fmt --all -- --check`、`git diff --check` | 既存 |
| `dependency-rules` | `ci/check-dependency-rules.ps1`（R1〜R14、blocking 化済みのみ FAIL） | 0.3 |
| `crate-tests` | 変更 crate と下流だけ `cargo test -p`（7.8 の matrix。それまでは `workspace-tests` と同一） | 7.8 |
| `workspace-tests` | `./ci/run-test-quiet.ps1 -Name 'workspace tests' -Command { cargo test --workspace }` | 既存 `ci.yml:80` |
| `irv-regression` | `scripts/measure-irv.ps1 -Compare verification/irv/baseline.json` | 0.8 |
| `dll-size` | `ci/check-dll-size.ps1` | 0.1 |
| `formal-verification` | TLC。0.4 では非ブロッキング、7.9 で必須に | 0.4／7.9 |

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 0.1 | DLL サイズゲート | `ci/check-dll-size.ps1` を `ci.yml` の `cargo build -p sakura-tsf --release` 後に追加する。script は `cargo metadata` の `target_directory` と `.cargo/config.toml` の `build.target` を解決し、現行設定では `target/x86_64-pc-windows-msvc/release/sakura_tsf.dll` を検査する | `pwsh ./ci/check-dll-size.ps1 -SelfTest`；CI 実行 | SelfTest が超過ダミーで失敗・正常で成功。実際に build した DLL が存在し、閾値以下 | S |
| 0.2 | package／target 単位テストの基準値記録 | `cargo metadata --no-deps --format-version 1` の workspace package と各 target kind から実行集合を作り、package ごとに同じ `cargo test -p <package>`（必要な既存 feature を基準 JSON に明記）を走らせ、lib／bin／integration／doc-test の結果を `ci/test-baseline.json` に記録する。`--lib` 固定や renderer だけの例外は置かない | `pwsh ./ci/record-test-baseline.ps1`；同 script の `-Compare ci/test-baseline.json` | metadata 上の全 workspace package と testable target が JSON に現れ、baseline と比較が同一 command universe を使う。件数は採取結果を記録し、過去の workspace 合計や crate 固定数を期待値にしない | S |
| 0.3 | 依存規則スクリプト | `ci/check-dependency-rules.ps1`（§3.3 の R1〜R9。R1・R2 は source と `cargo metadata` の直接 edge の両方を検査。現状違反する規則は `-Advisory` で警告のみ） | `pwsh ./ci/check-dependency-rules.ps1` | R1〜R9 の各結果が出て、blocking 化前の既知違反は列挙して exit 0。R10 は 3.5、R11 は 3.10 で実装・有効化する | S |
| 0.4 | 形式検証 workflow（非ブロッキング） | `.github/workflows/formal-verification.yml`：tla2tools.jar を SHA-256 固定で取得し、repository から model／cfg inventory を列挙して matrix 化する（2026-09-13 snapshot は 10 models／48 cfg）。1 cfg 20 分 timeout、既存 runner の process lifecycle を流用する。各 cfg を `must-pass` または `expected-counterexample` として manifest に分類し、後者は期待した invariant／property の反例を検出したとき job 成功、反例なし・別種の失敗・timeout は失敗に正規化する。workflow/job は 7.9 まで `continue-on-error: true` の manual 非ブロッキング | workflow_dispatch で実行し、列挙 inventory と matrix 集合を比較 | 列挙した全 cfg が完走し、分類後の全 job が緑。expected counterexample は反例検出を成功 assertion として記録する。分類と正規化が済むまでは blocking 候補にしない | M |
| 0.5 | `AGENTS.md` と `CLAUDE.md` の縮約 | `AGENTS.md` 新設（≤1 KB）、`CLAUDE.md` から §最優先タスク（10,147 B）を `docs/history/issues/vscode-crash-investigation-20260802.md` へ移動、stale 行番号 5 箇所を修正、`apply_patch` 記述を削除し、SolAdvisor 記録も履歴側へ移す（D1） | `wc -c CLAUDE.md AGENTS.md`；`rg -n 'dispatch.rs:1039\|keymap.rs:1295\|496 passed\|apply_patch' CLAUDE.md`；`scripts/measure-irv.ps1 -Compare` | ≤8,192 B／≤1,024 B。rg 空。無条件文書 bytes が 85,770 から減少 | S（D1 決定済み、2026-09-13 owner 委任） |
| 0.6 | main 保護 ruleset と merge gate | GitHub **ruleset**（classic branch protection ではなく）を `main` に作成：PR 必須（approval 0 でよいが PR 経由必須）、直接 push・force push・削除禁止、必須 status check ＝ 上表の `fmt`／`dependency-rules`／`workspace-tests`／`irv-regression`／`dll-size`（`crate-tests` は 7.8 後、`formal-verification` は 7.9 後に追加）、bypass actor 無し。定義 JSON を `ci/rulesets/main.json` に commit し、Phase 0 の実装担当が repo admin 権限を持つ認証済み `gh` で `gh api -X POST repos/{owner}/{repo}/rulesets --input ci/rulesets/main.json` を実行する。緊急時も同じ PR と必須 status check を通し、管理者権限で check を迂回しない。本計画の更新時点では ruleset を実適用せず、Phase 0 の実装 Issue で適用する。ruleset を選ぶ理由：JSON で export／再適用でき、複数を重ねられ、必須チェックの追加が差分 commit で追える（classic は UI 設定で履歴が残らない） | `gh api repos/tsuyoshi-otake/sakura-input/rulesets --jq '.[].name'`；`gh api repos/tsuyoshi-otake/sakura-input/rulesets/{id}` で `enforcement`・`conditions`・`bypass_actors`・必須 check を照合；`gh api repos/tsuyoshi-otake/sakura-input/rules/branches/main` で `main` に実際に適用される規則を照合；必須 check が未完了の検証 PR を `gh pr view <number> --json mergeStateStatus,statusCheckRollup` で確認 | `main-protection` 1 件が active かつ `main` 対象で、bypass actor は空。実効規則に `pull_request`・`required_status_checks`・`non_fast_forward`・`deletion` を含み、指定した必須 check が一致する。必須 check が未完了の検証 PR は `BLOCKED` | S（D12 決定済み、2026-09-13 owner 委任。適用者に repo admin 権限が必要） |
| 0.7 | IRV 基準値の固定 | `verification/irv/benchmarks.json`（10 ベンチマーク、§1.1.1）、`scripts/measure-irv.ps1`、`verification/irv/baseline.json` を commit（PR #153 に同梱済み） | `pwsh ./scripts/measure-irv.ps1 -SelfTest`；`pwsh ./scripts/measure-irv.ps1 -Compare verification/irv/baseline.json` | SelfTest PASS。同一ツリーで各ベンチマーク ok、無条件文書だけ予算超過 | S |
| 0.8 | `irv-regression` CI job | `ci.yml` に job 追加（`-Compare` を直接実行、パイプ無し）。0.5・7.11 の両方が merge されるまでは `-DocsBudgetMode Warn` を使う（予算超過中の増加だけ WARN、非増加は note／PASS）。両工程のうち後に完了する工程の PR で予算内を証明して FAIL mode へ切り替え、その次の PR から増加の有無にかかわらず予算超過を失敗させる（D14）。PR 本文の `IRV:` 行を必須にする PR テンプレ | 導入時はダミー PR で `dispatch.rs` に基準 Physical LOC × WARN 閾値を超える行数を追加（現 baseline 21,664 × 0.1 に対し 2,167 行）→ WARN、`text_service.rs` に 3,400 行追加 → FAIL を確認して close。0.5／7.11 のうち後に完了する工程の PR では `-DocsBudgetMode Warn` で無条件文書 ≤24,576 B を確認し、CI を FAIL mode に切り替えた状態でも同じ tree の `-Compare` が成功することを確認 | 0.5・7.11 の両方が merge されるまでは、文書予算超過かつ増加なら WARN、非増加なら note／PASS。その次の PR からは増加の有無にかかわらず予算超過が FAIL。その他の判定は常に §1.1.2 の表どおり | S（D14 決定済み、2026-09-13 owner 委任） |
| 0.9 | 文書の前倒し（crate 責務・依存規則・不変条件・所有・契約・移行規則） | `docs/architecture/README.md`（crate 図 §3.1、R1〜R14、憲章テンプレ §3.1.1、移行規則「構造 PR は `git mv` 先行、挙動変更と混ぜない」）、`docs/templates/crate-readme.md`（Purpose／Owns／Must not own／Allowed dependencies／Allowed consumers／Public API budget／Issue 型／テストコマンド）、`CODEOWNERS`（crate ごとの owner、当面は全て owner 1 名）、`docs/contracts/README.md`（§3.5 の完全 inventory と owner crate。本文は 7.5）。旧 7.13 をここに統合 | `wc -c docs/architecture/README.md`；`rg '^\| R[0-9]+ \|' docs/architecture/README.md`；`grep -c '^## ' docs/templates/crate-readme.md`；`ls docs/contracts/README.md CODEOWNERS` | ≤8,192 B。R1〜R14 の重複・欠落 0。8 見出し。存在 | S |

### Phase 1：テスト分離（挙動不変、11 ステップ）

原則：production の挙動を変えない。test module／path 宣言、renderer の薄い entry wrapper と機械的な整形を除き、移動元の production と test body を保存する。`git diff --stat` の削除行数だけでなく、実コードとテスト identity を照合する。**この Phase で下がるのは Physical IRV だけ**であり、各 PR の `IRV:` 行には `semantic unchanged` と明記する（§1.1）。受け入れ基準は #173 で訂正した §4.1 の Phase 1 行（`IRV-ENGINE-KEY-MODE` ≤10,100 行など）。desktop と R9 の条件も別に満たす。

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
| 1.10 | scheduled desktop job | D6 で admission 済みの GitHub-hosted `windows-latest` に `.github/workflows/desktop-tests.yml`（weekly＋dispatch、`all`／`settings`／`renderer`、直列実行）を作る。Cargo metadata と `--list --ignored` の集合を manifest と照合し、settings 21 件、renderer 11 件中 manual 900 秒 harness 1 件を明示除外して自動 10 件を固定する | workflow_dispatch で対象 test を列挙し、capability、各 exact identity と `1 passed`、process cleanup、結果 artifact を確認 | 選択した全対象が 1 件ずつ実行される。初回 admitted full-suite run の結果を Phase 1.10 の完了証拠にする | M |

Phase 1.10 の prerequisite 計測は commit `98ef18c18c94efdab9744aae93f0c97bb916d208`、GitHub Actions run `34713825191`／job `103607261803` で合格した。session 2、`WinSta0`／`Default` input desktop、cursor／screen、exact tab-focus test 3 回、process cleanup を artifact で確認したため D6 は GitHub-hosted `windows-latest` に決定した。根拠と digest は `docs/architecture/desktop-runner-admission.md` に固定する。この prerequisite は runner admission の証拠であり、全 ignored suite の合格証拠ではない。
| 1.11 | R9 を blocking に | 0.3 の `-Advisory` から R9 を外す | `pwsh ./ci/check-dependency-rules.ps1` | R9 違反 0 | S |

### Phase 2：葉 crate 抽出（依存の逆流を消す、15 ステップ）

各 crate の憲章は §3.1.1。新 crate を作るステップは README・依存規則・`benchmarks.json` 更新を同じ PR に含める（§4 冒頭 4.）。

**2.1 の設計（`sakura-values` と wire codec の方向）**。現状は `crates/sakura-proto/src/types.rs` が `use crate::wire::{Error, Reader, Sink}`（:15）を持ち、`KeyCode`（:71/:77）、`Modifiers`（:216/:222）、`KeyInput`（:243/:252）、`Mode`（:297/:303）、`AppearanceTheme`（:358/:364）、`PadShortcut`（:412/:418）、`InputScope`（:456/:462）が固有メソッド `encode<S: Sink>`／`decode(&mut Reader)` を持つ。値型をそのまま values に移すと values が wire codec に依存し、目的（`sakura-values ← core, proto`、`proto ← wire codec`）と逆になる。よって：

- **values が持つもの**：型定義、`repr`、判別値（`as u8` など）、`Default`／`Eq`／`Hash`、値としての不変条件（`FixedStr` の容量、`Modifiers` のビット）。
- **proto が持つもの**：`wire.rs`（`Reader`／`Sink`／`Error`）と、新設 `wire_types.rs` の `pub trait Wire { fn encode<S: Sink>(&self, s: &mut S) -> Result<(), Error>; fn decode(r: &mut Reader<'_>) -> Result<Self, Error>; }` および `impl Wire for sakura_values::Mode` 等の実装。バイト表現は現行 `types.rs` の本体をそのまま移すので **wire 互換**。
- **下流の呼び出し**：`Mode::encode(&m, &mut sink)` は `use sakura_proto::Wire;` を足せば同じ式で動く（trait メソッド）。`message.rs` 内の呼び出しは同一 crate なので import 1 行。
- **`PROTOCOL_VERSION`**：`lib.rs:63` の `22` を変えない。バイト列が変わらないことは 2.0 の golden fixture で固定する。

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 2.0 | wire golden fixture | 変更前に、代表 message（`KeyInput`×3、`Mode`×6、`AppearanceTheme`×2、`PadShortcut`×2、`InputScope` 全値、request/response 各 1）の encode 結果を hex で `crates/sakura-proto/tests/golden_v22.rs` に固定。挙動変更なし | `cargo test -p sakura-proto --test golden_v22` | 全 pass（自分自身と一致） | S |
| 2.1a | `sakura-values` 新設（下流の参照を維持） | `crates/sakura-values`（`fixed.rs`、`capacity.rs`、`input.rs`、`appearance.rs`、`scope.rs`、`ai_text.rs`）。proto は `pub use sakura_values::{...}` で同名再輸出し、codec は `wire_types.rs` の `impl Wire for` に移す。README（憲章 8 見出し）、R12 行、`benchmarks.json` 更新を同梱 | R12；`cargo tree -p sakura-values -e normal`；`cargo test -p sakura-proto`（roundtrip／robustness／zero_alloc／golden_v22）；`rg -n 'PROTOCOL_VERSION: u16 = 22' crates/sakura-proto/src/lib.rs`；`rg -n '^impl Wire for' crates/sakura-proto/src/wire_types.rs` と移動した 9 型の旧 impl がないことを確認；`cargo build --workspace` | R12 空。依存 0 行。全 pass、golden 一致。1 件。移動した 9 型の codec は `wire_types.rs` の 9 impl のみ。`types.rs` に残る複合型の codec は維持。下流無変更で build 成功 | M |
| 2.1b | core を values に向ける | `crates/sakura-core` の 18 参照（`calendar.rs:6`、`conversion.rs:12-13`、`dictionary.rs:11`、`editing.rs:7,325`、`input_repair.rs:10,1007`、`keymap.rs:33`、`lib.rs:74`、`numerals.rs:7`、`preferences.rs:13`、`romaji.rs:29`、`simd.rs:1762`、`text.rs:14`、`user_dictionary.rs:8`、`width.rs:20,830`）を `sakura_values::` に。`Cargo.toml` から `sakura-proto` を外す | R1；`cargo tree -p sakura-core -e normal \| rg proto`；`cargo test -p sakura-core` | R1 空。proto が出ない。件数不変 | S |
| 2.1c | `Fingerprint` を values へ | 2.3 の前提。`engine/src/long_conversion.rs` の候補 fingerprint 型を `values/fingerprint.rs` に | `cargo test -p sakura-engine --lib long_conversion` | 不変 | S |
| 2.2a | `sakura-store::input_history::{format,codec}`（純関数） | `engine/src/input_history.rs` の record 定義（:141-213）、`MAX_RECORD_BYTES` 16 KiB（:39）、`InputHistorySnapshot`（:423）、`InputHistoryStats`（:570）の **型と encode/decode** を `crates/sakura-store/src/input_history/{format,codec}.rs` に。on-disk scope は既存 tag 0／1／2 を保つ store-owned `HistoryScope` とし、6 値の `InputScope` および `ScopeClass::from_scope`／privacy exclusion は engine に残す。store の AI enum は 2.1a の values を使い、engine の `input_history.rs` は values と store を直接参照する。engine は `pub use` で同名参照。README・R13 行・`benchmarks.json` 同梱 | R13；`cargo test -p sakura-store`；`engine/src/input_history_tests.rs` の固定 plaintext payload 4 種を移動後 codec で decode→encode | R13 空。roundtrip pass。固定 payload byte 一致、scope tag 0／1／2 不変 | M |
| 2.2b | `sakura-store::persistence`（保持・compaction の純関数とファイル配置） | `RETENTION` 30 日（:41）、`MAX_INPUT_HISTORY_BYTES` 64 MiB（:47）、compaction の選別規則を `persistence.rs` に **入力 `&[Record]` → 出力 `Vec<Record>`** の純関数として。`%LOCALAPPDATA%\SakuraInput\history\input.bin` の path 規則と atomic write も同モジュール。writer loop（:1215）・queue・`InputHistoryService`（:674）・`ScopeClass`（:59）は **engine に残す**（4.1 で `history/service.rs`） | `cargo test -p sakura-store --lib persistence`；TLA+ `DeveloperHistory` の compaction 不変条件を correspondence に紐付け | 30 日／64 MiB の境界テスト pass。engine 側の行数が減るだけで挙動不変 | M |
| 2.2c | `sakura-store::crypto`（DPAPI を trait の背後に） | `input_history.rs:27-32` の DPAPI 呼び出しを `trait Sealer { fn seal(&[u8]) -> Result<Vec<u8>>; fn open(&[u8]) -> Result<Vec<u8>>; }` と `DpapiSealer`（feature `dpapi`、既定 on）に。テストは `PlainSealer` | `cargo test -p sakura-store --no-default-features`；`cargo test -p sakura-store`；既存 DPAPI ファイルの読み戻し | 非 Windows 依存でテスト可。既存ファイルが読める | S |
| 2.2d | settings を store に向ける | `settings/src/learning.rs:6`、`settings/src/input_history.rs:8` の `sakura_engine::` を `sakura_store::` に。`Cargo.toml` から `sakura-engine` を外す | R2；`cargo tree -p sakura-settings -e normal \| rg engine`；`cargo test -p sakura-settings`；`history show/export/stats` 統合テスト | R2 空。engine が出ない。pass。`input.bin` 形式不変 | S |
| 2.3 | `sakura-rerank-proto` | `neural-worker/src/protocol.rs:3-8` と `engine/src/long_conversion.rs:28-29` を 1 crate に（`Fingerprint` は 2.1c 済み）。README・R8 更新・`benchmarks.json` 同梱 | R8；`cargo test -p sakura-neural-worker`；2 候補 protocol v1 IPC テスト | R8 が 1 件。worker と engine が同じ定義を使う | S |
| 2.4 | `sakura-oracles` | engine の `*_oracle.rs` 12 モジュールを dev-dependency crate へ。`lib.rs:66` の常時 compile を削除。cargo-mutants 設定を repoint。README 同梱 | `cargo build -p sakura-engine --release` の後 `rg -n 'oracle' target/release/deps/*.d`；`cargo test -p sakura-engine` | release binary に oracle シンボル無し。テスト件数不変 | S |
| 2.5 | `sakura-context-research` | #34 休眠 4 モジュール 1,846 行を feature `context-research` 付き別 crate へ。README の Owns は 4 モジュール限定、Must not own に「既定 build から参照されるもの」。crate 化して残し、1 年間 feature が使われなかったことを確認した場合だけ、所在を `docs/history/research/` に記録する別の削除 PR で廃止する。期限到来だけで自動削除しない | `cargo build -p sakura-engine`；`cargo build -p sakura-engine --features context-research`；`rg -n 'sakura_context_research' crates/sakura-engine/src --glob '!**/*_tests.rs'` | 既定 build から 1,846 行が消える。feature 付きで従来どおり。参照は `#[cfg(feature)]` 配下のみ。削除は 1 年間の未使用を確認した別 PR でのみ行われる | S（D13 決定済み、2026-09-13 owner 委任） |
| 2.6 | engine dev-deps 整理 | `ime-eval` dev-dep を削除（`tests/pipe_round_trip.rs:36-37` は ime-eval 側の integration test へ）、`dictc` は feature `dev-fixtures` 背後 | `cargo tree -p sakura-engine -e dev`；`cargo test -p sakura-engine --features dev-fixtures` | ime-eval が出ない。テスト pass | S |
| 2.7 | `sakura-context-proto` 改名 | `sakura-neural-proto` → `sakura-context-proto`（`SCV1` は不変） | Cargo metadata の package／dependency 名と active source、manifest、script、workflow を `rg -n 'sakura_neural_proto\|sakura-neural-proto' Cargo.toml crates tools ci scripts .github` で検査。`docs/history/` と本計画を含む移行記録は対象外 | build graph と active 実行参照は空。履歴・移行記録の旧名は provenance として保持 | S |
| 2.8 | R1・R2・R8・R12・R13 を blocking に | 0.3 の `-Advisory` から外す | `pwsh ./ci/check-dependency-rules.ps1` | 違反 0 | S |
| 2.9 | Phase 2 受け入れ | `measure-irv.ps1 -Out` で基準値更新。§4.1 の Phase 2 行を PR 本文に | `pwsh ./scripts/measure-irv.ps1 -Compare`（更新前基準と） | `IRV-HISTORY-STORE` ≤3,000 行・≤8 ファイル。`IRV-CORE-CONVERSION` の集合から proto が消える | S |

### Phase 3：core／proto／ipc／reg／dictc の分割（10 ステップ）

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 3.1 | `conversion/` 分割 | §3.2 の 8 サブモジュール。公開 API（`convert`, `ConversionOptions`, `Candidate`）は `conversion/mod.rs` で維持 | `cargo test -p sakura-core --lib conversion`；`cargo bench`（あれば）；`tools/ime-eval` の固定コーパス比較 | テスト件数不変。ime-eval の recall／top-1 が bit 一致 | L |
| 3.2 | `dictionary/` 分割 | `format.rs` を唯一定義に。`dictc/tests/image.rs` の適合テストを core 側にも複製（reader 視点） | `cargo test -p sakura-core --lib dictionary`；辞書再ビルド | 通常ビルド辞書が 39,349,040 bytes、SHA-256 `b7d08643…` で一致 | M |
| 3.3 | `width/`＋simd | `width/scan/` に simd を収容 | `pwsh ./ci/check-simd-assembly.ps1`；`cargo test -p sakura-core --features simd-assembly-audit` | 成功 | M |
| 3.4 | `romaji/`、`keymap/`、`preferences/` | §3.2 どおり | `cargo test -p sakura-core --lib` | 件数不変 | M |
| 3.5 | facade 剪定 | `cargo metadata` が列挙する全 workspace package／target、docs と supported API の caller 調査で 0 件と証明できた再輸出だけ削除する。証明できないものは残す | `cargo build --workspace --all-targets`；`ci/check-facade.ps1`；R10 を実装して blocking 化 | build 成功。削除した各 item に caller 0 の証跡があり、未使用再輸出 0。件数を先に固定しない | S（D4 決定済み、2026-09-13 owner 委任） |
| 3.6 | `proto/message/` 分割 | tags/header/request/response/ui_state | `cargo test -p sakura-proto` | codec テスト全 pass、`PROTOCOL_VERSION` 22 | S |
| 3.7 | `ipc/security/` 分割 | admission（:88-141）と server_trust（:151-320, 453-513）を分離 | `cargo test -p sakura-ipc` | #104 のテストが `server_trust_tests.rs` に閉じる | S |
| 3.8 | `sakura-reg` 分割 | Credential Manager→`sakura-user-prefs`、maintenance/launcher/vscode_diagnostics→`sakura-install-maintenance`、`lib.rs:1-9` の偽記述を修正 | `cargo test -p sakura-reg -p sakura-user-prefs -p sakura-install-maintenance`；`ci/dep-policy.ps1` | pass。TSF DLL サイズ不変（0.1） | M |
| 3.9 | `dictc-core`＋thin `dictc` | lib を `dictc-core` へ、dictc を import しない 3 bin（2,323 行）を `tools/` へ | 辞書 2-pass 再ビルド | 39,349,040 bytes／SHA-256 `b7d08643…` 一致 | M |
| 3.10 | tools workspace 統合 | `candidate-sweep` を workspace member に、`candidate-snapshot` は dep-policy が `Cargo.lock` を読む | `pwsh ./ci/dep-policy.ps1` | tools の lock も監査される（R11） | S |

### Phase 4：engine の分割（12 ステップ）

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
| 4.9 | `Session` 分割 | 148 field を `state/{mode,composition,conversion,candidates,prediction}.rs` に | `cargo test -p sakura-engine` | 不変 | L（D5 決定済み、2026-09-13 owner 委任。Phase 4 最後の単独 PR） |
| 4.10 | verification 対応更新 | `verification/sakura-engine/*/correspondence.json` を新モジュールパスに | `pwsh ./ci/check-verification-correspondence.ps1` | 解決 100% | S |
| 4.11 | engine README | `crates/sakura-engine/README.md` ≤4 KB | `wc -c` | ≤4,096 B | S |
| 4.12 | R4 blocking＋file budget 報告 | `ci/report-file-budget.ps1`（1,500 行超の production ファイルを警告、非ブロッキング） | 実行 | engine で 1,500 行超が 0 | S |

### Phase 5：renderer／settings の分割（12 ステップ）

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
| 5.9 | topic registry | `ui/topics/` に 15 topic、`GeneralControls` 68 HWND を topic ごとに | D6 で選んだ interactive User32 desktop runner 上で、実装時に metadata から列挙した ignored desktop test（`tab_focus_order_skips_hidden_topics_and_ends_at_actions` 含む）を scheduled job で実行 | 列挙対象が全 pass。D6 の環境証拠が揃うまでは本 step を開始しない | L（D6 証拠ゲート） |
| 5.10 | topics を 1 つずつ移動 | 1 PR 1〜3 topic | 同上 | 同上 | M×5 |
| 5.11 | `cli/` 分割 | parse/run/render | `cargo test -p sakura-settings --lib cli` | 不変 | S |
| 5.12 | `config/`、`history/`、`engine/` | §3.2 どおり | `cargo test -p sakura-settings` | 不変 | S |

### Phase 6：TSF の分割（11 ステップ、後半 3 本は証拠ゲート付き）

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
| 6.9 | `session/candidate_board.rs` | `DualTsfCandidateBoard` 7 cfg と 1:1。`GuardForeignCandidateEnd`／`RestoreCurrentPlacement` は D7 と Issue #7 の実 dump／report 証拠を検証した後だけ扱う | `cargo test -p sakura-tsf --lib session::candidate_board`；TLC 7 cfg | pass。Issue #7 のコメントにある filename／report 記述だけを証拠済みとみなさない | L（D7・#7 証拠ゲート） |
| 6.10 | `com/text_service.rs` 残余分割 | ≤1,300 行（vtable 572＋委譲） | `wc -l crates/sakura-tsf/src/com/text_service.rs`；DLL サイズ；実機 smoke（メモ帳／VS Code で入力・確定・focus 移動） | ≤1,300。≤1 MiB。クラッシュ・ハング無し | L |
| 6.11 | TSF README＋`docs/contracts/write-journal.md` | | `wc -c` | ≤4 KB | S |

### Phase 7：文書・verification・CI（Phase 0 から並走、12 ステップ）

前倒し済みの項目：crate 図・依存規則・憲章テンプレ・移行規則・`CODEOWNERS`・契約一覧は 0.9、crate README は各 crate を新設または分割する PR（2.1a、2.2a、2.3〜2.5、4.11、6.11）に同梱。ここに残るのは既存文書の整理と検査の自動化だけ。

| # | ステップ | 変更 | Verify: | Expect: | 規模 |
|---|---|---|---|---|---|
| 7.1 | `docs/decisions/` 抽出 | CLAUDE.md の owner 判断 4 件（テスト出力 #111、IT エンジニア・ファースト、リリース署名、trust state #150）を 1 件 1 ファイルに。CLAUDE.md はリンク | `wc -c CLAUDE.md` | 段階的に ≤8 KB | S（D1 決定済み、2026-09-13 owner 委任） |
| 7.2 | `docs/history/` へ退避 | release-notes 36 本、research、停止中調査 | `ls docs` | `docs/` 直下は ≤10 ファイル | S |
| 7.3 | 既存 crate README の残り | 0.9 のテンプレで、Phase 2〜6 の PR に同梱されなかった workspace member のうち `crates/` 直下に manifest を持つ crate の README を書く | `cargo metadata --no-deps --format-version 1` から対象 manifest 集合を作り、各親 directory の `README.md` と 8 見出しを検査 | metadata が返す対象集合と README 集合が一致し、各 ≤4,096 B、各 8 見出し | S |
| 7.4 | DESIGN.md 分割 | 不変条件だけ残し、詳細を `docs/architecture/{conversion,ui,packaging}.md` へ | `wc -c DESIGN.md` | ≤40,960 B | M（D2 決定済み、2026-09-13 owner 委任） |
| 7.5 | `docs/contracts/` 完全化 | §3.5 の完全 inventory を 1 契約 1 文書で整備し、各文書に canonical code、owner crate、version／compatibility、verification artifact を記す。辞書 image reader／writer の全 tag・version・optional table 契約も `dictionary-image.md` に列挙 | `docs/contracts/README.md` と §3.5 の filename／owner 集合を比較；canonical site からの back-reference を検査；`rg -c 'Password.*URL.*Email.*Digits' --glob '!docs/contracts/input-scope.md' .` | inventory の欠落・重複・owner 重複 0。辞書 image contract の reader／writer inventory 一致。機微スコープ規則の再掲 0 | M |
| 7.6 | verification 再編 | §3.6。既存 artifact は `git mv` のみで内容不変、新設する README／correspondence は別集計する。`.gitignore:24` と tracked artifact の矛盾を解消 | 移動前後の committed path mapping を比較し、各既存 artifact の `git log --follow` が繋がることを検査；新設 artifact は schema／correspondence check で検査 | 移動対象の欠落・重複 0。新設 artifact が移動件数へ混入しない | M |
| 7.7 | `correspondence.json`＋検査 | 10 モデル分。`ci/check-verification-correspondence.ps1 -SelfTest` | 実行 | 解決 100%、SelfTest pass | M |
| 7.8 | crate 単位 CI matrix | `ci/changed-crates.ps1` が `git diff --name-only origin/main...HEAD` と `cargo metadata` の実 dependency graph から影響 package／target（依存下流含む）を JSON matrix にする。`data/`、root manifest／lockfile、`.cargo/`、toolchain、共通 `scripts/`／`ci/`／workflow は明示 mapping を持たせ、mapping のない shared path は full workspace へ fail-safe fallback。`ci.yml` は matrix job＋週次 full | crate-local、mapped shared path、未知 shared path の self-test | crate-local は当該 package と実下流、mapped shared path は定義対象、未知 shared path は full workspace が走る | M（#89） |
| 7.9 | 0.4 の TLC workflow をブロッキングに | expected-counterexample cfg の期待結果を 0.4 で分類・正規化し、全 cfg がその意味で green になった後にだけ `continue-on-error` を除去 | workflow 実行と cfg ごとの分類表照合 | success cfg は正常完了、expected-counterexample cfg は期待した invariant violation／trace の一致を成功として報告し、unexpected result は失敗 | S（D8 決定済み、2026-09-13 owner 委任） |
| 7.10 | scripts 整理・`rtk` 残滓除去 | `scripts/` を build／verify／release に分け、active script／CI／workflow／crate test tree の実行参照から `rtk` を削除。履歴文書・fixture data は分類して検索対象から除外 | `rg -n 'rtk' scripts ci .github crates --glob '*.ps1' --glob '*.yml' --glob '*.yaml' --glob '*/tests/**'` と除外一覧の監査 | active 実行参照 0。除外は履歴または data として明示分類 | S |
| 7.11 | `rules.md` 分割 | `.claude/memory/rules.md` 45,652 B を topic 別に（削除せず移動） | `wc -c .claude/memory/rules.md` | ≤16 KB＋`rules/<topic>.md` | S（D9 決定済み、2026-09-13 owner 委任） |
| 7.12 | Issue 型→ディレクトリ表 | §3.7 を CLAUDE.md に | `rg -n 'Issue 型' CLAUDE.md` | 1 件 | S |
| 7.13 | （0.9 に統合） | `docs/architecture/README.md` は Phase 0 で作成済み。ここでは Phase 6 完了後の crate 図更新のみ | `rg '^\| R[0-9]+ \|' docs/architecture/README.md` を rule ID 集合として検査 | R1〜R14 の重複・欠落 0 | S |

---

## 5. 読解セットの before → after

行数＝Issue 修正時に必ず読む production コード（**Physical IRV の見積り**。`benchmarks.json` の計測値とは集合が少し違う：この表はテストを除いた production 行、計測はファイル全体）。散文＝必読文書 bytes。after は各解析レポートの見積りで、§4.1 の Phase 目標値はこの after 列を切り上げたもの。Semantic IRV は §1.1.1 の表を正とする。

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

Phase 1（テスト分離）の訂正実測では、§4.1 の 6 読解集合の Physical LOC は約 16〜55% 減る。すべてが 40〜60% 減るという旧見積りは撤回する（#173）。Phase 2 で crate 越しの読解（proto codec、engine の store）を減らし、残りは Issue 型に対応する責務分離で削減する。

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
| 大 PR による review 不能 | §4 冒頭の PR 境界原則（1 PR ＝ 1 変更理由、構造／挙動／契約を混ぜない、production diff ≤1,500 行） |
| 分割したのに Issue が楽にならない（Physical だけ下がる） | Phase 受け入れを Semantic IRV でも判定（§4.1）。`semantic.ranges` を Phase 完了レビューで更新し、行範囲の実在を `sed -n` で確認 |
| 新 crate が「置き場のないものの置き場」になる | 憲章（§3.1.1）の Must not own と Public API budget を README に置き、`rg` で検査（R12、R13）。名前を広く取らない（`sakura-context` → `sakura-context-research`） |
| main へ直接 push されてゲートが素通りする | 0.6 の ruleset（PR 必須、force push・削除禁止、必須チェック）を Phase 1 の前に有効化 |
| `#[ignore]` テストの黙殺 | 1.10 の scheduled job を Phase 5 の前に稼働させる |
| CI 時間の増加 | 7.8 の metadata-driven matrix で crate-local PR の実行集合を狭める。所要時間目標は導入時の計測値から別途設定し、full は週次と release |

---

## 7. Owner 判断が必要な項目

D1、D2、D4〜D6、D8〜D14 は 2026-09-13 に owner が判断を委任し、下表の内容で決定した。D3 はそれ以前から計画上「決定済み扱い」である。D7 は Issue #7 の実 dump／report 証拠が揃うまで未決とする。

| ID | 判断 | 影響ステップ | 決定内容／未決項目の既定案 |
|---|---|---|---|
| D1 | CLAUDE.md の §最優先タスク（VS Code 調査）と SolAdvisor 記述をどう扱うか（履歴へ退避か削除か） | 0.5、7.1 | **決定済み（2026-09-13 owner 委任）**：`docs/history/issues/` へ退避、CLAUDE.md からは 3 行のリンクに |
| D2 | DESIGN.md 100 KB をどこまで分割するか | 7.4 | **決定済み（2026-09-13 owner 委任）**：不変条件と境界だけ残し ≤40 KB |
| D3 | `wire.rs`（proto の低レベル codec）を `sakura-values` に降ろすか proto に残すか | 2.1 | **決定済み扱い**：proto に残す。values は値型のみで、codec は proto の `impl Wire for` に置く（§3.1.1、R12）。異論があれば 2.1a の前に |
| D4 | core facade の未使用再輸出を削除してよいか | 3.5 | **決定済み（2026-09-13 owner 委任）**：Cargo metadata の全 package／target、docs、supported API の caller が 0 と証明できた item だけ削除。件数と外部利用者不在を先に仮定しない |
| D5 | `Session` 148 field の分割を行うか | 4.9 | **決定済み（2026-09-13 owner 委任）**：行う。ただし Phase 4 の最後、単独 PR |
| D6 | settings topic registry の desktop test を走らせる runner | 1.10、5.9 | **決定済み（2026-09-13 hosted diagnostic）**：GitHub-hosted `windows-latest` を使う。run `34713825191`／job `103607261803` で interactive User32 capability、metadata inventory、exact tab-focus test 3 回、process cleanup が合格した。weekly＋dispatch、リポジトリ単位の直列実行、毎回の capability／inventory fail-closed、artifact 保存を維持する。全 ignored suite の合格は Phase 1.10 初回実行で別途確認する（`docs/architecture/desktop-runner-admission.md`） |
| D7 | `GuardForeignCandidateEnd`／`RestoreCurrentPlacement` の扱い（6.9 の前提） | 6.9 | **未決**：Issue #7 コメントには 2026-08-02 の dump filename／report 記述があるが attachment は未確認。実 dump／report を取得・検証してから決め、6.8〜6.10 はそれまで開始しない |
| D8 | TLC workflow をブロッキングにするか | 7.9 | **決定済み（2026-09-13 owner 委任）**：Phase 3 完了後、expected counterexample を正規化して全 cfg が意味上 green と確認できた時点でブロッキング化 |
| D9 | `.claude/memory/rules.md` 45 KB の分割 | 7.11 | **決定済み（2026-09-13 owner 委任）**：topic 別に移動、削除しない |
| D10 | `session/` を独立 crate（`sakura-tsf-session`）にするか、ディレクトリのままか | 6.7 | **決定済み（2026-09-13 owner 委任）**：Phase 6 完了まではディレクトリ＋R3。安定後に別 Issue で crate 化を再検討 |
| D11 | 実行時の候補上限（`research-wide-candidates` feature）を残すか | 3.1 | **決定済み（2026-09-13 owner 委任）**：研究用として残し、別の runtime bound へ置換しない |
| D12 | main の ruleset を誰が作るか（repo admin 権限が要る）、緊急時の bypass を許すか | 0.6 | **決定済み（2026-09-13 owner 委任）**：Phase 0 の実装担当が repo admin 権限を持つ認証済み `gh` で適用。bypass actor は無し。緊急時も同じ PR と必須 check を通し、管理者権限で迂回しない。本計画更新では実適用しない |
| D13 | `sakura-context-research`（#34 休眠 1,846 行）を crate 化して残すか、削除して git 履歴に残すか | 2.5 | **決定済み（2026-09-13 owner 委任）**：crate 化して feature 限定で残す。1 年間 feature が使われなかったことを確認した場合だけ別の削除 PR で廃止し、期限だけで自動削除しない |
| D14 | IRV 回帰ゲートの無条件文書 FAIL 化のタイミング | 0.8 | **決定済み（2026-09-13 owner 委任）**：0.5 と 7.11 の両方が merge されるまで WARN mode。後に完了する工程の PR で予算内と FAIL mode の成功を検証して切り替え、その次の PR から FAIL |

---

## 8. 命名の統一

解析レポート間で名前が揺れていたものは以下に統一する。

| 統一名 | レポート内の別名 |
|---|---|
| `sakura-values` | `sakura-limits`（contracts §6） |
| `sakura-store` | `sakura-stores`（contracts §6） |
| `sakura-context-proto` | `sakura-neural-proto`（現行名） |
| `sakura-context-research` | `sakura-context`（engine §7、本書初版） |
| Physical IRV／Semantic IRV | 「読解セット」（§5、初版）は Physical IRV の見積りに相当 |
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
./scripts/measure-irv.ps1 -SelfTest
./scripts/measure-irv.ps1 -Compare verification/irv/baseline.json      # 全 PR。FAIL で exit 1
./scripts/measure-irv.ps1 -Out verification/irv/baseline.json          # Phase 完了 PR だけ
```

ローカルでは owner の実行指示に従い cargo の前に `CARGO_HTTP_CHECK_REVOKE=false` を付ける。repository config／CI／恒久環境へ保存しない。`measure-irv.ps1` の引数：`-Benchmarks`（既定 `verification/irv/benchmarks.json`）、`-Out`、`-Compare`、`-SelfTest`、`-RepositoryRoot`。

### 9.2 PR テンプレ（各ステップ共通）

```
Step: <Phase>.<n> <title>  (refs #152)
Reads: <このステップで読んだファイル、行数合計>
Changes: production <±行>, tests <±行>, moved <ファイル数>
Verify: <実行したコマンドと結果 1 行ずつ>
Expect: <観測値> (baseline: <値>)
IRV: <benchmark id> physical <before>→<after> LOC, semantic <before>→<after> (unchanged なら明記)
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
