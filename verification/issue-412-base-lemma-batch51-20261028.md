# Issue #412: base-lemma descriptions, batch 51

## Acceptance and results

- Verify: match each new description against its seven-column Mozc identity, category uniqueness and existing coverage; review the meaning. Expect: selected rows match and ambiguous meanings are excluded. Result: passed.
- Verify: build the dictionary twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed; descriptions increased from 254,749 to 256,215 (+1,466), or 38.74%. This comprises 83 curated descriptions, 1,030 generated and 353 native inflection descriptions. The 40% threshold is 264,572.
- Verify: compare binary sections, decode each previous detail by ordinal, annotation, description and relations, and compare all 14 category files. Expect: existing details and conversion data unchanged. Result: passed; all 254,749 old details and category files were unchanged. Only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `7004C1597510F3AD6DDF5BC737AA1BBD35E2A52E67009AC9FC489B59D29D23F8`; final `62C6854D777A60B91AF24FE7B7E5958B3430859E74F95969F1244F1CC67893AF`.
- Verify: compare images and native `食わし` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: pass. Result: passed; `食わし` inherits the labelled meaning of `食わす`. These tests cover selected queries, not every interactive conversion.
- Verify: run 100 alternating-order conversion pairs per reading and repeat with identical baseline images. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed; package tests and static analysis were run before this data-only batch, and image and ranking checks were run for this batch.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 808.5/805.4, `にほんご` 1,678.1/1,676.3, `へんかんこうほ` 1,196.7/1,197.6 and `さくらにゅうりょく` 219.5/224.6 (100 samples per image). For `さくらにゅうりょく`, current-minus-baseline paired p50 was -58.4/+52.8 µs by baseline-first/current-first order, while identical-image calibration was -37.6/+42.4 µs. This order effect means these measurements cannot establish a causal speed change or rule out smaller effects.

Open/validate p50 was 66.9/65.0 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These local measurements include cache and order effects and do not establish causal effects on startup or interactive latency.

## Scope and architecture

Curated base-lemma data owns the meanings, the existing compiler owns propagation, and the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. The required scope was curated data, build and import contracts, benchmark, ranking tests and this record. Whole-image comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks without reading unrelated modules; individual meanings and ambiguity still require review. The 40% milestone remains 8,357 entries away.
