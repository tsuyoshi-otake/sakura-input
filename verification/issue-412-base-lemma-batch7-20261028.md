# Issue #412: base lemma detail batch 7

## Scope and acceptance

Add independently selected, reliable definitions for previously undescribed base-form entries; inherit them to eligible inflections; preserve existing descriptions and conversion data; compare ranking and performance with the preceding dictionary. The curated file adds 118 base-form rows. Drafts contained mismatched definitions, so selection explicitly excluded those and checked each retained reading, surface and meaning.

## Coverage and integrity

- Total entries: 661,428, unchanged. Detail records: 68,904 → 70,566 (+1,662), or 10.42% → 10.67%. The 30% milestone requires 198,429 records, leaving 127,863; the overall objective remains 100%.
- Increase: 119 curated entry details, 1,218 generated inflection details, and 325 native inflection details.
- All 14 category TSVs were byte-identical. Only detail sections DIDX, DREC, DREL, DTOF, and DTXT differed. All 68,904 existing details and their relations remained identical at the same entry ordinal.
- Native inflection example: 撫で acquired `「撫でる」の活用形。基本形の意味：手のひらなどでそっと触れて動かす。` The image test confirms the matched entry had no baseline detail. An initial test chose 閉まり but found no newly detailed matching native entry; the reason was not individually established, so no inheritance claim is made for it.
- The two-pass deterministic dictionary build passed. SHA-256: baseline `6BDF66C4A20383E62388B59EA8CE7C125128E1A16C72F53ADA1BB656332E0A78`; final `992B930650E5CC29AA7FA04919E8A920E9CD44638C86AD543CBB8DD3BC3FC9AE`.

## Verification

- Dictionary package tests, formatting, inflection report validation and clippy for affected packages passed. Release ranking tests for issues 94, 348, 390 and 392 passed on both images. Identical non-detail sections provide broader structural evidence for preserving candidate identity and ordering inputs; ranking tests cover specific scenarios.
- Release full-conversion benchmark, 5,000 samples per case: initial current-only p50/p99 was 0.997/1.677 ms baseline and 0.935/1.841 ms final; target bridge was 1.658/2.616 ms baseline and 1.551/3.199 ms final. The final run included an outlier above 25 ms. Repeat current-only p50/p99 was 0.848/1.518 ms baseline and 0.836/1.453 ms final; repeat target bridge was 1.407/2.393 ms baseline and 1.384/2.295 ms final. Variability prevents attributing differences to the dictionary change.
- Image benchmark (31 samples): candidate search/presentation p50/p99 was 1.406/2.950 ms baseline and 1.685/3.195 ms final; open-and-validate was 53.452/78.977 ms baseline and 78.387/88.237 ms final. These small samples varied substantially and cannot establish a causal performance change.
- The first image test failed on the unverified 閉まり example; changing the example to the verified 撫で entry passed. Repository-scoped test processes were checked and none remained.

## Ownership and residual risk

Definitions live in the curated base-lemma data; the existing compiler owns strict inheritance and preserves explicit details. No conversion ranking or runtime state contract changed. Full-image comparison was required because rebuilt detail IDs do not by themselves prove preservation. This report gives future batches a reproducible baseline, but new definitions still require semantic review. Bulk name labels or automatic WordNet sense assignment have not been shown safe for all candidates; the 30% milestone remains incomplete.
