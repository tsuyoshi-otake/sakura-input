# dictc-core

## Purpose

The offline dictionary compiler library. It turns licensed TSV sources into Sakura's read-only mmap image. For the same inputs, it must write the same bytes.

## Owns

- Source parsing: entries, the connection matrix, and the Mozc trim.
- Overlays, boundary policy, and conversion priorities.
- Details, glossary, inflection, category, single-kanji, and WordNet stages.
- The LLM-detail targets and the release gate.
- Context-dataset import.
- The image writer (`compile`).

## Must not own

- The image read format. `sakura-core` owns `dictionary/format.rs`, and the writer must follow it rather than define its own format.
- Command-line argument handling, which lives in `dictc`.
- Engine ranking.
- Runtime I/O or IPC.

## Allowed dependencies

- Normal: `sakura-core`, `sakura-proto`, `sakura-context-proto`, `sakura-rerank-proto`, `quick-xml`, `flate2`, `serde`, `serde_json`, `sha2`, `unicode-normalization`.
- Development and build: none.

## Allowed consumers

- Production (offline): `dictc`.
- Development only:
  - `sakura-engine`, through the `dev-fixtures` feature.
  - `sakura-ime-eval` tests.
- It is never a normal dependency of a runtime crate. `ci/dep-policy.ps1` enforces this.

## Public API budget

- No growth beyond the surface moved in Phase 3.9: the `lib.rs` compile/parse functions plus the 11 stage modules.
- New public items need a consumer outside this crate.

## Issue types

| Change | Where to look |
| --- | --- |
| Image format change (IRV `IRV-DICTIONARY-FORMAT`) | `lib.rs` writer, `tests/image.rs`, `tests/dictionary_robustness.rs`; contract `dictionary-image.md` (see `docs/contracts/README.md`) |
| Source or overlay rule | `lib.rs` parsers plus the matching `tests/*_conditions.rs` and `tests/*_properties.rs` |
| LLM details (#112) | `llm_details.rs`, `llm_detail_targets.rs`, `tests/curated_details.rs` |

## Test commands

Run from the repository root using PowerShell 7:

```powershell
./ci/run-test-quiet.ps1 -Name 'dictc-core tests' -Command { cargo test -p dictc-core --locked }
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
```

These ignored tests need `--ignored`, and each has a prerequisite:

| Test | Prerequisite | Where it runs |
| --- | --- | --- |
| `conversion` real-dictionary gates | A built `artifacts/release/system.dic` | The release workflow |
| `dictionary_robustness` hostile fuzz | `--release` | CI |
