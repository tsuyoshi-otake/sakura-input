# Issue #412: base lemma detail batch 33

## Scope and acceptance

Add reviewed meanings for 368 previously unexplained base forms and inherit them to eligible inflections under the existing matching rules. Preserve prior details, candidates, costs and ranking.

- Verify: build both dictionary images twice. Expect: reproducible images and increased details. Result: passed; baseline SHA-256 `02ACC213F888B697B41AD586B6D921AA15D13492184ECE4CEAC95A969588F96F` matches batch 32, and final SHA-256 `9EBFED0518EDCD583F000FADEC46F868C9DAD8283DCDE3A8FA8094F89542B23F` matched across both final passes.
- Verify: count compiled details. Expect: more descriptions without additional candidates. Result: 661,428 entries and 127,714 details, up from 120,715 (+6,999): 368 curated, 4,466 generated-inflection and 2,165 native-inflection details. Coverage is 19.31%; 30% requires 198,429 records, leaving 70,715. The overall objective remains 100%.
- Verify: compare all binary sections and every previous detail and relation by ordinal, plus all 14 category files. Expect: only detail sections change, old details and categories remain identical. Result: passed; only DIDX, DREC, DREL, DTOF and DTXT changed, with all 120,715 previous details preserved.
- Verify: compare the images and inspect a newly described native inflection. Expect: `はずん` inherits the labelled meaning of `はずむ`. Result: passed.
- Verify: run dictionary package tests, affected-package clippy, formatting and inflection report validation; run issue 94, 348, 390 and 392 ranking tests against both images. Expect: all pass and repository test processes exit. Result: passed; final process check clean.
- Verify: measure 400 paired alternating conversions over four readings and compare candidate count and leader; separately measure image opening and detail lookup. Expect: report values and limitations. Result: paired baseline/final p50 1.057/1.052 ms and p99 1.865/1.892 ms; candidate-count and leader checks passed. Image open/validate p50 was 57.277/57.643 ms (31 samples), and exact detail lookup/preview p50 0.1/0.1 microseconds (101 samples). These samples do not establish a global latency bound; uncontrolled load affects timings.

## Ownership, scope and residual risk

Curated base-lemma data owns meanings; existing compiler inheritance owns matching and preservation. No new runtime state, ranking contract or matching rule was introduced. The scope was the curated data, dictionary image benchmark, compiler and engine test contracts, and this verification record; no architectural expansion was needed. Future additions can use the same data-only path but still require individual meaning and collision review, plus full-image comparison because detail IDs are regenerated. The 30% milestone remains incomplete.
