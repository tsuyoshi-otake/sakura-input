# TLC record — TsfProbeHostInsert

Date: 2026-08-18
Spec: `verification/tla/TsfProbeHostInsert.tla`
Script: `scripts/verify-tsf-probe-host-insert-tlc.ps1`
Jar: `C:\Users\developer\tmp\sakura-input-tla-1.7.4\tla2tools.jar`
TLC: **2.19** (rev 5a47802)
Java: Microsoft OpenJDK 11.0.31
Search: breadth-first, fingerprint index 0, seed `20260818`, 1 worker
Deadlock check: enabled (`Done` stutters at `Hosted` / `Realed`)

## Configurations

| Config | LocalClaim | MaxEvents | Generated | Distinct | Safety | Notes |
|---|---|---:|---:|---:|---|---|
| fix | TRUE | 4 | 5 | 4 | pass (exit 0) | TestKeyDown eats; host insert unreachable |
| bug | FALSE | 4 | 4 | 4 | **NeverHostInserts violated** (exit 12) | Probe timeout → host insert CE |

## Correspondence

- `LocalClaim = TRUE` ↔ TSF `PhysicalKeyOwner::Ime` returns eaten before `ask_probe`
- `LocalClaim = FALSE` ↔ `OnTestKeyDown` Probe timeout returns FALSE
- `hostInsertedSpace` ↔ Chromium inserting Space into a live reading
- `engineDesynchronized` ↔ `Link::desynchronized` after Probe timeout (product now keeps this false)

Existing `SpaceKeyDispatch.tla` is unchanged. It models dual-delivery idle Space, not TestKeyDown ownership.
