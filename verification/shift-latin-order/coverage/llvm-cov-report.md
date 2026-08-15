# llvm-cov line/region coverage — Shift-Latin production functions

Tool: `cargo-llvm-cov 0.8.7` + `llvm-tools-preview`.
Command: `rtk cargo llvm-cov -p sakura-engine --lib --json --output-path verification/shift-latin-order/coverage/llvm-cov.json -- shift_latin`.
Filter: `shift_latin` (45 tests after the coverage-neighbor pass). This is **line/region coverage**, not C2 and not MC/DC.

True C2 / MC/DC of these functions is still impossible from this artifact:
- `mcdc_records` on the live functions: {'feed_character': 0, 'render_preedit': 0, 'apply_backspace': 0, 'resync_shifted_ascii_from_raw': 0}.
- Keymap `shift+backspace` lives in TOML (`data/keymap-ms-ime.toml`, `data/keymap-atok.toml`) and has no LLVM counters; it is covered by `contract::keymap_contract_shift_backspace_is_delete_back_while_composing`.

## Whole-function region coverage

These percentages stay low because each function also owns kana / pending-romaji / CJK normalize arms. That is not a silent hole: see Out of scope below.

| function | executions | covered regions | total regions | region % |
|---|---:|---:|---:|---:|
| `feed_character` | 3487 | 126 | 142 | 88.7% |
| `apply_backspace` | 305 | 20 | 80 | 25.0% |
| `render_preedit` | 5944 | 77 | 172 | 44.8% |
| `resync_shifted_ascii_from_raw` | 3951 | 33 | 34 | 97.1% |

## Shift-Latin-relevant arm region coverage

Regions whose source span overlaps the Shift-Latin early-return / latch / raw-caret arms.

| function | arm lines | covered | total | arm region % |
|---|---|---:|---:|---:|
| `feed_character` | 2278–2326 | 64 | 65 | 98.5% |
| `apply_backspace` | 3502–3512 | 18 | 20 | 90.0% |
| `render_preedit` | 4165–4193 | 37 | 49 | 75.5% |
| `resync_shifted_ascii_from_raw` | 3476–3495 | 33 | 34 | 97.1% |

## Out of scope (kana / CJK / pending romaji)

These line ranges share the same functions but are not Shift-Latin branches. They are listed so the low whole-function percentages are not a silent hole.

| function | out-of-scope lines | why |
|---|---|---|
| `feed_character` | 2328–2368 | romaji `table.feed`, decimal-after-digit, kana insert |
| `apply_backspace` | 3514–3565 | pending-romaji / kana-group delete |
| `render_preedit` | 4195–4240 | kana pending + `normalizer.normalize_into` |
| `resync_shifted_ascii_from_raw` | — | entire function is Shift-Latin |

Whole-file `dispatch.rs` under the same filter (includes unrelated functions):
- lines 984/15361 (6.41%)
- regions 1355/24072 (5.63%)
- functions 55/537 (10.24%)

Remaining uncovered Shift-Latin-arm micro-regions (not silent):
- `feed_character` 2316:66 and `apply_backspace` 3510–3511 / `render_preedit` 4188:73 are `unwrap_or(u16::MAX)` / `if let None` overflow edges.
- `render_preedit` 4172–4182 is the conversion-service-dies-after-begin fallback. `begin_conversion` fail-closed beep is a different site (3310). Not constructed here.

Zero-count duplicate symbols for the same names exist in other codegen units and were ignored.
