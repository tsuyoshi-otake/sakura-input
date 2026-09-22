# Developer-history runtime and recovery corrections

Tracking: #250 (developer history only), #127, #130, #125.
Baseline: f06e85a4a41e3b11ef271434968bb7d101c97bb5 / 2.0.5.

## Behavior and ownership

History initialization is asynchronous. While an existing encrypted store is
being authenticated and decoded, normal engine requests remain available and
history statistics report inactive. A single lifecycle owner handles opening,
publication, retirement and joined shutdown. Configuration publication owns
enable/disable changes; an older request snapshot cannot create a new generation.
A failed opening is terminal until an explicit disable/enable transition.
Initialization does not retroactively record input received while inactive.
Disabling the runtime also disables its engine-process diagnostic trace under
the publication lock. Enabled-owner shutdown clears that flag; the disabled
startup placeholder cannot clear its replacement's active trace. This leaves
the separate TSF process's tracing state untouched.

Interrupted replacement files can be discarded only when a valid protected
publication plan identifies the intact canonical as the old generation and no
backup exists. Contradictory complete images, opaque data, an invalid plan,
unexpected participants and ambiguous generations still preserve evidence and
fail closed. No history format or wire protocol changes are required.

The existing `SKCP0001` plan records hashes but no expected replacement
length. A CRC-invalid trailing frame is classified as interrupted, including
a full-length damaged write; the format cannot distinguish these cases.
Discarding that replacement still requires the exact authenticated old
canonical and absence of a backup. DPAPI/schema errors and complete valid
images with a contradictory hash remain errors.

Capacity compaction authenticates and decodes the source once. It constructs
the retained image from those validated frames and checks the exact durable
replacement and published canonical by length and SHA-256. This removes two
redundant DPAPI/decode passes without removing the write/sync/publication
barriers, backup protection, retention window, size cap or bounded queue.

## Acceptance and evidence

The local executable rubric is
`.codex/goal-loop/history-runtime-recovery/rubric.md` (ignored local evidence).
The previous reproduction observed a separate key timing out during 25,759 ms
activation, four incomplete replacement lengths refusing every open, and 176
drops from 1,200 synthetic commits followed by 35,257 ms in stop.

Integrated verification, 2026-09-22:

- Workspace tests with `sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture`:
  1,974 passed, zero failed, 97 ignored across 114 reported suites. Three ignored
  tests are the separately executed large-store release acceptance cases.
- Instrumented core and engine library tests: 836 passed, zero failed, five
  ignored. `ci/check-conversion-conditions.ps1` reports MCC 28/28 combinations,
  unique-cause MC/DC 14/14 conditions and LLVM atomic branches 28/28 outcomes
  across seven named decisions. This is fixture-witnessed evidence with LLVM
  branch counters, not compiler-generated MC/DC or coverage of the whole
  repository. Lifecycle transitions have deterministic tests but no MCC claim.
- `cargo fmt --all -- --check`, workspace/all-target Clippy with `-D warnings`,
  dependency and facade gates, dependency-policy self-test and policy check,
  condition-checker self-test, `cargo audit --file Cargo.lock`, and whitespace
  checks pass. No dependencies were added. Existing dependency-rule future-path
  and renderer/settings transitional advisories remain.
- `cargo build --workspace --release --locked` passes. The TSF DLL gate and
  its self-test pass: 440,832 bytes against a 1,048,576-byte cap.
- IRV self-test passes. The comparison exits successfully with warnings:
  history 2,672 to 6,426 LOC (+140.5%), CI 5,034 to 5,630 LOC (+11.8%). The
  history inventory now counts its previously omitted test file as well as the
  new lifecycle and regression files; thresholds and baseline were not reset.
- A private-process trace regression first failed on continued writes after
  developer-mode OFF. After correction, 32 keys leave the trace byte-for-byte
  unchanged; ON resumes recording in both debug and release builds. File
  contents establish this invariant. A same-connection request fences the
  preceding key's post-response UI publication before each comparison; receiving
  its Output alone does not establish that its trace writes have completed.
  The capacity fixture creates missing parent
  directories so an absent user `tmp` directory does not prevent the test.

The three opt-in release acceptance tests passed (three run, zero failed,
two unrelated harness tests filtered), with these single-run observations:

