# Issue #412: base lemma detail batch 16

## Scope and acceptance

Add individually checked meanings for 48 previously undescribed base forms and inherit them to eligible inflections. Preserve existing details and conversion data; compare ranking and speed with the preceding image.

- Verify: build twice and inspect the report. Expect: reproducible output and added details. Result: two-pass build passed; 661,428 entries and 81,414 details, up from 80,638 (+776). Coverage is 12.31%; 30% requires 198,429 records, leaving 117,015. The overall objective remains 100%.
- Verify: compare all binary sections, all existing detail records and relations by entry ordinal, and all 14 category TSVs. Expect: only detail sections change, old details stay identical, and category files match. Result: passed; only DIDX, DREC, DREL, DTOF and DTXT changed. Added records comprise 48 curated, 565 generated inflection and 163 native inflection details.
- Verify: check a newly inherited native inflection. Expect: `転び` has no baseline detail and gains the meaning of `転ぶ`. Result: passed. Baseline SHA-256 `E19782AE03EFF256AF8F092D6DFFEA6137B3CBAEE4FF97EA7D8858162CC409E6`; final `D71DA3C07FAAD1B904DEF71922AE42B8CC543E95FA679B3A828E0E39C342374A`.
- Verify: dictionary package tests, formatting, inflection report validation, affected-package clippy and release ranking tests for issues 94, 348, 390 and 392 against both images. Expect: all pass and repository test processes exit. Result: passed; final process check clean.
- Verify: measure 400 paired alternating conversions across four readings and compare candidate count and leader; separately measure image opening and detail lookup. Expect: report values and limitations. Result: paired baseline/current p50 1.084/1.079 ms and p99 2.519/2.076 ms; all candidate-count and leader checks passed. Image open/validate p50 was 49.377/49.337 ms (31 samples), and exact detail lookup/preview p50 0.1/0.1 microseconds (101 samples). These samples do not establish a global latency bound, and uncontrolled load affects timings.

## Ownership and residual risk

Curated base-lemma data owns meanings; the existing compiler owns strict inheritance and explicit-detail preservation. Ranking and runtime state contracts did not change. Full-image comparison is needed because detail IDs are regenerated. Future batches can reuse the checks but must individually review meaning and reading; matching row identities alone cannot validate semantics. The 30% milestone remains incomplete.
