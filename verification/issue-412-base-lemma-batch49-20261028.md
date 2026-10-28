# Issue #412: base-lemma descriptions, batch 49

## Acceptance and results

- Verify: match each new description against its seven-column Mozc identity, category uniqueness and existing coverage; review the meaning. Expect: selected rows match and ambiguous meanings are excluded. Result: passed.
- Verify: build the dictionary twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed; descriptions increased from 251,378 to 253,591 (+2,213), or 38.34%. This comprises 139 curated descriptions, 1,417 generated and 657 native inflection descriptions. The 40% threshold is 264,572.
- Verify: compare binary sections, decode each previous detail by ordinal, annotation, description and relations, and compare all 14 category files. Expect: existing details and conversion data unchanged. Result: passed; all 251,378 old details and category files were unchanged. Only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `CC788D9D43027C45C925531A584094C5D8EF6AC338A23A0E3F12F5C50FB27420`; final `6BC571CE9D6F50063259CD19FD47C155F153A0B3EDBFE2863BB0EBBBE3595045`.
- Verify: compare images and native `うすれ` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: pass. Result: passed; `うすれ` inherits the labelled meaning of `うすれる`. These tests cover selected queries, not every interactive conversion.
- Verify: run 100 alternating-order conversion pairs per reading and repeat with identical baseline images. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed; package tests and static analysis were run before this data-only batch, and image and ranking checks were run for this batch.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 820.0/815.4, `にほんご` 1,683.7/1,689.5, `へんかんこうほ` 1,205.9/1,194.2 and `さくらにゅうりょく` 214.2/213.4 (100 samples per image). For `さくらにゅうりょく`, current-minus-baseline paired p50 was -61.9/+59.8 µs by baseline-first/current-first order, while identical-image calibration was -62.0/+61.2 µs. This order effect means these measurements cannot establish a causal speed change or rule out smaller effects.

Open/validate p50 was 66.9/78.3 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. The open measurement rose in this run but includes cache and order effects; it cannot establish a causal startup regression or rule one out.

## Scope and architecture

Curated base-lemma data owns the meanings, the existing compiler owns propagation, and the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. The required scope was curated data, build and import contracts, benchmark, ranking tests and this record. Whole-image comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks without reading unrelated modules; individual meanings and ambiguity still require review. The 40% milestone remains 10,981 entries away.
