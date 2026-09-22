# Conversion and history corrections

Tracking: #95 (single-kanji reachability), #60/#108 (conversion quality),
#127/#130 (history publication and bounded maintenance).

## Acceptance rubric

1. Verify: saturated synthetic dictionaries and all shipped single-kanji
   readings; actual engine candidate selection and commit.
   Expect: registered characters remain selectable without increasing the
   256/108/18 N-best search budgets; explicit smaller caller limits still hold.
2. Verify: shipped-dictionary regressions for ordinary phrases and their
   homophones, including selected preedit and committed text.
   Expect: the reported unnatural leaders are corrected without suppressing
   legitimate alternate words, technical spelling, or explicit user choices.
3. Verify: isolated history capacity and interrupted-publication fixtures,
   malformed/opaque data and ownership checks.
   Expect: bounded space for new records; recovery only from validated known
   states; uncertain data remains intact with an explicit error.
4. Verify: formatting, Clippy, focused and workspace tests with fixture features,
   architecture checks and process cleanup.
   Expect: every required check executes and passes; no test process survives.
5. Verify: the `mcc_mcdc` real-operation fixtures, instrumented atomic branch
   counters, and `ci/check-conversion-conditions.ps1` including its self-test.
   Expect: all 24 combinations and all 12 independent condition witnesses for
   the six named decisions; both outcomes of their 12 atomic branches.

## Scope and invariants

The audit found three distinct owners: core candidate search/tails, engine
selection plus dictionary costs, and developer-history persistence. Investigation
must cover all three because the user requested correction of the reported
findings. Keep these changes local to their owners; no general architecture
refactor. Do not publish raw input history. Preserve the 30-day retention rule,
64 MiB hard ceiling, store ownership, DPAPI protection and explicit failure states.

## Results

All five acceptance criteria pass within their stated scopes. Raw audit history and diagnostic
snapshots remain outside the repository at
`~/tmp/sakura-history-audit-20260922/`.

- Full workspace: 1,957 passed, zero failed, 94 ignored across 113 libtest
  results (TKW `93c042e301ee0c80b7329270c33c8376`). Ignored tests are not
  included in this passing count. A prior run found the installer default
  directory still named 2.0.4-dev; it was corrected before this full rerun.
- Shipped dictionary: 39 explicitly enabled ranking tests passed, zero failed,
  two filtered out (TKW `400813e4b5d76a822199a06f11417b84`). This includes eight
  reported ordinary phrases, four homophone controls, romaji/explicit-choice
  regressions, and all registered characters for 13 engine reading cases.
  The cross-commit timing benchmark was not run in this suite.
  After the final initial-selection simplification, the three `history_audit_`
  shipped-dictionary regressions passed again (TKW
  `de13d1158096929e16e4926edc1e5218`).
- Exhaustive core coverage: all 3,724 single-kanji readings / 23,688 registered
  reading-character pairs remain in candidates; zero omissions. Display
  capacity is 768, while normal search still uses 256/108/18 candidates.
  Bounded synthetic tests cover saturated ordinary lists and explicit limits.
- A private pipe to the release-profile 2.0.5 engine exercised 37 cases,
  including the 33 previously unreachable characters. Every target was
  selectable and committed correctly, and default commit matched preedit.
  The owned engine exited gracefully. Its private bytes rose from 34,070,528
  to 36,110,336 in this finite workload; this is not a long-running leak test.
- History tests cover the real 64 MiB limit, continued append after reclaim,
  known publication cut points, ownership, old-format logical equivalence,
  opaque/unknown/corrupt participants, and platform SHA-256 known answers.
  Unknown states fail with an explicit error and preserve the evidence.
  An isolated copy of the frozen real history recovered and accepted ten new
  records with zero persistence failures. It retained 194,718 records in
  58,723,276 bytes after the capacity reclaim. Opening/recovery took 115,922 ms
  on this machine; a large store can therefore delay engine startup. Original
  history and installed binaries were preserved during the probe.
- Formatting, Clippy (workspace/all targets, warnings denied), dependency
  policy and its self-test, facade and dependency-boundary checks pass.
  Existing transitional architecture warnings remain unchanged. No external
  dependency was added. The release workspace builds; the TSF DLL remains
  440,832 bytes, below its 1 MiB limit. Repository test processes exited.
  The added hash-table test was moved below the implementation to satisfy
  Clippy's test-module placement lint; the final instrumented suite reran it.
- A local 2.0.5 installer was built and audited with the corrected, reproducible
  624,276-entry dictionary. This is local preparation, not evidence that the
  release has been published or installed. Final local build ID:
  `92d7856a8e325d7b`; installer 24,534,220 bytes, SHA-256
  `2985dc3198bf86b5c19a4c353e5d76d8eb420049e3ff8b6e1f7ae00513e0e85e`.

## MCC and MC/DC evidence

The [condition report](conversion-condition-coverage.md) lists six critical
decisions in single-kanji admission, projection overflow and history recovery.
All 24/24 input combinations, 12/12 unique-cause MC/DC witnesses and 24/24 atomic
branch outcomes pass. Five test functions exercise actual candidate generation,
projection and encrypted filesystem recovery; assertions precede each evidence
row. Hash-table collisions, duplicate keys, wraparound and full-table bounded
termination have a separate regression.

