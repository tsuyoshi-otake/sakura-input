# Issue #412: base-lemma descriptions, batch 43

## Acceptance and results

- Verify: match each new description against its seven-column Mozc identity, category uniqueness and existing coverage; review the meaning. Expect: selected rows match and ambiguous meanings are excluded. Result: passed.
- Verify: build baseline and final dictionary twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed; descriptions increased from 210,169 to 222,528 (+12,359), or 33.64%. This comprises 692 curated descriptions, 8,835 generated and 2,832 native inflection descriptions. The 40% threshold is 264,572.
- Verify: compare binary sections, decode each previous detail by ordinal, annotation, description and relations, and compare all 14 category files. Expect: existing details and conversion data unchanged. Result: passed; all 210,169 old details and category files were unchanged. Only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `D7DD455B1AB9564B58100BFD3F6E86CD888990CAE13DD531236CA9F413AA085B`; final `516DF260AC2E27C19D9846561C152D9C1E4B490ECD749AD23C790CC7986B87C3`.
- Verify: compare images and native `よどみ` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: pass. Result: passed; `よどみ` inherits the labelled meaning of `よどむ`. These tests cover selected queries, not every interactive conversion.
- Verify: run 100 alternating-order conversion pairs per reading and repeat with identical baseline images. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed; package tests and static analysis were run before this data-only batch, and image and ranking checks were run for this batch.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 791.9/786.6, `にほんご` 1,650.0/1,647.1, `へんかんこうほ` 1,167.4/1,164.7 and `さくらにゅうりょく` 193.7/196.1 (100 samples per image). Pooled p50 was 887.4/1,054.9 µs across the four different readings; it crosses a gap between their latency distributions and should not be treated as a single-reading regression. For `さくらにゅうりょく`, current-minus-baseline paired p50 was -55.9/+55.2 µs by baseline-first/current-first order, while identical-image calibration was -43.6/+42.3 µs. This order effect means these measurements cannot establish a causal speed change or rule out smaller effects.

Open/validate p50 was 59.8/63.5 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These local measurements include cache and order effects and do not establish causal effects on startup or interactive latency.

## Scope and architecture

Curated base-lemma data owns the meanings, the existing compiler owns propagation, and the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. The required scope was curated data, build and import contracts, benchmark, ranking tests and this record. Whole-image comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks without reading unrelated modules; individual meanings and ambiguity still require review. The 40% milestone remains 42,044 entries away.
