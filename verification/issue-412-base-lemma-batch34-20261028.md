# Issue #412: base lemma detail batch 34

## Scope and acceptance

Add reviewed meanings for 334 previously unexplained base forms and inherit them to eligible inflections under the existing matching rules. Preserve prior details, candidates, costs and ranking.

- Verify: build both dictionary images twice. Expect: reproducible images and increased details. Result: passed; baseline SHA-256 `9EBFED0518EDCD583F000FADEC46F868C9DAD8283DCDE3A8FA8094F89542B23F` matches batch 33, and final SHA-256 `190B1DC8466B3F6C9A818D19B6763F4911EFBC2FFDED39BB1C9139FD7E91BB9E` matched across both final passes.
- Verify: count compiled details. Expect: more descriptions without additional candidates. Result: 661,428 entries and 133,871 details, up from 127,714 (+6,157): 334 curated, 3,264 generated-inflection and 2,559 native-inflection details. Coverage is 20.24%; 30% requires 198,429 records, leaving 64,558. The overall objective remains 100%.
- Verify: compare all binary sections and every previous detail and relation by ordinal, plus all 14 category files. Expect: only detail sections change, old details and categories remain identical. Result: passed; only DIDX, DREC, DREL, DTOF and DTXT changed, with all 127,714 previous details preserved.
- Verify: compare the images and inspect a newly described native inflection. Expect: `噛ん` inherits the labelled meaning of `噛む`. Result: passed.
- Verify: run dictionary package tests, affected-package clippy, formatting and inflection report validation; run issue 94, 348, 390 and 392 ranking tests against both images. Expect: all pass and repository test processes exit. Result: passed; final process check clean.
- Verify: measure 400 paired alternating conversions over four readings and compare candidate count and leader; separately measure image opening and detail lookup. Expect: report values and limitations. Result: paired baseline/final p50 1.028/1.027 ms and p99 2.007/1.967 ms; candidate-count and leader checks passed. Image open/validate p50 was 53.903/53.200 ms (31 samples), and exact detail lookup/preview p50 0.1/0.1 microseconds (101 samples). These samples do not establish a global latency bound; uncontrolled load affects timings.

## Ownership, scope and residual risk

Curated base-lemma data owns meanings; existing compiler inheritance owns matching and preservation. No new runtime state, ranking contract or matching rule was introduced. The scope was the curated data, dictionary image benchmark, compiler and engine test contracts, and this verification record; no architectural expansion was needed. Future additions can use the same data-only path but still require individual meaning and collision review, plus full-image comparison because detail IDs are regenerated. The 30% milestone remains incomplete.
