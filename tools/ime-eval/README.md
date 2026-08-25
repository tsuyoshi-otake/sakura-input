# ime-eval comparison commands

`ime-eval` keeps the fixed #73 Stage 1 quality scoreboard (50 cases) separate
from the Issue #93 ranking fixture (22 cases).  The Issue #93 adapter consumes
one independent structured candidate snapshot per build and compares case IDs
in the committed fixture order.  It rejects missing/duplicate IDs, reading or
order mismatches, fixture hash mismatches, and incompatible option identities.

The candidate snapshot JSON has the `engine_candidate_snapshot_v1` top-level
lane used by `tools/candidate-snapshot`:

```text
artifact{git_sha,evaluator_sha256,dictionary_sha256,fixture_sha256,source_diff_sha256,variant}
evaluator{name,version,executable_sha256,build_feature}
engine{package,api,origin_metadata,path_evidence_metadata,input_support_metadata}
options{profile,candidate_limit,method,it_bias,it_bias_per_mille,max_it_boost,
        initial_right_id,input_repair,learning,user_dictionary,reranker,material,
        options_sha256}
cases[{case_id,reading,candidate_limit,candidate_surfaces,candidates,terminal,truncated}]
```

Each `candidates` item is retained as bounded JSON evidence, including
segments, final cost, origin, base/ranking fields, and `unsupported_metadata`.
The scorer uses only the declared surface order for Top-1, recall@k, and rank;
it never infers provenance from surface text.  `evaluator_sha256` is the
evaluator artifact identity; it is not a shipped-engine hash.

Score one independently captured side:

```powershell
cargo run --manifest-path tools/ime-eval/Cargo.toml -- quality-rank-score `
  --fixture eval/corpus/behavioral/ranking-comparison-issue93/fixture.json `
  --snapshot before.candidate-snapshot.json `
  --out before.ranking-score.json
```

Compare the two independent snapshots directly (the command emits one
machine-readable comparison report and a concise human summary):

```powershell
cargo run --manifest-path tools/ime-eval/Cargo.toml -- ranking-compare `
  --fixture eval/corpus/behavioral/ranking-comparison-issue93/fixture.json `
  --before before.candidate-snapshot.json `
  --after after.candidate-snapshot.json `
  --out issue93-ranking-comparison.json
```

The report contract is
[`schema/issue93-ranking-comparison-v1.schema.json`](schema/issue93-ranking-comparison-v1.schema.json).
For the original Stage 1 report boundary, use `quality-compare` without
`--fixture`; `--side baseline` or `--side candidate` selects which independent
observation side is compared.
