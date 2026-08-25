# Issue #93 ranking baselines

These are deterministic `engine_candidate_snapshot_v1` observations captured
from each subject's own `sakura-core`, not three dictionaries evaluated by one
current conversion binary. The fixture, options, dictionary, evaluator, Git,
and exact ablation identities are embedded in every file and summarized in
`manifest.json`.

Each subject was captured twice with the same release build. The two JSON
files were byte-for-byte identical; one copy per subject is retained here.
Generated score/comparison reports are intentionally not duplicated in this
directory because they retain the full candidate evidence again. Regenerate
them with `quality-rank-score` and `ranking-compare` as documented in
`tools/ime-eval/README.md`; their expected fingerprints are pinned in the
manifest.

The manifest calls the per-case mixed Top-1/Recall@5 total
`declared_assertion_passes`. The CLI's compact output currently labels that
same field `recall`. This avoids describing the six Top-1 controls as though
they were Recall@5 assertions.

The ablation keeps the v1.0.23 release dictionary and normal IT option values.
It applies only the committed patch that removes the
`apply_it_compound_coherence` call. It is not the broader
`it_bias_per_mille=0` experiment.
