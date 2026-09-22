# Dead-code audit after #244 — #261

Verified 2026-09-22 on the #259/#261 changes based on `4f28f14`.

## Findings and removals

Scanned 365 Rust source files under `crates` and `tools`. In addition to
workspace Clippy, compared public-function names with Rust token references
and searched each candidate across repository callers, tests, scripts, CI and
documentation. Token counting is a conservative candidate filter, not a
semantic reachability proof: comments, common names and mutually referring
dead groups can hide candidates. No blanket claim of zero dead code is made.

There were 16 initial zero-reference public candidates: 15 obsolete functions
and one intentionally dormant research function. Removing the obsolete
constructors exposed one more unused constructor. Removed **16 functions**
here; #259 also removes three legacy unowned fence methods after migrating
their tests, for **19 removed functions** in the combined work.

| Responsibility | Removed functions |
|---|---|
| Engine startup (`server.rs`) | `with_services`, `with_configuration`, `with_configuration_and_profiles_and_history`, `with_runtime_configuration`, `with_runtime_configuration_and_profiles`, `with_runtime_configuration_and_profiles_and_history`, `appearance_theme_publisher` |
| Engine conversion/UI/test helpers | `with_raw_repair_conversion_hints`, `clear_session`, `test_named_key` |
| Core/dictionary/preferences/IPC | `ReplayTrace::has_one_local_anomaly`, `DictionaryCategory::display_name`, `AiEffort::api_value`, `Descriptor::for_endpoint` |
| Evaluation tool | `quality_delta`, `compare_scoreboards` |
| Fence, in #259 | `acquire`, `release`, `release_after_teardown` |

The unused constructor removal makes two private `Server::build` service
arguments always `None`; remove those arguments and their obsolete branches
of construction. Current product startup remains
`Server::new(...).reserve().with_startup_services(...)`, which installs the
actual prediction and history services. Existing useful conversion-history
and UI ownership comments are preserved on the live APIs.

The final candidate scan leaves only `keystroke_saving_rate` in
`sakura-context-research`: four explicitly dormant modules are retained for
#34. They were compiled and tested. Also retained the fixed numeric
`diagnostic_ring::RequestKind` schema values and the shared integration-test
module allowances; absence of a current event producer is not authorization
to silently change a diagnostic schema. Tests/callbacks/exports with real
consumers were not classified as dead code just because the normal release
binary does not call them directly.

## Verification

| Verify | Expect | Observed |
|---|---|---|
| Candidate definition/caller searches, before and after removal | No remaining consumer of removed symbols | Passed; one justified research candidate remains |
| `cargo clippy --locked --offline --workspace --all-targets --features 'sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture,sakura-engine/context-research' -- -D warnings` | All supported workspace targets and these features compile without warnings | Passed |
| Wrapped workspace `cargo test` with the same features and `-- --test-threads=1` | Executed assertions, not just compilation | 1,981 passed; 0 failed; 97 ignored; 114 reported suites |
| `ci/check-facade.ps1` | Existing public facade budget/consumer gate | Passed, 63 exports |
| `ci/check-dependency-rules.ps1 -Advisory -Enforce R1,R2,R8,R9,R12,R13` | Activated rules pass | Passed; R5/R7 remain advisory warnings; R3/R4/R6 future paths pending |
| `scripts/measure-irv.ps1 -Compare verification/irv/baseline.json -DocsBudgetMode Fail` | No failing critical/required-doc regression | Exit 0 with HISTORY-STORE and CI baseline warnings; no numerical semantic-IRV improvement claimed |
| Format, diff whitespace, process inventory | Intended edits; no owned test processes | Passed |

Tests use `ci/run-test-quiet.ps1`, a 600-second TKW timeout and the locked
offline dependency graph. The 97 ignored tests require separate dictionaries,
long campaigns, timing runs, AppContainer or native UI conditions; they are
not evidence of executed coverage. No dependencies, wire/persistence schemas
or installed user configuration changed. Local raw evidence is retained in
`~/tmp/sakura-memory-fix-20260922/`.

## Scope / IRV

- **A — Required scope:** public function definitions/callers across 365 Rust
  files, lint suppressions, 11 changed source/test files for #261, the fence
  owner from #259, startup service installation, facade/dependency contracts,
  full workspace tests and focused source diffs.
- **B — Expansion:** unused public wrappers span five crates and the evaluation
  tool. Rust's public visibility and explicit allowances prevent Clippy alone
  from establishing disuse, so caller searches and the full suite were needed.
- **C — Architecture:** existing owners and dependency directions remain;
  obsolete alternative startup/UI/conversion interfaces are removed. Tests
  keep exercising the production installation and ownership paths. Public Rust
  helper surface shrinks; no in-repository consumers remain. This is not a
  promise of source compatibility for untracked external Rust consumers.
- **D — Future IRV:** future startup work no longer has to distinguish six
  obsolete service constructors from the active reserve/install flow. Future
  audits can use this explicit retained-code rationale; they must still check
  research activation and diagnostic-schema contracts before further removal.
