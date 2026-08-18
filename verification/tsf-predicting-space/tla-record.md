# TLC record — Space while suggestions are visible

Date: 2026-08-18
Specs:

- `verification/tla/TsfPredictingSpace.tla`
- `verification/tla/SpaceKeyDispatch.tla` (Predicting added)

Scripts:

- `scripts/verify-tsf-predicting-space-tlc.ps1`
- `scripts/verify-space-key-dispatch-tlc.ps1`

Jar: `C:\Users\developer\tmp\sakura-input-tla-1.7.4\tla2tools.jar`
TLC: **2.19** (rev 5a47802)
Java: Microsoft OpenJDK 11.0.31
Search: breadth-first, fingerprint index 0, seed `20260818` (TsfPredictingSpace) / `20260816` (SpaceKeyDispatch)

## TsfPredictingSpace

| Config | RetargetLive | MaxEvents | Generated | Distinct | Depth | Safety |
|---|---|---:|---:|---:|---:|---|
| fix | TRUE | 6 | 6 | 5 | 5 | pass (exit 0) |
| bug | FALSE | 6 | — | — | 6 | **NeverHostConfirmsReading violated** (exit 12) |

Bug counterexample (shortest):

1. TypeReading
2. ShowSuggestions (`suggestionsVisible = TRUE`)
3. TestKeyDown (`testEaten = TRUE`)
4. KeyDownAbsorb
5. HostConfirmReading (`hostConfirmedReading = TRUE`, `converted = FALSE`)

Correspondence:

- `RetargetLive = TRUE` ↔ TSF `live_convert_context` still finds the reading's document (composition, suggestion layout, or queued candidate payload) and `ask()` converts
- `RetargetLive = FALSE` ↔ KeyDown absorbs Space without an engine convert; Chromium confirms the underlined reading (e.g. よそく)

`TsfProbeHostInsert` is unchanged and still distinguishes Probe-timeout host insert (Issue #68) from this confirm-reading defect.

## SpaceKeyDispatch

Predicting is a first-class state. Space on Predicting is Convert, not Insert and not a return to Idle.

| Config | Exit | Notes |
|---|---:|---|
| small | 0 | PredictingSpaceDoesNotInsert / PredictingSpaceDoesNotCommitReading hold |
| unfenced | 0 | same invariants hold without the idle-space fence |
| boundary | 0 | MaxEvents 6, dual delivery, fence on |
| actors1 | 0 | single connection |
| actors3 | 0 | three connections, dual delivery |
| reach-predict | 12 | **NeverConvertedFromPredicting violated** — Type → Suggest → SpaceOn reaches `Converting` |
| reach-convert | 12 | composing Space still reaches `Converting` |
| reach-dual | 12 | unfenced dual insert∧convert still reachable as a required-state probe |
| reach-insert | 12 | idle insert still reachable |

## Bounds

These models do not rank dictionary surfaces. Absence of 低地 for ていち is outside the Space ownership contract.
