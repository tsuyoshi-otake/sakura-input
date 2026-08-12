# Neural reranker evaluation

`neural-eval` compares the dictionary converter's Top-1 against the isolated
`sakura_neural_worker.exe` on the exact same dictionary-produced Top-6
snapshot. It emits one JSON row per supplied corpus item, including eligibility,
worker fallback, baseline and neural Top-1 correctness, and win/loss/tie. The
report also records shared candidate-generation Recall@6 and MRR@6 plus the same
quality fields grouped by `slice`; those metrics are not attributed to the
reranker.

It accepts the existing frozen `held-out.tsv` schema. Supplying that corpus is a
measurement convenience, not permission to tune against it. The evaluator's
input contract is an explicitly supplied, static Normal-scope corpus: it never
reads live text, history, user dictionaries, or an engine process. Do not add
invented human answers to this directory. Synthetic strings in Rust unit tests
are explicitly test fixtures only and are not corpus data.

## Quality-acceptance corpus

`neural-eval` refuses a quality run with fewer than **600** cases. The existing
60-row `held-out.tsv` is therefore limited to a worker/evaluator smoke run and
must pass `--exploratory`; its score is never a model-selection result.

The future frozen 600+ row holdout must include at least **200** independently
reviewed, non-duplicated `chat` rows and **200** corresponding `email` rows in
addition to the conversion slices already required by Issue #32. At a minimum,
retain the following source/use
categories in the record manifest so results can be reported separately rather
than blended into one flattering number:

- `chat`: short replies, requests, confirmations, scheduling, apologies, and
  informal-but-polite conversational messages.
- `email`: greeting, request, acknowledgement, delivery/status notice,
  scheduling, closing, and subject-like business messages.
- `general`, `it`, `homophone`, `segmentation`, and `outside-top6`: conversion
  difficulty slices that overlap the communication category only when the
  manifest explicitly records both dimensions.

Each record needs a stable ID, provenance, reviewer state, reading, expected
surface, communication category, and conversion-difficulty tags. The corpus
must be frozen with a SHA-256 before any model/penalty selection. Do not use
live chat, mail, developer history, user dictionaries, or generated template
permutations as a substitute for reviewed examples.

`neural-eval-communication-draft.tsv` is a 600-row authored regression corpus
(200 `chat`, 200 `email`, 200 `general`) for exercising the evaluator at its
required scale. It contains no user data. Its records are intentionally marked
as a draft and are **not** the independently reviewed, provenance-complete
holdout required to choose a production model or default. Use its report to
find regressions and worker failures, then freeze a separately reviewed corpus
before claiming a quality result.

Reports use schema version 2. `baseline.evaluated` and `neural.evaluated` both
cover every parsed row; `neural.eligible` is a separate count for rows that
passed the selected Tiny gate. This keeps long-only and all-normal Top-1
denominators comparable even when a row falls back before the worker call.
`candidate_rank.recall_at_6` and
`candidate_rank.mrr_milli_sum / candidate_rank.evaluated` describe the shared
dictionary snapshot. `slice_metrics` repeats coverage and Top-1/fallback counts
for `chat`, `email`, and `general` so a strong aggregate cannot hide a weak
communication slice. A report made with `--exploratory` records
`acceptance_eligible: false` even when it has 600 rows; the flag means the
corpus is a smoke/regression measurement, not an independently reviewed
acceptance holdout.

Run each mode independently. `long` mirrors the current engine gate: at least
two candidates and either a 10+ character reading or a 3+ segment top candidate.
`all-normal` is an Issue #32 experimental condition that removes only that
length/segmentation gate; the static corpus itself is the Normal-scope input.
In either mode a missing, invalid, timed-out, or malformed worker response is
recorded as `worker-fallback`, retaining dictionary Top-1.

```powershell
rtk cargo run --locked --release -p dictc --bin neural-eval -- `
  --dictionary <system.dic> `
  --corpus corpus\held-out.tsv `
  --worker artifacts\release\sakura_neural_worker.exe `
  --model-dir artifacts\release\neural\deberta-v2-tiny-japanese-char-wwm `
  --mode long `
  --exploratory `
  --report <neural-long.json>
```

The command exercises the actual worker/model only when the paths point to a
validated payload. It is intentionally an explicit CLI rather than a default
unit test, because model/runtime artifacts are optional and absent in ordinary
developer and CI environments.
