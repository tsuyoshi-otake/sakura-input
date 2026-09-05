# Revalidation ledger, 2026-09-05

Baseline: `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1` (remote main, v1.0.35).
Program status: **IN PROGRESS**, not a release qualification.
Initial labels below remain hypotheses until linked to current executable evidence.
Priority and confidence are independent; developer-history failures require explicit developer opt-in.

| ID | Current classification | Priority | Issue | Current evidence / next verification |
|---|---|---|---|---|
| H1 | CONFIRMED | P1 | #127 | real Windows sharing conflict loses canonical on old code; ReplaceFileW/backup/pending-marker containment tested; generation recovery, process-crash and power-loss matrix unfinished; history-compaction-publication.md |
| H2 | MITIGATED | P1 | #120, #123 | early secured endpoint reservation and cooperative per-path history writer lock; same-store process regression passes; legacy writers, physical cross-logon and watchdog work remain; engine-startup-ownership.md, history-store-ownership.md |
| H3 | MITIGATED | P1 | #105, #116, #130 | control/startup rewrite removed; periodic unexpired rewrite reproduced and gated by cached earliest timestamp; ciphertext reuse and cap-pressure retries remain; history-maintenance-plan.md |
| H4 | CONFIRMED | P1 | #116, #132 | startup ID and live counter panic/wrap reproduced and fixed; checked terminal allocation; durable generation/non-reuse remains; history-startup-scan.md, history-counter-exhaustion.md |
| H5 | CONFIRMED | P1 | #113 | fixed scan_frames propagation; 3 baseline counterexamples; 16 post-fix tests; see history-read-failure.md and results JSON |
| H6 | MITIGATED | P1 | #118, #125 | pre-Clear producer resurrection and stop terminal-result counterexamples reproduced/fixed; store ownership in #123; producer admission closure, Flush retries, Clear durability and crash matrix remain; history-clear-epoch.md, history-stop-outcome.md |
| T1 | CONFIRMED | P1 | #102, #134 | serial private-pipe callback110ms reproduced; parent deadline prevents later key send; pre-send/partial-frame/cancellation boundaries tested; physical COM/ETW/hard timing and deferred UI remain; tsf-callback-deadline.md |
| T2 | DEFENSE_IN_DEPTH | P1 | #52 | four component counterexamples reproduced and guarded; Requested/exact ticket, context generation, latest UI lease; execution receipt and real COM reachability remain; write-journal-authority.md |
| T3 | HYPOTHESIS | P1 | #57, #69, #7 | product reachability/COM teardown verification pending |
| T4 | HYPOTHESIS | P1 | #102, #107 | no current ETW attribution; prior timeout mitigation is not root-cause proof |
| C1 | HYPOTHESIS | P2 | #100 | old 221/477-char numbers are not this run's measurements |
| C2 | IMPROVEMENT | P2 | #103 | separate search/request/surface/frame/arena budgets; baseline measurement pending |
| C3 | IMPROVEMENT | P2 | #108, #93 | reported CandidateEvidence/projection implementation is in original dirty checkout, absent from fixed remote baseline; preserved, integration pending |
| O1 | IMPROVEMENT | P2 | #104 | test-only image-policy diagnostics after observed CI rejection; same-commit retry passed, root cause unknown; existing v2 provenance retained, broader runtime/error audit pending; appcontainer-policy-evidence.md |
| V1 | CONFIRMED | P2 | #106 | TLC runner returned success after Java failure, fixed with explicit result classification and process cleanup; 5 bounded searches and 4 expected witnesses rerun; historical verdict isolated as STALE; REQ09 model correspondence, PBT/C2/mutation manifest and dependency-closure gate pending; space-tlc-execution.md |
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
| T2 journal authority | #52 | #122 | TSF WriteCoordinator / guarded candidate cleanup | authority_*; write-journal-authority.md and results JSON |
| H2 history store ownership | #123 | #124 | engine input_history::open / clear_path / acquire_store_owner | store_owner_*, history_store_owner_in_another_process_keeps_input_available; history-store-ownership.md and results JSON |
| H6 joined stop result | #125 | #126 | engine InputHistoryService::stop / WriterShutdown | stop_outcome_*; history-stop-outcome.md and results JSON |
| H1 publication containment | #127 | #128 | engine compact_file / replace_history_file / require_no_compaction_transaction | publication_*; history-compaction-publication.md and results JSON |
| O1 AppContainer policy evidence | #104 | #129 | appcontainer integration test rejection diagnostic | image_policy_diagnostics_*; appcontainer-policy-evidence.md and results JSON |
| H3 periodic retention plan | #130 | #131 | input_history::RetentionPlan / writer_loop_with_file / append_payload | maintenance_*; history-maintenance-plan.md and results JSON |
| H4/H6 live counter exhaustion | #132 | #133 | InputHistoryService::allocate_counter / allocate_session_id / record_* / clear; dispatch/session unavailable-history boundary | counter_exhaustion_*; history-counter-exhaustion.md and results JSON |
| T1 callback IPC deadline | #134 | #135 | TSF CallbackDeadline/key entries/Engine, Client::call_until/transfer | callback_deadline_*, partial_reply_deadline, completion_racing_timeout, expiry_between_final_check; tsf-callback-deadline.md and results JSON |
| V1 TLC execution / stale verdict | #106 | pending | verify-space-key-dispatch-tlc.ps1 / Get-TlcOutcome / Invoke-BoundedProcess; traceability schema 2 | self-test, 9 TLC configs, actual timeout; space-tlc-execution.md and generated results |

PRs are intentionally stacked in this order. They are unmerged; upstream main is not fixed merely because a branch test passes. CI Build and test, Build installer, and Dependency policy passed on #114/#115/#117/#119 when checked; fuzz jobs were skipped. Stacked-base CodeRabbit reviews were skipped, so their green status is not review evidence. Independent requested static reviews were separate, behaviorally read-only, and found no actionable defect within each recorded patch scope.

## Evidence limits

Unit/component tests using synthetic paths on the current Windows host are distinct from isolated real-TSF and physical E2E. No installation, IME registration, production pipe request, merge, release, signing change or production history read is authorized by this program.
