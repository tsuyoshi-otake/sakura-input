# T2 journal authority tranche (#52)

Main baseline: `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1`, v1.0.35. Patch base: PR #121 (`08c2ba0`).
Classification: **DEFENSE_IN_DEPTH** with four reproduced component counterexamples. No physical TSF or historical crash diagnosis is claimed.

## Contract

`complete_applied` requires the current Requested head and exact callback epoch/result revision through the existing document-free validator. Ready rejection remains valid; cancellation and known-prefix/unknown-document reconciliation retain their prior semantics. Context generations travel with operations, tickets and UI leases, so observing A, B, then A does not recover old context authority.

UI adoption now requires the exact latest issued lease. A newer same-revision output invalidates an old lease, even if the old completion arrives late. Explicit UI clear invalidates its issued authority as well. `clear_ui_lease_if_current` lets failed candidate publication retire only its own current lease. The TextService failure branch uses this guarded revocation, so a refused stale adoption does not clear/end a newer candidate publication. The content-free debug label is now `show_failed`; its CLI legend no longer promises that every failure ended UI.

These checks are necessary authority conditions, not evidence that a Requested document write actually executed. An explicit execution-completion receipt is still unfinished. Existing TextService callers continue to validate and complete after their document/projection or no-document path. Real COM reentrancy, shared candidate ownership across TextService instances, #57/#69 product reachability and #7 historical crash causality remain separate work.

## Executable criteria

| Verify | Exit | Expect / observed |
|---|---:|---|
| `cargo test --locked -p sakura-tsf --lib write_coordinator::tests::authority_` before fix | 101 | Four semantic failures: Ready Applied, altered ticket accepted, ABA lease adopted, older same-revision lease adopted |
| `cargo test --locked -p sakura-tsf --lib write_coordinator::tests` initial fix | 0 | 24 passed, 0 failed, 0 ignored |
| `cargo test --locked -p sakura-tsf --lib` after guarded candidate cleanup | 0 | 191 passed, 0 failed, 0 ignored |
| `cargo test --locked --workspace` final | 0 | 1,782 passed, 84 ignored, 0 failed; 93 summaries |
| `cargo fmt --all -- --check`, `git diff --check` | 0 | No format/whitespace failures |
| `ci/check-process-clean.ps1 -RepositoryRoot <worktree>` via pwsh | 0 | No owned runner remains |

Regressions additionally check six altered ticket fields independently on the fixed implementation, preserved Ready rejection, no terminal/projection change after invalid Applied, current UI lease survival after stale revocation, and one-time current revocation. The baseline altered-ticket test stopped at its first failing field (activation); it did not separately execute all malformed field cases on old code. Test inputs are synthetic, and component tests call no real host document.

Windows x64, Rust/Cargo 1.96.0, locked dependencies, debug profile. Existing TSF library tests and private pipe tests ran; they are not the #67 real ThreadMgr/KeystrokeMgr harness. T2-specific PBT/C2/mutation/TLC, shipped DLL inspection, isolated physical TSF and COM failure injection remain NOT_RUN. Input hashes and parsed full-log summaries are in `write-journal-authority-results.json`.

Independent adversarial static review inspected the diff, caller consequences and before/after logs and found no actionable in-scope defect. The parent executed all tests. The reviewer was behaviorally read-only, without OS-enforced read-only access.
