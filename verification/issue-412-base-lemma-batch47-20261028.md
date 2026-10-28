# Issue #412: base-lemma descriptions, batch 47

## Acceptance and results

- Verify: match each new description against its seven-column Mozc identity, category uniqueness and existing coverage; review the meaning. Expect: selected rows match and ambiguous meanings are excluded. Result: passed.
- Verify: build the dictionary twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed; descriptions increased from 242,482 to 247,461 (+4,979), or 37.41%. This comprises 302 curated descriptions, 3,662 generated and 1,015 native inflection descriptions. The 40% threshold is 264,572.
- Verify: compare binary sections, decode each previous detail by ordinal, annotation, description and relations, and compare all 14 category files. Expect: existing details and conversion data unchanged. Result: passed; all 242,482 old details and category files were unchanged. Only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `2E2D48859A9C8E80DA423EF44AA21D2E6B45C2CA49BABC2DCFECF0B05F19BEDC`; final `B1E37786A40E4F976CAA535351FB4601C49CA049E4B86BC0FA930AF481269B3E`.
- Verify: compare images and native `おがみ` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: pass. Result: passed; `おがみ` inherits the labelled meaning of `おがむ`. These tests cover selected queries, not every interactive conversion.
- Verify: run 100 alternating-order conversion pairs per reading and repeat with identical baseline images. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed; package tests and static analysis were run before this data-only batch, and image and ranking checks were run for this batch.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 767.7/771.4, `にほんご` 1,625.6/1,617.3, `へんかんこうほ` 1,130.0/1,135.4 and `さくらにゅうりょく` 192.0/193.0 (100 samples per image). For `さくらにゅうりょく`, current-minus-baseline paired p50 was -34.7/+37.5 µs by baseline-first/current-first order, while identical-image calibration was -75.6/+73.9 µs. This order effect means these measurements cannot establish a causal speed change or rule out smaller effects.

Open/validate p50 was 63.3/65.1 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These local measurements include cache and order effects and do not establish causal effects on startup or interactive latency.

## Scope and architecture

Curated base-lemma data owns the meanings, the existing compiler owns propagation, and the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. The required scope was curated data, build and import contracts, benchmark, ranking tests and this record. Whole-image comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks without reading unrelated modules; individual meanings and ambiguity still require review. The 40% milestone remains 17,111 entries away.
