# Issue #412: base lemma detail batch 30

## Scope and acceptance

Add reviewed meanings for 39 previously unexplained base forms and inherit them to eligible inflections using the existing matching rules. Preserve prior descriptions, candidate entries, costs and ranking.

- Verify: build each dictionary image twice. Expect: reproducible images and more descriptions. Result: passed; 661,428 entries and 112,772 details, up from 112,059 (+713). Coverage is 17.05%; the 30% milestone requires 198,429 details, leaving 85,657. The overall objective remains 100%.
- Verify: compare all binary sections and every previous detail and relation by entry ordinal, plus all 14 category files. Expect: only detail sections change; existing details and categories remain identical. Result: passed; only DIDX, DREC, DREL, DTOF and DTXT changed, and all 112,059 prior records were preserved. The increase comprises 39 curated, 421 generated inflection and 253 native inflection details. Baseline SHA-256 `6BF9EB054D326BB4AD7D36F3133BC5338FF5B4769709DCAD657FADFCB220EA5A`; final `81105BBCB3CD24DD210038E9D5BECFC0EAC5EEE49323674F59FA68E2EE0C8969`.
- Verify: compare the images and inspect a newly described native inflection. Expect: `青臭っ` inherits the labelled meaning of `青臭い`. Result: passed.
- Verify: run dictionary package tests, affected-package clippy, formatting and inflection report validation; run issue 94, 348, 390 and 392 ranking tests against both images. Expect: all pass and repository test processes exit. Result: passed; final process check clean. An earlier process check ran while the separate dictionary build was still active and correctly reported its compiler processes; the check passed after the build exited.
- Verify: measure 400 paired alternating conversions over four readings, checking candidate counts and leaders; separately sample image opening and detail lookup. Expect: report values and limitations. Result: paired baseline/final p50 1.071/1.057 ms and p99 3.003/2.978 ms; candidate-count and leader checks passed. Image open/validate p50 was 54.828/56.072 ms (31 samples), and exact detail lookup/preview p50 0.1/0.1 microseconds (101 samples). These samples do not establish a global latency bound; uncontrolled load affects timings.

## Ownership, scope and residual risk

Curated base-lemma data owns the meanings; the existing compiler inheritance owns matching and preservation. No new runtime state, ranking contract or matching rule was introduced. The scope was the curated data, dictionary image benchmark, compiler and engine test contracts, and this verification record. Investigation expanded to possible additional native forms, but their indexing was not changed in this batch. Future similar work can follow this same data-only path; individual meanings and collisions still require review, and full-image comparisons remain necessary because detail IDs are regenerated. The 30% milestone remains incomplete.
