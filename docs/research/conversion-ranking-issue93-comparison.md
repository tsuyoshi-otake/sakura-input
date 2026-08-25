# Issue #93 conversion-ranking comparison contract

Date: 2026-08-25

Phase 0 fixes the input and observation boundary before a CandidateRanker
refactor changes production behavior. The checked-in corpus is
`eval/corpus/behavioral/ranking-comparison-issue93/fixture.json`. Its local
schema is an adapter input for the existing #73 quality-observation boundary;
it must not be substituted for the 50-case
`eval/corpus/behavioral/conversion-quality-stage1/fixture.json` Stage 1 fixture,
and it must not introduce a second capture or release-gate semantics.

## Pinned release subjects

The initial comparison uses the published GitHub Release installers. The
installer digests match their release assets, and the engine/dictionary hashes
below were measured from those installers after extraction.

| Subject | Git SHA | Installer SHA-256 | Engine SHA-256 | `system.dic` SHA-256 |
|---|---|---|---|---|
| v1.0.5 | `e15c7b647c3af0c4c63ef4a7665b7e6d774cc530` | `07df28c465d3bbdfb9f6ed916e9f9d188e56d9855588bfc61c039f0a18828909` | `c58a32d0fd68599af927370d04aba54d13e11d2b82dba7525cf214ce4918496c` | `b7d08643395181f6d214866f9bb98646de366dc71caa15320effe774bc4c1d90` |
| v1.0.23 | `46d649282fff2dfebc09be9e0e8d8915031fb8b0` | `e24d1c44fe6cffcf4091c0bbb1a7b0241332921182395139e4110a6cb553c637` | `f923164ca35c8e5222c99d232eff9d0a3ac6c6e3f3fd29118be4289e5191ae33` | `ec296b7f68d6d5e55d7faed458473101675e29440dc1e8456998473e60c7632d` |

The v1.0.23 dictionary hash is also the hash of the v1.0.24 release dictionary;
v1.0.24 changed release provenance handling, not the conversion corpus used by
this experiment.

## What the fixture covers

The first six rows are fixed general-language regression controls from the
confirmed cross-version comparison:

| Reading | Expected fixed-control Top-1 | Reproduced after-side regression |
|---|---|---|
| `しようをかくにん` | `使用を確認` | `仕様を確認` |
| `しようをつづける` | `使用を続ける` | `仕様を続ける` |
| `しようじょうのちゅうい` | `使用上の注意` | `仕様上の注意` |
| `きのうのてんき` | `昨日の天気` | `機能の天気` |
| `きのうのできごと` | `昨日の出来事` | `機能の出来事` |
| `こうせいなはんだん` | `公正な判断` | `構成な判断` |

The reference comparison recorded the first column with v1.0.5, the bad
second column with v1.0.23, and the first column again when the v1.0.23 IT
compound bonus was disabled. The fixture records that evidence per case, but
the reference table is not a new source of semantic truth.

The IT side contains both positive compounds and coverage checks:

- `機能確認` is a Recall@5 candidate-presence target until its exact
  dictionary identity and calibrated cost are fixed;
- `機能紹介`, `機能要件`, and `機能仕様` are explicit compound Top-1 checks
  backed by the existing project-authored overlay;
- standalone `要件` and `紹介` are Recall@5 checks because `用件` and `照会`
  are valid homophones without context; and
- `縦`, `版`, `日`, `時`, `方`, and `替え` are Recall@5 coverage sentinels,
  and `気遣い` is a representative compound-rendaku sentinel. They are not
  unconditional Top-1 gold.

The general and IT rows share `contrast_group` values where they are useful
counterparts. A group is only a reporting slice: it does not lend context
from one reading to another, and it does not authorize a second ranking rule.

## Pairing and report invariants

Before/after comparison is a paired experiment over the same requests:

1. Load one immutable fixture and one canonical options object.
2. Run every case in the same order and with the same candidate bound on both
   sides.
3. Capture each side's candidates, terminal status, and stable provenance.
4. Score the declared Top-1 and Recall@5 assertions independently, then report
   general controls, IT positives, and coverage sentinels separately.

The pair must have equal corpus and options hashes. It must not require equal
artifact or dictionary hashes. Each report records those identities so a
reviewer can tell whether a change came from code, dictionary data, or both.
Different dictionary entry ordinals are expected across builds; surface
presence and stable candidate identity, when available, are the comparison
units. A missing origin or cost is an explicit unsupported observation, not a
reason to invent one from the surface string.

The exact compound-coherence ablation is the pinned patch in the fixture's
`ablation/` directory. It removes only that ranking-pass call from v1.0.23.
Using `it_bias_per_mille=0` is a broader experiment and must be labelled as
such because it also disables entry IT bias and completion coherence.

The report should reject, rather than silently repair, any of the following:

- a changed fixture or options object between sides;
- a missing case, duplicate case ID, or changed case order;
- a candidate list that exceeds the declared bound without a truncation
  terminal; or
- an assertion that treats a `candidate_presence_only` case as Top-1.

The reports may contain different engine/dictionary identities and different
candidate orders. Those are the measured result, not an input mismatch.

## Captured Phase 0 result

The three pinned subjects were captured twice with the standalone evaluator;
each pair was byte-for-byte identical. The retained candidate snapshots and
their SHA-256 values are in
`eval/baselines/ranking-comparison-issue93/manifest.json`.

The table below reports the declared assertion for each role. General controls
are Top-1 assertions; IT and coverage rows are a documented mixture of Top-1
and Recall@5 assertions.

| Subject | General controls | IT positives | Coverage sentinels |
|---|---:|---:|---:|
| v1.0.5 | 6/6 | 3/6 | 9/10 |
| v1.0.23 | 0/6 | 6/6 | 10/10 |
| v1.0.23 compound-coherence off | 6/6 | 6/6 | 10/10 |

The v1.0.23-to-ablation comparison changed seven measured ranks. All six
general controls improved from rank 2 to rank 1. `機能確認` moved from rank 1
to rank 2, but remains within its declared Recall@5 contract. The other five
IT positives and all ten coverage sentinels retained their declared result.
This isolates the global compound-coherence pass without claiming that the
ablation is the final CandidateRanker design.

## Scope and non-claims

This is a deterministic ranking observation fixture. It does not evaluate TSF
key routing, candidate popup behavior, learning, user dictionaries, neural
reranking, or application context. The fixed general Top-1 controls are
bounded regression assertions for the empty-context options in this fixture;
they are not claims that the same output is correct for every sentence.
Likewise, a Recall@5 pass says only that a useful candidate survived admission
and ranking. It does not establish that the candidate should be selected
without context.

The fixture therefore supports the Phase 0 acceptance question—whether the
same general-language floor and IT candidate coverage can be compared before
and after a single-ranker change—without allowing an ambiguous context-free
reading to become unconditional semantic gold.
