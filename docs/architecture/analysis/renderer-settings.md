# `sakura-renderer` / `sakura-settings` structural analysis (HEAD a370477)

Read-only. `prod` = total minus `#[cfg(test)]` regions at column 0. `unsafe` = token occurrences.

## 1. Module inventory

### `crates/sakura-renderer` (binary only — **no `lib.rs`**, `src/main.rs:28-42` declares all 16 modules)

| file | total | prod | fn | pub fn | `unsafe` | responsibility |
|---|---|---|---|---|---|---|
| src/main.rs | 741 | 644 | 18 | 0 | 28 | Process entry, host window, `App` god-struct, `WM_UI` fan-out |
| src/candidate.rs | 3,432 | 2,067 | 114 | 7 | 43 | Candidate popup: layout + placement + GDI paint + delete overlay + commit clicks |
| src/pad.rs | 3,752 | 3,096 | 105 | 8 | 112 | Sakura Pad window: layout, controls, painting, `PadState` model, 295-line wndproc |
| src/pad_storage.rs | 1,310 | 989 | 66 | 23 | 6 | `SKRLPAD2` document codec, DPAPI, debounced writer thread |
| src/indicator.rs | 1,023 | 683 | 43 | 4 | 25 | Mode indicator popup: placement vs caret/candidate popup, paint |
| src/pad_rail.rs | 831 | 737 | 31 | 5 | 52 | Custom scroll rail control |
| src/pad_icon.rs | 600 | 494 | 22 | 3 | 9 | Vector pictograms |
| src/watch.rs | 573 | 484 | 22 | 4 | 2 | Engine watchdog: connect/follow/backoff/relaunch + delete/commit worker threads |
| src/raw_input.rs | 501 | 368 | 20 | 10 | 6 | RAWINPUT registration + packet → `GestureInput` |
| src/pad_gesture.rs | 486 | 314 | 18 | 5 | 0 | Pure Ctrl-double-tap state machine |
| src/accessibility.rs | 470 | 272 | 21 | 6 | 6 | COM apartment + `IRawElementProviderSimple` |
| src/theme.rs | 435 | 335 | 24 | 19 | 8 | Palette, DPI scale, shared GDI text/fill helpers |
| src/pad_list.rs | 383 | 204 | 21 | 6 | 1 | Pure list model |
| src/glyph.rs | 183 | 129 | 11 | 5 | 4 | あ/A glyph + `Mode` ↔ `isize` codec |
| src/pad_tooltip.rs | 165 | 165 | 3 | 2 | 5 | Tooltip control |
| src/pad_caption.rs | 162 | 151 | 5 | 3 | 5 | DWM-tinted title bar |
| tests/candidate_detail_uia.rs | 1,751 | — | 73 | 0 | 78 | Real renderer process on a private pipe; UIA detail (4 `#[test]`, all `#[ignore]`) |
| tests/pad_ui.rs | 1,258 | — | 43 | 0 | 33 | Real renderer process (3, all `#[ignore]`) |
| tests/candidate_uia.rs | 647 | — | 31 | 0 | 22 | Real release engine+renderer UIA gate (1, `#[ignore]`) |
| tests/watchdog_recovery.rs | 414 | — | 15 | 0 | 2 | Kills a real engine (1, `#[ignore]`) |
| tests/resource_budget.rs | 262 | — | 14 | 0 | 1 | Private working set (1, `#[ignore]`) |

### `crates/sakura-settings` (lib + 2 bins; `src/lib.rs:17-29` exports 12 modules; **`ui` and `cli` are `main.rs`-private**, `src/main.rs:7-8`)

