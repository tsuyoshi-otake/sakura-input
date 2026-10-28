# Issue #412: continued basic-lemma descriptions towards 50%

## Acceptance and results

- Verify: compare candidate identities with Mozc, category, coverage and reviewed LLM releases and review their meanings. Expect: only previously undescribed basic forms. Result: 32 curated entries added.
- Verify: build twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and 661,428 entries. Result: passed; 269,339 descriptions (40.72%), up 519 from 268,820 (40.64%): 32 curated, 364 generated and 123 native inflection details. 50% requires 330,714 descriptions; 61,375 remain.
- Verify: compare every previous decoded detail, binary tables and 14 category files. Expect: previous descriptions and conversion data unchanged. Result: passed; all 268,820 old details and category files match; only DIDX, DREC, DREL, DTOF and DTXT differ. Initial SHA-256 `B685BCAC4B5D90B13EEF75DED84D99F1944533D0908DC3329A97A14FE7941C1C`; final `76BDA3728A213122E53E821A745E841BD48A9D6B808EDBB7FFDB6D6AA08A0AD1`.
- Verify: compare images and native `過ぎり` inheritance; run `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` ranking regressions on both. Expect: pass. Result: passed; the native form inherits the labelled meaning of `過ぎる` read `よぎる`. Ranking tests cover selected queries.
- Verify: alternate 100 conversion pairs per reading, plus identical-image calibration. Expect: report timing and ordering effects. Result: passed; see timing evidence.
- Verify: `cargo fmt --all -- --check`, `git diff --check` and `ci/check-process-clean.ps1`. Expect: pass and no surviving repository test process. Result: passed. Earlier work ran package tests and clippy; this batch changes curated data and a benchmark fixture.

## Timing evidence

Initial/final conversion p50 in microseconds: `きょう` 775.5/772.1, `にほんご` 1,634.8/1,638.4, `へんかんこうほ` 1,081.3/1,083.6 and `さくらにゅうりょく` 192.3/192.6. For the last reading, current-minus-initial paired p50 was -28.0/+27.8 µs by order; identical-image calibration was -44.6/+35.5 µs. Order effects prevent causal conclusions or ruling out smaller effects.

Open/validate p50 was 71.4/69.3 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. Cache and order effects limit interpretation; conversion covers four readings and first-candidate access rather than UI dispatch.

Curated data owns meanings; the existing compiler propagates details; candidate costs, ranking state and runtime contracts are unchanged. Scope included meaning/identity review, build/import reports, image comparison, ranking and timing checks. Whole-image comparison was needed because descriptions share the dictionary image. Future additions can reuse these checks; each meaning still needs review. The 50% goal remains open.
