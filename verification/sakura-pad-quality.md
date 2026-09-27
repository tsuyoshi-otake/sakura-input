# Sakura Pad: topmost window and title persistence

Date: 2026-09-27. Related existing issue: #92.
Scope: local source change and direct verification; not a release or installed update.
The separate protection proposal is `docs/plans/sakura-pad-protection.md`.

## Behavior and ownership

`PadWindow` creates its HWND with `WS_EX_TOPMOST` and reasserts `HWND_TOPMOST`
when requested. Existing explicit foreground/focus handling remains; z-order
enforcement uses `SWP_NOACTIVATE`. Ownership/taskbar and DPI behavior remain covered.

`PadState` owns editor snapshots and storage submission. Previously, selecting
another memo or copying could consume a pending edit without submitting it;
the later timer then saw unchanged controls and never saved that edit. Those
paths now submit the captured document. Window destruction captures a final
pending edit while controls are live, before shutting down the storage worker.
The existing two-second worker shutdown budget is unchanged.

`sync_rows` previously compared only IDs/order. A rename preserving those IDs
left the LISTBOX's native label stale, and paint invalidation happened before
capturing the current edit. The changed row is now replaced at its existing
position, selection/top index restored, and painting invalidated after capture.
No automatic title generation, storage-format change, or new dependency is added.

## Executable rubric

Set `CARGO_HTTP_CHECK_REVOKE=false` for every Cargo invocation. Run ordinary
Cargo tests inside `ci/run-test-quiet.ps1`; the retained local driver is
`~/tmp/sakura-pad-quality-20260927-tests.ps1`, with TKW native Cargo captures
and a 300-second timeout. It checks process cleanup in `finally`.

| Criterion | Verify | Expect / observed |
|---|---|---|
| Topmost lifetime | `cargo test -p sakura-renderer --test pad_ui the_pad_splits_above_the_breakpoint_and_folds_below_it -- --ignored --test-threads=1` through the wrapper | topmost on first visibility, restored after deliberate demotion, retained after hide/reopen; existing layout checks pass |
| Stable-position title | wrapped `pad_ui typing_into_a_fresh_pad_puts_the_memo_in_the_list` | native labels update for Japanese, emoji, rename, empty title; original title field remains unchanged by hints |
| Viewport and selection | wrapped `pad_ui renaming_a_scrolled_title_preserves_selection_and_scroll` | with 12 memos, selected row 3 and top row 2 survive a rename |
| Pending title durability | wrapped `pad_ui a_title_edited_just_before` | switch, failed copy, and shutdown each reopen with the exact new title |
| Regressions and static checks | wrapped `cargo test -p sakura-renderer`; `cargo clippy -p sakura-renderer --all-targets -- -D warnings`; `cargo fmt --all -- --check`; `git diff --check` | 152 normal tests pass, 15 explicitly ignored; six real-process Pad tests additionally pass; static checks pass |

Each test command in the table is the inner Cargo command, not permission to
bypass the wrapper. The failed-copy fixture holds the clipboard open and never
empties/writes it; its guard releases ownership even on panic. Test renderer
processes use isolated app data and private fixture pipes. All owned test
processes exited; the installed renderer/engine were not stopped or replaced.

## Evidence and corrections

- Fail-first pending switch: TKW `c9490de1cd6c813a6c573a81fdae9577` reopened
  with `編集前` instead of `切り替え直前のタイトル 🌸`.
- Fail-first native label: TKW `477bcda9a9a38f5ae87b98ffac4ce46f` timed out
  waiting for `会議メモ 🌸` without an ID/order change.
- First topmost attempt set z-order only after show/paint; TKW
  `4fd6dbd193da20686b5201da9fe7957e` observed visibility before topmost.
  Creation now sets the style, closing that initial interval.
- Full normal renderer regression: TKW `8a8d79ccadc8f1bcefd8a38ff44f8427`.
- Final native suite: six pass, one manual preview excluded, TKW
  `d145267040ab5fbd6c4c92f1108e8671` (request `273ea749f089477284b8a9abafc58538`).
- Initial viewport fixture requested a height below Pad's 360 logical-pixel
  outer minimum; it now uses a 400 logical-pixel client. Transient sort notices
  were also an invalid completion fence; the command is now synchronous.
  The existing notice-layout test now waits for seed storage completion before
  posting its notice, so an older save completion cannot erase the observation.
- Clippy required safety comments at three new test unsafe blocks; corrected
  and rerun successfully (request `e531da2b5e2449598c5ab2c8ac730aae`).

R6 is a FUTURE gate targeting `src/pad/rail.rs`; forcing it fails because that
path does not exist. Existing `pad_rail.rs` still calls `pad::dpi_of`. No new
edge was added and no unrelated architecture migration was attempted. Use the
current CI gate set: `ci/check-dependency-rules.ps1 -Advisory -Enforce R1,R2,R8,R9,R12,R13`.

## IRV and remaining limits

A. Actual implementation scope: `pad.rs` window lifecycle, editor capture,
list synchronization, and storage submission; `pad_ui.rs` real-process tests.
Supporting contract reads: `pad_storage.rs`, `pad_list.rs`, placeholder/layout
tests, README Pad section, required repository/machine/test guidance.

B. Scope expanded from display quality to durability because selection/copy
consume the same pending title edit as the debounce timer. Shutdown owns the
last capture. Additional mandatory guide reading exceeded the default
three-guide trigger; no unrelated source subsystem was changed.

C. UI state remains in `PadState`, HWND lifecycle in `PadWindow`, persistence
in the existing `StorageWorker`. No dependency or persisted schema changes.
The proposal for protection does not implement or change these boundaries yet.

D. The next title issue can start from the named `pad_ui` cases and this
capture/submission contract. There is no claim that the existing large Pad
module has become smaller or that broader architectural IRV has improved.

Residual limits: this verifies normal local save/restart, not arbitrary disk
failure or power loss. Other topmost windows and secure desktops follow Windows
z-order rules. No full workspace or release/installer run was needed for this
renderer-local change. New password/TOTP/YubiKey protection remains a proposal.
