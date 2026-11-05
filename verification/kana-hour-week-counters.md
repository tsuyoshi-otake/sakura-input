# Spoken hour and week counters (#341, #372)

## Contract and scope

Recognize spoken hour/hour-duration (including half hours) and week/week-duration readings as a complete numeric span. Prefer exact dictionary number forms and inherit the dictionary's number spelling when constructing a new temporal form. Without lexical spelling evidence, keep the existing Arabic/full-width/kanji order. Global orthography policy (#342), minute/yen counters, personal learning, IT ranking, and persistence formats are outside this change.

The numeral parser owns unit recognition and consumed byte boundaries. Conversion synthesis owns generated forms and their evidence; the engine owns selected composition and commit. The calendar's existing relative-date recognition contract takes precedence over a new numeric interpretation of `せんしゅう`. No new dependency or state owner is required.

## Verification rubric

| Criterion | Verify | Expect |
|---|---|---|
| Reproduced failure | Wrapped core numeral tests and shipped-dictionary `kana_hour_and_week` regression before implementation | The hour and two-week cases fail on the original implementation |
| Parser boundaries | Wrapped core numeral unit suite | Complete spans, longest unit, valid weekly contractions; unrelated words preserved |
| Selected and committed text | Wrapped shipped-dictionary `kana_hour_and_week` tests using `SAKURA_SYSTEM_DIC` | Correct numeric candidate is selected and committed; both spelling forms remain available |
| Existing behavior | Core and engine tests, shipped numeric/lexical regressions | Calendar, bare numbers, typed digits and lexical homophones retain their contracts |
| Accuracy | Current `accuracy-gate` against the same shipped dictionary, new authored cases, reviewed snapshot then `--check` | Zero worsened rows; all intended changes accounted for |
| Integration | Formatting, Clippy, workspace tests and dependency/facade/IRV checks | All required gates pass |
| Cleanup | `ci/check-process-clean.ps1` and owned temporary-file cleanup | No build/test/evaluator survivors or discarded temporary artifacts |

## Results

The baseline is main `3aa36b991329d9a7e924950a5d356db3d4f28f35`, using the installed 2.0.27 dictionary with SHA-256 `7952ed2939d5562382f1e9b342348984ae5d695bccfb61c1e96ed08fc3f13154`. The baseline evaluator was rebuilt from those original production sources before comparing the new evaluator against the same bytes. The repository's historical dictionary build report has a different image hash; this comparison is bound to the installed release image, not that stale report.

- Fail before: both new parser tests failed; the dispatcher regression selected/committed `重一時`, `十二支`, `に習慣`, and `さん習慣`. The first complete accuracy run also exposed typed-digit continuation regressions (`12時版`, `1時間版`, `2週間語`) and displacement of `三時間ほど眠った`. These were repaired before accepting snapshots. The first workspace run then exposed removal of the generated relative-date alternatives for `せんしゅう`; protecting the calendar recognition contract made the complete 37-test `dictc-core` conversion suite pass again (one unrelated ignored test).
- Core suite: **324 passed**, 3 intentionally ignored. Cases cover longest units, conditioned weekly contractions, typed-digit boundaries, lexical homophones, exact numeric spelling inside sentences, lexical stem spelling, missing dictionary units, and relative-date words/alternatives.
- Shipped dictionary: **85 passed**, including numeric candidate availability, selected preedit and committed text. One historical migration test requires an additional pre-#334 dictionary (`SAKURA_BASE_SYSTEM_DIC`) and was excluded; it is not a counter regression. The other filtered test is not an ignored artifact test.
- Authored corpus: **1,587 conversions**, 1,425 lenient / 1,356 strict. Against main with the same expanded corpus: **8 rows improved, 0 worsened**. Of those, 2 are pre-existing sentences and 6 are new counter cases. All **1,605 prediction words are unchanged**. Thirteen public authored rows were added, including five independent lexical guards. No accepted-break waiver or lower floor was introduced. The accepted snapshot passes `accuracy-gate --check`.
- Additional scratch corpus: **243 conversions** (223 numeric expressions and 20 lexical/date guards), main **72/243** acceptable -> changed **193/243**, **121 improved, 0 worsened**; strict **59 -> 138**. All 20 lexical/date guards retain their original first candidate. This is a directed counter probe, not a population accuracy estimate. Fifty expressions do not lead with the expected numeric form, including ambiguous readings; this patch does not claim comprehensive temporal conversion.

