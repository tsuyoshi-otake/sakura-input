## Overflow-hazard test construction (dictionaries and prediction)

- **`dictc_core::parse_entries` rejects any single `reading`/`surface` field over
  `MAX_PREEDIT_BYTES` (1536 bytes) at compile time.** A dictionary TSV cannot
  contain an oversized field to use as an overflow-test fixture — `dictc`
  itself refuses to compile it. To construct a *runtime* overflow with a
  *compile-valid* dictionary, attach a custom `AppProfile` with
  `WidthPolicy { alnum: Width::Full, .. }` and use an ASCII surface: each
  ASCII byte widens to a 3-byte fullwidth character during
  `Normalizer::normalize_into`, so e.g. 600 dictc-legal ASCII bytes become
  1800 bytes at render/commit time, well past the 1536-byte scratch buffer.
  Confirmed working for `oversized_render_segment_dispatcher`
  (`crates/sakura-engine/src/dispatch.rs`).

- **`Converter::search_n_best` (`crates/sakura-core/src/conversion.rs`) used
  to let one oversized N-best candidate's `ConversionError::OutputTooLong`
  abort the whole search via `?`, discarding every candidate already found —
  including the guaranteed-good cheapest one from `build_viterbi_candidate` —
  and silently degrading the entire conversion to raw/unconverted display.**
  Fixed by catching `Err(ConversionError::OutputTooLong)` specifically inside
  the search loop and `continue`-ing instead of propagating it; every other
  error variant still propagates via `return Err(error)`. This was found
  purely as a side effect of writing an unrelated dispatch.rs regression
  test — a symptom worth remembering: "conversion silently fell back to the
  raw reading" is a search_n_best-abort symptom, not just a lattice/dictionary
  problem.

- **A `PredictionCandidate` surface is typed `FixedStr<MAX_PREDICTION_SURFACE_BYTES>`
  (512 bytes) regardless of source (system dictionary, user dictionary, or
  learned history) — `crates/sakura-engine/src/prediction.rs`.** An entry
  whose surface exceeds 512 bytes is not truncated, it is silently dropped:
  `system_candidate`/`user_candidate`/the history callback all build the
  candidate with `.push_str(...).ok()?`, so a `None` return removes the
  candidate from the ranked list with no error anywhere. Symptom: a
  dictionary entry with `flags=predict` never appears as a Tab suggestion —
  `State::Predicting` is never reached — even though the same dictionary
  entry converts fine through ordinary (non-prediction) conversion. Check
  `MAX_PREDICTION_SURFACE_BYTES` before assuming a Tab-suggestion bug is a
  reading-length or indexing bug.

- **`MAX_PREDICTION_SURFACE_BYTES` (512) × the widest possible
  `normalize_into` expansion ratio (3, ASCII→fullwidth) exactly equals
  `MAX_PREEDIT_BYTES` (1536).** This means `commit_suggestion_at`'s
  `normalizer.normalize_into(candidate.surface(), ...)` call can **never**
  actually overflow for any real (system/user/history) prediction candidate:
  the maximal legitimate surface always lands exactly on the 1536-byte
  boundary, and `FixedStr::push_str` accepts a write landing exactly on
  capacity (`new_len > N` is the rejection condition, not `>=`). Do not write
  a black-box regression test asserting `ErrorCode::TooLarge` from this path
  — it is unreachable given today's constants, not merely hard to trigger.
  Instead assert the boundary succeeds exactly at 1536 bytes (see
  `a_maximal_suggestion_commit_fits_exactly_at_the_preedit_boundary` in
  `crates/sakura-engine/src/dispatch.rs`), so a future change narrowing
  either constant (or widening the expansion ratio) fails loudly here instead
  of silently reopening the corruption `commit_suggestion_at`'s
  stage-before-mutate ordering was written to prevent.

- **Any regex in the packaging scripts that anchors on a line of a tracked
  text file must allow the carriage return (`\r?$`, or `\s*$`).** The
  repository stores LF; this machine and the GitHub Actions Windows runners
  both check out with `core.autocrlf=true`, so the working tree is CRLF and
  .NET's multiline `$` matches before the `\n`, leaving the `\r` unmatched.
  Verified 2026-08-14 (#50): `scripts/build-installer.ps1` and
  `.github/workflows/release.yml` each refused a correct tree this way. The
  trap is that the same file builds fine until a `git checkout` touches it —
  a working copy written by an editor keeps LF and hides the bug, so "it
  worked last release" is not evidence. `crates/sakura-regtool/tests/
  packaging_version.rs::every_packaging_version_gate_allows_a_carriage_return`
  guards the two known gates; extend its table when adding a new one.

- **Do not trust MSYS/Git Bash `cat -A`, `sed`, or `hexdump` to tell you a
  file's line endings.** Those tools opened `installer/setup.iss` in text mode
  and showed `22 0a` (LF) for a line that .NET read as `22 0d` (CRLF).
  Verified 2026-08-14 (#50) — the LF reading sent the investigation the wrong
  way for several minutes. Read the bytes through the runtime that actually
  consumes the file: `[IO.File]::ReadAllText` in PowerShell for the packaging
  scripts, `std::fs::read_to_string` in Rust for the tests.
