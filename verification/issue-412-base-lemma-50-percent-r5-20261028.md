# Issue #412: additional basic-lemma descriptions towards 50%

## Acceptance and results

- Verify: match candidate identity against Mozc, category, coverage and reviewed LLM releases and review the meaning. Expect: only previously undescribed basic forms. Result: 39 entries added.
- Verify: build twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and unchanged 661,428 entries. Result: passed; 268,820 descriptions (40.64%), up 647 from 268,173 (40.55%): 39 curated, 414 generated inflections and 194 native inflections. 50% requires 330,714 descriptions; 61,894 remain.
- Verify: compare all decoded prior details, binary tables and 14 category files. Expect: existing descriptions and conversion data unchanged. Result: passed; 268,173 prior details and category files match; only DIDX, DREC, DREL, DTOF and DTXT changed. Initial SHA-256 `F39253C2740CD7AC92808202024C26F4392B43E9ACE401A64A2C360A3CB3ACF5`; final `B685BCAC4B5D90B13EEF75DED84D99F1944533D0908DC3329A97A14FE7941C1C`.
- Verify: compare images and `撰び` inheritance; run `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` ranking regressions against both. Expect: pass. Result: passed; the native form inherits the labelled meaning of `撰ぶ`. The ranking tests cover selected queries.
- Verify: alternate 100 conversion pairs for each reading and calibrate using identical images. Expect: report ordering effects. Result: passed; see timing evidence.
- Verify: `cargo fmt --all -- --check`, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass and no repository test process left. Result: passed; the interrupted test invocation was followed by a fresh complete run after process cleanliness was confirmed.

## Timing evidence

Initial/final conversion p50 in microseconds: `きょう` 770.0/758.4, `にほんご` 1,595.7/1,610.5, `へんかんこうほ` 1,151.7/1,134.1 and `さくらにゅうりょく` 201.5/204.0. For the last reading, current-minus-initial paired p50 was -32.8/+30.6 µs by order; identical-image calibration was -56.3/+48.6 µs. Order effects prevent causal conclusions and cannot rule out smaller effects.

Open/validate p50 was 67.8/69.8 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. Cache and order effects limit interpretation; conversion covers four readings and first-candidate access rather than the UI.

Curated data owns meanings; the existing compiler propagates details; candidate costs, ranking state and runtime contracts are unchanged. Scope included curated data, build/import reports, image comparison, regression and timing tests. Whole-image comparison was needed because descriptions share the dictionary image. Future additions can reuse these checks, but individual meanings still require review. The 50% goal remains open.
