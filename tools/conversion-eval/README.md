# sakura-conversion-eval

## Purpose

Offline measurement of conversion quality against a built dictionary image. None of these binaries compile dictionaries or ship in the product.

## Owns

- `corpus-eval`: fixed-corpus top-1 and recall report, with a latency probe.
- `neural-eval`: offline reranker comparison through the isolated worker, using the SKNR/SKNS codec from `sakura-rerank-proto`.
- `compound-homophone-scan`: a compound homophone survey.
- Their report formats and exit codes.

## Must not own

- Dictionary compilation, which belongs to `dictc-core` and `dictc`.
- The reranker wire contract, which belongs to `sakura-rerank-proto`.
- Conversion rules, which belong to `sakura-core`.
- The blind judging and release gates, which belong to `sakura-ime-eval`.

## Allowed dependencies

- Normal: `sakura-core`, `sakura-proto`, `sakura-rerank-proto`, `serde`, `serde_json`.
- Development and build: none.

## Allowed consumers

- No crate depends on this package.
- Developers and `scripts/verify-phase2.ps1` / `scripts/verify-phase4.ps1` run it with `cargo run -p sakura-conversion-eval --bin <name>`.

## Public API budget

- Zero library items. This package has binaries only.

## Issue types

| Change | Where to look |
| --- | --- |
| Corpus accuracy gate | `src/bin/corpus_eval.rs`, `corpus/README.md` |
| Reranker offline eval | `src/bin/neural_eval.rs`, `corpus/neural-eval.md`; contract `neural-rerank.md` (see `docs/contracts/README.md`) |
| Homophone survey | `src/bin/compound_homophone_scan.rs` |

The IRV benchmark that reads `neural_eval.rs` is listed in `verification/irv/benchmarks.json`.

## Test commands

Run from the repository root using PowerShell 7:

```powershell
./ci/run-test-quiet.ps1 -Name 'conversion-eval tests' -Command { cargo test -p sakura-conversion-eval --locked }
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
```

These tests are unit tests only. A real evaluation run needs a built `system.dic`; `neural-eval` also needs the worker and model directory.
