# Journal

Append-only. What was tried, what happened, and what it cost to find out.
Entries that produced a general rule say so; the rule itself lives in
`rules.md`.

---

## 2026-07-31 — Phase 1 (M0 plumbing)

### The SIMD width normalizer was a measured regression

Committed in a previous session with a doc table describing speedups nobody
had measured. Writing `crates/sakura-core/tests/width_bench.rs` produced:

| corpus | first measurement |
|---|---|
| one keystroke | 0.47× |
| 45-byte shell command | 6.5× |
| 90 B Japanese prose | **0.66×** |
| 84 B mixed | 1.24× |

Japanese prose is the workload this IME exists to serve, and the "optimized"
path made it a third slower.

Investigated: the scalar fallback re-sliced the source by byte index
(`src[at..].chars().next()`), and `&str` indexing re-validates a character
boundary every time. On Japanese every character takes that branch, so the
validation cost was paid per character while the run scanner found nothing to
skip.

Fixed structurally rather than by tuning a constant: walk a `Chars` iterator
and use `chars.as_str()` for the remainder, and short-circuit inputs shorter
than one vector block to a plain per-character loop in an inlinable body.
Japanese went 0.66× → 0.93×, the shell command settled at a stable 5.3×, and
mixed text reached 1.4–1.5×. The residual ~0.4 ns on a single keystroke is
real and was reported as nanoseconds rather than dressed up as a ratio; it is
irrelevant against a 5 ms budget.

The full-width-everything policy is still ~20 % slower than scalar, and that
is documented in `width.rs` as knowingly accepted rather than hidden: nobody
sets every channel to full-width, and the half-width default is the fast one.

Distilled → `rules.md` (benchmark before the doc table; `&src[at..]`
re-validates; short strings skip the scanner).

### The benchmark itself was measuring noise

The same unchanged loop reported 53 ns and then 100 ns on consecutive runs.
Switched from one timed run to the minimum of seven, which is the right
estimator when every source of error is additive. Distilled → `rules.md`.

### CI had never run

Both workflows triggered on `master`; the branch, and GitHub's default
branch, is `main`. `gh run list` was empty — not "no recent runs", but no runs
ever, on any commit. Fixed and confirmed both workflows then executed and
passed. Distilled → `rules.md`.

### Exit criteria measured, not asserted

- IPC latency: p99 **35.9 µs** against a 5 ms budget (5,000 samples, release,
  over the real per-logon-session pipe with the client in a separate process).
  139× of headroom, which is the useful part: the transport is not what will
  decide whether typing feels instant once Phase 2 puts dictionary lookups
  behind it.
- DLL size: `sakura_tsf.dll` **142.5 KB** against a 1 MB ceiling.
- No orphaned processes after every run, checked by process listing rather
  than by the test command returning.

### The watchdog had never actually been watched

`watch.rs` unit-tests the *rule* (`decide` → Launch / Wait / GiveUp) and stops
there on purpose, because a unit test that really started an engine would
seize the pipe a developer's live IME is using. Nothing had ever checked the
other half: that a running renderer notices a dead engine and starts another.
That is the half the crash-resilience criterion actually promises, and its
failure mode is not a stutter — the IME is gone until the next logon.

`crates/sakura-renderer/tests/watchdog_recovery.rs` now does it end to end,
with a control phase first (kill the engine with no renderer, and the pipe
must stay dead) so that the recovery is attributable to the renderer rather
than to anything ambient. Measured: **recovery in ~100 ms**, consistently
across three back-to-back runs, and the restarted engine composes さ rather
than merely answering the pipe.

Two things went wrong on the way, both worth keeping:

- Deliberately removing the renderer made it fail after 30 s with the intended
  message — which is the only reason "it passed" means anything.
- One anomalous run recovered at 12.75 s with **no renderer started by the
  test**. That is roughly one `WATCH_BUDGET` (15 s), and the explanation is a
  renderer leaked by the previous run still sitting in its long poll. The
  control's 5 s window missed it entirely. Fixed by refusing to start when any
  Sakura process is running — a renderer holds no pipe, so the pipe probe
  cannot see it and only process enumeration can — and by killing the renderer
  *before* shutting the engine down, since a live watchdog restarts whatever
  the teardown stops. The test now also asserts nothing is left running.

AVX-512 CI coverage was raised and closed as an accepted decision: verified
locally, CI is not to be extended for it.

### The sandbox claim, answered by a real token

`sakura-ipc`'s pipe descriptor names both AppContainer SIDs, withholds
`FILE_CREATE_PIPE_INSTANCE`, and carries a low mandatory label — three things
whose unit tests only check the SDDL *string*. Nothing had handed that
descriptor a token Windows built as an AppContainer, and every other test on
this pipe connects from an ordinary desktop token, which cannot fail any of
the three.

