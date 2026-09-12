# `sakura-core` / `sakura-proto` structural analysis (HEAD a370477)

Read-only. All counts measured at HEAD; "production lines" = total minus every `#[cfg(test)]` / `#[cfg(all(test,…))]` region (brace-matched). `fn` counts are production-only. Caveat: brace counting is fooled by char literals such as `'{'..='~'` at `crates/sakura-core/src/input_repair.rs:310`, so single-function spans in that file are reported from manual inspection.

## 1. Module inventory

### `crates/sakura-core`

| file | total | prod | fn | pub fn | responsibility |
|---|---|---|---|---|---|
| `src/conversion.rs` | 7,405 | 5,009 | 135 | 65 | Lattice build, Viterbi, A* N-best, candidate assembly/ranking/evidence, numerals+date+single-kanji+punctuation synthesis, raw-repair passes, cross-commit bridge |
| `src/dictionary.rs` | 2,671 | 2,187 | 73 | 31 | `image_format` constants, mmap image parse+validate, LOUDS trie lookup, entry/surface/annotation/detail readers |
| `src/simd.rs` | 2,058 | 927 | 24 | 6 | Passthrough LUT construction, scalar/SSSE3/AVX2/AVX-512 width-scan kernels, runtime kernel selection |
| `src/width.rs` | 1,985 | 780 | 30 | 9 | `Width`/`PunctuationStyle`/`BracketStyle` policy, punctuation families, `Normalizer` run-based rewrite |
| `src/preferences.rs` | 1,889 | 1,122 | 47 | 31 | Preference enums, `Preferences`/`AppProfile`, TOML parse + serialize + per-app resolution |
| `src/keymap.rs` | 1,845 | 773 | 26 | 15 | `State`/`Action` vocabulary, key-spec parsing, preset merge, `KeyMap::lookup` |
| `src/romaji.rs` | 1,797 | 1,046 | 47 | 32 | Romaji table compile, input FSM (`feed`/`flush`), replay trace, local completions |
| `src/input_repair.rs` | 1,234 | 1,003 | 31 | 9 | Typo/kana variant generation, English-spelling→katakana, contextual punctuation swap |
| `src/user_dictionary.rs` | 815 | 472 | 20 | 15 | User dictionary text format + in-memory reading trie |
| `src/config.rs` | 618 | 369 | 15 | 5 | The minimal TOML subset every shipped data file uses |
| `src/numerals.rs` | 487 | 416 | 20 | 7 | Arabic / full-width / kanji numeral rewrite of number readings |
| `src/calendar.rs` | 480 | 373 | 25 | 14 | Civil dates, eras, weekday, `きょう`-class surfaces |
| `src/editing.rs` | 387 | 322 | 10 | 9 | F6–F10 segment transforms, identifier casing |
| `src/cpu.rs` | 223 | 169 | 11 | 9 | CPUID feature probe, resolved once |
| `src/text.rs` | 111 | 68 | 8 | 0 | `TextSink` trait — the write target everything shares |
| `src/lib.rs` | 84 | 84 | 0 | 0 | Module list + flat facade re-export |
| `tests/zero_alloc.rs` | 253 | — | 11 | 0 | Counting allocator: FSM + width + keymap allocate 0 per keystroke |
| `tests/width_bench.rs` | 285 | — | 5 | 0 | `#[ignore]` run-scanner benchmark over four corpora |
| `tests/fsm_robustness.rs` | 175 | — | 8 | 0 | Sharded deterministic FSM/keymap fuzz campaign (CI job) |

### `crates/sakura-proto`

| file | total | prod | fn | pub fn | responsibility |
|---|---|---|---|---|---|
| `src/types.rs` | 1,657 | 1,305 | 70 | 68 | Domain value types (`KeyCode`…`Mode`…`Candidate`) **plus** their per-type encode/decode |
| `src/output.rs` | 1,436 | 948 | 51 | 37 | `OutputBuf`: zero-allocation engine-side output builder + frame encoder |
| `src/message.rs` | 1,325 | 1,219 | 17 | 6 | `Request`/`Response`/`UiState`/`Header`, frame encode/decode dispatch |
| `src/wire.rs` | 447 | 342 | 31 | 16 | `Reader`, `Sink`/`VecSink`/`SliceSink`, the crate-wide `Error` |
| `src/fixed.rs` | 390 | 282 | 30 | 25 | `FixedStr`, `FixedVec`, `Overflow` — allocation-free containers |
| `src/lib.rs` | 138 | 138 | 0 | 0 | Facade + all wire capacity constants |
| `tests/roundtrip.rs` | 773 | — | 24 | 0 | Every message encodes/decodes identically |
| `tests/robustness.rs` | 533 | — | 17 | 0 | Sharded hostile-decoder fuzz campaign (CI job) |
| `tests/zero_alloc.rs` | 141 | — | 7 | 0 | `OutputBuf` frame encode allocates 0 |

## 2. Intra-crate dependency graphs

### `sakura-core` (production edges; test-only edges marked ⓣ)

