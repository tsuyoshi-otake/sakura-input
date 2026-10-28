# Issue #412: base-lemma descriptions, batch 52

## Acceptance and results

- Verify: match each new description against its seven-column Mozc identity, category uniqueness and existing coverage; review the meaning. Expect: selected rows match and ambiguous meanings are excluded. Result: passed.
- Verify: build the dictionary twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed; descriptions increased from 256,215 to 257,764 (+1,549), or 38.97%. This comprises 89 curated descriptions, 1,127 generated and 333 native inflection descriptions. The 40% threshold is 264,572.
- Verify: compare binary sections, decode each previous detail by ordinal, annotation, description and relations, and compare all 14 category files. Expect: existing details and conversion data unchanged. Result: passed; all 256,215 old details and category files were unchanged. Only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `62C6854D777A60B91AF24FE7B7E5958B3430859E74F95969F1244F1CC67893AF`; final `F44980D3E81AFFAE3301DFA83F754B23CBB8D2036BE4F75E5A84E9B1558E76EB`.
- Verify: compare images and native `憚り` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: pass. Result: passed; `憚り` inherits the labelled meaning of `憚る`. These tests cover selected queries, not every interactive conversion.
- Verify: run 100 alternating-order conversion pairs per reading and repeat with identical baseline images. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed; package tests and static analysis were run before this data-only batch, and image and ranking checks were run for this batch.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 790.6/844.2, `にほんご` 1,646.1/1,651.7, `へんかんこうほ` 1,234.8/1,224.0 and `さくらにゅうりょく` 243.9/243.9 (100 samples per image). For `さくらにゅうりょく`, current-minus-baseline paired p50 was -62.1/+65.0 µs by baseline-first/current-first order, while identical-image calibration was -69.8/+69.0 µs. This order effect means these measurements cannot establish a causal speed change or rule out smaller effects.

Open/validate p50 was 65.3/67.5 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These local measurements include cache and order effects and do not establish causal effects on startup or interactive latency.

## Scope and architecture

Curated base-lemma data owns the meanings, the existing compiler owns propagation, and the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. The required scope was curated data, build and import contracts, benchmark, ranking tests and this record. Whole-image comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks without reading unrelated modules; individual meanings and ambiguity still require review. The 40% milestone remains 6,808 entries away.
