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
