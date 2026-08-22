# TLC record — History suggestion Space conversion

Date: 2026-08-18
Spec: `verification/tla/HistorySuggestionConvert.tla`
Script: `scripts/verify-history-suggestion-convert-tlc.ps1`

Jar: `C:\Users\developer\tmp\sakura-input-tla-1.7.4\tla2tools.jar`
TLC: **2.19** (rev 5a47802)
Java: Microsoft OpenJDK 11.0.31
Search: breadth-first, fingerprint index 0, seed `20260818`, 1 worker

This spec is independent of `DualTsfCandidateBoard.tla` (shared popup Hide)
and `TsfPredictingSpace.tla` (TestKeyDown retarget). It models Space while a
履歴 list is visible, including an identity pair (`reading = surface`).

## Constants

| Constant | Meaning |
|---|---|
| `PreferIdentity` | TRUE = conversion ranking may select にほんご→にほんご; FALSE = identity is ignored |
| `GuardForeignEnd` | TRUE = idle Dual TSF Hide does not end a sibling list |
| `MaxEvents` | logical event bound (6) |

Product correspondence:

- `crates/sakura-engine/src/learning.rs` (`best_candidate` skips `surface == reading`)
- `crates/sakura-tsf/src/conversion_key.rs` (`ends_shared_candidate_ui`)

## Results

| Config | PreferIdentity | GuardForeignEnd | Generated | Distinct | Depth | Expected |
|---|---:|---:|---:|---:|---:|---|
| fix | FALSE | TRUE | 20 | 11 | (queue 0) | pass (exit 0) |
| bug | TRUE | FALSE | 8 | 8 | CE | **NeverHostConfirms** (exit 12) |
| reach-identity | TRUE | FALSE | 5 | 5 | CE | **NeverIdentityTop** (exit 12) |

All three searches matched the expected exit codes. The fix configuration left
zero states on the queue. The bug configuration's CE is HostConfirm after Space
selects the identity 履歴 surface, which is the developer-log path
`にほんご` committed as `にほんご`.
