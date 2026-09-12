# Docs / verification / CI analysis (HEAD a370477, v1.0.39)

Read-only analysis. All sizes measured with `wc -c` / `awk`. No repository file modified.

## 1. Document inventory

### Root documents

| path | bytes | lines | audience | contents | freshness signal |
|---|---:|---:|---|---|---|
| `CLAUDE.md` | 40,118 | 315 | agent (auto-loaded) | 18 `## ` sections: 4 owner decisions, 9 per-issue handoffs, one 10 KB unfinished-investigation note, workflow rules | mixed 2026-08-02 … 2026-09-09; `rtk` absent (0 hits) after #70; 5 of 5 code-line citations stale (§3) |
| `DESIGN.md` | 100,119 | 1,619 | dev | 15 sections; §5 conversion engine 24,221 B, §3 architecture 13,684 B, §8 UI 12,216 B, §12 packaging 8,414 B | no per-section dates; §10 memory table has dated row (`DESIGN.md:1367`) |
| `PLAN.md` | 17,079 | 308 | dev/release | Phase 0–5 acceptance criteria + `## Verification entry points` (`PLAN.md:11`) → `scripts/verify-phase*.ps1` | none of the phase scripts run in CI (§5) |
| `README.md` | 25,728 | 156 | user + dev | 9 sections; `## 開発者向け` (1,611 B, `README.md:152`) duplicates the CI command set | current |
| `ISSUE-COMPLETION-REVIEW.md` | 5,547 | 35 | agent (historical) | 2026-08-08 audit of #7/#10/#14/#15 closure claims | dated; self-describes as "別スレッドの記録" |
| `AGENTS.md` | — | — | — | **absent at repo root** although `C:/Codes/CLAUDE.md` contains `@AGENTS.md` | dangling include |
| `.claude/memory/rules.md` | 45,652 | 711 | agent | 7 sections: Performance 1,581 / CI-and-verification 9,960 / Windows 5,288 / This machine 3,058 / TSF re-entrancy 1,286 / Overflow-hazard tests 5,091 / Session state 18,410 | owner memory — relocate only |
| `.claude/memory/journal.md` | 231,293 | 1,609 | agent | 50 entries, 2026-07-31 → 2026-09-09 | newest `journal.md:1545`; holds the only recorded CI timing (`journal.md:1528`, Build-and-test 6m58s) |
| `THIRD_PARTY_NOTICES.md` | 1,920 | — | user/release | notices index | current |

### `docs/` (207,266 B, 53 files)

| path | bytes | audience | contents | freshness |
|---|---:|---|---|---|
| `docs/guide-ja.md` | 15,596 | user | post-install operations manual | current |
| `docs/release-notes-v1.0.*.md` (**34 files**) | 96,062 | user/release | per-version notes | latest v1.0.39 |
| `docs/research/*.md` (9 files) | 50,323 | dev (historical) | context-prediction Phase 0/2A/4/5, #36, #93 | all "research for Issue #34"; dormant |
| `docs/settings-user32-e2e.md` | 19,113 | dev | real-desktop User32 E2E scenario contract | a *contract* filed under docs/ |
| `docs/settings-appearance-verification.md` | 6,120 | dev | Issue #144 appearance evidence | verification record misfiled |
| `docs/neural-context-contract-v1.md` | 6,391 | dev | "dormant contract draft for Issue #34" | dormant |
| `docs/neural-eval-communication-20260811.md` | 4,182 | dev (historical) | "Historical evidence… DeBERTa Tiny removed under #32" | superseded |
| `docs/vscode-crash-diagnostics-runbook.md` | 6,195 | operator | #35 diagnostic runbook | operator doc |
| `docs/vscode-crash-diagnostics-investigation-20260811.md` | 3,284 | dev (historical) | #35 decision boundary | dated |

### `verification/` — 50 top-level entries, 187 tracked files, 705,796 B

**Per-feature record dirs (8):** `developer-history/`, `shift-latin-order/` (largest: 7 coverage scripts, 3 PBT seeds, tracked `tlc/`), `space-key-dispatch/` (`requirements.md`, `scope-manifest.md`, `oracle-provenance.md`, `failure-injection/*.txt`, `mutants-*.txt`, `traceability.json`, `tlc-runner-results.json`, `tlc-timeout-results.json`, `historical/traceability-4c7113c.json`), plus `dual-tsf-candidate-board/`, `dual-tsf-physical-key-arbitration/`, `history-suggestion-convert/`, `tsf-predicting-space/`, `tsf-probe-host-insert/` (a single `tla-record.md` each).

**Flat records at `verification/` root: 23 `.md` (170,700 B) + 15 `-results.json` (122,221 B)**:

