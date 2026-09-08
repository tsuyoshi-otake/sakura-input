# H3 periodic history maintenance (#130)

Baseline main f48d8f55fcab3f9abb210aa2a442cd8b217df1f1, v1.0.35. Patch base 15761bc (#129). Unexpired idle history is rewritten by the old maintenance actor: **CONFIRMED** with a synthetic DPAPI store and deterministic timeout-observation hook. This patch removes unnecessary periodic compaction; full H3 remains MITIGATED.

The existing startup validation scan now records the earliest retained timestamp. Current-format startup adds no second scan. Legacy migration and successful compaction return the new earliest timestamp. Successful Clear resets it. Append carries internal timestamp metadata and observes it immediately before writing: post-write errors can leave complete frames, whereas pre-write validation, encryption, capacity and open errors cannot introduce a record. Sticky append failure reporting remains unchanged.

Idle and append-count checks perform O(1) expiry checks instead of unconditional O(N) decrypt/re-encrypt/publication. They compact only when oldest < now minus 30 days, preserving the inclusive retention boundary. Successful compaction refreshes the hint; failures leave a conservative hint for later bounded-interval maintenance. Size-pressure compaction remains O(N) and may retry on subsequent appends. No new thread, key-path filesystem access, retention policy, cap, dependency version or trust change is introduced.

## Executable criteria

| Verify | Expect / observed |
|---|---|
| `cargo test --locked -p sakura-engine maintenance_unexpired_idle_store_preserves_ciphertext` on old code | Semantic failure: unexpired store bytes changed, exit 101 (`maintenance-before.log`) |
| `cargo test --locked -p sakura-engine input_history::tests` | 47 passed, 0 failed (`maintenance-final-history.log`) |
| New uncertain-append actor test | Injected error after complete write still schedules idle expiry; record removed; Shutdown still reports append loss |
| New pre-write rejection test | Oversized rejected payload leaves the current unexpired hint unchanged |
| Existing idle-expiry and new cutoff/startup/compaction tests | Expired records removed, cutoff retained, clock rollback safe, empty Clear has no work |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Exit 0 (`maintenance-final-clippy.log`) |
| `cargo test --locked --workspace` | 1,805 passed, 84 ignored, 0 failed (`workspace-maintenance-final.log`) |
| `cargo fmt --all -- --check`; `git diff --check` | Exit 0 |
| `pwsh -NoProfile -File ci/check-process-clean.ps1 -RepositoryRoot <worktree>` | Exit 0, no repository-scoped test survivors |

Final full checks ran after the final production and regression-test changes. Initial uncertain-write test compilation used a nonexistent encoder name; corrected to the existing record encoder before testing. That compiler failure is not a semantic counterexample. Independent adversarial review found the intermediate all-errors hint update could trigger unnecessary maintenance before any write; moved observation to the write boundary and added negative control. Re-review PASS. Reviewer was behaviorally read-only, not OS-enforced; all execution was parent-owned.

The barrier benchmark test now supplies its known synthetic timestamp plan and opens the append handle before timing; it does not accidentally measure the test helper's fixture scan. No new size benchmark was executed in this tranche and older barrier numbers are not this implementation's performance evidence. Ciphertext reuse, cap-pressure repeated scans, crash recovery, power-loss/Clear durability and physical TSF remain unfinished. No production history, installation or registration was touched.
