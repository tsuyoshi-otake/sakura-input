# Issue #412: base-lemma descriptions, batch 42

## Acceptance and results

- Verify: match each new description against its seven-column Mozc identity, category uniqueness and existing coverage; review the meaning. Expect: selected rows match and ambiguous meanings are excluded. Result: passed.
- Verify: build baseline and final dictionary twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed; descriptions increased from 198,877 to 210,169 (+11,292), or 31.77%. This comprises 631 curated descriptions, 7,611 generated and 3,050 native inflection descriptions. The 40% threshold is 264,572.
- Verify: compare binary sections, decode each previous detail by ordinal, annotation, description and relations, and compare all 14 category files. Expect: existing details and conversion data unchanged. Result: passed; all 198,877 old details and category files were unchanged. Only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `624554A082629DD693FED5DB4359392A6115129234F0C7B4C11C34C3F82C2463`; final `D7DD455B1AB9564B58100BFD3F6E86CD888990CAE13DD531236CA9F413AA085B`.
- Verify: compare images and native `彷徨い` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: pass. Result: passed; `彷徨い` inherits the labelled meaning of `彷徨く`. These tests cover selected queries, not every interactive conversion.
- Verify: run 100 alternating-order conversion pairs per reading and repeat with identical baseline images. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 788.1/783.5, `にほんご` 1,629.6/1,636.7, `へんかんこうほ` 1,140.8/1,142.0 and `さくらにゅうりょく` 195.6/195.4 (100 samples per image). The pooled p50 was 1,078.2/1,062.4 µs. For `さくらにゅうりょく`, current-minus-baseline paired p50 was -34.2/+33.8 µs by baseline-first/current-first order, while identical-image calibration was -46.1/+39.2 µs. The order effect means these measurements cannot establish a causal speed change or rule out smaller effects.

Open/validate p50 was 63.8/63.1 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These local measurements include cache and order effects and do not establish causal effects on startup or interactive latency.

## Scope and architecture

Curated base-lemma data owns the meanings, the existing compiler owns propagation, and the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. The required scope was curated data, build and import contracts, benchmark, ranking tests and this record. Whole-image comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks without reading unrelated modules; individual meanings and ambiguity still require review. The 40% milestone remains 54,403 entries away.
