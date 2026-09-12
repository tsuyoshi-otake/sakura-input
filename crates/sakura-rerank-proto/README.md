# `sakura-rerank-proto`

## Purpose
Own the bounded SKNR/SKNS protocol v1 shared by engine, neural worker, and the offline `dictc` evaluator.

## Owns
Magic, version, frame/candidate/context bounds, request/response layout, and encode/decode.

## Must not own
Scoring, ONNX, model validation, process lifecycle, timeout, retry, or fallback policy.

## Allowed dependencies
`sakura-values` only.

## Allowed consumers
`sakura-engine`, `sakura-neural-worker`, and offline neural evaluation (currently `dictc`, then its tools package after Phase 3.9).

## Public API budget
Six top-level public items: `Limits`, `Candidate`, `Score`, `Frame`, `read_frame`, and `write_frame`.

## Wire behavior
Decoding retains arbitrary f32 score bits for engine validation, matching the old engine parser. Encoding a response rejects non-finite scores with `InvalidInput`, matching worker `write_success`. Request candidates borrow text during engine/dictc encoding, so only a bounded vector of at most six descriptors is allocated and strings are not copied.

## Issue types
Protocol layout, version, magic, framing, and bounds changes enter through `read_frame`/`write_frame`; preserve the SKNR/SKNS v1 byte contract and malformed-frame error classes. Verify the shared codec, neural-worker adapter, engine adapter, and offline evaluator against the `IRV-ENGINE-CANDIDATE` read set in [`benchmarks.json`](../../verification/irv/benchmarks.json). The canonical contract inventory entry is [`neural-rerank.md`](../../docs/contracts/README.md).

## Test commands
Run from the repository root using PowerShell 7:

```powershell
./ci/run-test-quiet.ps1 -Name 'rerank protocol and worker' -Command { cargo test -p sakura-rerank-proto -p sakura-neural-worker --locked }
./ci/run-test-quiet.ps1 -Name 'engine rerank adapter' -Command { cargo test -p sakura-engine --lib long_conversion --locked }
./ci/run-test-quiet.ps1 -Name 'offline rerank adapter' -Command { cargo test -p dictc --bin neural-eval --locked }
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
```

Preserve the worker's literal fixtures, generated malformed frames, and real-model two-candidate IPC test. Enforce dependency rule R8 separately.
