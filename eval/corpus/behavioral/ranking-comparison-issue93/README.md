# Issue #93 Phase 0 ranking comparison fixture

This directory contains the small, deterministic corpus for comparing the
ordinary-conversion ranking before and after the Issue #93 work. It is a
versioned comparison input/adaptor on top of the #73 quality-observation
boundary. It is deliberately separate from the 50-case
`eval/corpus/behavioral/conversion-quality-stage1/fixture.json`: the Stage 1 parser requires exactly 50 cases
and its segment observations have a different contract. The comparator must
map both inputs to the same artifact/options/observation identity fields
defined by that existing boundary; this fixture does not create a second
quality runner or release gate.

The fixture has 22 cases in three explicit roles:

- `general_negative_control`: the six general-language phrases whose Top-1
  regression was reproduced in the Issue #93 cross-version investigation;
- `it_positive`: technical compounds and standalone terms that must remain
  useful while the general controls are repaired; and
- `coverage_sentinel`: `縦`, `版`, `日`, `時`, `方`, and `替え`, plus a
  representative `気遣い` compound, used to detect a surface disappearing
  because internal POS/connection rows or rendaku boundaries consumed a
  candidate budget.

`contrast_group` puts related general and IT cases in the same comparison
slice. It is a reporting grouping, not a claim that the cases share one
meaning or that one case supplies context for another.

## Assertion contract

Each case declares `assertion_kind` and `assertion_k`:

- `top1` is used for the six fixed general regression controls and for the
  explicit IT compounds where the target is justified by the existing
  dictionary contract.
- `recall_at_k` is used for homophone-prone standalone terms and coverage
  sentinels. The target only needs to occur within the first five candidates;
  it must not be promoted to an unconditional semantic Top-1 label.

`expected_surface` is the surface being measured. For a recall assertion it
means “keep this candidate reachable,” not “this is the right meaning in every
context.” `semantic_scope` makes that distinction machine-readable. The
`ambiguity` and `rationale` fields are required review notes, so adding a case
does not silently turn an ambiguous reading into semantic gold.

The six direct observations retain `reference_observation` for the
v1.0.5/v1.0.23/IT-bonus-off investigation. Those values explain why the
controls exist; they are not a substitute for capturing the actual artifact
and dictionary used by a new run.

## Before/after comparison identity

The baseline and after reports must be generated from the same committed
`fixture.json`, the same canonical `options` object, the same case order, and
the same candidate limit. The runner should record a corpus hash and an
options hash in each report and reject a pair when either hash differs.

Artifact and dictionary identity are intentionally per-report metadata. The
baseline and after sides may use different engine binaries, git revisions,
system dictionaries, or dictionary build hashes; those differences are the
thing being measured and must be shown rather than treated as a fixture
mismatch. A report pair is invalid only when the declared corpus/options
inputs differ or when provenance is missing.

At minimum, each side records:

- git/build identity and engine SHA-256;
- dictionary path-independent identity and SHA-256;
- the exact options/profile identity;
- candidate surfaces in emitted order, candidate rank, and stable candidate
  identity when the API exposes one; and
- terminal/truncation status and bounded timing metadata.

Candidate text is compared by the fixture's declared surface assertions.
Dictionary entry identity, source, and cost are diagnostic evidence; they are
not allowed to make a surface disappear from the comparison merely because
the before and after dictionaries have different ordinals.

The reference labels in the fixture are historical (`v1.0.5`, `v1.0.23`,
and `v1.0.23-it-compound-coherence-off`). A new report must still use its actual
artifact/dictionary identities and must not claim to reproduce those labels
without the corresponding artifacts.

The exact v1.0.23 feature ablation is fixed under `ablation/`. Its manifest
pins the v1.0.23 commit and patch SHA-256. The patch removes only the
`apply_it_compound_coherence` call; it does not set `it_bias_per_mille=0`,
because that would also remove entry-level IT bias and completion coherence
and would therefore measure a different experiment.