| topic prefix | files | bytes | contents |
|---|---|---:|---|
| `history-*` (9 md + 8 json) | clear-epoch, compaction-publication, control-barriers, counter-exhaustion, maintenance-plan, read-failure, read-failure-issue, startup-scan, stop-outcome, store-ownership | ~73,000 | per-PR tranche records for #113–#132 |
| `space-*` (2 pairs) | space-oracle-correspondence, space-tlc-execution | 33,342 | #106 oracle/runner correction evidence |
| single pairs | engine-startup-ownership (#120), write-journal-authority (#52), tsf-callback-deadline (#134), appcontainer-policy-evidence (#104) | 47,747 | per-PR tranches |
| md-only (8) | ai-text-verification (#58), dictionary-format-v2, high-load-input-integrity (#148, **43,165 B**), homophone-detail-verification + `.tsv` 15,313, issue-141-tdd, revalidation-20260905, update-signing-v2 | ~102,000 | specs + ledgers, no machine record |
| other | `tla/` (10 `.tla` + 48 `.cfg`, 92,628 B, `README.md` documenting only `EngineRecovery`), `fixtures/update-signing-v2/`, `reports/issues-56-57-verification.md` | | |

### `eval/`, `corpus/`, `data/`

`eval/README.md` 14,053 (174 files under `eval/`; `eval/corpus/` input, `eval/baselines/` evidence); `corpus/README.md` 2,426 (9 files); `data/README.md` 8,043 + `data/SOURCES.lock` 2,616; `installer/README.md` 3,319 (documented exception to the full-scratch rule).

### Crate documentation presence

**Zero of 15 shipped crates has a README or a `//!` crate doc.** Only `tools/candidate-snapshot`, `tools/candidate-sweep`, `tools/ime-eval` have READMEs. Src bytes: dictc 682,073; sakura-core 941,803; sakura-engine 1,813,157; sakura-ipc 171,018; sakura-proto 203,461; sakura-reg 127,998; sakura-renderer 582,508; sakura-settings 579,616; sakura-tsf 780,723. 6.1 MB of Rust has no local entry point.

## 2. Reading cost today, per issue type

"Must consult" = where the governing rule lives, plus the always-loaded `CLAUDE.md` (40,118 B) and `.claude/memory/rules.md` (45,652 B).

| # | issue type | documents + sections that govern | bytes |
|---|---|---|---:|
| a | engine key/state bug (`無変換`) | CLAUDE.md 40,118 · DESIGN §2 7,601 + §5 24,221 · rules.md §Session state 18,410 · keymap TOMLs · verification/space-key-dispatch/* ~34,000 + `space-oracle-correspondence.*` 23,008 | ~148,000 |
| b | TSF composition / re-entrancy crash | CLAUDE.md 40,118 (§最優先タスク 10,147) · ISSUE-COMPLETION-REVIEW.md 5,547 · DESIGN §4 8,777 + §7 2,165 · rules.md §TSF 1,286 · `write-journal-authority.*` 15,666 · `tsf-callback-deadline.*` 8,808 · `verification/tla/README.md` 3,297 · vscode-crash docs 9,479 | ~140,000 |
| c | candidate popup (#142) | CLAUDE.md 40,118 · DESIGN §8 12,216 · README 6,224 · `settings-appearance-verification.md` 6,120 · release-notes-v1.0.37 2,406 · `issue-141-tdd.md` 13,138 | ~127,000 |
| d | dictionary content / ranking | CLAUDE.md 40,118 · DESIGN §5 24,221 + §6 6,887 · `data/README.md` 8,043 + SOURCES.lock · `eval/README.md` 14,053 · `corpus/README.md` · `dictionary-format-v2.md` 5,946 · homophone 24,127 · research #93 7,307 | ~181,000 |
| e | updater / signing / trust state (#150) | CLAUDE.md 40,118 · DESIGN §12 8,414 · `update-signing-v2.md` 11,532 + fixtures · `ci/release-workflow-policy.ps1` 13,235 + `ci/test-update-signing-v2.ps1` 7,761 · sign/verify scripts 27,004 · `release.yml` 32,115 | ~189,000 |
| f | AI text (#58) | CLAUDE.md 40,118 · `ai-text-verification.md` 8,656 · `AiTextLifecycle.tla` + cfgs 8,507 · DESIGN §9 · README §設定 | ~113,000 |
| g | developer input history (#113–#132) | CLAUDE.md 40,118 · DESIGN §5.4.1 · README · **10 flat `history-*` records 73,000** · `developer-history/` ~60,000 · `DeveloperHistory.tla` + cfgs 14,278 | ~265,000 |
| h | CI / process change | CLAUDE.md 40,118 · rules.md §CI 9,960 · `ci.yml` 10,176 (+ release 32,115, installer 3,906, fuzz 3,944) · `ci/*.ps1` 66,438 · README · PLAN.md 17,079 | ~215,000 |
| i | lossless input under pressure (#148) | CLAUDE.md 40,118 · rules.md 19,991 · DESIGN §10 + §7 · `high-load-input-integrity.md` **43,165** · `tsf-callback-deadline.*` · `revalidation-20260905.md` 10,108 · release-notes-v1.0.39 | ~176,000 |

Median reading set today: **~170 KB of prose before the first line of Rust**, of which 85,770 B (`CLAUDE.md` + `rules.md`) is unconditional.

### Duplicated rules

| rule | stated at | consistent? |
|---|---|---|
| Sensitive input scopes fail closed | `CLAUDE.md:40`, `:87`, `:258` · `DESIGN.md:640` · `README.md:86,126,140` · `docs/guide-ja.md:158` · `docs/neural-context-contract-v1.md:51` · release notes v1.0.5, v1.0.19, v1.0.32 | consistent; **10 restatements**, no canonical home |
| Test output convention (`./ci/run-test-quiet.ps1`) | `CLAUDE.md:3–7` · `README.md:154` · scripts · workflows | consistent; **absent from DESIGN §11 and rules.md** |
| Full-scratch dependency rule | `DESIGN.md §3.1` · `ci/dep-policy.ps1` header · `installer/README.md` | consistent; **not in CLAUDE.md or README** |
| Unsigned-release policy | `CLAUDE.md:17–22` · `DESIGN §12` · README · guide-ja · 7 release notes · `update-signing-v2.md` | consistent; the *decision* exists only in CLAUDE.md |
| Test processes must be proven exited | global CLAUDE.md · `ci.yml` (5 steps) · `release.yml` · `ci/check-process-clean.ps1` | consistent; **not in DESIGN §11 or rules.md** |
| 15 MiB working set is TSF/engine only | `CLAUDE.md:88` · `DESIGN.md:1367` · `README.md:142` · `guide-ja.md:162` · `rules.md:199` | 5 restatements |
| Neural reranker fail-closed + no cloud | CLAUDE.md §32 · README · guide-ja · contract doc · release notes | consistent |
| Candidate-popup ownership | CLAUDE.md §27 · DESIGN §8 | consistent |

## 3. `CLAUDE.md` audit

| line | section | bytes | class |
|---:|---|---:|---|
| 3 | テスト出力規約（#111） | 1,147 | **durable rule** (owner decision) |
| 9 | ITエンジニア・ファースト | 1,906 | **durable rule** (owner decision) |
| 17 | リリース署名 | 937 | **durable rule** (owner decision) |
| 24 | 更新trust state（#150） | 2,822 | durable contract, phrased as a handoff |
| 33 | Issue #58 AI text | 3,010 | contract + handoff; `:42` contradicts `:103` |
| 44 | Issue #26 mode-indicator asset | 904 | per-issue handoff → `sakura-tsf` |
| 51 | Issue #27 candidate popup | 2,096 | handoff + historical status |
| 59 | Issue #28 dictionary detail | 3,024 | handoff with hard-coded counts/hashes — **historical status** |
| 67 | Issue #30 dictionary descriptions | 1,465 | per-issue handoff |
| 73 | Issue #32 reranker | 3,481 | "1.0.5同梱準備中" while HEAD is **1.0.39** — **stale** |
| 98 | 最優先タスク：VS Code crash | **10,147** | historical status + retired workflow (SolAdvisor preflight, stopped thread ID, 2026-08-02 SHA-256s) |
| 201 | 既存の目的 | 893 | durable rule |
| 211 | 実装済みの重要箇所 | 1,609 | **stale**: all 4 code line references wrong |
| 234 | 開発者モード | 2,193 | durable contract → `sakura-engine` + `docs/contracts/` |
| 262 | 検証済みの状態 | 1,498 | **stale historical status** (2026-08-02 build) |
| 286 | 別セッションで再開するときの手順 | 1,231 | workflow (partly stale) |
| 302 | 切り分け | 969 | durable troubleshooting → engine crate |
| 309 | 作業上の注意 | 433 | **contradicted** (`apply_patch`) |

### Spot-checks against the code

1. `write_coordinator.rs` `validate_callback` "469行付近" (`CLAUDE.md:167`) → actual `:482`. **Drifted.**
2. `dispatch.rs:1039` `Action::ModeKanaCycle` (`CLAUDE.md:212`) → line 1039 is an AI-worker comment; the match arm is `dispatch.rs:3435`. **Stale.** File is 16,850 lines.
3. `dispatch.rs:2908` / `:2998` regression tests → 2908 is `fn remove_raw_range<const N: usize>`; 2998 is inside a function body. **Stale.**
4. `keymap.rs:1295` ATOK Tab test → line 1295 is `fn atok_predicting_f7_is_bound_to_transform_katakana`. **Stale.**
5. "`cargo test --workspace`: 496 passed / 12 ignored" (`CLAUDE.md:266`) → workspace now has 1,988 `#[test]` attributes and 95 `#[ignore]`s; `space-oracle-correspondence-results.json` records **1,821 passed / 84 ignored** at `bc90f95`. **Stale by ~4×.**
6. Build ID `1f5ca43e59d305b6`, path `…/versions/1.0.0-…` (`CLAUDE.md:270,283`) → `Cargo.toml:23` is `1.0.39`. **Stale by 39 releases.**
7. `apply_patch` mandated (`CLAUDE.md:312`) → Codex-CLI-specific; no such tool here. **Contradicted.**
8. SolAdvisor 0.2.1 preflight mandated (`CLAUDE.md:103,188,191`) while `CLAUDE.md:42` records the owner forbidding SolAdvisor. **Self-contradictory**; embeds a machine path.
9. `rtk` removal (#70) — confirmed for `CLAUDE.md`, but **not repo-wide**: still in `scripts/verify-phase0..5.ps1`, `scripts/verify-all-phases.ps1`, `scripts/build-dictionary.ps1`, `scripts/run-fuzz-shard.ps1`, `crates/sakura-settings/tests/settings_topic_user32.rs`, and three `verification/space-key-dispatch/mutants-*.txt`.

### Proposed auto-loaded `CLAUDE.md` (≤ 8,192 B)

Keep only: (1) how to work here — test-output convention verbatim (1,147 B), process-exit rule, fmt/clippy/run-test-quiet command block, "never reset the worktree"; (2) a component map of 15 crates, one line each, linking `crates/<c>/README.md` (~1,500 B); (3) a "where things are" index — contracts, decisions, verification, history, memory (~1,200 B); (4) the four owner decisions as one-line summaries linking `docs/decisions/` (~600 B); (5) product principle in 3 lines (~400 B). No line numbers, build IDs, hashes, issue status, or machine paths.

Relocation: §テスト出力規約/§リリース署名/§IT-first/§更新trust state → `docs/decisions/`; §Issue #26/#27/#28/#30/#32/#58 → `docs/contracts/<contract>.md` (contract half) + `docs/history/issues/<n>.md` (status half); §最優先タスク + §検証済みの状態 + `ISSUE-COMPLETION-REVIEW.md` → `docs/history/`; §実装済みの重要箇所 + §切り分け → `crates/sakura-engine/README.md`; §開発者モード → `docs/contracts/developer-history.md`.

## 4. Verification landscape

10 TLA+ modules, 48 `.cfg` files, 92,628 B under `verification/tla/`.

| module (bytes) | cfgs | record | runner | CI? | Rust module(s) modelled | correspondence form |
|---|---:|---|---|---|---|---|
| `AiTextLifecycle.tla` (7,592) | 3 | flat `ai-text-verification.md` | **none** | no | `sakura-engine/src/ai_text.rs` (named only in `CLAUDE.md:39`) | prose, indirect |
| `DeveloperHistory.tla` (13,075) | 4 | `developer-history/tla-record.md` + `correspondence-and-audit.md` | `verify-developer-history-tlc.sh` | no | engine history service | prose |
| `DualTsfCandidateBoard.tla` (6,487) | 7 | `dual-tsf-candidate-board/tla-record.md` | `.ps1` | no | **not named** | absent |
| `DualTsfPhysicalKeyArbitration.tla` (8,066) | 8 | dir | `.ps1` | no | `sakura-tsf/src/conversion_key.rs` | prose (one line) |
| `EngineRecovery.tla` (10,652) | 4 | **none** — only `verification/tla/README.md` | `.ps1` | no | implied `sakura-tsf/src/engine_recovery.rs` | **absent** |
| `HistorySuggestionConvert.tla` (5,390) | 3 | `history-suggestion-convert/tla-record.md` | `.ps1` | no | `learning.rs::best_candidate`, `conversion_key.rs::ends_shared_candidate_ui` | prose with symbols (best in repo) |
| `ShiftLatinInput.tla` (7,341) | 6 | `shift-latin-order/` (11 files) | `.ps1` | no | `shift_latin_oracle.rs`, `shift_latin_order_tests.rs` | prose + `correspondence-and-audit.md` |
| `SpaceKeyDispatch.tla` (10,398) | 9 | `space-key-dispatch/` (21 files) + 2 flat pairs | `.ps1` | **`-SelfTest` only** (`ci.yml:76`) | `space_key_dispatch_oracle{,_tests}.rs`, `space_key_dispatch_tests.rs` | `traceability.json` (**status STALE**) |
| `TsfPredictingSpace.tla` (4,791) | 2 | `tsf-predicting-space/tla-record.md` | `.ps1` | no | not named | prose |
| `TsfProbeHostInsert.tla` (4,270) | 2 | `tsf-probe-host-insert/tla-record.md` | `.ps1` | no | not named | prose |

`verification/tla/README.md` (3,297 B) is titled "Engine timeout recovery model" and documents **one** of the ten modules.

**Flat `<topic>.md` + `<topic>-results.json` pairs.** The JSON is *hand-assembled per PR*: each `runs[]` entry carries `command`, `exit_code`, `test_summaries`, and a `log_name` + `sha256` for a log that is **not committed** (`h2-before.log`, `h6-before.log`, `tlc-runner-before.log` absent). Nothing in `.github/workflows/`, `ci/` or `scripts/` reads or regenerates any `verification/*-results.json`; the only CI/script references into `verification/` are `ci/test-update-signing-v2.ps1` → `verification/fixtures/update-signing-v2/*` and the TLC runners → `verification/tla/*`. Some JSON files hash exact sources (`tsf-callback-deadline-results.json` hashes 10 files) — traceability exists per record, never aggregated.

**Other evidence assets.** cargo-mutants configs: 3. PBT: 6 seed files + 6 shrunk-counterexample notes. Coverage dirs: `developer-history/coverage/`, `shift-latin-order/coverage/` (+ **7 ad-hoc `.py`/`.ps1` scripts**), `space-key-dispatch/coverage/`. `verification/reports/` holds exactly one file.

**`traceability.json` currency:** schema 2, 1,221 B, self-declaring `"status": "STALE"` and `"product_conformance": "INCONCLUSIVE"`, with **no Rust symbol references** (schema 1, preserved at `historical/traceability-4c7113c.json`, carried hashes and blocker IDs). The five symbols `correspondence-and-audit.md:19–21` names — `space_effect`, `begin_conversion`, `idle_space_commit`, `no_dual_effect`, `apply_space` — **all exist**. The prose correspondence is current; the machine-readable file is a deliberate tombstone. `historical/` quarantines a superseded NO_GO verdict — a good pattern, used exactly once.

## 5. CI and test isolation

`.github/workflows/ci.yml` (10,176 B), 3 jobs.

**`build-and-test`** (windows-latest): toolchain+CPU print (`ci.yml:31`) → `rust-cache` → `ensure-cargo-audit.ps1 -SelfTest` → run → `cargo audit --file Cargo.lock` → `cargo fmt --all -- --check` → `cargo clippy --workspace --all-targets -- -D warnings` → `test-run-test-quiet.ps1` self-test → `verify-space-key-dispatch-tlc.ps1 -SelfTest` → **one** `run-test-quiet.ps1 … { cargo test --workspace }` (`ci.yml:80`) → `check-process-clean.ps1` → `cargo test -p sakura-core --lib -- simd:: --nocapture` → process-clean → `check-simd-assembly.ps1 -SelfTest` + audit → process-clean → `cargo test -p sakura-engine --test appcontainer -- --exact the_pipe_is_reachable_from_a_real_appcontainer_token --ignored` → process-clean → `cargo build --workspace --release` → `write-step-timings.ps1`.

**`dependency-policy`**: `ci/dep-policy.ps1 -SelfTest` then `ci/dep-policy.ps1` — enforces DESIGN §3.1.

**`fuzz`** (main pushes only, `shard: [0]`, 15 min, `SAKURA_FUZZ_ITERS=200000`): `sakura-proto --test robustness`, `dictc --test dictionary_robustness`, `sakura-core --test fsm_robustness`. The 72-hour 4-shard run lives in `fuzz-campaign.yml`.

**Per-crate work?** Only three steps name a crate, each because the test *cannot* run inside `cargo test --workspace`. There is **no per-crate correctness gate**.

**`#[ignore]`/desktop tests**: 95 `#[ignore]` attributes; CI opts into exactly 4 by exact name. The AppContainer step carries a 12-line comment explaining `--exact` is load-bearing (`ci.yml:129–141`).

**`scripts/verify-*.ps1` and CI wiring:**

| script | bytes | in CI? |
|---|---:|---|
| `verify-phase0.ps1` … `verify-phase5.ps1` | 9,491 / 14,347 / 9,717 / 9,719 / 19,980 / 31,710 | **no** |
| `verify-all-phases.ps1` | 28,456 (reads `.codex/goal-loop/all-phases`, 24 h freshness) | **no** |
| `verify-fuzz-campaign.ps1` | 9,212 | yes (`fuzz-campaign.yml:99`) |
| `verify-update-manifest.ps1` | 10,891 | yes (`release.yml:227,429`) |
| `verify-release-signatures.ps1` | 1,259 | **no** |
| `verify-context-prediction-source.ps1` | 8,885 | **no** |
| 8 × `verify-*-tlc.ps1/.sh` | 26,468 | **only** `verify-space-key-dispatch-tlc.ps1 -SelfTest` |

`PLAN.md:11` names a 123 KB script suite of which **two** run in any workflow. `.codex/` and `/artifacts/` are gitignored, so phase evidence is not reviewable from a PR.

**Step timings**: `ci/write-step-timings.ps1` posts per-step durations to the job summary; nothing committed. Only recorded numbers: `journal.md:1528` (PR #149, **Build and test 6m58s**) and `appcontainer-policy-evidence.md` (8m27s rerun).

**Contributor cost today for a one-crate change**: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `./ci/run-test-quiet.ps1 -Name 'workspace tests' -Command { cargo test --workspace }` — building and testing all 18 crates for a change to `sakura-logon` (8,896 B).

**Minimal per-crate gate** (feasible today): lints declared centrally in `Cargo.toml`; `cargo clippy -p <crate> --all-targets -- -D warnings` reproduces the workspace lint level exactly; `cargo test -p <crate> --locked` reproduces that crate's tests. A `changed-crates` matrix job (paths-filter on `crates/<c>/**`) running those two commands, with the workspace job retained for `main` pushes and release tags, answers #89. `-p` selection is already proven safe here — four CI steps use it.

## 6. Naming / discoverability audit

- **Test code under `src/`, indistinguishable from production**: `crates/sakura-engine/src/{developer_history_oracle.rs, developer_history_oracle_tests.rs, developer_history_order_tests.rs, shift_ascii_space_tests.rs, shift_latin_oracle.rs, shift_latin_oracle_tests.rs, shift_latin_order_tests.rs, space_key_dispatch_oracle.rs, space_key_dispatch_oracle_tests.rs, space_key_dispatch_tests.rs}`, `crates/sakura-tsf/src/engine_recovery_tests.rs`, `crates/sakura-tsf/src/bin/sakura_tsf_test_host.rs`, `tools/ime-eval/src/oracle.rs`. (`sakura-engine` also has 78 `#[cfg(test)]` blocks, `sakura-tsf` 40.)
- **Verification split three ways**: `verification/<feature>/` (8 dirs), `verification/tla/` (models detached from records), and 23 flat `<topic>.md` at the root added 2026-09-05..09.
- **`.gitignore` contradiction**: `.gitignore:22` ignores `/verification/*/tlc/`, yet `verification/{developer-history,shift-latin-order,space-key-dispatch}/tlc/**` are **tracked** (≈183 KB); the line silently prevents *new* TLC evidence.
- **`docs/` is four categories in one flat directory**: user manual, 34 release notes, 9 dormant research reports, two live contracts, one misfiled verification record.
- **`eval/` vs `corpus/` vs `data/`**: three top-level names, two containing a `corpus` directory.
- **`scripts/` mixes four concerns** in one flat 452 KB directory: build, release, verify, one-offs (`vm-smoke.ps1` 25 KB, `test-ai-api-key.ps1`, a stray `generate-developer-history-pbt-artifacts.rs`).
- **`installer/out`** is gitignored, yet `CLAUDE.md:277` cites `installer/out/sakura_setup.exe` as artifacts of record.
- **`THIRD_PARTY_LICENSES`** directory with no index vs `THIRD_PARTY_NOTICES.md` at root.

## 7. Findings

1. **The auto-loaded file is 40,118 B and mostly per-issue history**; `CLAUDE.md:98–200` alone is 10,147 B of a stalled investigation with a retired tool workflow. → Cut to ≤ 8 KB of durable rules + index.
2. **Five of five code citations in `CLAUDE.md` are stale.** → Replace line numbers with symbol names owned by `crates/<crate>/README.md`.
3. **Zero of 15 crates has a README or `//!` doc.** → Add `crates/<crate>/README.md`: responsibility, invariants, how to test, which verification binds.
4. **One workspace `cargo test` is the only correctness gate** (`ci.yml:80`). → Paths-filtered per-crate matrix; keep the workspace run for `main`/tags. Closes #89.
5. **123 KB of `scripts/verify-phase*.ps1` runs in no workflow** while `PLAN.md:11` presents them as entry points. → Wire a subset or demote `PLAN.md` to `docs/history/`.
6. **Eight of ten TLA+ models never run in CI.** → One `formal-verification` workflow (manual + weekly) over all 48 cfgs with the pinned `tla2tools.jar` hash.
7. **`verification/tla/README.md` documents 1 of 10 models**; `EngineRecovery.tla` has no record dir. → Index table in `verification/README.md`; record dir per model.
8. **23 flat records at `verification/` root** whose `-results.json` cite SHA-256s of uncommitted logs. → Move to `verification/<crate>/<feature>/`; commit the logs or drop the hash claim.
9. **`traceability.json` is a self-declared STALE tombstone** with no Rust symbols. → One machine-readable `correspondence.json` schema (model → cfgs → Rust symbols → tests → runner), CI-checked. #106's missing deliverable.
10. **Twelve oracle/test modules live in `src/`.** → Move to `tests/` or a feature-gated submodule.
11. **The sensitive-scope fail-closed rule is restated in 10 places.** → `docs/contracts/input-scope.md`, owned by `sakura-tsf`.
12. **The full-scratch dependency rule is invisible to the agent.** → One line in `CLAUDE.md` + `docs/contracts/dependencies.md`.
13. **Contracts filed as docs**: `docs/settings-user32-e2e.md`, `docs/neural-context-contract-v1.md`; verification record `docs/settings-appearance-verification.md`. → `docs/contracts/` and `verification/sakura-settings/appearance/`.
14. **`ISSUE-COMPLETION-REVIEW.md` is a root-level historical thread.** → `docs/history/2026-08-08-issue-completion-review.md`.
15. **`AGENTS.md` referenced by `C:/Codes/CLAUDE.md` does not exist here.** → Add a 1 KB pointer file, or (owner decision) drop the parent include.

## 8. Target documentation & verification layout

```
CLAUDE.md                          owner   ≤ 8 KB   how to work + component map + index
AGENTS.md                          owner   ≤ 1 KB   pointer to CLAUDE.md
README.md                          owner   ≤ 20 KB  user-facing only; dev section → link block
DESIGN.md                          dev     ≤ 40 KB  cross-cutting architecture only (§1,3,7,9,10,11)
docs/
  architecture/README.md           component map, dependency direction rules, process split
  architecture/conversion.md       ex-DESIGN §5
  architecture/ui.md               ex-DESIGN §8
  architecture/packaging.md        ex-DESIGN §12
  contracts/                       main plan §3.5 の完全 inventory（1契約1文書1 owner）
    input-scope.md                  fail-closed scope classification (single home)
    ipc-v1.md                      wire framing/version/request ids/size caps
    ipc-pipe-security.md           name/DACL/label
    ipc-server-trust.md            image-path/admission policy
    ipc-client-deadline.md         timeout/late reply
    ipc-diagnostics.md             on-disk diagnostic format
    debug-trace.md                 debug branch trace
    callback-deadline.md           TSF callback deadline
    write-journal.md               ticket/epoch/validate_callback authority
    candidate-ui.md                popup ownership/UIA/placement
    dictionary-image.md            reader+writer tags/versions/optional tables
    developer-history.md           input-history store contract
    learning-store.md              learning store contract
    update-trust.md                signing/WinVerifyTrust/stale trust state
    neural-rerank.md               full protocol v1/fail-closed/bounds
    ai-text.md                     worker protocol and lifecycle
    context-protocol.md            SCV1 context contract
    registry-layout.md             GUID/registry layout
    keymap-format.md               keymap TOML
    config-format.md               config compatibility
    engine-admin.md                fault injection/admin protocol
    dependencies.md                Cargo edge rules
    test-output.md                 quiet wrapper/process-exit proof
  decisions/2026-08-22-release-signing.md        owner-authored, ≤ 2 KB each, append-only
  decisions/2026-08-24-it-engineer-first.md
  decisions/2026-08-30-test-output-convention.md  (#111)
  decisions/2026-09-09-update-trust-state.md      (#150)
  history/releases/v1.0.<n>.md     (34 files moved from docs/)
  history/issues/<n>.md            per-issue handoffs from CLAUDE.md + ISSUE-COMPLETION-REVIEW.md
  history/research/                docs/research/* + neural-eval-communication-20260811.md
  runbooks/vscode-crash-diagnostics.md
  guide-ja.md                      user manual (unchanged)
crates/<crate>/README.md           ≤ 4 KB each: responsibility · invariants · `cargo test -p <c>` · contracts owned · verification that binds it
verification/README.md             ≤ 3 KB index: model → cfgs → record → runner → CI job
verification/<crate>/<feature>/    model.tla, *.cfg, record.md, results.json, correspondence.json, coverage/, mutants/, pbt/
verification/_historical/          superseded verdicts
```

Rules: `CLAUDE.md` holds durable rules and links only — **no** line numbers, build IDs, hashes, issue status, or machine paths. `docs/decisions/*` owner-authored, dated, append-only, never agent-edited. `crates/*/README.md` link rather than restate DESIGN prose. `verification/<crate>/<feature>/record.md` states the claim and its bound; `results.json` references committed logs only.

### Reading set, before vs after

| issue type | before | after (auto-loaded + targeted) | reduction |
|---|---:|---|---:|
| a engine key/state | ~148,000 | 8,192 + engine README 4,000 + `architecture/conversion.md` §keys ~6,000 + record 6,000 | ~24,000 (−84 %) |
| b TSF re-entrancy crash | ~140,000 | 8,192 + tsf README 4,000 + `contracts/write-journal.md` 5,000 + record 6,000 | ~23,000 (−84 %) |
| c candidate popup (#142) | ~127,000 | 8,192 + renderer README 4,000 + `contracts/candidate-ui.md` 5,000 + `architecture/ui.md` §popup 5,000 | ~22,000 (−83 %) |
| d dictionary / ranking | ~181,000 | 8,192 + dictc README 4,000 + `architecture/conversion.md` §ranking 8,000 + `data/README.md` 8,043 + `eval/README.md` 14,053 | ~42,000 (−77 %) |
| e updater / signing (#150) | ~189,000 | 8,192 + settings README 4,000 + `contracts/update-trust.md` 5,000 + decision 2,000 + verification 12,000 | ~31,000 (−84 %) |
| f AI text (#58) | ~113,000 | 8,192 + `contracts/ai-text.md` 5,000 + `verification/sakura-engine/ai-text/` 17,000 | ~30,000 (−73 %) |
| g developer history | ~265,000 | 8,192 + `contracts/developer-history.md` 5,000 + crate README 4,000 + verification index 3,000 | ~20,000 (−92 %) |
| h CI / process | ~215,000 | 8,192 + `contracts/test-output.md` 2,000 + `contracts/dependencies.md` 2,000 + `ci.yml` 10,176 | ~22,000 (−90 %) |
| i lossless input (#148) | ~176,000 | 8,192 + tsf README 4,000 + `architecture/README.md` §budgets 3,000 + `high-load-input-integrity` 43,165 | ~58,000 (−67 %) |

## 9. Migration sequence

One PR per step. Old locations keep a one-line pointer for one release.

1. **[owner decision] Extract owner decisions from `CLAUDE.md`** into `docs/decisions/`. `Verify:` `ls docs/decisions/*.md | wc -l` = 4 and `rg -c 'docs/decisions/' CLAUDE.md` ≥ 4. `Expect:` each file reproduces its section byte-for-byte minus heading; `CLAUDE.md` drops 6,812 B.
2. **[owner decision] Demote `CLAUDE.md` history.** Move §最優先タスク (10,147 B), §検証済みの状態, status halves of §Issue #26/#27/#28/#30/#32/#58, and `ISSUE-COMPLETION-REVIEW.md` into `docs/history/issues/`. Move whole, do not summarise. `Verify:` `wc -c CLAUDE.md` ≤ 8192. `Expect:` no `1f5ca43e` / `496 passed` string remains in `CLAUDE.md`.
3. **Add per-crate READMEs for the metadata-derived set.** Use `cargo metadata --no-deps --format-version 1` and select workspace manifests directly under `crates/`; do not freeze a historical crate count. Fix stale code citations as symbol names. `Verify:` compare that directory set with README parents and validate the template headings. `Expect:` exact set equality; every crate README names an executable package-appropriate test/build command.
4. **Split `DESIGN.md`.** §5 → `docs/architecture/conversion.md`, §8 → `ui.md`, §12 → `packaging.md`, §2 → `docs/history/product-requirements.md`. `Verify:` `wc -c DESIGN.md` ≤ 40960; link checker 0 broken. `Expect:` concatenated byte count ≥ 100,119 (nothing lost).
5. **Create `docs/contracts/` and dedupe.** Materialize the complete unique inventory in the main plan §3.5, including the full dictionary-image reader/writer contract; duplicate restatements become links. `Verify:` exact filename/owner set equality between the index and inventory, canonical-symbol resolution, and sensitive-scope dedupe. `Expect:` no missing/duplicate owner; `input-scope.md` is the only normative sensitive-scope statement.
6. **Relocate `docs/` history.** 34 release notes → `docs/history/releases/`; research → `docs/history/research/`; misfiled verification → `verification/sakura-settings/appearance/`; contracts → `docs/contracts/`. `Verify:` `ls docs/*.md | wc -l` = 1. `Expect:` `ls docs/history/releases/*.md | wc -l` = 34.
7. **Consolidate `verification/`.** Move every committed existing artifact through an explicit old→new path mapping; put each model/cfg beside its record, move historical material to `_historical`, and update runner paths in the same PR. New `verification/README.md` and correspondence files are tracked separately from moves. Commit evidence logs or remove unverifiable hash claims. `Verify:` mapping is bijective, each moved artifact keeps `git log --follow`, and new files pass their schema checks. `Expect:` no existing artifact is lost or duplicated; no fixed historical total is used after new files are added.
8. **Add `correspondence.json` per verification folder + CI check.** Schema: model, cfgs, runner, Rust symbols, binding tests. `Verify:` new `ci/check-verification-correspondence.ps1 -SelfTest` exits 0 and the real run fails when a named symbol is absent. `Expect:` all 10 models have a non-STALE correspondence file (#106).
9. **Per-crate CI job (closes #89).** Build the matrix from changed paths plus the actual `cargo metadata` dependency graph. Explicitly map `data/`, root manifests/lockfile, `.cargo/`, toolchain, common scripts/CI/workflows; any unmapped shared path falls back to the full workspace. Run package-appropriate clippy/test commands and process cleanup. `Verify:` self-tests cover crate-local, mapped shared, and unknown shared paths. `Expect:` local changes include the package and real downstreams; unknown shared input cannot skip tests.
10. **Wire formal verification.** New `formal-verification.yml` runs every discovered TLC runner/config with the pinned jar hash. First classify configs as normal-success or expected-counterexample and normalize the latter only when the expected invariant/trace appears; an unexpected success/failure remains red. Make blocking only after all configs are green under that classification. `Verify:` compare discovered runner/config set with workflow matrix and check Java cleanup. `Expect:` every config reaches an explicit classified result and no Java survivor remains.
11. **Separate test code without banning sibling tests.** Move cross-module/process tests and oracles to integration/verification support where APIs permit; keep private-unit sibling `*_tests.rs` under `src/` behind `#[cfg(test)]`. `Verify:` architecture check rejects shipped references to test-only modules and compares the Phase 0 metadata test universe. `Expect:` no production target reaches test support and no test disappears.
12. **Split `scripts/`** into `scripts/{build,release,verify,tools}/` and fix active `rtk` references. Search real active locations: scripts, `ci/`, workflows, and crate `tests/**`; classify historical docs and fixture/data hits as exclusions instead of demanding a repository-wide empty result. `Verify:` every active script path referenced by workflows resolves. `Expect:` active execution references are zero and each residual hit has an explicit historical/data classification.
13. **Split `.claude/memory/` (D9 decided 2026-09-13).** Relocate component-scoped rules without deletion and leave an index pointer. `Verify:` total rule-bullet identity across old/new files is preserved. `Expect:` the root file meets its budget while no learning is lost.
