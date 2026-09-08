# H6 producer epoch tranche (#118)

Main baseline: `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1`, v1.0.35. Patch base: PR #117 (`cc5c997`).
Classification: paused producer resurrection **CONFIRMED and fixed**; full H6 remains **MITIGATED**.

Each recording entry point captures the current epoch before constructing content, then carries that epoch unchanged through encoding and nonblocking queue admission. Clear advances the epoch; the writer retains its monotonic maximum and rejects appends from earlier epochs. Queue acceptance still determines persistence order. The change introduces no key-path lock, disk I/O or waiting.

This is an intra-service capture contract: content which has not yet entered a recording method is outside the captured operation. Concurrent post-increment producers may be erased if accepted before Clear itself, because overlapping calls are not ordered by sequence number. No cross-process ownership, durable generation, power-loss, Clear sync, overflow, duplicate-stop or retry guarantee is added.

## Executable criteria

| Verify | Exit | Expect / observed |
|---|---:|---|
| `cargo test --locked -p sakura-engine --lib clear_rejects_content_prepared_before_its_epoch` before production fix | 101 | Old content survived two completed Clears for all 3 content variants |
| `cargo test --locked -p sakura-engine --lib input_history::tests` after fix | 0 | 26 passed, 0 ignored, 0 failed |
| `cargo test --locked --workspace` final | 0 | 1,774 passed, 84 ignored, 0 failed; 93 summaries |
| `cargo fmt --all -- --check`, `git diff --check` | 0 | Formatting and whitespace checks succeed |
| `pwsh -NoProfile -File ci/check-process-clean.ps1 -RepositoryRoot <worktree>` | 0 | No repository-scoped runner remains |

The regression uses actual `record_key`, `record_commit`, `record_ai_text`, Clear, Flush and Shutdown on synthetic Windows files with DPAPI. A thread-local `cfg(test)` hook pauses the producer after content construction and before enqueue. Channels establish order without sleeps. The pause has a 10-second timeout; the owned producer is released and joined before assertions. The test checks both absence of pre-Clear content and acceptance of post-Clear content. The fixture is removed by RAII. Service filesystem operations still rely on ordinary Windows I/O completion and are not forcibly preempted by this hook timeout.

Both the hook call and its declaration are under `cfg(test)`; independent static review checked that default/release code has no hook. No shipped DLL/export inspection is claimed. The reviewer found no actionable in-scope defect and ran no tests; executable verification remained with the parent. Read-only review was a behavior constraint, not an enforced OS sandbox.

Rust/Cargo 1.96.0, x86_64-pc-windows-msvc, debug profile, locked dependencies. No production pipe, input store, IME registration or installed artifact was used. Log hashes and parsed summaries are in `history-clear-epoch-results.json`; full logs remain in the sibling local evidence directory. H6-specific PBT/C2/mutation/TLC, crash matrices and physical Windows E2E remain NOT_RUN.