| file | total | prod | fn | pub fn | `unsafe` | responsibility |
|---|---|---|---|---|---|---|
| src/ui.rs | 4,999 | ~4,493 | 213 | 2 | 150 | Whole control panel: `App` god-object, all topic pages' values/commands, theme, Win32 control factory, enum↔combo mappings, 322-line wndproc |
| src/updater.rs | 2,422 | 1,592 | 88 | 10 | 29 | Update state machine + WinHTTP + BCrypt SHA-256 + Authenticode + silent installer |
| src/update_trust.rs | 1,712 | 1,162 | 54 | 12 | 8 | Frozen signing-v2 manifest/keyring/signature contract + rollback trust state |
| src/cli.rs | 1,268 | 956 | 30 | 4 | 0 | Argument parsing + 411-line `run()` dispatch + text rendering |
| src/input_history.rs | 923 | 708 | 35 | 9 | 2 | Developer history view/export/mine/clear/flush/stats |
| src/ui/presentation.rs | 886 | 705 | 45 | 26 | 15 | Logical geometry, font ownership, scrolling, `PageBuilder` |
| src/ui/pages.rs | 536 | 536 | 5 | 5 | 0 | 5 `create_*_controls` factories (343-line general one) |
| src/formats.rs | 483 | 412 | 19 | 7 | 2 | Sakura/MS-IME/ATOK/Mozc dictionary text formats |
| src/user_dictionary.rs | 267 | 184 | 16 | 7 | 0 | Transactional dictionary CRUD/import/export |
| src/configuration.rs | 243 | 141 | 12 | 4 | 0 | Loss-aware config document read/modify/write |
| src/engine_faults.rs | 233 | 142 | 12 | 5 | 0 | Engine fault-injection counters over IPC |
| src/diagnostics.rs | 228 | 153 | 14 | 8 | 0 | Render bounded timeout/disconnect logs |
| src/engine_timing.rs | 193 | 122 | 10 | 4 | 0 | Engine per-stage timing over IPC |
| src/ui/tabs.rs | 162 | 162 | 3 | 1 | 10 | Tab-control subclass + dark paint |
| src/learning.rs | 125 | 77 | 7 | 4 | 0 | Learning view/export/clear |
| src/engine_admin.rs | 115 | 115 | 5 | 5 | 0 | Shared engine admin connect/handshake/trust-policy |
| src/bootstrap.rs | 114 | 96 | 5 | 0 | 0 | Install-root launcher |
| src/storage.rs | 88 | 66 | 3 | 1 | 0 | `atomic_write` |
| src/paths.rs | 61 | 61 | 11 | 10 | 0 | Canonical per-user paths |
| src/main.rs | 43 | 43 | 1 | 0 | 0 | GUI-vs-CLI branch |
| tests/settings_topic_user32.rs | 3,037 | — | 109 | 0 | 93 | Launches payload, real `SendInput`; 17 `#[test]`, **all `#[ignore]`** |
| tests/support/settings_visual.rs | 429 | — | 11 | 0 | 29 | Screenshot/UIA capture helpers |
| tests/format_fixtures.rs | 87 | — | 2 | 0 | 0 | 2 headless tests |

## 2. Intra-crate dependency graphs

**renderer**

```
main       -> accessibility, candidate, indicator, pad, pad_gesture, raw_input, watch, glyph
candidate  -> accessibility, theme, watch          (candidate.rs:36,38,39,43)
indicator  -> candidate, glyph, theme              (indicator.rs:57)
pad        -> pad_caption, pad_icon, pad_list, pad_rail, pad_storage, pad_tooltip, theme
pad_rail   -> pad, theme                           (pad_rail.rs:351 `crate::pad::dpi_of`)
pad_list   -> pad_storage
pad_icon, pad_caption -> theme
raw_input  -> pad_gesture
leaves: theme, glyph, pad_storage, pad_gesture, watch, accessibility, pad_tooltip
```

Cycles: **`pad` ↔ `pad_rail`** (`pad.rs:616,1461-1463,2398` ⇄ `pad_rail.rs:351`) — the only one; one function call (`dpi_of`).
Hubs: `theme` (in-degree 6), `pad` (out-degree 6, 3,096 prod lines), `candidate` (2,067 prod lines).
Wrong-direction edge: **`indicator` → `candidate`** for three geometry constants and `monitor_work_area` (`indicator.rs:76,261,573`). A mode-indicator issue drags 2,067 lines of candidate code into the reading set.

**settings**

```
bin(main) -> cli, ui
cli  -> configuration, diagnostics, engine_faults, engine_timing, formats, input_history, learning, paths, updater, user_dictionary
ui   -> ui::pages, ui::presentation, ui::tabs, configuration, formats, user_dictionary, diagnostics, learning, paths, updater
ui::pages        -> super::*  (pages.rs:3)
ui::presentation -> super::*  (presentation.rs:7)
ui::tabs         -> super::*  (tabs.rs:3)
updater -> update_trust, storage
learning / input_history / engine_faults / engine_timing -> engine_admin, storage
configuration / user_dictionary -> storage, formats
leaves: paths, storage, engine_admin, formats
```

Cycles: **`ui` ↔ `ui::pages`**, **`ui` ↔ `ui::presentation`**, **`ui` ↔ `ui::tabs`** — all three `use super::*` back-edges. `ui.rs:18` does `use pages::*` while `pages.rs:3` does `use super::*`; `tabs.rs:39,73` reads the `App` type and its `GWLP_USERDATA` pointer; `presentation.rs:589-701` calls ui.rs's control factories. The #144 split moved *text* but created no dependency boundary: none of the three submodules compiles without ui.rs.

