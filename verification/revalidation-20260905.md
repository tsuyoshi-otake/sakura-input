# Revalidation ledger, 2026-09-05

Baseline: `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1` (remote main, v1.0.35).
Program status: **IN PROGRESS**, not a release qualification.
Initial labels below remain hypotheses until linked to current executable evidence.
Priority and confidence are independent; developer-history failures require explicit developer opt-in.

| ID | Current classification | Priority | Issue | Current evidence / next verification |
|---|---|---|---|---|
| H1 | CODE_CONFIRMED | P1 | pending dedicated issue | input_history::compact_file fixed temp, remove then rename; Windows cut points pending |
| H2 | MITIGATED | P1 | #120 | real duplicate engine initialized dictionary before ownership; fixed by early secured endpoint reservation; per-user cross-logon store ownership remains; engine-startup-ownership.md |
| H3 | MITIGATED | P1 | #105 | Flush/Shutdown compaction reproduced and removed; 0/16/64 MiB before/after measured; startup/periodic work remains; history-control-barriers.md |
| H4 | CONFIRMED | P1 | #116 | startup ID regression reproduced and fixed; one validation/ID pass, append handle before spawn; runtime overflow and durable non-reuse remain; history-startup-scan.md |
| H5 | CONFIRMED | P1 | #113 | fixed scan_frames propagation; 3 baseline counterexamples; 16 post-fix tests; see history-read-failure.md and results JSON |
| H6 | MITIGATED | P1 | #118 | old-content resurrection after Clear reproduced for key/commit/AI and fixed with entry epoch capture; duplicate stop, Flush retries, ownership and crash matrix remain; history-clear-epoch.md |
| T1 | CODE_CONFIRMED | P1 | #102 | send_key/link/connect/request and resync each supply independent 50ms budgets; whole-callback scripted/real pipe measurement pending |
| T2 | DEFENSE_IN_DEPTH | P1 | #52 | four component counterexamples reproduced and guarded; Requested/exact ticket, context generation, latest UI lease; execution receipt and real COM reachability remain; write-journal-authority.md |
| T3 | HYPOTHESIS | P1 | #57, #69, #7 | product reachability/COM teardown verification pending |
| T4 | HYPOTHESIS | P1 | #102, #107 | no current ETW attribution; prior timeout mitigation is not root-cause proof |
| C1 | HYPOTHESIS | P2 | #100 | old 221/477-char numbers are not this run's measurements |
| C2 | IMPROVEMENT | P2 | #103 | separate search/request/surface/frame/arena budgets; baseline measurement pending |
| C3 | IMPROVEMENT | P2 | #108, #93 | reported CandidateEvidence/projection implementation is in original dirty checkout, absent from fixed remote baseline; preserved, integration pending |
| O1 | IMPROVEMENT | P2 | #104 | existing v2 source_commit/provenance retained; runtime/error-boundary audit pending |
| V1 | CODE_CONFIRMED | P2 | #106 | requirements/spec pins mismatch even with LF normalization; actual rerun and dependency-closure gate pending |
| V2 | IMPROVEMENT | P2 | #67 | real ThreadMgr harness feasibility and CI execution graph pending |
| R1 | CODE_CONFIRMED | P2 | pending | learning::maintenance holds state try_lock across compact_state/sync_data; delayed-I/O reproduction pending; not a #107 diagnosis |
| A1 | IMPROVEMENT | P2 | pending | extraction only after two concrete applicable stores; runtime/supervisor audit pending |
| A2 | IMPROVEMENT | P2 | pending | config/dictionary/upgrade/security contract audit pending |

Required Issue snapshots (#7, #52, #57, #67, #69, #93, #100, #102–#108) were fetched through gh. No user input content is included in this ledger. Original dirty worktree remains untouched.

## Delivered patch mapping

| ID / bounded change | Issue | PR | Files / entry points | Test and artifact |
|---|---|---|---|---|
| H5 opaque frame preservation | #113 | #114 | engine input_history::scan_frames / repair_file | complete_frame_*; history-read-failure.md and results JSON |
| H3 control barriers / view retention | #105 | #115 | engine input_history::writer_loop / sync_writer_file; settings input_history::view | barrier_preserves_existing_ciphertext, control_barrier_size_matrix; history-control-barriers.md and results JSON |
| H4 startup maxima | #116 | #117 | engine input_history::open / scan_frames | startup_preserves_ciphertext_and_recovers_ids_before_retention; history-startup-scan.md and results JSON |
| H6 producer epoch | #118 | #119 | engine input_history::record_key / record_commit / record_ai_text / enqueue | clear_rejects_content_prepared_before_its_epoch; history-clear-epoch.md and results JSON |
| H2 early endpoint ownership | #120 | #121 | engine main::run, Server::reserve / run_when_ready | duplicate_engine_is_rejected_before_dictionary_initialization, startup_reservation_*, startup_spawn_failure_*; engine-startup-ownership.md and results JSON |

PRs are intentionally stacked in this order. They are unmerged; upstream main is not fixed merely because a branch test passes. CI Build and test, Build installer, and Dependency policy passed on #114/#115/#117/#119 when checked; fuzz jobs were skipped. Stacked-base CodeRabbit reviews were skipped, so their green status is not review evidence. Independent requested static reviews were separate, behaviorally read-only, and found no actionable defect within each recorded patch scope.

## Evidence limits

Unit/component tests using synthetic paths on the current Windows host are distinct from isolated real-TSF and physical E2E. No installation, IME registration, production pipe request, merge, release, signing change or production history read is authorized by this program.
