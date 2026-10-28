# Issue #412: base lemma detail batch 35

## Scope and acceptance

Add reviewed meanings for 491 previously unexplained base forms and inherit them to eligible inflections under the existing matching rules. Preserve prior details, candidates, costs and ranking.

- Verify: build both dictionary images twice. Expect: reproducible images and increased details. Result: passed; baseline SHA-256 `190B1DC8466B3F6C9A818D19B6763F4911EFBC2FFDED39BB1C9139FD7E91BB9E` matches batch 34, and final SHA-256 `67F805942D4F50F46CE1C4CD73DFBA3D6A58AB47B3BC921ACCDEAA8F02A58E22` matched across both final passes.
- Verify: count compiled details. Expect: more descriptions without additional candidates. Result: 661,428 entries and 143,145 details, up from 133,871 (+9,274): 491 curated, 5,056 generated-inflection and 3,727 native-inflection details. Coverage is 21.64%; 30% requires 198,429 records, leaving 55,284. The overall objective remains 100%.
- Verify: compare all binary sections and every previous detail and relation by ordinal, plus all 14 category files. Expect: only detail sections change, old details and categories remain identical. Result: passed; only DIDX, DREC, DREL, DTOF and DTXT changed, with all 133,871 previous details preserved.
- Verify: compare the images and inspect a newly described native inflection. Expect: `瞬け` inherits the labelled meaning of `瞬く` read `しばたたく`. Result: passed.
- Verify: run dictionary package tests, affected-package clippy, formatting and inflection report validation; run issue 94, 348, 390 and 392 ranking tests against both images. Expect: all pass and repository test processes exit. Result: passed; final process check clean.
- Verify: measure 400 paired alternating conversions over four readings and compare candidate count and leader; separately measure image opening and detail lookup. Expect: report values and limitations. Result: candidate-count and leader checks passed in all three paired runs. Baseline/final p50 was 1.046/1.071 ms, 0.947/1.038 ms, and 1.043/1.042 ms; p99 was 1.711/1.750 ms, 1.720/1.720 ms, and 1.750/1.845 ms. The second run's median difference was 9.6%, while the third was nearly unchanged; uncontrolled load and the four-query median distribution limit causal interpretation. Image open/validate p50 was 52.818/54.301 ms (31 samples), and exact detail lookup/preview p50 0.1/0.1 microseconds (101 samples). These samples do not establish a global latency bound.

## Ownership, scope and residual risk

Curated base-lemma data owns meanings; existing compiler inheritance owns matching and preservation. No new runtime state, ranking contract or matching rule was introduced. The scope was the curated data, dictionary image benchmark, compiler and engine test contracts, and this verification record. Investigation expanded to repeated paired performance measurements because median timings differed; no cause was established. Future additions can use the same data-only path but still require individual meaning and collision review, plus full-image comparison because detail IDs are regenerated. The 30% milestone remains incomplete.