## 3. `settings → engine` coupling

`rg -n 'sakura_engine' crates/sakura-settings` → 6 hits, 3 of them one doc comment and two test-only:

| use site | items | what for | verdict |
|---|---|---|---|
| `src/learning.rs:6` | `read_snapshot, LearningService, LearningSnapshot, LEARNING_FORMAT_VERSION` | decode `learning.bin`, offline clear | **store format** |
| `src/input_history.rs:8` | `clear_path, read_snapshot, InputHistoryRecord, InputHistorySnapshot, KeyHistoryRecord, ScopeClass, INPUT_HISTORY_FORMAT_VERSION` | decode DPAPI history, render, mine, offline clear | **store format** |
| `src/input_history.rs:712,750` (tests) | `CommitHistoryRecord, KeyHistoryRecord, InputHistoryRecord, InputHistoryService::open` | fixtures | store format |
| `src/engine_faults.rs:4-5` | doc comment only | — | no link |

The entire dependency on a 30-module engine crate exists to read two file formats. `crates/sakura-engine/src/input_history.rs` imports only `std`, `sakura_proto`, `windows` (lines 14-32) — **zero `crate::` edges**, fully extractable. `crates/sakura-engine/src/learning.rs` imports `sakura_ipc::debug_trace`, `sakura_proto`, and exactly two engine internals: `crate::session::text_hash` and `crate::timing` (lines 23-27). A `sakura-store` crate needs only those two symbols moved or duplicated. Fault injection, timing and diagnostics are already IPC-only via `engine_admin`.

Removing the dep also drops transitive `sakura-neural-proto` and `sakura-ai-proto`-via-engine links and the engine's `windows` feature set from the settings build.

Other crate usage by settings: `sakura_core` — `configuration.rs:7` (`parse_preferences`, `serialize_preferences_with_profiles`, `default_app_profiles`, `is_valid_profile_process_name`, `Preferences`, `AppProfile`, `CONFIG_FORMAT_VERSION`), `formats.rs:13`, `user_dictionary.rs:9,188`, `cli.rs:5`, `ui.rs:28-33` (21 preference enums). `sakura_proto` — `Mode` (`cli.rs:10`, `ui.rs:34`); `Request/Response` + `PROTOCOL_VERSION` in `engine_admin.rs:16`, `learning.rs:11`, `input_history.rs:14`, `engine_timing.rs:17`, `engine_faults.rs:19`. `sakura_ipc` — `Client, Endpoint, Fault, ServerTrustPolicy` (`engine_admin.rs:15`), `diagnostics::*` (`diagnostics.rs:12,157`, `paths.rs:36`), `debug_trace::{read_text, clear}` (`cli.rs:586,633,646`). `sakura_reg` — `user_preferences::{AiAuth, …, AiTextPreferences}` (`ui.rs:35-37`); `com_server, RegistryView` (`bootstrap.rs:17`). `sakura_ai_proto` — one symbol, `MODEL`, at `ui/pages.rs:74`.

## 4. `settings/ui.rs` + `ui/` anatomy

| cluster | lines | note |
|---|---|---|
| imports, window classes, colour constants | 7-162 | 40 palette constants inline |
| topic index constants + tree labels | 166-201 | `INPUT_TOPIC_*` 0..10, `INPUT_TREE_LABELS[13]` |
| `GeneralControls` (68 HWND fields) | 203-270 | one struct for 11 topics |
| `ThemeBrushes` / `UiTheme` | 279-531 | `resolve`, `apply_control_colors`, `draw_button` |
| `DictionaryControls`/`Learning`/`Diagnostics`/`UpdateControls` | 533-580 | |
| update enums + `close_decision` | 582-613 | |
| `App` struct (33 fields) | 615-647 | |
| single-instance + `run` + `show_fatal_error` | 651-767 | |
| `App::new` / `handle_command` | 770-1004 | `handle_command` = 136 lines of `if source == …` (174 `self.general.` reads) |
| navigation: `show_panel`, `show_page_controls`, `populate_topic_list`, `populate_input_tree`, `show_topic_controls`, `layout` | 1006-1381 | `show_topic_controls` 135 lines, hard-coded per panel |
| general/profile values | 1382-1510, 2015-2080 | |
| save/global apply | 1511-1589 | |
| AI provider/credential UI | 1590-1600, 1697-1734, 3505-3656 | |
| input-support (repair) topic | 1601-1696 | |
| normalizer + notation presets | 1735-1857 | |
| appearance/DPI/theme | 1858-2014, 3854-3979 | |
| user-dictionary editor | 2081-2224 | |
| learning management | 2225-2278 | |
| diagnostics | 2279-2300 | |
| updater UI | 2301-2532 | |
| close/status | 2533-2574 | |
| Win32 control factory | 2672-3204 | `label/button/checkbox/radio/combo/edit/listbox/tree/control` |
| text/list/combo helpers | 3228-3396 | |
| **enum ↔ combo index mappings** | 3397-3853 | ~460 lines, pure, headless |
| theme application to native controls | 3854-3979 | |
| misc (`confirm`, `message_box`, `pump`) | 3980-4055 | |
| `panel_window_procedure` / `window_procedure` | 4074-4495 | 322-line wndproc |
| tests | 4497-4999 | 26 headless `#[test]` |

