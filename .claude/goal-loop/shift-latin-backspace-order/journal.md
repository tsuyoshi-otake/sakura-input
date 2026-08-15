# Journal — shift-latin-backspace-order

## 2026-08-15 shift-latin-backspace-order

### Work
- Oracle in `crates/sakura-engine/src/shift_latin_oracle.rs` (no dispatch/session import).
- Production: Shift-Latin edits use the raw caret; `shift+backspace` is bound.
- PBT seed 6002245328167110677, no counterexample.
- C2 report 16/16 polarities.
- Manual mutation 5 killed / 1 equivalent / 1 campaign survivor.
- TLC 2.19 small+boundary pass; reach-aiueo proves AIUEO reachable.

### Fail / investigate / verify
Verifier [C6 fail](566c544b-f64f-4c36-8285-1fbd325f8665): filter
`shift_latin_order::contract` ran 0 tests because the module was named
`shift_latin_order_tests`. Confirmed with the verifier evidence
(`0 passed, 309 filtered`). Renamed the test module to `shift_latin_order`.

Also: `forward_deleting_the_english_composition_away_ends_the_temporary_mode`
failed after the raw-caret fix (`raw="K"`). Diagnosis: Left+one Delete now
removes one visible letter, not the whole kana group. Confirmed by reading
the assertion and the new `apply_delete_forward` shifted-ascii arm. Updated
the test to Home + two Deletes.

Verifier iteration 2 ([pass](72deeb37-c58e-4179-a2f7-4e80c4e8bfe2)): overall=pass, C1–C8 pass.
`shift_latin_order::contract` now runs 10 tests. `sakura-engine --lib`:
307 passed / 2 ignored.

## 2026-08-15 residual-risk close-out

### Work
- TSF: fake-pipe `Engine::send_key` Shift+Latin + Shift+Backspace + retype;
  write-journal AIUEO→AIUE→AIUEO and ProjectionMismatch on stolen AIUOEO.
- Convert-cancel-then-edit PBT seed 6002246440563640341; named AIUEO/CLAUDE
  neighbors; `resync_is_required_for_shifted_ascii_dictionary_conversion`.
- TLC medium 6/8, events 5/9, large 6/10 all finished (large: 23,139,035 /
  6,341,996 / diameter 11, 4 workers, 4m 43s).
- cargo-mutants 27.1.0 scoped to dispatch.rs: S1 caught; Shift-Latin-arm
  19/19 after unshifted-letter latch test.
- cargo-llvm-cov 0.8.7 JSON + `llvm-cov-report.md` (not MC/DC).
- Rubric extended C9–C13. Release decision moved Conditional go → Go.

### Fail / investigate / verify
cargo-mutants `--re` with `|` is a PowerShell pipe; used a TOML config.
`examine_re` still admitted `from struct` field-deletes. Classified as
filter leakage, not Shift-Latin survivors. Confirmed S1 in `caught.txt`.
Unshifted-letter production test killed the four `starts_shifted_ascii`
`&&`→`||` mutants on `--iterate`.

Verifier iteration 3 ([pass](1f8cd395-4e90-4225-b3b6-ebc7524895f8)): overall=pass, C1–C13 pass.

## 2026-08-15 coverage-and-hwnd tighten-up

### Work
- Named + PBT-neighbor tests for Home/BS-at-0, mid-insert, Delete, digit/
  punctuation, full erase, convert-cancel+Home, non-ASCII English exit,
  convert-without-dictionary. Coverage PBT seed 6002246410498869269.
- llvm-cov re-run (45 `shift_latin` tests). Whole-function: feed 42.3→88.7,
  backspace 25.0→25.0, render 22.1→44.8. Shift-Latin arms: 98.5 / 90.0 /
  75.5 / 97.1. Kana ranges documented as out of scope. Still not MC/DC.
- Next HWND layer: `plan_from_visible` AIUEO→AIUE→AIUEO chain, plus a
  process-local EDIT HWND through `checked_host_call`. `e2e-host` / installer
  not run. Irreducible is now live `ITfContext` + installed-IME profile only.
- Rubric C14–C15 added. Canvas and audit updated with the new numbers.

### Fail / investigate / verify
First engine run: Escape after Convert cleared the buffer (oracle only
cancels conversion). Replaced with converting Backspace, matching the
existing convert-cancel contract. Coverage PBT Space-after-punctuation
inserted a literal U+0020; Convert was removed from that alphabet (convert
already has its own PBT).

Verifier iteration 4 ([pass](a20d5fd2-a89c-40a5-99ab-688625ac29a6)): overall=pass, C1–C15 pass.
