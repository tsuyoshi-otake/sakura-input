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