**What #144 (8859ea7) actually moved**: only *control creation* (`ui/pages.rs`) and *geometry/fonts/scroll* (`ui/presentation.rs`) plus the tab subclass. Every value read/write, every command route, the enum-mapping table, the theme, the control factory the pages call back into, and both window procedures remain in ui.rs. Net effect on an agent's reading set is small because all three submodules `use super::*`.

Functions > 120 lines: `ui.rs:4174-4495 window_procedure` (322), `ui.rs:869-1004 handle_command` (136), `ui.rs:1162-1296 show_topic_controls` (135), `ui/pages.rs:5-347 create_general_controls` (343).

**Test drive**: `tests/settings_topic_user32.rs` launches the real `sakura_settings_payload.exe` (`:1946`) with an isolated config fixture, injects mouse via `SendInput`; `WM_COMMAND` only reads back state. All 17 tests are `#[ignore = "requires an interactive User32 desktop"]`. Topics: profile (:205), DPI reflow (:397), tab focus order (:508), Escape/cancel (:579), single-instance (:696), conversion tree (:744), input-support (:961), input-assist (:1008), segment (:1119), normalizer reset (:1184), normalizer apply (:1328), notation preset (:1494), input-method radios (:1607), dictionary (:1704), learning+update (:1842).

**Topic → line range**:

| topic | control creation (`ui/pages.rs`) | values/commands (`ui.rs`) |
|---|---|---|
| Basic | 15-45 | 1382-1491, 1511-1589 |
| Input assist | 46-65 | 4791-4808, `handle_command` slice |
| AI text | 66-104 | 1590-1600, 1697-1734, 3505-3656 |
| Prediction | 105-117 | 1382-1491 |
| Segment | 118-138 | 3404-3417 |
| Normalizer | 139-172 | 1735-1857, 3657-3771 |
| Association | 173-182 | — |
| Input repair | 183-204 | 1601-1696 |
| Input symbol | 205-225 | 1601-1696 |
| Display | 226-241 | 1858-1937, 3792-3853 |
| Profile | 242-347 | 1492-1510, 2015-2080 |
| Dictionary | 349-423 | 2081-2224, 3381-3396 |
| Learning | 424-460 | 2225-2278 |
| Diagnostics | 461-484 | 2279-2300 |
| Update | 485-536 | 2301-2532 |

## 5. Renderer anatomy

`main.rs`: `App` (`:129-153`) owns `Indicator`, `CandidateWindow`, `Option<PadWindow>`, `RawInputOwner`, the `Arc<Mutex<Option<UiState>>>` mailbox and two completion receivers. One hidden host window (`create_host` `:323`); watcher/deleter/committer threads at `:180-190,220-226`.

```
watch::run/follow (:312/:380, watcher thread)
  -> Signal::Ui -> report() main.rs:371  (mailbox, PostMessageW WM_UI)
  -> procedure() main.rs:412  WM_UI arm (:418-500)
       -> CandidateWindow::update  candidate.rs:250
            -> layout()            candidate.rs:1544   (pure)
            -> detail_layout()     candidate.rs:1606   (pure)
            -> monitor_work_area() candidate.rs:432    (Win32)
            -> popup_placement()   candidate.rs:638    (pure; place_candidates :567)
            -> CandidateAccessibility::update accessibility.rs:152 -> announcement() :194 (pure)
            -> set_window_rect :857 / InvalidateRect
       -> Indicator::show/reposition_if_visible indicator.rs:150/224
            -> geometry() :252 -> placement_in() :281   (pure)
       -> WM_PAINT -> paint_display candidate.rs:1228 -> draw() :1335 (GDI)
                   -> indicator paint() indicator.rs:543
```

