# Correspondence and adversarial re-audit — developer-history hot-enable

## Requirement → evidence

| Requirement | Oracle | Concrete / PBT | TLA | Implementation | Evidence |
|---|---|---|---|---|---|
| ON → publish → request attaches without restart | `RequestBoundary` sets `service_attached = published_on` | `observed_stale_inactive…`, PBT seed `0x4448435332000001` | `RequestBoundary`, Safety `ForbiddenStaleInactive` | `DynamicRuntimes.input_history`, `set_input_history(Option)` | `server.rs` test; pipe hot-enable |
| OFF → publish → request detaches | same | `hot_disable_stops_new_durable_keys` | ModeOff + detach | clear dynamic + dispatcher `None` | dispatch hot detach test |
| Durable only Normal∧classified∧¬test_only | Key arms | `unclassified_password…` | `Key` | `record_key` | existing dispatch history test |
| `stats.active ⇔ attached` | `stats_active()` | oracle asserts | `StatsIffAttached` (same var) | `input_history_stats` | hot attach/detach stats replies |
| Forbid live stale-inactive | `forbidden_stale_inactive` | concrete + PBT | Safety | hot path removes defect | TLC logs pass |

## Boundary → fault injection

| Boundary | Injection | Result |
|---|---|---|
| Queue full | oracle `QueueFull` + Key | `dropped` increments |
| Persist fail | `PersistFail` | counter only |
| Clear epoch | `Clear` | durable cleared |
| Torn log | existing `input_history` repair tests | ignored tail / repair |
| Crash/restart | oracle + TLC Crash/Restart | durable retained; attach from setting |
| Publish before request | mid-state detached allowed | not forbidden until request |
| CLI settle | retry 30×100ms across watcher | settle-failed hard error |

## Adversarial re-audit

1. **Could CLI still print restart-required as success?** No — strings removed; settle mismatch returns `Err`.
2. **Could two writers open the same file?** `runtime_services` prefers `Shared.input_history` before dynamic open; OFF drops only dynamic.
3. **Could attach leave sessions on colliding protocol ids?** Attach reallocates via `reallocate_history_session_ids`.
4. **Is TLC diameter a product guarantee?** No — exploration bound only; documented in `tla-record.md`.
5. **Did this cloud agent run Windows cargo-mutants / llvm-cov?** No — labeled N/A; must not be claimed as scores.
6. **Conversion ranking (#60)?** Out of scope.

## Residual risk / release decision

| Residual | Severity | Disposition |
|---|---|---|
| Watcher ≤500 ms + CLI 3 s budget | Low | Accepted; documented |
| DPAPI / disk OS failures | Medium | Fail-closed counters; not TLC-exhaustive |
| Windows integration tests not executed on this Linux agent | Medium | Code present; run on Windows CI/host before ship |
| Mutant score unmeasured here | Medium | Killer tests exist; run mutants on Windows |
| Fingerprint collision notes from TLC | Negligible | No error found |

**Release decision for this change set:** merge as the hot-enable defect fix with verification artifacts; gate installer/ship on Windows test green for engine/settings history suites.

## Rubric map

| Criterion | Artifact |
|---|---|
| C1 | `developer_history_oracle.rs` + oracle tests |
| C2 | `pbt-seed.txt`, `pbt-shrunk-counterexample.md`, `coverage/c2-report.md` |
| C3 | server/dispatch/CLI changes + tests |
| C4 | `coverage/c2-report.md` |
| C5 | `mutation-report.md` |
| C6 | `boundary-inventory.md` + pipe/dispatch tests |
| C7 | `tla-record.md` + `tlc/*/stdout.log` |
| C8 | this file + `developer-history-verification.canvas.tsx` |
