# TLC record — ShiftLatinInput

Spec: `verification/tla/ShiftLatinInput.tla`
Script: `scripts/verify-shift-latin-tlc.ps1`
Jar: `C:\Users\developer\tmp\sakura-input-tla-1.7.4\tla2tools.jar`
TLC: **2.19** (rev 5a47802, 08 August 2024)
Java: Microsoft OpenJDK 11.0.31
Search: breadth-first, fingerprint index 0, seed `20260815`
Deadlock check: enabled (terminal `Idle` action when `eventCount = MaxEvents`)

Passing configs below used `-workers 4` except the previously recorded small/boundary/reach-aiueo rows, which remain the 1-worker 2026-08-15 morning runs.

## Environment and fairness

Documented in the spec header. One user; totally ordered keys; host does
not edit a consumed key (`hostStolen` stays FALSE). Weak fairness on
`Backspace` and `Commit` for `BufferEventuallyClearable`.

## Configurations

| Config | Letters | MaxLen | MaxEvents | Workers | Generated | Distinct | Diameter | Queue left | Safety | Liveness | Deadlock | Notes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| `ShiftLatinInput-small.cfg` | 5 | 5 | 8 | 1 | 904,088 | 246,917 | 9 | 0 | pass | pass | none | logs in `tlc/ShiftLatinInput-small/stdout.log` |
| `ShiftLatinInput-boundary.cfg` | 5 | 4 | 7 | 1 | 146,552 | 40,565 | 8 | 0 | pass | pass | none | shorter buffer bound |
| `ShiftLatinInput-reach-aiueo.cfg` | 5 | 5 | 8 | 1 | (counterexample) | — | 7 | — | **NeverAiueo violated** | n/a | n/a | required-state proof: AIUEO is reachable |
| `ShiftLatinInput-medium.cfg` | 5 | 6 | 8 | 4 | 1,216,588 | 340,667 | 9 | 0 | pass | pass | none | increased MaxLen only; 17s |
| `ShiftLatinInput-events.cfg` | 5 | 5 | 9 | 4 | 3,808,999 | 1,048,519 | 10 | 0 | pass | pass | none | increased MaxEvents only; 50s |
| `ShiftLatinInput-large.cfg` | 5 | 6 | 10 | 4 | 23,139,035 | 6,341,996 | 11 | 0 | pass | pass | none | previously timed out at 120s/1 worker; finished in 4m 43s |

Fingerprint collision estimates:

| Config | optimistic | actual fingerprints |
|---|---|---|
| small | ~1e-8 | (prior record) |
| medium | 1.6e-8 | 5.6e-9 |
| events | 1.6e-7 | 6.6e-9 |
| large | 5.8e-6 | 9.4e-8 |

## Reachability

- Required: `composing = <<A,I,U,E,O>>` — reached (trace in
  `tlc/ShiftLatinInput-reach-aiueo/stdout.log`, ShiftDown then five
  TypeLetter actions).
- Forbidden: `hostStolen = TRUE` — not reached under any passing config
  (`NoHostSteal` held on every distinct state, including the 6,341,996
  large-config states).

## Unexplored scope

TSF write-journal epochs, COM re-entrancy, key repeat, Unicode beyond the
five model letters, concurrent host edits, unfair schedules, MaxLen > 6
or MaxEvents > 10, and more than one typist. Those bounds are now
explicitly larger than the previous unfinished 6/10 attempt; the 6/10
config itself is no longer unexplored.
