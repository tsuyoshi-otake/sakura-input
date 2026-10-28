# Issue #412: base lemma detail batch 19

## Scope and acceptance

Add individually checked meanings for 45 previously undescribed base forms and inherit them to eligible inflections. Preserve existing details and conversion data; compare ranking and speed with the preceding image.

- Verify: build twice and inspect the report. Expect: reproducible output and added details. Result: two-pass build passed; 661,428 entries and 83,815 details, up from 83,135 (+680). Coverage is 12.67%; 30% requires 198,429 records, leaving 114,614. The overall objective remains 100%.
- Verify: compare all binary sections, existing detail records and relations by entry ordinal, and all 14 category TSVs. Expect: only detail sections change, old details stay identical, and category files match. Result: passed; only DIDX, DREC, DREL, DTOF and DTXT changed. Added records comprise 45 curated, 517 generated inflection and 118 native inflection details.
- Verify: check a newly inherited native inflection. Expect: `滅入り` has no baseline detail and gains the meaning of `滅入る`. Result: passed. Baseline SHA-256 `3B1DDED11BB1CA75DDFF02C96D8979CFC0ED407A70E9B9016AD383D086582914`; final `732E2EB3D86269713BAECEE6BCCB7F87E1EFE89D52D97563243715B47634C28A`.
- Verify: dictionary package tests, formatting, inflection report validation, affected-package clippy and release ranking tests for issues 94, 348, 390 and 392 against both images. Expect: all pass and repository test processes exit. Result: passed; final process check clean.
- Verify: measure 400 paired alternating conversions across four readings and compare candidate count and leader; separately measure image opening and detail lookup. Expect: report values and limitations. Result: paired baseline/current p50 1.059/1.055 ms and p99 1.836/1.794 ms; all candidate-count and leader checks passed. Image open/validate p50 was 53.290/51.869 ms (31 samples), and exact detail lookup/preview p50 0.1/0.2 microseconds (101 samples). These samples do not establish a global latency bound, and uncontrolled load affects timings.

## Ownership and residual risk

Curated base-lemma data owns meanings; the existing compiler owns strict inheritance and explicit-detail preservation. Ranking and runtime state contracts did not change. Full-image comparison is needed because detail IDs are regenerated. Future batches can reuse the checks but must individually review meaning and reading; matching row identities alone cannot validate semantics. The 30% milestone remains incomplete.
