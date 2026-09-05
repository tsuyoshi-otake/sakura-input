# H3 control-path tranche (#105)

Main baseline: f48d8f55fcab3f9abb210aa2a442cd8b217df1f1, v1.0.35.
Patch base: H5 commit 5f3463b091e5200c66354a2aef2ad7881df61eda (PR #114).
Status: control-handler rewrite **CONFIRMED and fixed**; full H3/#105 **MITIGATED**, not closed.

## Behavior

Flush and Shutdown process preceding accepted queue commands and then call `File::sync_all` on the owned append handle. Neither control handler decrypts, sorts, encrypts or rewrites the store. A missing handle or synchronization failure is an error. A preceding append failure stays observable at later barriers until a successful explicit Clear; syncing a prefix cannot recover a dropped append. Queue admission drops remain separately counted.

The queue remains FIFO with a 1,024-command bound. Concurrent producers are ordered by actual channel acceptance, not sequence-allocation time. Flush does not reset maintenance counters or postpone its deadline. Maintenance already queued/in progress, synchronous filesystem stalls and caller timeout are not preempted. A barrier reply is an observed completed sync, not a guarantee against every filesystem/controller/power-loss failure.

Settings `view` now applies the same inclusive 30-day cutoff in memory; export and mine use that view. The raw `read_snapshot` API remains unfiltered for repair/IDs/accounting. This deliberately also removes expired records from an offline view/mine instead of waiting for the next maintenance rewrite. Size limits, periodic retention, DPAPI, opt-in and scope checks are unchanged. Startup compaction, every-256-append maintenance, stable ownership/recovery and retry/Clear/stop coordination remain separate H1/H2/H4/H6 work.

## Verification

Windows x64 local synthetic files and real DPAPI, rustc/cargo 1.96.0, locked dependencies, debug profile. No installed history, production pipe, registration or IME installation used.

| Command / check | Exit | Result |
|---|---|---|
| `cargo test --locked -p sakura-engine --lib barrier_preserves_existing_ciphertext` before fix | 101 | 2 semantic failures: control handler rewrote/expired frames |
| `cargo test --locked -p sakura-engine --lib input_history::tests` after initial fix | 0 | 21 passed, 0 ignored, 0 failed |
| `cargo test --locked --workspace` on final code | 0 | 1,770 passed, 84 ignored, 0 failed; 93 result summaries including 21 completely empty and 9 ignored-only targets |
| `cargo test --locked -p sakura-engine --lib control_barrier_size_matrix -- --nocapture` with old control branches / final branches | 0 / 0 | 1 executed test each; size measurements below |
| format, diff whitespace, process-clean | 0 | no errors or owned surviving runners |

Additional tests cover real Windows read-only-handle sync failure, missing handle, repeated barriers after failed append, successful Clear resetting that failure, retention cutoff/order/early clock, and real settings view preserving raw bytes. The deterministic control tests prequeue commands and run the writer to Shutdown on the test thread, without probabilistic sleep or a detached worker. Independent read-only adversarial review found no actionable patch defect. Executable verification belongs to the implementing agent.

### Same-host size comparison

The timed operation is **Flush + Shutdown together**, not one RPC or a physical key callback. Fixtures repeat a synthetic encrypted frame to measure byte-count cost; they are not a unique-ID or language corpus. Setup writes and syncs before timing; both paths open the same size/type of store. The old handler branches were temporarily restored in the test build, then the final source restored. No new performance threshold is imposed on noisy CI runners.

| Requested MiB | Actual bytes | Old handler pair, ms | Final handler pair, ms |
|---:|---:|---:|---:|
| 0 | 8 | 3.371 | 1.662 |
| 16 | 16,777,054 | 44,504.910 | 5.375 |
| 64 | 67,108,764 | 189,808.530 | 5.105 |

Single sample per size/path, debug builds, warm OS data after fixture sync. These are not p95, cold-start, production export or shipped-artifact measurements. Old path total test time was 234.41 seconds; it completed normally, then the scoped process check found no survivor. The new handler removes O(store records/bytes) application compaction work from each control handler; filesystem sync cost can still depend on dirty data and OS behavior.

Full logs remain in the local sibling evidence directory; generated `history-control-barriers-results.json` stores their hashes and parsed results. H3-specific PBT/C2/mutation/TLC, live export/pipe delays, crash/disk-full/Clear race matrix, and physical Windows/TSF remain NOT_RUN. The baseline H5 manifest remains evidence for its recorded parent scope, not an attestation of this changed file.
