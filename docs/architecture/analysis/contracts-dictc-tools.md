# Structural analysis: contract/plumbing crates, dictc, offline tools (HEAD a370477)

Read-only. No repository file was modified; no cargo command was run.

## 1. Crate dependency graph

```
sakura-proto        → (none)                                         lib   win-feat 0  ext 0
sakura-core         → sakura-proto                                   lib   win-feat 0  ext 0
sakura-ai-proto     → (none)                                         lib   win-feat 0  ext 0
sakura-neural-proto → (none)                                         lib   win-feat 0  ext 0   publish=false
sakura-ipc          → sakura-proto, windows-core, windows            lib   win-feat 8  ext 0
sakura-reg          → windows-core, windows                          lib   win-feat 13 ext 0
sakura-engine       → sakura-core, sakura-ai-proto, sakura-ipc,      lib+bin win-feat 10 ext 0
                      sakura-neural-proto, sakura-proto, sakura-reg, windows
                      dev→ dictc, dev→ sakura-ime-eval (tools/ime-eval)
sakura-tsf          → sakura-ipc, sakura-proto, sakura-reg, windows-core, windows   cdylib+bin win-feat 16 (bin gated on `e2e-host`)
sakura-renderer     → sakura-ipc, sakura-proto, windows-core, windows               bin   win-feat 23
sakura-settings     → sakura-ai-proto, sakura-core, sakura-engine, sakura-ipc,      2 bins win-feat 18
                      sakura-proto, sakura-reg, windows-core, windows
sakura-regtool      → sakura-ipc, sakura-proto, sakura-reg, windows  bin   win-feat 3
sakura-logon        → sakura-reg                                     lib+bin win-feat 0
sakura-ai-worker    → sakura-ai-proto, serde_json, windows           bin   win-feat 5  ext 1
sakura-neural-worker→ ort, serde, serde_json, sha2                   bin   win-feat 0  ext 4
dictc               → sakura-core, sakura-neural-proto, sakura-proto, quick-xml, flate2, serde, serde_json, sha2, unicode-normalization   lib+12 bins ext 6
tools/ime-eval      → sakura-core, sakura-ipc, sakura-proto, serde, serde_json, sha2   lib+bin ext 3
tools/candidate-snapshot → sakura-core, serde, serde_json, sha2      bin   OWN [workspace]
tools/candidate-sweep    → sakura-core                               bin   OWN [workspace]
```

Layer picture:

```
L0 value types   sakura-proto        sakura-ai-proto   sakura-neural-proto
L1 portable core  sakura-core (→ proto)
L2 Windows plumbing  sakura-ipc(→proto)      sakura-reg(→nothing)
L3 processes      sakura-engine ── sakura-tsf ── sakura-renderer ── sakura-regtool ── sakura-logon
L4 control panel  sakura-settings (→ engine!)
L5 offline        dictc, tools/ime-eval, tools/candidate-{snapshot,sweep} [outside workspace]
       dev edges:  sakura-engine ⇢ dictc,  sakura-engine ⇢ sakura-ime-eval   (L3 ⇢ L5, upward)
```

Edges that break clean layering:

- **`sakura-core → sakura-proto`** (`crates/sakura-core/Cargo.toml:12-13`). 18 references, all value-type family (`FixedStr`/`Overflow`, `Mode`, `KeyCode`, `AppearanceTheme`, `MAX_PREEDIT_BYTES`, `MAX_CANDIDATES`).
- **`sakura-settings → sakura-engine`** (`crates/sakura-settings/Cargo.toml:23`). Whole engine linked to reach `sakura_engine::input_history::*`, `InputHistoryService::open`, `learning::*`, `fault_injection` — five uses.
- **`sakura-engine dev→ dictc`** (`crates/sakura-engine/Cargo.toml:41`). 112 call sites of four functions: `parse_entries` (40), `compile` (35), `parse_connection` (35), `compile_with_details` (2), across `src/dictionary.rs`, `src/dispatch.rs`, `src/prediction.rs`, `src/server.rs`, `src/user_dictionary.rs`, `src/*_tests.rs`, `tests/common/mod.rs`, `tests/cross_commit_learning.rs`, `tests/shipped_dictionary_ranking.rs`, `tests/zero_alloc_dispatch.rs`.
- **`sakura-engine dev→ tools/ime-eval`** (`crates/sakura-engine/Cargo.toml:42`). One test file: `crates/sakura-engine/tests/pipe_round_trip.rs:36-37`.
- **`dictc → sakura-neural-proto`** — one consumer, `crates/dictc/src/context_dataset.rs:14`.
- **`dictc → sakura-proto`** — `crates/dictc/src/lib.rs:13` (`MAX_PREEDIT_BYTES`), `crates/dictc/src/bin/compound_homophone_scan.rs:759`, plus three tests.
- **`tools/candidate-snapshot` and `tools/candidate-sweep` are outside the root workspace** (each declares its own `[workspace]`) yet path-depend on `crates/sakura-core`. Documented for snapshot (`tools/candidate-snapshot/src/main.rs:3-5`); undocumented for sweep.

