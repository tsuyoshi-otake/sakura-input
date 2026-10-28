# Issue #412: base lemma detail batch 18

## Scope and acceptance

Add individually checked meanings for 49 previously undescribed base forms and inherit them to eligible inflections. Preserve existing details and conversion data; compare ranking and speed with the preceding image.

- Verify: build twice and inspect the report. Expect: reproducible output and added details. Result: two-pass build passed; 661,428 entries and 83,135 details, up from 82,309 (+826). Coverage is 12.57%; 30% requires 198,429 records, leaving 115,294. The overall objective remains 100%.
- Verify: compare all binary sections, existing detail records and relations by entry ordinal, and all 14 category TSVs. Expect: only detail sections change, old details stay identical, and category files match. Result: passed; only DIDX, DREC, DREL, DTOF and DTXT changed. Added records comprise 49 curated, 622 generated inflection and 155 native inflection details.
- Verify: check a newly inherited native inflection. Expect: `静まり` has no baseline detail and gains the meaning of `静まる`. Result: passed. Baseline SHA-256 `6786C86F995B6EAA4BCEE7EC9085C419B4678330F03A633193AF8AFEAAD3D42E`; final `3B1DDED11BB1CA75DDFF02C96D8979CFC0ED407A70E9B9016AD383D086582914`.
- Verify: dictionary package tests, formatting, inflection report validation, affected-package clippy and release ranking tests for issues 94, 348, 390 and 392 against both images. Expect: all pass and repository test processes exit. Result: passed; final process check clean.
- Verify: measure 400 paired alternating conversions across four readings and compare candidate count and leader; separately measure image opening and detail lookup. Expect: report values and limitations. Result: paired baseline/current p50 1.165/1.160 ms and p99 3.179/3.191 ms; all candidate-count and leader checks passed. Image open/validate p50 was 68.917/55.194 ms (31 samples), and exact detail lookup/preview p50 0.1/0.1 microseconds (101 samples). These samples do not establish a global latency bound, and uncontrolled load affects timings.

## Ownership and residual risk

Curated base-lemma data owns meanings; the existing compiler owns strict inheritance and explicit-detail preservation. Ranking and runtime state contracts did not change. Full-image comparison is needed because detail IDs are regenerated. Future batches can reuse the checks but must individually review meaning and reading; matching row identities alone cannot validate semantics. The 30% milestone remains incomplete.
