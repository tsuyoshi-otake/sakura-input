# Issue #412: base lemma detail batch 8

## Scope and acceptance

Add reliable descriptions of previously undescribed base forms and inherit them only through the existing strict matching contract. Preserve existing descriptions and conversion data, and measure candidate ranking and speed against the preceding dictionary. Individually selected 56 base-form rows; unusual readings and uncertain meanings were excluded.

## Coverage and integrity

- Total entries: 661,428, unchanged. Detail records: 70,566 → 71,279 (+713), or 10.67% → 10.78%. The 30% milestone requires 198,429 records, leaving 127,150; the overall objective remains 100%.
- Increase: 56 curated entry details, 517 generated inflection details, and 140 native inflection details.
- All 14 category TSVs were byte-identical. Only detail sections DIDX, DREC, DREL, DTOF and DTXT changed. All 70,566 pre-existing descriptions and relations remained identical at their entry ordinals.
- Native inflection 守り抜き gained `「守り抜く」の活用形。基本形の意味：最後まで守り続ける。` The image test confirms the same entry had no baseline detail.
- The two-pass deterministic build passed. Baseline SHA-256: `992B930650E5CC29AA7FA04919E8A920E9CD44638C86AD543CBB8DD3BC3FC9AE`; final: `C3BBFDB93AA66F3FFD9B7D1C08BEAA7D6052A73E821C0363BCBBBE8C0AFBD68B`.

## Verification

- Dictionary package tests, formatting, inflection report validation, and affected-package clippy passed. Release ranking tests for issues 94, 348, 390 and 392 passed on both images. The identical non-detail sections independently establish preservation of candidate identity and ordering inputs; the tests cover selected conversion scenarios.
- Full conversion, 5,000 samples per case: initial current-only p50/p99 was 0.842/1.468 ms baseline and 1.392/1.534 ms final; target bridge was 1.393/2.308 ms baseline and 2.199/2.395 ms final. On repeating in reverse order, current-only was 0.839/1.470 ms baseline and 0.832/1.429 ms final; target bridge was 1.392/2.304 ms baseline and 1.380/2.250 ms final. The large first-run median difference did not repeat; the measurements do not establish its cause.
- Image benchmark (31 samples): candidate search/presentation p50/p99 was 1.128/2.785 ms baseline and 1.519/3.038 ms final; open-and-validate was 53.392/62.620 ms baseline and 60.728/81.573 ms final. This small sample is sensitive to load; no causal performance conclusion follows from it.
- Repository-scoped test processes were checked and none remained.

## Ownership and residual risk

Curated base-lemma data owns definitions; the dictionary compiler owns strict inheritance and preservation of explicit details. Ranking and runtime state contracts were not changed. Full-image comparison was necessary because rebuilt detail identifiers alone cannot prove old descriptions are preserved. The next batch can reuse these checks but must review new meanings. Matching reading and POS alone does not establish that different spellings share a sense: 押す/推す and 起こる/怒る provide counterexamples, so automatic copying is not justified. The 30% milestone remains incomplete.
