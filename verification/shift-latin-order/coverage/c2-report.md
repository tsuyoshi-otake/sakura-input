# Atomic-condition (C2) coverage — Shift-Latin oracle

Scope: `crates/sakura-engine/src/shift_latin_oracle.rs` predicates in `atomic_conditions`.
This is atomic-condition polarity coverage of the independent oracle, not MC/DC of the whole workspace and not line coverage claimed as C2.

| condition | false | true |
|---|---|---|
| `composing_empty` | true | true |
| `english_latched` | true | true |
| `converting` | true | true |
| `event_shifted` | true | true |
| `event_latin_letter` | true | true |
| `cursor_at_start` | true | true |
| `cursor_at_end` | true | true |
| `cursor_interior` | true | true |

Covered polarities: 16/16 (100%). Seed `0x534c4154494e0001`. Cases: 2048.

Production predicates exercised by named `shift_latin_order` tests (boolean, not llvm-cov):

| production predicate | evidence |
|---|---|
| Shift+letter starts English latch | `production_aiueo_shift_backspace_retype_keeps_press_order` |
| Shift+Backspace consumed while composing | `contract::send_key_contract_consumes_shift_backspace_during_english` |
| Backspace deletes the raw character before the caret | `production_left_then_backspace_deletes_the_character_before_the_caret` |
| Retype after end-delete is not AIUOEO | `production_aiueo_shift_backspace_retype_keeps_press_order` |
| Empty composition releases the latch | `emptying_the_buffer_releases_the_english_latch` |
| Convert then backspace then retype keeps AIUEO | `convert_then_backspace_then_retype_keeps_aiueo_press_order` |
| Resync required for dictionary convert | `resync_is_required_for_shifted_ascii_dictionary_conversion` |

Line/region coverage of production functions is **not** this table. See `llvm-cov-report.md`.