Pad: `PadWindow::new` `pad.rs:801` + `create_controls` `:1324`; `pad_procedure` `:2833` (295 lines) drives `PadState` `:2336-2831`; `layout()` `:243-662` (420 lines, **pure**) then `update_layout` `:1612` and `paint` `:1672`. Storage: `PadState::publish :2578` → writer thread → `poll_storage :2601`. Gesture: WM_INPUT → `raw_input::reduce_keyboard_packet :63` → `PadGesture::handle pad_gesture.rs:151` → `WM_PAD_TRIGGER`.

Functions > 120 lines: `pad.rs:243 layout` (420), `pad.rs:2833 pad_procedure` (295), `main.rs:412 procedure` (223), `candidate.rs:1335 draw` (208), `candidate.rs:1648 draw_detail` (146), `pad.rs:1324 create_controls` (149), `pad_rail.rs:339 rail_procedure` (144).

Pure-vs-GDI: `candidate.rs` is already cleanly split — layout/placement types (`:87-127`) and pure functions (`:410-2066`) vs `draw`, `draw_detail`, `draw_delete_overlay`, `paint_*` on an HDC. The leak is `text_width_96` (`:1846`) — a *pure estimate* used by layout while `theme::text_width` measures with a real DC. `pad.rs` mixes: `layout` (pure, 420 lines) sits 1,000 lines away from `paint`/`draw_row`, with `PadState` between. `indicator.rs` similarly holds pure `placement_in` (`:281`) beside `paint` (`:543`).

## 6. State machines and invariants

**Watchdog** — `watch.rs`: connect (`PipeBinding::connect :277`) → `follow :380` → `Ending` (`:355`) → `retry_schedule :370` → `decide :438` → `relaunch :454`. Floor 250 ms (`:58`), ceiling 30 s (`:59`), `grow_backoff :366`; healthy link restarts at floor (test `:524`); protocol rejection keeps exponential delay (test `:515`); `RELAUNCH_GAP` 5 s (`:67`); `WATCH_BUDGET` 15 s (`:50`) must outlast the heartbeat (test `:492`). Invariants: `:12` "must never spawn anything" for the test path; `:436` engine is a per-logon singleton.

**Candidate window** — `candidate.rs`: width is page-derived and must not move on selection (test `:2533`). Detail placement right → left → below → absent (`popup_placement :638`, test `:3345`); detail dropped rather than covering composition (`covers_composition :425`, `covers_document :458`); malformed details omitted (`:266-271`). Delete-overlay lifecycle: `clear_pending_history_deletes_for_new_revision :1152`, `is_history_delete_pending :1172`, `finish_pending_history_delete :1179`, `delete_overlay_should_be_visible :1144` (tests `:2430,:2457`). `:940` "The display popup owns screen placement."

