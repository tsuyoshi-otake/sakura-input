# History store ownership (#123)

Main baseline: `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1` (v1.0.35). Patch base: PR #122. H2 remains **MITIGATED** overall; two store-owner contract violations were reproduced before this patch.

`InputHistoryService::open` acquires an exclusive, separately named `input.bin.writer.lock` before repair, migration or append initialization. The actual worker owns the handle until exit or unwind. A caller retaining the stopped service Arc does not prevent a successor. Public offline `clear_path` participates in the same protocol; already-owned maintenance calls a private helper without acquiring recursively.

The handle uses atomic `create_new`, no sharing, `FILE_FLAG_DELETE_ON_CLOSE` and `FILE_FLAG_OPEN_REPARSE_POINT`. Existing objects are refused rather than adopted or removed. A surviving sidecar after OS/storage failure requires explicit recovery; this change does not implement automatic stale-lock removal. The sidecar stays independent of canonical history replacement. Normal input remains available when this optional developer-history service cannot acquire ownership; the existing runtime initialization-failure latch prevents per-key retries while enabled configuration is unchanged.

## Executable criteria

| Verify | Expect / observed |
|---|---|
| `cargo test --locked -p sakura-engine --lib input_history::tests::store_owner_` on old implementation | Two semantic failures: second service admitted and offline Clear bypassed owner (`store-lock-before.log`, exit 101) |
| Collision regression against initial `.create(true)` iteration | One semantic failure: preexisting lock adopted; corrected to `.create_new(true)` (`store-lock-collision-before.log`, exit 101) |
| `cargo test --locked -p sakura-engine --lib input_history::tests` after collision fix | 30 passed, 0 failed, exit 0 (`store-lock-final-targeted.log`) |
| `cargo test --locked -p sakura-engine --test pipe_round_trip history_store_owner_in_another_process_keeps_input_available` | Parent owns synthetic store; separate owned engine reports history inactive, returns normal key Output and preserves store bytes; exit 0 (`store-lock-process.log`) |
| `cargo test --locked --workspace` after collision fix | 1,787 passed, 84 ignored, 0 failed; exit 0 (`workspace-store-lock-final.log`) |
| `cargo test --locked -p sakura-engine --lib input_history::tests::store_owner_preserves_preexisting_symlink_and_target` after explicit reparse flag | Actual synthetic Windows file symlink refused, link and target preserved, no canonical created; 1 passed, exit 0 (`store-lock-symlink.log`) |
| `cargo test --locked --workspace` after review changes | 1,788 passed, 84 ignored, 0 failed; exit 0 (`workspace-store-lock-reviewed.log`) |
| `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` | Passed, exit 0; Clippy log `store-lock-clippy.log` |
| `pwsh -NoProfile -File ci/check-process-clean.ps1 -RepositoryRoot <worktree>` after tests | No repository-scoped runner remains |

The canonical replacement fixture uses rename plus a new synthetic file, not ReplaceFileW crash injection. The symlink case requires Windows permission to create file symlinks. Review raised reparse-target deletion as a concern; deletion through the prior CREATE_NEW implementation was not reproduced. The explicit flag and real symlink regression are defense in depth. An earlier iteration using `.create(true)` to open existing files did have a reproduced regular-file preservation violation.

All data and pipe names are test-owned. Windows x64 MSVC, Rust/Cargo 1.96.0, debug profile, locked dependencies. No dependency version changed. The Windows filesystem feature is enabled on the existing windows dependency. Logs are local sibling artifacts; the results JSON records hashes and exact run scope, not hermetic attestation or physical TSF qualification.

Independent adversarial static review requested the explicit reparse flag and fixture, then passed the corrected scope. The parent performed all executable verification. Read-only behavior was requested from the reviewer, without OS-enforced read-only access. A panic in the test-only setup callback before Engine construction can still leak its synthetic profile; the successful test paths explicitly clean up.

This protocol coordinates updated writers sharing the same logical store path. It does not establish compatibility with old binaries ignoring the sidecar, hard-link aliases, different-user ACL setups, RDP/multi-logon physical execution, learning/config ownership, watchdog pacing, atomic durable compaction, Clear durability, runtime ID exhaustion, duplicate-stop semantics or full crash recovery. H1/H2/H6 and the complete program are not closed.
