# Issue #412: base lemma detail batch 12

## Scope and acceptance

Add descriptions of previously undescribed base forms, using the existing strict inheritance contract for inflections. Preserve existing descriptions and conversion data; compare ranking and speed against the immediately preceding dictionary. The curated file adds 64 individually selected base-form rows. One draft contained widespread mismatches between candidate rows and descriptions and was discarded in full; selected rows from other drafts were checked individually.

## Coverage and integrity

- Total entries: 661,428, unchanged. Detail records: 77,049 → 78,100 (+1,051), or 11.65% → 11.81%. The 30% milestone requires 198,429 records, leaving 120,329; the overall objective remains 100%.
- Increase: 64 curated entry details, 773 generated inflection details, and 214 native inflection details.
- All 14 category TSVs were byte-identical. Only detail sections DIDX, DREC, DREL, DTOF and DTXT changed. All 77,049 existing descriptions and relations remained identical at their entry ordinals.
- Native inflection 羨み gained `「羨む」の活用形。基本形の意味：他人の持つものや境遇を自分も欲しいと思う。` The image test confirms the same entry had no baseline detail.
- The two-pass deterministic build passed. Baseline SHA-256: `CE445DBFC17B8BE387EAC4D5FAF8935057C225A82B828F771D9BE52516083CB9`; final: `1BDB5563C864BE38562CD41B1EB1F67A5AA69AB828CDE54F179DB885549BF870`.

## Verification

- Dictionary package tests, formatting, inflection report validation and affected-package clippy passed. Release ranking tests for issues 94, 348, 390 and 392 passed on both images. Identical non-detail sections provide structural evidence for candidate identity and ordering inputs; ranking tests cover selected scenarios.
- Full conversion, 5,000 samples per case: initial current-only p50/p99 was 1.404/1.565 ms baseline and 0.858/1.500 ms final; target bridge was 2.222/2.463 ms baseline and 1.425/2.361 ms final. The subsequent baseline repeat was 0.858/1.529 ms current-only and 1.428/2.470 ms target bridge. The initial median difference did not repeat; separate runs and uncontrolled load limit causal conclusions.
- Image benchmark (31 samples): candidate search/presentation p50/p99 was 1.153/1.908 ms baseline and 1.175/1.913 ms final; open-and-validate was 54.714/72.876 ms baseline and 51.227/56.184 ms final. This small sample is sensitive to load.
- Repository-scoped test processes were checked and none remained.

## Ownership and residual risk

Definitions belong to curated base-lemma data; the existing compiler owns strict inheritance and preserves explicit details. Ranking and runtime state contracts were not changed. Full-image comparison was required because rebuilt detail identifiers do not prove old-description preservation. Future batches can reuse these checks but must review new meanings; row identity validation does not detect mismatched meanings. The 30% milestone remains incomplete.
