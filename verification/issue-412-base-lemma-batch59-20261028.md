# Issue #412: base-lemma descriptions, batch 59 and 40% milestone

## Acceptance and results

- Verify: review selected meanings and match their seven-column dictionary identities, category and existing coverage. Expect: unambiguous descriptions and no overwritten details. Result: passed for 51 selected base entries.
- Verify: build twice with `scripts/build-dictionary.ps1`. Expect: reproducible images and 661,428 entries. Result: passed; 264,875 entries have descriptions (40.05%), exceeding the exact 40% threshold of 264,572. Batch 59 added 837 descriptions: 51 curated, 587 generated inflections and 199 native inflections.
- Verify: compare all compiled sections, every existing decoded detail and all 14 category files against both batch 58 and the start of this continuation (batch 50). Expect: only detail sections change and previous details remain. Result: passed; batch 50's 254,749 details and every category file are unchanged, and 10,126 descriptions were added. Only DIDX, DREC, DREL, DTOF and DTXT changed. Initial SHA-256 `7004C1597510F3AD6DDF5BC737AA1BBD35E2A52E67009AC9FC489B59D29D23F8`; final `67A6BDBD044DD266DB7EAC0F6439D0D9CAEB19558C37BCC40AA15A5A0BA0B401`.
- Verify: compare images and native `見付かり` inheritance, and run ranking regressions `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` against initial and final images. Expect: pass. Result: passed; `見付かり` inherits the labelled meaning of `見付かる`. Ranking tests sample selected queries rather than all interactions.
- Verify: run 100 alternating-order conversion pairs per reading and an identical-image calibration, after dictionary generation has finished. Expect: report measurements and ordering effects. Result: passed; see timing evidence.
- Verify: `cargo fmt --all -- --check`, `git diff --check` and `ci/check-process-clean.ps1`. Expect: pass and no repository test processes remain. Result: passed; an earlier process check found the still-running, intentionally concurrent dictionary build, and the check was repeated successfully after the build exited. Package tests and affected-package clippy were run earlier in the continuation.

## Timing evidence

Initial/final conversion p50 in microseconds: `きょう` 793.1/796.2, `にほんご` 1,666.6/1,661.8, `へんかんこうほ` 1,145.7/1,154.8 and `さくらにゅうりょく` 197.8/196.4. For the last reading, paired current-minus-initial p50 was -33.0/+33.1 µs by execution order; identical-image calibration was -68.7/+69.3 µs. The order effect prevents a causal conclusion or ruling out smaller effects.

Open/validate p50 was 68.8/76.3 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. Open/validate includes mapping, validation and OS cache effects, so this observation cannot isolate a startup regression. The conversion benchmark measures four readings and first-candidate access, not UI dispatch.

## Scope and architecture

Curated base-lemma data owns meanings; the existing compiler propagates them to inflected forms; the benchmark selects an added native form. Candidate costs, ranking state, conversion runtime and their contracts did not change. Scope included curated data, import/build contracts, benchmark and ranking tests. Whole-image comparison and calibrated timings were needed because descriptions share the compiled image. Future additions can reuse these checks without reading unrelated modules; review of each meaning and ambiguity remains necessary. The original 100% objective remains open: 396,553 entries lack descriptions.
