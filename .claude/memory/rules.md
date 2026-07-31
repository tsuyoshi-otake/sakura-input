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

- **The CI runner's CPU is not the development machine's CPU, and that
  silently narrows differential SIMD coverage.** GitHub's `windows-latest` is
  an **AMD EPYC 7763** (Zen 3): AVX and AVX2, but **no AVX-512**. The
  development machine is a Ryzen 7 9700X (Zen 5), which has AVX-512BW. The
  `simd::` agreement tests only exercise kernels the host actually supports,
  so the AVX-512 kernel has **no CI coverage at all** and is verified only
  locally. Say so rather than reporting the criterion as blanket-verified.

- **A sandbox test that does not prove it is sandboxed proves nothing.** A
  test that connects to the pipe "from an AppContainer" passes just as
  happily when the AppContainer was never applied. The child must assert
  `TokenIsAppContainer` on its own token before it does anything else.

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
