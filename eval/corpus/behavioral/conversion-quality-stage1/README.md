# Conversion Quality Program — Stage 1

This directory contains the deterministic fixture for the user-provided 50
conversion examples.

The fixture is deliberately under `eval/corpus/behavioral/`. It is not loaded
by `tools/ime-eval` as a semantic case, and its expected surfaces, segment
arrays, ranks, and negative controls must never be passed to the Luna Max
Judge.

`fixture.json` stores slash-free surfaces and separate segment arrays. A
surface match therefore cannot be mistaken for a segment match. Every case is
an observation candidate; none is an unconditional Top-1 product assertion.
The supplied “before” surface is retained as a competitor control, not as an
automatically forbidden result. The `assertion_scope` value is retained in
scored observations so future `context_required` and `hold` cases cannot
silently become candidate-only assertions.

The Stage 1 options identity fixes the production candidate bound at 18 and
disables learning, user-dictionary, reranker, and input-repair inputs for this
deterministic baseline. The main `whole_reading_core` lane uses the existing
`sakura-core::Converter` on each complete reading, so its segment sequence and
cost are observed where the API provides them. Origin is recorded only from a
system-entry ordinal; otherwise it is explicit `null`/unsupported.

`ime-eval quality-core-capture` collects whole-reading candidates. Ordinary
cases are stored under `pairs`; the declared negative controls are
independently stored under `control_pairs` and are scored by
`ime-eval quality-score`. The separate `quality-capture` command replays the
real engine's active-segment UI only and must not be scored as whole-reading
quality.
