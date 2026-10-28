# Issue #412: next basic-lemma descriptions towards 50%

## Acceptance and results

- Verify: compare each chosen entry against Mozc, category identity, coverage and reviewed LLM releases and review its meaning. Expect: only previously undescribed basic forms with a supported meaning. Result: 50 curated entries added.
- Verify: build twice using `scripts/build-dictionary.ps1`. Expect: identical images and unchanged 661,428 entries. Result: passed; 267,364 entries have descriptions (40.42%), up 902 from 266,462 (40.29%). Added details comprise 50 curated, 631 generated inflections and 221 native inflections. The 50% threshold is 330,714; 63,350 more descriptions are required.
- Verify: decode previous details and compare binary sections and all 14 category files. Expect: preserve all existing details and conversion data. Result: passed; all 266,462 details and all category files match; only DIDX, DREC, DREL, DTOF and DTXT differ. Initial SHA-256 `EB090878990D3D90ADE89A0DA2883AD9F78654BB4E02D5250B4DB2970F45D7C2`; final `93B4DFE1F4A6B4C8BB4D120737E708483B83CF3DFC5689CF454C78C8E912611C`.
- Verify: compare images, check native `膿み` inheritance and run `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` ranking regressions against both images. Expect: pass. Result: passed; `膿み` explicitly inherits `膿む`'s meaning. Ranking checks are selected queries, not exhaustive interactive coverage.
- Verify: 100 alternating-order conversion pairs per reading and an identical-image calibration after builds exit. Expect: report measurement and ordering effects. Result: passed; see timing evidence.
- Verify: `cargo fmt --all -- --check`, `git diff --check` and `ci/check-process-clean.ps1`. Expect: pass and no surviving repository test process. Result: passed. Earlier work ran package tests and clippy; this batch changes curated data and a benchmark fixture.

## Timing evidence

Initial/final conversion p50 in microseconds: `きょう` 834.7/837.4, `にほんご` 1,784.3/1,785.3, `へんかんこうほ` 1,200.6/1,202.9 and `さくらにゅうりょく` 218.3/217.5. For the last reading, paired current-minus-initial p50 was -51.4/+39.4 µs by execution order; identical-image calibration was -65.8/+56.3 µs. Order effects prevent a causal conclusion or ruling out smaller changes.

Open/validate p50 was 77.4/84.8 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. Mapping and OS cache effects prevent attribution of the load difference. Conversion covers four fixed readings and first-candidate access, not UI dispatch.

## Additional investigation and limits

We inspected existing native adjective entries with matching left/right IDs, no existing detail, and a unique described basic form found by matching reading and surface suffixes. There were 791 mechanically plausible `文語基本形` pairs and two `口語基本形` pairs. These are not verified semantic equivalences. In particular, the two colloquial entries `こええ` and `こえー` mechanically matched `こい`, even though that spelling can mean a different word. Neither colloquial pair nor any of the 791 literary pairs was backed by the same unique adjectival synset in the pinned Japanese WordNet. Therefore no automatic propagation rule was added. Word-by-word validation or a stronger source is needed; the count is not an estimate of safely usable entries.

Curated data owns basic meanings and the existing compiler owns inheritance. Candidate costs, ranking state and runtime contracts remain unchanged. Scope included identity/meaning review, build/import reports, whole-image comparison and representative ranking and timing checks. Whole-image and timing checks were needed because descriptions share the dictionary image. Future additions can reuse these checks; they must still establish semantic equivalence for each form. The broader 50% goal remains open.
