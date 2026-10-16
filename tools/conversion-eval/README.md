# sakura-conversion-eval

## Purpose

Offline measurement of conversion quality against a built dictionary image. None of these binaries compile dictionaries or ship in the product.

## Owns

- `corpus-eval`: fixed-corpus top-1 and recall report, with a latency probe.
- `neural-eval`: offline reranker comparison through the isolated worker, using the SKNR/SKNS codec from `sakura-rerank-proto`.
- `compound-homophone-scan`: a compound homophone survey.
- `learning-replay`: replays the developer input history against the engine's learning with and without #305 correction learning. It is built only with the `learning-replay` feature and reports counts only.
- `accuracy-gate`: the accuracy gate (#333). It measures conversion (product default options, top candidate only) and prediction (one character at a time, rank inside the nine suggestions) on `corpus/accuracy/`, compares every row with the committed snapshot and fails on a regression (`--check`) or rewrites the snapshot and floors (`--accept`). It is built only with the `accuracy-gate` feature.
- Their report formats and exit codes.

## Must not own

- Dictionary compilation, which belongs to `dictc-core` and `dictc`.
- The reranker wire contract, which belongs to `sakura-rerank-proto`.
- Conversion rules, which belong to `sakura-core`.
- Learning, ranking and correction detection, which belong to `sakura-engine`. `learning-replay` drives them through the engine's `replay-eval` API and never copies them.
- Conversion and prediction rules. `accuracy-gate` calls `Converter` and `PredictionRuntime` as the product does and never copies their ranking.
- The blind judging and release gates, which belong to `sakura-ime-eval`.

## Allowed dependencies

- Normal: `sakura-core`, `sakura-proto`, `sakura-rerank-proto`, `serde`, `serde_json`.
- Optional, behind the `learning-replay` feature: `sakura-engine` with its `replay-eval` feature. The feature stays off in a workspace build, so the shipping engine is never compiled with `replay-eval`.
- Optional, behind the `accuracy-gate` feature: `sakura-engine` without extra features (`dictionary::open`, `PredictionRuntime`), and `sha2` (already in the lockfile for `dictc-core`) for the report header's SHA-256. No new crate in `Cargo.lock`.
- Development: `dictc-core`, to compile the fixture dictionaries of the `learning-replay` and `accuracy-gate` tests.

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
| Accuracy gate, snapshots, floors (#333) | `src/bin/accuracy_gate.rs` and `src/bin/accuracy_gate/`, `corpus/accuracy/README.md` |
| Correction learning replay (#305) | `src/bin/learning_replay.rs`, `crates/sakura-engine/src/replay_eval.rs`; results in `docs/decisions/issue-305-correction-learning.md` §5 |

The IRV benchmark that reads `neural_eval.rs` is listed in `verification/irv/benchmarks.json`.

## Test commands

Run from the repository root using PowerShell 7:

```powershell
./ci/run-test-quiet.ps1 -Name 'conversion-eval tests' -Command { cargo test -p sakura-conversion-eval --features 'learning-replay,accuracy-gate' --locked }
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
```

These tests are unit tests only. A real evaluation run needs a built `system.dic`; `neural-eval` also needs the worker and model directory, `learning-replay` needs the plaintext `history export` and `learning export` files, and `accuracy-gate` needs a built `system.dic` (see `corpus/accuracy/README.md`).
