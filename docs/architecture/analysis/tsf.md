# `crates/sakura-tsf` structural analysis (HEAD a370477, read-only)

All counts measured on the worktree. "Production lines" = total minus every `#[cfg(test)]` item (brace-balanced scan).

## 1. Module inventory

| File | total | prod | fn | pub fn | `unsafe {` | Responsibility |
|---|---:|---:|---:|---:|---:|---|
| `src/lib.rs` | 39 | 37 | 0 | 0 | 0 | Crate doc (no panic / no spawn / thin) + 18 `mod` declarations. |
| `src/text_service.rs` | 9,192 | 6,939 | 323 | 1 | 72 (68 prod) | God object: the `TextService` COM class and everything it coordinates. |
| `src/engine.rs` | 2,572 | 1,255 | 79 | 26 | 1 | Named-pipe conversation with `sakura_engine.exe`; budgets, reconnect, resync, session effects. |
| `src/composition.rs` | 1,941 | 1,027 | 59 | 8 | 36 | Inside-the-lock document mutation: ranges, preedit write, delete-before-caret, geometry, display attributes. Generic over `Authority`. |
| `src/write_coordinator.rs` | 1,271 | 741 | 72 | 38 | 0 | COM-free write journal: admission, ordering, epochs, revisions, UI leases, exactly-once terminals. |
| `src/mode_item.rs` | 895 | 697 | 34 | 13 | 15 | `GUID_LBI_INPUTMODE` language-bar item state, menu model, BGRA→HICON assets. |
| `src/conversion_key.rs` | 589 | 292 | 54 | 24 | 0 | COM-free Dual-TSF arbitration: IME seat, live-composition table, candidate-End authority, claim tokens. |
| `src/diagnostic_ring.rs` | 563 | 354 | 16 | 5 | 1 | Fixed 64-slot, allocation-free, content-free metadata ring (#35). |
| `src/candidate_ui.rs` | 495 | 337 | 25 | 4 | 11 | UI-less `ITfCandidateListUIElement` + `CandidateUi` lifecycle. |
| `src/engine_recovery_tests.rs` | 432 | 432 | 15 | 0 | 0 | **Test-only file living in `src/`**; oracle + fake-API PBT for `engine_recovery`. |
| `src/key_handler.rs` | 344 | 244 | 17 | 1 | 5 | Win32 VK/scan/layout → `sakura_proto::KeyInput` translation only. |
| `src/display_attributes.rs` | 314 | 262 | 20 | 2 | 4 | Three named preedit styles via `ITfDisplayAttributeProvider`. |
| `src/edit_session.rs` | 311 | 207 | 13 | 3 | 2 | `ITfEditSession` wrapper; sync-first then async fallback. |
| `src/reconversion.rs` | 251 | 187 | 17 | 1 | 3 | Immutable COM candidate snapshots for `ITfFnReconversion`. |
| `src/exports.rs` | 154 | 154 | 11 | 8 | 4 | The five DLL entry points, module handle, live-object count. |
| `src/engine_recovery.rs` | 121 | 117 | 9 | 9 | 0 | COM-free one-owner recovery fence for engine-timeout finalizers. |
| `src/ai_wait_cursor.rs` | 109 | 75 | 6 | 2 | 6 | Thread-local `IDC_APPSTARTING` cursor while an AI job is in flight. |
| `src/class_factory.rs` | 71 | 71 | 4 | 1 | 2 | `IClassFactory` constructing `TextService`. |
| `src/callback_deadline.rs` | 63 | 37 | 4 | 2 | 0 | Thread-local RAII IPC allowance shared by nested COM re-entry (#134). |
| `src/bin/sakura_tsf_test_host.rs` | 218 | 218 | 4 | 0 | 18 | `e2e-host`-gated Win32 EDIT host with no Sakura logic. |
| `tests/prediction_navigation_user32.rs` | 839 | 839 | 33 | 0 | 38 | `e2e-host`-gated, all `#[ignore]`d: real profile + synthesized keys + UIA (#37). |

Inline `#[test]` totals: text_service 58, engine 28, write_coordinator 24, composition 23, conversion_key 23, mode_item 11, key_handler 9, diagnostic_ring 5, engine_recovery_tests 5, display_attributes 4, edit_session 4, candidate_ui 3, reconversion 1, ai_wait_cursor 1, callback_deadline 1.

## 2. Intra-crate dependency graph

- `text_service` → `ai_wait_cursor`, `candidate_ui::CandidateUi`, `composition::{self,DocumentEdit,Update}`, `conversion_key`, `diagnostic_ring`, `display_attributes`, `edit_session`, `engine`, `engine_recovery`, `exports::{on_object_created,on_object_destroyed}`, `key_handler`, `mode_item::{self,MenuCommand}`, `reconversion`, `write_coordinator`, plus inline `crate::callback_deadline` (`text_service.rs:6809,6814,6845`) — **15 sibling modules**.
- `engine` → `callback_deadline` (`engine.rs:37`; inline at 277/311/340/396/448/537/573/619/680/722/776/823/897/909/981/1021), `crate::exports` (1×).
- `class_factory` → `text_service::TextService`, `exports::{on_object_created,on_object_destroyed}`.
- `exports` → `class_factory::TextServiceFactory` (`exports.rs:15`).
- `engine_recovery_tests` (test-only) → `engine_recovery`.
- Every other module (`composition`, `write_coordinator`, `conversion_key`, `candidate_ui`, `mode_item`, `key_handler`, `display_attributes`, `edit_session`, `reconversion`, `diagnostic_ring`, `engine_recovery`, `callback_deadline`, `ai_wait_cursor`) has **zero** intra-crate `use`.

**Cycles:** exactly one, trivial: `exports.rs:15` ↔ `class_factory.rs` (`use crate::exports::{on_object_created, on_object_destroyed}`).

**Hubs / god clients:** `text_service` is the sole god client (15 of 18 modules). `callback_deadline` is the only shared sink with two importers. The graph is a star: one hub, 13 leaves, one 3-node DLL-entry cluster. **Coupling is not this crate's problem — mass in the hub is.**

## 3. `text_service.rs` anatomy

### 3.1 `impl` blocks

| Lines | Block | ~size |
|---|---|---:|
| 269–309 | `impl CompositionWriteOwner` | 41 |
| 323–335 | `impl Default for CompositionState` | 13 |
| 595–673 | `impl PhysicalKeyOwner` (`of`, `terminal_eaten`, `journal_replacement_applies`, `decide_real_fence`, `decide_probe_fence`) | 79 |
| 695–739 | `impl<T> …DeferredWork<T>` | 45 |
| 746–777 | `impl<T> DeferredDispatchState<T>` | 32 |
| 795–800 | `impl FocusFinalizationPhase` | 6 |
| 814–871 | `impl DeferredState` | 58 |
| 923–980 | `impl LayoutState` | 58 |
| **1057–4060** | **`impl TextService`** (inherent, ~200 methods) | **3,004** |
| 4061–4069 | `impl Drop for TextService` | 9 |
| 4072–4084 | `impl Drop for SyncClaimOnExit<'_>` | 13 |
| 4085–4560 | module-level free functions (window proc, scope classification, adjacency proofs) | 476 |
| **4563–6366** | **`impl TextService_Impl`** (generated wrapper; deferred dispatch + write pipeline) | **1,804** |
| 6367–6376 | `ITfTextInputProcessor_Impl` | 10 |
| 6377–6412 | `ITfTextInputProcessorEx_Impl` | 36 |
| 6413–6486 | `ITfLangBarItem_Impl` | 74 |
| 6487–6567 | `ITfLangBarItemButton_Impl` | 81 |
| 6568–6586 | `ITfSource_Impl` | 19 |
| 6587–6610 | `ITfFunctionProvider_Impl` | 24 |
| 6611–6616 | `ITfFunction_Impl` | 6 |
| 6617–6769 | `ITfFnReconversion_Impl` | 153 |
| 6770–6858 | `ITfKeyEventSink_Impl` | 89 |
| 6859–6892 | `ITfCompositionSink_Impl` | 34 |
| 6893–6920 | `ITfTextLayoutSink_Impl` | 28 |
| 6921–6938 | `ITfDisplayAttributeProvider_Impl` | 18 |
| 6940–9192 | `mod tests` | 2,253 |

**The actual COM vtable surface is only 572 lines (6367–6938).** 4,808 lines (1057–6366) are non-COM coordination that merely lives behind `&self`.

### 3.2 Fields of `TextService` (`text_service.rs:994–1056`), grouped

*Lifecycle:* `activation: RefCell<Option<Activation>>` (2 `try_borrow_mut`, 2 `try_borrow`), `focus_foreground: Cell<bool>` (8 sites), `instance_id: u64` (9), `claim_generation: Cell<u64>` (3).

*Composition/write (invariant core):* `composition: RefCell<CompositionState>` (12 `borrow_mut`, 8 `borrow`, 4 `try_borrow`, 3 `try_borrow_mut` = **27 borrow sites**), `writes: RefCell<WriteCoordinator<PendingWrite>>` (16 `try_borrow_mut`, 7 `borrow`, 5 `borrow_mut`, 1 `try_borrow` = **29**), `undo_terminalization: Cell<Option<UndoCommitOutcome>>` (8), `engine_recovery: Cell<EngineRecoveryFence>` (8), `document_adjacency: RefCell<Option<DocumentAdjacency>>` (6) + `document_adjacency_must_reset: Cell<bool>` (7).

*Engine:* `engine: RefCell<Engine>` (19 `try_borrow_mut`, 7 `borrow`, 3 `borrow_mut` = **29**), `cached_input_scope: Cell<…>` (4).

*Candidate board / layout:* `candidate_ui: RefCell<CandidateUi>` (2), `layout: RefCell<LayoutState>` (**11**), `layout_abandon_pending: Cell<…>` (7), `candidate_operation_active: Cell<bool>` (2), `candidate_end_pending: Cell<bool>` (9).

*Deferred dispatch:* `deferred: RefCell<DeferredState>` (18 `try_borrow_mut`, 10 `borrow_mut`, 10 `borrow`, 2 `try_borrow` = **40 — the single most shared field**), `focus_gain_reconciliation_pending: Cell<bool>` (6).

*AI text:* `ai_text: RefCell<AiTextState>` (2), `ai_key_latched: Cell<bool>`, `last_ai_error: RefCell<Option<String>>` (2).

*UI chrome:* `mode_item: mode_item::ModeItemState` (12), `category_mgr: RefCell<Option<ITfCategoryMgr>>` (1).

File totals: **11 `RefCell<…>` fields / 32 `RefCell` mentions, 22 `Cell<…>`, 128 `borrow_mut()`, 95 `try_borrow_mut()`, 52 `try_borrow()`, 34 `borrow()`, 72 `unsafe` blocks (68 production)**. `deferred`, `writes`, `engine`, `composition` are each touched by 25–40 sites across five responsibility clusters — that is the hidden coupling channel, not `use` edges.

### 3.3 Responsibility clusters

- **Activation / lifecycle** — `attach` 2362, `detach` 2524, `warm_up` 2607, `Activate/ActivateEx/Deactivate` 6368–6412, `preserve_registered_key` 205, `create/destroy_deferred_window` 1088/1141, `Drop` 4061. ~500 lines.
- **Key handling & arbitration** — `handle_key` 5223, `handle_key_input` 5238, `conversion_key_disposition` 1550, `claim_token`/`sync_live_claim`/`retire_live_claim` 1517–1530, `host_eaten` 1539, `PhysicalKeyOwner::*` 595–673, `OnTestKeyDown/OnKeyDown/OnKeyUp/OnPreservedKey` 6803–6858. ~700 lines.
- **Composition writing / journal** — `reserve_write` 2166 … `settle_applied_write` 6263, `submit_output` 5663, `submit_pending_write` 5709, `request_next_write` 5761, `apply_queued_write` 5875, `plan` 3976 / `plan_from_visible` 3998, `begin/commit/fail/cancel_composition_write` 3711–3975. ~1,600 lines — largest cluster.
- **Candidate board & layout** — `show_candidates` 5014, `request_candidate_layout` 5080, `end_candidates` 3290, `ensure_layout_subscription` 3354, `begin/abandon/terminalize/complete_layout_query` 3459–3630, `LayoutState`/`DeferredState` impls. ~900 lines.
- **Mode indicator / language bar** — `sync_mode_item` 2630, `refresh_mode_item_for_focus` 2653, `select_mode_menu_command` 2670, `publish_idle_mode_anchor` 3631, `ITfLangBarItem*_Impl` 6413–6567. ~350 lines.
- **Engine connection / recovery / deadline** — `ask` 2837, `ask_probe` 2845, `disconnect` 2823, `recover_from_engine_unavailable` 5536, `begin/finish/cancel_engine_recovery` 1450–1476, `settle_engine_recovery_completion` 2347. ~400 lines.
- **Reconversion** — `ask_reconversion` 3133, `QueryRange`/`GetReconversion`/`Reconvert` 6618–6769. ~200 lines.
- **AI text** — `ai_trigger_matches` 2901 … `start_ai_text_request` 3053, `dispatch_ai_text_poll` 4663, `apply_completed_ai_text` 4841, timers 1186/1202. ~500 lines.
- **Diagnostics** — `record_diagnostic_write` 1884, `record_diagnostic_lifecycle` 1912, `diagnostic_cancel_code` 4137. ~60 lines.
- **Display attributes / input scope** — `classify_input_scope_with_cookie` 4197 … `input_scope_priority` 4341, `EnumDisplayAttributeInfo` 6922. ~200 lines.
- **Test support** (7362–7420 helpers, five `fake_engine_*` pipe servers) and **tests** (6940–9192).

### 3.4 Functions > 120 lines

`apply_queued_write` `text_service.rs:5875` ~292 · `handle_key_input` `:5238` ~291 · `attach` `:2362` ~162 · `request_candidate_layout` `:5080` ~143 · `apply_completed_ai_text` `:4841` ~133 · (test) `converting_space_keeps_a_live_composition_across_context_replacement` `:7832` ~137. **No other file in the crate has a function over 120 lines.**

## 4. COM interface surface

`#[implement(...)]` sites (9):

| Attr | File:line |
|---|---|
| `ITfTextInputProcessorEx, ITfKeyEventSink, ITfCompositionSink, ITfDisplayAttributeProvider, ITfTextLayoutSink, ITfFunctionProvider, ITfFnReconversion, ITfLangBarItem, ITfLangBarItemButton, ITfSource` (10 interfaces on one object) | `text_service.rs:981` |
| `ITfCandidateListUIElement` | `candidate_ui.rs:30` |
| `IClassFactory` | `class_factory.rs:14` |
| `ITfDisplayAttributeInfo` | `display_attributes.rs:98` |
| `IEnumTfDisplayAttributeInfo` | `display_attributes.rs:173` |
| `ITfEditSession` | `edit_session.rs:61` |
| `ITfCandidateString` | `reconversion.rs:21` |
| `ITfCandidateList` | `reconversion.rs:47` |
| `IEnumTfCandidates` | `reconversion.rs:86` |

DLL exports (`exports.rs`): `DllMain` :52, `DllGetClassObject` :75, `DllCanUnloadNow` :104, `DllRegisterServer` :117, `DllUnregisterServer` :123, with `MODULE_HANDLE` :24 and `LIVE_OBJECTS` :29. Registration is delegated: `exports.rs:127 register()` → `sakura_reg::register_all(…)` (`crates/sakura-reg/src/lib.rs:52`); `sakura-reg` owns CLSID/profile/category registry writes (`com_server.rs:28`, `profile.rs`, `guids.rs`). `class_factory.rs:31 IClassFactory_Impl::CreateInstance` builds `TextService` and bumps the live-object count.

## 5. State machines and invariants

### 5.1 Write coordinator — ticket / epoch / journal authority (#52)
Owner `write_coordinator.rs` (`WriteCoordinator<T>`, `Epoch` :72, `Ticket` :94, `UiLease` :112, `Phase` :132). Transitions: `activate` :224, `deactivate` :238, `focus_changed` :247, `composition_terminated` :253, `document_changed` :265, `observe_context` :274, `reserve` :339, `attach` :372, `begin_head` :433, `validate_callback` :482, `complete_applied` :512, `reject` :523, `cancel_ticket` :537, `cancel_all` :545, `abandon_projection` :562, `adopt_ui_lease` :572, `validate_ui_lease` :588, `clear_ui_lease_if_current` :606.
Invariants: `:6` "lifetime epochs, document revisions, and **exactly-once terminal outcomes**"; `:179` "at most one unresolved reservation, so a re-entrant key cannot create a gap"; `:264` "Every queued base revision is now **stale**"; `:308` "the real callback **must**…"; `:319` "boundary must fence all later input"; `:461` "the current head's **authority** without changing its phase"; `:479` "**must** be called before *any* document or UI access from a queued"; `:561` "advances the revision so an old callback **cannot regain authority**"; `:604` "Failed deferred work may retire **only the lease it still owns**"; `:672` "Only the head can become terminal through a request/callback"; `:1097` "A malformed callback terminal **must leave the reservation available**".
Mirror in `text_service.rs`: `can_admit_write_for_context` :2135, `ui_lease_is_current` :2157, `validate_pending_write` :4564, `settle_applied_write` :6263, plus `:357`, `:378` "journal outcome, **never** merely when the provider returned a result", `:466` "A malformed/stale ticket **must not** leave a later payload retryable".

### 5.2 Composition state
Owner `text_service.rs:311 CompositionState` + `:263 CompositionWriteOwner` (`begin` :270, `owns` :283, `finish` :287, `lifecycle_is_current` :300, `invalidate` :304) + `:239 CompositionFlight` / `:249 CompositionIdentity` / `:252 ExpectedSelfTermination`. Document side is `composition.rs` (`start` :545, `write_text` :645, `clear_and_end` :692, `end_composition` :717, `insert_plain` :765, `delete_before_caret` :423).
Invariants: `text_service.rs:10` "`CompositionState::text` is no longer the preedit — it is a record of what the document is currently *showing*"; `:24` "**No `RefCell` borrow is ever held across a call into TSF**… a double borrow is the host application dying"; `:260` "callback can **never** put its retained handle back after the lifecycle event"; `:298` "A lifecycle invalidation advances the **epoch** before"; `:3859` "**never** uses it as if its text still matched a speculative projection". The Electron/VS Code path is pinned by `composition.rs:1517 electron_document_path_never_queries_insert_at_selection`.

### 5.3 Engine recovery / timeout fence
Owner `engine_recovery.rs` (`RecoveryToken` :13, `RecoveryStart` :26, `EngineRecoveryFence` :73), held as `TextService.engine_recovery: Cell<EngineRecoveryFence>` (`:1026` "It lives outside the write RefCell so re-entrant key probes can **fail closed** without borrowing the journal currently in a callback"). Transitions: `begin_engine_recovery` :1450, `finish_engine_recovery` :1457, `cancel_engine_recovery` :1463, `engine_recovery_pending` :1469, `engine_recovery_disposition` :1473, `settle_engine_recovery_completion` :2347, `recover_from_engine_unavailable` :5536.
Invariants: `engine_recovery.rs:5` "old visible composition can **only** be finalized through an asynchronous edit session"; `:66` "from an older lifecycle **must not** clear the authority of a newer recovery"; `text_service.rs:1649` "fail closed and keep this key consumed until that owner finishes"; `:5315` "A borrowed fence cannot be proven open. **Fail closed** as the".

### 5.4 Callback deadline (#134)
Owner `callback_deadline.rs` (thread-local `CURRENT: Cell<Option<Instant>>` :7, `CallbackDeadline::enter` :18, `Drop` :27, `limit` :33). Entered at exactly three places: `text_service.rs:6809` (`OnTestKeyDown`), `:6814` (`OnKeyDown`), `:6845` (`OnPreservedKey`). Consumed at 16 sites in `engine.rs`. Doc `engine.rs:10-17` "The key callback owns one 50 ms IPC deadline… the **deadline** prevents renewed serial IPC waits"; `:1004` "The budget is one deadline shared by three round trips". `limit` takes `min(parent, own)` so nested re-entry can never extend it.

### 5.5 Candidate UI lease / teardown (#7, #142)
Split across `candidate_ui.rs` (`show_or_update` :228, `renderer_visible` :309, `is_active` :320, `end` :326) and `text_service.rs` (`begin_candidate_operation` :1399, `finish_candidate_operation` :1407, `end_candidates` :3290, `run_candidate_teardown_host_calls` :968, `show_candidates` :5014). Exclusion domain: `candidate_operation_active` + `candidate_end_pending` `Cell`s.
Invariants: `candidate_ui.rs:165` "state that **must** remain available until"; `:178` "one **independently re-entrant** candidate host call"; `:325` "**re-entrant callbacks observe the terminal inactive state**"; `text_service.rs:674` "an inner dispatch must **never** replace the controller temporarily"; `:1013` "If an end request cannot enter `DeferredState`, detach/drop remains its explicit lifecycle owner"; `:4626` "A **stale** deferred candidate payload **must not** tear down a newer"; `:3362` "**stale** work must not replace its lease or reset its"; `:880` "`QueryQueued` has **exactly one owner**"; `:815` "Gives **exactly one** hidden-window message ownership".

### 5.6 Physical-key arbitration (Dual TSF, #69)
Owner `conversion_key.rs` (`ImeSeat` :21, `ProcessSoleIme` :47, `ConversionKeyDisposition` :140, `ClaimToken` :222, `LiveCompositionTable` :236, `ProcessLiveCompositions` :268) + `text_service.rs:589 PhysicalKeyOwner` (`of` :596, `terminal_eaten` :605, `journal_replacement_applies` :618, `decide_real_fence` :626, `decide_probe_fence` :651).
Invariants: `conversion_key.rs:4` "it **never** talks to the engine"; `:7` "**must not** claim the first letter of a Japanese composition"; `:29` "**Only** the elected IME may send keys"; `:67` "Focus loss releases it **only if** we hold it"; `:177` "Host-eligible failure **must** still eat Japanese typing"; `:189` "IME-on letters **must not** fall through as direct input"; `:201` "Shared candidate UI may End **only** from the instance that owns the popup"; `:205` "An idle peer with no lease **must not**"; `:215` "Numeric claim only. **Never** store COM objects here"; `text_service.rs:584` "the host must **never** receive"; `:5375`; `:5433` "must not become WM_CHAR".

### 5.7 AI text polling (#58)
Owner `text_service.rs` (`AiTextState` :418, `PendingAiText` :409, `AiTextTarget` :403) + `engine.rs` (`start_ai_text` :296, `poll_ai_text` :331, `cancel_ai_text` :387). 50 ms `WM_TIMER` (`AI_TEXT_TIMER_ID` :99, `AI_TEXT_POLL_MS` :100, `arm_ai_text_timer` :1186, `dispatch_ai_text_poll` :4663, `apply_completed_ai_text` :4841, `terminalize_pending_ai_record` :3022, `cancel_pending_ai` :3037). `:4979` "terminal owner. It **must not** send a second engine commit".

### 5.8 Deferred dispatch / focus reconciliation (cross-cutting)
`DeferredState` :802, `DeferredWork<T>` :677, `FocusFinalizationPhase` :779, `LayoutState` :912. `:839` "A queued retry **must not** disappear if its message cannot be delivered"; `:1405` "an older operation can **never** overwrite a"; `:1655` "A focus-gain callback **must never** return with a pre-engine focus-loss"; `:1786` "Schedules **exactly one** document-free retirement retry"; `:2326` "Transfer **exactly one** later document-free"; `:2375` "The activation transaction owns **exactly one** preserved physical key"; `:2464` "partial registration **must never** become a permanent stale item"; `:2534` "Detach is the final lifecycle owner even if a re-entrant journal".

## 6. Formal-verification links

| Model | cfgs | Record doc | Runner | In CI? | Rust correspondence |
|---|---|---|---|---|---|
| `DualTsfCandidateBoard.tla` | 7 | `verification/dual-tsf-candidate-board/tla-record.md` (79 ln) | `scripts/verify-dual-tsf-candidate-board-tlc.ps1` | **No** | **Prose**, one line (`:48`): "`submit_output` maps `output.candidates = None` to `CandidateEffect::Hide`, then `clear_ui_lease` + `queue_end_candidates`." Constants `GuardForeignCandidateEnd` and `RestoreCurrentPlacement` recorded **not implemented** (`:19`,`:20`). |
| `DualTsfPhysicalKeyArbitration.tla` | 8 | `.../dual-tsf-physical-key-arbitration/tla-record.md` (69 ln) | `scripts/verify-dual-tsf-physical-key-arbitration-tlc.ps1` | **No** | **Prose, file-level only** (`:26`): "Product correspondence: `crates/sakura-tsf/src/conversion_key.rs`." Plus `:52` "`PhysicalKeyOwner::of` sees only `has_live_composition()`". No per-action table. |
| `TsfPredictingSpace.tla` | 2 | `.../tsf-predicting-space/tla-record.md` (59 ln) | `scripts/verify-tsf-predicting-space-tlc.ps1` | **No** | **Prose bullets** (`:34-38`): "`RetargetLive = TRUE` ↔ TSF `live_convert_context` still finds the reading's document … and `ask()` converts"; "`RetargetLive = FALSE` ↔ KeyDown absorbs Space without an engine convert". |
| `TsfProbeHostInsert.tla` | 2 | `.../tsf-probe-host-insert/tla-record.md` (26 ln) | `scripts/verify-tsf-probe-host-insert-tlc.ps1` | **No** | **Prose bullets** (`:19-24`): "`LocalClaim = TRUE` ↔ TSF `PhysicalKeyOwner::Ime` returns eaten before `ask_probe`"; "`LocalClaim = FALSE` ↔ `OnTestKeyDown` Probe timeout returns FALSE"; "`engineDesynchronized` ↔ `Link::desynchronized`". |
| `EngineRecovery.tla` | 4 | `verification/tla/README.md` (72 ln); cited by `verification/reports/issues-56-57-verification.md` | `scripts/verify-engine-recovery-tlc.ps1` | **No** | **Absent.** README `:3` says "implementation-independent"; no Rust names. Stand-in is the independent oracle at `src/engine_recovery_tests.rs:17`. |
| `SpaceKeyDispatch.tla` | 9 | `verification/space-key-dispatch/` (traceability.json, correspondence-and-audit.md, mutation-report.md) | `scripts/verify-space-key-dispatch-tlc.ps1` | **Partially** — `.github/workflows/ci.yml:76` runs `-SelfTest` only (runner/process-lifecycle self-test, not a TLC search) | Machine-checked traceability exists, but targets dispatch in `sakura-core`, not the DLL. |
| `AiTextLifecycle.tla` | 3 | `verification/ai-text-verification.md:47` | none in `scripts/` | **No** | Explicitly **none**: "実装の型・関数を参照せず". |

Flat records: `write-journal-authority.md` (29 ln, #52, DEFENSE_IN_DEPTH; criteria are `cargo test -p sakura-tsf --lib write_coordinator::tests::authority_` etc., no TLA+ model); `tsf-callback-deadline.md` (34 ln, #134; "TLC … remain unfinished"); `high-load-input-integrity.md` (600 ln, #148 — §2.1 is a stage-by-stage `path:line` table of one keystroke, the best existing map of the key path); `revalidation-20260905.md` (65 ln) classifies **T3 (#57, #69, #7) as HYPOTHESIS — "product reachability/COM teardown verification pending"** and **T4 (#102, #107) as HYPOTHESIS — "no current ETW attribution"**; `issue-141-tdd.md` (214 ln, unissued-expiry → `Rejected`).

**Correspondence status: prose in four TSF models, absent in two, machine-checked in none.** No TLC runner executes in CI. No DLL-size gate in CI either — the ≤ 1 MiB check lives in `scripts/verify-phase1.ps1:258`, which no workflow calls.

Cross-check of claimed names: `live_convert_context` → `text_service.rs:3242` ✅ · `ask_probe` → `text_service.rs:2845` ✅ · `PhysicalKeyOwner::of` → `text_service.rs:596` ✅ · `clear_ui_lease_if_current` → `write_coordinator.rs:606` ✅ · `submit_output` → `text_service.rs:5663` ✅. **`Link::desynchronized` does NOT exist** — `rg 'fn desynchronized'` returns nothing; the real name is `Engine::is_desynchronized` (`engine.rs:812`). The `tsf-probe-host-insert` correspondence line is stale.

## 7. Test topology

- **Pure logic, no COM / desktop / engine** (already isolated): `write_coordinator` (24), `conversion_key` (23), `callback_deadline` (1), `diagnostic_ring` (5), `key_handler` (9), `engine_recovery_tests` (5), `mode_item` asset-selection subset, `ai_wait_cursor` (1).
- **Trait-mocked host calls, no real COM**: `composition` (23) — generic over `Authority`, with `checked_host_call` (`composition.rs:301`) and per-operation seams (`:321`, `:342`, `:364`, `:472`). Two tests (`:994`, `:1052`) create a process-local Win32 EDIT `HWND`.
- **Real COM object construction (in-process)**: `candidate_ui` (3), `display_attributes` (4), `edit_session` (4), `reconversion` (1), parts of `mode_item`.
- **Real named pipe + spawned thread (fake engine)**: `engine` (28, via `fake_engine` `engine.rs:1355`) and `text_service` (58, via five `fake_engine_*` helpers at `:6961`, `:7020`, `:7098`, `:7211`; pipe imports at `:6944`). The slow tests.
- **Live desktop + installed profile + real engine + renderer**: `tests/prediction_navigation_user32.rs` only — `#![cfg(all(windows, feature = "e2e-host"))]` (`:1`), all four tests `#[ignore]`d (`:89`, `:169`, `:260`). Drives `src/bin/sakura_tsf_test_host.rs`, which deliberately contains no Sakura logic (`:3-9`).
- `src/engine_recovery_tests.rs`: 5 tests plus an *independent oracle* (`:17 oracle_disposition`, `:28 oracle_finish`) and a fake API (`:112-208`); doc `:14` says it "intentionally does not call or mirror the production methods". It is in `src/` because it is `#[cfg(test)] mod engine_recovery_tests;` (`lib.rs:31`) — a `cdylib` has no integration harness for crate-private items, so a unit-test module is the only route to `engine_recovery`'s `pub(crate)` API. The name is misleading and 432 test lines sit at the same level as 18 production modules.

CI runs one `cargo test --workspace` (`.github/workflows/ci.yml:80`); there is no per-crate job, so a `sakura-tsf` change pays for the whole workspace.

## 8. Findings

1. **`text_service.rs` is a 6,939-production-line god object with ten COM interfaces on one struct** (`:981`), yet only 572 lines are actual vtable methods (6367–6938). *Boundary:* keep the `#[implement]` struct; move the 4,808 lines of coordination (1057–6366) into COM-free modules it delegates to — no new allocation or indirection on the key path.
2. **`deferred: RefCell<DeferredState>` is a 40-site hidden coupling channel** (`:807`) touched by candidates, focus, layout, AI and writes. *Boundary:* `DeferredState` gets its own module with a typed work enum, so "why did my message get dropped" is one 200-line file.
3. **Change reasons are mixed at file granularity**: `text_service.rs` touched by 27 of the last 200 commits, `engine.rs` 22, everything else ≤ 9. Nearly every issue (#7, #52, #57, #69, #102, #134, #142, #148) lands in one file. *Boundary:* churn concentrates in three clusters (write pipeline, key arbitration, candidate board) with no code-level reason to share a file.
4. **`apply_queued_write` (`:5875`, ~292 ln) and `handle_key_input` (`:5238`, ~291 ln) each span 4–5 invariant domains** (journal, fence, arbitration, adjacency, candidates). *Boundary:* extract the pure decision — `PhysicalKeyOwner`/`decide_real_fence` (595–673) already prove it is extractable — from the COM effects.
5. **Journal authority is duplicated across the RefCell boundary.** `write_coordinator.rs:482 validate_callback` is the authority, but `text_service.rs:4564`, `:2135`, `:2157` re-encode fragments, each with its own fail-closed borrow path (`:1497` "Fail closed: if composition or the journal cannot be inspected"). *Boundary:* one façade owning both `composition` and `writes`, so a borrow failure is decided once.
6. **The re-entrancy exclusion domain is spread over five bare `Cell` bits** — `candidate_operation_active` (:1005), `candidate_end_pending` (:1013), `focus_gain_reconciliation_pending` (:1017), `layout_abandon_pending` (:1000), `undo_terminalization` (:1022) — each with a paragraph explaining why it is *not* in the `RefCell`. Exactly the #7 hazard surface. *Boundary:* one `ReentryLatches` struct in one file with exhaustive tests.
7. **`src/engine_recovery_tests.rs` (432 lines, 100% test) sits among production modules** and is named for a 121-line module. An agent grepping `src/*.rs` cannot tell it is a test file. *Boundary:* fold it under `engine_recovery/`.
8. **`text_service.rs` carries 2,253 lines of inline tests including five fake named-pipe engines** (`:6961`, `:7020`, `:7098`, `:7211`). Any agent reading the file for a 20-line fix pays for them. *Boundary:* one reusable fixture, not five.
9. **`engine.rs` grew 686 lines since 2026-09-01** and mixes transport (`Link`, `connect`, `open` :1007–1090), session policy (`session_effect` :1142, `update_mode_for_scope` :1185, `scope_is_sensitive` :1201) and eight feature-specific request wrappers. *Boundary:* transport+deadline vs. request catalogue.
10. **The `TsfProbeHostInsert` record cites a function that does not exist** (`Link::desynchronized`; real name `Engine::is_desynchronized`, `engine.rs:812`). Prose correspondence rots silently. *Boundary:* each model names one Rust module and the runner asserts it exists.
11. **No TLC model and no DLL-size gate run in CI.** `ci.yml:76` runs only `verify-space-key-dispatch-tlc.ps1 -SelfTest`; the ≤ 1 MiB assertion at `scripts/verify-phase1.ps1:258` is uninvoked. Both are cheap gates protecting the crate's two hardest-to-recover properties.
12. **`exports` ↔ `class_factory` is a needless 2-node cycle** (`exports.rs:15`). *Boundary:* a 20-line `live_objects.rs` leaf.
13. **`composition.rs` is the model the rest of the crate should copy** — 1,027 production lines fully generic over `Authority` with named host-call seams (`:301`, `:321`, `:342`, `:364`, `:472`), so 23 tests run with no COM. Nothing in `text_service.rs` uses this technique.
14. **`mode_item.rs` (697 prod ln, 9 commits) is cohesive, but its language-bar sink lives 6,000 lines away from the `ITfLangBarItem*_Impl` bodies at `text_service.rs:6413–6567`** — one feature, two files, opposite ends of a 9,000-line file.
15. **Names hide location.** `write_coordinator` is the journal; `conversion_key` is Dual-TSF arbitration; `engine_recovery` is a timeout fence; `mode_item` is the language bar. An agent given issue #69 cannot guess `conversion_key.rs` from the title.

## 9. Target structure proposal

Constraint check: every move below is a file move plus `pub(crate)` visibility — no new trait objects, no `Box`, no new allocation on the key path, no change to the single `#[implement]` struct. DLL layout and `opt-level = "z"` output are unchanged.

```
crates/sakura-tsf/src/
  lib.rs                       # module tree + crate invariants
  com/                         # (a) COM glue: translate TSF calls into commands, nothing else
    text_service.rs            # the #[implement] struct, its fields, the 12 trait impls (~1,100 ln)
    class_factory.rs
    exports.rs
    live_objects.rs            # breaks the exports<->class_factory cycle
    edit_session.rs
    display_attributes.rs
    reconversion.rs
    candidate_element.rs       # ITfCandidateListUIElement (from candidate_ui.rs)
  session/                     # (b) COM-free controller: the crate's correctness lives here
    mod.rs                     # SessionController facade: owns composition + writes + fence together
    key_arbitration.rs         # conversion_key.rs + PhysicalKeyOwner (text_service:589-673)
    write_journal.rs           # write_coordinator.rs
    write_pipeline.rs          # reserve->submit->apply->settle decisions (pure)
    recovery_fence.rs          # engine_recovery.rs
    callback_deadline.rs
    candidate_board.rs         # lease/teardown decisions + the 5 reentry latches
    deferred_work.rs           # DeferredState/DeferredWork/FocusFinalizationPhase
    layout_lease.rs            # LayoutState/LayoutQueryClaim/GeometryPhase
    input_scope.rs             # classify_input_scope_* (text_service:4197-4359)
    ai_text.rs                 # AiTextState machine (poll/apply/cancel decisions)
  host/                        # (c) adapters: perform the effects the controller decided
    composition.rs             # unchanged; already Authority-generic
    candidate_ui.rs            # CandidateUi effects
    language_bar.rs            # ITfLangBarItem*_Impl bodies + mode_item.rs
    mode_assets.rs             # BGRA/HICON selection (pure)
    key_translation.rs         # key_handler.rs
    wait_cursor.rs             # ai_wait_cursor.rs
    engine/{mod,link,requests}.rs
  diagnostics/ring.rs
  bin/sakura_tsf_test_host.rs
tests/prediction_navigation_user32.rs
```

One-way dependencies: `com/ → session/`, `com/ → host/`, `host/ → (nothing)` except the explicit `host/engine → session/callback_deadline` edge, and `session/ → (nothing)`. **`session/` must not `use windows::` at all** — mechanically checkable and simultaneously the formal-verification boundary. The architecture check allowlists only that exact host exception; it must reject any other `host → session` edge.

Test convention: `session/**` and `host/**` may use inline or sibling `*_tests.rs` private-unit tests, with no pipes/COM/threads in session tests and trait-injected host calls in host tests. `com/**` uses fake-engine pipe fixtures consolidated into one `com/testing.rs`. Desktop E2E stays in `tests/`.

Bound verification artifacts: `session/key_arbitration.rs` ← `DualTsfPhysicalKeyArbitration.tla` + `TsfProbeHostInsert.tla`; `session/candidate_board.rs` ← `DualTsfCandidateBoard.tla`; `session/recovery_fence.rs` ← `EngineRecovery.tla`; `session/write_journal.rs` ← `verification/write-journal-authority.md`; `session/callback_deadline.rs` ← `verification/tsf-callback-deadline.md`; `session/ai_text.rs` ← `AiTextLifecycle.tla`; `session/write_pipeline.rs` ← `TsfPredictingSpace.tla`.

### Reading set

| Issue shape | BEFORE | AFTER |
|---|---|---|
| #7 re-entrant candidate UI crash | clusters at `text_service.rs` 968, 1005–1016, 1399–1430, 3290–3353, 4626, 5014–5079 scattered through 9,192 lines (realistically the whole file is scanned) + `candidate_ui.rs` 495 → **~9,700** | `session/candidate_board.rs` ~450 + `host/candidate_ui.rs` ~340 + teardown call sites in `com/text_service.rs` ~120 → **~910** |
| #57 stale composition replay after key timeout | `text_service.rs` 6,939 prod + `write_coordinator.rs` 741 + `engine_recovery.rs` 117 + `engine_recovery_tests.rs` 432 → **~8,230** | `session/write_journal.rs` ~740 + `session/recovery_fence.rs` ~120 + `session/write_pipeline.rs` ~600 → **~1,460** |
| #69 Dual-TSF idle peer passes Space | `conversion_key.rs` 292 + `text_service.rs:584-675`, `:1517-1583`, `:5238-5528` — findable only by reading the hub → **~7,400** | `session/key_arbitration.rs` ~450 + `OnTestKeyDown/OnKeyDown` in `com/text_service.rs` ~90 → **~540** |
| #134/#142 shared deadline / placement timeout | `callback_deadline.rs` 37 + `engine.rs` 1,255 + layout/candidate paths ~900 inside 6,939 → **~8,230** | `session/callback_deadline.rs` ~40 + `host/engine/link.rs` ~500 + `session/layout_lease.rs` ~250 → **~790** |

Median reduction ≈ 8×, coming from *removing the need to scan a 9,192-line file*, not from adding layers: module count rises 19 → 28 but no call gains an indirection.

## 10. Migration sequence

Ordered by reading-cost reduction per unit of risk. One PR each; compiles at every step; pure move/rename unless noted.

**Step 0 — add the missing gates (no code move).** Add to `.github/workflows/ci.yml` a DLL-size assertion reusing `scripts/verify-phase1.ps1:258`'s rule, plus a `cargo test -p sakura-tsf --lib` job.
`Verify:` `ls -l target/x86_64-pc-windows-msvc/release/sakura_tsf.dll`; `./ci/run-test-quiet.ps1 -Name 'tsf lib' -Command { cargo test -p sakura-tsf --lib }`.
`Expect:` DLL ≤ 1,048,576 bytes; ~191 lib tests pass; every later step becomes measurable. Risk: none.

**Step 1 — `git mv src/engine_recovery_tests.rs src/engine_recovery/oracle_tests.rs`,** making `engine_recovery` a directory module; adjust `lib.rs:30-31`.
`Verify:` `cargo test -p sakura-tsf --lib engine_recovery`; `rg -n 'engine_recovery_tests' crates/sakura-tsf` → no hits.
`Expect:` 5 tests pass; the oracle is discoverable as an allowed sibling private-unit test and no shipped target imports it. Risk: nil.

**Step 2 — break the `exports`↔`class_factory` cycle.** Move `on_object_created`/`on_object_destroyed`/`LIVE_OBJECTS` (`exports.rs:29-48`) into `src/live_objects.rs`.
`Verify:` `rg -n 'use crate::exports' crates/sakura-tsf/src` → no hits; `cargo test -p sakura-tsf --lib`.
`Expect:` acyclic graph; `DllCanUnloadNow` behavior unchanged. Risk: nil.

**Step 3 — extract the five re-entry latches into `src/session/reentry_latches.rs`.** Regroup `candidate_operation_active`, `candidate_end_pending`, `focus_gain_reconciliation_pending`, `layout_abandon_pending`, `undo_terminalization` (`text_service.rs:1000-1024`) into one `#[derive(Debug, Default)]` field. No logic change.
`Verify:` `cargo test -p sakura-tsf --lib`; `rg -c 'Cell<' crates/sakura-tsf/src/text_service.rs` drops by 5; DLL size check.
`Expect:` all 58 text_service tests pass; DLL size unchanged ±2 KB. Risk: low.

**Step 4 — move `LayoutState`/`LayoutQueryClaim`/`GeometryPhase`/`LayoutSubscription` (`text_service.rs:872-980`) and `ensure_layout_subscription`…`complete_layout_query` (`:3354-3630`) to `session/layout_lease.rs` + `host/layout.rs`.** Pure decisions to `session`, `ITfContextView` calls stay in `host`.
`Verify:` `cargo test -p sakura-tsf --lib layout`; `rg -n 'use windows' crates/sakura-tsf/src/session/` → empty.
`Expect:` `stale_layout_proposal_…` (`:8330`), `stale_same_context_lease_…` (`:8339`), `matching_layout_abandonment_…` (`:8940`), `refused_layout_abandonment_…` (`:8950`) still pass. Risk: medium (#142's domain) — do it after Step 0.

**Step 5 — move `DeferredState`/`DeferredWork`/`DeferredDispatchState`/`FocusFinalizationPhase` (`:676-871`) to `session/deferred_work.rs`; hidden-window plumbing (`create_deferred_window` :1088, `post_deferred_work` :1254, window proc at :4085ff) to `host/deferred_window.rs`.**
`Verify:` `cargo test -p sakura-tsf --lib -- deferred candidate_operation focus_`.
`Expect:` `candidate_operation_holds_reentrant_deferred_work_until_controller_restore` (`:8792`) and `candidate_operation_does_not_post_again_when_work_already_has_a_message` (`:8830`) pass unchanged.

**Step 6 — split `engine.rs` into `host/engine/{mod,link,requests}.rs`.** `Link`, `connect`, `connect_to`, `open`, `resync`, `drop_link`, `left` → `link.rs`; the 20 request wrappers → `requests.rs`; `Engine` facade + `session_effect`/`scope_is_sensitive` stay in `mod.rs`.
`Verify:` `cargo test -p sakura-tsf --lib engine::`; `rg -n 'fn is_desynchronized' crates/sakura-tsf/src/host/engine/`.
`Expect:` 28 engine tests pass; also correct `verification/tsf-probe-host-insert/tla-record.md:23` to `Engine::is_desynchronized`.

**Step 7 — move `PhysicalKeyOwner` (`:589-675`) into `conversion_key.rs`, then move the file to `session/key_arbitration.rs`.** Both types already model one thing and are already COM-free.
`Verify:` `cargo test -p sakura-tsf --lib key_arbitration::`; `pwsh scripts/verify-dual-tsf-physical-key-arbitration-tlc.ps1`; `pwsh scripts/verify-tsf-probe-host-insert-tlc.ps1`.
`Expect:` 23+5 tests pass; TLC matches recorded counts (283/87/9 for `fix-3state`; 5/4 for `TsfProbeHostInsert-fix`). Update both records to the new path.

**Step 8 — move `write_coordinator.rs` → `session/write_journal.rs`; pull the pure decisions out of `apply_queued_write` (`:5875`), `submit_output` (`:5663`), `request_next_write` (`:5761`) into `session/write_pipeline.rs`,** leaving `ITfRange`/`ITfComposition` calls behind.
`Verify:` `cargo test -p sakura-tsf --lib write_journal:: write_pipeline::`; DLL size check.
`Expect:` all 24 journal tests including the four `authority_*` (`write_coordinator.rs:804,817,853,865`) pass; `verification/write-journal-authority.md:18-20` criteria re-run green. **Risk: high** (#52's domain, touches the composition/journal state machine).

**Step 9 — extract `session/candidate_board.rs`** (lease adoption, teardown ordering, the decision half of `run_candidate_teardown_host_calls`), leaving `host/candidate_ui.rs` with the COM half. **Risk: highest.**

**Step 10 — split the residual `text_service.rs`** into `com/text_service.rs` (struct + 12 trait impls + delegation) and `session/mod.rs` (`SessionController` owning `composition`+`writes`+`recovery_fence`); relocate the language-bar impl bodies (`:6413-6567`) beside `mode_item.rs` as `host/language_bar.rs`.
`Verify:` `wc -l crates/sakura-tsf/src/com/text_service.rs` ≤ 1,300; `./ci/run-test-quiet.ps1 -Name 'workspace tests' -Command { cargo test --workspace }`; DLL size.
`Expect:` workspace suite green; DLL ≤ 1 MiB.

### Steps blocked on diagnostic evidence

`CLAUDE.md` forbids speculative changes to the VS Code crash state machine, and `verification/revalidation-20260905.md` still classifies **T3 (#7, #57, #69) and T4 (#102, #107) as HYPOTHESIS** ("product reachability/COM teardown verification pending"; "no current ETW attribution").

- **Steps 8, 9 and 10 must NOT begin before a crash dump / diagnostic-ring capture for #7 exists.** They move the exact code (`write_coordinator`, candidate lease/teardown, the `TextService` field layout) whose current shape is the only recorded defense for T2/T3. A behavior-preserving move is still a move: if the crash is later reproduced against a different layout, the `verification/` evidence no longer points at readable code.
- **Steps 0–7 are safe now:** 0–2 change no production logic; 3–5 regroup fields and relocate already-cohesive types; 6–7 touch modules that are already COM-free or isolated and are covered by TLC configs runnable before and after.
- **Decisions and gates:** (i) D10 is decided: keep `session/` as a directory plus R3 through Phase 6 and reconsider a crate only in a later Issue; (ii) D8 is decided: make TLC blocking only after expected-counterexample normalization and all discovered configs are meaningfully green; (iii) D7 remains blocked on verified Issue #7 dump/report evidence. The 2026-08-02 filename/report mentioned in comments is not an attachment, so Steps 8–10 must not infer `GuardForeignCandidateEnd` / `RestoreCurrentPlacement` behavior from it.
