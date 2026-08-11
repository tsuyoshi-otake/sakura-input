# Context Prediction — Phase 5B Deterministic Dataset Gate

Status: offline schema, split, deduplication, audit-selection, and verification
tooling for Issue #34. This change does not contain a Wikipedia dump, a real
Sakura replay, a generated dataset, a completed human audit, or a model.

## Input boundary

`context-dataset build` accepts LF-canonical UTF-8 JSONL produced by a future
offline replay adapter. Every record identifies the pinned source, article,
revision, stable sample ordinal, candidate-set fingerprint, bounded committed
context, reading, the actual ordered Sakura candidate snapshot, classification
tier, and expected candidate index when labeled.

The gate deliberately rejects random dictionary-word negatives. Candidate
records must carry the transient values and structural fields from an actual
Sakura prediction replay: runtime candidate id, reading, surface, dictionary
entry ordinal, base cost, authority, source, right id, and IT-domain marker.
Only ordinary system-dictionary candidates are accepted. Learned history and
user-dictionary candidates are rejected so private per-user data cannot enter
the public-corpus lane. Their production priority is evaluated separately and
must still have zero loss at the Issue #34 quality gate.

The producer owns Tier assignment and must preserve its evidence for human
review:

- Tier A: a uniquely aligned high-confidence label eligible for the mandatory
  precision audit.
- Tier B: a labeled but ambiguous or transformation-dependent example that is
  audited separately and is not counted toward the Tier A precision claim.
- Tier C: an intentionally unlabeled diagnostic example. The schema rejects an
  expected candidate on Tier C.

This phase validates the structural tier contract. It does not claim that any
real producer has met those semantic definitions yet.

## Frozen identity and split rules

All text is NFC-normalized without NFKC or width conversion. Stable sample and
candidate ids use length-framed values and SHA-256. A sample identity binds the
source id, article id, revision id, sample ordinal, and actual Sakura snapshot
fingerprint. Candidate identity binds canonical reading, surface, dictionary
kind, and exact dictionary ordinal.

The split algorithm is `sha256-article-80-10-10-v1`. It hashes only the pinned
source id and article id into train (80%), tuning (10%), or held-out (10%). All
samples from one article therefore enter exactly one split.

Exact duplicates bind context, reading, the ordered stable candidate ids, and
the expected candidate id. Near-duplicate v1 removes only layout/punctuation
variation: it NFC-normalizes and lowercases the context, retains alphanumeric
characters, requires at least 16 characters, and binds reading plus expected
candidate id. It is intentionally narrow and versioned; semantic similarity is
not guessed. Deterministic sample-id order chooses the retained record.

The manifest reports duplicates removed within and across splits. Accepted
outputs must have zero exact and near-duplicate leakage, and the verifier
recomputes that condition from every output record.

## Immutable external artifacts

Raw JSONL and generated artifacts must be outside the Git repository. The CLI
rejects either path when it resolves beneath `--repo-root` (the current
directory by default), refuses an existing output directory, writes artifacts
with create-new semantics, and creates `manifest.json` last as the commit
marker.

The output contains:

- `train.jsonl`, `tuning.jsonl`, and `held-out.jsonl`;
- deterministic held-out `audit-tier-a/b/c.jsonl` selections;
- `manifest.json`, binding the source manifest, raw input, generator,
  dictionary, algorithms, record counts, byte lengths, and artifact SHA-256s.

The Tier A request cannot be configured below 1,000. `tier_a_requirement_met`
is true only when the accepted held-out set actually contains at least 1,000
Tier A records. Build success alone is not an acceptance result.

Example after an external replay exists:

```powershell
rtk cargo run -p dictc --bin context-dataset -- build `
  --records C:\context-data\replay\records.jsonl `
  --source-manifest corpus\context-prediction\source-manifest.json `
  --output-dir C:\context-data\datasets\jawiki-20260801-run-001 `
  --generator-sha256 <64-lowercase-hex> `
  --dictionary-sha256 <64-lowercase-hex>

rtk cargo run -p dictc --bin context-dataset -- verify `
  --dataset-dir C:\context-data\datasets\jawiki-20260801-run-001
```

Verification parses every artifact, recomputes sample/candidate identities and
article splits, checks NFC and bounded protocol fields, revalidates candidate
structure and tier labels, proves audit records are deterministic held-out
subsets, recomputes duplicate leakage, and rejects altered bytes or manifest
accounting.

## Verified synthetic scope and remaining work

The checked-in tests use only bounded synthetic records outside the repository.
They prove two builds are byte-identical, one article never crosses a split,
cross-split exact and near duplicates are removed, personal candidate sources
fail closed, the fixed Tier A gate cannot be weakened, and artifact tampering
is detected.

Still required before the data gate can pass:

- stream-extract the pinned dump and implement the actual Sakura replay adapter;
- bind the adapter executable and stock dictionary hashes to a real run;
- generate and independently review the external Tier A/B/C audit artifacts;
- record at least 1,000 Tier A decisions, >=99.5% point precision, and >=99.0%
  Wilson 95% lower bound;
- record zero provenance omissions and the real Oracle Recall@32 result.

