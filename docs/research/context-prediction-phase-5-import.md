# Context Prediction — Phase 5D Sakura-Rerank Import Boundary

Status: implemented and verified against the externally held Issue #13 snapshot
for Issue #34. This phase does not claim Tier A acceptance, a model, or a
production change.

## Purpose and trust boundary

`context-dataset import-rerank` joins the verified Sakura-Rerank source spans
and research top-32 converter records by their sorted, unique `stable_id`. The
caller must pin the SHA-256 of both aggregate manifests. The adapter then
independently verifies the manifest-to-artifact hashes, record and candidate
accounting, exporter identity, Sakura Input and dictionary identities, converter
candidate fingerprints, segment coverage, and the top-6 prefix.

The resulting `records.jsonl` is the strict input to the existing Phase 5B
dataset builder. `manifest.json` is aggregate-only and written last. All inputs
and outputs must resolve outside Git, and an existing output directory is
rejected.

## Actual converter candidates

The Phase 5B schema is version 2 because a real full-reading top-32 path is not
always one exact dictionary entry. The importer preserves:

- a single exact system entry with its ordinal;
- a compound path made entirely of system dictionary edges;
- a public generated reading, katakana, or literal fallback path.

All remain ordinary candidates. Any user-dictionary segment is rejected by the
strict exporter parser, and the dataset gate still rejects history, explicit
learning, and user-dictionary authority. Generated paths are represented
explicitly rather than mislabeled as exact dictionary entries or dropped from
the candidate pool.

## Label and context rule

The committed prefix is truncated to its newest complete UTF-8 suffix within
512 bytes. It is never written to an aggregate manifest. A unique exact gold
surface in top-32 becomes Tier A; an out-of-pool gold surface becomes unlabeled
Tier C. Multiple matching candidates are rejected. This records Oracle misses
without inventing a label.

## Invocation

```powershell
rtk cargo run -p dictc --bin context-dataset -- import-rerank `
  --source-spans C:\context-data\rerank\source-spans.jsonl `
  --source-span-manifest C:\context-data\rerank\source-spans.manifest.json `
  --source-span-manifest-sha256 <pinned-manifest-sha256> `
  --exporter-records C:\context-data\rerank\top32.jsonl `
  --snapshot-manifest C:\context-data\rerank\top32.manifest.json `
  --snapshot-manifest-sha256 <pinned-manifest-sha256> `
  --source-id wikimedia-jawiki-20260801 `
  --output-dir C:\context-data\imports\jawiki-run-001

rtk cargo run -p dictc --bin context-dataset -- verify-rerank-import `
  --import-dir C:\context-data\imports\jawiki-run-001
```

The verified Issue #13 snapshot is bound to the pre-#36 Sakura Input revision.
It is useful for contract validation, but a quality comparison must regenerate
the snapshot from the #36-capable candidate generator and record a new verified
manifest before drawing Oracle Recall or training conclusions.

## Measured Issue #13 import

The externally held verified Issue #13 artifacts were imported on 2026-08-12:

- source records: 1,969;
- converter candidates: 35,414;
- unique gold surfaces present in top-32: 1,969;
- unlabeled Oracle misses: 0;
- imported records SHA-256:
  `422b4869e6b016a08309326f827f384ddc49ff14eadee4cfe57943604e82d114`.

Two independent dataset builds were byte-identical for all seven artifacts.
Deduplication retained 1,966 records, removed three exact duplicates including
one cross-split duplicate, and left zero measured exact or near-duplicate
leakage. The frozen split produced 1,594 train, 199 tuning, and 173 held-out
records. Only 173 held-out Tier A records were available, so the fixed 1,000
record audit gate correctly remained unmet. Dataset manifest SHA-256:
`a97338c4ac972f41dce1577a5b398ffce6e677d224698aa9e963865784df6fd6`.

This is contract and pipeline evidence. Because the exporter is pinned to
Sakura Input `8e966dff...`, it is not evidence for the #36 candidate generator,
current Oracle Recall, or model quality.

## Verification rubric

- `Verify:` deterministic synthetic import twice. `Expect:` identical output
  hashes and complete record/candidate accounting.
- `Verify:` alter an input after its manifest is written. `Expect:` rejection
  before an output directory or trusted manifest is published.
- `Verify:` compound-system and generated candidates. `Expect:` the entire
  public candidate pool survives with explicit provenance.
- `Verify:` user source, malformed segment ranges, identity mismatch, missing
  gold, and manifest mismatch tests. `Expect:` explicit fail-closed outcomes;
  missing gold is Tier C rather than a fabricated label.
