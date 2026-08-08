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
  before releasing anything that touches the kernels, and confirm it prints
  `["scalar", "avx", "avx2", "avx512"] (tier avx512bw)`. Verified here on
  2026-07-31: it does.

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
