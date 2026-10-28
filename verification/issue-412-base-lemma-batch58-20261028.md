# Issue #412: base-lemma descriptions, batch 58

## Acceptance and results

- Verify: review each selected meaning and match its seven-column identity against the dictionary, category and existing coverage. Expect: unambiguous descriptions with no existing detail overwritten. Result: passed.
- Verify: run `scripts/build-dictionary.ps1` twice. Expect: reproducible output and unchanged entry count. Result: passed; 661,428 entries and 264,038 details (39.92%), an increase of 1,409: 84 curated, 986 generated inflection and 339 native inflection descriptions. The 40% threshold is 264,572.
- Verify: compare compiled sections, every existing decoded detail and all 14 category files. Expect: only detail sections change. Result: passed; all 262,629 previous details and category files are unchanged. Only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `1783D8983C0DD0F1C816B554F96A0F82C1BD030863EF69E9586702FFCDCCC0CB`; final `9AFC842824EC62E1C606A78E603C471F536971D4E9F815FD4B1CAE754747CDD4`.
- Verify: compare images and native `貶し` inheritance, and run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: pass. Result: passed; `貶し` inherits the labelled meaning of `貶す`. Ranking checks sample selected queries.
- Verify: run 100 alternating-order conversion pairs per reading and repeat against identical baseline images. Expect: report measurements and ordering effects. Result: passed; see timing evidence.
- Verify: `ci/check-process-clean.ps1`. Expect: no repository test processes remain. Result: passed. Package tests and static analysis were run before this data-only batch.

## Timing evidence

Baseline/final conversion p50 in microseconds: `きょう` 762.0/764.6, `にほんご` 1,606.2/1,606.5, `へんかんこうほ` 1,186.5/1,177.0 and `さくらにゅうりょく` 227.8/228.9. For the last reading, current-minus-baseline paired p50 was -58.5/+56.8 µs by execution order, while identical-image calibration was -63.2/+62.6 µs. The order effect prevents a causal conclusion or ruling out smaller effects.

Open/validate p50 was 66.7/66.5 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. Cache and order effects limit conclusions about startup and interactive latency.

## Scope and architecture

Curated base-lemma data owns meanings; the existing compiler propagates descriptions to inflections; the benchmark chooses a newly covered native form. Conversion runtime, candidate costs, ranking state and their contracts did not change. Scope included curated data, build/import contracts, benchmark and ranking tests. Whole-image comparison and calibrated timing were needed because descriptions share the compiled image. Future additions can reuse these checks; individual meanings and ambiguity still require review. The 40% milestone remains 534 entries away.
