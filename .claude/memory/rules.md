# Verified rules for this repository

Only things that were actually observed here go in this file. Each rule says
what was measured, so a later reader can tell a law from a guess. When a rule
turns out to be wrong, delete it — a stale rule is worse than no rule.

## Performance

- **Do not ship a performance claim a benchmark has not made.** The SIMD run
  scanner in `sakura-core::width` was written, reviewed and committed with a
  confident doc table before anything measured it. The benchmark, written
  afterwards, showed it made Japanese prose — the single most common input
  this IME will ever see — **34 % slower**. Write the benchmark first, or at
  minimum before the doc comment.

- **`&src[at..]` re-validates a UTF-8 character boundary on every index.** On
  text where most characters take the slow branch (i.e. all Japanese), that
  check costs more than a vectorized run scan saves. Walk a `Chars` iterator
  and take `chars.as_str()` for the remainder instead. This one change moved
  Japanese prose from 0.66× to 0.93× against the scalar loop.

- **Below one vector block there is nothing to amortize a scanner's setup
  over.** `normalize_into` short-circuits `src.len() < MIN_VECTOR_BYTES` to a
  plain per-character loop in a body small enough to inline, so a single
  keystroke — the overwhelmingly common call — never pays for a function call
  into the run scanner.

- **Microbenchmarks here report the best of N runs, never the mean.**
  Everything that makes a run slower (preemption, a frequency dip, a cache
  eviction) is additive noise on top of a fixed cost, so the minimum is the
  estimator that converges. Measured with the mean, one unchanged loop
  reported 53 ns and then 100 ns on consecutive runs; best-of-7 is stable to
  within a few percent. See `crates/sakura-core/tests/width_bench.rs`.

## CI and verification

- **A workflow that never runs is indistinguishable from a workflow that
  passes.** `ci.yml` and `installer.yml` both triggered on `push: branches:
  [master]` while this repository's only branch is `main`. `gh run list`
  returned nothing: neither had run on any commit, and several "CI is green"
  assumptions were assumptions about a workflow that was not executing. Check
  `gh run list` after adding or editing a workflow, not just the YAML.

- **A processor's model name is not evidence about the ISA available to your
  process. Only a feature probe is.** This was got wrong twice in one hour,
  each time by reasoning from the name printed by `Get-CimInstance
  Win32_Processor`:

  | run | reported processor | inferred | **measured** |
  |---|---|---|---|
  | `6848ac5` | AMD EPYC 7763 (Zen 3) | no AVX-512 | not printed |
  | `e829ff9` | AMD EPYC 9V74 (Zen 4) | AVX-512 | not printed |
  | `577a550` | Intel Xeon Platinum 8573C | AVX-512 | **`tier avx512bw`** |
  | `6905660` | AMD EPYC 9V74 (Zen 4) | AVX-512 | **`tier avx2`** |
  | `173c216` | AMD EPYC 7763 (Zen 3) | no AVX-512 | **`tier avx2`** |

  The fourth row is the point. Zen 4 has AVX-512 in silicon, and that runner
  still reports `avx2` — the hypervisor does not expose it to the guest.
  `is_x86_feature_detected!` knows that; a datasheet does not. The fifth row
  is the same lesson from the other side: an inference that happened to be
  right is still not a measurement until the log prints one.

  Of five runs, three printed a kernel list and **one** covered AVX-512. Treat
  CI coverage of that kernel as occasional, never as given.

- **`windows-latest` is not one machine, so a green CI run's differential
  SIMD coverage is not a fixed quantity.** Four runs of one workflow inside
  an hour drew three processors and two different ISA tiers. Since the
  `simd::` tests only exercise kernels the host supports, and `cargo test`
  captures stdout, the first two runs covered AVX-512 or did not with nothing
  readable afterwards to tell the two apart — worse than a known gap, and the
  same failure as a workflow that never runs.

  Fixed by making each run state its own scope: a CI step re-runs `simd::`
  with `--nocapture` so the log prints `kernels under test: [...] (tier ...)`.
  It paid for itself immediately by producing the fourth row above and
  refuting the inference in the second. **Quote that line, never the CPU
  name, when claiming a kernel was covered.**

  **AVX-512 verification is local, by the owner's decision (2026-07-31)**, and
  CI is not to be extended to *require* it — the step above only reports what
  happened to be covered. The standing obligation is therefore to run
  `cargo test -p sakura-core --lib -- simd:: --nocapture` on this machine
  before releasing anything that touches the kernels. Since production now
  keeps AVX-512 bench-only, confirm the printed `kernels under test` includes
  the scalar, AVX/SSSE3, AVX2, and all three AVX-512BW+VL threshold variants;
  `resolved width scan avx2-hybrid` is the intended shipping selection, not a
  coverage failure. Verified here on 2026-08-22.

