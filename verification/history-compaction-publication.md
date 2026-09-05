# H1 compaction publication containment (#127)

Baseline main `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1` (v1.0.35); patch base #126 (`c458481`). Canonical loss under a real Windows sharing conflict and legacy-temp overwrite are **CONFIRMED**. This is the first bounded publication patch, not completed generation-based recovery or H1 qualification.

The old implementation removes canonical before renaming a fixed temporary file. A test-only hook now opens the prepared replacement with no delete sharing immediately before publication. On old code, rename fails and canonical is absent. This is an actual Windows filesystem failure using synthetic encrypted data; the hook only supplies the timing/handle, not the failing return value. A separate old-code test shows the fixed legacy compact.tmp is consumed even when it already contains unrelated synthetic bytes.

Compaction now exclusively creates an `input.bin.compaction` directory adjacent to canonical. Its `replacement.bin` is create_new; `previous.bin` is the ReplaceFileW backup. Directory collision is refused, with no PID-only uniqueness assumption or adoption of preexisting artifacts. This bounded namespace permits at most one unresolved transaction for that store. It remains independently protected by #124's store-owner lock. Initial canonical creation remains separate from replacement.

Replacement receives full schema/frame CRC/DPAPI validation and a successful sync_all before publication. Canonical is never pre-deleted. ReplaceFileW uses backup and flags zero; published canonical is synced and validated before removing backup and the directory. Validation is bounded at the existing cap plus one byte. The final length/CRC comparison detects accidental mismatch; it is not cryptographic authentication or a validated generation number.

Any unresolved directory causes read, startup/ensure/reopen and both offline/worker Clear to fail with a content-free recovery-required error. On a publication/sync/validation/cleanup failure, the code retains the directory and whatever canonical/replacement/backup candidates exist. This prevents reopening a missing canonical as a new empty store. Successful publication removes its backup before returning. Clear cannot falsely succeed while this new transaction retains old history. No automatic restoration or mtime-based selection is implemented.

## Failure states and evidence

| Cut / outcome | Retained state / response |
|---|---|
| Replacement write/sync-stage injected failure | Old canonical plus pending directory/replacement; read/reopen/Clear refuse |
| Injected 1175, 1176 with backup, or ordinary error 5 | Original canonical and replacement preserved; reopen/Clear refuse |
| Injected 1177 | Original moved to previous.bin, replacement retained, canonical absent; reopen cannot create it |
| After successful replace, before/after canonical sync | New canonical, previous.bin and pending directory; reopen/Clear refuse |
| After backup removal, before directory removal | New canonical and pending directory; reopen/Clear refuse |
| Successful retained or empty compaction | Valid canonical, no transaction directory or backup; normal read/Clear work |

The partial-error fixtures model the documented file-name states using real files, then inject the specified error. They do not reproduce those errors inside the Windows kernel. The five stage failures return synthetic disk-full errors at boundaries; they are not actual disk exhaustion, process termination, failed hardware sync, or power-loss tests. Microsoft's [ReplaceFileW contract](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew) documents partial outcomes, unsupported WRITE_THROUGH and the same-volume requirement; flags zero preserve ACL failure handling.

## Executable criteria

| Verify | Expect / observed |
|---|---|
| `cargo test --locked -p sakura-engine --lib input_history::tests::publication_real_sharing_failure_preserves_canonical` on old production code | Exit 101: canonical absent after failed rename (`publication-sharing-before.log`) |
| `cargo test --locked -p sakura-engine --lib input_history::tests::publication_` before fix | Exit 101: legacy-temp collision plus two negative controls for the newly introduced pending-transaction contract (`publication-before.log`); those two controls are not historical backup incidents |
| Same publication filter after first fix | Exit 0, 4 passed (`publication-after.log`) |
| `cargo test --locked -p sakura-engine --lib input_history::tests` with partial outcomes/stages | Exit 0, 42 passed (`publication-history.log`) |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Exit 0 (`publication-clippy.log`) |
| `cargo test --locked --workspace` | Exit 0, 1,799 passed / 84 ignored / 0 failed (`workspace-publication.log`) |
| Final publication filter after review comment/assertion strengthening | Exit 0, 7 passed (`publication-reviewed.log`); no production behavior changed after the workspace run |
| `pwsh -NoProfile -File ci/check-process-clean.ps1 -RepositoryRoot <worktree>` | Exit 0, no repository-scoped runner survivors |

Windows x64 MSVC, Rust/Cargo 1.96.0, debug, locked dependencies. Test hooks compile only in the unit-test binary. No production data, pipe, installation or registration touched. Format and diff checks passed.

Independent adversarial review questioned backup removal before directory cleanup. Reassessment confirmed that verified/synced new canonical is a sufficient surviving candidate at that boundary. The comment was narrowed and the final test explicitly validates canonical after backup deletion; scoped re-review passed with those changes. Reviewer access was behaviorally read-only, not OS-enforced, and the parent ran all tests.

## Deliberate unfinished contracts

There is no validated generation, automatic/manual recovery command, OS-power-loss guarantee, process-crash matrix, or complete Clear durability guarantee in this tranche. A harmless transient publication failure can leave history unavailable until a separately validated recovery procedure is implemented. Artifacts remain encrypted and bounded; the patch never instructs users to delete the marker to recover. Existing legacy compact.tmp artifacts are not adopted, restored or deleted automatically; their privacy/recovery handling remains a migration item. Legacy writers ignoring the store lock, path aliases and physical RDP/TSF remain unqualified.

Compaction still decodes/re-encrypts retained records and remains O(N), now with additional bounded validation I/O. This patch prioritizes failure containment; unchanged-store rewrite avoidance and ciphertext reuse remain H3 work. The 0/16/64 MiB performance figures from older barrier patches do not measure this implementation. Full H1/H3/H6 and the program remain IN PROGRESS.
