# H5: preserve opaque developer-history frames (#113)

Baseline production source: `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1` (v1.0.35).
Classification: **CONFIRMED by component counterexamples**, developer-mode only.
This patch does not establish a real-user DPAPI failure or address all H1–H6.

## Contract and implementation

`scan_frames` distinguishes structurally incomplete/checksum-damaged tails from complete CRC-valid frames whose content cannot be decrypted or decoded. The latter now propagate an error, preventing `repair_file`, `compact_file`, and `InputHistoryService::open` from treating an opaque frame and everything after it as disposable tail bytes. Startup already catches the history error and continues normal input without history. DPAPI OS errors and record decoding errors keep their existing error representation.

The successful scan remains O(file bytes + number of records); no new scan, allocation, encryption, dependency, wire or format version is introduced. Existing structural-tail repair and legacy-version reading remain unchanged. Deliberate user Clear, replacement crash safety, writer ownership, startup high-watermarks and runtime retry semantics remain separate work.

## Executed verification

Windows x64, rustc 1.96.0 (`ac68faa20`), cargo 1.96.0 (`30a34c682`), locked workspace dependencies. Tests use synthetic files and real current-user DPAPI; they never read installed history or change IME registration.

| Command | Exit | Observable result |
|---|---|---|
| `cargo test --locked -p sakura-engine --lib complete_frame_` on baseline production code plus new tests | 101 | 0 passed, 3 failed, 0 ignored; each byte-preservation assertion failed because repair erased opaque data |
| `cargo test --locked -p sakura-engine --lib input_history::tests` after fix | 0 | 16 passed, 0 failed, 0 ignored |
| `cargo test --locked --workspace` after fix | 0 | 1,763 passed, 0 failed, 84 ignored across 93 result summaries; 30 summaries ran zero tests, including empty doctest targets |
| `cargo fmt --all -- --check` | 0 | no formatting errors |
| `git diff --check` | 0 | no whitespace errors |
| `pwsh -NoProfile -File ci/check-process-clean.ps1 -RepositoryRoot <worktree>` after targeted and workspace tests | 0 | no repository-scoped Sakura process or test runner remains |

New regressions cover invalid DPAPI ciphertext, DPAPI-valid unknown record type, DPAPI-valid incomplete record fields, unsupported file version, and structural torn/CRC-damaged tail negative controls. Fixtures retain a valid record both before and after the opaque record; full byte equality is checked after repair, compaction and failed service startup. Fixture Drop removes the owned file and known compaction sidecar even during assertion failure. This guarantee covers complete frames inside the hard size cap; preexisting over-cap truncation is unchanged.

Independent read-only adversarial review identified that the original unknown-record fixture failed before reaching the unknown-kind branch. It now supplies the complete common header and checks that exact decode error. The v1 test now also covers repair, compaction and startup/stop. Reinstating the baseline scan failure branches with corrected tests again produced 3 semantic failures (exit 101); restoring the fix produced 16 passing History tests (exit 0). Workspace totals above precede these test-only improvements; production logic is identical. A second read-only review found no remaining actionable defect in this bounded H5 patch. Review was by a fresh-context GPT-5.6 Luna agent; execution and cleanup verification were performed by the implementing agent.

Full logs are retained in the local sibling `sakura-revalidation-evidence-20260905` directory. `history-read-failure-results.json` records their hashes and parsed test results. The fail-first log is from before rustfmt; baseline production identity is fixed by Git, but a contemporaneous hash of the augmented test file was not captured. Do not treat it as a complete hermetic build attestation.

PBT/C2 results produced by existing workspace campaigns are not proof of new H5 predicates. H5-specific mutation, TLC, disk-full/crash cut points, 16/64 MiB measurements, AppContainer, real TSF, shipped artifacts and physical Windows E2E are **NOT_RUN**. Existing ignored tests were not executed by the workspace command. No measured latency improvement or power-loss guarantee is claimed.

## API reference check

The current [Rust File documentation](https://doc.rust-lang.org/std/fs/struct.File.html) distinguishes synchronous I/O and explicit synchronization; the online page currently describes a newer stable compiler than this run's 1.96 toolchain. [ReplaceFileW documentation](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew) was re-read for the separate H1 work. This H5 patch adds neither replacement nor durability claims.
