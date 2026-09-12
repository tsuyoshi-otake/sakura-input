# `crates/sakura-engine` — structural analysis (HEAD a370477)

All line numbers are from this worktree. "Prod" = total lines minus every `#[cfg(test)]` region (brace-matched).

## 1. Module inventory

### src/ (production modules)

| file | total | prod | fn | pub fn | responsibility |
|---|---|---|---|---|---|
| `dispatch.rs` | 16850 | **6409** | 339 | 19 | Request→Reply, key→action, composition edit, conversion orchestration, candidates, prediction, commit, AI text, render into `OutputBuf` |
| `learning.rs` | 3714 | **2362** | 133 | 23 | Bounded persistent personalization store + prediction history + process-wide epoch (`learning.rs:1`) |
| `input_history.rs` | 3632 | **1909** | 153 | 27 | Developer-mode interaction history: record format, DPAPI, writer thread, compaction, counters (`input_history.rs:1`) |
| `server.rs` | 3189 | **1940** | 98 | 18 | Accept loop, endpoint/instance pool, admission, startup gate, per-request serve loop (`server.rs:1`) |
| `session.rs` | 2584 | **1875** | 131 | 92 | Per-editing-session state + `SessionTable` (`session.rs:1`); pure, no Windows |
| `prediction.rs` | 2244 | **1041** | 90 | 14 | Process-wide bounded prediction worker + mailbox (`prediction.rs:1`) |
| `ui.rs` | 1937 | **1223** | 82 | 23 | Renderer-facing shared board: snapshots, revisions, candidate commit queue (`ui.rs:1`) |
| `dictionary.rs` | 1594 | **524** | 49 | 20 | Mapped dictionary image + bounded conversion-worker pool (`dictionary.rs:1`) |
| `long_conversion.rs` | 924 | **705** | 44 | 6 | Optional ONNX reranker bridge + runtime/process client (`long_conversion.rs:1`) |
| `ai_text.rs` | 735 | **556** | 30 | 7 | AI worker job lifecycle: owner, cooldown, cancel, detach (`ai_text.rs:1`) |
| `main.rs` | 607 | **490** | 13 | 0 | Process entry, CLI options, startup ordering (`main.rs:137` `run`) |
| `context_evaluation.rs` | 606 | **374** | 20 | 5 | Offline replay metrics for dormant context work (#34) |
| `configuration.rs` | 597 | **416** | 29 | 8 | Config load + appearance/config file watchers |
| `prediction_snapshot.rs` | 491 | **362** | 23 | 3 | Dormant hash-only top-32 snapshot (#34) |
| `context_intelligence.rs` | 476 | **322** | 22 | 7 | Bounded per-session semantic context (#34) |
| `event_log.rs` | 462 | **312** | 19 | 7 | Content-free lifecycle diagnostics + dump pruning |
| `user_dictionary.rs` | 420 | **305** | 25 | 7 | Bounded hot reload of user dictionary |
| `fault_injection.rs` | 417 | **296** | 15 | 5 | Deterministic key-path delays (#148) |
| `candidate_projection.rs` | 343 | **269** | 15 | 6 (`pub(crate)`) | Display-facing dedup/index mapping of candidate list (#108/#85) |
| `composition_fence.rs` | 356 | **234** | 22 | 8 | Process-wide composing/converting claims for idle-Space absorption |
| `context_baseline.rs` | 273 | **154** | 8 | 2 | Deterministic offline context-prediction baseline (#34) |
| `timing.rs` | 221 | **147** | 12 | 3 | 9 lock-free key-path accumulators |
| `lib.rs` | 73 | **33** | 0 | 0 | Module list + crate docs |
| `shift_ascii_space.rs` | 43 | **43** | 1 | 1 | Pure domain decision for Shift-started ASCII + Space |

### src/ (test-only or oracle modules compiled into the lib)

| file | total | `#[test]` | responsibility |
|---|---|---|---|
| `shift_latin_order_tests.rs` | 998 | 33 | Shift-Latin ordering conformance vs oracle |
| `space_key_dispatch_tests.rs` | 808 | 23 | Space dispatch conformance (uses `Dispatcher`, `UiBoard`, `dictc`) |
| `space_key_dispatch_oracle_tests.rs` | 561 | 24 | Oracle self-tests |
| `space_key_dispatch_oracle.rs` | 412 | 0 | Independent domain oracle (always compiled, `#[allow(dead_code)]`, `lib.rs:66`) |
| `developer_history_oracle.rs` | 302 | 0 | History lifecycle oracle |
| `shift_latin_oracle.rs` | 290 | 0 | Shift-Latin oracle |
| `developer_history_oracle_tests.rs` / `_order_tests.rs` | 244 / 240 | 10 / 2 | Oracle + ordering tests |
| `shift_latin_oracle_tests.rs` | 244 | 12 | Oracle self-tests |
| `shift_ascii_space_tests.rs` | 126 | 2 | Domain-function tests |

### tests/

| file | total | `#[test]` | `#[ignore]` | responsibility |
|---|---|---|---|---|
| `shipped_dictionary_ranking.rs` | 2089 | 38 | **37** | Candidate order of the *shipped* `system.dic` (`:67` falls back to `artifacts/release/system.dic`) |
| `pipe_round_trip.rs` | 1293 | 10 | 0 | Real engine process over a private test pipe |
| `appcontainer.rs` | 1135 | 5 | 4 | Real AppContainer token against the well-known pipe |
| `common/mod.rs` | 548 | 2 | – | `Engine::spawn_isolated{,_with_setup,_with_faults}`, isolated `LOCALAPPDATA`, `test_dictionary`, key builders |
| `zero_alloc_dispatch.rs` | 547 | 6 | 0 | Hot path allocates nothing (DESIGN 5.7) |
| `high_load_key_integrity.rs` | 390 | 6 | 0 | Lossless input while the engine is deliberately stalled (#148; 14 fault-injection refs) |
| `space_key_dispatch_pipe.rs` | 323 | 12 | 0 | Real-process Space protocol/failure injection |
| `ipc_latency.rs` | 233 | 1 | 1 | Keystroke round-trip latency |
| `resource_budget.rs` | 229 | 1 | 1 | Real-process dictionary + private working set |
| `cross_commit_learning.rs` | 217 | 2 | 0 | Cross-commit context/personalization authority |
| `context_intelligence_budget.rs` | 164 | 3 | 1 | Allocation/latency evidence for #34 core |

## 2. Intra-crate dependency graph

`use crate::` edges (production code only; doc-link-only references excluded):

```
dispatch  -> ai_text(61) candidate_projection(62) composition_fence(63) dictionary(64)
             input_history(65) learning(66) long_conversion(67) prediction(68)
             session(69) shift_ascii_space(73) ui(77) timing(632) fault_injection(635)
server    -> ai_text(57) composition_fence(58) dictionary(59) dispatch(60)
             fault_injection(61) input_history(62) learning(63) long_conversion(64)
             prediction(65) timing(66) ui(67)
prediction        -> dictionary(20) learning(21)
learning          -> session(26 `text_hash`) timing(27)
long_conversion   -> dictionary(21)
user_dictionary   -> dictionary(18)
prediction_snapshot -> prediction
context_evaluation  -> prediction_snapshot(10)
main      -> event_log(12) fault_injection(13) server(14) [+ dictionary/user_dictionary/learning/input_history by path]
leaves: session, ui, dictionary, ai_text, timing, fault_injection, candidate_projection,
        composition_fence, shift_ascii_space, event_log, configuration, context_*
```

**Cycles: none in production code.** Every apparent two-way pair is a doc link only, and each is a latent coupling worth noting:
- `session.rs:8,27,257,319,1798` → `crate::dispatch`; `session.rs:1674` → `crate::server` (rustdoc only).
- `ui.rs:4,30` → `crate::server` (rustdoc only); real edge is `server.rs:67 → ui`.
- `composition_fence.rs:4` → `crate::dispatch::Dispatcher` (rustdoc only).
- Test-only real cycles exist: `space_key_dispatch_tests.rs:10` and `shift_latin_order_tests.rs:12` import `crate::dispatch`, so the lib's test cfg is one strongly-connected blob.

**Hubs** (depended on by many): `dictionary` (4 in-edges), `session`, `ui`, `learning`, `timing`, `composition_fence`.
**God clients**: `dispatch` (13 intra-crate deps, 6409 prod lines) and `server` (11 deps). `server` and `dispatch` together import *every* service module; nothing else does.

## 3. Cross-crate imports

| module | sakura_core | sakura_proto | sakura_ipc | sakura_reg | sakura_ai_proto | sakura_neural_proto | windows |
|---|---|---|---|---|---|---|---|
| `dispatch` | conversion, dictionary, keymap, romaji, width, root (`:38–51`) | `:53` | `debug_trace` `:52` | – | – | – | – |
| `server` | `:44` | `:45,:50` | `:52,:53` | – | – | – | yes (`core::Result`) |
| `session` | conversion, keymap, romaji, root (`:31–41`) | `:42,:43` | – | – | – | – | – |
| `ui` | – | `:46,:47` | `debug_trace` `:45` | – | – | – | – |
| `dictionary` | `:16,:17` | `:21` | – | – | – | – | `:22,:23,:26` (mapping) |
| `input_history` | – | `:26` | – | – | – | – | `:27–:33` (DPAPI, files) |
| `learning` | – | `:24` | `:23` | – | – | – | – |
| `prediction` | `:14` | `:18` | – | – | – | – | – |
| `ai_text` | – | `:17` | – | `:18` | `:16` | – | `:19` |
| `long_conversion` | `:17,:18` | `:19` | – | – | – | – | – |
| `composition_fence` | – | `:42` | `ConnectionProbe` `:41` | – | – | – | – |
| `configuration` | `:10` | – | – | – | – | – | – |
| `candidate_projection` | – | `:20` | – | – | – | – | – |
| `timing` / `fault_injection` | – | `:32` / `:41` | – | – | – | – | – |
| `context_{baseline,evaluation,intelligence}`, `prediction_snapshot` | – | some | – | – | – | `:7/:8/:16/:7` | – |

**Reverse view.** `sakura-settings` links the whole engine crate for exactly three things:
- `crates/sakura-settings/src/learning.rs:6` → `learning::{read_snapshot, LearningService, LearningSnapshot, LEARNING_FORMAT_VERSION}`
- `crates/sakura-settings/src/input_history.rs:8` → `input_history::{clear_path, read_snapshot, InputHistoryRecord, InputHistorySnapshot, KeyHistoryRecord, ScopeClass, INPUT_HISTORY_FORMAT_VERSION}`; `:712` adds `CommitHistoryRecord`
- `crates/sakura-settings/src/engine_faults.rs` references `fault_injection` in prose only (no import).

`tools/ime-eval` contains **zero** `sakura_engine` references — it is a dev-dependency *of* the engine (`Cargo.toml`), i.e. the reverse of what the crate manifest suggests, and it drags `dictc` + ime-eval into every `cargo test -p sakura-engine`.

Integration tests use `sakura_engine::{server::Server, dictionary, learning, input_history, dispatch::Dispatcher, ui::UiBoard}` plus `sakura_ipc::Client`.

## 4. `dispatch.rs` anatomy

Production region is `1–6454`; the trailing test module is `6455–16850` (10,395 lines, 168 `#[test]`). Six earlier `#[cfg(test)]` regions sit in production code: `1896–1899`, `1901–1904`, `1907–1910` (a conversion-lookup counter used to assert cache behaviour), `1926–1932` and `2016–2027` (calls to `record_conversion_lookup_for_test()` *inside* production functions), and `2054–2067` (`mod associative_conversion_setting_tests`, 2 tests). So production conversion code contains test instrumentation at `dispatch.rs:1927` and `dispatch.rs:2017`.

| cluster | ranges | ≈prod lines |
|---|---|---|
| Imports, consts, `NewError`, `Reply` | 34–141 | 108 |
| `Dispatcher` state (28 fields) | 143–193 | 51 |
| **IPC request handling** — `impl Dispatcher`, 51 methods: 8 constructors (199–295), service setters (296–350, 390–416), `reset` (522), `apply_runtime_configuration` (535), `dispatch` (584), session CRUD (773–827), learning/history commands (828–890), scope/mode (891–950), AI text (951–1105), `send_key` (1106), `probe_key`/`probe_session` (1309/1356), `commit`/`commit_candidate`/`reconvert`/`revert`/`undo_commit_outcome` (1413–1655) | 195–1656 | 1462 |
| Key-pass plumbing: `KeyServices`, `ExecutionPolicy`, `state_code`, `KeyWork`, `PredictionCache`, `PredictionCacheWork` | 1660–1848 | 189 |
| Conversion option assembly + test counters | 1850–1910 | 61 |
| Session conversion-input accessors (5 `with_session_*`) | 1912–2052 | 141 |
| Long-conversion scheduling | 2069–2103 | 35 |
| **Candidate preference / projection**: `preferred_candidate_index`, `has_authoritative_candidate_preference`, `candidate_preference_matches_context`, `is_opaque_ascii_identifier`, `classify_conversion_input`, `literal_policy_for` | 2105–2235 | 131 |
| Raw-repair planning: `build_correction_runs`, `build_local_completion_plans`, `project_repair_segment` | 2242–2372 | 131 |
| Learning recording: `CommitSegmentMeta`, `candidate_meta`, `session_cross_commit_bridge`, `record_learning`, `candidate_learning_key`, `is_mode_switch` | 2373–2504 | 132 |
| **`apply_key`** | 2514–2734 | 221 |
| Raw-buffer/romaji arithmetic: `raw_chars_for_emitted`, `MatchSink`, `remove_raw_chars/range`, offsets, boundaries, `segment_raw_text` | 2791–3108 | 318 |
| Character feed: `feed_character`, `feed_input_character`, `feed_kana_character`, `flush_pending` | 3110–3341 | 232 |
| **`apply_action`** — 45 `Action::` arms incl. `ModeKanaCycle` at 3435 | 3350–3780 | 431 |
| Commit paths: `commit_conversion_then_feed_literal`, `commit_numbered_candidate`, `commit_numbered_suggestion`, `delete_focused_prediction_history` | 3788–4154 | 367 |
| Candidate surface/projection helpers + `commit_staged_raw_conversion` | 4157–4426 | 270 |
| `commit_converted_segments` | 4433–4705 | 273 |
| Conversion entry: `begin_conversion`, `prepare_shifted_ascii_reading`, `build_reconversion` | 4708–5104 | 397 |
| Editing: `apply_transform`, `resync_shifted_ascii_from_raw`, `apply_backspace`, `apply_delete_forward`, `CaretMove`, `move_caret` | 5110–5325 | 216 |
| `commit_pending`, `switch_mode`, `apply_alnum_char` | 5337–5467 | 131 |
| Prediction/suggestion: eligibility, `refresh_prediction`, focus, `commit_selected_suggestion`, `commit_suggestion_at` | 5469–5687 | 219 |
| **Output rendering**: `render_suggestions`, `publish_system_candidate_detail`, `render_prediction_projection`, `render_preedit`, `render_staged_raw_repair`, `render_converted_segments` | 5689–6453 | 765 |
| Timing/fault hooks | 632, 635, 1218, 1239 | 4 |

Punctuation/notation style is *not* a cluster — it is three call sites into `sakura_core` (`:3211`, `:3298`, `:4837`) plus `SpaceWidth` handling at `:154/:362/:4771`.

**Functions > 120 lines (10, all in production):** `apply_action` 3350–3780 (431), `commit_converted_segments` 4433–4705 (273), `begin_conversion` 4708–4975 (268), `render_converted_segments` 6208–6453 (246), `commit_numbered_candidate` 3831–4060 (230), `apply_key` 2514–2734 (221), `render_staged_raw_repair` 5997–6203 (207), `send_key` 1106–1307 (202), `feed_character` 3110–3255 (146), `dispatch` 584–710 (127).

**Test module themes** (keyword occurrences in the 168 names; names overlap): commit 33, predict 24, space 21, candidate 20, session 17, history 16, convert 12, mode 12, learn 12, ascii 11, caret 10, shift 10, backspace 9, probe 9, kana 8, overflow 8, suggest 7, scope 7, projection 7, width 5, delete 5, renderer 4, detail 3, password 3, undo 3, revert 3, punctuation 2. Zero tests mention fence, timing, fault or long_conversion.

**Fixtures:** 22 `ConversionService` constructions, all via `dictc::compile` + `Box::leak` into `from_static_bytes` (e.g. `dispatch.rs:6586` `conversion_fixture`, `:7975` `detail_conversion_fixture`, `:8113` `phase_one_prediction_conversion`). **No dispatch test needs a dictionary image on disk** — 44 `dictc` references build them in memory. The cost is that `dictc` is a dev-dependency of the whole crate.

## 5. Anatomy of the other large modules

**`server.rs`** — owns `Shared` (`:174`), `RuntimeConfiguration`/`DynamicRuntimes`/`RuntimeServiceSnapshot` (`:70–105`), `Admission`+`AdmissionPermit` (`:122–172`, cap `MAX_CONNECTIONS_PER_PID = 8` at `:126`), `Server` (`:402`, 12 constructors `:415–:571`), `reserve` (`:627`, #120), `InstanceSlot` (`:813`), `StartupGate` (`:1033`), endpoint helpers (`:868–:907`), worker spawn (`:909–:1032`), `worker_with_gate` (`:1130–1289`, 160), `first_request`/`request_allowed` (`:1357`, `:1461`), `serve` (`:1519–1883`, **365 lines** — the request loop, timing spans, `debug_trace`, UI publish, history-candidate delete), `runtime_services` (`:260–397`, 138). Connects to dispatch by owning one `Dispatcher` per worker and calling `dispatcher.dispatch(...)` in `serve`.

**`session.rs`** — `Session` (`:262`, ~148-field state), `SessionTable` (`:1680`, `MAX_SESSIONS = 64` at `:57`), `HostPolicy` (`:80`), `UndoRecord` (`:173`), `SessionCrossCommitBridge` (`:208`), commit cache (`:127`, `:163`), `text_hash` (`:1602`, the only symbol `learning` needs). 92 `pub`/`pub(crate)` accessors, no function > 120 lines. Purely a data module — all transitions are driven from `dispatch`.

**`ui.rs`** — `UiBoard` (`:509`) with `UiSnapshot` (`:442`), `CandidateSnapshot` (`:95`), `CandidateDetailSnapshot` (`:307`), `PendingCandidateCommit` (`:87`), `Delivery` RAII (`:1200`). Entry points `publish_output(_from)` (`:693`,`:702` — 131 lines), `publish_placement*`, `queue/take/reject_candidate_commit` (`:954–:1041`), `invalidate_stale_prediction_candidates` (`:1061`), `wait_past` (`:1138`). Dispatch writes into an `OutputBuf`; `server` hands that buffer to `UiBoard`.

**`learning.rs`** — `LearningKey`/`PreferenceQuery`/`LearningStrength` (`:72–:139`), `Slot`/`Index` (`:140–:417`), `HistoryEntry`/`PredictionHistory` (`:418–…`), the durable log, and the process-wide epoch (`:939`). `forget_prediction_exact` `:1047–1282` (236). Six small `#[cfg(test)]` islands sit inside production (`:1627–1709`).

**`prediction.rs`** — `PredictionCandidate`/`PredictionResult` (`:74`,`:131`), `Query`/`Mailbox` (`:230`,`:251`), `TestPredictionScript` (`:264` — a test hook in production), `predict_into` `:672–843` (172). Eight `#[cfg(test)]` islands inside production (`:258–:531`).

**`input_history.rs` — the mixed change reasons** (each row was touched by a different issue in #113–#132):

| change reason | ranges |
|---|---|
| store/wire format + record types + encode/decode | 36–54, 141–422 |
| low-level codec primitives (`Reader`, `put_*`, crc32, escape) | 1868–1975 |
| snapshot / export / admin (`to_tsv`, `retain_current_records`, `last_engine_identity`) | 423–569 |
| counters & exclusion policy | 570–654, 817–848 |
| service handle, queue, ownership of writer thread | 655–748, 736–800 |
| **epoch / Clear** | 657–680, 831–868, 1065–1085, 1256–1330 |
| **control barriers** (flush/stop FIFO barrier) | 1044–1136 |
| store ownership lock + paths | 1143–1214 (`acquire_store_owner` :1192) |
| writer lifetime loop | 1215–1349 |
| **compaction / retention** | 1350–1516, 1525–1542, 1617–1777 |
| startup scan / repair | 1489–1524, 1543–1616 |
| DPAPI protect/unprotect | 1808–1867 |
| engine identity (release label) | 1778–1807 |

No function exceeds 120 lines here; the problem is 13 unrelated reasons in one 1,909-line file, plus 7 `#[cfg(test)]` islands inside production (`:850`, `:1232`, `:1277`, `:1425`, `:1662`, `:1688`, `:1735`).

## 6. State machines and invariants

| machine | definition | transitions implemented in |
|---|---|---|
| `State{Idle,Composing,Converting,Predicting}` | `sakura-core/src/keymap.rs:88`; mirrored by `dispatch.rs:1705 state_code` | `dispatch::apply_action` 3350–3780, `session::begin_conversion/reset/cancel_conversion` (`:926`,`:679`,`:730`) |
| `Mode` | `sakura-proto/src/types.rs:271` | `dispatch::switch_mode` `:5419`; `ModeKanaCycle` arm `dispatch.rs:3435`; `session::remember/restore_mode_after_sensitive` `:594`,`:600` |
| Composition fence | `composition_fence.rs:48 CompositionFence`, `:53 FenceState{counts,torn_down,owned}`, `:65 OwnedClaim{probe,active}` | `dispatch::sync_composition_fence` `:448`, `release_composition_fence` `:493`, `release_all_composition_fence_claims` `:504` |
| AI job lifecycle | `ai_text.rs:59 JobState{Pending,Complete}`, `:65 Job{owner,cancel,signature,detached}`, `:83 Cooldown` | `dispatch.rs:1034 start_ai_text`, `:1060 poll_ai_text`, `:1089 cancel_ai_text` |
| Learning store + epoch | `learning.rs:939` process-wide epoch; `:167 Index`, `:443 PredictionHistory` | `dispatch::invalidate_stale_prediction_cache` `:751`; `ui::invalidate_stale_prediction_candidates` `:1061` |
| History epoch / barrier | `input_history.rs:655 Command`, `:680 epoch: AtomicU64` | `enqueue` `:849`, `clear` `:1065`, `stop` `:1086`, `writer_loop_with_file` `:1248` (`cleared_epoch` `:1256`, `:1289`, `:1326`) |
| Startup endpoint reservation (#120) | `server.rs:405 reserved_instances` | `Server::reserve` `:627`, consumed `:730`; ordering asserted in `main.rs:162` |
| Prediction cache capability | `dispatch.rs:1745 PredictionCacheWork{Apply,Probe}` + `:1680 ExecutionPolicy{Probe,Apply}` | `apply_key` `:2514`, `probe_key` `:1309` |

Representative invariant comments: `dispatch.rs:685` "fail-closed no-op rather than deleting by a guessed surface"; `:930` "mode menu is deliberately fail-closed until TSF has classified"; `:1406` "must not spend the one absorption a teardown is owed"; `:1552` "Reconversion must never pull selected password text"; `:2189` cached preference without the lexical tail "must not"; `:2423` "Sensitive scopes never reach the store"; `session.rs:78`,`:84`,`:265`,`:295`,`:388`; `ui.rs:459` stale lists "must disappear", `:777` "internal invariant violation", `:802` "never hide the authoritative candidate list"; `learning.rs:120` "must never transplant", `:551` "never allowed to replace a newer canonical log", `:688` epoch; `server.rs:990` cap "can never be exceeded", `:1022`, `:1277`, `:1437` "this invariant remains explicit", `:1835` "abandoned client's output must not replace another"; `input_history.rs:1044` "FIFO barrier", `:1260` loss "observable at barriers until a successful explicit Clear".

**Verification binding.** TLA+ specs at `verification/tla/{SpaceKeyDispatch,DeveloperHistory,ShiftLatinInput,AiTextLifecycle,EngineRecovery,HistorySuggestionConvert,DualTsf*,Tsf*}.tla`, driven by `scripts/verify-*-tlc.ps1`. `verification/space-key-dispatch/correspondence-and-audit.md:19–37` maps each REQ to a Rust anchor by *approximate line* ("`Action::Convert` / `begin_conversion` ~2739", "`idle_space_commit` ~1837–1871") — those line numbers no longer match `dispatch.rs` (`begin_conversion` is at 4708), so the traceability is already stale. `verification/space-key-dispatch/traceability.json` carries no Rust field at all. `verification/developer-history/correspondence-and-audit.md:7–11` binds to `DynamicRuntimes.input_history`, `set_input_history`, `record_key`, `input_history_stats`. The flat `verification/history-*.md` pairs mostly cite no code; only `history-read-failure-issue.md:8` names `scan_frames`, `repair_file`, `compact_file`, `InputHistoryService::open`. `verification/engine-startup-ownership.md:8` binds to `main::run` + `Server::reserve`. `verification/high-load-input-integrity.md:182,281,330` binds to `server.rs:1756-1759`, `debug_trace::emit`, `timing.rs`. Only `space_key_dispatch_oracle.rs` (6 file-level + ~20 line-level citations) and `shift_latin_oracle.rs` have precise, stable anchors — because they are separate small files.

## 7. Test topology

- **Inline (in the lib):** 170 tests in `dispatch.rs`, plus 96 in the five `*_tests.rs` src modules and ~230 across the other modules' trailing test blocks. Total inline lines ≈ **22,000**.
- **tests/ (11 binaries):** 86 `#[test]`, of which **45 are `#[ignore]`d** — 36 need `artifacts/release/system.dic` (`shipped_dictionary_ranking.rs:67`, override `SAKURA_SYSTEM_DIC`), 4 build a real AppContainer profile, 5 are release-only benchmarks.
- **Compiled dictionary image:** never needed inline (all `dictc::compile` in memory); needed on disk only by `shipped_dictionary_ranking.rs` and `resource_budget.rs` (`SAKURA_PHASE2_DICTIONARY`).
- **Real named pipe / real process:** `pipe_round_trip.rs`, `appcontainer.rs`, `space_key_dispatch_pipe.rs`, `ipc_latency.rs`, `resource_budget.rs`, `high_load_key_integrity.rs` — all through `tests/common/mod.rs`, which provides `Engine::spawn_isolated{,_with_setup,_with_faults}` (`:64`,`:69`,`:84`), `spawn_well_known_for_appcontainer` (`:101`), an isolated `LOCALAPPDATA` (`:201`), `test_dictionary` (`:420`), `char_key`/`test_char_key`/`named_key` (`:460–:494`), and `Cleanup` proving no survivor process.
- **Fault-injection arming:** only `high_load_key_integrity.rs` (14 refs) via `spawn_isolated_with_faults`; the plan is parsed in `main.rs:537` and refused unless a private test pipe is also given.
- **Compile unit today:** a one-line change to key dispatch rebuilds `sakura-engine` *including* its 22k lines of inline tests, then relinks all 11 integration binaries, and — because `dictc` and `sakura-ime-eval` are dev-dependencies — pulls both into the test profile. CI has no per-crate job; the only gate is one `cargo test --workspace`.
- **Editing cost:** an agent changing a 30-line function in `dispatch.rs` has a 16,850-line file in context; the relevant tests are somewhere in lines 6455–16850 with no naming convention that maps a function to its tests.

## 8. Findings

1. **`dispatch.rs` is a god module with ~10 unrelated change reasons in 6,409 production lines** (`dispatch.rs:34–6454`). Boundary: split into `ipc/`, `keys/`, `edit/`, `conversion/`, `candidates/`, `prediction/`, `commit/`, `render/`.
2. **The trailing 10,395-line test module** (`dispatch.rs:6455–16850`) makes every edit expensive and hides which tests cover which function. Boundary: one `_tests.rs` sibling per new module.
3. **Test instrumentation inside production conversion code** — `record_conversion_lookup_for_test()` called at `dispatch.rs:1927` and `:2017`, defined `:1901–1910`. Boundary: move the counter into the conversion module as a `pub(crate)` observer.
4. **`impl Dispatcher` has 8 near-identical constructors** (`dispatch.rs:199–295`) and `Server` has 12 (`server.rs:415–571`) — 20 overloads of "same object plus one more optional service". Boundary: one `EngineServices` builder shared by both.
5. **`apply_action` is a 431-line, 45-arm match** (`dispatch.rs:3350–3780`) that every key/state issue (#51 #56 #68 #102 #141) must open. Boundary: one file per action family, each ≤150 lines.
6. **`input_history.rs` mixes 13 change reasons** (§5 table); #113–#132 each touched it. Boundary: `history/{record,codec,store,writer,compaction,epoch,counters,export}.rs`.
7. **`sakura-settings` links the entire engine for a record format** (`sakura-settings/src/learning.rs:6`, `input_history.rs:8`). Boundary: extract `sakura-store` with record types, codecs, `read_snapshot`, `clear_path`, format constants; the engine keeps only the writer.
8. **`tools/ime-eval` and `dictc` are dev-dependencies of the engine** although `ime-eval` is never imported by any engine file. Boundary: drop the `sakura-ime-eval` dev-dep; gate `dictc` behind a fixture feature.
9. **`serve` (365 lines, `server.rs:1519–1883`) interleaves protocol, timing spans, debug trace, UI publish and history-candidate deletion.** Boundary: `serve` keeps framing + deadline; `serve_effects.rs` owns post-dispatch side effects.
10. **Verification anchors are line-number prose that has already drifted** (`verification/space-key-dispatch/correspondence-and-audit.md:19` says `begin_conversion ~2739`; it is at `dispatch.rs:4708`). Boundary: bind records to module paths and put `rust_path` in `traceability.json`.
11. **Doc-comment back-references create phantom cycles** (`session.rs:8,27,257,319,1798`, `ui.rs:4,30`, `composition_fence.rs:4`). Boundary: state the contract in the depended-on module's own docs.
12. **`Session` is a ~148-field struct with 92 accessors** (`session.rs:262–1600`) covering composition, conversion, undo, prediction focus, raw-repair, cross-commit bridge and scope memory. Boundary: split into `CompositionState`, `ConversionState`, `UndoState`, `ScopeState`.
13. **Four dormant #34 modules (1,846 lines / 1,212 prod) sit in the engine's compile+lint unit but are unreachable from `dispatch`/`server`.** Boundary: move to the feature-gated `sakura-context-research` crate.
14. **Oracles are always-compiled in the shipped lib** (`lib.rs:66` `#[allow(dead_code)] mod space_key_dispatch_oracle`) so cargo-mutants can score them. Boundary: a `verification` feature or a `sakura-oracles` dev crate.
15. **Naming does not signal the change target**: `dispatch.rs` also renders output, `ui.rs` is a shared snapshot board not UI code, `dictionary.rs` is a worker pool, `long_conversion.rs` is a neural reranker. An agent handed "#99 candidate order" cannot tell whether to open `dispatch.rs`, `candidate_projection.rs`, `dictionary.rs` or `long_conversion.rs`.

## 9. Target structure proposal

New crates (extracted whole, no logic change):

- **`crates/sakura-store`** — pure record types, codecs, retention/compaction, paths/atomic persistence, DPAPI, snapshot readers, format constants, and shared `text_hash`. Depends only on `sakura-values` + `windows`. It owns no queue, channel, writer loop, timing, diagnostics, or service lifecycle. `sakura-settings` depends on this instead of `sakura-engine`. Binds `verification/history-*.md`.
- **`crates/sakura-context-research`** — the four dormant #34 modules, available only behind `context-research`. Depends on `sakura-context-proto` and `sakura-values` as required by actual imports.
- **`crates/sakura-oracles`** (dev-only) — the three oracles + self-tests. Binds `verification/*/tla-record.md`, `oracle-provenance.md`.

```
crates/sakura-engine/src/
  lib.rs  main.rs
  ipc/        server.rs  admission.rs  instances.rs  startup.rs  serve.rs  serve_effects.rs
  request/    router.rs  services.rs(builder)  ai_commands.rs  history_commands.rs  learning_commands.rs
  keys/       policy.rs  key_pass.rs  mode.rs  caret.rs  segment.rs  candidate_keys.rs  commit_keys.rs
              transform.rs  space.rs
  edit/       raw_buffer.rs  feed.rs  romaji_offsets.rs
  conversion/ begin.rs  segments.rs  reconvert.rs  options.rs  repair.rs
  candidates/ projection.rs  preference.rs  detail.rs
  predict/    service.rs  cache.rs  suggestions.rs
  commit/     pending.rs  numbered.rs  converted.rs  learning_record.rs
  render/     preedit.rs  candidates.rs  suggestions.rs  repair.rs
  state/      session.rs  composition.rs  conversion_state.rs  undo.rs  scope.rs  table.rs
  services/   dictionary.rs  learning.rs  history_writer.rs  ai_text.rs  reranker.rs
              user_dictionary.rs  configuration.rs
  runtime/    ui_board.rs  composition_fence.rs  timing.rs  fault_injection.rs  event_log.rs
```

Dependency direction is acyclic: `ipc/` and `request/` call domain controllers in `keys/`,`commit/`,`render/`,`conversion/`,`candidates/`,`predict/`,`edit/`; controllers may depend on both `state/` and `services/`; `state/` and `services/` never depend on each other. `state/` depends only on stable value/core types. `services/` depends on `sakura-store`/core/proto and owns runtime writer loops; it accepts value records/events constructed by controllers and never imports `session`. `runtime/` remains a lower utility boundary. Enforce actual Cargo edges with metadata and module edges with the architecture checker, not source grep alone. Convention: unit tests as sibling `src/<area>/<module>_tests.rs`; cross-module and real-process tests as `tests/<area>_<topic>.rs`.

Verification binding: `keys/space.rs` ← `verification/space-key-dispatch/*`; `keys/mode.rs`+`keys/transform.rs` ← `verification/shift-latin-order/*`; `services/ai_text.rs` ← `verification/tla/AiTextLifecycle.tla`; `sakura-store` history modules ← `verification/developer-history/*` and `verification/history-*.md`; `ipc/startup.rs` ← `verification/engine-startup-ownership.md`; `ipc/serve.rs`+`runtime/timing.rs`+`runtime/fault_injection.rs` ← `verification/high-load-input-integrity.md`.

### Reading set, before vs after

| issue | BEFORE (files, ≈lines open) | AFTER |
|---|---|---|
| `無変換` mode cycle (#51-type) | `dispatch.rs` 16,850 + `session.rs` 2,584 + keymap toml ≈ **19,600** | `keys/mode.rs` ~260 + `keys/mode_tests.rs` ~350 + `state/scope.rs` ~180 + keymap toml ≈ **900** |
| candidate order (#99/#108) | `dispatch.rs` 16,850 + `candidate_projection.rs` 343 + `dictionary.rs` 1,594 + `tests/shipped_dictionary_ranking.rs` 2,089 ≈ **20,900** | `candidates/preference.rs` ~200 + `candidates/projection.rs` ~270 + `_tests.rs` ~500 + `tests/candidates_shipped_ranking.rs` 2,089 ≈ **3,050** (~970 without the shipped-dict suite) |
| history compaction (#127) | `input_history.rs` 3,632 + `server.rs` 3,189 + `verification/history-compaction-publication.md` ≈ **7,000** | `sakura-store/src/compaction.rs` ~300 + `compaction_tests.rs` ~400 + `writer.rs` ~250 ≈ **950** |
| key-path timing / lossless input (#148) | `server.rs` 3,189 + `dispatch.rs` 16,850 + `timing.rs` 221 + `fault_injection.rs` 417 + `tests/high_load_key_integrity.rs` 390 ≈ **21,100** | `ipc/serve.rs` ~380 + `runtime/timing.rs` 221 + `runtime/fault_injection.rs` 296 + `tests/ipc_high_load_integrity.rs` 390 ≈ **1,290** |

## 10. Migration sequence

Each step is one PR, compiles at every point, behaviour-preserving.

1. **Move `dispatch.rs`'s trailing test module to a sibling file.** Technique: move lines 6455–16850 into `src/dispatch_tests.rs`, add `#[cfg(test)] #[path = "dispatch_tests.rs"] mod tests;` (the pattern `lib.rs:38,55,63` already uses). Risk: low. `Verify:` `cargo test -p sakura-engine --lib dispatch::` `Expect:` 170 tests pass; `wc -l src/dispatch.rs` = 6,455. **Highest reading-cost reduction per unit of risk — do first.**
2. **Same for `learning.rs`, `input_history.rs`, `server.rs`, `session.rs`, `prediction.rs`, `ui.rs`, `dictionary.rs`.** Risk: low. `Verify:` run engine lib tests and record production-only lines for the files changed in this step. `Expect:` the Phase 0 test universe is unchanged and each changed file meets its recorded production-line target; this step makes no global ≤2,400-line claim about `dispatch.rs`, whose production split is Steps 7–10.
3. **Extract `crates/sakura-store`** with only pure input-history/learning record definitions, codecs, retention/compaction, persistence paths/atomic operations, crypto, snapshot readers, and `text_hash`; engine retains queues, writer loops, timing, diagnostics, and service lifecycle. Repoint settings in the same PR (D4 caller/removal proof applies before compatibility reexports are removed). Store may depend only on `sakura-values` and `windows`, and must not import proto/ipc/session. Risk: medium (DPAPI + on-disk format). `Verify:` metadata dependency checks, store/engine/settings tests, existing fixture byte round-trip, and thread/channel/service import audit. `Expect:` settings has no engine edge, store has no runtime owner loop or session back-edge, and on-disk bytes remain compatible.
4. **Split the rest of `input_history.rs` into `history/{writer,compaction,epoch,counters,export}.rs`** with `pub(crate)` re-exports. Risk: medium. `Verify:` `cargo test -p sakura-engine --lib history::` `Expect:` all 53 tests pass in the file owning their concern.
5. **Move the 4 dormant #34 modules into `crates/sakura-context-research` behind `context-research`.** Risk: low. `Verify:` build engine with and without the feature and inspect metadata edges. `Expect:` default build has no research implementation; feature build preserves it through `sakura-context-proto`.
6. **Move the three oracles + self-tests into a dev-only `crates/sakura-oracles`; drop `#[allow(dead_code)]` at `lib.rs:66`.** **Owner decision:** `verification/*/cargo-mutants.toml` must be repointed. Risk: low-medium. `Verify:` `rg -n 'allow\(dead_code\)' crates/sakura-engine/src/lib.rs` `Expect:` no match; `scripts/verify-space-key-dispatch-tlc.ps1` still resolves its oracle path.
7. **Extract the render cluster (`dispatch.rs:5689–6453`) to `src/render/`** — file move, `pub(crate)` fns, no signature change. Risk: low. `Verify:` `cargo test -p sakura-engine --lib render::` and `--test zero_alloc_dispatch` `Expect:` allocation assertions still pass.
8. **Extract the edit/raw-buffer cluster (2791–3341) to `src/edit/`.** Risk: low. `Verify:` `rg -n 'use crate::(keys|commit|render)' crates/sakura-engine/src/edit/` `Expect:` no matches.
9. **Split `apply_action` (3350–3780) into `keys/{mode,caret,segment,candidate_keys,commit_keys,transform,space}.rs`,** leaving a thin dispatch table. Risk: medium (highest-churn code). `Verify:` `cargo test -p sakura-engine --lib keys::` + `--test space_key_dispatch_pipe` `Expect:` all pass, no `keys/*.rs` over 200 lines.
10. **Extract candidates (2105–2235, 4157–4426 + `candidate_projection.rs`) to `src/candidates/`; commit paths (3788–4705) to `src/commit/`.** Risk: medium. `Verify:` `cargo test -p sakura-engine --lib candidates:: commit::` `Expect:` the 20 candidate- and 33 commit-themed tests pass.
11. **One `EngineServices` builder replacing the 8 `Dispatcher::new_*` and 12 `Server::with_*` constructors.** Risk: medium; touches every fixture. **Owner decision:** whether the old constructors stay as `#[deprecated]` shims for one release. `Verify:` `rg -c 'fn (new|with)_\w+' crates/sakura-engine/src/request/services.rs crates/sakura-engine/src/ipc/server.rs` `Expect:` ≤ 3 each.
12. **Split `serve` (`server.rs:1519–1883`) into `ipc/serve.rs` (framing + deadline) and `ipc/serve_effects.rs` (UI publish, history delete, debug trace, timing spans).** Risk: medium-high — this is the #148 deadline path. `Verify:` `cargo test -p sakura-engine --test high_load_key_integrity --test ipc_latency` `Expect:` lossless-input and latency assertions unchanged.
13. **Split `Session` (`session.rs:262`) into `state/{composition,conversion_state,undo,scope}.rs` sub-structs (D5 decided 2026-09-13).** Do this as the final standalone Phase 4 change. Risk: high. Keep transitions in controllers so neither `state` nor learning services call back into the other. `Verify:` engine tests plus `zero_alloc_dispatch` and architecture cycle check. `Expect:` no new allocations, all tests pass, and no `session → learning → session` cycle.
14. **Repoint verification records to module paths** — rewrite the `Implementation` column of the two `correspondence-and-audit.md` files to `engine::keys::space::…` style and add `rust_path` to `traceability.json`. Risk: docs only. `Verify:` `rg -n '~[0-9]{3,}' verification/*/correspondence-and-audit.md` `Expect:` no approximate line numbers remain.
15. **Drop the `sakura-ime-eval` dev-dependency and gate `dictc` behind a `dev-fixtures` feature.** Risk: low. `Verify:` `cargo test -p sakura-engine --features dev-fixtures --no-run`; inspect the no-feature dependency graph and search active engine sources for `ime_eval`. `Expect:` fixture-enabled tests compile, ordinary builds omit dictc, and ime-eval is absent.
