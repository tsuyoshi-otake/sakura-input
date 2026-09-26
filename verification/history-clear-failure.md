# History Clear failure handling — Issue #266

## Problem and contract

After a live Clear dropped its old handle, a partial header write returned an
error with `file = None`. The next Append reopened that nonempty, invalid
header without validating it. Append and Flush could succeed even though the
history snapshot was unreadable. This is separate from #265's interrupted
frame append; the reproduction includes that earlier working-tree fix.

The existing writer now treats a Clear error after relinquishing the old
handle as a terminal storage failure. Append and idle compaction stay blocked;
Flush/Shutdown retain failure. Only successful explicit Clear or a validated
service reopen can resume recording. Preflight refusal retains the previous
handle and bytes. A failed Clear is still reported to its caller.

Live and offline Clear now share `write_cleared_header`. Success requires both
header writing and `File::sync_all` to succeed before the live handle is
published or the offline operation returns. This explicitly strengthens Clear
completion; it is not a behavior-preserving refactor or power-loss guarantee.

## Reproduction and verification

Base `f55e1d5` plus the uncommitted #265 fix, Windows, 2026-09-26.

The before-fix TKW run `5871b23e88be907e7bfa271174fc727f` executed four
tests and failed two semantic assertions:

- At a one-byte Clear header, Flush returned success while `read_snapshot`
  returned `invalid input history header`.
- A synchronization-failure injection was never observed; live Clear returned
  success. Inspection confirmed both live and offline completion used only
  `flush`, without `sync_all`.

Focused corrected run `82d5672be8d0cc6661752398d3e8a46f` passed six
`failed_clear_` tests (five new plus one existing #265 case), with zero
failures or ignores and 241 unrelated tests filtered. Test-only fault injection performs real partial file
writes and intercepts the Clear synchronization result. Native successful
synchronization still runs in the success/retry paths.

| Criterion | Verify | Expect |
| --- | --- | --- |
| Partial-header failure is terminal | `cargo test -p sakura-engine --lib failed_clear_` | Five header failure offsets reject later writes and keep Flush/Shutdown failed; idle maintenance never reopens the damaged file |
| Clear recovery and refusal | Same command | Successful retry permits a readable subsequent record; preflight refusal preserves handle and bytes |
| Completion synchronization | Same command | Live/offline Clear propagate sync failure; live Clear never publishes the failed handle |
| Combined regression | `cargo test --workspace --features sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture` | All executed tests pass, including #265; record ignored cases |
| Static checks and cleanup | `cargo clippy --workspace --all-targets --features sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture -- -D warnings`; `cargo fmt --all -- --check`; `ci/check-process-clean.ps1` | Exit 0 and no repository test processes |

All Cargo tests use `ci/run-test-quiet.ps1`, TKW retained requests, a bounded
timeout, and `CARGO_HTTP_CHECK_REVOKE=false`.

Final combined workspace run `308585bee9b47f0bd96e3241800d47ba` passed
**1,990 tests**, with **zero failures and 97 ignored** across 114 reported test
groups. Ignored cases still require desktop/AppContainer execution, external
artifacts, release timing fixtures, or long fuzz campaigns. Exact output is
retained in TKW and `~/tmp/sakura-history-clear/workspace-tests.txt`.

Clippy `-D warnings` run `4f95e14c1c0225b95df609fbc0a99b7a` and fmt-check
run `1b7b2f80eca3c88bdb5734e9fcabb46a` both exited 0. Process cleanup
passed after focused/workspace tests and after static checks; no cargo,
rustc, or clippy runner remained. Whitespace validation passed.

IRV snapshot `~/tmp/sakura-history-clear/irv.json` exited 0 with the existing
HISTORY-STORE and CI warning categories. HISTORY-STORE is now 6,992 physical
LOC versus 6,775 after #265 (+217 in this follow-up); the historical checked-in
baseline is 2,672. CI remains 5,630 versus historical 5,034. Unconditional
documents remain 11,298 bytes within the 24,576-byte limit. Added regression
coverage increases physical reading scope; no structural reduction is claimed.

## Scope, ownership and limits

This issue needed only the existing history writer, its sibling tests, the
developer-history decision, and the existing store-owner/replacement contracts.
The store API was read to establish its recovery requirements; no store code
or compaction transaction changed. Investigation expanded to offline Clear
because live/offline success must apply the same header synchronization rule.

The writer remains the sole owner of the file and blocked/loss state. The
existing public API, epoch filtering, queue admission, DPAPI, scope exclusions,
and normal key replies remain unchanged. No dependency edge was added.

Future Clear investigations can start at `write_cleared_header` and
`failed_clear_` tests. They must distinguish refusal before handle replacement
from failure after destructive work, and understand successful-Clear-only
unblocking. No measured IRV reduction is claimed. The shared helper keeps
live/offline completion knowledge together; the private writer still owns
runtime failure state.

Clear continues to truncate in place. This patch does not make Clear atomic,
restore content explicitly requested for deletion, or make partial-header
startup recovery permissive. Physical power loss, device cache guarantees,
and installed-product incident attribution remain unqualified. All test
content is synthetic; no real user history was cleared.
