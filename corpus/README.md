# corpus/

Evaluation input for conversion accuracy, kept separate from `data/` because
nothing here ships: it exists to tell us whether a change to the cost model
made conversion better or merely different.

| File | Phase | Purpose |
|------|-------|---------|
| `tuning.tsv` | 2 | Cost/overlay development set; never used for a release score |
| `held-out.tsv` | 2 | Frozen general + IT grading set, including homophone phrases |
| `mozc-baseline.tsv` | 2 | Frozen Mozc top-1 output for exactly the held-out row ids |
| `context.tsv` | 3 | Cases that only convert correctly given recent commits (DESIGN 5.8) |

Corpus rows are `id<TAB>slice<TAB>reading<TAB>expected`. The release harness
grades `held-out.tsv` only, checks its ids one-for-one against the Mozc baseline,
and emits every Sakura miss to a machine-readable gap list. Tuning work may read
`tuning.tsv`; it must not inspect held-out output until the proposed costs have
been frozen.

## Running the Phase 2 gate

Build the pinned dictionary first (see `data/README.md`), then run the evaluator
in release mode. The latency input is intentionally exactly 30 characters, the
PLAN.md budget's specified workload.

```powershell
$env:CARGO_HTTP_CHECK_REVOKE = 'false'
rtk cargo run --locked --release -p dictc --bin corpus-eval -- `
  --dictionary "$env:USERPROFILE\tmp\sakura-input-dictionary-build\system.dic" `
  --corpus corpus\held-out.tsv `
  --baseline corpus\mozc-baseline.tsv `
  --report "$env:USERPROFILE\tmp\sakura-phase2-quality.json" `
  --latency-reading きょうかいぎでせっていへんこうのけっかをくわしくせつめいする
```

The command fails unless Sakura reaches 80% of Mozc's held-out correct count,
the IT slice reaches the Phase 2 interim 90% floor, conversion p99 is at most
20 ms, the image is at most 128 MiB, and the exact matrix is at most 4 MiB.

## Regenerating the Mozc baseline

Baseline changes are reviewer-owned data updates, never an evaluator side
effect. Check out the revision recorded in `data/SOURCES.lock`, run that Mozc
build's `mozc_tool --mode=converter` with a clean profile and default config,
submit each `reading` from `held-out.tsv`, record the first conversion candidate
under the same row id, and review the complete TSV diff. Run `corpus-eval` after
the update. CI consumes the checked-in answers and never installs or invokes
Mozc, so an upstream release cannot silently weaken Sakura's gate.
