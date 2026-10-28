# Issue #412: base-lemma descriptions, batch 36

## Acceptance and verification

- Verify: match every one of 102 new curated entries against all seven source identity fields and require no existing coverage or curated duplicate. Expect: all match, no duplicates. Result: passed; ambiguous and circular draft descriptions were excluded.
- Verify: run `scripts/build-dictionary.ps1` twice as part of its reproducibility check. Expect: identical outputs, 661,428 entries. Result: passed. Details rose from 143,145 to 145,199 (+2,054): 21.95% coverage, with 53,230 entries remaining to 30% (198,429). The curated source contributed 102; generated inflection details increased by 1,219 and native inflection details by 733.
- Verify: decode and compare every existing detail by entry ordinal, including annotation, description and relations; compare all other binary sections and 14 category TSV hashes. Expect: no removals or modifications, changes confined to detail sections, identical category files. Result: passed: 143,145 preserved, 2,054 added; only DIDX, DREC, DREL, DTOF and DTXT changed. Baseline SHA-256 `67F805942D4F50F46CE1C4CD73DFBA3D6A58AB47B3BC921ACCDEAA8F02A58E22`; final `A45C3DD3612FF61E2ADC8F722DBEC92E6C6F5B9262AEFCE8A4946F12FB3EE400`.
- Verify: `dictionary_image_bench` compares the two images, native `ちっ` detail, candidate counts and leading candidate; run shipped ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against both images. Expect: comparisons and all tests pass. Result: passed. This is evidence for the tested queries; it is not an exhaustive interactive IME test.
- Verify: paired release-mode conversion benchmark, 100 pairs per reading with alternated order; repeat against the same baseline image to expose measurement noise. Expect: report distributions and order effects rather than infer causality from pooled medians. Result: passed; details below.
- Verify: dictionary package tests, affected-package clippy with warnings denied, `cargo fmt --all -- --check`, inflection-report validation, `git diff --check`, and `ci/check-process-clean.ps1`. Expect: pass and no repository test process remains. Result: passed.

## Performance observations

Four-readings pooled conversion p50 baseline/final was 1,180.9/1,182.3 µs (400 pairs); this pooled figure combines different workloads. Per-reading p50 baseline/final in µs: `きょう` 861.1/879.2, `にほんご` 1,881.2/1,833.3, `へんかんこうほ` 1,262.1/1,270.6, `さくらにゅうりょく` 243.2/237.6. Each is 100 samples per image. Current-minus-baseline paired p50 by baseline-first/current-first order respectively was -4.3/+10.2, -12.8/+8.1, +4.8/-36.7, and -57.5/+60.1 µs (50 pairs in each order).

The same-image calibration produced -67.8/+67.0 µs for `さくらにゅうりょく` by order, showing a substantial order effect even with identical bytes. These measurements do not establish a speed change or rule out a smaller one. Exact detail lookup p50 was 0.1 µs for both images in this run. Open/validate p50 was 58.1/69.3 ms in a separate non-paired 31-sample measurement; its measurement order and noise prevent attributing that difference to the change. Broader controlled startup and interactive measurements remain a limitation.

## Scope and architecture

The curated base-lemma TSV owns the added descriptions; the existing inflection import owns propagation. The benchmark owns reporting by reading and order; conversion and ranking runtime code was unchanged. Investigation expanded to benchmark aggregation and same-image calibration because the previous pooled measurements did not isolate order effects. Future similar work can use the per-reading paired outputs and binary comparison without inspecting unrelated components; it must still account for order effects and benchmark noise. No production boundary or state ownership changed.
