# Issue #412: base-lemma descriptions, batch 37

## Acceptance and results

- Verify: match all 397 new descriptions against source dictionary identities and exclude existing coverage and duplicate identities. Expect: every match succeeds. Result: passed; draft meanings were reviewed and ambiguous or circular descriptions excluded.
- Verify: run the dictionary pipeline twice each for baseline and final. Expect: reproducible images and unchanged 661,428 entry count. Result: passed. Details increased from 145,199 to 152,585 (+7,386), or 23.07%; 45,844 more are needed to reach 198,429 (30%). The increase comprises 397 curated base descriptions, 4,573 generated and 2,416 native inflection descriptions.
- Verify: compare all binary sections and decode every existing detail by ordinal, annotation, description and relations, and hash all 14 category TSVs. Expect: no old detail removed or modified, only detail sections change, categories unchanged. Result: passed. All 145,199 existing details were preserved; changed sections were DIDX, DREC, DREL, DTOF and DTXT. Baseline SHA-256 `A45C3DD3612FF61E2ADC8F722DBEC92E6C6F5B9262AEFCE8A4946F12FB3EE400`; final `6B1212BB39F8048FF25C5782F91B651CF53D0ABE0971F86F21FCC66675C3C61F`.
- Verify: compare images and native `泣き出し` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: candidate checks and tests pass. Result: passed; `泣き出し` inherits the labelled meaning of `泣き出す`. These are selected query tests, not exhaustive interactive IME testing.
- Verify: measure 100 alternating-order conversion pairs per reading; repeat with identical baseline images. Expect: report per-reading and order effects and calibration rather than infer causality from a pooled median. Result: passed; see below.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass with no remaining repository test process. Result: passed.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 775.3/769.4, `にほんご` 1,613.3/1,613.6, `へんかんこうほ` 1,142.6/1,144.4, and `さくらにゅうりょく` 193.4/182.9 (100 samples per image). Current-minus-baseline paired p50 by baseline-first/current-first order respectively was -12.5/+1.3, -20.7/+14.1, +9.8/-8.7, and -55.9/+52.6 µs (50 pairs per order). Same-image calibration for `さくらにゅうりょく` was -44.2/+57.7 µs, showing a substantial order effect without a dictionary change. The pooled median was 928.0/1,052.3 µs across the four dissimilar readings; it must not be interpreted as a per-query slowdown. The sample cannot rule out smaller effects.

Open/validate p50 was 53.4/54.6 ms in a non-paired 31-sample run, and exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These do not establish the change's causal effect on startup or interactive latency.

## Scope and architecture

The curated base-lemma data owns the new meanings and the existing compiler owns propagation; the benchmark selects a newly covered native form. Conversion runtime, candidate cost, ranking state, and their contracts did not change. Required scope was the curated data, dictionary build and import contracts, benchmark, ranking tests, and this record. The investigation included full binary comparison and order-calibrated timing because descriptions are compiled into a shared image. Future additions can reuse those checks without inspecting unrelated modules; individual meanings and ambiguity still require review. The 30% milestone remains incomplete.
