# Correspondence table and adversarial re-audit

Date: 2026-08-15 (coverage / HWND tighten-up). Workdir: `C:\Codes\tsuyoshi-otake\sakura-input`.

## Correspondence

| Requirement / bound / failure | Oracle clause | Example test | PBT | TLA Action/Property | Implementation | Evidence |
|---|---|---|---|---|---|---|
| Shift held types Latin in press order | `apply_latin` + latch | `reported_aiueo_repair_keeps_press_order` | `shift_latin_order_pbt_matches_oracle_and_persists_seed` | `TypeLetter` | `feed_character` shifted-ascii arm | `crates/sakura-engine/src/shift_latin_oracle.rs`, `dispatch.rs` ~2305 |
| First Shift+letter latches English | `starts_english` | `latch_keeps_following_unshifted_ascii_in_press_order` | PBT emits unshifted Latin only while latched | `TypeLetter` sets `latched'` | `starts_shifted_ascii` | oracle + `dispatch.rs` 2278; `production_unshifted_letter_does_not_start_english` |
| Backspace deletes the char before the caret, Shift or not | `DomainEvent::Backspace` | `production_aiueo_shift_backspace_retype_keeps_press_order` | PBT includes both Backspace polarities | `Backspace` | `apply_backspace` + keymap `shift+backspace` | `data/keymap-ms-ime.toml`, `keymap-atok.toml` |
| AIUEO then BS then O is AIUEO, not AIUOEO | visible = reconstruction | same + `assert_ne!(..., "AIUOEO")` | no shrink found | `NeverAiueo` reachability probe | raw caret insert/delete | `pbt-shrunk-counterexample.md` |
| Left then BS deletes the letter before the (now visible) caret | cursor model | `production_left_then_backspace_...` | PBT includes Left | `MoveLeft` then `Backspace` | `move_caret` + render cursor | `production_left_moves_the_visible_raw_caret` |
| Empty buffer releases latch | `release_latch_if_empty` | `emptying_the_buffer_releases_the_english_latch` | PBT | `EmptyReleasesLatch` | `release_shifted_ascii_without_composition` | existing #51 + new oracle |
| Convert then BS then retype keeps Latin order | converting cancel | `production_convert_then_backspace_then_retype_keeps_aiueo_press_order` | `shift_latin_convert_cancel_then_edit_pbt_...` | `Convert` then `Backspace` | keymap converting `shift+backspace=cancel` + resync | `pbt-convert-seed.txt` `6002246440563640341` |
| Resync rebuilds conversion provenance | n/a (reading is raw) | `resync_is_required_for_shifted_ascii_dictionary_conversion` | convert PBT types CLAUDE/OPENAI | n/a | `resync_shifted_ascii_from_raw` | cargo-mutants caught S1 |
| Max buffer | insert fails closed | `resource_exhaustion_...` | n/a | `Len(composing) < MaxLen` | `FixedStr` overflow | contract test |
| Drop / reorder / cancel / restart | event semantics | `contract::*` | n/a | single-actor Next | `SendKey` | `boundary-inventory.md` |
| Host must not steal Shift+Backspace | consumed=true | `send_key_contract_consumes_shift_backspace_...` plus TSF fake-pipe + journal | n/a | `NoHostSteal` | keymap exact Shift match | TLC large 6,341,996 states; TSF tests |
| TSF write applies engine plans in order | journal `before` must match tail | `shift_latin_backspace_retype_plans_commit_in_order_and_never_aiuoeo` | n/a | abstracted | `WriteCoordinator::attach` | host-stolen AIUOEO → `ProjectionMismatch` |
| Home / Delete / mid-insert / digit keep press order | caret + insert_at | `production_home_then_*`, `production_delete_*`, `production_digit_and_punctuation_*` | `shift_latin_coverage_neighbor_pbt_*` seed `6002246410498869269` | `MoveHome` / `Delete` / `TypeLetter` | raw caret in `feed_character` / `apply_backspace` / `apply_delete_forward` | `pbt-coverage-seed.txt` |
| Non-ASCII exits English without reordering | n/a (oracle refuses non-ASCII) | `production_non_ascii_exits_english_without_reordering_the_latin_prefix` | n/a | unmodeled | `feed_character` 2295–2303 | llvm-cov arm 64/65 |
| Document projection of Show(AIUEO→AIUE→AIUEO) | n/a | `shift_latin_backspace_retype_plans_chain_to_aiueo_not_aiuoeo` + process-local EDIT HWND | n/a | abstracted | `plan_from_visible` + `checked_host_call` + `SetWindowTextW` | composition HWND test |

