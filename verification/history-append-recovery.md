# History partial-append recovery — Issue #265

## Contract and change

The engine's opt-in `InputHistoryService` writer owns frame boundaries, the
file handle, and append admission after storage failures. An unsuccessful
frame write must not make later successful records unreadable.

On append failure, the writer restores the previous file length and cursor.
Windows append-only handles cannot truncate; `open_append` now opens a
read/write handle and positions it at EOF under existing exclusive store
ownership. Successful appends add no extra seek or filesystem scan.

If truncation or cursor restoration fails, a writer-local latch rejects
subsequent appends before encryption or filesystem operations and skips idle
compaction. It remains set after a failed Clear. Successful Clear resets it;
a service reopen uses existing validation and structural-tail recovery.
Flush/Shutdown continue reporting the earlier append loss until successful
Clear, even when rollback succeeded and later records were saved.

This is a behavior fix, not an architecture refactor. Record framing, DPAPI,
scope exclusions, queue capacity, wire contracts, and normal input replies
remain unchanged. The producer/key path acquires no new lock or I/O work.

## Executable rubric

Run in PowerShell 7 with `CARGO_HTTP_CHECK_REVOKE=false`, through the repository
test wrapper and TKW with a bounded timeout. Exact retained runs are below.

| Criterion | Verify | Expect |
| --- | --- | --- |
| Partial append recovery | `cargo test -p sakura-engine --lib partial_append` | Five tests pass; eight failure offsets on both initial and post-Clear handles preserve the original bytes and the next complete record |
| Terminal failure and recovery | Same five tests | Failed rollback preserves the tail, blocks later writes/maintenance, failed Clear stays blocked, successful Clear/reopen recovers, barriers retain loss |
| Regression | `cargo test --workspace --features sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture` | All executed tests pass; retain skips explicitly |
| Static correctness | `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --features sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture -- -D warnings` | Exit 0 |
| Boundaries and cleanup | `ci/check-dependency-rules.ps1`; `ci/check-facade.ps1`; `ci/check-process-clean.ps1` | Enforced architecture rules pass; no repository test process remains |

## Evidence

Base: `f55e1d5` (2.0.6), Windows, 2026-09-26.

- Baseline history tests passed, TKW run `174909dcc06199f40afae7d479a78aab`.
- Before the production fix, run `055f01fa8b0c4c0a6fd5dcd9cef7d5ed`
  executed the new regression and failed semantically: after a one-byte
  interrupted frame, the next append returned success but the snapshot only
  contained the preceding record. The next record disappeared into the
  ignored tail.
- Initial rollback implementation exposed Windows append-only handle rights:
  run `23a8ccc1e08409c3e4dd8dc153ec7f4e` failed the continuation assertions.
  Opening the owned writer with read/write access resolved these failures.
- Focused final run `da134c804efcfc29cb45d84b0b7355d7`: five passed,
  zero failed, zero ignored (237 unrelated tests filtered).
- Workspace run `99e72e37db3fe15d10463469122aea4f`: **1,985 passed,
  zero failed, 97 ignored**, zero filtered, across 114 reported test groups.
  The ignored cases require real desktop/AppContainer execution, external
  dictionaries/models, release timing/resource fixtures, or long fuzz runs.
  Exact identities are retained in TKW stdout and
  `~/tmp/sakura-history-append/workspace-tests.txt`.
- Clippy `-D warnings`: run `a50f68c19d5ad87de3400e720c6af3c8`, exit 0.
  Format check: run `1d88b45174d3e127dcab0fcf2be285b0`, exit 0.
- Dependency audit: R1/R2/R8/R9/R12/R13 passed; existing R3/R4/R6 targets
  pending, R5/R7 advisory warnings. No new dependency edge was introduced.
  Facade gate: R10 passed, 63 exports with evidenced callers.
- `check-process-clean.ps1` reported no repository-scoped test process after
  both focused and workspace tests. `git diff --check` passed.
- IRV comparison exited 0 with WARN: HISTORY-STORE 2,672 to 6,775 LOC
  (+153.6%) and CI 5,034 to 5,630 (+11.8%) against the checked-in historical
  baseline. These totals include prior work; they are not this patch's delta.
  Unconditional docs were 11,298 bytes within the 24,576-byte budget. Snapshot:
  `~/tmp/sakura-history-append/irv.json`. This patch does not resolve these
  advisory architecture/IRV warnings.

## Scope and limits

The implementation scope is `input_history.rs`, its sibling regression tests,
and the existing capacity tests that call the private append helper. The
public contract is in `docs/decisions/developer-history.md` and DESIGN §5.4.1.
Investigation also mapped the already implemented #118/#132 contracts and
read shutdown/lifecycle boundaries before narrowing to append failures.

No responsibility moved, public API changed, or dependency was added. The
same writer owns both the sticky loss result and the separate blocked-write
state. Future similar work can start from `partial_append` tests and this
record; no measured repository-wide IRV reduction is claimed. The next change
must understand Windows handle rights and successful-Clear-only unblocking.

All fault data is synthetic. The tests exercise real file writes and DPAPI,
with test-only byte-limit and rollback fault injection. They do not simulate
physical power loss or establish the cause of any installed-product incident.
When rollback fails, history recording and retention maintenance remain
unavailable until explicit recovery; persistence-failure counters and barrier
errors expose that condition. Ordinary input processing continues unchanged.
