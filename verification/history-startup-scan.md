# H4 startup scan (#116)

Main baseline: `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1`, v1.0.35.
Patch base: `7fc39b7f0b3cbe4d958894a6515ad2872de9a05b` (PR #115).
Status: startup ID regression **CONFIRMED and fixed**. Full history lifecycle is not complete.

## Contract and change

Current-format startup validates the bounded store once, recovering sequence/session maxima before retention removes any frames. It preserves existing ciphertext. Mandatory append-handle creation completes before the worker starts; read/open errors propagate instead of becoming zero identifiers. Already exhausted stored identifiers are rejected. Structural tail repair now syncs its truncation before returning success; the H5 distinction between torn tails and complete opaque frames remains intact.

Legacy startup explicitly migrates before appending current-format engine markers. This exception still rewrites the store using existing compaction and inherits the unresolved H1 publication risks. Runtime overflow, durable non-reuse across Clear/retention/restart, cross-process store ownership and maintenance scheduling remain separate work. No filesystem atomicity or power-loss guarantee is added by this patch.

The old startup made several O(N) decode passes and an unconditional rewrite. The current-format path makes one O(N) bounded validation pass, stores only frame maxima plus the bounded file bytes, and opens the append handle. No startup latency benchmark was executed for this tranche; the earlier H3 size benchmark does not measure startup.

## Verification

Windows x64, Rust/Cargo 1.96.0, locked dependencies, debug profile, synthetic private files with real DPAPI. No production history or installed IME was touched.

| Verify | Exit | Expect / observed |
|---|---:|---|
| `cargo test --locked -p sakura-engine --lib startup_preserves_ciphertext_and_recovers_ids_before_retention` with old startup | 101 | Semantic failure: prefix=false, session=2 instead of 701, sequence=4 instead of 901 |
| `cargo test --locked -p sakura-engine --lib input_history::tests` final | 0 | 25 passed, 0 failed, 0 ignored |
| `cargo test --locked --workspace` final | 0 | 1,773 passed, 0 failed, 84 ignored; 93 summaries, 21 completely empty targets and 9 ignored-only targets |
| `cargo fmt --all -- --check`, `git diff --check` | 0 | No formatting/whitespace errors |
| `pwsh -NoProfile -File ci/check-process-clean.ps1 -RepositoryRoot <worktree>` | 0 | No repository-owned test runner remains |

Regressions additionally cover sequence/session exhaustion without mutation, legacy migration followed by current marker append, and inherited opaque-frame/future-format failure preservation. Independent adversarial static review found no actionable defect within this scope; all executable verification was performed by the implementing agent. The reviewer was behaviorally read-only, without an OS-enforced read-only sandbox.

`history-startup-scan-results.json` binds source/lockfile hashes to parsed full-log results. Full logs are retained in the local sibling evidence directory. PBT/C2/mutation/TLC specific to H4, disk-full/open-failure injection, concurrent writers, isolated real TSF and physical E2E remain NOT_RUN. Prior manifests describe their own recorded revision and scope.
