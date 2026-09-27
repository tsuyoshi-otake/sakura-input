# Sakura Pad Settings action icons — #269, 2026-09-27

The Sakura Pad Settings page previously placed three text-only actions in one
row. In the default 640×480 window each button was too narrow for a legible
icon beside its full Japanese name. The page now puts Open and Lock in the
first row and Protection Settings in the second, with notebook, lock and shield
line icons drawn beside the unchanged action names.

| Criterion | Verify | Expect / observed |
|---|---|---|
| Compact layout and native names | `cargo test --locked -p sakura-settings pad_action_names_and_two_row_layout_are_reachable_at_supported_dpi -- --test-threads=1` | At 96 DPI all three named buttons fit the first viewport without scrolling. At 120–192 DPI they stay vertically visible and can be reached by native horizontal focus scrolling when the work area is narrow. |
| Light/dark appearance | `SAKURA_SETTINGS_SCREENSHOTS=~/tmp/sakura-pad-icons-269-v3` with ignored `settings_topic_user32` test `capture_settings_layout_matrix` | Passed; inspected `light-input-11.bmp` and `dark-input-11.bmp`. Icon and label are visibly separate in each theme. The settings shield is empty so it does not imply an unprotected Pad is already secured. |
| Existing settings behavior | `ci/run-test-quiet.ps1` running `cargo test --locked -p sakura-settings -- --test-threads=1` | Passed: 115 tests; 21 interactive User32 tests remain ignored by the ordinary suite. The visual capture was run separately. |
| Static checks and runner lifecycle | `cargo clippy --locked -p sakura-settings --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `git diff --check`, `ci/check-process-clean.ps1` | Passed; no repository-scoped test runner remained. |

The button text still supplies the native control name and keyboard focus
target. The existing high-contrast path switches these controls to Windows
native push buttons, so the icon is omitted there and system colors remain in
charge. An actual high-contrast screenshot was not captured. The icon group
falls back to text-only if an enlarged font cannot fit beside it.

The first CI run after formatting exposed an overstrong test assumption at
192 DPI: its work area reduced the form viewport below the authored content
width. The production layout already has horizontal focus scrolling. The
regression test now exercises that path explicitly on a constrained viewport
instead of treating the DPI-dependent horizontal scrollbar as a defect.

The page owns action placement, the Settings theme owns button drawing and
state colors, and `pad_action_icons.rs` owns only the three scale-independent
shapes. No Pad protection state or renderer message contract changed. The
work expanded from the Pad page to the shared button painter only because
the existing owner-draw path is the single owner of light/dark, focus,
pressed and disabled states. Future Settings action-icon changes can start
from this painter and its local shape module without reading Pad storage or
authentication internals; they must still understand the native control name
and high-contrast fallback contracts.
