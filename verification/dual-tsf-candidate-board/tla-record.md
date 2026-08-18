# TLC record — Dual TSF shared candidate board

Date: 2026-08-18 (constants split)
Spec: `verification/tla/DualTsfCandidateBoard.tla`
Script: `scripts/verify-dual-tsf-candidate-board-tlc.ps1`

Jar: `C:\Users\developer\tmp\sakura-input-tla-1.7.4\tla2tools.jar`
TLC: **2.19** (rev 5a47802)
Java: Microsoft OpenJDK 11.0.31
Search: breadth-first, fingerprint index 0, seed `20260818`, 1 worker

Public `origin/main` is still 1.0.12 (`1af807b16dc6c5dd174718c6ed23ce5f1eb89cdf`). This spec lives in the local dirty worktree only.

## Constants

| Constant | 1.0.15 product | Meaning |
|---|---|---|
| `IgnoreForeignEmpty` | implemented | idle empty `publish_output` does not clear `UiBoard` |
| `GuardForeignCandidateEnd` | **not implemented** | TSF `candidates=None` must not `Hide`/`End` a live `CandidateUi` / `UiLease` |
| `RestoreCurrentPlacement` | **not implemented; NO-GO as `renderer_visible=true`** | republish current live placement after convert, only when lease/host/layout allow |

## Results

| Config | IgnoreForeignEmpty | GuardForeignCandidateEnd | RestoreCurrentPlacement | Generated | Distinct | Depth | Safety |
|---|---:|---:|---:|---:|---:|---:|---|
| legacy-bug | FALSE | FALSE | FALSE | 4 | 4 | 4 | **NeverPeerClearsSuggestedList** (exit 12) |
| shipped-1.0.15 | TRUE | FALSE | FALSE | 5 | 5 | 4 | **NeverForeignEndHidesLive** (exit 12) |
| reach-hide | TRUE | FALSE | FALSE | 12 | 10 | 5 | **NeverInvisibleConversion** (exit 12) |
| end-guard-only | TRUE | TRUE | FALSE | 27 | 11 | 7 | pass (exit 0) |
| proposed-fix | TRUE | TRUE | TRUE | 27 | 11 | 7 | pass (exit 0) |

end-guard-only and proposed-fix have the same generated/distinct/depth. With `GuardForeignCandidateEnd`, `visible` never falls before convert, so `RestoreCurrentPlacement` is vacuous in this model. **This model does not justify writing `renderer_visible = true` in `publish_output`.**

## Required traces

legacy-bug / `NeverPeerClearsSuggestedList`:

1. TypeReading
2. ShowSuggestions
3. PeerSpaceEmpty (`kind = "None"`, `visible = FALSE`)

shipped-1.0.15 / `NeverForeignEndHidesLive`:

1. TypeReading
2. ShowSuggestions (`kind = "Suggestion"`, `visible = TRUE`, `owner = "Live"`)
3. PeerCandidateEnd (`kind` stays Suggestion, `visible = FALSE`)

Correspondence: `submit_output` maps `output.candidates = None` to `CandidateEffect::Hide`, then `clear_ui_lease` + `queue_end_candidates`. Engine `UiBoard` may still hold the live list (1.0.15 skip).

reach-hide / `NeverInvisibleConversion` (same constants as shipped):

1. TypeReading
2. ShowSuggestions
3. PeerCandidateEnd (`visible = FALSE`)
4. LiveSpaceConvert (`kind = "Conversion"`, `visible = FALSE`)

## Runtime provenance (this host, 2026-08-18 14:25 JST)

Cursor PID 41864 started 13:22:11, after engine/renderer 13:21:29.

| Process | PID | Path | SHA-256 |
|---|---:|---|---|
| sakura_engine.exe | 30444 | `...\versions\1.0.15-eaf2a60b1b7703ea\sakura_engine.exe` | `f986d0930c792ac851d15d28f5d8b4bf8f92e459d68bc5e2f86626b7a1ffc722` |
| sakura_renderer.exe | 34640 | `...\versions\1.0.15-eaf2a60b1b7703ea\sakura_renderer.exe` | `203d5963931458bd6fc150a90b6f49c72bb24cd1685749170ed2ac8bf396b981` |
| sakura_tsf.dll (Cursor 41864) | — | `...\versions\1.0.15-eaf2a60b1b7703ea\sakura_tsf.dll` | `68a59a7b3ee867d6bdc197b77ae1a717a78165166897e48ab682fbba7a23c4e4` |

No engine/renderer process is running from an older version directory. Explorer/Chrome/Teams still load 1.0.13 or 1.0.14 TSF; that does not drive this Cursor process.

Installer build id `1.0.15-eaf2a60b1b7703ea` is not a git commit. Git HEAD remains 1.0.12.

**Old-engine confound for this Cursor session is closed.** The remaining 1.0.15 failure, if still reproduced here, is not “Cursor restarted but engine stayed on 1.0.14”.

## Still unproven

- Exact runtime writer of the first `visible=true` → `false` on one physical Space (method, owner, revision, reason)
- Whether Cursor’s idle peer shares the live `CandidateUi` (same TextService) or is a second TextService whose `Hide` cannot end the live element
- Delayed `SetUiPlacement(anchor=None)` after layout lease rollover

If a later causal trace shows a writer that is not `PeerCandidateEnd`, this shipped CE is the wrong explanation and must not be implemented.