`crates/sakura-engine/tests/appcontainer.rs` now launches a copy of its own
binary through `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` — the only way a
process becomes an AppContainer — and the child asserts `TokenIsAppContainer`
on itself before touching the pipe, because a launch that quietly produced an
ordinary process would connect just as happily. Verified load-bearing by
removing the attribute: it then fails at that assertion, never reaching
`Client::connect`. Restored, green, and green on CI.

It passes locally and on `windows-latest`, which also settles a question the
file's own docs flagged as uncertain — a hosted runner *can* create an
AppContainer profile and launch a sandboxed child.

### CI's SIMD coverage was a coin flip, and the log did not say which side

Recording "the CI runner is an EPYC 7763, so AVX-512 has no CI coverage" was
wrong within the hour. The very next run came up **EPYC 9V74** — Zen 4, which
has AVX-512. Same workflow, same repo, 37 minutes apart.

The dangerous part was not the varying CPU but that `cargo test` captures
stdout: the `simd::` tests print the kernel list they exercised, and neither
run's log contained it. So a green CI run covered AVX-512 or did not, and
nothing distinguished the two — the same defect as this repository's earlier
workflow-that-never-ran, in a subtler place.

Fixed by having the run state its own scope: a step re-runs `simd::` with
`--nocapture` so the log names the kernels. Reporting, not requiring — the
owner's decision that AVX-512 is verified locally stands, and that local run
was performed: `["scalar", "avx", "avx2", "avx512"] (tier avx512bw)`.

The step paid for itself on its first green run, which drew a **third**
processor in three runs — an Intel Xeon Platinum 8573C — and printed
`["scalar", "avx", "avx2", "avx512"] (tier avx512bw)`. So that run really did
cover AVX-512 in CI, and for the first time the log says so instead of leaving
it to be guessed from a processor name.

Then it paid for itself a second time, by refuting me. The fourth run came up
**EPYC 9V74 again — and printed `["scalar", "avx", "avx2"] (tier avx2)`.** I
had already written "EPYC 9V74 (Zen 4, which has it)" into the rule and "yes"
into the Issue #2 table, on the strength of the part number alone. Zen 4 does
have AVX-512 in silicon; this runner does not expose it to the guest, and
`is_x86_feature_detected!` is the only thing in the loop that knows the
difference.

So the correction is sharper than "the runner pool varies": **a CPU model name
is not evidence about the ISA a process can use.** Both times I got this wrong,
the mistake was the same shape — reading a name and concluding a capability.
The two rows I had marked from the datasheet were never measurements, and are
now recorded as unknown rather than quietly left as "yes".

A fifth run (`173c216`) drew an EPYC 7763 and printed `tier avx2`, which is the
same lesson from the friendlier side: the Zen 3 guess turned out right, and it
was still a guess until the log printed it. Tally so far — five runs, three
with a printed kernel list, **one** covering AVX-512. That kernel's CI
coverage is occasional, which is exactly why it is verified locally.

Distilled → `rules.md` (replacing the wrong version of the rule outright, twice
now, rather than appending a correction beside it).

And the fix itself broke CI, in a way worth more than the fix. The new step's
command ends in the test filter `simd::`, and unquoted in YAML that is a colon
immediately before a space — the mapping separator. The file stopped parsing
at that line. GitHub never said so: the run appeared named
`.github/workflows/ci.yml` rather than `CI`, ran zero jobs, had no log to
fetch, and `gh workflow run` rejected a dispatch with *"Workflow does not have
'workflow_dispatch' trigger"* about a file whose seventh line is
`workflow_dispatch:`. Every symptom pointed at configuration; the cause was
syntax.

Found it by reading the diff for what YAML would object to rather than by
hunting for a missing trigger, then confirmed with a parser: `mapping values
are not allowed here`, line 56, column 54. Quoted the value. Validated both
workflow files locally before pushing again, and checked the validator
actually rejects the committed broken one first — a checker nobody has seen
fail is not a checker. Distilled → `rules.md`.

### Left manual on purpose

Typing matrix (Notepad / Windows Terminal / Chrome), elevated host, crash
resilience, focus loss mid-composition, and clean uninstall need a real host
application or a VM snapshot. `scripts/vm-smoke.ps1` automates the install →
type → uninstall → still-type loop and is explicit about the line between what
it verifies and what it reports as MANUAL — a smoke test that reports green
for something it never checked is worse than not having one.

`installer/setup.iss` has never been compiled locally (Inno Setup is not
installed on this machine); `.github/workflows/installer.yml` is what actually
runs ISCC over it, and it passes.
