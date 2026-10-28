# Issue #412: base-lemma descriptions, batch 50

## Acceptance and results

- Verify: match each new description against its seven-column Mozc identity, category uniqueness and existing coverage; review the meaning. Expect: selected rows match and ambiguous meanings are excluded. Result: passed.
- Verify: build the dictionary twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed; descriptions increased from 253,591 to 254,749 (+1,158), or 38.51%. This comprises 69 curated descriptions, 814 generated and 275 native inflection descriptions. The 40% threshold is 264,572.
- Verify: compare binary sections, decode each previous detail by ordinal, annotation, description and relations, and compare all 14 category files. Expect: existing details and conversion data unchanged. Result: passed; all 253,591 old details and category files were unchanged. Only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `6BC571CE9D6F50063259CD19FD47C155F153A0B3EDBFE2863BB0EBBBE3595045`; final `7004C1597510F3AD6DDF5BC737AA1BBD35E2A52E67009AC9FC489B59D29D23F8`.
- Verify: compare images and native `ばっくれ` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: pass. Result: passed; `ばっくれ` inherits the labelled meaning of `ばっくれる`. These tests cover selected queries, not every interactive conversion.
- Verify: run 100 alternating-order conversion pairs per reading and repeat with identical baseline images. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests, `git diff --check` and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed; package tests and static analysis were run before this data-only batch, and image and ranking checks were run for this batch.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 772.9/771.5, `にほんご` 1,625.1/1,626.1, `へんかんこうほ` 1,088.0/1,089.9 and `さくらにゅうりょく` 193.7/192.2 (100 samples per image). For `さくらにゅうりょく`, current-minus-baseline paired p50 was -32.0/+29.3 µs by baseline-first/current-first order, while identical-image calibration was -59.8/+57.3 µs. This order effect means these measurements cannot establish a causal speed change or rule out smaller effects.

Open/validate p50 was 68.7/71.0 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These local measurements include cache and order effects and do not establish causal effects on startup or interactive latency. The preceding batch's open/validate measurement was 66.9/78.3 ms, so startup performance remains an uncertainty.

## Scope and architecture

Curated base-lemma data owns the meanings, the existing compiler owns propagation, and the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. The required scope was curated data, build and import contracts, benchmark, ranking tests and this record. Whole-image comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks without reading unrelated modules; individual meanings and ambiguity still require review. The 40% milestone remains 9,823 entries away.
