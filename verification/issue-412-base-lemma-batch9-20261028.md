# Issue #412: base lemma detail batch 9

## Scope and acceptance

Add reliable descriptions of previously undescribed base forms, using the existing exact-match inheritance contract for inflections. Preserve prior descriptions and conversion data; compare ranking and speed with the immediately preceding dictionary. The curated file adds 87 individually selected base-form rows; special readings and unclear meanings were excluded.

## Coverage and integrity

- Total entries: 661,428, unchanged. Detail records: 71,279 → 72,700 (+1,421), or 10.78% → 10.99%. The 30% milestone requires 198,429 records, leaving 125,729; the overall objective remains 100%.
- Increase: 87 curated entry details, 1,049 generated inflection details, and 285 native inflection details.
- All 14 category TSVs were byte-identical. Only detail sections DIDX, DREC, DREL, DTOF and DTXT changed. All 71,279 pre-existing descriptions and relations remained identical at their entry ordinals.
- Native inflection 立ち去り gained `「立ち去る」の活用形。基本形の意味：その場所から離れて行く。` The image test confirms the same entry had no baseline detail.
- The two-pass deterministic build passed. Baseline SHA-256: `C3BBFDB93AA66F3FFD9B7D1C08BEAA7D6052A73E821C0363BCBBBE8C0AFBD68B`; final: `A23586BF32666326628A8CE3C547D568F7D98F41EE4231846F43E8166F38DC9B`.

## Verification

- Dictionary package tests, formatting, inflection report validation and affected-package clippy passed. Release ranking tests for issues 94, 348, 390 and 392 passed on both images. Identical non-detail sections provide broader structural evidence for candidate identity and ordering inputs; ranking tests cover selected scenarios.
- Full conversion, 5,000 samples per case: initial current-only p50/p99 was 1.394/1.516 ms baseline and 0.857/1.771 ms final; target bridge was 2.207/2.377 ms baseline and 1.421/2.884 ms final. A later baseline run measured 1.407/1.925 ms current-only and 2.222/3.027 ms target bridge. These are separate runs under uncontrolled load, and do not establish the cause of differences or a causal speed improvement.
- Image benchmark (31 samples): candidate search/presentation p50/p99 was 1.186/2.220 ms baseline and 1.145/1.820 ms final; open-and-validate was 60.103/79.899 ms baseline and 56.791/66.417 ms final. This small sample is sensitive to load.
- Repository-scoped test processes were checked and none remained.

## Ownership and residual risk

Definitions are owned by curated base-lemma data; the existing compiler owns strict inheritance and preserves explicit details. Ranking and runtime state contracts were not changed. Full-image comparison was required because rebuilt detail identifiers do not establish old-description preservation. Future batches can reuse these checks but must review new meanings; the 30% milestone remains incomplete.
