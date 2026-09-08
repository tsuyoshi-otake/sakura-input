# Settings appearance — Issue #144

## Owner feedback and implemented appearance

On 2026-09-08 the owner rejected the 900×680 preview as too large, supplied a
roughly 615×457 property-sheet reference, and requested conventional connected
tabs instead of separate navigation buttons. The resulting default outer size
is **640×480 logical pixels**, scaled for the monitor and clamped to its work
area. This supersedes the earlier large-window proposal in Issue #144.

- A standard `SysTabControl32` owns category selection, arrows, focus and UIA.
  Its hit-test window covers only the tab strip; the root paints the surrounding
  content frame so decoration cannot cover form controls or change Tab order.
- Native TreeView/ListBox topic navigation remains on the left. Forms use
  Yu Gothic UI: 18px titles, 13px body and 12px supplementary text at 100%.
- Common rows use approximately 27px fields and a 41–42px vertical pitch.
  Long endpoint inputs occupy the available width. The three root actions stay
  fixed below the scrollable form. Larger native lists/text areas are capped to
  the viewport and retain their own scrollbars.
- Dark tabs paint over native tab rectangles without replacing native selection
  or accessibility. High contrast uses the existing system-role/native paths.
- Settings schemas, AI models/authentication, save operations and engine behavior
  were not changed. This work does not install or release a new binary.

Implementation: `crates/sakura-settings/src/ui.rs`, `ui/pages.rs`,
`ui/presentation.rs`, `ui/tabs.rs`.

## Direct verification on 2026-09-08

| Verify | Observed result |
|---|---|
| `cargo fmt --all -- --check` | Passed after formatting the settings crate using its Cargo edition |
| `cargo test -p sakura-settings --locked` | 72 passed; interactive tests remain opt-in |
| `cargo test --workspace --locked` | Passed |
| `git diff --check` | Passed; Git reports normal LF/CRLF conversion notices |
| `every_native_form_keeps_actions_separate_and_reveals_keyboard_targets_at_supported_dpi` | All 19 forms at 96/120/144/192 DPI; root height capped to 728px and width to 1366px; action separation and native keyboard target reachability passed |
| `capture_native_settings_pages` | Native selection reached all input and non-input topics; real native captures saved. Physical routing is covered by the dedicated interaction tests |
| `capture_settings_layout_matrix` | Light/Dark native rendering of every form and the bottom of long forms captured using native selection messages |
| `tab_focus_order_skips_hidden_topics_and_ends_at_actions` | Physical Tab traversal passed |
| `learning_and_update_topics_are_discoverable_and_clickable` | Native tab and topic clicks passed |
| `theme_combo_updates_input_tree_colors_through_user32` | Physical dropdown selection and theme changes passed |
| `apply_persists_preferences_across_a_user32_relaunch` | Native Home/Down selection, physical Apply/OK, process exit and persisted-value readback passed |
| `native_tabs_expose_selected_names_and_keyboard_focus_to_uia` | Native UIA Tab type, focusability, single selection and all five selected names passed |
| Full ignored User32 suite | All 21 native interaction, persistence, DPI, rendering, and UIA tests passed sequentially after the compact top-alignment change |
| Runner/process enumeration | No owned cargo/rustc/settings payload or test runner survived completed tests |

Native screenshot commands are ignored tests in `settings_topic_user32`. Set
`SAKURA_SETTINGS_SCREENSHOTS` to a directory under `~/tmp/` and run with
`--ignored --test-threads=1`. Rendering-only tests do not move the pointer;
physical tests require an interactive desktop. Fixtures disable automatic
network update checks in their isolated LOCALAPPDATA before launching.

Local evidence is under `~/tmp/sakura-settings-144-artifacts/`: `baseline`,
`compact-tabs-03`, `compact-physical`, `compact-tabs-matrix-02`,
`settings-tests.log` and `workspace-tests.log`. Intermediate matrix images
predate the final scrollbar and tab-frame adjustments; `compact-physical` and
later captures identify their actual build conditions in companion text files.

After the compact-layout review, redundant page descriptions were removed and
every form begins directly below its heading. The final top-aligned evidence is
in `compact-top-02`, `compact-top-matrix`, and `compact-top-physical`. The last
two cover every form in both themes and every native topic-selection path.

## Limits and verified learnings

- Physical tests can lose desktop foreground to concurrent activity. The Escape
  cancellation rerun stopped at its foreground prerequisite during an earlier
  attempt. A later full sequential run passed Escape cancellation and all other
  21 User32 tests. Actual Windows high-contrast screenshots, Windows
  text-enlargement changes and real cross-monitor moves have not all been
  completed. They are not represented as passing results.
- Native form geometry checks exercise DPI notifications and reachability;
  they are not a claim of real cross-monitor visual acceptance.
- Compute scrollbar needs from the area without either previous scrollbar.
  Reusing a long page's reduced client area can keep both bars visible on a
  short page. A regression test covers that dependency.
- A native tab body placed over sibling controls can intercept their clicks.
  Restrict its hit area to the strip while keeping the original tab order.
- Submit absolute mouse movement, down and up in one `SendInput` batch.
  Separate cursor positioning and click calls permitted concurrent device
  movement to redirect a test click. Tests still verify the native hit target
  and the resulting selection rather than assuming input was accepted.
- Resolve TreeView test targets by their native text, not historical sibling
  positions: adding the AI and input-repair branches invalidated old indexes.

Related improvements are tracked separately in #145 (nonmodal automatic-update
notification) and #146 (settings search).