The installed nightly (`1.99.0-nightly ba28ff76f`, LLVM 23.1.0) rejects
`-Z coverage-options=mcdc`. It supports branch instrumentation. Consequently,
this report combines explicit condition vectors/witness pairs with actual LLVM
branch counters; it does not claim compiler-generated MC/DC instrumentation.
The instrumented core/engine library suite passed 822 tests with five ignored
(TKW `e1e046c388efaf77e37abd7c711e78ec`). Stable matrix evidence is retained in
TKW `d118f593b57468177bfff7df0b31f878`.

This is a named-decision scope, not whole-workspace MC/DC. Dispatcher lifecycle
branches and filesystem/Windows error paths also have ordinary integration,
state-transition and fault-injection tests, but are not included in those MC/DC
totals. The initial-selection direct-origin check was redundant after the
direct-prefix clamp and first-representative projection; it was removed rather
than inventing an unreachable test vector. No architecture baseline was relaxed.

Reproduction commands (run from the repository, with existing nightly,
llvm-tools and cargo-llvm-cov installed):

```powershell
$evidence = Join-Path ([Environment]::GetFolderPath('UserProfile')) ('tmp/condition-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($evidence)
$tkw = Join-Path $env:LOCALAPPDATA 'Programs/TKW/tkw.exe'
$cargo = (rustup.exe which cargo).Trim()
./ci/run-test-quiet.ps1 -Name 'condition matrices' -Command {
    & $tkw run --caller codex --timeout-ms 600000 -- $cargo test -p sakura-core -p sakura-engine --lib --features sakura-engine/dev-fixtures mcc_mcdc -- --nocapture --test-threads=1 | Tee-Object -FilePath (Join-Path $evidence 'matrix.log')
}
# Use a separate shell for these environment overrides, or restore them after.
$env:RUSTUP_TOOLCHAIN = 'nightly'
$env:CARGO_LLVM_COV_TARGET_DIR = Join-Path $evidence 'target'
$cargo = (rustup.exe which cargo).Trim()
./ci/run-test-quiet.ps1 -Name 'instrumented libraries' -Command {
    & $tkw run --caller codex --timeout-ms 600000 -- $cargo llvm-cov --branch --json --output-path (Join-Path $evidence 'branch-coverage.json') -p sakura-core -p sakura-engine --lib --features sakura-engine/dev-fixtures
}
./ci/check-conversion-conditions.ps1 -SelfTest
./ci/check-conversion-conditions.ps1 -LogPath (Join-Path $evidence 'matrix.log') -BranchCoveragePath (Join-Path $evidence 'branch-coverage.json') -OutputPath (Join-Path $evidence 'condition-report.md')
./ci/check-process-clean.ps1
```

Both test captures and coverage output belong outside the tracked repository.
The checker rejects incomplete/duplicate matrices, ineffective conditions,
missing atomic outcomes and stale/ambiguous source anchors. It combines repeated
LLVM instantiations only at identical source ranges. Source hashes normalize LF.

## Understanding scope and architecture impact

A. Required scope: core search/tail generation and its tests; shared candidate
capacity and protocol golden/round-trip fixtures; engine projection, initial
segment selection, history owner and tests; dictionary priority data and shipped
ranking tests; version/packaging metadata. Relevant contracts are the fixed wire
bounds, contextual segment projection, store ownership, DPAPI validation and
30-day/64 MiB retention. Input behavior and developer-history decisions were
the relevant specifications.

B. Scope expanded because dictionary presence alone did not imply selectable
engine candidates, and the history audit also found a frozen full store.
Increasing candidate capacity exposed a worker-stack overflow: projection's
large deduplication scratch table is now local to construction instead of
travelling in every returned projection. Existing real-pipe tests verify the
original worker-stack budget. No general module reorganization was necessary.

C. Core owns separate search and display bounds. The engine owns mapping a
whole-path choice into each segment's visible candidate domain. History service
owns recovery under its existing store lock; its private Windows SHA-256 helper
owns hash-handle cleanup. No new crate dependency or reverse architectural edge
was introduced. Wire version 23 requires all installed components to update
together; dictionary data and history record formats retain their contracts.

D. Future audits can start with the three `history_audit_` ranking tests and
history publication/capacity tests instead of rediscovering the distinction
between dictionary coverage, visible selection and actual commit. They must
understand the independent candidate budgets and generation-bound recovery.
The three behavioral owners remain separate; the evidence does not establish
coverage of unregistered readings, every host application, or long-duration
memory behavior. The condition-evidence checker adds one verification entry
point for the six decision matrices. It increases the measured CI reading scope
to 5,625 LOC (+11.7% against the existing baseline), triggering a non-blocking
IRV warning. The growth is the explicit evidence validator; no baseline update
or unrelated refactoring hides it.

## Publication state

The four publication checks were run together: tracked/new files, all-ref
author/committer metadata, visibility/tags/releases, and LICENSE. The repository
is already public, v2.0.4 is the latest published release, and MIT LICENSE exists.
The local detailed report records every match: repository/account references,
example/API-like addresses, generic local paths, functional GUIDs and historical
task IDs; no credential-pattern match was found. Existing personal author/tagger
metadata remains in history. The recorded publication exception explicitly
covered 2.0.4, so 2.0.5 publication awaits the owner's decision. No force push or
history rewrite is part of this correction.
