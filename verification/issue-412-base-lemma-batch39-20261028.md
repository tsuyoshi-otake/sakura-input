# Issue #412: base-lemma descriptions, batch 39

## Acceptance and results

- Verify: match all 660 new descriptions against source dictionary identities and exclude existing coverage and duplicate identities. Expect: all matches succeed. Result: passed; ambiguous and circular draft meanings were excluded after review.
- Verify: build baseline and final twice through the dictionary pipeline. Expect: reproducible images and unchanged 661,428 entries. Result: passed. Details increased from 163,209 to 175,279 (+12,070), or 26.50%; 23,150 more are needed to reach 198,429 (30%). The increase comprises 660 curated base descriptions, 7,038 generated and 4,372 native inflection descriptions.
- Verify: compare all binary sections and decode every existing detail by ordinal, annotation, description and relations; hash all 14 category TSVs. Expect: no old detail removed or modified, only detail sections change, categories unchanged. Result: passed. All 163,209 old details were preserved; changed sections were DIDX, DREC, DREL, DTOF and DTXT. Baseline SHA-256 `BD02022B3F0EE2406F0D6C94541447EF3BD4096210BB1641C701DB506C9AF66F`; final `14FC3790974130449D71667528D81C5010CD806962F085D29AE1215A6B849E50`.
- Verify: compare images and native `うずくまっ` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: candidate checks and tests pass. Result: passed; `うずくまっ` inherits the labelled meaning of `うずくまる`. These cover selected queries, not exhaustive interactive IME behavior.
- Verify: measure 100 alternating-order conversion pairs per reading; repeat with identical baseline images. Expect: report per-reading and order effects and calibration. Result: passed; see below.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass with no remaining repository test process. Result: passed.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 766.9/764.7, `にほんご` 1,614.1/1,612.7, `へんかんこうほ` 1,112.3/1,098.9, and `さくらにゅうりょく` 191.0/189.7 (100 samples per image). Current-minus-baseline paired p50 by baseline-first/current-first order respectively was -12.4/+8.6, -16.3/+13.2, -8.6/+1.9, and -31.6/+31.8 µs (50 pairs per order). Same-image calibration for `さくらにゅうりょく` was -63.7/+61.8 µs, showing a substantial order effect even with identical bytes. These samples do not establish a speed change or rule out smaller effects.

Open/validate p50 was 55.8/56.3 ms in a non-paired 31-sample run, and exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These measurements do not establish causal effects on startup or interactive latency.

## Scope and architecture

The curated base-lemma data owns the new meanings and the existing compiler owns propagation; the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state, and their contracts did not change. Required scope was curated data, dictionary build and import contracts, benchmark, ranking tests, and this record. Full binary comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse those checks without inspecting unrelated modules, but individual meanings and ambiguity still need review. The 30% milestone remains incomplete.
