# Issue #36 coherent fallback measurement

Date: 2026-08-12

This record contains aggregate, text-free evidence only. The licensed MSR-IME
requests, AJIMEE cases, candidate snapshots, and corpus text remain local and
untracked.

## Problem and terminal contract

The previous N-best search admitted lexical/fallback mixtures into the arena
and rejected them only after a complete path was popped. Once the fixed 65,536
state arena filled, the search returned success without distinguishing budget
exhaustion from ordinary exhaustion. A valid reading could therefore produce an
empty candidate list.

The converter now:

- constructs the whole-reading, one-segment fallback independently of lattice
  and search arenas;
- carries `neutral`, `lexical`, `reading`, or `katakana` coherence in each
  search state and rejects an invalid transition before allocating its child;
- bounds path depth before materialization, so segment overflow cannot turn the
  fallback path into a hard failure;
- reports `CandidateLimitReached`, `SearchExhausted`,
  `StateBudgetReached`, or `LatticeBudgetReached` with text-free aggregate
  counters; and
- preserves hard errors for invalid input, invalid options, and dictionary
  decoding failures.

Production keeps the existing 18-candidate bound. The `research-top32` feature
raises only the isolated evaluation bound to 32 and is not enabled by shipping
targets. `conversion-test-support` exposes deterministic lattice/search budget
reduction only to regression tests.

## Inputs and identity

- Fixed system dictionary SHA-256:
  `6d34364b5354d3c67efefaf15b50142b1365b21140ec8eee0f77570d828544ad`
- Previous pinned research converter base:
  `8e966dff456e4e7165e025f97c1f73327ff3f550`
- Issue #36 production implementation commit before this measurement record:
  `22b4c3f`
- MSR-IME Corpus 1.0: 5,168 supported requests. The source license is
  non-commercial and forbids redistribution.
- AJIMEE-Bench JWTD_v2/v1: 200 requests, pinned source data SHA-256
  `798736ae6d26db74a1cd88de07eef95a62b1506df17f4b326e578c6cb3f96137`.
  Source data is CC BY-SA 3.0; its reference evaluator is CC0 1.0.

The before and after runs used the same release profile, dictionary, request
files, machine, Top-32 limit, and comparison runner. Timing starts immediately
before one converter call and stops immediately after it; JSON parsing and
candidate comparison are outside the timed region. These are local engineering
measurements, not a product latency guarantee.

## Aggregate result

| Metric | Before | After |
|---|---:|---:|
| Evaluated requests | 5,368 | 5,368 |
| Empty candidate lists | 2,637 | 0 |
| p50 converter latency | 22.405 ms | 7.759 ms |
| p99 converter latency | 196.482 ms | 93.353 ms |
| Maximum converter latency | 645.810 ms | 246.575 ms |
| Old Top-1 changed on a previously non-empty result | 0 | 0 |
| Old Top-6 surfaces lost | 0 | 0 |
| Old Top-32 surfaces lost | 0 | 0 |

The after run exactly matched 2,674 previous candidate lists. The other 2,694
lists changed only by adding candidates or extending previously truncated/empty
results; no previous Top-1, Top-6, or Top-32 surface was lost. Therefore the
previous gold metrics remain conservative lower bounds without exposing or
reprocessing corpus text:

- MSR-IME: exact at least 498/5,168, Recall@6 at least 946, Recall@32 at least
  1,226 (previous empty count 2,624, now zero).
- AJIMEE: exact at least 77/200, Recall@6 at least 130, Recall@32 at least 146
  (previous empty count 13, now zero).

The after-run terminal counts were 2,680 candidate-limit, 317 exhausted, 2,371
state-budget, and zero lattice-budget results. It pruned 630,646,883 incoherent
prefix extensions before state allocation and independently inserted the
lossless fallback in 2,043 results. State-budget termination remains common on
Top-32 sentence evaluation, but it is now observable and can no longer erase
all candidates.

## Acceptance and residual risk

- Deterministic zero-state and zero-lattice tests return a non-empty,
  whole-reading, one-segment fallback and the matching explicit terminal.
- Unicode prefix boundaries do not leak lexical/fallback mosaics.
- A path over the segment bound degrades to the one-segment fallback.
- A full one-candidate lexical result is not displaced merely to reserve an
  additional fallback.
- Public-benchmark candidate recall did not regress, and the dominant serial
  search cost decreased materially in this run.

The absolute Top-32 p99 remains above an interactive key-path target. This fix
removes the correctness failure and attacks the dominant incoherent-state
explosion, but it does not claim that long-sentence Top-32 export latency is an
accepted production UI latency. Production requests remain bounded to 18
candidates, and further latency work should use the explicit terminal/counter
evidence rather than increasing the fixed arena.
