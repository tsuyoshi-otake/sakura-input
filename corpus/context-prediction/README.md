# Context Prediction corpus metadata

Only small, reviewable provenance and schema documentation belongs here.
`source-manifest.json` pins the public source bytes used by the offline research
lane.

Do not add dumps, indexes, extracted articles, raw Sakura replays, generated
train/tuning/held-out files, audit records, checkpoints, ONNX files, or other
derived artifacts to this directory or elsewhere in Git. Store them outside
the repository and bind them through the hash-bearing dataset manifest produced
by `context-dataset`.

See:

- `docs/research/context-prediction-phase-5-source.md`
- `docs/research/context-prediction-phase-5-dataset.md`
- `scripts/verify-context-prediction-source.ps1`
- `crates/dictc/src/bin/context_dataset.rs`