## 2. sakura-ipc anatomy

`crates/sakura-ipc/src/lib.rs:27-37` declares five modules and re-exports 15 items.

| module | lines | owns |
|---|---|---|
| `security.rs` | 1,350 (422 test) | pipe naming, SDDL/DACL/mandatory label, `CLIENT_ACCESS` mask, AppContainer admission, client trust classification, **server image-path policy (#104)** |
| `transport.rs` | ~1,100 | one pipe instance, overlapped I/O, `u32`-prefixed framing, `Accept`, `ConnectionProbe`, `Fault` taxonomy |
| `client.rs` | ~650 | connect (+verified), deadline arithmetic, late-reply discard by request id, `Desynchronized` retirement |
| `diagnostics.rs` | ~880 | two bounded on-disk fixed-record logs (timeouts, disconnects), magics `SKTO`/`SKDC` |
| `debug_trace.rs` | ~190 | developer TSV branch trace — unrelated to the pipe |

Consumers: 32 files across `sakura-engine` (6 src + 7 tests), `sakura-tsf` (2), `sakura-renderer` (1 src + 5 tests), `sakura-settings` (8), `sakura-regtool` (1), `sakura-proto/src/output.rs` (doc-comment only), `tools/ime-eval` (1). Item frequency: `Client` 11, `diagnostics::*` 10, `debug_trace::*` 12, `pipe_name`/`pipe_name_for` 6, `Fault::*` 8, `ConnectionProbe` 5, `security::*` 2, `MAX_INSTANCES` 1.

Separable change reasons (four of five independent):

1. *Framing/transport* — `transport.rs:595-660` (`read_frame`, `read_frame_with_deadline`, `wait_for_bytes`).
2. *Security* splits in two with different consumers: pipe admission (`security.rs:88-141`, invariant narrative `:1-57`) vs. server identity/image-path policy (`security.rs:151-320, 453-513`, used by the client path and `crates/sakura-engine/tests/appcontainer.rs`). `security.rs:219-228` records the #104 defect.
3. *Client deadline / late-reply* — `client.rs:239-460`, invariants at `client.rs:18-39`. The #134 shared-callback deadline is **not** here: it lives at `crates/sakura-tsf/src/callback_deadline.rs:12`; `sakura-ipc` only receives a `Duration`.
4. *Diagnostics logs* — `diagnostics.rs:27-32` (`SKTO`, `SKDC`, `FORMAT_VERSION: u16 = 1`, `RECORD_BYTES = 32`, 1 MiB ceilings). Written by sakura-tsf, read by sakura-settings; neither needs the pipe.
5. *`debug_trace.rs`* — not about the pipe at all.

Test placement: 422 of 1,350 security lines and ~350 transport lines are inline `#[cfg(test)]`; the only integration test is `crates/sakura-ipc/tests/client_server.rs`.

## 3. sakura-reg anatomy

13 modules (`crates/sakura-reg/src/lib.rs:13-45`), one integration test, 13 `windows` features.

| module | size | change reason | consumers |
|---|---|---|---|
| `guids.rs` | 4.4 KB | frozen identity | sakura-tsf (`display_attributes.rs:25`, `composition.rs:23,1696`) |
| `com_server.rs` | 3.6 KB | CLSID registry entries | regtool, logon, settings/bootstrap.rs |
| `profile.rs` | 6.8 KB | TSF profile + categories | reg internal, regtool, logon |
| `registry.rs` | 16.2 KB | RAII `RegKey` primitive | reg internal |
| `module.rs` | 2.2 KB | `GetModuleFileNameW` | sakura-tsf (`engine.rs`, `exports.rs`) |
| `wide.rs` | 2.0 KB | UTF-16 helpers | reg internal |
| `user_profile.rs` | 8.1 KB | HKCU input list | regtool, logon |
| `launcher.rs` | 19.6 KB | Task Scheduler logon task | regtool, logon, own test |
| `maintenance.rs` | 14.5 KB | **elevated SYSTEM** cleanup task | regtool only |
| `payloads.rs` | 10.1 KB | payload directory GC | regtool only |
| `diagnostics.rs` | 2.7 KB | machine-wide WER `LocalDumps` | regtool only |
| `vscode_diagnostics.rs` | 20.4 KB | **VS Code-specific** WER policy | regtool only |
| `user_preferences.rs` | 14.0 KB | **AI prefs + Credential Manager** | sakura-engine `ai_text.rs`, sakura-settings `ui.rs`, sakura-tsf `text_service.rs:68` |

**Verdict: a "misc Windows utilities" crate.** `lib.rs:1-9` claims the crate is only GUIDs/registry/TSF categories and that "nothing here runs on the input path"; both are now false — `user_preferences::read_ai_text_key` is called from the TSF frontend on the AI-trigger path (`crates/sakura-tsf/src/text_service.rs:68`), and `user_preferences.rs:345-436` (`api_key_is_saved`, `read_api_key`, `write_api_key`, `clear_api_key`) owns Windows Credential Manager secrets. `unregister_all` at `lib.rs:76` exists because reverse-order teardown is a safety property (DESIGN 12.1).

Blast radius of a split is clean: `{maintenance, payloads, diagnostics, vscode_diagnostics}` (47.7 KB) has **exactly one** consumer, `sakura-regtool`.

## 4. Worker protocol crates and workers

**`sakura-ai-proto`** (1 file, 19 KB, zero deps): `REQUEST_MAGIC b"SAIR"`, `RESPONSE_MAGIC b"SAIS"`, `VERSION: u16 = 1`, `MODEL = "gpt-5.6-luna"`, `MAX_TEXT_BYTES` 4 KiB, `MAX_METADATA_BYTES` 128, `MAX_ENDPOINT_BYTES`/`MAX_API_KEY_BYTES` 2 KiB (`crates/sakura-ai-proto/src/lib.rs:6-14`). Inline tests `:321+` include an independent wire oracle (`:414`) and a `Debug`-redaction test (`:395`). Both sides link it — `sakura-ai-worker` and `crates/sakura-engine/src/ai_text.rs:16`. **No duplication.** Engine-local constants (`ai_text.rs:21-26`) are lifecycle policy, correctly not in the protocol.

**`sakura-neural-proto`** (1 file, 29 KB, zero deps, `publish = false`): `CONTRACT_VERSION 1`, `WIRE_VERSION 2`, `MAGIC b"SCV1"`, `MAX_FRAME 32 KiB`, `MAX_RERANK_CANDIDATES 6`, `MAX_PREDICTION_DEADLINE_MS 10`, `MAX_RERANK_DEADLINE_MS 500` (`crates/sakura-neural-proto/src/lib.rs:12-27`); `ContextSnapshot::validate:174`, codecs `:316/340/407/443`.

**The duplication finding.** `crates/sakura-neural-worker/Cargo.toml` does **not** depend on `sakura-neural-proto`. The worker hand-writes a different protocol at `crates/sakura-neural-worker/src/protocol.rs:3-8`:

```
MAX_FRAME = 32*1024;  MAX_CANDIDATES = 6;  MAX_CANDIDATE_BYTES = 3*1024;
REQUEST_MAGIC = 0x524e_4b53;  RESPONSE_MAGIC = 0x534e_4b53;  VERSION: u16 = 1;
```

and the engine re-declares the same literals a third time at `crates/sakura-engine/src/long_conversion.rs:25-30` (`MAXIMUM_MODEL_CANDIDATES = 6`, `MAXIMUM_FRAME_BYTES = 32*1024`, `REQUEST_MAGIC = 0x524E_4B53 // SKNR`, `RESPONSE_MAGIC = 0x534E_4B53 // SKNS`, `PROTOCOL_VERSION = 1`, `WORKER_RESPONSE_TIMEOUT = 500 ms`).

So the live reranker protocol exists in two hand-written copies with no shared crate, while the crate named "the neural protocol" (`SCV1`, wire v2) is dormant: its only consumers are `crates/sakura-engine/src/context_intelligence.rs:16` (imports `Fingerprint` only) and `crates/dictc/src/context_dataset.rs:14`. `docs/neural-context-contract-v1.md` documents `SCV1` and says nothing about `SKNR`/`SKNS`.

## 5. dictc anatomy

`crates/dictc/src/lib.rs` is 1,987 lines and **contains zero inline tests**. Pipeline in file order: parse (`parse_entries:378`, `parse_category_entries:388`, `parse_mozc_entries:527`, `parse_connection:656`, `parse_mozc_connection:601`) → trim (`TrimPolicy:136`, `MozcTrimmer:162`) → merge/overlay (`merge_entries:1164`) → details (`extract_entry_details:1204`, `attach_entry_details:1221`, `encode_details:1654`) → optional tables (`OptionalTables:802`, `encode_single_kanji:985`, `encode_boundaries:1042`) → image write (`compile:774`, `compile_with_details:781`, `compile_with_tables:815`, `build_trie:1435`, `flatten_trie:1473`, `write_entry:1570`, `front_code:1621`, `assemble_image:1814`). Bounds at `lib.rs:31-41`.

| bin | lines | purpose | lib modules used |
|---|---|---|---|
| `dictc` (`src/main.rs`) | 663 | the image compile CLI | root, `wordnet` |
| `glossary-import` | — | Mozc→overlay TSV import | `glossary`, root |
| `mozc-trim` | — | trims Mozc shards | `category`, root |
| `inflection-expand` | 152 | conjugation expansion | `inflection`, root |
| `corpus-eval` | 507 | conversion accuracy | **none** (`sakura_core` only) |
| `neural-eval` | 966 | reranker offline eval | **none** (`sakura_core` only) |
| `category-split` | — | splits sources by category | `category`, root |
| `llm-detail-targets` | — | committed target manifests | `category`, `llm_detail_targets`, root |
| `llm-detail-drafts` | — | draft JSONL | `llm_detail_targets`, `llm_details` |
| `llm-detail-promote` | — | promotes reviewed drafts | `llm_detail_targets`, `llm_details`, root |
| `context-dataset` | — | neural context dataset | `context_corpus`, `context_dataset`, `context_rerank_import` |
| `compound-homophone-scan` | 850 | homophone survey | **none** (`sakura_core`, `sakura_proto:759`) |

Module sizes (lines): `llm_details` 2,069; `lib` 1,987; `category` 1,395; `context_dataset` 1,326; `llm_detail_targets` 1,196; `context_rerank_import` 1,053; `glossary` 917; `inflection` 771; `context_corpus` 608; `segmenter` 454; `single_kanji` 402; `wordnet` 362.

**Single-binary lib modules:** `context_corpus` (608) and `context_rerank_import` (1,053) → only `bin/context_dataset.rs`; `context_dataset` (1,326) → that binary plus `context_rerank_import`; `inflection` (771) → `bin/inflection_expand.rs` + `tests/inflection.rs`. 3,758 lines (19 % of `src`) reachable from one binary. Three binaries totalling 2,323 lines import nothing from the dictc library at all.

Tests: 18 files, 8,663 lines, none inside `src/lib.rs` (largest: `conversion.rs` 1,271; `source_format_conditions.rs` 1,051; `overlay_conditions.rs` 910; `overlay_properties.rs` 645; `image.rs` 536). Data/env coupling: `tests/conversion_priorities.rs:25`, `tests/curated_terms.rs:103`, `tests/curated_details.rs:20`, `tests/homophone_compounds.rs:11,75` read `data/`; `tests/conversion.rs:1111` reads `SAKURA_PHASE2_DICTIONARY`; `tests/dictionary_robustness.rs:139`, `tests/overlay_properties.rs:606`, `tests/source_format_properties.rs:429` read env-var seeds/iterations. `crates/dictc/src/lib.rs` changed in **18 of the last 150 commits** — the highest churn of any file examined.

**Format ownership.** One canonical definition, in the *reader* crate: `crates/sakura-core/src/dictionary.rs:20-93` (`pub mod image_format`: `MAGIC = b"SKRADIC\0"` :21, `VERSION 1` :23, `VERSION_V2 2` :24, `TAG_*` :29-66, `MATRIX_MAGIC b"MSP1"` :87, `BOUNDARY_MAGIC b"SBD1"` :93). dictc aliases it at `crates/dictc/src/lib.rs:12` and uses 36 distinct `format::*` items. The conformance suite lives in the writer crate (`crates/dictc/tests/image.rs`, `tests/dictionary_robustness.rs`) and never runs under `cargo test -p sakura-core`. Doc: `verification/dictionary-format-v2.md` (#109).

## 6. tools anatomy

**`tools/ime-eval`** — 24 `pub mod`s (`src/lib.rs:7-30`), ~7,700 lines; `cli.rs:6-20` imports 15 siblings. Largest: `quality.rs` 1,426, `ranking_comparison.rs` 1,446, `cli.rs` 834, `comparison.rs` 793, `capture_engine.rs` 559. Own tests: `tests/comparison.rs` 30 KB, `phase1.rs` 13 KB, `quality_stage1.rs` 12 KB. Its lib doc forbids shipping dependence (`src/lib.rs:3-5`), yet the engine dev-dep means `cargo test -p sakura-engine` builds all 24 modules for two imports at `crates/sakura-engine/tests/pipe_round_trip.rs:36-37`.

**`tools/candidate-snapshot`** (own `[workspace]`, version 1.0.24, feature `modern`) — isolation reason documented in code (`src/main.rs:3-5`: copy the nested workspace into each release worktree so it links that worktree's own `sakura-core`). Constants `:21-30`.

**`tools/candidate-sweep`** (own `[workspace]`, version 1.0.32, feature `wide = ["sakura-core/research-wide-candidates"]`) — `src/main.rs:1-11` gives no isolation reason; plausible one is feature unification.

**Both are invisible to every gate.** `rg 'candidate-snapshot|candidate-sweep' .github ci scripts README.md DESIGN.md docs verification` returns nothing. Mentions exist only in `tools/ime-eval/README.md:10,33,43,44`, `tools/ime-eval/src/ranking_comparison.rs:6`, `tools/ime-eval/tests/comparison.rs:483,519,561,616`, `crates/sakura-core/src/conversion.rs:306,1782`, and `eval/baselines/ranking-comparison-issue93/*.json`. `ci/dep-policy.ps1:131+` (`Get-WorkspaceCrateName`) derives its crate list from the root `members` array, so neither tool's `serde`/`serde_json`/`sha2` graph is audited, and neither is built by `cargo test --workspace` or clippy.

## 7. Contract documentation map

| contract | canonical code | canonical doc | cross-referenced? | version constant |
|---|---|---|---|---|
| IPC wire protocol | `crates/sakura-proto/src/lib.rs:1-38`, `message.rs` | DESIGN.md §5.5 (:772), §7 (:1081) | yes, both ways | `PROTOCOL_VERSION` |
| Pipe naming / DACL / label | `sakura-ipc/src/security.rs:1-57, 88-141` | DESIGN.md §7, §9 (:1307) | doc→§ yes; §→file no | none |
| Server image-path policy (#104) | `security.rs:151-320, 453-513` | `verification/appcontainer-policy-evidence.md` | code names #104 at `:219` | none |
| Client deadline / late reply | `client.rs:18-39, 239-460`; `Fault::Desynchronized` `transport.rs:132` | `verification/issue-141-tdd.md` | no | none |
| TSF callback deadline (#134) | `sakura-tsf/src/callback_deadline.rs:12` | `verification/tsf-callback-deadline.md:1,5` | doc→behaviour only | none |
| IPC diagnostics logs | `sakura-ipc/src/diagnostics.rs:27-32` | module `//!` only | no | `FORMAT_VERSION = 1` |
| Debug branch trace | `debug_trace.rs:16-18` | module `//!` only | no | header string |
| Dictionary image v2 | `sakura-core/src/dictionary.rs:20-93` | `verification/dictionary-format-v2.md`; DESIGN §6.1 (:957) | doc→contract yes | `VERSION 1`/`VERSION_V2 2` |
| AI worker protocol | `sakura-ai-proto/src/lib.rs:6-14` | `verification/ai-text-verification.md`; CLAUDE.md §58 | partial | `VERSION = 1` |
| Neural context (dormant) | `sakura-neural-proto/src/lib.rs:12-29` | `docs/neural-context-contract-v1.md` | yes, both ways | `CONTRACT_VERSION 1`, `WIRE_VERSION 2` |
| **Reranker worker v1 (live)** | **two copies**: `sakura-neural-worker/src/protocol.rs:3-8`, `sakura-engine/src/long_conversion.rs:25-30` | CLAUDE.md Issue #32 only | no | `1`, duplicated |
| Registry/GUID layout | `sakura-reg/src/guids.rs` | DESIGN.md §12.1 (:1417) | code states the freeze rule | none |
| Update signing v2 | `sakura-settings/src/update_trust.rs:29-33, 807, 995` | `verification/update-signing-v2.md`; DESIGN §12.3 (:1499) | yes, detailed | `EMBEDDED_TRUST_EPOCH = 1` |
| Keymap TOML | `sakura-core/src/keymap.rs:1-12`; `data/keymap-*.toml` | DESIGN.md §2, §8 (:1118) | crate doc names both § | none |
| Config file format | `sakura-core/src/config.rs:171` | **none found** | no | none |
| Learning store | `sakura-engine/src/learning.rs:29-33` (`SKLR`) | `verification/write-journal-authority.md` (adjacent) | no | `LEARNING_FORMAT_VERSION = 3` |
| Input-history store | `sakura-engine/src/input_history.rs:36-49` (`SKIH`) | DESIGN §5.4.1; CLAUDE.md | yes | `= 2`, min 1 |
| Engine admin / fault injection | `sakura-proto/src/types.rs:1194,1267`; `sakura-engine/src/fault_injection.rs` | **none found** | no | none |

Two contracts have no document at all; three have only a module comment or a duplicated literal.

## 8. Findings

1. **The live reranker protocol has two hand-written copies and no owning crate** — `sakura-neural-worker/src/protocol.rs:3-8` vs `sakura-engine/src/long_conversion.rs:25-30`. *Boundary:* one `sakura-rerank-proto` leaf crate owned by both sides.
2. **`sakura-neural-proto` is dormant and misnamed** — live consumers are `context_intelligence.rs:16` (`Fingerprint` only) and `dictc/src/context_dataset.rs:14`. *Boundary:* rename to `sakura-context-proto`; move `Fingerprint` to a leaf type crate.
3. **`sakura-reg` mixes four unrelated change reasons and its doc is now false** (`lib.rs:1-9` vs `sakura-tsf/src/text_service.rs:68`). *Boundary:* `sakura-reg` (identity+registration) / `sakura-install-maintenance` (regtool-only) / `sakura-user-prefs`.
4. **A registration crate owns the only secret store** — `sakura-reg/src/user_preferences.rs:345-436`. *Boundary:* a dedicated crate so a security review reads one file, not thirteen.
5. **`debug_trace.rs` is inside `sakura-ipc` but is not about the pipe**; 12 of ~50 `sakura_ipc::` references target it. *Boundary:* move it and `diagnostics.rs` into `sakura-diagnostics`.
6. **The IPC diagnostics log is a versioned on-disk contract with no document** (`diagnostics.rs:27-32`). *Boundary:* own crate + `docs/contracts/ipc-diagnostics-log.md`.
7. **`security.rs` (1,350 lines) fuses two policies with different consumers** — admission (`:88-141`) vs server trust (`:151-320, 453-513`, #104). *Boundary:* `security/admission.rs` + `security/server_trust.rs`.
8. **`sakura-settings` links all of `sakura-engine` for five uses.** *Boundary:* extract `sakura-stores` (`learning.rs`, `input_history.rs`).
9. **`sakura-engine` dev-depends on all of `dictc` for four functions and on all of `tools/ime-eval` for one test file** (`Cargo.toml:41-42`). *Boundary:* a `test-dictionary` fixture façade and a small `semantic-fixtures` crate.
10. **`dictc/src/lib.rs` (1,987 lines) has zero inline tests** and the highest churn measured (18/150 commits). *Boundary:* move the format conformance suite next to the format definition in `sakura-core`.
11. **19 % of dictc's library serves one binary** — `context_corpus` + `context_dataset` + `context_rerank_import` = 2,987 lines for `bin/context_dataset.rs`. *Boundary:* a `dictc-context-dataset` unit.
12. **Three dictc binaries import nothing from dictc** (`bin/corpus_eval.rs`, `bin/neural_eval.rs`, `bin/compound_homophone_scan.rs`, 2,323 lines). *Boundary:* move to a `tools/conversion-eval` member.
13. **`dictc → sakura-proto` is one constant and `dictc → sakura-neural-proto` is one module.** *Boundary:* a `sakura-limits` leaf crate holding shared bounds.
14. **`tools/candidate-sweep` is outside the workspace with no stated reason and no CI reference**; `candidate-snapshot`'s isolation is justified in code but nowhere in `docs/` or `ci/`. *Boundary:* fold sweep in; document snapshot's exemption.
15. **`ci/dep-policy.ps1` cannot see nested workspaces** — `Get-WorkspaceCrateName` (`:131+`) derives names from the root `members` regex, so `tools/candidate-snapshot`'s `serde`/`serde_json`/`sha2` are unaudited against the DESIGN §3.1 allowlist.

## 9. Target structure proposal

```
crates/
  sakura-limits/            NEW leaf: shared bounds + Fingerprint. deps: none.
  sakura-proto/             wire messages. deps: sakura-limits.
  sakura-core/              portable core. deps: sakura-limits (NOT sakura-proto).
                            + tests/dictionary_format.rs  ← MOVED from dictc/tests/{image,dictionary_robustness}.rs
  sakura-ipc/               ONLY the pipe.
    src/transport.rs            framing + one instance
    src/security/admission.rs   name, SDDL, label, CLIENT_ACCESS   → appcontainer-policy-evidence.md
    src/security/server_trust.rs image path / reparse (#104)       → appcontainer-policy-evidence.md
    src/client.rs               deadline + late-reply rule         → issue-141-tdd.md
  sakura-diagnostics/       NEW: diagnostics.rs + debug_trace.rs. deps: std only.
  sakura-reg/               identity + registration ONLY (guids, com_server, profile, registry, module, wide, user_profile, launcher)
  sakura-install-maintenance/ NEW: maintenance, payloads, diagnostics(WER), vscode_diagnostics. single consumer: sakura-regtool
  sakura-user-prefs/        NEW: user_preferences + credentials
  sakura-stores/            NEW: learning.rs + input_history.rs (cuts settings→engine)
  sakura-ai-proto/          unchanged
  sakura-rerank-proto/      NEW: SKNR/SKNS v1, one definition
  sakura-context-proto/     RENAMED from sakura-neural-proto (SCV1)
  dictc-core/               NEW lib: parse → trim → overlay → boundaries → details → image write
  dictc/                    thin: main.rs + 8 dictionary binaries
tools/
  dictc-context-dataset/    NEW member: context_corpus/dataset/rerank_import + its binary
  conversion-eval/          NEW member: corpus-eval, neural-eval, compound-homophone-scan
  ime-eval/                 unchanged member
  semantic-fixtures/        NEW tiny member for pipe_round_trip.rs
  candidate-sweep/          BECOMES a workspace member
  candidate-snapshot/       stays nested; reason recorded in docs/contracts/ + dep-policy
docs/contracts/             NEW: one file per contract naming owning crate + verification artifact
```

Reading set for four representative issues:

| issue shape | BEFORE | AFTER |
|---|---|---|
| #88/#104 pipe authorization | `security.rs` 1,350 + `lib.rs` 38 + `transport.rs` create path ~120 + `client.rs` connect ~120 + `tests/client_server.rs` 232 + evidence doc + `sakura-engine/tests/appcontainer.rs` ≈ **2,100** | `security/server_trust.rs` ~450 + `docs/contracts/server-trust.md` ~80 + `tests/server_trust.rs` ~200 ≈ **730** |
| #109 image format change | `sakura-core/src/dictionary.rs` ~2,000 + `dictc/src/lib.rs` 1,987 + `dictc/tests/image.rs` 536 + `tests/dictionary_robustness.rs` 167 + doc ≈ **4,900** | `image_format.rs` ~120 + reader ~800 + `sakura-core/tests/dictionary_format.rs` ~700 + `dictc-core/src/image_write.rs` ~600 ≈ **2,300** |
| #112 dictionary content restore | `llm_details.rs` 2,069 + `llm_detail_targets.rs` 1,196 + `main.rs` 663 + `bin/llm_detail_promote.rs` ~411 + `tests/curated_details.rs` 330 ≈ **4,700** | `dictc-core/src/details/{release,targets}.rs` ~2,000 + promote binary ~411 + test 330 ≈ **2,750** |
| #134 IPC deadline change | `callback_deadline.rs` 60 + `engine.rs` deadline sites ~400 + 3 sites in 6,800-line `text_service.rs` + `client.rs` 650 + `transport.rs` deadline paths ~150 + doc ≈ **1,700** | `callback_deadline.rs` 60 + `client.rs` 650 + `docs/contracts/ipc-wire.md#deadlines` ~60 ≈ **770** |

## 10. Migration sequence

Every step is one PR, compiles at each point, keeps old paths alive with `pub use` re-exports.

**Step 1 — `docs/contracts/` index.** One file per §7 row: owning crate, canonical `path:line`, verification artifact, version constant; back-reference comment at each canonical site. Risk: none. *Verify:* `rg -c '^' docs/contracts/*.md` lists ≥15 files; `rg -n 'docs/contracts/' crates | wc -l` ≥ 15. *Expect:* every contract has a document and a code→doc link.

**Step 2 — unify the reranker protocol.** Extract `crates/sakura-rerank-proto` from `sakura-neural-worker/src/protocol.rs`; the worker `pub use`s it; replace `long_conversion.rs:25-30` literals with imports. Add to root `members` and `$RuntimeCrates` (`ci/dep-policy.ps1:121-124`). Risk: medium — assert byte-equality of old and new literals before deleting either. *Verify:* `rg -n '0x524[eE]_4[bB]53' crates` returns one hit; quiet wrapper on `cargo test -p sakura-rerank-proto -p sakura-neural-worker -p sakura-engine`; `pwsh ./ci/dep-policy.ps1 -SelfTest` then `pwsh ./ci/dep-policy.ps1`. *Expect:* one definition site, all tests pass, no new packages in `Cargo.lock`.

**Step 3 — split `sakura-diagnostics` out of `sakura-ipc`.** Move `diagnostics.rs` + `debug_trace.rs` verbatim; keep re-exports. Risk: low. *Verify:* `cargo test -p sakura-ipc -p sakura-diagnostics`; `rg -n 'windows' crates/sakura-diagnostics/Cargo.toml` empty; `pwsh ./ci/dep-policy.ps1`. *Expect:* `sakura-ipc` drops ~1,070 lines; both log formats platform-independently testable.

**Step 4 — split `security.rs` into `admission.rs` + `server_trust.rs`.** Move the 422 inline test lines into `tests/admission.rs` and `tests/server_trust.rs`. *Verify:* `cargo test -p sakura-ipc`; each new file < 600 lines. *Expect:* an #104-shaped issue reads `server_trust.rs` alone.

**Step 5 — `sakura-install-maintenance` out of `sakura-reg`** (maintenance, payloads, diagnostics, vscode_diagnostics). New crate depends on `sakura-reg` for `registry::RegKey`. Keep shims one release. *Verify:* `cargo test -p sakura-reg -p sakura-install-maintenance -p sakura-regtool`; `rg -n 'vscode_diagnostics|maintenance|payloads' crates/sakura-tsf crates/sakura-engine` empty. *Expect:* the TSF DLL's registration dependency no longer contains an elevated SYSTEM task installer.

**Step 6 — `sakura-user-prefs` out of `sakura-reg`.** Owner-decision: moves `Win32_Security_Credentials` to a new crate name to be added to `$RuntimeCrates`. *Verify:* `rg -n 'Credentials' crates/*/Cargo.toml` names only `sakura-user-prefs`; `pwsh ./ci/dep-policy.ps1`. *Expect:* one crate owns every secret-bearing API.

**Step 7 — `sakura-limits` leaf crate.** Move shared bounds and `FixedStr`/`Overflow` there; `sakura-proto` re-exports; then drop `dictc → sakura-proto`. Owner-decision: new allowlisted member. *Verify:* `rg -n 'sakura_proto' crates/sakura-core/src | wc -l` → 0; `cargo test -p sakura-core -p sakura-proto`; `pwsh ./ci/dep-policy.ps1 -SelfTest`. *Expect:* the portable core no longer depends on the wire protocol.

**Step 8 — `sakura-stores` out of `sakura-engine`; cut settings→engine.** Move `learning.rs` and `input_history.rs` with their `verification/history-*.md` cross-references. Risk: medium (DPAPI store formats with live migration v1→v3). *Verify:* `cargo test -p sakura-stores -p sakura-engine -p sakura-settings`; `rg -n 'sakura-engine' crates/sakura-settings/Cargo.toml` empty. *Expect:* the settings binary stops linking dispatch/conversion.

**Step 9 — dictc split, three PRs.** (a) extract `dictc-core`; (b) move the context-dataset unit to `tools/dictc-context-dataset`; (c) move three non-dictc binaries to `tools/conversion-eval`. Owner-decision: `$OfflineDetailParserCrates` (`ci/dep-policy.ps1:125-129`) re-scoped. Risk: medium-high. *Verify:* `cargo build --workspace --bins`; quiet wrapper on `cargo test -p dictc-core -p dictc`; rebuild the shipped image and compare against 39,349,040 bytes / SHA-256 `b7d08643…` in `data/dictionary-build.report.json`; dep-policy self-test + run. *Expect:* byte-identical image; `dictc-core` under ~9,000 lines.

**Step 10 — move the format conformance suite to `sakura-core`.** `dictc/tests/image.rs` + `tests/dictionary_robustness.rs` → `crates/sakura-core/tests/dictionary_format.rs`. Owner-decision: accept a `sakura-core dev→ dictc-core` edge or move a minimal writer helper into `sakura-core` behind a test-only feature. *Verify:* `cargo test -p sakura-core --test dictionary_format`; `rg -n 'image_format' crates/dictc/tests` empty. *Expect:* the reader's conformance suite runs under `cargo test -p sakura-core`.

**Step 11 — fixture crate; drop both engine dev edges.** Create `tools/semantic-fixtures` for `pipe_round_trip.rs:36-37`; replace the `dictc` dev-dep with a four-function façade. *Verify:* `rg -n 'sakura-ime-eval|dictc' crates/sakura-engine/Cargo.toml` shows only the façade; `cargo test -p sakura-engine`. *Expect:* engine tests stop building ime-eval's 24 modules.

**Step 12 — fold `candidate-sweep` in; document `candidate-snapshot`.** Delete the nested `[workspace]` from `tools/candidate-sweep/Cargo.toml`, add to root `members`, drop its `Cargo.lock`; keep `candidate-snapshot` nested and extend `ci/dep-policy.ps1` to audit its `Cargo.lock`. Owner-decision: allowlist extension. *Verify:* `cargo clippy --workspace`; quiet wrapper `cargo test --workspace` includes `sakura-candidate-sweep`; dep-policy self-test + run; `rg -n 'candidate-snapshot' ci docs` non-empty. *Expect:* one previously invisible crate is linted and audited; the other's exemption is explicit.
