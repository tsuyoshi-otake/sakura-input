# Issue #412: base lemma detail batch 10

## Scope and acceptance

Add reliable descriptions of previously undescribed base forms, using the existing strict inheritance contract for inflections. Preserve existing descriptions and conversion data; compare ranking and speed against the immediately preceding dictionary. The curated file adds 145 individually selected base-form rows. Special readings and unclear meanings were excluded.

## Coverage and integrity

- Total entries: 661,428, unchanged. Detail records: 72,700 → 74,824 (+2,124), or 10.99% → 11.31%. The 30% milestone requires 198,429 records, leaving 123,605; the overall objective remains 100%.
- Increase: 145 curated entry details, 1,574 generated inflection details, and 405 native inflection details.
- All 14 category TSVs were byte-identical. Only detail sections DIDX, DREC, DREL, DTOF and DTXT changed. All 72,700 existing descriptions and relations remained identical at their entry ordinals.
- Native inflection 居座り gained `「居座る」の活用形。基本形の意味：その場所に長くとどまって、なかなか立ち去らない。` The image test confirms the same entry had no baseline detail.
- The two-pass deterministic build passed. Baseline SHA-256: `A23586BF32666326628A8CE3C547D568F7D98F41EE4231846F43E8166F38DC9B`; final: `F2BA163C4E124BD8112326675CA0C3F4C54ECCDEC505328D06D31016679C843C`.

## Verification

- Dictionary package tests, formatting, inflection report validation and affected-package clippy passed. Release ranking tests for issues 94, 348, 390 and 392 passed on both images. Identical non-detail sections provide structural evidence for candidate identity and ordering inputs; the ranking tests cover selected scenarios.
- Full conversion, 5,000 samples per case: initial current-only p50/p99 was 0.849/1.531 ms baseline and 1.445/3.621 ms final; target bridge was 1.404/2.420 ms baseline and 2.281/4.030 ms final. Repeated in reverse order, current-only was 1.394/1.621 ms baseline and 1.417/1.896 ms final; target bridge was 2.203/2.539 ms baseline and 2.237/2.988 ms final. Both runs had high outliers and the large initial median difference did not repeat; uncontrolled load limits causal conclusions.
- Image benchmark (31 samples): candidate search/presentation p50/p99 was 1.084/1.852 ms baseline and 1.465/3.012 ms final; open-and-validate was 55.174/72.340 ms baseline and 62.925/80.900 ms final. This small sample is sensitive to load.
- Repository-scoped test processes were checked and none remained.

## Ownership and residual risk

Definitions belong to curated base-lemma data; the existing compiler owns strict inheritance and preserves explicit details. Ranking and runtime state contracts were not changed. Full-image comparison was required because rebuilt detail identifiers do not prove old-description preservation. Future batches can reuse these checks but must review new meanings; the 30% milestone remains incomplete.
