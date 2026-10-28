# Issue #412: base-lemma descriptions, batch 38

## Acceptance and results

- Verify: match all 553 new descriptions against source dictionary identities and exclude existing coverage and duplicate identities. Expect: every match succeeds. Result: passed; ambiguous or circular draft meanings were excluded after review.
- Verify: build baseline and final twice using the dictionary pipeline. Expect: reproducible images and unchanged 661,428 entries. Result: passed. Details increased from 152,585 to 163,209 (+10,624), or 24.68%; 35,220 more are needed to reach 198,429 (30%). The increase comprises 553 curated base descriptions, 6,561 generated and 3,510 native inflection descriptions.
- Verify: compare all binary sections and decode every existing detail by ordinal, annotation, description and relations; hash all 14 category TSVs. Expect: no old detail removed or modified, only detail sections change, categories unchanged. Result: passed. All 152,585 old details were preserved; changed sections were DIDX, DREC, DREL, DTOF and DTXT. Baseline SHA-256 `6B1212BB39F8048FF25C5782F91B651CF53D0ABE0971F86F21FCC66675C3C61F`; final `BD02022B3F0EE2406F0D6C94541447EF3BD4096210BB1641C701DB506C9AF66F`.
- Verify: compare images and native `そよい` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: candidate checks and tests pass. Result: passed; `そよい` inherits the labelled meaning of `そよぐ`. These cover selected queries, not exhaustive interactive IME behavior.
- Verify: measure 100 alternating-order conversion pairs per reading; repeat with identical baseline images. Expect: report per-reading and order effects and calibration. Result: passed; see below.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass with no remaining repository test process. Result: passed.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 751.3/754.3, `にほんご` 1,608.8/1,614.8, `へんかんこうほ` 1,068.9/1,069.3, and `さくらにゅうりょく` 189.0/189.2 (100 samples per image). Current-minus-baseline paired p50 by baseline-first/current-first order respectively was -10.4/+12.6, -11.7/+15.9, -12.8/+14.4, and -30.4/+28.2 µs (50 pairs per order). Same-image calibration for `さくらにゅうりょく` was -61.7/+58.8 µs, showing a substantial order effect without a dictionary change. These samples do not establish a speed change or rule out smaller effects.

Open/validate p50 was 56.1/56.3 ms in a non-paired 31-sample run, and exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These measurements do not establish causal effects on startup or interactive latency.

## Scope and architecture

The curated base-lemma data owns the new meanings and the existing compiler owns propagation; the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state, and their contracts did not change. Required scope was curated data, dictionary build and import contracts, benchmark, ranking tests, and this record. Full binary comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse those checks without inspecting unrelated modules, but individual meanings and ambiguity still need review. The 30% milestone remains incomplete.
