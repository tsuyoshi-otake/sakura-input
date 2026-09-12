# Verified rules for this repository

This file contains unconditional rules and mandatory routing to detailed,
verified rules. A topic document is required reading when its trigger matches;
it is not optional background. Each detailed rule retains its observation and
scope so later work does not generalize it beyond the evidence.

## Unconditional operating rules

- Prefix **every** Cargo invocation on this machine with
  `CARGO_HTTP_CHECK_REVOKE=false`; otherwise dependency fetches fail with
  `CRYPT_E_NO_REVOKE_CHECK (0x80092012)`.
- Run repository PowerShell scripts with PowerShell 7 (`pwsh`), never Windows
  PowerShell 5.1. Do not pipe a script through another command that can hide its
  nonzero exit status.
- `.cargo/config.toml` pins `x86_64-pc-windows-msvc`. Resolve release artifacts
  under `target/x86_64-pc-windows-msvc/release/`; do not assume
  `target/release/`.
- Route ordinary Cargo tests through `ci/run-test-quiet.ps1`. Preserve the
  original test exit code. After a test, identify repository-owned cargo,
  rustc, and runner processes and prove they exited.
- A passing exit code is evidence only when the intended work executed. Check
  workflow runs/jobs and test `running N tests` counts; prove a test can fail
  when its failure path is material to the claim.
- Do not publish a performance claim before a benchmark measures that exact
  workload. For this repository's microbenchmarks, use the documented best-of-N
  method rather than substituting a mean.
- Maintain explicit terminal outcomes for stateful operations. A catch, skip,
  retry, refusal, stale callback, or suppressed error must still identify who
  finalizes the operation and how the caller observes it.
- Keep tests away from real per-user state unless that state is the subject of
  the test. Real-process tests normally isolate `LOCALAPPDATA`; production
  writers require a test destination so tests cannot corrupt user diagnostics.
- Preserve raw bytes for release files governed by SHA-256. Pin their line
  endings and write bytes explicitly; default Windows text writes can introduce
  CRLF and invalidate embedded or reviewed artifacts.
- Rules in this file and `docs/rules/` must remain evidence-based. If later
  measurement disproves one, correct or remove it and record the new evidence;
  do not retain a stale rule for continuity.

## Mandatory topic routing

Read every matching topic before changing code, tests, scripts, workflows, or
claims in its scope. Multiple rows can apply to one issue.

| Trigger | Required exact rules |
| --- | --- |
| Performance, normalization, SIMD, or benchmark design/claims | `docs/rules/performance.md` |
| CI/workflows, test validity, sandbox claims, persistence tests, diagnostic/error semantics | `docs/rules/ci-verification.md` |
| Windows real-process tests, UI Automation, named pipes, installer/uninstaller, watchdogs, child deadlines, DPI | `docs/rules/windows.md` |
| Any Cargo, PowerShell, shell text edit, release-artifact lookup, or candidate-popup capture on this machine | `docs/rules/this-machine.md` |
| TSF/COM re-entrancy, candidate begin/update/teardown, callback authority, cleanup, or borrow refusal | `docs/rules/tsf-reentrancy.md` |
| Dictionary/prediction limits, overflow fixtures, packaging regexes, line endings, or raw-hash evidence | `docs/rules/overflow-packaging.md` |
| Composition/session/keymap/Shift-Latin behavior, engine child tests, dictionary sparse inputs/ranking, stack limits, CI branch coverage, or update trust state | `docs/rules/session-and-later-findings.md` |

`docs/rules/README.md` describes ownership and
`docs/rules/section-map.md` proves every original section has a destination.
`docs/history/rules.pre-154.md` is the byte-exact pre-compaction snapshot for
provenance; active work should load the smaller matching topic extracts above.

## Cross-topic checks

- Candidate order and selected candidate are separate contracts; assertions
  that cover only one do not prove committed output.
- Suppressing persisted learning at write time does not neutralize older stored
  learning at read/rank time. Preference invariants must close both directions.
- Error names and diagnostics must state what was actually determined. Trace
  failures back to the step that generated or discarded information; do not use
  a peer-trust error for a failure that occurred before evaluating the peer.
- Resource ceilings affect time, heap, stack, and inline array dimensions.
  Measure the dominant bound and preserve allocation-free contracts where they
  exist; a passing single run near a limit is not a stable boundary.
- A user-deletable trust record that is well-formed but weaker than the embedded
  binary floor is equivalent to absence, then rewritten from verified input.
  Malformed records remain terminal and identify the recoverable full path.
