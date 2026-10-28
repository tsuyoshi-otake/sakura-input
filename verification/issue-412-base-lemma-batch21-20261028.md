# Issue #412: base lemma detail batch 21

## Scope and acceptance

Add individually checked meanings for 21 previously undescribed base forms and inherit them to eligible inflections. Preserve existing details and conversion data; compare ranking and speed with the preceding image.

- Verify: build twice and inspect the report. Expect: reproducible output and added details. Result: two-pass build passed; 661,428 entries and 84,881 details, up from 84,588 (+293). Coverage is 12.83%; 30% requires 198,429 records, leaving 113,548. The overall objective remains 100%.
- Verify: compare all binary sections, existing detail records and relations by entry ordinal, and all 14 category TSVs. Expect: only detail sections change, old details stay identical, and category files match. Result: passed; only DIDX, DREC, DREL, DTOF and DTXT changed. Added records comprise 21 curated, 220 generated inflection and 52 native inflection details.
- Verify: check a newly inherited native inflection. Expect: `廃れ` has no baseline detail and gains the meaning of `廃れる`. Result: passed. Baseline SHA-256 `C2CFC52A2F4FEC5F3213EEB6FAFBC8D80E30DE0A8E4A3DA97143D7CE8DD014A2`; final `85B83330DAA546BD6B390F259AB4298715D02AE6324D40C30B54D22580711115`.
- Verify: dictionary package tests, formatting, inflection report validation, affected-package clippy and release ranking tests for issues 94, 348, 390 and 392 against both images. Expect: all pass and repository test processes exit. Result: passed; final process check clean.
- Verify: measure 400 paired alternating conversions across four readings and compare candidate count and leader; separately measure image opening and detail lookup. Expect: report values and limitations. Result: paired baseline/current p50 1.223/1.236 ms and p99 3.358/3.150 ms; all candidate-count and leader checks passed. Image open/validate p50 was 56.328/62.395 ms (31 samples), and exact detail lookup/preview p50 0.1/0.1 microseconds (101 samples). The opening measurement rose in this run; these samples do not establish causation or a global latency bound, and uncontrolled load affects timings.

## Ownership, scope and residual risk

Curated base-lemma data owns meanings; the existing compiler owns strict inheritance and explicit-detail preservation. Ranking and runtime state contracts did not change. Full-image comparison is needed because detail IDs are regenerated; this expands verification beyond the edited data. Future batches can reuse the checks but must individually review meaning and reading; matching row identities alone cannot validate semantics. No architecture boundary changed. The 30% milestone remains incomplete.
