# Judge v1 rubric

This rubric is identity for Judge v1. Changing it creates a new Judge version
and requires a Calibration Set rerun.

## Verdict

- `A` / `B`: the named anonymous system is preferable for a Japanese user.
- `tie`: no material user-visible difference.
- `ungradable`: the supplied context is insufficient.

Do not invent a preference to avoid `tie` or `ungradable`.

## Severity (of the worse system relative to the better one)

If the verdict is `tie` or `ungradable`, severity must be `0`.

| Value | Meaning |
|------:|---------|
| 0 | equivalent |
| 1 | cosmetic or extremely minor ranking difference |
| 2 | noticeable quality degradation, easily recoverable |
| 3 | serious incorrect conversion or significant correction burden |
| 4 | destructive or clearly unacceptable corruption of user intent |

Severity 4 examples: `ESP32` → `ESP3㊁`; unrelated lexical conversion that a
user could commit; destruction of a literal identifier.

## Reason codes

Use only schema enum values. Prefer the smallest set that explains the verdict.

## What this Judge must not decide

Composition lifecycle, key routing, timeouts, candidate UI ownership, IPC,
process provenance, and TLA+ properties are out of scope. Those belong to the
deterministic oracle.