## Adversarial re-audit

### Unverified

- A physical HWND *plus the installed IME language profile* with a real Shift-held Backspace (VS Code, Notepad, or `sakura_tsf_test_host` under `e2e-host`). That binary uses the active Windows profile; running it would exercise the user's installed IME. Installer/reinstall remains forbidden.
- A live COM `ITfContext` / `ITfRange`. This crate cannot construct those without a host (`text_service.rs` recovery test documents the same bound).
- True C2 / MC/DC of `dispatch.rs`. `mcdc_records` on the live functions are all 0. See `coverage/llvm-cov-report.md`.
- Full `cargo test --workspace`.
- ATOK/MS-IME predicting Shift+Left on a live host.
- `render_preedit` 4172–4182 (conversion service dies *after* `begin_conversion`). `begin_conversion`'s own fail-closed beep is a different site (3310). The 1-column uncovered arm regions are `unwrap_or(u16::MAX)` / `if let None` overflow edges.

### Unmodeled

- Romaji of *unlatched* ASCII (oracle refuses those keys; production test now asserts they do not latch).
- TSF journal epochs beyond the new unit test, COM re-entrancy, key repeat.
- Dictionary conversion *surfaces* remain outside the TLA spec; they are inside the new convert PBT and the S1 killer.

### Abstractions

- Five model letters `{A,I,U,E,O}`.
- One actor, totally ordered keys.
- `hostStolen` is a ghost that no action sets; the new journal test is the implementation-side check that a stolen AIUOEO plan cannot attach.

### Env / fairness

- Weak fairness on Backspace and Commit only.
- Unfair schedules and real-time duration are unexplored.

### Search limits

- Finished configs now include MaxLen 6 / MaxEvents 10 (23,139,035 generated / 6,341,996 distinct / diameter 11, 4 workers, seed 20260815, 4m 43s).
- Also finished: MaxLen 6 / MaxEvents 8 and MaxLen 5 / MaxEvents 9.
- Unexplored: MaxLen > 6 or MaxEvents > 10, unfair scheduling, Unicode beyond five letters.

### Surviving mutants

- S1 resync no-op: **killed** by `shift_latin` (`resync_is_required_for_shifted_ascii_dictionary_conversion`).
- S2 saturating_sub: not generated this campaign; shifted-ascii `cursor - 1` ±/÷ mutants were killed.
- Remaining misses are Preferences field-deletes (filter leakage) or kana/decimal arms after the shifted-ascii return. Shift-Latin-arm score 19/19.

## Residual risk

1. **Irreducible under this charter:** no *installed-IME* physical Shift+Backspace. Looked harder than the fake-pipe: production `plan_from_visible` chains AIUEO→AIUE→AIUEO, and a process-local EDIT HWND receives those SetText payloads through `checked_host_call`. Still impossible without a live COM host: `ITfRange::SetText` on a real `ITfContext`, and User32 keys through `sakura_tsf_test_host` (that path is the active language profile).
2. True MC/DC of `dispatch.rs` is still unavailable (`mcdc_records` empty). Shift-Latin-arm region coverage is now measured separately and is ≥70% on each named function (see `coverage/llvm-cov-report.md`). Whole-function % stays low on `apply_backspace` because the kana-group arm is out of scope.
3. TLC still abstracts one typist and five letters. The previously unfinished 6/10 bound is now finished.

## Release decision

**Go** for the *engine* Shift-Latin order fix.

Ship the engine/keymap change. Coverage of the Shift-Latin arms is now honestly separated from kana/CJK (feed 98.5%, backspace 90.0%, render 75.5%, resync 97.1%). The HWND gap is reduced to the physical IME/COM bound, not “we only had a fake pipe.”

Do **not** treat this as a quality-gate pass for reranker, AI text, or the VS Code crash investigation. Do **not** claim a live host `ITfRange` write or an installed-IME keystroke was exercised.
