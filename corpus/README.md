# corpus/

Evaluation input for conversion accuracy, kept separate from `data/` because
nothing here ships: it exists to tell us whether a change to the cost model
made conversion better or merely different.

| File | Phase | Purpose |
|------|-------|---------|
| `general.tsv` | 2 | Everyday Japanese — reading ⇥ expected conversion |
| `it.tsv` | 2 | IT-domain sentences, the accuracy target that matters most |
| `context.tsv` | 3 | Cases that only convert correctly given recent commits (DESIGN 5.8) |

Each row is `reading<TAB>expected`. The harness reports top-1 accuracy per file
and fails the build on a regression against the recorded baseline, so an
accuracy loss has to be argued for in a diff rather than noticed months later.
