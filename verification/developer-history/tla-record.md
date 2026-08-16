# TLC record — DeveloperHistory

Spec: `verification/tla/DeveloperHistory.tla`  
Script: `scripts/verify-developer-history-tlc.sh`  
Jar: `/tmp/sakura-tla/tla2tools.jar` (sha256 `ab323b79802aedc3203b3f9af37c6aca3ed43f4e0225b36f2aa77b26de46c05f`)  
TLC: **2026.08.11.125311** (rev 0894c34)  
Java: OpenJDK 21.0.10  
Search: breadth-first, fingerprint index `-fp 0`, seed `20260816`, workers `2`  
Deadlock check: enabled (terminal `Idle` when `eventCount = MaxEvents`)

## Environment and fairness

Documented in the spec header. One live engine; WatcherPublish is distinct
from SetDeveloperMode; DPAPI succeeds unless PersistFail. Weak fairness on
`WatcherPublish` (when pending) and `RequestBoundary` (when attach mismatches
published). Liveness consequents include `eventCount = MaxEvents` so the
exploration budget is not mistaken for a fairness violation.

## Configurations

| Config | MaxRecords | MaxEvents | MaxEpoch | QueueCap | Generated | Distinct | Diameter | Queue left | Safety | Liveness | Deadlock | Notes |
|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| `DeveloperHistory-small.cfg` | 2 | 8 | 2 | 2 | 104,163 | 18,837 | 9 | 0 | pass | pass | none | `tlc/DeveloperHistory-small/stdout.log` |
| `DeveloperHistory-boundary.cfg` | 3 | 10 | 2 | 1 | 549,716 | 92,067 | 11 | 0 | pass | pass | none | QueueCap 1 |
| `DeveloperHistory-concurrent.cfg` | 3 | 12 | 3 | 2 | 2,370,365 | 368,986 | 13 | 0 | pass | pass | none | larger event budget |
| `DeveloperHistory-crash.cfg` | 2 | 10 | 2 | 2 | 529,708 | 87,542 | 11 | 0 | pass | pass | none | Crash/Restart in Next |

Fingerprint collision estimates (TLC reported “two distinct states had the same fingerprint” as an optimistic probability note; no error):

| Config | reported optimistic |
|---|---|
| small | present in log |
| boundary | ~1.7e-10 |
| concurrent | ~3.0e-8 |
| crash | ~1.7e-10 |

## Reachability / forbidden

- Required: hot enable (Set → Publish → RequestBoundary → attached), hot disable, Normal durable key while attached.
- Forbidden: live stale-inactive (`publishedOn ∧ requestAfterPublish ∧ ¬serviceAttached`) — Safety `ForbiddenStaleInactive`.

## Unexplored (not claimed)

Real HWND / installed IME keys, Windows DPAPI OS failures, COM re-entrancy,
multi-engine `already_running` races beyond BootNoop, unfair scheduling,
and TLC state counts above these constant bounds.