```
text        -> (leaf; uses proto FixedStr/Overflow)
cpu         -> (leaf)
config      -> (leaf)
simd        -> cpu                         (simd.rs:58)
editing     -> text                        (editing.rs:9)
numerals    -> text                        (numerals.rs:9)
calendar    -> text                        (calendar.rs:8)
dictionary  -> text                        (dictionary.rs:13)
width       -> editing, simd, text         (width.rs:17,18,19)
romaji      -> config, text                (romaji.rs:31,32)
keymap      -> config                      (keymap.rs:35)
user_dict   -> dictionary                  (user_dictionary.rs:10)
input_repair-> dictionary, preferences     (input_repair.rs:12,13)
preferences -> config, keymap, width       (preferences.rs:8,9,10)
conversion  -> calendar, dictionary, input_repair, numerals, preferences,
               user_dictionary, width, text  (conversion.rs:15–28, 1048, 1442)
```

No production cycles. Two test-only back-edges violate the documented one-way stack:

- `preferences.rs:1609,1612` calls `crate::allows_system_entry` (owned by `input_repair`), while `input_repair.rs:13` imports `crate::preferences::InputSupport` — **cyclic pair at test level**.
- `simd.rs:1761,1899` reach into `crate::width` from `mod tests`, against `width.rs:18 → simd` — **cyclic pair at test level**.

Hubs (in-degree): `text` 6, `dictionary` 4, `config` 3, `width` 3, `preferences` 2. Out-degree hub: `conversion` 8 — it depends on more than half the crate.

### `sakura-proto`

```
fixed   -> (leaf)
wire    -> fixed, lib consts               (wire.rs:18,19)
types   -> wire                            (types.rs:15)
message -> types, wire, lib consts         (message.rs:21,26,27)
output  -> fixed, message(RES_OUTPUT), types, wire, lib consts (output.rs:16–23)
```

Acyclic. The only non-obvious edge is `output -> message` for one wire constant (`output.rs:17`, `RES_OUTPUT`).

## 3. `core -> proto` coupling

Every `sakura_proto` reference inside `crates/sakura-core/src` (18 sites, complete):

| site | items | classification |
|---|---|---|
| `calendar.rs:6` | `Overflow` | container |
| `numerals.rs:7` | `Overflow` | container |
| `editing.rs:7` (+`:325` ⓣ `FixedStr`) | `Overflow` | container |
| `text.rs:14` | `FixedStr`, `Overflow` | container |
| `romaji.rs:29` | `FixedStr`, `FixedVec`, `Overflow` | container |
| `dictionary.rs:11` | `FixedStr`, `MAX_PREEDIT_BYTES` | container + capacity constant |
| `user_dictionary.rs:8` | `MAX_PREEDIT_BYTES` | capacity constant |
| `input_repair.rs:10` (+`:1007` ⓣ `Mode`) | `FixedStr`, `FixedVec`, `MAX_PREEDIT_BYTES` | containers + constant |
| `conversion.rs:12,13` | `MAX_CANDIDATES`, `FixedStr`, `FixedVec`, `MAX_PREEDIT_BYTES`, `MAX_SEGMENTS` | containers + constants |
| `width.rs:20` (+`:830` ⓣ) | `Mode`, `Overflow` | **domain value type** + container |
| `preferences.rs:13` | `AppearanceTheme`, `Mode`, `PadShortcut` | **domain value types** |
| `keymap.rs:33` | `KeyCode`, `KeyInput`, `Modifiers` | **domain value types** |
| `simd.rs:1762` ⓣ | `FixedStr`, `Mode` | test only |
| `lib.rs:74` | `pub use sakura_proto::{AppearanceTheme, PadShortcut}` | pure pass-through |

**Not one genuine wire type** (`Request`, `Response`, `Header`, `OutputBuf`, `Reader`, `Sink`, `Error`, `encode_*`, `decode_*`) is used by `sakura-core`. The whole arrow is: 3 containers (`FixedStr`, `FixedVec`, `Overflow`), 6 value types (`Mode`, `KeyCode`, `KeyInput`, `Modifiers`, `AppearanceTheme`, `PadShortcut`), 3 capacity constants.

Classification of `sakura-proto`'s public surface:

- **Allocation-free containers** — `fixed.rs:16,33,195`. Used by core, engine, dictc; nothing protocol-specific.
- **Cross-crate domain value types** — `types.rs:24 KeyCode`, `:168 Modifiers`, `:229 KeyInput`, `:271 Mode`, `:323 AppearanceTheme`, `:382 PadShortcut`, `:432 InputScope`, `:478 UnderlineKind`, `:860 ScreenRect`, `:994 ErrorCode`. Each carries an `encode`/`decode` method — the only thing tying them to the wire.
- **Engine-output rendering** — `types.rs:515 Segment`, `:537 Preedit`, `:550 Candidate`, `:579 CandidateDetail`, `:689 CandidatePresentation`, `:722 CandidateKind`, `:746 CandidateList`, `:919 Output`; all of `output.rs`.
- **Diagnostics** — `types.rs:1057,1142,1194,1267`.
- **Wire codec / messages** — all of `wire.rs`, all of `message.rs`, `lib.rs:63–137`.

