# Issue #412: base lemma detail batch 13

## Scope and acceptance

Add individually reviewed meanings for 46 previously undescribed base forms and use the existing strict inheritance contract for their inflections. Preserve existing descriptions and conversion data. Compare ranking and speed against the preceding dictionary.

- Verify: build the dictionary twice and inspect the build report. Expect: reproducible output with additional detail records. Result: both builds matched; 661,428 entries and 78,810 details, up from 78,100 (+710). Coverage is 11.92%; the 30% milestone requires 198,429 records, leaving 119,619. The overall objective remains 100%.
- Verify: compare all binary sections, every existing detail and relation by entry ordinal, and all 14 category files. Expect: only detail sections change, no old detail is lost or modified, and category files remain identical. Result: passed; only DIDX, DREC, DREL, DTOF and DTXT changed. Added records comprise 46 curated details, 526 generated inflection details and 138 native inflection details.
- Verify: check a newly inherited native inflection. Expect: `巣食い` has no baseline detail and gains the meaning of `巣食う`. Result: passed. Baseline SHA-256 `1BDB5563C864BE38562CD41B1EB1F67A5AA69AB828CDE54F179DB885549BF870`; final `B6232786A0B69763AED768E33904E1DAF8B5A15867937A361E64D9009F3788DD`.
- Verify: run dictionary tests, formatting, inflection report validation, affected-package clippy, and release ranking tests for issues 94, 348, 390 and 392 against both images. Expect: all pass and test processes exit. Result: passed, and the final repository-scoped process check was clean. A preliminary process check detected the concurrently running dictionary build; it completed normally and the final check was repeated after it finished.
- Verify: compare full conversion latency with 5,000 samples and image candidate search/presentation latency with 31 samples. Expect: measure and investigate material changes, without treating noisy samples as proof of equivalence. Result: full conversion current-only p50/p99 was 0.853/1.503 ms baseline and 0.857/1.494 ms final; target bridge was 1.417/2.384 ms and 1.427/2.383 ms. The initial candidate search/presentation p50/p99 was 1.126/1.721 ms and 1.663/3.078 ms; repeat was 1.116/1.760 ms and 1.159/1.805 ms. The initial increase did not repeat, and uncontrolled load and the small sample limit the conclusion.

## Ownership and residual risk

Curated base-lemma data owns the meanings; the existing dictionary compiler owns inheritance and preservation of explicit details. Ranking inputs and runtime state contracts were not changed. Full-image comparison was needed because regenerated detail identifiers alone cannot establish preservation. Future batches can reuse these checks but must individually check meanings and readings: identity validation cannot detect a mismatched gloss. The full-coverage objective remains incomplete.