**UI placement (#142)** — `UiState::document: Option<ScreenRect>`; request `sakura_proto::Request::SetUiPlacement` (`message.rs:293`). Renderer-side rules in `place_candidates :567` (`MAX_CARET_DETOUR_96 = 160`, `:55`), `caret_distance :541`, `opaque_window_covers :487`; timeout/retry is TSF-side.

**Pad storage** — `pad_storage.rs`. `SKRLPAD2`/v2 (`:48-49`), legacy v1 migrates in as memo 1 (`:55-59`). Bounds: 256 title units, 65,536 body, 200 memos, 4,000,000 document units, 24 MiB protected (`:30-61`). `validate :333` runs on both encode and decode. Debounce 300 ms, shutdown flush 2 s.

**Updater** — `updater.rs`. `check_for_update_at :382` → `apply_update :455` (166 lines) walking `UpdateStage` (`:244`): download → hash/size → `InstallerFileGuard::open :678` / `verify :694` → `verify_identity :808` / `verify_path_identity :822` → `SignatureVerifier` (`AuthenticodeVerifier :1433`) → `InstallerRunner` (`SilentInstaller :1501`). Bounds: manifest 64 KiB, 5 redirects, 30-min install, 15-min installer, 2-s trust lock (`:66-72`). Test `:2394` "an unsigned file must fail closed".

**Update trust (#150)** — `update_trust.rs`. `authorize_manifest :893` → `TrustDecision` (`:792`). `TrustState :798`; files `trust-state.txt` / `.lock` / `apply.lock` (`TrustPaths::adjacent_to :868`). Rules: equivocation rejected (`:945`); inconsistent release rejected (`:957`); state below embedded floor treated as absent and rebuilt (test `:1458`); `atomic_replace_trust_state :1042`; terminal error names the file (`name_trust_state_file :1001`). Keyring compiled in (`:38 include_bytes!`), max 3 keys.

## 7. Test topology

Headless today: all renderer inline tests except `indicator.rs:780`; all settings inline tests; `tests/format_fixtures.rs`. That is 33 candidate, 20 pad, 16 indicator, 15 pad_storage, 8 watch, 26 ui.rs, 16 updater, 9 update_trust, 7 cli tests.

Needs a live desktop / real processes: **every** file under `crates/sakura-renderer/tests/` and `crates/sakura-settings/tests/settings_topic_user32.rs` + `support/settings_visual.rs` — 27 of 28 integration tests `#[ignore]`d, so CI's `cargo test --workspace` never runs them.

Structural blocker: **the renderer has no `lib.rs`**, so `crates/sakura-renderer/tests/*` links none of the renderer's own code (`pad_ui.rs:3-8`) — every integration test must drive a process. `cargo test -p sakura-renderer --lib` does not exist; pure functions like `pad::layout`, `placement_in`, `popup_placement`, `PadGesture` are reachable only through `--bin sakura_renderer`. Same for `ui.rs`/`cli.rs` (`settings/src/main.rs:7-8`).

Would become headlessly testable after separating layout/model from painting: `pad::layout` + `PadState` transitions, `indicator::placement_in`, `candidate::popup_placement` with a real text measurer, the settings enum-mapping table (`ui.rs:3397-3853`), `show_topic_controls`'s visibility rule as a pure `topic → visible pages` function, and `handle_command`'s routing as a pure `control-id → intent` map.

## 8. Findings

1. `settings` links the whole `sakura-engine` for two file formats only (`learning.rs:6`, `input_history.rs:8`). → Extract `sakura-store`.
2. `sakura-engine::input_history` has **zero** intra-engine imports; `learning` has exactly two (`session::text_hash`, `timing`). → The extraction is mechanical.
3. `ui.rs` is a 4,493-line god-module with a 33-field `App` and a 68-HWND `GeneralControls`; 174 sites read `self.general.`. → One module per settings topic.
4. The #144 split created three `use super::*` cycles and moved no dependency. → Make `presentation` a leaf; `pages` one file per topic depending on `presentation` only.
5. `ui.rs:3397-3853` is ~460 lines of pure enum↔combo mappings with 12 of the 26 headless tests. → `settings_model.rs`, testable with `--lib`.
6. `window_procedure` (322), `handle_command` (136), `show_topic_controls` (135) each fan out to every topic. → Topic registry table.
7. `indicator.rs:57,76,261,573` reaches into `candidate`. → Move `monitor_work_area`, `ROW_HEIGHT_96`, `NUMBER_WIDTH_96` into `theme`/`screen`.
8. `pad.rs` is 3,096 production lines mixing pure `layout` (420), wndproc (295), `PadState`, and painting. → `pad/{layout,model,paint,window}.rs`.
9. `pad_rail.rs:351` calls `crate::pad::dpi_of`, the only renderer cycle. → Move `dpi_of` into `theme`.
10. `candidate.rs` already has a clean pure/GDI seam but shares one file. → `candidate/{layout,placement,paint,overlay,window}.rs`.
11. `candidate.rs:1846 text_width_96` estimates while `theme.rs:285 text_width` measures — two authorities. → One `Measure` trait.
12. No `lib.rs` in `sakura-renderer`. → Add one so `cargo test -p sakura-renderer --lib` exists.
13. `updater.rs` mixes state machine (`:356-655`) with WinHTTP (`:898-1290`), BCrypt (`:1291-1432`), Authenticode (`:1433-1500`), installer runner (`:1501-1590`) — already behind four traits (`:181,192,221,233`). → Split by trait implementation.
14. `update_trust.rs` combines the frozen wire contract (`:118-790`) with mutable rollback state (`:792-1162`). → `update_contract.rs` (frozen, bound to `verification/fixtures/update-signing-v2`) and `update_trust_state.rs` (#150); #150-shaped issues read ~370 lines instead of 1,162.
15. 27 of 28 integration tests are `#[ignore]` and CI runs one `cargo test --workspace`. → Per-crate jobs plus a named desktop job.

## 9. Target structure proposal

```
crates/sakura-store/               (new; no Win32 UI, no engine)
  src/lib.rs
  src/learning_store.rs            <- sakura-engine/src/learning.rs
  src/input_history_store.rs       <- sakura-engine/src/input_history.rs
  src/text_hash.rs                 <- engine session::text_hash
  deps: sakura-proto, sakura-ipc(debug_trace), windows{Security_Cryptography,Storage_FileSystem}
  sakura-engine re-exports under old paths

crates/sakura-renderer/
  src/lib.rs                       (new) pub mod for every pure module
  src/main.rs                      process entry + host window + App wiring        (<300)
  src/theme.rs                     palette, dpi, dpi_of, text measurement          (leaf)
  src/screen.rs                    monitor_work_area, ScreenRect<->RECT            (leaf)
  src/glyph.rs                                                                     (leaf)
  src/candidate/{mod,layout,placement,paint,overlay,window}.rs
  src/indicator/{mod,placement,paint,window}.rs
  src/pad/{mod,layout,model,paint,window,storage,list,rail,icon,caption,tooltip}.rs
  src/input/{raw_input,gesture}.rs
  src/watch/{mod,backoff,relaunch,workers}.rs
  src/accessibility.rs
  tests/  (process-level only)
allowed edges: window -> paint -> layout/placement -> theme/screen ; model -> storage
              accessibility -> proto only ; nothing depends on candidate except main

crates/sakura-settings/
  src/lib.rs
  src/paths.rs  src/storage.rs  src/engine_admin.rs                       (leaves)
  src/config/{configuration,formats,user_dictionary}.rs
  src/history/{view,mine,admin}.rs        (uses sakura-store)
  src/learning.rs                          (uses sakura-store)
  src/engine/{faults,timing,diagnostics}.rs
  src/update/{contract,trust_state,flow,http,digest,authenticode,installer}.rs
  src/ui/
    mod.rs            App shell: window, message loop, navigation registry  (<600)
    presentation.rs   geometry, fonts, scrolling, control factory  (LEAF - no super::*)
    theme.rs          palette + owner-draw
    tabs.rs           depends on presentation + a TabModel value, not App
    topics/{basic,input_assist,ai_text,prediction,segment,normalizer,association,
            input_repair,input_symbol,display,profile,dictionary,learning,
            diagnostics,update}.rs   each: controls struct, create(), load(), save(), on_command()
    model.rs          enum<->index mappings and labels (pure, headless)
  src/cli/{parse,run,render}.rs
allowed edges: ui::topics -> ui::presentation, ui::theme, ui::model, domain modules
               ui::mod -> ui::topics (registry only); no topic -> topic; no ->super::*
```

Test convention: pure logic inline `#[cfg(test)]`; cross-module headless in `tests/*_headless.rs` against the new `lib`; process/desktop tests stay in `tests/` with `#[ignore]`.

Bound verification artifacts: `tests/candidate_detail_uia.rs` + `candidate_uia.rs` → `src/candidate/`; `tests/watchdog_recovery.rs` → `src/watch/`; `tests/resource_budget.rs` → renderer crate root; `verification/fixtures/update-signing-v2/*` + `ci/test-update-signing-v2.ps1` → `src/update/contract.rs` (frozen); `docs/settings-appearance-verification.md` + `tests/support/settings_visual.rs` → `src/ui/theme.rs` + `presentation.rs`; `tests/format_fixtures.rs` → `src/config/formats.rs`.

| issue | BEFORE | ≈lines | AFTER | ≈lines |
|---|---|---|---|---|
| #47/#142 popup placement | candidate.rs, indicator.rs, main.rs, theme.rs | 3,730 | candidate/placement.rs, candidate/layout.rs, screen.rs | 700 |
| #92 pad feature | pad.rs, pad_storage.rs, pad_list.rs, pad_rail.rs, main.rs | 5,570 | pad/model.rs, pad/layout.rs (+ pad/storage.rs) | 1,300 |
| #150 updater trust | update_trust.rs, updater.rs, ui.rs(2301-2532) | 2,990 | update/trust_state.rs, update/flow.rs | 800 |
| #146 settings navigation | ui.rs, ui/pages.rs, ui/presentation.rs, ui/tabs.rs | 5,900 | ui/mod.rs (registry), ui/topics/<one>.rs | 800 |

## 10. Migration sequence

1. **Renderer `lib.rs`.** Add `crates/sakura-renderer/src/lib.rs` re-exporting existing modules; `main.rs` becomes `use sakura_renderer::…`. Risk: low (check the single-instance mutex is only touched from `main`). `Verify:` `cargo test -p sakura-renderer --lib`. `Expect:` the ~150 previously bin-only unit tests run under `--lib` with the same pass count.
2. **Break the two renderer cycles.** Move `pad::dpi_of` (`pad.rs:1513`) and `candidate::monitor_work_area` (`candidate.rs:432`) + `ROW_HEIGHT_96`/`NUMBER_WIDTH_96` into `theme.rs`/new `screen.rs`; fix `pad_rail.rs:351`, `indicator.rs:57,76,261,573`. `Verify:` `rg -c 'crate::pad::' crates/sakura-renderer/src/pad_rail.rs` → 0 and `rg -c 'candidate::' crates/sakura-renderer/src/indicator.rs` → 0; `cargo test -p sakura-renderer --lib`. `Expect:` both zero, tests unchanged.
3. **Split `candidate.rs` by seam.** `candidate/layout.rs` (`:87-127,1544-2066`), `placement.rs` (`:425-757`), `paint.rs` (`:1228-2018`), `overlay.rs` (`:758-1227`), `window.rs` (`:147-409`). Risk: medium (33 tests re-homed). `Verify:` `cargo test -p sakura-renderer --lib candidate::`. `Expect:` 33 tests, same names, all pass.
4. **Extract `sakura-store`.** Move `sakura-engine/src/{input_history,learning}.rs` + `session::text_hash`; re-export from the engine; repoint `settings/src/{input_history,learning}.rs`; drop `sakura-engine` from `crates/sakura-settings/Cargo.toml`. Risk: medium (DPAPI + writer threads move verbatim; do not touch scope fail-closed checks). `Verify:` `rg -c 'sakura_engine' crates/sakura-settings/src` → 0; `cargo test -p sakura-store`; `cargo test -p sakura-settings --lib`. `Expect:` zero hits; the 6 history + 2 learning tests pass unchanged.
5. **Split `update_trust.rs`.** `update/contract.rs` (`:29-790`) and `update/trust_state.rs` (`:792-1162`). Keep `include_bytes!` paths and the fixture directory byte-identical. Risk: medium-high (fail-closed boundary). `Verify:` `pwsh ./ci/test-update-signing-v2.ps1`; `cargo test -p sakura-settings --lib update::`. `Expect:` script passes with unchanged fixture output; 9 trust tests pass.
6. **Split `updater.rs` by trait impl.** `update/{flow,http,digest,authenticode,installer}.rs`. `Verify:` `cargo test -p sakura-settings --lib update::flow`; `pwsh ./ci/test-update-signing-v2.ps1`. `Expect:` 16 updater tests pass; no new `windows` feature needed.
7. **Extract `ui/model.rs`.** Move `ui.rs:3397-3853` plus 12 mapping tests. `Verify:` `cargo test -p sakura-settings --lib ui::model` (after step 9) or `--bin sakura_settings_payload model`. `Expect:` same 12 tests, ui.rs down ~460 lines.
8. **Make `ui/presentation.rs` a leaf.** Move control factory (`:2912-3227`) and helpers (`:3228-3396`) into it; delete `use super::*`. `Verify:` `rg -c 'use super::\*' crates/sakura-settings/src/ui/presentation.rs` → 0; `cargo build -p sakura-settings`. `Expect:` zero, clean build.
9. **`ui.rs` → `ui/mod.rs` + topic registry.** Replace `show_topic_controls` (`:1162-1296`) with a table and `handle_command` (`:869-1004`) with per-topic `on_command`. Risk: medium — visibility and focus order covered only by ignored desktop tests. **Owner decision:** run `settings_topic_user32.rs` (17 ignored tests) manually as the gate. `Verify:` `cargo test -p sakura-settings --test settings_topic_user32 -- --ignored` on a desktop. `Expect:` 17 pass, including `tab_focus_order_skips_hidden_topics_and_ends_at_actions`.
10. **Move topics out one at a time**, easiest first (diagnostics, learning, update, dictionary, then the eleven input topics). `Verify:` `cargo build -p sakura-settings && cargo test -p sakura-settings`; `wc -l crates/sakura-settings/src/ui/mod.rs`. `Expect:` mod.rs shrinks monotonically toward <600 lines; no test name changes.
11. **Split `pad.rs`.** `pad/layout.rs` (`:173-685` + 14 tests), `pad/model.rs` (`:728-784,2336-2831`), `pad/paint.rs` (`:1963-2335`), `pad/window.rs` (`:785-1961,2833-3141`). `Verify:` `cargo test -p sakura-renderer --lib pad::layout`. `Expect:` the geometry tests pass without a desktop.
12. **Per-crate CI jobs.** Add `cargo test -p sakura-renderer --lib`, `-p sakura-settings --lib`, `-p sakura-store` alongside the workspace run. **Owner decision:** scheduled desktop job for the 27 ignored tests. `Verify:` `pwsh ./ci/run-test-quiet.ps1 -Name 'renderer lib' -Command { cargo test -p sakura-renderer --lib }`. `Expect:` one `PASS:` line per job.
