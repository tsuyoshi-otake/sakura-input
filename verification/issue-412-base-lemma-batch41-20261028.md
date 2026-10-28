# Issue #412: base-lemma descriptions, batch 41

## Acceptance and results

- Verify: match each new description against the seven-column source identity and exclude existing coverage and duplicate reading/surface pairs. Expect: every selected row matches. Result: passed; ambiguous or circular meanings were excluded after review.
- Verify: build the baseline and final dictionary twice using `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed. Details increased from 186,220 to 198,877 (+12,657), reaching 30.07%. The increase comprises 701 curated base descriptions, 8,327 generated and 3,629 native inflection descriptions. The 30% threshold is 198,429.
- Verify: compare binary sections, decode every old detail by ordinal, annotation, description and relations, and hash all 14 category TSVs. Expect: existing details preserved, only detail sections changed, and category files identical. Result: passed; all 186,220 previous details preserved, with changes limited to DIDX, DREC, DREL, DTOF and DTXT. Baseline SHA-256 `70BBE8D5DFFAAF65D25AF75BB305001BA7FC59AB295C7F5F7EF26092BA716FE3`; final `624554A082629DD693FED5DB4359392A6115129234F0C7B4C11C34C3F82C2463`.
- Verify: compare dictionary images and native `くばっ` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: candidate checks and tests pass. Result: passed; `くばっ` inherits the labelled meaning of `くばる`. The tests cover selected queries, not every interactive conversion.
- Verify: run 100 alternating-order conversion pairs per reading and repeat with identical baseline images. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: run dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 765.6/770.6, `にほんご` 1,610.9/1,622.6, `へんかんこうほ` 1,113.3/1,108.1 and `さくらにゅうりょく` 191.0/191.4 (100 samples per image). The combined p50 was 1,051.0/1,051.8 µs. For `さくらにゅうりょく`, current-minus-baseline paired p50 was -32.8/+32.0 µs by baseline-first/current-first order, while same-image calibration was -34.5/+33.2 µs. This order effect means the measurements do not establish a causal speed change or rule out smaller effects.

Open/validate p50 was 56.3/58.5 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These local measurements include cache and ordering effects and do not establish causal effects on startup or interactive latency.

## Scope and architecture

The curated base-lemma data owns the meanings, the existing compiler owns propagation, and the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. The required scope was curated data, dictionary build and import contracts, benchmark, ranking tests and this record. Full binary comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks without inspecting unrelated modules, but individual meanings and ambiguity still require review. The 30% milestone is complete; 462,551 entries remain without details toward the longer-term 100% goal.
