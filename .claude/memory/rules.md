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
- The repository is public. Journal entries, rules, commits, PRs and Issues
  that draw on the owner's history or diagnostics carry counts and patterns
  only: no typed words or dated input examples, and no local path where the
  private exports or backups are kept (#313 review caught both).
- Rules in this file and `docs/rules/` must remain evidence-based. If later
  measurement disproves one, correct or remove it and record the new evidence;
  do not retain a stale rule for continuity.
- This checkout can be shared with another session that has uncommitted work
  in it (2026-10-28: `git commit -a` swept four foreign files into a pushed PR
  branch, #413). Run `git status` before every commit and stage explicit
  paths; never `git commit -a` or `git add -A`. A branch switch carries the
  foreign changes along, so when the tree is dirty build the commit from a
  temporary `GIT_INDEX_FILE` (`read-tree`, `update-index --cacheinfo`,
  `write-tree`, `commit-tree`) and push its SHA, leaving HEAD and the working
  tree alone. Check that `git pull` succeeded before branching from `main`.

## Mandatory topic routing

Read every matching topic before changing code, tests, scripts, workflows, or
claims in its scope. Multiple rows can apply to one issue.

| Trigger | Required exact rules |
| --- | --- |
| Performance, normalization, SIMD, or benchmark design/claims | `docs/rules/performance.md` |
| CI/workflows, test validity, sandbox claims, persistence tests, diagnostic/error semantics | `docs/rules/ci-verification.md` |
| Windows real-process tests, UI Automation, named pipes, installer/uninstaller, watchdogs, child deadlines, DPI, window classes, DLL pinning, `cdylib` tests, or junction/reparse-point checks | `docs/rules/windows.md` |
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
- A change to dictionary costs or to a generator is measured beyond the
  authored corpus: write a scratch corpus for the affected class, run
  `accuracy-gate --corpus-dir <dir>` with placeholder snapshot rows
  (`id<TAB>0<TAB>-`) and `--accept` against main's build, then measure the
  new build and report every losing class with its count (kana lemma,
  sibling homograph, repair path) instead of only the rows the change was
  meant to fix. A probe whose expected surface is the cheapest lemma cannot
  see those classes (#348).
- Run `scripts/build-dictionary.ps1` from Git Bash through a `.ps1` file
  (`pwsh -File build.ps1` with the paths inside it), not `pwsh -Command`:
  MSYS rewrites a `/c/...` path to `C:\c\...` and a `C:/...` argument as a
  path list (`...\sakura-input\C`, #390). Either way the script throws and
  the stale `artifacts/accuracy` build stays in place, so a gate run after a
  failed build measures the old dictionary. Check the build exit code and
  the `system.dic` hash before reading any gate result.
- A generator that reprices rows from native evidence matches a native row
  by POS class, not by spelling: `(reading, surface)` collides across verbs
  and across 活用形 (満つ's 未然形 みた floored 見た at 5,017, #390).
- A bounded diagnostic log keeps its newest data. An append-only file with a
  hard ceiling refuses everything once it is full, so it shows the start of a
  long session and never the moment of failure (the binary IPC ring, #313; the
  text `debug.tsv`, #406). Choose the retention when the log is added: a ring
  for fixed slots, two-generation rotation for text. Rotation across processes
  needs a non-blocking lock and a size re-check by path under it, and a failed
  attempt must silence the writer with a back-off instead of costing I/O on
  every key.
- A fall-back or circuit-breaker counter has two parts to test: the trip
  threshold and what counts toward it. The threshold shows in behaviour
  tests; the classification does not when one miss leaves the behaviour
  unchanged. Counting a never-issued request or a broken link as a miss
  escaped every behaviour test in #415 until a test-only reader of the count
  and a direct call of `call_for_key` with a spent callback killed it. Mutate
  each branch that decides what counts, not only the threshold.
- `crates/sakura-tsf` sets `clippy::indexing_slicing = "warn"` and the CI
  clippy job runs `--workspace --all-targets -- -D warnings`, so test code is
  linted too: `waited[0]` in a new test failed the local run (#415).
  Destructure (`let [a, b, c] = waited;`) or iterate, and run that clippy
  command on a test change before pushing.
- An operation that reaches a store by path (clear, repair, move) must wait for
  the asynchronous lifecycle that retires the store's owner. `active` turns
  false at the next request boundary, but the writer thread keeps
  `<history>.writer.lock` until the `HistoryRuntime` worker has joined it (6-28
  ms, sometimes over 270 ms), so a `ClearInputHistory` right after developer
  mode went off hit `AlreadyExists` and answered `Internal` in about 30 % of
  isolated runs (#417). `HistoryRuntime::hold_idle` is the barrier and the
  lease; `serve()` takes it for a Clear only, keeps it until the dispatch, and
  answers `Busy` when the lifecycle does not settle inside the client's 2 s
  call budget. Waiting alone is not enough: a developer-mode toggle between
  the wait and the dispatch retired the attached service (`BrokenPipe`) or
  started a generation under a by-path Clear (`AlreadyExists`), both
  `Internal` (found in review of PR #418). While a hold is outstanding the
  worker neither retires nor opens, and `configure` still never waits for it,
  because it runs under the configuration lock that every request reads
  through.
- `.claude/memory/journal.md` is not valid UTF-8: the 2.0.20 entry holds a
  Shift-JIS fragment. A script that rewrites it (a keep-both conflict resolver,
  an appender that decodes) must work on bytes or decode with
  `surrogateescape`; a strict decode fails with `UnicodeDecodeError` (#431).
- Reviewed detail sources (`data/curated-*.tsv`) replace an imported glossary
  or WordNet description of the same exact identity, relations included
  (#429). Before that the imported text won and the curated report only
  counted the loss (`suppressed_by_existing`). The smile-chat glossary gives a
  term's first sense to every entry of the same surface, so a general word can
  carry one product's meaning; fix it with a reviewed row and prove it on the
  built image with `real_dictionary_ships_every_reviewed_description_over_imported_ones`
  (19 rows failed on the 2.0.25 image, all pass after the rebuild).
- A mutation sweep must leave cargo no mutant build output that still looks
  fresh. Restoring the mutated source with a timestamp-preserving copy
  (`Copy-Item`) makes it older than the last mutant's output, so the next
  `cargo test -p <crate>` with the same feature set reuses that mutant's test
  executables and engine binary (#417: the zero-ceiling mutant's executable
  failed the very test written to kill it, and the independent verifier saw the
  lib and engine gates red). After a sweep touch the restored files, or run
  `cargo clean -p <crate>`, and rebuild before any gate. Also derive survivors
  with `Where-Object { $_ }`: in PowerShell `@($null)` has one element, and four
  survivors were reported as KILLED with no test name.
- A flag a condvar waiter tests under its mutex has to be set under that mutex.
  `Mailbox::stop` stored `stopping` and notified without taking `state`, while
  `worker()` checks `stopping` and then waits on `pending_changed` holding it,
  so a stop that landed between the check and the wait was lost and
  `PredictionRuntime::drop` joined a sleeping worker forever (#422). In the
  suite it showed as one loaded run in 110 that never finished, not as a
  failure; a loop of start, one request and drop hung in 4 of 4 idle-machine
  runs and passed 3 of 3 (about 300,000 iterations each) once the flag was set
  under the lock. Test it by holding the lock in the test and asserting the flag
  stays unset (`stop_sets_the_flag_only_while_holding_the_state_lock`).
- A cleanup that deletes scratch directories builds its manifest first and
  refuses to run when the manifest is not the size expected. On 2026-10-28 the
  cleanup of the 4 directories a baseline test run had left removed 761
  `sakura_history_runtime_*` directories instead: it was a `pwsh -Command '...'
  arg` one-liner, `$args[0]` was empty, the snapshot was never read, and every
  directory counted as new. Write the cleanup as a `.ps1` with `param()`, print
  the manifest, and compare its count with the expected one before the first
  `Remove-Item`.
- A test-only patience seam goes where the timeout is consumed, and its
  coverage is checked with the whole library under load. The first seam of #422
  sat in `PredictionService::request_into`; four `prediction::tests::*` call
  `request`, which waits through the same `Mailbox::wait_into` but never passes
  `request_into`, and a run of only the `dispatch::tests::` family could not
  show it. The whole `sakura-engine` library next to 16 busy-loop threads failed
  3 of 5 runs (8 failures in those four tests) before the seam moved into
  `wait_into`, and 0 of 15 runs (10 next to 16 threads, 5 next to 32) after.
- The `ci/*.ps1` gates start `cargo metadata` through .NET processes, which take
  the process working directory, not PowerShell's `Set-Location`. A gate script
  launched in the background from another directory checks that directory:
  `check-dependency-rules.ps1` failed with `could not find Cargo.toml` in the
  launching directory (#422), and the launch before it, made from the main
  checkout, passed and most likely checked the main checkout's tree instead of
  the worktree's. Start the gates with the repository root as the process
  directory (`cd` before `pwsh`, or set `[Environment]::CurrentDirectory`) and
  print both directories next to each result.