Who else consumes proto's domain types (counts of `sakura_proto::<Item>` paths, excluding proto itself): `sakura-tsf` 6× `InputScope`, 5× `Modifiers`, 4× `ScreenRect`, 3× `KeyCode`; `sakura-engine` 7× `InputScope`, 4× `ErrorCode`; `sakura-renderer` 3× `AppearanceTheme`, 3× `CandidateKind`, 2× `Candidate`, 2× `Mode`; `sakura-settings` 3× `Mode`; `dictc` 2× `MAX_PREEDIT_BYTES`, 1× `FixedStr`, 1× `MAX_SEGMENTS`; `tools/ime-eval` 1× `MAX_CANDIDATES`. `sakura-ipc` uses only codec items.

**Conclusion.** A leaf crate — `sakura-values` — containing `fixed.rs` verbatim, the ten value types, and the capacity constants would remove the `core -> proto` arrow entirely (`rg -c 'sakura_proto' crates/sakura-core/src` → 0), and stop `dictc`/`sakura-settings` depending on the IPC crate for non-IPC reasons. `sakura-proto` keeps `wire`, `message`, `output`, diagnostics and rendering types, and re-exports `sakura-values` so no consumer path breaks. Owner decision: whether `encode`/`decode` stay on the value types (then `wire.rs` moves down too) or become free functions in proto. Moving `wire.rs` down is the smaller diff; keeping the leaf codec-free is the cleaner boundary.

## 4. `conversion.rs` anatomy

