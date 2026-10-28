# Issue #412: progress towards 50% description coverage

## Acceptance and results

- Verify: compare candidate identity against Mozc, category, coverage and existing LLM releases; review each meaning. Expect: only unambiguous, previously undescribed basic forms. Result: 87 reviewed forms added.
- Verify: build twice using `scripts/build-dictionary.ps1`. Expect: reproducible image and unchanged entry count. Result: passed; 266,462 / 661,428 entries have descriptions (40.29%), up 1,587 from 264,875 / 661,428 (40.05%). The increase comprises 87 curated details, 1,211 generated inflections and 289 native inflections. 50% requires 330,714, leaving 64,252.
- Verify: decode every previous detail and compare binary tables and 14 category files. Expect: unchanged prior details and conversion data. Result: passed; all 264,875 prior details and all category files match, and only DIDX, DREC, DREL, DTOF and DTXT differ. Initial SHA-256 `67A6BDBD044DD266DB7EAC0F6439D0D9CAEB19558C37BCC40AA15A5A0BA0B401`; final `EB090878990D3D90ADE89A0DA2883AD9F78654BB4E02D5250B4DB2970F45D7C2`.
- Verify: run image comparison, native `鳴り止み` inheritance and ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against initial and final images. Expect: pass. Result: passed. The native form inherits the explicitly labelled meaning of `鳴り止む`; the ranking tests cover selected queries.
- Verify: run 100 alternating-order conversion pairs per reading and identical-image calibration after build completion. Expect: report measurements and ordering effects. Result: passed; see timing evidence.
- Verify: `cargo fmt --all -- --check`, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed. Earlier work ran package tests and clippy; this batch changes data and benchmark fixtures only.

## Timing evidence

Initial/final conversion p50 in microseconds: `きょう` 799.3/799.5, `にほんご` 1,667.7/1,692.5, `へんかんこうほ` 1,158.1/1,151.0 and `さくらにゅうりょく` 206.3/209.1. For the last reading, paired current-minus-initial p50 was -31.7/+31.6 µs by execution order; identical-image calibration was -54.2/+60.0 µs. The order effect prevents a causal conclusion or ruling out smaller effects.

Open/validate p50 was 75.3/75.9 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.2/0.1 µs in 101 samples. This includes cache and ordering effects; conversion measures four fixed readings and first-candidate access, not UI latency.

## Remaining scope

In the initial image, intersecting ENTR ordinals whose left ID is a Mozc autonomous verb or adjective basic form with DIDX shows 21,020 basic-form entries, of which 8,232 lack descriptions. The existing inflection report records 15,580 generated and 8,307 native forms withheld for missing lemma details. Adding all of these counts to the initial 264,875 descriptions, without subtracting overlap and assuming every item could safely be filled, gives an optimistic 296,994 (44.90%). Adding even all 2,689 generated and 18 native ambiguous forms and 31 changed native forms gives 299,732 (45.32%). This is an estimate for these existing categories, not a proven upper bound on new rules or other parts of speech; it does not justify automatically explaining ambiguous or changed entries. Reaching 50% needs additional safely identified entries or a broader, separately verified inheritance scope.

Curated base-lemma data owns meanings; the existing compiler owns propagation; ranking state and conversion contracts are unchanged. Scope included curated data, binary image and build reports, benchmark and ranking tests. The whole image and performance needed examination because descriptions share its binary. Future work can reuse these checks but still needs individual meaning review and safe identity matching.
