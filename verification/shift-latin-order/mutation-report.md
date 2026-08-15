# Mutation report — Shift-Latin order

Tool: `cargo-mutants 27.1.0` (installed this run).
Scope: `crates/sakura-engine/src/dispatch.rs` with
`verification/shift-latin-order/cargo-mutants.toml`
(`examine_re` = feed_character / apply_backspace / render_preedit /
resync_shifted). Not the whole workspace.
Test filter: `cargo test -p sakura-engine --lib -- shift_latin`.
Output: `verification/shift-latin-order/mutants-out/mutants.out/`.

A first pass tested 59 generated mutants (5m, 13 caught / 46 missed).
`--iterate` after adding `production_unshifted_letter_does_not_start_english`
retested the 46 missed and caught 8 more. Combined: **21 caught / 38 missed /
0 timeout**.

The examine regex also admitted 24 out-of-function field-delete mutants
(`Preferences`, `ConversionOptions`, `ConversionSegment`) because those
names contain `from`. They are listed below as filter leakage, not as
Shift-Latin survivors.

## Score

| | count |
|---|---:|
| Generated (dispatch.rs, this config) | 59 |
| Caught by `shift_latin` | 21 |
| Missed | 38 |
| Timeout | 0 |
| Filter leakage (Preferences / Conversion* field deletes) | 24 |
| Kana/decimal-path survivors (after the shifted-ascii early return) | 14 |
| Shift-Latin-arm mutants (latch, raw caret, resync, English render) | 19 |
| Shift-Latin-arm caught | 19 |
| Shift-Latin-arm score | 19/19 = 100% |
| In-function score including kana arms | 21/35 = 60.0% |
| Raw score (caught / generated) | 21/59 = 35.6% |

## S1 — `resync_shifted_ascii_from_raw` becomes `Ok(())`

**Caught** by the `shift_latin` suite:

`crates/sakura-engine/src/dispatch.rs:3481:5: replace resync_shifted_ascii_from_raw -> Result<(), Overflow> with Ok(())`

Killer: `resync_is_required_for_shifted_ascii_dictionary_conversion`
(Space after `CLAUDE` must convert to `Claude`; a no-op resync leaves
`preedit` empty and `begin_conversion` beeps).

Equivalent? **No.**

## S2 — `saturating_sub` vs `-`

cargo-mutants 27.1.0 did not emit that equivalent. It did emit
`cursor - 1` → `+` / `/` on the shifted-ascii Backspace arm (line 3507);
both were **caught**.

## Caught (combined)

First pass:

- `feed_character` → `Ok(())`
- `shifted_ascii && !character.is_ascii()` polarity (2295)
- `apply_backspace` → `Ok(())`
- shifted-ascii `cursor == 0` and `cursor - 1`
- `resync_shifted_ascii_from_raw` → `Ok(())` (S1)
- `render_preedit` → `Ok(())` and `!is_composing` / empty-raw guards

Iterate (new unshifted-letter test):

- `starts_shifted_ascii` `&&` → `||` (2279–2282)
- `shifted_ascii && character.is_ascii()` → `||` (2305)
- one later `feed_character` `!` delete (2352)
- English-render `&&` → `||` (4186)
- one kana-render `!` delete (4201)

## Missed, classified

### Filter leakage (24) — not Shift-Latin

Deleting unused `Preferences` / `ConversionOptions` fields in
`create_session` / `probe_key` / `conversion_options`. The `shift_latin`
filter never reads those fields. Equivalent relative to this campaign.

### Kana / decimal path (14) — out of this campaign

`apply_backspace` after the `shifted_ascii` early return, `feed_character`
period-after-digit, and kana `render_preedit` pending/cursor arms. Visible
English text never takes those arms. Not equivalent in the romaji product;
not a residual Shift-Latin order risk.

## Defect-detection power

The campaign now kills the operators that recreate AIUOEO (end-pop vs caret
delete, append vs insert, hidden caret) **and** the S1 resync no-op that
used to survive this filter. Keep the named conversion test in the
`shift_latin` suite so a future filter-only run cannot ship a provenance
regression.
