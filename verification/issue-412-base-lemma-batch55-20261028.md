# Issue #412: base-lemma descriptions, batch 55

## Acceptance and results

- Verify: match each new description against its seven-column Mozc identity, category uniqueness and existing coverage; review the meaning. Expect: selected rows match and ambiguous meanings are excluded. Result: passed.
- Verify: build the dictionary twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed; descriptions increased from 259,674 to 261,057 (+1,383), or 39.47%. This comprises 78 curated descriptions, 1,013 generated and 292 native inflection descriptions. The 40% threshold is 264,572.
- Verify: compare binary sections, decode each previous detail by ordinal, annotation, description and relations, and compare all 14 category files. Expect: existing details and conversion data unchanged. Result: passed; all 259,674 old details and category files were unchanged. Only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `818E2CA2D4FCE96747B9A79323933CB7BA4BC702C229F72FAE8BEF73B89B6D3D`; final `908E4B8BF05E8B79ED0A385A71BF1165FEB733F8A572388E0A6F00A8301F5921`.
- Verify: compare images and native `見初め` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: pass. Result: passed; `見初め` inherits the labelled meaning of `見初める`. These tests cover selected queries, not every interactive conversion.
- Verify: run 100 alternating-order conversion pairs per reading and repeat with identical baseline images. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed; package tests and static analysis were run before this data-only batch, and image and ranking checks were run for this batch.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 833.9/836.8, `にほんご` 1,771.6/1,790.1, `へんかんこうほ` 1,273.1/1,234.5 and `さくらにゅうりょく` 224.6/234.2 (100 samples per image). For `さくらにゅうりょく`, current-minus-baseline paired p50 was -41.8/+46.6 µs by baseline-first/current-first order, while identical-image calibration was -59.5/+58.0 µs. This order effect means these measurements cannot establish a causal speed change or rule out smaller effects.

Open/validate p50 was 63.7/80.4 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These local measurements include cache and order effects and do not establish causal effects on startup or interactive latency. The open measurement increased in this run; a causal startup regression cannot be confirmed or excluded.

## Scope and architecture

Curated base-lemma data owns the meanings, the existing compiler owns propagation, and the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. The required scope was curated data, build and import contracts, benchmark, ranking tests and this record. Whole-image comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks without reading unrelated modules; individual meanings and ambiguity still require review. The 40% milestone remains 3,515 entries away.
