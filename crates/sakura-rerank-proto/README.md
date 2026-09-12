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
`sakura-engine`, `sakura-neural-worker`, and offline `dictc` neural evaluation.

## Public API budget
Six top-level public items: `Limits`, `Candidate`, `Score`, `Frame`, `read_frame`, and `write_frame`.

## Wire behavior
Decoding retains arbitrary f32 score bits for engine validation, matching the old engine parser. Encoding a response rejects non-finite scores with `InvalidInput`, matching worker `write_success`. Request candidates borrow text during engine/dictc encoding, so only a bounded vector of at most six descriptors is allocated and strings are not copied.

## Verification
For protocol layout, bounds and malformed-frame changes (#178), run `./ci/run-test-quiet.ps1 -Name 'rerank protocol' -Command { cargo test -p sakura-rerank-proto -p sakura-neural-worker }`. Preserve the worker's existing literal fixtures, generated malformed frames and real-model two-candidate IPC test; run engine and dictc adapter tests and enforce dependency rule R8.
