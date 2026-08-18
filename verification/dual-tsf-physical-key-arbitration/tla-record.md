# TLC record — Dual TSF physical conversion-key arbitration

Date: 2026-08-18
Spec: `verification/tla/DualTsfPhysicalKeyArbitration.tla`
Script: `scripts/verify-dual-tsf-physical-key-arbitration-tlc.ps1`

Jar: `C:\Users\developer\tmp\sakura-input-tla-1.7.4\tla2tools.jar`
TLC: **2.19** (rev 5a47802)
Java: Microsoft OpenJDK 11.0.31
Search: breadth-first, fingerprint index 0, seed `20260818`, 1 worker

This spec is independent of `SpaceKeyDispatch.tla` (engine dual delivery) and
`DualTsfCandidateBoard.tla` (shared candidate UI). It models only TSF
process-local disposition of one physical Space across two TextService actors.

## Constants

| Constant | Meaning |
|---|---|
| `ThreeState` | FALSE = two-valued local `PhysicalKeyOwner`; TRUE = `HostEligible` / `ApplyLocal` / `AbsorbPeer` |
| `PeerFirst` / `OwnerFirst` | delivery order; both FALSE = either order |
| `ProbeCanFail` | `HostEligible` TestKeyDown is uneaten (Probe Unavailable / idle insert) |
| `MaxEvents` | logical event bound (8) |
| `MaxGen` | claim generation bound (2) |

Product correspondence: `crates/sakura-tsf/src/conversion_key.rs`.

## Results

| Config | ThreeState | Order | ProbeCanFail | Generated | Distinct | Depth | Expected |
|---|---:|---|---:|---:|---:|---:|---|
| bug-2state | FALSE | either | TRUE | 9 | 6 | 3 | **NoHostInsertWhileLive** (exit 12) |
| fix-3state | TRUE | either | TRUE | 283 | 87 | 9 | pass (exit 0) |
| fix-peer-first | TRUE | B then A | TRUE | 251 | 87 | 9 | pass (exit 0) |
| fix-owner-first | TRUE | A then B | TRUE | 251 | 87 | 9 | pass (exit 0) |
| fix-probe-failure | TRUE | B then A | TRUE | 251 | 87 | 9 | pass (exit 0) |
| fix-owner-teardown | TRUE | either | TRUE | 283 | 87 | 9 | pass (exit 0) |
| fix-context-replace | TRUE | either | TRUE | 283 | 87 | 9 | pass (exit 0) |
| reach-host-space | TRUE | either | TRUE | 3 | 3 | 2 | **NeverHostInserts** (exit 12) |

fix-3state, fix-owner-teardown, and fix-context-replace share constants; they
are named for the requirement they document. All three include `Teardown` and
`ReplaceContextA`. Peer-first and owner-first omit the unused delivery action,
so generated count is 251 instead of 283. Distinct states stay 87.

## Required traces

bug-2state / `NoHostInsertWhileLive`:

1. TypeA (`liveOwner = "A"`, `uiOwner = "A"`)
2. DeliverSpace (`engineAppliedThisKey = TRUE`, `appliedBy = "A"`, `hostInsertedThisKey = TRUE`)

Idle B is `HostEligible` and uneaten while A converts. Chromium may insert a
document space into the live reading. Correspondence: 1.0.15
`PhysicalKeyOwner::of` sees only `has_live_composition()` on that instance.

reach-host-space / `NeverHostInserts`:

1. DeliverSpace from Init (`liveOwner = "None"`, `hostInsertedThisKey = TRUE`)

Idle Space reaches the host when no sibling owns a reading. The three-state
fix must not swallow that Space.

## Bounds and unexplored space

The model abstracts COM, renderer, dictionary, 50 ms Probe, and
`CandidateEffect` Keep/Show/End. It checks two actors, generation two, and
eight events. It does not prove Cursor's exact TestKeyDown interleaving beyond
the two delivery orders above.