- **A `cargo test` filter that ends in `::` makes an unquoted YAML `run:`
  line unparseable, and GitHub reports it as anything but a syntax error.**
  `run: cargo test -p sakura-core --lib -- simd:: --nocapture` puts a colon
  immediately before a space, which is YAML's mapping separator: the file
  stops parsing at that line (`mapping values are not allowed here`, line 56
  column 54). What GitHub then showed was **the whole workflow having no
  triggers** — `gh run list` named the run `.github/workflows/ci.yml` instead
  of `CI`, it ran **zero jobs**, `gh run view --log-failed` said "log not
  found", and `gh workflow run ci.yml` refused with HTTP 422 *"Workflow does
  not have 'workflow_dispatch' trigger"* about a file that plainly has one.
  Recognise that signature: it means unparseable, not misconfigured.

  Quote any `run:` value containing `: `. And parse workflow files locally
  before pushing — `~/tmp/yamlvenv/Scripts/python.exe` has `pyyaml` for
  exactly this; a red run is a cheap way to find out, but a run that never
  starts teaches nothing on its own.

- **A sandbox test that does not prove it is sandboxed proves nothing.** A
  test that connects to the pipe "from an AppContainer" passes just as
  happily when the AppContainer was never applied. The child must assert
  `TokenIsAppContainer` on its own token before it does anything else.

- **A test that leaks a watchdog corrupts the *next* run, not its own.**
  `tests/watchdog_recovery.rs` kills the engine and waits for the renderer to
  restart it, with a no-renderer control phase to prove nothing ambient does
  the restarting. An early version leaked its renderer, and the following
  run saw an engine reappear 12.75 s after the kill — about one
  `WATCH_BUDGET` — with no renderer of its own started. The control's 5 s
  window missed it, so the test would have passed for entirely the wrong
  reason. Two fixes, both structural: refuse to start when **any** Sakura
  process is running (a renderer holds no pipe, so only the process list
  finds it), and tear down the watchdog *before* the thing it watches, or it
  dutifully restarts what the teardown just stopped.

- **Verify a test can fail before believing it passed.** Commenting out the
  renderer spawn made `watchdog_recovery` fail after 30 s with the intended
  message. Without that run, "it passed" would have been indistinguishable
  from "it cannot fail".

## Windows specifics

- **Real-process tests must isolate `LOCALAPPDATA` unless they explicitly test
  the installed user profile.** The engine's learning, configuration, user
  dictionary, and diagnostics all derive from that root. A candidate UIA test
  once restored a previously learned candidate at index 11 and opened on page 2
  before the test sent PageDown. A unique per-run app-data directory both makes
  the test deterministic and prevents verification from mutating user state.

- **Auxiliary indexes over a mapped dictionary should retain image offsets, not
  copied records.** Copying every 24-byte entry into the prediction index pushed
  private working set over the 15 MiB release gate. A four-byte entry index lets
  the hot path materialize the validated record from the read-only mapping only
  when ranking or rendering it; the compact index passed the footprint gate
  while keeping end-to-end prediction p99 below 0.3 ms.

- **A server-side UI Automation raw provider needs COM initialized on the
  renderer UI thread before the provider/window is created.** The candidate
  window handled `WM_GETOBJECT` and called `UiaReturnRawElementProvider`, yet a
  separate real UIA client saw only the generic host-window provider (empty
  Name and no Sakura AutomationId). Adding an STA guard before window creation
  made the custom `IRawElementProviderSimple` discoverable; the real-process
  `candidate_uia` test now proves Name, AutomationId, control type, bounding
  rectangle, paging updates, and hidden/off-screen state.

