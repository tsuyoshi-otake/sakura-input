# Issue #412: further basic-lemma descriptions towards 50%

## Acceptance and results

- Verify: compare chosen entries against Mozc, category identity, coverage and reviewed LLM releases and review meanings. Expect: only previously undescribed basic forms with a supported meaning. Result: 47 curated entries added.
- Verify: build twice with `scripts/build-dictionary.ps1`. Expect: identical images and unchanged 661,428 entries. Result: passed; 268,173 entries have descriptions (40.55%), up 809 from 267,364 (40.42%). Added details comprise 47 curated, 548 generated inflections and 214 native inflections. The 50% threshold is 330,714; 62,541 more descriptions are required.
- Verify: compare every decoded prior detail, binary sections and 14 category files. Expect: preserve previous descriptions and conversion data. Result: passed; 267,364 prior details and all category files match; only DIDX, DREC, DREL, DTOF and DTXT differ. Initial SHA-256 `93B4DFE1F4A6B4C8BB4D120737E708483B83CF3DFC5689CF454C78C8E912611C`; final `F39253C2740CD7AC92808202024C26F4392B43E9ACE401A64A2C360A3CB3ACF5`.
- Verify: compare images, check native `根付き` inheritance and run `issue_94_`, `issue_348_`, `issue_390_`, `issue_392_` ranking regressions against both images. Expect: pass. Result: passed; `根付き` explicitly inherits `根付く`'s meaning. Ranking checks cover selected queries.
- Verify: 100 alternating-order conversion pairs per reading and identical-image calibration after builds exit. Expect: report timing and ordering effects. Result: passed; see timing evidence.
- Verify: `cargo fmt --all -- --check`, `git diff --check` and `ci/check-process-clean.ps1`. Expect: pass and no surviving repository test process. Result: passed. Earlier work ran package tests and clippy; this batch changes curated data and a benchmark fixture.

## Timing evidence

Initial/final conversion p50 in microseconds: `きょう` 792.6/793.6, `にほんご` 1,661.6/1,660.6, `へんかんこうほ` 1,126.9/1,120.9 and `さくらにゅうりょく` 196.2/196.3. For the last reading, paired current-minus-initial p50 was -29.4/+32.0 µs by order; identical-image calibration was -50.5/+43.0 µs. The order effect prevents a causal conclusion or ruling out smaller changes.

Open/validate p50 was 68.4/73.2 ms in a non-paired 31-sample run; exact detail lookup p50 was 0.1/0.1 µs in 101 samples. Mapping and OS cache effects prevent attribution of the load difference. Conversion covers four fixed readings and first-candidate access, not UI dispatch.

## Additional source investigation

[EDRDG JMdict](https://www.edrdg.org/jmdict/j_jmdict.html) supplies readings, written forms, part-of-speech information and English glosses; it is not itself a source of Japanese definitions. The [EDRDG licence](https://www.edrdg.org/edrdg/newlic.html) includes attribution, share-alike and update conditions that must be assessed before introducing derivative data. No JMdict data or translations were imported.

For an exploratory upper bound, we compared the baseline's 8,094 undescribed Mozc verb/adjective base identities against a third-party archive dated 2025-10-11, accounting for reading restrictions, sense restrictions and inherited part of speech. The official server's certificate did not match its hostname, so its bytes could not be verified against the archive; the archive SHA-256 was `80EA9C34C4005391A2D3ECBE65583D28B8F3EC6FB486EE0F7842E83AA7CA5422`. The broad match found 785 identities: 283 with one listed sense, 502 with multiple senses, including 216 matched to multiple JMdict entries. These groups overlap. The comparison used broad part-of-speech matching and treats a reading as a possible kana surface even when kanji are listed; it is not proof that any pair, English gloss or Japanese translation is suitable for import. Individual semantic review, exact final-image identity checks, source provenance, licence handling and reproducible source pinning remain necessary. The source is insufficient on its own to fill the remaining 62,541 descriptions.

Curated data owns meanings and the existing compiler owns inheritance; candidate costs, ranking state and runtime contracts are unchanged. Scope covered entry review, import/build reports, whole-image comparison, ranking and paired timings, and source licensing. Whole-image and timing checks were needed because descriptions share the dictionary image. Future work can reuse those checks but needs semantic review and source verification. The 50% goal remains open.