The scratch probe crosses values 1, 2, 3, 5, 8, 10, 11, 12, 13, 20, 21, 22 with 時/時間/週/週間 and bare/から/まで/半/後 endings (後 omitted for clock hours). Weekly 1/8/10/20/21 use いっ/はっ/じゅっ/にじゅっ/にじゅういっ. Bare ambiguous forms are instead tested as lexical guards; the guards include 指示, 虹, 誤字, 惨事, 従事, 一周, 先日, 算術, 三条, 五条, 一緒, 発信, 実験, the five newly committed guard sentences, 先週 and 先週の会議.

An existing raw-key artifact test failed identically on main before the counter patch. Its assertions still expected the composed mixed reading at candidate 0, contradicting the explicit 2026-10-13 owner contract in `docs/decisions/input-behavior.md`. The test now checks physical ASCII keys first, the mixed reading second, and retains its repair-availability and deduplication assertions. No raw-key production behavior changed.

The complete CI-feature workspace suite passes: **2,678 passed, 0 failed, 188 ignored**, with 129 target summaries and a clean runner-process check. Formatting and all-target Clippy pass. An initial unbounded-parallel Clippy run exhausted compiler memory; the same command succeeded with `-j 2`. Subsequent Cargo jobs use two build workers. The dependency rules, facade audit, dependency policy/self-test and IRV comparison pass their enforced gates. IRV reports advisory growth against its historical baseline (including unchanged engine/history/renderer benchmarks); this patch does not claim an IRV reduction. The workspace release build passes; TSF DLL size is **452,608 bytes**, below the **1,048,576-byte** gate. The final evaluator passes `--check` again after the relative-date fix.

## Reproduction

Set `CARGO_HTTP_CHECK_REVOKE=false`, `CARGO_BUILD_JOBS=2` and `SAKURA_SYSTEM_DIC` to the built release dictionary. Route each Cargo test below through `ci/run-test-quiet.ps1` and finish with `ci/check-process-clean.ps1`:

```text
cargo test --locked -p sakura-core --lib -- --test-threads=4
cargo test --locked --release -p sakura-engine --features dev-fixtures --test shipped_dictionary_ranking -- --ignored --test-threads=1 --skip issue_334_learning_made_on_the_old_te_form_right_id_still_applies
cargo test --locked --workspace --no-fail-fast --features sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture,sakura-conversion-eval/learning-replay,sakura-conversion-eval/accuracy-gate -- --test-threads=4
accuracy-gate --dictionary <same-release-system.dic> --check
```

No native TSF/host typing test or installation is claimed by these in-process dispatcher checks. Learned preferences, reranking and personal context are outside the corpus measurement.

## Reading scope and architecture impact

A. Required scope: `numerals.rs`, conversion lattice numeric-node construction, `synthesis/numerals.rs`, their core tests, the shipped-dictionary dispatcher tests, the authored accuracy corpus and its loader/snapshot contract, and the existing raw-key owner decision.

B. Parser-only work was insufficient: full-sentence tests required preserving dictionary connection IDs and exact numeric spelling, the independent counter probe required lexical number spelling, and workspace tests required the relative-date recognition boundary. The broader artifact suite additionally exposed one stale test assertion, resolved against its explicit owner decision. Investigation was expanded for these observed failures; unrelated ranking and learning implementations were not refactored.

C. Number recognition and consumed-byte ownership stay in core. Numeric synthesis owns dictionary-derived spelling and connection metadata; the engine continues to own selected preedit and commit. No runtime state, dependency, wire format, persistence format, dictionary image, candidate budget or retry loop was added or moved.

D. This verification entry and named regression tests provide a direct entry point for future spoken-counter work. Future extensions must still establish pronunciation constraints, lexical ambiguity, connection IDs and whole-corpus effects. No measured reduction of physical or semantic reading scope is claimed.