- **`FILE_APPEND_DATA` and `FILE_CREATE_PIPE_INSTANCE` are the same bit
  (0x0004).** A named-pipe client that asks for `GENERIC_READ | GENERIC_WRITE`
  is therefore also asking for permission to create a pipe instance, which the
  server's DACL rightly refuses. Clients must request the exact
  `CLIENT_ACCESS` mask — see `sakura-ipc::security`.

- **Inno Setup has no native way to fail an uninstall when an
  `[UninstallRun]` entry exits nonzero.** A failing entry is a line in the log
  and file removal proceeds regardless — which is exactly how an IME leaves
  Windows pointing at a text service whose DLL is gone. `installer/setup.iss`
  runs `--unregister` from a `Check:` function that execs it, reads the real
  exit code and calls `Abort`.

- **`$args` is a PowerShell automatic variable.** Assigning to it inside a
  function shadows the unbound-argument array for the rest of that body. Name
  the local something else (`$installerArgs`).

- **`$PSCmdlet` resolves from the enclosing script scope inside plain
  (non-advanced) functions of an advanced script**, so `-WhatIf` propagates
  into helpers that never declared `[CmdletBinding()]`. Verified with a
  purpose-built probe script rather than assumed.

- **Opening the engine pipe is not a successful protocol handshake.** The
  renderer watchdog used to reset its reconnect delay as soon as `CreateFileW`
  succeeded. A reachable engine that rejected `Hello` therefore produced an
  immediate reconnect storm, even though the source comment claimed backoff
  would slow it. Only a valid `Hello` may reset the delay; protocol rejection
  retains exponential backoff up to the ceiling. The schedule is now a tested
  explicit terminal transition in `sakura-renderer::watch`.

- **Every child wait must consume the caller's remaining deadline, not a fresh
  per-operation timeout.** `sakura-regtool --stop` had an overall deadline but
  each pipe reconnect could independently wait the full two-second patient
  budget, allowing the loop to overshoot its advertised terminal. Each connect
  and sleep is now capped by the remaining duration, with the cap covered by a
  regression test.

- **After a synthetic `WM_DPICHANGED`, wait relative to the most recently
  observed rectangle.** The live candidate UIA test compared the post-DPI move
  against its original caret rectangle. Because an earlier placement had
  already changed that rectangle, the wait completed immediately on stale
  state and raced the new placement. Capturing the DPI rectangle first and
  requiring a subsequent change made the real HWND/UIA assertion deterministic.

- **A Cargo target directory is not the installed product layout.** The
  watchdog correctly starts its sibling engine, but that engine cannot discover
  `{app}\dict\system.dic` when both executables live under `target\...\debug`.
  Installed-layout supervisor tests must pass an explicit dictionary path to
  the supervisor so the restarted child inherits the same validated data root.

- **Diagnostic tier names must come from the same canonical vocabulary as CPU
  dispatch.** The core selected `avx512bw` while the engine event log shortened
  it to `avx512`, causing machine-readable Phase 1 evidence to reject a healthy
  startup. `CpuTier::name()` now emits the exact core tier and has a unit test.

- **Isolating `LOCALAPPDATA` also hides per-user developer tools.** A clean
  verification profile could not find Inno Setup even though it was installed
  under `%USERPROFILE%\AppData\Local\Programs`. Tool discovery used by isolated
  real-process tests must accept an explicit path and check that fixed per-user
  install location instead of treating the isolated application-data root as
  the developer's tool root.

## This machine

- **Every `cargo` invocation must be prefixed with
  `CARGO_HTTP_CHECK_REVOKE=false`**, or the fetch fails with
  `CRYPT_E_NO_REVOKE_CHECK (0x80092012)`.

- **Heredocs through the Bash tool fail here** (`unexpected EOF`, and
  `$TMPDIR` is unset so `/msg.txt` is a permission error). Write commit
  messages with the file-writing tool into the session scratchpad and use
  `git commit -F <path>`.

