# Human Calibration Set

Luna Max may replace day-to-day IME reading only after this set measures it.

Target size: 300 stratified cases.

| Slice | n |
|-------|--:|
| normal Japanese | 60 |
| technical / ASCII | 50 |
| mixed romaji | 40 |
| typo repair | 40 |
| prediction ranking | 40 |
| katakana / loanwords | 30 |
| multi-segment | 20 |
| known regressions | 20 |

Split:

- `train-like/` — 200 cases for prompt / rubric development
- `holdout/` — 100 locked cases. Do not inspect while editing Judge vN

Humans label with the same schema as Luna: `A` / `B` / `tie` / `ungradable`,
severity, reason codes. Initial acceptance:

- overall human agreement ≥ 90%
- material A/B agreement ≥ 92%
- severity 3/4 regression recall ≥ 95%
- literal corruption false-negative = 0
- weighted Cohen's kappa ≥ 0.80
- ungradable disagreement ≤ 10%
- severity 3/4 mutant kill rate = 100%

Labels are not committed in Phase 1.

The machine-readable label file is `schema_version: 1` with
`split: "train-like"` or `"holdout"` and an `observations` array. Each
observation contains only the opaque pair id, the human result, the Judge
result, and an explicit `literal_corruption` gold flag. The schema is
`eval/judge/v1/calibration.schema.json`; calculate metrics with
`ime-eval calibrate --labels <file>`.