| cluster | lines | items |
|---|---|---|
| Search bounds & cost constants | 30–179 | `MAX_LATTICE_NODES`, `MAX_SEARCH_STATES`, cross-commit bounds 52–59, `MAX_CONVERSION_CANDIDATES` 61–68 (feature-gated), budget tiers 71–79, synthesis costs 80–142, `COUNTER_FORMS` 144 |
| Edge budget | 181–218 | `DictionaryEdgeBudget` |
| Candidate shape predicates | 220–325 | `numeric_form_cost`, `is_atomic_whole_reading_surface`, `is_kana_fragment_prefix_split`, `is_trustworthy_exact_surface` |
| Budget policy | 327–337 | `candidate_budget` |
| Options & context ids | 339–398, 1040–1060 | `ConversionOptions`, `LeftContextId`, `RightContextId` |
| **Cross-commit bridge (#83)** | 400–460 | `CrossCommitBridge`, `CommitBridgeTail`, `CommitBridgeTailStorage`, `BridgeBoundaryKind` |
| **Ranking evidence (#108)** | 462–713 | `RawRepairBudget`, `CandidateAuthority`, `CandidateEvidence` (+alias 530), `RepairTier`, `CandidateOrigin`, `PathEvidence`, `repair_kind_bit` |
| **Correction map / repair projection** | 715–1038 | `CorrectionRunKind`, `CorrectionRun`, `CorrectionMapError`, `CorrectionMap`, `RawRepairPlan` |
| Input classification | 1062–1154 | `LiteralPolicy`, `ConversionInputClass`, `ConversionInput` |
| Result/diagnostics | 1156–1220 | `ConversionSearchTerminal`, `ConversionDiagnostics`, `ConversionResult` |
| Candidate/segment types | 1222–1465 | `ConversionCandidate`, `ConversionSegment`, `impl ConversionCandidate` 1273–1437, `ConversionError` |
| Lattice internals | 1467–1543 | `Surface`, `Node`, `SearchState`, `HeapItem`, `PathClass`, `SearchRun` |
| **`Converter`** | 1545–4472 | struct 1545–1582, `impl` 1589–4467 |
| Cost & coherence helpers | 4474–4578 | `connection_cost`, `is_contextual_orthographic_sibling`, `is_exact_user_candidate`, `candidate_path_is_coherent` |
| **Candidate assembly/projection** | 4581–4937 | `dictionary_has_exact_surface`, `single_kanji_annotation`, `make_lossless_fallback`, `make_synthetic_exact`, `CandidateMaterialization`, `make_candidate` |
| Text/char helpers | 4939–5024 | `write_katakana`, `char_class`, `char_run`, identifier/latin predicates |
| Tests | 5026–7405 (2,380 lines, 49 `#[test]`) | see below |

`Converter` methods: **public entry points** (14) `convert` 1675 … `with_raw_repair_conversion` 2810, plus `set_civil_date` 1622 / `set_commit_repair_readings` 1629; **exact-only path** 2217, 2257, 2357; **raw-repair passes** `admit_repair_pass` 2679, `merge_repair_scratch` 2747; **special-candidate synthesis** `append_single_kanji` 2841, `append_punctuation_family` 2946, `ensure_lossless_fallback` 3032, `add_numeric_forms` 3241, `prefer_numeric_forms` 3310, `add_date_candidates` 3422; **ranking filters (the #94/#99/#108 surface)** 3050, 3107, 3138, 3176, 3195, 3207, 3223; **lattice/search** `reset` 3498, `build_lattice` 3510, `add_local_repair_edges` 3755, `add_commit_repair_edges_for_whole_query` 3817, `add_repaired_dictionary_edges` 3857, `build_single_segment_lattice` 3939, `add_node` 4108, `compute_suffix_costs` 4170, `best_final_node` 4201, `build_viterbi_candidate` 4227, `viterbi_path_is_coherent` 4269, `search_n_best` 4285, `push_state` 4432.

**Functions > 120 lines** (7): `convert_with_user_dictionary_detailed` 1754–1878 (125), `convert_with_user_dictionary_input_bridge_detailed` 1945–2215 (271), `convert_input_with_raw_repair_plans` 2480–2665 (186), `build_lattice` 3510–3748 (239), `build_single_segment_lattice` 3939–4106 (168), `search_n_best` 4285–4430 (146), `make_candidate` 4777–4937 (161).

**Test themes** (5026–7405): edge budget & surface diversity 5069–5213; single-kanji tail 5214–5316; candidate budget tiers 5317–5468; punctuation family 5469–5617; fixture builders 5618–5867; correction map 5868–5931; candidate authority/evidence 5932–6030; cross-commit bridge 6031–6426; literal policy 6427–6537; numerals/date ranking 6538–6712; trustworthy-exact vs repair 6713–6849; raw multi-pass 6850–7405. **No test needs a real dictionary image**: there is no `include_bytes!` and no `system.dic` reference anywhere in `crates/sakura-core`; every fixture is built in memory by `synthetic_image` (`conversion.rs:5685`) / `synthetic_dictionary` (`:5618`) against `crate::dictionary::image_format`. `dictionary.rs` tests do the same via `TestTable` (`:2191`). This is the single most valuable property for splitting the file — the tests are portable and carry their own fixtures.

## 5. Other module anatomies

**`dictionary.rs`** — `image_format` (pub mod 20–103, the format contract shared with `dictc`), `EntryFlags` 125, `Entry` 160, `PrefixMatch` 213, `Error` 224, `Dictionary<'a>` 399, `DictionaryDetail<'a>` 302, `SingleKanjiVariant*` 328/391, `DetailRelationKind` 104. Owned state: only borrowed slices over the mapped image (`Table<'a>` 281, `Details<'a>` 288). Entry points: `Dictionary::parse` 443 (**249 lines**, the only >120 fn), then a lookup API (`common_prefix_search` 856, `visit_descendant_entries` 895, `visit_prediction_entries` 992, `entry_at` 1020, `write_surface` 1096, `write_annotation` 1168, `detail_at` 1223, `connection_cost` 822, `bunsetsu_boundary` 717, `single_kanji*` 741–820). Mixed change reasons: (a) format constants 20–103, (b) structural validation 1263–1636 + 1873–2152 (~500 lines of `validate_*`/`read_*`), (c) LOUDS traversal 1638–1741, (d) lookup/query API 717–1261, (e) detail/relation semantics 302–397 + 1742–1865. A format change (#109) touches (a)(b); a lookup change touches (d) — both mean opening the same 2,671-line file.

**`preferences.rs`** — value enums 22–405, `InputSupport` 406, `Preferences` 508, `AppProfile` 576, `ContextPreferences` 602. Entry points: `parse_preferences` 686 (**126 lines**), `serialize_preferences(_with_profiles)` 815/820, `resolve_context_preferences` 635, `default_app_profiles` 617. Mixed reasons: the **vocabulary** (~400 lines), the **TOML codec** (`parse_*`/`*_name` 900–1121 plus the two big functions, ~500 lines), and **profile resolution** (617–670, 900–953). Adding one preference key means editing the enum, the parser, the serializer, and the format-version constant at `:17` — four regions of one file.

**`keymap.rs`** — `Preset` 48, `State` 88, `Action` 128 (a ~100-variant enum; `impl` 225–369 is name↔variant mapping), `KeyMap` 398, errors 405–472, `impl KeyMap` 473–651, free parsing helpers 652–771 incl. `NAMED_KEYS` 707. No fn ≥ 120 lines. Mixed reasons: **action vocabulary** (128–369, shared semantically with `sakura-engine/dispatch.rs`), **key-spec syntax** (652–771), **document merge/override** (486–650). `State` is exported and used 105× in `sakura-engine`, 25× in `sakura-renderer` — cross-crate vocabulary living in a parsing module.

**`romaji.rs`** — `Table` 94, `Input` 102 (the FSM's owned pending buffer), replay types 175–342, local-completion types 351–433, errors 435–517, `impl Table` 526–994. No fn ≥ 120 lines; longest are `drive` 815–869 and `drive_trace` 870–950. Mixed reasons: **table compilation** (533–566, 1001–1045), **the hot FSM** (`feed` 587, `flush` 626, `drive` 815 — the zero-alloc surface), **replay/diagnostics** (175–342, 642–950), **prediction support** (`plan_local_completions` 738). Only the second is on the hot path and only it is covered by `tests/zero_alloc.rs`.

**`width.rs` + `simd.rs`** — `width.rs` holds three separable things: **policy vocabulary** (24–333, incl. `PUNCTUATION_FAMILY_LEN`/`COMMA_FAMILY`/`PERIOD_FAMILY` 234–302), **the normalizer** (335–691, run-based, SIMD-dispatching), and **scalar character mapping** (693–826). `simd.rs` holds **LUT construction** (151–232), **kernel metadata/selection** (78–150, 234–474), and **the kernels** (601–1032, all `#[cfg(target_arch = "x86_64")]` except `scan_scalar` 622). `COMMA_FAMILY`/`PERIOD_FAMILY` are consumed only by `conversion.rs:2946` — nothing outside the crate references them, so the punctuation family is a self-contained feature spread across two large files.

## 6. Feature flags

Complete list of `feature = "…"` sites in `sakura-core` (10 total):

| feature | sites | what it gates | enabled by |
|---|---|---|---|
| `research-top32` | `conversion.rs:11,60,62` | `MAX_CONVERSION_CANDIDATES = 32` | `crates/dictc/Cargo.toml:13` |
| `research-wide-candidates` | `conversion.rs:11,60,62,67,1784,1906` | `MAX_CONVERSION_CANDIDATES = 512`; suppresses two ceiling assertions | `tools/candidate-sweep/Cargo.toml:18` (as `wide`) |
| `conversion-test-support` | `conversion.rs:1652,1660,1670` | three `set_*_budget_for_test` methods | `crates/dictc/Cargo.toml:12` |
| `simd-assembly-audit` | `simd.rs:356` | retains one bench-only kernel for the audited `--emit=asm` artifact | `ci/check-simd-assembly.ps1:273` |

Nothing enables these in the shipping workspace build. The research features are three constant definitions and two `#[cfg(not(...))]` assertions — already tiny. The better move is to make the ceiling a `Converter` construction parameter rather than a `cfg`, deleting all nine conversion sites and letting `candidate-sweep` sweep without a rebuild. Owner decision: it turns a compile-time bound into a runtime one on the hot path.

## 7. Public API surface

`sakura-core/src/lib.rs:42–83` re-exports 113 items in 14 flat groups, and every module is also `pub mod` (`lib.rs:26–40`), so both `sakura_core::Converter` and `sakura_core::conversion::Converter` compile. Consumers use both: `sakura-engine` references `sakura_core::conversion::…` deep paths 63× and `sakura_core::keymap::…` 8×; `dictc` references `sakura_core::dictionary::…` 30×. There is no enforced facade — the deep path is a first-class API and narrowing it is a breaking change to five crates plus three tools.

**Re-exported but unused outside `sakura-core`** (~28 items — whole-word search across all non-core `.rs` in `crates/` and `tools/`; upper bound, a few names are generic): `date_offset_for_reading`, `date_surface_specs`, `is_today_date_reading`, `DateSurfaceSpec`, `JapaneseEraYear`, `Weekday`, `parse_config`, `CandidateEvidenceClass`, `ConversionResult`, `LeftContextId`, `PathEvidence`, `RawRepairBudget`, `CpuFeatures`, `PrefixMatch`, `identifier_into`, `IdentifierStyle`, `allows_system_entry`, `english_spelling_katakana_reading`, `RepairVariant`, `RepairVariantList`, `ADVANCED_REPAIR_PENALTY`, `COMMIT_HISTORY_PENALTY`, `ENGLISH_KATAKANA_PENALTY`, `REPAIR_PENALTY`, `KeyMapErrorKind`, `serialize_preferences`, `ParsedPreferences`, `TableErrorKind`, `KernelMetadata`, `KernelSet`, `WidthScanStrategyId`, `UserDictionaryErrorKind`, `UserPosSpec`, `USER_DICTIONARY_FORMAT_VERSION`.

Heaviest external users: `ConversionOptions` (engine 102, dictc 87), `Preferences` (engine 98), `State` (engine 105, renderer 25), `EntryFlags` (dictc 112), `Dictionary` (dictc 107), `Action` (engine 78), `UserPartOfSpeech` (settings 75), `SegmentTransform` (engine 63). `sakura-core` also launders two proto types to its own consumers (`lib.rs:74`).

## 8. Findings

1. **`conversion.rs` is eight features in one file.** 5,009 production lines, 8 intra-crate deps, 24 of the last 150 commits. Lattice/search (3498–4472), candidate assembly (4581–4937), ranking filters (3050–3240), special candidates (2841–3497), evidence (462–713), correction-map projection (715–1038) change for unrelated reasons. *Boundary:* a `conversion/` directory with `search/`, `candidates/`, `ranking/`, `synthesis/`, `evidence.rs`, `repair_plan.rs`.
2. **The `core -> proto` arrow carries no protocol.** All 18 sites want containers, six value types, and three capacity constants. *Boundary:* leaf `sakura-values`; proto re-exports it; `rg -c 'sakura_proto' crates/sakura-core/src` goes to 0.
3. **Tests are ~32% of both crates and sit inside the modules they test.** `conversion.rs` 2,380 test lines, `simd.rs` 1,131, `width.rs` 1,205, `keymap.rs` 1,072. Changing `append_punctuation_family` (86 lines) means opening a 7,405-line file. *Boundary:* move each `mod tests` to a sibling `*_tests.rs` via `#[path]`, then split by cluster.
4. **Special-candidate synthesis is one feature split three ways.** Families at `width.rs:234–302`, assembly at `conversion.rs:2946–3031`, parsing at `preferences.rs:1058–1117`, tests at `conversion.rs:5469–5617`; nothing outside the crate uses the families. *Boundary:* `conversion/synthesis/punctuation.rs` owns all of it; `width.rs` keeps only the `PunctuationStyle` value.
5. **Ranking evidence (#108) is already a separable sub-language.** `conversion.rs:462–713` (~250 lines) + 4 tests at 5932–6030, consumed by `sakura-engine` 30/19/14/7× and `sakura-neural-proto` 6×. *Boundary:* `conversion/evidence.rs`.
6. **`Dictionary::parse` (249 lines) plus ~500 lines of `validate_*` are a different concern from lookup.** 443–691, 1263–1636, 1873–2152 vs 717–1261. `verification/dictionary-format-v2.md` is the bound artifact but is referenced from **no source file**. *Boundary:* `dictionary/{format,parse,validate,louds,lookup,detail}.rs`, doc cited in `format.rs`.
7. **`preferences.rs` mixes a vocabulary with a codec.** Enums 22–405 vs `parse_preferences` 686 / `serialize_preferences*` 815 / helpers 900–1121. *Boundary:* `preferences/{model,parse,serialize,profiles}.rs`.
8. **`keymap::State`/`Action` are cross-crate vocabulary trapped in a parser** (`keymap.rs:88`, `:128`). *Boundary:* `keymap/vocabulary.rs` separate from `keymap/config.rs`.
9. **`romaji.rs` mixes the zero-alloc hot path with replay diagnostics.** 587/626/815 are covered by `tests/zero_alloc.rs`; 175–342 and 642–950 are diagnostics that must never be on it. *Boundary:* `romaji/{table,fsm,replay,completion}.rs`.
10. **`simd.rs` is three artifacts under one roof, and only one is audited.** `ci/check-simd-assembly.ps1:273` builds the whole 2,058-line module to audit the kernels at 601–1032. *Boundary:* `width/scan/{lut,select,scalar,kernels_x86}.rs`, audit pointed at `kernels_x86.rs`.
11. **Two test-only cycles contradict the documented one-way stack** (`preferences.rs:1609,1612`; `simd.rs:1761,1899`). *Boundary:* move both assertions into the owning module's tests.
12. **`proto/types.rs` mixes three vocabularies** — input/mode 24–431, rendering 478–992, diagnostics 1057–1303. Only the first is wanted by core. *Boundary:* the `sakura-values` line from finding 2.
13. **`proto/message.rs` is 1,219 production lines with 6 public functions and 14/150 commits** — one flat `Request` (99) / `Response` (381) plus hand-written dispatch; adding one message edits five regions. *Boundary:* `message/{request,response,ui_state,header,tags}.rs`.
14. **The facade is not a facade.** Every module is `pub mod` *and* flat-re-exported; ~28 re-exports have no external consumer; consumers use deep paths anyway. *Boundary:* keep `pub mod`, delete the dead flat re-exports, let directory names tell an agent where a change goes.
15. **`conversion.rs` grew 470 lines since 2026-09-01 for one issue (#108)** while its tests already build every fixture in memory (`:5685`). Splitting cost is low and this is the file that keeps growing.

## 9. Target structure proposal

```
crates/sakura-values/                 # NEW leaf; no deps
  src/lib.rs                          # facade
  src/fixed.rs                        # FixedStr, FixedVec, Overflow   (from proto/fixed.rs)
  src/capacity.rs                     # MAX_PREEDIT_BYTES, MAX_SEGMENTS, MAX_CANDIDATES, …
  src/input.rs                        # KeyCode, KeyInput, Modifiers, Mode, InputScope
  src/appearance.rs                   # AppearanceTheme, PadShortcut

crates/sakura-proto/                  # deps: sakura-values
  src/lib.rs                          # + pub use sakura_values::*  (compat)
  src/wire.rs
  src/codec/{input.rs, render.rs, diagnostics.rs}
  src/render.rs                       # Segment, Preedit, Candidate*, Output, ScreenRect
  src/diagnostics.rs                  # EngineTiming*, Fault*
  src/message/{tags.rs, header.rs, request.rs, response.rs, ui_state.rs}
  src/output.rs                       # OutputBuf
  tests/{roundtrip.rs, robustness.rs, zero_alloc.rs}   # unchanged

crates/sakura-core/                   # deps: sakura-values ONLY
  src/text.rs  src/cpu.rs  src/config.rs
  src/width/{policy.rs, punctuation.rs, chars.rs, normalize.rs,
             scan/{lut.rs, select.rs, scalar.rs, kernels_x86.rs}}
  src/romaji/{table.rs, fsm.rs, replay.rs, completion.rs}
  src/keymap/{vocabulary.rs, config.rs}
  src/preferences/{model.rs, parse.rs, serialize.rs, profiles.rs}
  src/dictionary/{format.rs, parse.rs, validate.rs, louds.rs, lookup.rs, detail.rs}
  src/user_dictionary.rs  src/calendar.rs  src/numerals.rs  src/editing.rs
  src/input_repair/{variants.rs, english.rs, punctuation_context.rs}
  src/conversion/{options.rs, input.rs, result.rs, evidence.rs, repair_plan.rs,
                  search/{lattice.rs, viterbi.rs, nbest.rs, cost.rs},
                  candidates/{materialize.rs, dedup.rs, project.rs},
                  ranking/{coherence.rs, quality_gate.rs, it_terms.rs},
                  synthesis/{single_kanji.rs, numerals.rs, dates.rs, punctuation.rs},
                  bridge.rs, raw_repair.rs, fixtures.rs (cfg(test))}
  tests/{zero_alloc.rs, fsm_robustness.rs, width_bench.rs}
```

Dependencies are strictly downward in the list above. Per-module contract:

| module | single responsibility | may depend on | tests | bound verification artifact |
|---|---|---|---|---|
| `sakura-values` | containers + shared value vocabulary | — | own `tests/` | proto `roundtrip.rs` still covers their encodings |
| `proto/wire`,`codec`,`message` | wire framing and message layout | values | `tests/roundtrip.rs`, `robustness.rs` | fuzz job `.github/workflows/ci.yml:209` |
| `proto/output` | zero-allocation output builder | values, wire, message tags | `tests/zero_alloc.rs` | zero-alloc test |
| `core/width/scan/kernels_x86` | vector kernels, scalar-equivalent | `cpu` | `kernels_x86_tests.rs` | `ci/check-simd-assembly.ps1`, `cargo test -p sakura-core --lib -- simd::` |
| `core/width/normalize` | run-based rewrite through the policy | policy, scan, text | sibling | `tests/width_bench.rs`, `tests/zero_alloc.rs` |
| `core/romaji/fsm` | the keystroke hot path | table, text | sibling | `tests/zero_alloc.rs`, `tests/fsm_robustness.rs` |
| `core/keymap/vocabulary` | `State`/`Action`/`Preset` names only | values | sibling | `tests/fsm_robustness.rs` |
| `core/dictionary/format`+`parse`+`validate` | image layout, hostile-input rejection | values | sibling | `verification/dictionary-format-v2.md`, dictc `dictionary_robustness` (`ci.yml:221`) |
| `core/dictionary/lookup` | trie queries over a validated image | format, louds | sibling | — |
| `core/conversion/search` | lattice + Viterbi + A*, bounded | dictionary, cost | sibling | stack-bound tests (`conversion.rs:6176`, `7095`) move here |
| `core/conversion/ranking` | ordering and suppression rules | evidence, candidates | sibling | ime-eval quality corpus |
| `core/conversion/synthesis/*` | one generated-candidate family each | width policy, numerals, calendar | sibling | — |
| `core/conversion/evidence` | authority/origin/tier vocabulary | values | sibling | — |

### Reading set, before vs after

| issue shape | BEFORE (files an agent must open, total lines) | AFTER |
|---|---|---|
| #99 punctuation-family candidate bug | `conversion.rs` 7,405 + `width.rs` 1,985 + `preferences.rs` 1,889 = **11,279** | `conversion/synthesis/punctuation.rs` ≈100 + its tests ≈150 + `width/policy.rs` ≈200 = **≈450** |
| #94/#108 homophone ranking | `conversion.rs` 7,405 + `dictionary.rs` 2,671 + `input_repair.rs` 1,234 = **11,310** | `conversion/evidence.rs` ≈250 + `ranking/*` ≈400 + `candidates/project.rs` ≈250 + tests ≈400 = **≈1,300** |
| #56 romaji FSM edge case | `romaji.rs` 1,797 + `tests/fsm_robustness.rs` 175 + `tests/zero_alloc.rs` 253 = **2,225** | `romaji/fsm.rs` ≈450 + `romaji/table.rs` ≈250 + the two test files 428 = **≈1,130** |
| #109 dictionary image format change | `dictionary.rs` 2,671 + `dictc/lib.rs` 1,987 + conversion fixtures 250 + `verification/dictionary-format-v2.md` 112 = **5,020** | `dictionary/format.rs` ≈120 + `parse.rs` ≈350 + `validate.rs` ≈550 + doc 112 = **≈1,150** + dictc writer |

## 10. Migration sequence

One PR per step, behavior-preserving, compilable at every point, ordered by reading-cost reduction per unit of risk.

**Step 1 — Move every inline `mod tests` in both source trees to sibling `*_tests.rs`.** Mechanical: `#[cfg(test)] #[path = "conversion_tests.rs"] mod tests;`; cut the block verbatim; add `use super::*;`. Sibling private-unit test modules are an allowed final layout. Risk: very low.
`Verify:` `./ci/run-test-quiet.ps1 -Name 'workspace tests' -Command { cargo test --workspace }`; recursively search `crates/sakura-core/src/**` and `crates/sakura-proto/src/**` for inline `mod tests {` bodies.
`Expect:` the Phase 0 metadata baseline test universe is unchanged; no inline `mod tests {` body remains in either tree.

**Step 2 — Fix the two test-only cycles.** Move the `allows_system_entry` assertions into `input_repair_tests.rs`; move the `crate::width` uses out of `simd_tests.rs`. Risk: very low.
`Verify:` `rg -n 'crate::allows_system_entry' crates/sakura-core/src/preferences*`; `rg -n 'crate::width' crates/sakura-core/src/simd*`.
`Expect:` both 0 matches; `cargo test -p sakura-core --lib` unchanged.

**Step 3 — Extract `sakura-values`; keep wire codec in proto (D3).** Move `proto/fixed.rs` verbatim plus value definitions including `InputScope`; add compat `pub use sakura_values::{…}` to `sakura-proto/src/lib.rs`; implement encoding in proto's `wire_types.rs`; repoint core and its Cargo dependency. Risk: medium — the only crate-graph change; compat re-exports mean no consumer outside core is edited.
`Verify:` `rg -c 'sakura_proto' crates/sakura-core/src`; `cargo tree -p sakura-core --depth 1`; `cargo test --workspace`; `cargo test -p sakura-proto --test roundtrip`.
`Expect:` `0`; core's only dependency is `sakura-values`; roundtrip/robustness byte-identical, `PROTOCOL_VERSION` still 22.

**Step 4 — Split `conversion.rs` into `conversion/`.** `mod.rs` keeps the `Converter` struct, the public entry points, and `pub use` of every currently-public item so `sakura_core::conversion::X` and `sakura_core::X` both resolve. Move clusters in this order: `evidence.rs` (462–713), `repair_plan.rs` (715–1038), `bridge.rs` (400–460 + bridge methods), `synthesis/*` (2841–3031, 3241–3497), `ranking/*` (3050–3240, 4495–4578), `candidates/*` (4581–4937), `search/*` (3498–4472). Private methods become `pub(crate) fn(&mut Converter, …)` — preserves behavior exactly and avoids re-plumbing arena lifetimes. Fixtures (5618–5867) become `conversion/fixtures.rs`. Risk: medium.
`Verify:` `cargo test -p sakura-core --lib conversion::`; `cargo test -p dictc`; `cargo test -p sakura-engine`.
`Expect:` 49 conversion tests pass; no consumer path edits in `sakura-engine`, `dictc`, `tools/candidate-snapshot`, `tools/candidate-sweep`.

**Step 5 — Split `dictionary.rs` into `dictionary/`.** `format.rs` (20–103), `parse.rs` (443–691 + readers 2154–2186), `validate.rs` (1263–1636 + 1944–2152), `louds.rs` (1638–1741, 1866–1872), `lookup.rs` (717–1261), `detail.rs` (104–124, 302–397, 1742–1865). Keep `Dictionary<'a>` in `mod.rs`; split by `impl` block. Cite `verification/dictionary-format-v2.md` in `format.rs`. Risk: low-medium.
`Verify:` `cargo test -p sakura-core --lib dictionary::`; `cargo test -p dictc --release --test dictionary_robustness sharded_hostile_dictionary_campaign -- --exact --ignored`.
`Expect:` identical pass counts; dictc's 107 `Dictionary` / 112 `EntryFlags` references unchanged.

**Step 6 — Split `width.rs` + `simd.rs` into `width/`.** `policy.rs` (24–333), `punctuation.rs` (234–302), `chars.rs` (693–826), `normalize.rs` (335–691), `scan/{lut,select,scalar,kernels_x86}.rs` from `simd.rs`. Keep `pub use crate::width::scan as simd;` in `lib.rs` so `sakura_core::simd::startup` (used by the engine) still resolves. Risk: medium — benchmarked, assembly-audited code; the move must not change inlining across the LUT/kernel boundary.
`Verify:` `cargo test -p sakura-core --lib -- simd:: --nocapture`; `pwsh ./ci/check-simd-assembly.ps1`; `cargo test -p sakura-core --release --test width_bench -- --ignored --nocapture`.
`Expect:` 16 simd tests pass; the audit reports the same kernel set; benchmark within run-to-run noise of the pre-split baseline recorded in the PR.

**Step 7 — Split `preferences.rs`, `keymap.rs`, `romaji.rs`.** `preferences/{model,parse,serialize,profiles}.rs`; `keymap/{vocabulary,config}.rs`; `romaji/{table,fsm,replay,completion}.rs`, each with `pub use` in the new `mod.rs`. Risk: low.
`Verify:` `cargo test -p sakura-core --lib`; `cargo test -p sakura-core --test zero_alloc`; `cargo test -p sakura-core --release --test fsm_robustness sharded_fsm_campaign -- --exact --ignored`.
`Expect:` zero-alloc still reports 0 allocations per keystroke; FSM campaign completes with the same shard/seed defaults.

**Step 8 — Prune the facade after caller proof (D4).** Deleting a `pub use` is a public API change. Enumerate all workspace packages/targets with `cargo metadata`, supported external API references, and docs; remove only exports whose callers are proven zero and keep every unproven export. Do not preselect a count. Risk: low in-tree, externally visible.
`Verify:` `cargo build --workspace --all-targets`; retain a per-item caller report from metadata targets, docs, and supported API checks.
`Expect:` clean build; every removed export has proven caller count zero.

**Step 9 — Preserve the research candidate-bound feature while splitting (D11).** Move the `research-top32` / `research-wide-candidates` cfg sites with their owning logic and keep the current default/research behavior; do not replace the research feature with a runtime bound in this refactor. Risk: medium because arena sizing underpins zero allocation.
`Verify:` test default and research-feature builds through the quiet wrapper, including zero-allocation and conversion suites; audit that each cfg moved with its logic.
`Expect:` feature names and both behaviors remain available; zero-allocation and candidate ceilings are unchanged.

**Step 10 — Split `proto/message.rs` and `proto/types.rs`.** `message/{tags,header,request,response,ui_state}.rs` with wire tags kept in one file so a tag collision stays a one-file check; `types.rs` residue splits into `render.rs` and `diagnostics.rs`. Risk: low.
`Verify:` `cargo test -p sakura-proto`; `cargo test -p sakura-proto --release --test robustness sharded_protocol_campaign -- --exact --ignored --nocapture`; `rg -n 'PROTOCOL_VERSION: u16 = 22' crates/sakura-proto/src/lib.rs`.
`Expect:` roundtrip passes, fuzz campaign completes, `PROTOCOL_VERSION` untouched — a version bump here would mean the split was not behavior-preserving.
