# Issue #412: base-lemma descriptions, batch 40

## Acceptance and results

- Verify: match all 578 new descriptions against source dictionary identities and exclude existing coverage and duplicate identities. Expect: all matches succeed. Result: passed; ambiguous and circular draft meanings were excluded after review. A malformed agent draft was corrected to eight TSV columns before the identity check.
- Verify: build baseline and final twice through the dictionary pipeline. Expect: reproducible images and unchanged 661,428 entries. Result: passed. Details increased from 175,279 to 186,220 (+10,941), or 28.15%; 12,209 more are needed to reach 198,429 (30%). The increase comprises 578 curated base descriptions, 6,705 generated and 3,658 native inflection descriptions.
- Verify: compare all binary sections and decode every existing detail by ordinal, annotation, description and relations; hash all 14 category TSVs. Expect: no old detail removed or modified, only detail sections change, categories unchanged. Result: passed. All 175,279 old details were preserved; changed sections were DIDX, DREC, DREL, DTOF and DTXT. Baseline SHA-256 `14FC3790974130449D71667528D81C5010CD806962F085D29AE1215A6B849E50`; final `70BBE8D5DFFAAF65D25AF75BB305001BA7FC59AB295C7F5F7EF26092BA716FE3`.
- Verify: compare images and native `およい` inheritance; run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: candidate checks and tests pass. Result: passed; `およい` inherits the labelled meaning of `およぐ`. These cover selected queries, not exhaustive interactive IME behavior.
- Verify: measure 100 alternating-order conversion pairs per reading; repeat with identical baseline images. Expect: report per-reading and order effects and calibration. Result: passed; see below.
- Verify: dictionary package tests, affected-package clippy with warnings denied, format check, inflection report tests, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass with no remaining repository test process. Result: passed.

## Timing evidence

Per-reading baseline/final conversion p50 in microseconds was `きょう` 765.0/766.2, `にほんご` 1,627.5/1,616.2, `へんかんこうほ` 1,092.0/1,070.0, and `さくらにゅうりょく` 190.0/190.2 (100 samples per image). Current-minus-baseline paired p50 by baseline-first/current-first order respectively was -11.6/+10.6, -15.3/+10.6, -41.2/-3.4, and -29.7/+26.6 µs (50 pairs per order). Same-image calibration for `さくらにゅうりょく` was -40.3/+38.7 µs, showing an order effect even with identical bytes. These samples do not establish a speed change or rule out smaller effects.

Open/validate p50 was 57.7/59.8 ms in a non-paired 31-sample run, and exact detail lookup p50 was 0.1/0.1 µs in 101 samples. These measurements do not establish causal effects on startup or interactive latency.

## Scope and architecture

The curated base-lemma data owns the new meanings and the existing compiler owns propagation; the benchmark selects a newly covered native form. Conversion runtime, candidate costs, ranking state, and their contracts did not change. Required scope was curated data, dictionary build and import contracts, benchmark, ranking tests, and this record. Full binary comparison and order-calibrated timing were needed because descriptions share the compiled image. Future additions can reuse those checks without inspecting unrelated modules, but individual meanings and ambiguity still need review. The 30% milestone remains incomplete.