| Workload | Observed result |
| --- | --- |
| Full 64 MiB store, 20 commits/s for 60 s | 1,200/1,200 records read back in order; zero drops and persistence failures; size cap retained |
| Full-store opening / stop after workload | 33,741 ms opening; 1 ms stop (previous measured stop: 35,257 ms) |
| Hot enable with 48 MiB store | 26,411 ms initialization; 512 keys overlapped opening; maximum key 661 microseconds |
| Cold enabled startup with 48 MiB store | 24,999 ms initialization; 495 keys overlapped opening; maximum key 609 microseconds |
| Private-pipe readiness / process shutdown | 23 ms readiness after spawn and 26 ms shutdown in each hot/cold fixture |

Every stats and key call during the opening loop has a 100 ms deadline.
These are acceptance measurements on this machine, not best-of-N performance
benchmarks or worst-case guarantees. Initialization still scans the full
encrypted store; moving its owner removes its wait from the request path.

Reproduce the large-store cases with a preserved, complete 64 MiB fixture:

```powershell
$env:SAKURA_HISTORY_FIXTURE = '<private preserved input.bin>'
$env:CARGO_HTTP_CHECK_REVOKE = 'false'
./ci/run-test-quiet.ps1 -Name 'history release acceptance' -Command {
    cargo test --release -p sakura-engine --features dev-fixtures --test history_maintenance -- --ignored --nocapture --test-threads=1
}
```

Use an external 600-second process-tree timeout for the build and tests. The
verified run used TKW with that timeout. Both the repository process-clean
gate and a separate inventory of the temporary coverage target found no
surviving test processes after the tests.
The final release build also left no repository or coverage-target runner.
Both preserved history-file hashes and both installed production resident
PID/image/creation identities match the pre-test snapshot.

The opt-in `history_maintenance` tests read `SAKURA_HISTORY_FIXTURE`, use only
private copies and print aggregate counts/timing. Every engine connection uses
the shared harness's exact image and child PID checks. The original encrypted
store and production engine are not modified.

## A–D: scope and architecture

**A. Required scope:** engine history lifecycle (`main`, `server`,
`history_runtime`), history persistence and its focused recovery/capacity tests,
the private-pipe harness contract, history record/DPAPI public APIs and the
developer-history specification. Named condition fixtures and the coverage
checker provide the MCC/MC/DC evidence boundary. No conversion-ranking,
renderer, TSF or AI-provider behavior changes are needed.

**B. Expansion:** the lifecycle change crossed startup and live configuration
because both opened the same store synchronously. Integration inspection also
identified stale request snapshots as a possible source of re-enabling history;
configuration publication must therefore remain the only desired-state writer.
The persistence change needed crash-state matrices and large-store timings,
not inspection of users' actual text.
The trace regression also needed the server reply/publication ordering contract:
an Output response precedes UI publication, while a subsequent request on the
same connection fences that earlier key's remaining side effects.

**C. Impact:** history-runtime owns service generations and retirement. The
existing writer still owns accepted records and durable file mutations.
Recovery owns generation validation; compaction uses the identity of the
already validated image to verify publication. Dependencies remain one-way
from server/runtime to the history service and store; no dependency is added.
The lifecycle owner also retires the engine's developer trace with its mode;
it never changes another process's diagnostic state.
The visible compatibility change is that enabled history becomes active after
asynchronous initialization; it no longer delays engine readiness or keys.
An early shutdown still joins an unfinished opening scan and can therefore
wait for that scan. The five-second shutdown acceptance threshold applies
only after the measured 60-second append workload, not during initialization.

**D. Future IRV:** lifecycle races can be understood in the dedicated owner and
its deterministic tests, without reading conversion or key dispatch internals.
The recovery and capacity fixtures localize the relevant persistence contracts.
Future changes must preserve generation-safe publication, inactive initialization,
conservative rollback and exact-image verification; bounded queue drops remain
possible under workloads or storage stalls beyond the measured acceptance case.

The IRV inventory now includes the lifecycle owner and four history test
files, including the previously omitted existing `input_history_tests.rs`.
Their reading cost is reported rather than hidden by moving tests. The
baseline and regression thresholds are unchanged; a history-size warning is
expected from the added behavior and this inventory correction. This task
does not claim a numeric IRV reduction.
