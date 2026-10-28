# Issue #412: base lemma detail batch 11

## Scope and acceptance

Add descriptions of previously undescribed base forms, using the existing strict inheritance contract for inflections. Preserve existing descriptions and conversion data; compare ranking and speed against the immediately preceding dictionary. The curated file adds 145 individually selected base-form rows; special readings and unclear meanings were excluded.

## Coverage and integrity

- Total entries: 661,428, unchanged. Detail records: 74,824 → 77,049 (+2,225), or 11.31% → 11.65%. The 30% milestone requires 198,429 records, leaving 121,380; the overall objective remains 100%.
- Increase: 145 curated entry details, 1,653 generated inflection details, and 427 native inflection details.
- All 14 category TSVs were byte-identical. Only detail sections DIDX, DREC, DREL, DTOF and DTXT changed. All 74,824 existing descriptions and relations remained identical at their entry ordinals.
- Native inflection 俯き gained `「俯く」の活用形。基本形の意味：顔を下の方へ向ける。` The image test confirms the same entry had no baseline detail.
- The two-pass deterministic build passed. Baseline SHA-256: `F2BA163C4E124BD8112326675CA0C3F4C54ECCDEC505328D06D31016679C843C`; final: `CE445DBFC17B8BE387EAC4D5FAF8935057C225A82B828F771D9BE52516083CB9`.

## Verification

- Dictionary package tests, formatting, inflection report validation and affected-package clippy passed. Release ranking tests for issues 94, 348, 390 and 392 passed on both images. Identical non-detail sections provide structural evidence for candidate identity and ordering inputs; ranking tests cover selected scenarios.
- Full conversion, 5,000 samples per case: current-only p50/p99 was 1.401/1.509 ms baseline and 1.400/1.589 ms final; target bridge was 2.214/2.380 ms baseline and 2.211/2.511 ms final. Separate runs and uncontrolled load limit causal conclusions.
- Image benchmark (31 samples): initial candidate search/presentation p50/p99 was 1.150/1.926 ms baseline and 1.663/3.070 ms final; repeat was 1.652/3.199 ms baseline and 1.428/2.990 ms final. Initial open-and-validate was 50.698/61.747 ms baseline and 50.724/73.896 ms final; repeat was 52.736/60.788 ms baseline and 54.112/73.926 ms final. This small sample is sensitive to load and does not prove a causal speed change.
- Repository-scoped test processes were checked and none remained.

## Ownership and residual risk

Definitions belong to curated base-lemma data; the existing compiler owns strict inheritance and preserves explicit details. Ranking and runtime state contracts were not changed. Full-image comparison was required because rebuilt detail identifiers do not prove old-description preservation. Future batches can reuse these checks but must review new meanings; the 30% milestone remains incomplete.
