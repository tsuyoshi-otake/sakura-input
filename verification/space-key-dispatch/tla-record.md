# TLC record — SpaceKeyDispatch (assurance gate re-run)

Date: 2026-08-16
Spec: `verification/tla/SpaceKeyDispatch.tla`
Spec SHA-256: `31dd13cc39f5e34877ebc62bd4c97ba6f74297e0dceacea2040bcfd4b9fef3fe`
Script: `scripts/verify-space-key-dispatch-tlc.ps1`
Jar: `C:\Users\developer\tmp\sakura-input-tla-1.7.4\tla2tools.jar`
Jar SHA-256: `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88` (2,274,532 bytes)
TLC: **2.19** (rev 5a47802, 08 August 2024)
Java: Microsoft OpenJDK 11.0.31
Search: breadth-first, fingerprint index 0, seed `20260816`, 2 workers
Deadlock check: enabled (terminal `Idle` when `eventCount = MaxEvents`)

## Environment and fairness

Documented in the spec header.

- Actors: 1..3 engine connections for one host process.
- `FenceIdleSpace` models process-wide idle-Space absorb (not in product).
- `DualDelivery` models Electron/TSF delivering one WM_KEYDOWN to every live connection.
- Logical time is `eventCount`. No wall clock.
- Weak fairness on `Commit(c)` for `ConversionEventuallyTerminates`.

## Configurations (exhausted unless reachability CE)

| Config | Fence | Dual | Actors | MaxEvents | MaxSpaces | Generated | Distinct | Diameter | Queue | Safety | Liveness | Notes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|
| small | TRUE | TRUE | 2 | 5 | 3 | 8202 | 1761 | 6 | 0 | pass | pass | fence holds under dual |
| unfenced | FALSE | FALSE | 2 | 5 | 3 | 6841 | 1598 | 6 | 0 | pass | pass | focused delivery only |
| boundary | TRUE | TRUE | 2 | 6 | 3 | 19633 | 3727 | 7 | 0 | pass | pass | higher MaxEvents |
| actors1 | TRUE | FALSE | 1 | 5 | 3 | 1707 | 611 | 6 | 0 | pass | pass | single actor |
| actors3 | TRUE | TRUE | 3 | 4 | 2 | 6574 | 1381 | 5 | 0 | pass | pass | three actors |
| reach-dual | FALSE | TRUE | 2 | 5 | 3 | 53 | 38 | CE | n/a | **NeverDualEffect violated** | n/a | required dual-effect state |
| reach-convert | TRUE | FALSE | 1 | 4 | 3 | 26 | 26 | CE | n/a | **NeverConverts violated** | n/a | required convert state |
| reach-insert | TRUE | FALSE | 1 | 3 | 3 | 7 | 7 | CE | n/a | **NeverInserts violated** | n/a | required insert state |

Exit codes: safety configs **0**; reachability configs **12** (expected CE).

## Reachability

- Required dual effect under unfenced DualDelivery: reached (Type then DualSpace).
- Required convert / insert states: reached.
- Forbidden dual effect under fence: not reached (`NoDualEffect` held).

## Unexplored

COM re-entrancy, TSF write-journal epochs, dictionary ranking (`変換昨日`),
Shift+Space width, >3 connections, unfair schedules beyond MaxEvents,
Windows named-pipe accept-pool limits. The fence is **not** implemented in
share-nothing engine workers.
