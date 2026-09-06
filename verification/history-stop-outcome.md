# History shutdown terminal outcome (#125)

Baseline main `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1`, v1.0.35. Patch base #124 (`7503362`). Three component contract violations are **CONFIRMED**; the broader H6 lifecycle remains **MITIGATED**.

`InputHistoryService::stop` now retains the worker handle and terminal result under one existing control mutex. One stop caller sends Shutdown, receives its result, joins even after reply loss, and only then publishes the outcome. Concurrent and later stop callers receive the same result. An OS error retains its raw code; other errors retain their kind and message. Worker panic prevents successful shutdown even after an earlier success reply. Duplicates do not inflate persistence-failure counters or send more Shutdown commands. The worker already counts its sync failure; stop only counts transport loss or join panic.

The writer never takes this mutex or owns the service Arc, so holding it across join does not introduce a worker-to-stop lock cycle. Recording/enqueue paths do not take it. This preserves the existing blocking control API: it is not a bounded shutdown deadline or proof that a stuck filesystem call can be interrupted. Producer admission closure after Shutdown, Flush/Clear concurrency and durable compaction are separate unfinished contracts.

## Executable criteria

| Verify | Expect / observed |
|---|---|
| `cargo test --locked -p sakura-engine --lib input_history::tests::stop_outcome_` before fix | Exit 101, three semantic failures: later stop loses durable error, lost reply retains unjoined handle, worker panic follows success reply but stop succeeds (`stop-before.log`) |
| Same command after initial fix | Exit 0, three passed (`stop-after.log`) |
| `cargo test --locked -p sakura-engine --lib input_history::tests` after concurrent fixture | Exit 0, 35 passed, no ignored/failed (`stop-history.log`) |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Initial map-identity lint failed; corrected to copied, final exit 0 (`stop-clippy-final.log`) |
| `cargo test --locked --workspace` | Exit 0, 1,792 passed / 84 ignored / 0 failed (`workspace-stop.log`) |
| `cargo fmt --all -- --check`, `git diff --check`, `pwsh -NoProfile -File ci/check-process-clean.ps1 -RepositoryRoot <worktree>` | Exit 0, no format/whitespace failure or repository-scoped runner survivor |

The concurrent fixture starts eight callers together, gates the synthetic worker with channels, checks that no duplicate Shutdown was queued, joins all callers and verifies every caller receives OS error 5. The first three are old-code counterexamples; the concurrent test is additional post-fix coverage. The dropped-reply test checks the actual join handle was consumed rather than assuming a scheduled worker has exited. Fixtures carry no production content or history path access. Windows x64, Rust/Cargo 1.96.0, debug, locked dependencies.

Independent adversarial static review passed the scoped lifecycle, caller and lock-order checks. The reviewer ran no tests and made no edits; read-only behavior was requested without OS enforcement. The cached outcome is in-memory state, not an on-disk durability record.

These tests do not qualify TLC/PBT/mutation, physical TSF, RDP, power loss, or all of H6. They verify the concrete stop terminal-result contract. Full-program status remains IN PROGRESS.
