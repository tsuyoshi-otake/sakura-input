# Correspondence table and adversarial re-audit — Space-key-dispatch

Date: 2026-08-16
Revision: `f26191aa16a6b3569cdf004e4852650f7de1a17f`
Machine-readable map: `traceability.json`

## Correspondence

| ID | Oracle | Example / API | PBT / ATOM | MUT | TLA | Implementation | Evidence |
|---|---|---|---|---|---|---|---|
| REQ-SPACE-01 | `space_effect` Convert | `composing_space_converts_*`, `api_composing_space_*`, `production_single_connection_composing_*` | PROP-SPACE-NODUAL; ATOM-COMPOSING/CONVERTING | space_effect / apply_space caught | `FocusedSpace`, `PerConnectionSpaceIsConvert`, reach-convert | `Action::Convert` / `begin_conversion` ~2739 | oracle tests; pipe |
| REQ-SPACE-02 | Idle Insert | `idle_space_inserts_*`, `api_idle_space_*` | PROP; ATOM-IDLE | caught | `FocusedSpace`, reach-insert | `idle_space_commit` ~1837–1871 | oracle; pipe |
| REQ-SPACE-03 | `no_dual_effect` + fence in `apply_space` | `reported_dual_delivery_must_not_*` | PROP-SPACE-NODUAL | canary `no_dual_effect->true` caught | `NoDualEffect`, reach-dual | **product violates** share-nothing workers | `pbt-production-shrunk-counterexample.md`; `fail-dual-delivery.txt` |
| REQ-SPACE-04 | Idle Absorb when peer converting | `idle_peer_is_absorbed_*` | ATOM-PEER-CONVERTING | peer_converting caught | `FencedIdleDoesNotInsert` | **absent** (no cross-worker fence) | oracle only; product CE |
| REQ-SPACE-05 | Commit/Cancel/Replace → Idle | `commit_returns_*`, `cancel_and_replace_*`, `fail_cancel_*` | PBT | apply caught | `Commit`, `ReplaceContext` | Enter/Escape keymap | oracle; pipe |
| REQ-SPACE-06 | CrashRestart | `crash_restart_*`, `fail_crash_restart_*` | ATOM-CRASH-EVENT | apply caught | `CrashRestart` | new Dispatcher / new process | unit + pipe |
| REQ-SPACE-07 | TimeoutSpace absorb | `timeout_space_*`, `fail_timeout_*`, `fail_retry_*` | PBT | apply Timeout arm | `TimeoutSpace` | client budget / Busy | failure-injection |
| REQ-SPACE-08 | DropSpace | `dropped_space_*`, `fail_omission_*` | PBT | apply | (omission = stutter) | host omit | oracle; pipe |
| BND-SPACE-01 | MAX_DOCUMENT_SPACES=4 | `resource_exhaustion_*`; product unbound witness | ATOM-SPACE-EXHAUSTED | match-guard caught | `SpacesBounded` / MaxSpaces | **model only**; product no quota | `fail-exhaust.txt` |
| BND-SPACE-02 | MAX_CONNECTIONS=3 | PBT actors 1..3 | — | — | ActorCount 1/2/3 | accept pool | TLC actors configs |
| BND-SPACE-03 | logical_time++ | PBT | — | — | eventCount | oracle/TLA | c2 + TLC |
| FAIL-SPACE-DUAL | fence absorbs idle | production CE recorded | PROP | — | DualSpace + NeverDualEffect | server share-nothing | dual CE artifacts |
| FAIL-SPACE-DUP | idle accumulates | `fail_duplicate_*` | — | — | FocusedSpace×2 | idle_space_commit | pipe |
| FAIL-SPACE-OMIT | DropSpace | `fail_omission_*` | — | — | stutter | host | pipe |
| FAIL-SPACE-REORDER | Space then Type | `fail_reorder_*` | — | — | FocusedSpace; Type | dispatch | pipe |
| FAIL-SPACE-CANCEL | Escape then Space | `fail_cancel_*` | — | — | Commit/Cancel | Escape | pipe |
| FAIL-SPACE-TIMEOUT | TimeoutSpace | `fail_timeout_*`, `fail_retry_*` | — | — | TimeoutSpace | 1ms budget | failure-injection |
| FAIL-SPACE-CRASH | CrashRestart | `fail_crash_restart_*` | — | — | CrashRestart | kill+respawn | pipe |
| FAIL-SPACE-EXHAUST | oracle absorb | oracle exhaustion; product unbound | ATOM-SPACE-EXHAUSTED | — | SpacesBounded | none in product | fail-exhaust |
| FAIL-SPACE-DEAD | Disconnect Ignore | `disconnected_connection_*`, `fail_partial_drop_*` | ATOM-LIVE | peer/space Ignore | Disconnect | drop client | oracle; pipe |

## Adversarial re-audit

### Unverified

- Live COM `ITfContext` / multi-context Electron/Cursor host with one physical Space.
- Installed IME profile end-to-end.
- True MC/DC of `dispatch.rs` / `server.rs` (C2 is oracle-only).
- Production mutation score (baseline red under REQ-SPACE-03).
- Dictionary ranking of `変換昨日` vs `変換機能` (out of scope).

### Unmodeled / abstracted

- Named-pipe accept pool and Windows scheduling.
- Shift+Space width policy detail (only fullwidth idle slot modeled).
- History session IDs (process-wide) beyond dual-connection abstraction.
- >3 connections; unfair schedules beyond MaxEvents.

### Env / fairness

- Weak fairness only on `Commit(c)`.
- DualDelivery is an environment constant, not a fairness obligation.

### Search limits

- Finished finite spaces: MaxEvents≤6, Actors≤3, MaxSpaces≤3.
- Reach configs stop at first CE (not full exhaustion) by design.

### Surviving mutants

- Oracle campaign: **0** missed; 1 unviable excluded.

## Residual risk

1. **Release-blocking:** share-nothing workers still insert U+3000 and convert on one physical Space (history 2026-08-16 class). Fence exists only in oracle/TLA.
2. Product has no idle-space quota (BND-SPACE-01 is exploration bound).
3. Host-level dual delivery (TSF/Electron) is inventoried, not live-COM proven.

## Verdict dimensions

| Dimension | Result |
|---|---|
| Evidence completeness | Pass for declared gates (oracle, PBT, C2, mutants, pipe, TLC, map) |
| Product conformance | **Fail** — REQ-SPACE-03 / FAIL-SPACE-DUAL counterexample remains |

**NO_GO** for release of Space dual-delivery behavior.