- **`.cargo/config.toml` pins `x86_64-pc-windows-msvc`**, so release artifacts
  live under `target/x86_64-pc-windows-msvc/release/`, not `target/release/`.
  Anything that hard-codes the old path (installer sources, size checks) is
  silently looking at a stale or absent file.

## TSF re-entrancy safety

- **Treat every separately re-entrant COM call as its own authority boundary.**
  A lease check around an aggregate operation is insufficient when that
  operation performs several host calls. Check authority before and after each
  call, and suppress all later calls as soon as lifecycle, focus, context, or
  operation ownership changes.

- **Install cleanup ownership before validating authority after a successful
  host Begin.** Re-entry can invalidate the caller while `BeginUIElement`
  succeeds. Record the exact manager, element, and id first, then evaluate the
  post-call lease; otherwise teardown loses the only matching `EndUIElement`.

- **A refused single-threaded state borrow needs an out-of-band terminal
  owner.** For delayed TSF callbacks, retain an exact operation token plus one
  bounded deferred or lifecycle-owned settlement. Never leave a state such as
  `QueryQueued` merely because a `RefCell` borrow or hidden-window post failed.

- **Candidate teardown and candidate Begin/Update must share one exclusion
  domain.** Hold it across subscription removal, `UnadviseSink`,
  `EndUIElement`, controller restoration, and retained-work repost. Re-entrant
  work must survive the old teardown and keep its own eventual cleanup owner.

## Overflow-hazard test construction (dictionaries and prediction)

- **`dictc::parse_entries` rejects any single `reading`/`surface` field over
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

## Session state that describes a composition

- **A flag that qualifies a composition must not be able to outlive one.**
  `Session::shifted_ascii` (the temporary English composition) was cleared
  only by `Session::reset` and by receiving a non-ASCII character. Erasing the
  composition with Backspace or forward Delete reached neither, and the
  resulting `Idle`-with-latch state was invisible, sticky (every romaji
  keystroke is ASCII), and unrecoverable by any key (#51). Restore such an
  invariant at one point per key in `apply_key`, before prediction and
  rendering — not by adding a clear to each erase path, which is the
  list-of-cases shape `Session::reset`'s doc comment already argues against.

- **`is_composing()` is not a safe proxy for "this flag is still meaningful".**
  It deliberately ignores `shifted_ascii` once `raw_input` is empty, and
  `commit_pending` returns early on `!is_composing()`. Any state that both
  gates on `is_composing()` and is only cleared inside it can strand itself.

- **Measure a "nothing works" report instead of reasoning about it.** Pressing
  each plausible recovery key from the stuck state and recording consumed /
  resulting preedit / flag turned an unfalsifiable severity claim into a table
  that named the two keys that *would* have recovered (Escape, Enter) and why
  neither could fire. That table is what proved the root cause was the whole
  cause and not one of several.

- **Key-map modifiers match exactly, so a held Shift turns Backspace into a
  different key.** `[composing] backspace = delete_back` does not consume
  Shift+Backspace. Holding Shift to type English therefore leaked Backspace
  (and Left/Right) to the host unless those chords were bound. Verified by
  `KeyMap::lookup(State::Composing, shift+backspace)` and the AIUEO repair
  tests in `shift_latin_order_tests`.

- **When visible English text is `raw_input`, the caret must be a raw-input
  index.** `render_preedit` used to pin the caret at `raw_input.len()` while
  Backspace popped the last raw byte and deleted `preedit[cursor-1]`. After
  Left, that pair produced AIUEO → AIUOE / AIUOEO. Verified by
  `production_left_then_backspace_deletes_the_character_before_the_caret`
  and by a mutant that restored end-append (`AIUOE`).

- **`resync_shifted_ascii_from_raw` is what makes later English conversion
  possible, even though the user sees `raw_input`.** `begin_conversion`
  beeps when `preedit` is empty. A no-op resync leaves `preedit` empty after
  Shift+Latin typing, so Space never reaches the IT-flag dictionary path.
  Visible-text tests cannot kill that mutant; a convert-after-CLAUDE test
  can. Verified by cargo-mutants 27.1.0
  (`replace resync_shifted_ascii_from_raw -> Ok(())` caught) and
  `resync_is_required_for_shifted_ascii_dictionary_conversion`.

- **`WriteCoordinator::attach` refuses a plan whose `before` is not the
  journal tail.** After engine plans commit `AIUEO`, a host-stolen
  `AIUE`→`AIUOEO` attach is `ProjectionMismatch`. This is the strongest
  COM-free stand-in for “Shift+Backspace must not be applied by the host
  while the engine still owns the key.” Verified by
  `shift_latin_backspace_retype_plans_commit_in_order_and_never_aiuoeo`.

- **Whole-function llvm-cov of `feed_character` / `apply_backspace` /
  `render_preedit` is not Shift-Latin coverage.** Each function returns
  early on `shifted_ascii`; the remaining regions are kana / pending-romaji
  / CJK normalize. Measure the early-return arm line ranges separately
  (2278–2326, 3502–3512, 4165–4193) and list the rest as out of scope.
  Verified 2026-08-15: `shift_latin` filter, 45 tests, arm 98.5 / 90.0 /
  75.5 while whole-function backspace stayed 25.0% (20/80). `mcdc_records`
  were 0.

- **Escape after Convert is not convert-cancel.** Production Escape clears
  the English buffer; converting Backspace cancels conversion without
  deleting a letter. A coverage PBT that maps `Convert` to Space after
  punctuation also diverges: `decide_shift_ascii_convert` inserts a literal
  U+0020. Verified by the first fail of
  `production_convert_cancel_then_home_backspace_keeps_press_order` (got
  `"X"` vs `"XAIUEO"`) and coverage-neighbor case 1 (`"AIUEO--IEUA "` vs
  `"AIUEO--IEUA"`).

- **`sakura_tsf_test_host` / `e2e-host` is the installed language profile,
  not a Sakura-only HWND.** The next automated layer that does not touch
  the installed IME is a process-local EDIT plus `checked_host_call` /
  `plan_from_visible`. A live `ITfContext` / `ITfRange` still cannot be
  constructed in this crate. Verified by
  `shift_latin_settext_payloads_reach_a_process_local_edit_hwnd_and_never_aiuoeo`
  and the recovery-test comment in `text_service.rs`.

- **Process-leak assertions must identify the owned artifact, not only the
  executable name.** The capture verifier initially counted the user's
  installed `sakura_engine.exe` as a leak; scoping by the repository debug
  path plus `--test-pipe` distinguished it from owned children. Verified by
  the missing-dictionary capture check: exit 2, no output, identical private
  process sets.

- **Serialize real engine child lifetimes within a Windows integration test
  binary when parallel startup is load-sensitive.** `space_key_dispatch_pipe`
  reproduced `STATUS_ACCESS_VIOLATION` under repeated `--test-threads=2` runs
  while five serial runs passed; a lifetime mutex in the shared harness was
  followed by ten successful parallel-thread repetitions and a green
  workspace run. The mutex owns no protected data, so recover its poison after
  a failed assertion; otherwise one failed child test suppresses the terminal
  results of every later integration test in the process.

- **Pass a fixed diagnostic payload as a record, not positional scalar
  arguments.** Converting `debug_trace` to `TraceEvent` preserved its
  content-free wire row and made the evaluation dependency graph pass strict
  `cargo clippy -- -D warnings` without an allow-list.

- **A release sparse-checkout must include every pinned input the local builder
  validates, not only the obvious dictionary directory.** `build-dictionary`
  consumes Mozc's `src/data/rules/segmenter.def` in addition to
  `src/data/dictionary_oss`; omitting it made the release workflow fail before
  compilation. Verified by a complete two-pass 1.0.18 dictionary build after
  adding that exact file to the workflow checkout.

- **Do not generate a numeric surface already supplied by an exact dictionary
  edge.** N-best deduplicates by rendered surface, so a cheaper generated `一日`
  can otherwise hide the lexical entry's cost, ordinal, and detail provenance.
  Skip only the identical generated surface, then rank the remaining numeric
  spellings behind the exact lexical form. Verified by the synthetic core test
  and all 19 shipped-dictionary ranking tests for 1.0.18.
