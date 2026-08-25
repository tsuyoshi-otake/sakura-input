# Journal

Append-only. What was tried, what happened, and what it cost to find out.
Entries that produced a general rule say so; the rule itself lives in
`rules.md`.

## 2026-08-19 — real release-artifact Issue #66 capture

Ran `ime-eval capture` against the release `sakura_engine.exe` and the
release dictionary (`system.dic` SHA-256
`f09f8bf4ebf6e21d170123672ddbb8c7a5f450571807a3ba938e42497c723b80`) with
the same artifact on both baseline and candidate sides. The engine SHA-256
was `b595b55645d51f4c0375feef2fddcd5d52b6c93eb4f500efcac3a9ac4562b045`.
Across all 25 Issue #66 cases, 16 produced capture files and 9 terminated
fail-closed with exit code 2 and no capture file. `sem-000066-kyou` and
`sem-000066-esp32` were confirmed successful; `sem-000066-avx-512` produced
no candidate list. No release test-engine, `ime_eval`, cargo, or rustc
process remained afterward. This is an artifact-coverage smoke run, not an
A/B quality comparison, because baseline and candidate were identical.

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

## 2026-08-13 — load-sensitive flake: TSF handshake tests under `--workspace`

`text_service::tests::local_reconvert_encode_failure_rejects_only_that_operation`
failed once during a full `cargo test --workspace` run ("the handshake must
have completed", text_service.rs:5493) and then passed 10/10 in isolation.
Root cause is load sensitivity, not the code under test: `Engine::attached_to`
runs connect + Hello + CreateSession against the fake named-pipe engine inside
the wall-clock `RECONNECT_BUDGET` of 50 ms (engine.rs), and a parallel
workspace run can delay scheduling of the fake server thread past that budget.
Same category as the prediction-handoff flake fixed in #42 (that one retries
in the test while keeping the engine's 10 ms fail-open window untouched). Any
TSF test asserting `is_connected()` right after `attached_to` shares this
exposure. If it recurs, make the *test* tolerate load (retry the attach), do
not widen the product's 50 ms reconnect budget — the budget is deliberately
no larger than a keystroke budget.

## 2026-08-14 — the packaging version gate depended on the checkout (#50)

`scripts/build-installer.ps1` refused to build 1.0.2 with "setup.iss must
contain exactly one AppProductVersion" on a tree where the file declared
exactly one, correctly. `installer/setup.iss` is stored with LF; this machine
has `core.autocrlf=true`, so a checkout materializes CRLF. The gate anchored
its regex with `(?m)^#define AppProductVersion "([^"]+)"$`, and .NET's
multiline `$` matches immediately before the `\n`, leaving the `\r` unmatched.

What made it confusing: the same file had built fine for the 1.0.1 release a
few hours earlier. The difference was not the content but the provenance of
the working copy — 1.0.1's setup.iss had been written by an editor (LF
preserved), while 1.0.2's had been materialized by `git checkout main`
followed by a fast-forward, which runs the smudge filter. So the failure only
appeared after a checkout touched the file, which is exactly the case a CI
runner always hits.

`.github/workflows/release.yml` carried the same shape twice. Fixed all three
with `\r?$` and added `crates/sakura-regtool/tests/packaging_version.rs`,
whose third test reads the gate files and fails if a version anchor stops
allowing the CR. Proved non-vacuous by reintroducing the old anchor.
Distilled → `rules.md`.

## 2026-08-14 — A latch that describes a composition outlived one (#51)

User report: "Shift を押して英語入力したあとにさ、復帰しない場合があるね。
なにしても日本語打てない。他の IME に切り替えて戻ると復旧する。"

`Session::shifted_ascii` is the temporary English composition latch. Set in
`feed_character`; cleared in exactly two places, `Session::reset` and the
`shifted_ascii && !character.is_ascii()` arm. Erasing the composition with
Backspace reaches neither: `apply_backspace`'s shifted branch pops the
buffers and returns, `is_composing` stops counting the latch once `raw_input`
is empty so the session reports `Idle`, and `commit_pending` returns early on
`!is_composing()` — so Enter never reaches `reset`. The non-ASCII arm cannot
fire because every romaji keystroke is ASCII.

The severity claim in the report was worth measuring rather than assuming. A
probe pressed each plausible recovery key from the latched-idle state and then
typed `ka`: Escape, Enter, Space, Muhenkan, Henkan, KanaMode and Backspace all
left the latch set and produced `ka`. That is the literal "なにしても", and the
IME switch recovers because deactivation ends the text service and the next
activation builds a fresh `Session`.

Fixed by restoring the invariant once per key in `apply_key`, before
prediction and rendering, rather than adding a clear to each erase path —
`Session::reset`'s own doc comment already argues against the list-of-cases
shape, and forward Delete proved the point by having the same leak.

Three of the four new tests fail with the one-line fix commented out; the
fourth is the opposite polarity (a partly erased composition stays English)
and correctly passes either way.

## 2026-08-19 — real-engine capture goal-loop iteration 1

The independent rubric verifier passed C1, C2, C3, C6, and C7. C4 failed
because its post-check observed `sakura_engine` PID 3608 still running after
the missing-dictionary command. C5 failed during `cargo test --workspace
--offline` when `space_key_dispatch_pipe` terminated with
`0xc0000005 (STATUS_ACCESS_VIOLATION)`. These are verifier observations; the
cause and ownership of PID 3608 still require confirmation before any fix.

Investigation confirmed two separate issues. PID 3608 was the user's installed
engine at `C:\Program Files\Sakura Input\versions\1.0.17-f007efdaa1d99083`,
not a capture child; the verifier's broad process check was invalid. The
workspace crash left six owned debug `--test-pipe` children with dead parent
PID 43508; those were stopped by exact executable-path and private-pipe
filtering, while the installed engine was left untouched.

The access violation is load/order-sensitive in the test harness: the
`space_key_dispatch_pipe` binary passed in isolation and in five serial
repetitions (`--test-threads=1`), but failed on iteration 2 of a five-run
parallel stress (`--test-threads=2`) with the same
`0xc0000005 (STATUS_ACCESS_VIOLATION)`. The failing run left no private engine
afterward; this establishes test-process concurrency as the reproducible
boundary, not a capture protocol failure.

The first C4 process check was corrected to exclude the installed engine and
compare only the repository debug executable with `--test-pipe`. The
missing-dictionary command returned exit code 2, wrote no capture file, and
left the exact private-test-engine set unchanged. After stopping two
orphaned debug children left by the interrupted workspace attempt, the full
`cargo test --workspace --offline` run completed with exit code 0 and no
private test engines or cargo/rustc processes remained.

Iteration 2 used a fresh rubric-verifier context after narrowing C4's process
identity and adding the real-engine test harness lifetime lock. All seven
rubric criteria passed: the real capture test spawned the actual engine and
found `今日`, the invalid-artifact command exited 2 without an output file,
the workspace test exited 0, source isolation/bounds were observed, and the
final private test-engine count was zero. The two confirmed reusable rules
were promoted to `rules.md`.

The follow-up strict clippy check exposed the previously known
`debug_trace::{emit,emit_at}` argument-count warnings, plus a needless
`return` in the capture cfg branch. The trace API was changed to a bounded
`TraceEvent` record, all callers were migrated, and the cfg branch was made
expression-based. `cargo fmt --all -- --check` and
`cargo clippy -p sakura-ime-eval --all-targets --offline -- -D warnings` then
passed.

Iteration 3 added strict clippy as C8 and ran a fresh independent verifier.
All eight criteria passed: workspace evidence was `405 passed / 0 failed` and
`181 passed / 0 failed` across the relevant suites, the real capture and
fail-closed checks passed, clippy emitted no diagnostics, and no private
test-engine process remained. The fixed-payload API lesson was promoted to
`rules.md`.

## 2026-08-25 Sakura Pad wireframe rebuild, phases 1-2 (Issue #92, follows #91)

Owner supplied a wireframe and asked for Sakura Pad to be rebuilt to it:
a memo list with a count in the title, a search box, a sort control, a
responsive 520-logical-px breakpoint (1a folded / 1b split), a bottom bar,
and a GitHub sync flow. Header colour must come from the existing
sakura-input palette, not the wireframe's pink. Sync is to be implemented
including real network traffic. Six phases were approved; this entry covers
phases 1 and 2.

Phase 1 extracted `crates/sakura-renderer/src/theme.rs` from `candidate.rs`
so the pad and the candidate popup resolve every colour, font and scale
through one module, and moved pad storage to `SKRLPAD2` with a v1 migration
that keeps the old single memo as the first entry.

Phase 2 rebuilt `pad.rs` around `layout()`, a pure function of the client
rectangle, the DPI and which pane is showing. Controls are native
(owner-drawn LISTBOX and BUTTONs) so UI Automation comes free, and a control
that is not part of a shape is hidden rather than moved offscreen.

Three defects were found only by looking at the running window, and each one
is now covered by a test:

1. Symptom: the header's bottom rule appeared as two short segments with a
   gap in the middle. Root cause: `paint()` draws the rule across the whole
   header, but `layout()` gave the header-title STATIC `bottom:
   header.bottom`, so the child's own background repainted exactly its own
   span of the rule. Fix: the title now stops one border above the header's
   bottom edge. Verification: `no_control_covers_the_rule_the_header_paints`
   checks every header child against the rule at four DPIs and five widths,
   and a pixel probe of the captured window now reports one run `0-515`
   across the full client width instead of `0-55, 420-507`.

2. Symptom: the folded list pane showed the open memo's status line
   (`10:17 - 48 文字 - 保存済み`) above its search box, describing an editor
   the user could not see. Root cause: the meta band was cut from the top of
   the content unconditionally, though it belongs to the editor. Fix:
   `status` became `Option<RECT>` — the file's existing convention for a
   control that is absent rather than misplaced — and the meta band collapses
   when the editor is not showing, so the list takes the room. Verification:
   `every_band_contains_the_controls_it_owns` now asserts the status line
   exists exactly when the editor does.

3. Symptom: the search box was a blank rectangle with no affordance; the
   wireframe shows 「検索」 in it. `EM_SETCUEBANNER` was not usable because it
   needs comctl32 version 6, which would mean shipping a visual-styles
   manifest that restyles every control in the pad. Putting the word in the
   field's text would have been worse: the filter reads that text, so a
   resting pad would have matched no memo and shown an empty list. Fix: the
   search field keeps its own window procedure and the hint is painted after
   the class procedure's `WM_PAINT`, using the field's own `EM_GETMARGINS` so
   the hint starts exactly where typing will. Verification:
   `the_search_hint_shows_only_while_the_field_is_empty_and_unfocused` for the
   predicate, plus a real-process assertion that the field's text is empty
   while the hint is on screen.

Learning, and the reason all three existed at once: a layout function that is
graded only by its own unit tests grades the arrangement, not the window. The
unit tests were green while the rule was broken, the status line was in the
wrong pane, and the field had no hint, because none of those are properties
of the returned rectangles. `crates/sakura-renderer/tests/pad_ui.rs` was
written to close that gap — it drives a real renderer process over a private
`--test-pipe`, resizes across the breakpoint, and reads back the actual
control rectangles.

Two Win32 facts cost a test run each and are worth keeping:
`SetWindowTextW` and `GetWindowTextW` do not cross a process boundary for a
control. `SetWindowTextW` reports success and does nothing, which is how an
early version of the fixture seeded five memos that were all empty;
`GetWindowTextW` returns an empty string. `WM_SETTEXT` / `WM_GETTEXT` /
`WM_GETTEXTLENGTH` through `SendMessageW` are marshalled and do work.
Separately, a renderer started with `--test-pipe` exits with code 0 as soon
as its connection fails, because `watch::run` treats `binding.is_test()` as a
terminal `Signal::Ended` — so a fixture must claim and serve the pipe before
spawning the renderer, not after.

Verification of the pair of phases: `cargo fmt --all -- --check` clean,
`cargo clippy -p sakura-renderer --all-targets` clean,
`cargo test --workspace` 91 suites all ok with 0 failures (123 of them in the
renderer binary), `cargo test -p sakura-renderer --test pad_ui -- --ignored`
passing against a real renderer process, `git diff --check` clean,
`ci/dep-policy.ps1 -SelfTest` and `ci/dep-policy.ps1` both passing over 73
locked packages, and no leftover cargo, rustc, renderer or engine processes.
Both shapes were confirmed on screen from the captured window: 1b shows the
resident list, the search hint, the editor meta row with 共/削, and a bottom
bar confined to the list column; 1a shows ≡, one pane at a time, and 共/削 in
the bottom bar with 削 at the far end.

Not committed at the time of writing, so there is no SHA to record. The
working tree carries the owner's own uncommitted Issue #91 work across 18
tracked files — engine dispatch/session/server/ui, proto, core preferences,
settings, and the renderer's main/watch/candidate/Cargo.toml — and phases 1
and 2 edit several of those same files, so no per-phase commit can be made
without either sweeping that work in or splitting it hunk by hunk. Left for
the owner to decide.

## 2026-08-25 — Issue #92 phase 2: reconciled the pad against the authoritative design

The wireframe the phase was planned from turned out not to be the design. The
owner supplied the real one — a Claude Design page, `Sakura Pad Mockup.dc.html`
— after phase 2 was already green, so the phase closed with a reconciliation
pass rather than a rewrite. Eight differences were real and were changed; six
more are deliberate and are recorded here so the next session does not "fix"
them back.

One of the eight was a genuine rendering defect rather than a taste
difference. `Arc` draws the sync icon's ring as a dotted, broken circle at 18
logical pixels: GDI fits a curve to a pixel grid the figure barely spans, and
what survives is a scatter of pixels with a blob where the arrowhead is. It
was replaced with a ten-segment polyline ring on the same 32-unit grid plus a
filled triangle head. Ten honest straight lines read as a circle at that size;
one dishonest curve does not. The magnifier added for the search chip is drawn
the same way for the same reason. `Ink::arc` and the `Arc` import are gone —
nothing in the pad should reach for it again at icon sizes.

The other seven: the selected row's rail is the pad's own `ROW_RAIL_96` = 3
rather than the candidate popup's `RAIL_WIDTH_96` = 2, because it marks which
memo the whole right pane is showing, not which line of a glanced-at list is
current; unselected rows are separated by a hairline in `selected`; the filled
control rests at `rail` and darkens to `action` when pressed (it had them the
other way round, so pressing it lightened it); pressable things get 6-logical-
pixel corners via a new `rounded_box`; `destructive` and `button_shape` now
take `wide`, so the trash is danger-colored only in the folded bar and the
folded bar's controls are borderless; and the search field is a filled rounded
chip with a magnifier rather than the window's one outlined box.

`RoundRect` leaves the four corners outside its figure, so every owner-drawn
button paints its ground first and then its face. Skipping that shows whatever
the DC was holding in the corners. `rounded_box` also has to create a pen even
when the caller wants no outline, because `RoundRect` outlines with whatever
pen the DC holds; the fill color is used as its own edge.

Deliberate divergences, all of them either an owner decision or a Windows
constraint: the design's custom 38-pixel title bar stays a standard Windows
caption (plan item 5 — snap, maximize, high contrast and UIA all stay
standard); Zen Kaku Gothic New and Klee One do not ship with Windows, so the
pad stays on Yu Gothic UI; the unsynced dot and the S1/S3/S4/S5 sync sheets
belong to phases 3-5; and the ruled background was implemented **against**
plan item 3, which had said it would be skipped — the plan was written from
the wireframe, where the ruling looked like drawing texture, and the design
specifies a 24-pixel grid outright. The colors needed no reconciliation at
all: the design's `#B28D96`, `#F7F6F4`, `#E8E5E2` and `#FFFDFB` are already
`rail`, `surface`, `selected` and `paper`, and the grid's `#F9F6F4` /
`#2F2D2D` are exactly what the design's translucent rules blend to over paper.

Verification: `cargo fmt --all -- --check` clean, `cargo clippy -p
sakura-renderer --all-targets -- -D warnings` clean, `cargo test --workspace`
all suites ok with 0 failures, `git diff --check` clean, no leftover cargo,
rustc, renderer or engine processes. Confirmed on screen at 96 DPI in the
light theme from captured windows of a real renderer: the two-pane shape shows
the chip, the ruled paper, the 3-pixel rail, the hairlines, the filled pill
and the two framed controls in the editor's head row; the folded shape shows
the borderless bar with the trash alone in danger red at its end. Dark and
Windows high contrast are still unconfirmed on screen.

A false alarm along the way is worth remembering: `PrintWindow` with
`PW_RENDERFULLCONTENT` re-renders through `WM_PRINT`, so it can capture a
half-painted control. One capture showed an empty editor body and a
preview-less row, which looked exactly like memo content being destroyed on
resize. It was not — re-capturing showed both intact, and
`SendMessageW(WM_GETTEXTLENGTH)` reported the same 6 and 48 units at every
step of wide → narrow → toggle → toggle → wide. A screenshot cannot prove the
absence of a paint artifact; ask the control.

Still not committed, for the reason recorded in the previous entry.

## 2026-08-25 — Issue #92 phase 2: the pad's title bar

Symptom: the owner said the window title's level of finish was low. Zooming a
screen capture of the caption showed what that meant — Windows' generic
placeholder icon beside "Sakura Pad", on a caption in the system's own color,
sitting above a window whose first band is `surface`. The top thirty pixels
read as belonging to a different program than the twenty-five hundred below
them.

Root cause: `register_class()` in `pad.rs` built `WNDCLASSW` with a window
procedure, a cursor and a class name and nothing else, so `hIcon` was null.
Windows draws its placeholder for a class with no icon — there is no fallback
to the executable's resource for a class-registered window. The caption color
was never asked about at all.

Fix: a new `crates/sakura-renderer/src/pad_caption.rs`. `icons()` loads
`assets/sakura-input-icon/sakura-input.ico` — ten sizes from 16 to 256 —
at `GetSystemMetricsForDpi(SM_CXICON/SM_CXSMICON)` and hands both to the
window with `WM_SETICON`; `dress()` sets `DWMWA_USE_IMMERSIVE_DARK_MODE`,
`DWMWA_CAPTION_COLOR` = `surface`, `DWMWA_TEXT_COLOR` = `ink` and
`DWMWA_BORDER_COLOR` = `border`. `PadState` owns the pair, because `WM_SETICON`
borrows rather than takes: the handles have to outlive the window and be
destroyed after it, which the field ordering and `PadWindow::drop` already give.
Icons are applied at `WM_CREATE` and re-applied at `WM_DPICHANGED`; colors are
re-applied at `WM_CREATE`, `set_theme` and `WM_THEMECHANGED`/`WM_SETTINGCHANGE`.
`theme.rs` gained `resolves_dark` / `resolve_dark`, asked of the same inputs
`resolve_palette` uses so the caption cannot end up light over a dark window.
`Win32_Graphics_Dwm` was added to the renderer's feature list.

Under Windows high contrast all four attributes are handed back: the dark flag
goes false and the three colors go `DWMWA_COLOR_DEFAULT`. A program tinting its
own caption is precisely what that setting exists to stop.

The design's custom 38-pixel title bar was **not** built, which is plan item 5
and was already the owner-approved shape. A redrawn caption has to
re-implement `WM_NCCALCSIZE`, `WM_NCHITTEST` with `HTMAXBUTTON` for the snap
flyout, the resize borders, the system menu, and the whole of high contrast —
and loses UIA's idea of what a window is if any of it is missed. The real
caption plus an icon and four DWM attributes reaches the same visual intent at
none of that risk.

Verification: `cargo fmt --all -- --check` clean, `cargo clippy -p
sakura-renderer --all-targets -- -D warnings` clean, `cargo test --workspace`
all suites ok with 0 failures (the renderer binary went 126 → 129: two in
`pad_caption`, one in `theme`), `git diff --check` clean, 0 residual cargo,
rustc, renderer or engine processes. Confirmed on screen at 96 DPI in the light
theme: the product icon is in the caption and the caption is `#F7F6F4`,
continuous with the header band under it. Dark and high contrast are still
unconfirmed on screen, as they were before this change.

Two things learned. `PrintWindow` is the wrong instrument for a caption: DWM
composes the frame, so a `WM_PRINT` re-render shows the legacy non-client
painting rather than the color the window actually has. The capture for this
had to be `CopyFromScreen` over `GetWindowRect`. And the installed layout needs
no change — `installer/setup.iss` already copies `sakura-input.ico` into
`{#AppVersionedDir}`, the same directory as `sakura_renderer.exe`, so
`current_exe().parent()` resolves it there exactly as it does in a build tree.

Still not committed, for the reason recorded in the previous entries.

## 2026-08-25 — Issue #92 phase 2: the pad's drawn faces, first frame, and scroll bars

Four things the owner reported after looking at the pad, and one they did not
have to: the opening frame, the icons, the icon contrast, and the scroll bars.

### The drawn faces were badly drawn

Symptom: 「全体的にアイコンの質がわるい」, and 「Markdown でのコピーボタンが
わかりにくい」.

Root cause: two separate ones. GDI does not antialias — `LineTo`, `Polyline`
and `Polygon` snap to whole pixels — so the search ring at 18 px was a lumpy
polygon and every diagonal was a staircase. And the copy control's face was an
outbound arrow, which says the memo leaves for somewhere; it does not, it goes
to the clipboard as Markdown.

Fix: `pad_icon.rs` rewritten. Each glyph is drawn at `SUPERSAMPLE = 4` into a
32bpp top-down `CreateDIBSection` — white ground, black ink, `ExtCreatePen` with
`PS_ENDCAP_ROUND | PS_JOIN_ROUND` — then each 4×4 block is averaged into
coverage (`255 - (pixel & 0xFF)`), written as premultiplied BGRA into a second
DIB, and `AlphaBlend`ed. GDI+ was considered and rejected: it is a second
drawing model in a process that has a 10 MiB private-working-set gate.
`PadIcon::Share` became `PadIcon::Copy`, drawn as two sheets; the control's
window text — which is its accessible name — became `Markdown としてコピー`,
and `pad_tooltip.rs` gives every icon-only control a sentence for the pointer
(`このメモを Markdown としてコピー`). Tooltips use `TTF_IDISHWND | TTF_SUBCLASS`,
so nothing in the pad's own procedure relays mouse messages, and `Tooltips` owns
the UTF-16 buffers because `TTTOOLINFOW` keeps the pointer rather than the text.

Then: 「アイコンだけコントラスト高いな」. An icon is a continuous stroke where a
word is a row of thin ones, so `ink` reads darker on a face than on the text
beside it — and antialiased strokes read heavier than the aliased ones did at
the same width, because every pixel is covered rather than snapped. Icon-only
unpressed faces moved to `colors.annotation` and `STROKE` went 2.2 → 2.0 grid
units; pressing one still brings it to full ink.

### The first frame was not a finished frame

Symptom: 「起動時の描画が美しくなかったな」.

Root cause: the class has a null `hbrBackground` and `WM_ERASEBKGND` returns 1,
so between `ShowWindow` and the first `WM_PAINT` the window's surface held
whatever the compositor last had for it — and a window of child controls paints
in pieces as each child takes its turn. Separately, `CreateWindowExW` had to ask
for the pad's size in 96-DPI pixels, because a window has no monitor, and so no
scale, until it exists.

Fix: `pad_caption::cloak()` (`DWMWA_CLOAK`) around the first show — cloak,
`ShowWindow`, `RedrawWindow(RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN |
RDW_UPDATENOW)`, uncloak — so the first visible frame is a painted one.
`RDW_UPDATENOW` is what makes it synchronous. And `resize_to_logical()` runs
`AdjustWindowRectExForDpi` at the window's real DPI right after creation, so the
pad opens at its logical size rather than at 96-DPI pixels on a scaled display.

### The two panes disagreed about scroll bars

Symptom: 「スクロールバーのデザインがサイドバーとエディター上で統一されてないね」.

Root cause: two, and only the first was the obvious one. A `LISTBOX` and an
`EDIT` are drawn by different scroll bar theme classes by default. And once that
was settled, the panes still disagreed in the resting state: a multi-line `EDIT`
keeps its bar on screen whether or not there is anything to scroll, while a
`LISTBOX` takes its away — so a screenshot with a short memo showed a thin dark
thumb on the left and a wide light track with an arrow on the right, which is
exactly the picture the owner was looking at.

Fix: `SetWindowTheme(control, "Explorer", null)` on both `state.list` and
`state.body`, plus `LBS_DISABLENOSCROLL` on the list so it keeps its gutter too.
A side effect worth having: the list's rows no longer change width the moment a
fourth memo arrives.

Verified by measurement rather than by style: `pad_ui.rs` now reads
`window width − client width` on both controls in the two-pane shape — the
controls carry no border styles, so that difference is the bar and nothing else
— and asserts the two are equal and non-zero. Commenting out
`LBS_DISABLENOSCROLL` was run once to confirm the assertion fails without it,
then restored.

### Verification

`cargo fmt --all -- --check` clean. `cargo clippy` clean for every workspace
member; the only failures anywhere are `elidable_lifetime_names` and
`too_many_arguments` in `tools/ime-eval/src/ranking_comparison.rs`, which is the
owner's own untracked work and was not touched (it reaches `sakura-engine`
through a dev-dependency, which is why `-p sakura-engine` reports them).
`cargo test --workspace` all suites ok, 0 failures, renderer binary 129 → 134.
`cargo test -p sakura-renderer --test pad_ui -- --ignored
the_pad_splits_above_the_breakpoint_and_folds_below_it` ok. `git diff --check`
clean. 0 residual cargo, rustc, renderer, engine or test-runner processes.

Confirmed on screen at 96 DPI in the light theme, from `CopyFromScreen` captures
zoomed 6–8×: curves read as curves, the trash ribs are legible, the copy control
reads as two sheets, the faces sit at the weight of the `0字` / `保存済み` text
beside them, and both panes now show the same track, the same width, the same
arrow and the same thumb. Dark, Windows high contrast, and 144/192 DPI are still
unconfirmed on screen.

Learned: a scroll bar comparison is only valid between two controls in the same
state. The first three screenshots compared a list that had something to scroll
against an editor that did not, and the difference they showed — thumb versus
arrow — was mostly the disabled state, not the theme. Filling the editor until
it overflowed is what separated the two causes, and both turned out to be real.

Still not committed, for the reason recorded in the previous entries.

## 2026-08-25 — Sakura Pad: taskbar buttons, a clipped notice, a bold heading riding high, and a dead band under the list (#92, #91)

Symptom, four reports from the owner while using the installed build:

1. 「タスクバーに出さないことできないの？これは設定画面にも言えることだけど。」 — both
   the Pad and the settings sheet had their own taskbar buttons.
2. 「これわかりにくいよ」, over a red circle around 「Markdown をコピ」 — a notice cut
   off partway.
3. 「上に配置しすぎだし、ここだけ太文字だよ。保存済みもいらんでしょ」 — the memo title
   sat above the readings beside it, was the only bold thing in the row, and the
   row said 保存済み almost all of the time.
4. 「何この無駄な空白」 over an empty band between the last memo and the bottom bar,
   with 「レスポンシブデザインなのわすれないでね修正漏れしないように」.

Root causes, each a different one:

- Taskbar: Windows gives a top-level window a button when it asks with
  `WS_EX_APPWINDOW` **or** when it is unowned. Both windows were unowned.
  `WS_EX_TOOLWINDOW` is the other way out and was rejected: it shrinks the
  caption of a window that shows a title.
- Clipped notice: the status slot was a fixed `STATUS_WIDTH_96 = 108` logical
  px, sized for `10:27 同期済`, and `SS_ENDELLIPSIS` cut anything longer — even
  with a wide empty gap beside a short title. Separately, `status_message` was
  never cleared, so a one-off notice sat where the memo's own time belongs for
  the rest of the session.
- Heading: an `EDIT` draws its one line along the top of whatever rectangle it
  is given, while a `STATIC` with `SS_CENTERIMAGE` centres in one. The row was
  handed out whole, so only the title rode high. The bold came from
  `fonts.heading` (weight 600), which belongs to the band's own name, not to a
  field the writer types into.
- Dead band: a `LISTBOX` rounds its own height down to a whole number of rows
  unless it is told not to, and hands the remainder back as bare surface.

Fixes:

- `PadWindow::new` now takes an owner (the renderer's hidden host window) and
  drops `WS_EX_APPWINDOW`; `resize_to_logical` passes the matching ex-style to
  `AdjustWindowRectExForDpi`. The settings exe grows a hidden `WS_POPUP` owner
  of its own class `SakuraInputSettingsOwner` — a class of its own so the
  single-instance `FindWindowW` still finds the sheet and never the owner —
  destroyed in `run()` **after** the pump, never from the sheet's `WM_DESTROY`
  (destroying an owner destroys what it owns).
- `layout` takes `status_want`; the slot is measured against its actual text and
  grows leftward into the gap the title is not using, bounded by `TITLE_MIN_96`,
  with the character count keeping its place. `update_status` splits into
  `set_status` (a state, sticky) and `notify` (news, expiring after
  `NOTICE_MS = 4000` via `PAD_NOTICE_TIMER`). Notices were also shortened from
  sentences to phrases — 「コピーしました」, 「GitHub 未設定」 — because no window
  width makes a 20-character sentence fit beside a 120 px heading minimum.
- The meta row and the folded band both centre a `TEXT_LINE_96 = 22` band inside
  the row; the title takes `fonts.body`. Both shapes, because the same `EDIT`
  serves either side of the breakpoint — that was the 修正漏れ the owner warned
  about, and it was real: only the wide row had been fixed.
- `LBS_NOINTEGRALHEIGHT` on the list.
- `保存済み` becomes an empty status, so the row falls back to the memo's time.
  The save *failure* still speaks.

Verification: `cargo fmt --all -- --check` clean; `cargo clippy -p
sakura-renderer -p sakura-settings --all-targets -- -D warnings` clean;
`cargo test --workspace` 1,617 passed / 0 failed; the ignored
`the_pad_splits_above_the_breakpoint_and_folds_below_it` passes on the
interactive desktop with new assertions for the taskbar rule
(`WS_EX_APPWINDOW` clear, `WS_EX_TOOLWINDOW` clear, `GW_OWNER` present), for the
notice fitting its slot and expiring, and for the list reaching the bar;
`git diff --check` clean; 0 residual cargo/rustc/renderer/engine/test-runner
processes. Confirmed on screen at 96 DPI light in both shapes: the title is
centred and unbolded, the row shows the time instead of 保存済み, and the list
runs to the bar.

Learned: three different Win32 controls were vertically centring three different
ways in one row, and the row looked broken in exactly one place. When a single
element in a row looks misaligned, suspect the control class before the
arithmetic — the placement was correct the whole time. And a responsive layout
has two branches: fixing the one in the screenshot is half the fix.

Still not committed, for the reason recorded in the previous entries.
Dark, Windows high contrast, and 144/192 DPI remain unconfirmed on screen.

## 2026-08-25 — Sakura Pad: two scroll bars borrowed from Explorer, two grounds in one row, and a row reporting the resting state (#92, #91)

Symptom, three reports from the owner over screenshots:

1. 「スクロールバーなんだけど、それぞれに色をあわせて、もっと細くしてよ」 over both
   panes' scroll bars.
2. 「この部分のデザインが統一されてないね」 over the header rows of the two shapes,
   clarified as 「色だよ色」.
3. 「時刻のも表示しなくていいよ」.

Root causes:

- Scroll bars: a window's scroll bar is drawn by the theme and sized by
  `SM_CXVSCROLL`. `SetWindowTheme(hwnd, "Explorer", NULL)` — which is what the
  pad was doing — only picks which theme class draws it; there is no per-window
  colour or width. So the two panes carried a control the pad had no say over,
  in a grey that belonged to neither pane's ground.
- Two grounds: `layout` gave the whole wide editor column, head row included,
  to `paper`, and `WM_CTLCOLOR*` painted the head row's three controls on
  `paper` while the folded shape put the same two readings on the header band,
  which is `surface`. Sampled from my own captures: `#FFFDFB` wide against
  `#F7F6F4` folded.
- The time: `status_line` fell back to the memo's own last-changed time, which
  the memo's list row already carries, and to 「新しいメモ」 for a memo that is
  visibly new.

Fixes:

- New `crates/sakura-renderer/src/pad_rail.rs`: a `SakuraInputPadRail` child
  per pane, 10 logical px wide with a 4 px rounded thumb, track painted in the
  pane's own ground (`surface` for the list, `paper` for the body), thumb
  `border` at rest and `annotation` under the pointer. `WS_VSCROLL` and
  `LBS_DISABLENOSCROLL` are gone from both panes, and the strip is carved out
  of the pane rather than added beside it, so a document that grows past the
  view does not move the words.
- The rail keeps no scroll position: it reads `LB_GETTOPINDEX`/`LB_GETCOUNT`/
  `LB_GETITEMHEIGHT` or `EM_GETFIRSTVISIBLELINE`/`EM_GETLINECOUNT` plus the
  pane's own font metrics every time it paints, so the wheel, a key, a caret
  leaving the view and a memo being added all move the thumb with no second
  copy of the state to disagree.
- The head row stands on `surface` in both shapes: `paper` now starts below
  `meta`, the `WM_CTLCOLOR*` special case is gone, and the two owner-drawn
  buttons in the row lost their `paper` ground too.
- `status_line` returns only what there is to report. A `status_want` of zero
  now means no slot at all, so the memo's name takes the width instead of
  standing beside a reserved blank.

One real bug on the way: the pane subclass probes the pane to see whether it
scrolled, and `line_height` asks a text pane for its font — `WM_GETFONT` came
straight back into the subclass, which probed again, and the renderer overflowed
its stack before the window appeared. Listing the probe's messages is a list to
keep in step with the probe; a thread-local `PROBING` flag covers whatever it
asks. One thread owns every pad window, so that is the whole of the exclusion
needed.

Verification: `cargo fmt --all -- --check` clean; `cargo clippy -p
sakura-renderer --all-targets -- -D warnings` clean; `cargo test --workspace`
92 test binaries, 0 failed; the ignored
`the_pad_splits_above_the_breakpoint_and_folds_below_it` passes on the
interactive desktop with new assertions that neither pane has a system scroll
bar left (`scroll_gutter == 0` for both), that each rail stands against its
pane at the pad's own width and thinner than `SM_CXVSCROLL`, and that the
notice's slot is given back entirely when it expires; `git diff --check` clean;
0 residual renderer/cargo/test-runner processes. Confirmed on screen at 96 DPI
light: the head row samples `#F7F6F4` in both shapes, each rail samples its own
pane's ground, and a short window shows the list's thumb at the right length
while the body — which fits — shows none.

Learned: `SetWindowTheme` reads like a way to restyle a control and is not one;
it selects a theme class, and anything the theme does not expose is not
settable. When the design calls for a control the platform draws its own way,
the choice is to accept the platform's or to own the drawing — there is no
third setting to find.

Still not committed, for the reason recorded in the previous entries. Dark,
Windows high contrast, and 144/192 DPI remain unconfirmed on screen.

## 2026-08-25 — Pad: ホイールが効かない／エディタの残像（#92、#91）

- 症状1: 独自スクロールレールへ置き換えた後、一覧・本文のどちらもマウスホイールでスクロールしなくなった。
- 根本原因1: `LISTBOX` と `EDIT` のホイール処理は、スクロールバーを持っている場合の実装に含まれる。レール導入時に `WS_VSCROLL` を外したため、コントロール自身のホイール処理も一緒に失われた。レール側は自分の 10 px の帯の上にポインタがあるときだけ `WM_MOUSEWHEEL` を転送していたので、ペイン本体の上では誰も受け取らなかった。
- 修正1: `crates/sakura-renderer/src/pad_rail.rs` のペイン subclass (`watched`) で `WM_MOUSEWHEEL` を自分で処理する。`SPI_GETWHEELSCROLLLINES`（`WHEEL_PAGESCROLL` は表示行数、0 はスクロールなし）を読み、`WHEEL_DELTA` 未満の端数はスレッドローカル `CARRY` に持ち越して高分解能ホイールでも取りこぼさない。純関数 `notches(turned, carried)` に切り出し、1 ノッチと 1/3 ノッチ×3 の 2 テストで固定した。
- 症状2: 本文で改行して文字が下へずれると、しばらく残像が残る。
- 根本原因2: 本文は罫線のパターンブラシの上に `TRANSPARENT` で描かれる。`EDIT` は挿入位置より下を `ScrollWindow` 相当でずらすため、罫線ごと動いた画素と新しく描かれた行が二重になる。既存の再描画フックは「先頭表示行が変わったとき」だけ全体を再描画していたので、先頭行が動かない編集（改行・挿入・削除）では発火しなかった。
- 修正2: `crates/sakura-renderer/src/pad.rs` の `body_proc` の監視対象を、先頭表示行だけから (`EM_GETFIRSTVISIBLELINE`, `EM_GETLINECOUNT`, `WM_GETTEXTLENGTH`) の 3 値へ広げた。編集は必ずこのどれかを変えるのに対し、キャレット移動だけでは変わらないので、打鍵ごとの無駄な全体再描画にはならない。プローブ自身が同じ subclass へ戻る再帰は、レールと同じスレッドローカル `PROBING` フラグで止める（メッセージ ID の列挙は取りこぼす）。
- 検証: `cargo fmt --all -- --check` 成功、`cargo clippy -p sakura-renderer --all-targets -- -D warnings` 成功、`cargo test --workspace` 失敗 0、`pad_ui` の対話デスクトップ用 ignored テスト成功、`git diff --check` 成功、残存プロセス 0。実プロセスでの実測は、本文 40 行に対し先頭表示行 0 →（3 ノッチ下）9 →（1 ノッチ上）6、一覧は 6 行すべてが収まるため 0 のまま（正しい）。画面キャプチャで、先頭に改行 3 つを挿入した直後と行途中への挿入直後のいずれにも残像・罫線の二重描画がないことを確認した。
- 学び: スクロールバーを外すことは、見た目だけの変更ではなく、そのコントロールのホイール処理を外すことでもある。そして、背景を `TRANSPARENT` で描くコントロールでは「スクロールした」だけを再描画の合図にすると、内容が動く編集を取りこぼす。
- commit: 未コミット（#92 のフェーズ 2 として作業ツリーに保持）。

## 2026-08-25 — 1.0.25 リリース（Sakura Pad、GitHub 同期は未実装）（#91、#92、#93）

- 作業: owner 指示により、GitHub 同期が未実装のまま Sakura Pad を 1.0.25 として切り出した。対象は「作業ツリー全部」、公開範囲は「コミット＋タグ push まで」で、`scripts/publish-release.ps1` は実行していない。
- 設計判断: 作業ツリーには #91／#92 の Pad と #93 の ranking 比較ツールという独立した2系統が入っていたが、owner が全部を1コミットにする選択をしたため分割せず、CI を止めていた `tools/ime-eval/src/ranking_comparison.rs` の clippy 2 件（elidable lifetimes、引数 8 個）を除外ではなく修正で通した。`ranking_view` は 8 引数から `&RankingSnapshotObservation` + 4 引数へまとめ、`metadata_status()?` を関数内へ移して `Result` を返す形にした。
- 版の面: `Cargo.toml`、`Cargo.lock`（`cargo check` で再生成）、`installer/setup.iss` の `AppProductVersion` と `AppVersionedDir`、`.github/workflows/release.yml` の workflow_dispatch 既定値の4か所。`docs/release-notes-v1.0.25.md` を 1.0.24 と同じ構成で新規作成し、`README.md` に `## Sakura Pad（ローカルメモ）` を追加して「GitHub 同期はこのリリースには含みません」を明記した。
- 検証: `cargo fmt --all -- --check` 成功、`cargo clippy --workspace --all-targets -- -D warnings` 成功、`cargo test --workspace` 失敗 0、`git diff --check` 成功、`ci/dep-policy.ps1 -SelfTest` と本体（73 packages、違反 0）成功、`ci/release-workflow-policy.ps1 -SelfTest` と本体（reviewed action 7 件）成功、`ci/check-process-clean.ps1` で残存プロセス 0。
- commit: d8db8d4e482d4232220b566164dbf236cb312350 / tag `v1.0.25` を origin へ push 済み。push により Release candidate、CI、Installer の3ワークフローが起動した。
- 学び: リリース単位を owner が「作業ツリー全部」と決めた場合、CI ゲートは同梱される全ツールへ及ぶ。無関係に見える調査用ツールの lint も、リリース作業の一部として先に片づける必要がある。

## 2026-08-25 — v1.0.25 が CI で落ちた原因と 1.0.26 としての公開（#92、#93）

- 症状: ローカルでは全ゲート green だった v1.0.25 が、`Release candidate` ワークフローの Test 段階で `checked_in_issue93_snapshots_match_manifest_and_report_fingerprints` だけ失敗した。manifest が pin する `c81d3e78…` に対し、runner 上の計算値は `8bc2ed95…`。
- 根本原因: `core.autocrlf` の下で、#93 の `eval/corpus/behavioral/ranking-comparison-issue93/fixture.json` と `eval/baselines/ranking-comparison-issue93/*` が CRLF で checkout され、manifest が pin している SHA-256 と一致しなくなった。ローカルの作業ツリーは LF なので通っていた。手元で LF/CRLF 両方の hash を計算し、CRLF 版が runner の値と一致することで確定した。
- 修正: `.gitattributes` に `/eval/corpus/** text eol=lf` と `/eval/baselines/** text eol=lf` を追加した。`/data/llm-detail-targets/**` などの manifest 拘束ディレクトリに元からある扱いと同じで、`git ls-files --eol` で index／worktree／attr がすべて lf になることを確認した（commit 0b11095）。
- 版の扱い: owner 判断により、公開済みタグ v1.0.25 は動かさず、同じ内容を 1.0.26 として切り直した。`Cargo.toml`、`Cargo.lock`、`installer/setup.iss`、`release.yml` の既定値を更新し、`docs/release-notes-v1.0.25.md` を `-v1.0.26.md` へ rename した（workflow が版に一致するノートを要求するため）。v1.0.25 は失敗タグとして残る（commit d0d52c9、tag v1.0.26）。
- 検証: v1.0.26 の `Release candidate`、`CI`、`Installer` の3ワークフローが success。artifact の installer は 24,670,077 bytes、SHA-256 `dadc729ed5c8b6622ecc2105556b117a6647ca44519e9251a73799a59e6114fb`、`Get-AuthenticodeSignature` は `NotSigned`（owner 承認済みの未署名リリース）。`release-manifest.txt` の sha256／size が実体と一致することを確認した。
- 公開: owner の明示承認を得て、`gh release create --draft` → 添付2件を再ダウンロードして hash 一致を確認 → `--draft=false` の順で公開した。読み戻しは `isDraft=false`、`isPrerelease=false`、`publishedAt=2026-08-25T10:35:50Z`、assets は `sakura_setup.exe` と `release-manifest.txt` の2件のみ。`scripts/publish-release.ps1` は署名検証を必須にするため、未署名リリースでは使えず、同じ検査手順を `gh` で手動実行した。
- 学び: manifest が生バイトの SHA-256 を pin するデータを追加したら、その時点で `.gitattributes` の eol 指定も一緒に入れる。Windows の `autocrlf=true` では、ローカルの緑と CI の緑は同じ意味ではない。

## 2026-08-25 — Pad: 初回起動のメモが一覧に出ない（#92）

- 症状: 初回起動時、Pad は空の一覧と開いた編集面で出る。そこへ直接入力すると本文は残るが一覧に行が現れず、見出しも「メモ帳（0）」のままで、保存できていないように見える。実際は保存されており、後から「新規メモ」を押すと先に書いたメモが一覧に現れた（ownerの実機報告）。
- 根本原因: `crates/sakura-renderer/src/pad.rs` の `PAD_EDIT_TIMER` は `capture_controls()`（必要なら `document.entry()` でメモを新規作成する）と `publish()` を呼ぶが、`refresh_list()` を呼んでいなかった。初回のメモは「新規メモ」ではなく打鍵で生まれるため、行の再構築が起きる契機が他になく、並べ替え・検索・新規作成など別操作までずっと一覧が空のままになる。
- 修正: `sync_rows()` を追加し、capture が document を変えたときだけ `pad_list::rows()` を再計算して、行集合または順序が実際に変わった場合に `refresh_list()` する。毎回の全再構築を避けるのは、`LB_RESETCONTENT` が一覧のスクロール位置を戻すため。行のラベルは owner-draw が document から直接描くので、タイトル打ち直しの見た目は既存の再描画で足りる。
- 検証: `crates/sakura-renderer/tests/pad_ui.rs` に `typing_into_a_fresh_pad_puts_the_memo_in_the_list` を追加。隔離した `LOCALAPPDATA` で実 renderer を起動し、「新規メモ」を押さずに本文へ入力して行数と見出しの件数を待つ。修正を一時的に外すと FAILED、戻すと ok になることを確認した（テスト単体 3 件成功、`cargo test --workspace` 失敗 0、fmt・clippy 成功、残存プロセス 0）。
- 補足: 検索はタイトルと本文の両方に対する部分一致で、`fold` は `to_lowercase` のみ。全角半角・ひらがなカタカナの正規化はしていない。
- commit: ac65c90（未リリース。1.0.26 のインストール済みビルドにはこの修正は入っていない）。
- 学び: 「作成は明示操作」という前提で書いた UI に、暗黙の作成経路（打鍵で生まれる最初の1件）が1つでもあると、その経路にだけ再描画が抜ける。空状態はテストの seed で隠れやすいので、seed しない初回状態のテストを別に持つ。

## 2026-08-25 — 1.0.27 リリース（Pad 初回メモ修正）（#92）

- 内容: ac65c90（初回起動の Pad で最初のメモが一覧に出ない修正）をリリース化した。版は `Cargo.toml`、`Cargo.lock`、`installer/setup.iss`、`release.yml` 既定値の4か所、ノートは `docs/release-notes-v1.0.27.md`。
- 検証: fmt、`clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（失敗 0）、`git diff --check`、dep-policy、release-workflow-policy すべて成功。`Release candidate`／`CI`／`Installer` の3ワークフローも success。
- artifact: `sakura_setup.exe` 24,681,776 bytes、SHA-256 `ad7624aba2cd9aa52cd6dff380de412ae91fb760b2d88421e0ede231bd37c4ab`、`NotSigned`（owner 承認済みの未署名リリース）。manifest の sha256／size と一致。
- 公開: draft 作成 → 添付2件を再ダウンロードして hash 一致を確認 → `--draft=false`。読み戻しは `isDraft=false`、`isPrerelease=false`、`publishedAt=2026-08-25T11:54:08Z`、assets 2件。
- commit: 50659ae / tag `v1.0.27`。

## 2026-08-25 — Pad: 無題プレースホルダ、Ctrl+A、キャプションのアイコン除去（#92）

- 症状: owner の実機報告3点。(1) 初回起動の Pad はタイトルが空欄のままで、そのメモが何と呼ばれるのか画面に出ない。(2) Ctrl+A が効かず全選択できない。(3) タイトルバー左端のアイコンが不要。
- 根本原因: (1) 空欄のヒントは検索欄だけに実装されていた（`search_proc` / `SEARCH_PROC`）。(2) 素の `EDIT` は Ctrl+A を実装せず、`IsDialogMessageW` も処理しない。誰も処理しないため打鍵が何も起こさなかった。(3) 自前アイコンを持たないウィンドウクラスは Windows の既定プレースホルダで描かれる。ジェスチャで呼ぶ Pad には並んだウィンドウ列から見分けられる必要がなく、製品アイコンを入れても冗長になるだけ。
- 修正: (1) ヒント機構を control id 引きへ一般化（`PLACEHOLDER_PROC` / `placeholder_proc` / `install_placeholder` / `placeholder_text`）し、タイトル欄へ `無題` を **描画**する。テキストにすると実タイトルとして保存され、無題メモが恒久的に `無題` という名前になるため採らない。同じ理由で検索欄もテキストにしない（フィルタが読むため）。本文欄は意図的にヒントなし。`TITLE_PLACEHOLDER == pad_list::UNTITLED` をテストで固定。(2) `dialog_navigation()` が `IsDialogMessageW` より前に `select_all()` を試し、`EM_SETSEL(0, -1)` を送って打鍵を飲み込む（0x01 制御文字がテキストへ入るのを防ぐ）。判定は純関数 `selects_all()` に分離。Ctrl+Alt+A は AltGr+A なので除外、一覧も除外（1件編集の Pad で全メモ選択は無意味）。(3) `pad_caption::hide_icon()` が `WS_EX_DLGMODALFRAME` + `SWP_FRAMECHANGED` と `ICON_BIG`／`ICON_SMALL` の null `WM_SETICON` を行う。システムメニューは Alt+Space と右クリックで従来どおり。`CaptionIcons`／`icons()`／DPI 変更時の再適用は削除。
- 検証: 純関数の unit test、実 renderer を使う `pad_ui` テスト（拡張スタイル、両アイコンが null、タイトル欄の text が空のまま）、空の Pad の実画面スクリーンショットで `無題`／`検索` 表示とアイコン無しを目視、実行中の Pad への実 Ctrl+A 打鍵で `selection 0..251`。fmt、`clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（失敗 0）、`git diff --check` 成功。
- 落とし穴（記録）: `GetWindowDC` + `BitBlt` は DWM が描くキャプションを正しく取れず、アイコン除去後も古いプレースホルダや黒帯を返した。キャプションの見た目確認は `CopyFromScreen` による実デスクトップ撮影で行うこと。また別プロセスから合成キーを送る前に `AttachThreadInput` + 合成 Alt で foreground lock を外し、対象が実際に前面かを確認してから送らないと、キーは前面の別アプリ（Chrome）へ入る。
- commit: 6d07794（未リリース。1.0.27 のインストール済みビルドにはこの3点は入っていない）。
- 学び: 「ヒント＝プレースホルダ」を実装するとき、テキストとして入れてよいかは欄ごとに違う。検索欄は読み取り側が壊れ、タイトル欄は保存側が壊れる。どちらも描画で解決するのが正しく、同じ機構を id 引きで共有できる。

## 2026-08-25 — 1.0.28 リリース（Pad の無題・Ctrl+A・キャプション）（#92）

- 内容: 6d07794 をリリース化した。版は `Cargo.toml`、`Cargo.lock`、`installer/setup.iss`、`release.yml` 既定値の4か所、ノートは `docs/release-notes-v1.0.28.md`（1.0.27 のノートを rename）。
- 検証: fmt、`clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（失敗 0）、`git diff --check`、dep-policy（73 packages）、release-workflow-policy（7 action 参照）すべて成功。commit 361a330 / tag `v1.0.28`。
- CI のフレーク2件（重要）: `Release candidate` と `Installer` は1回目で success。`CI` の `Build and test` だけが2回続けて、しかも**別々のテスト**で落ちた。
  - 試行1: `sakura-core` の `raw_multi_pass_core_path_fits_128_kib_thread_stack` が `STATUS_STACK_OVERFLOW`（0xc00000fd）。本番の worker stack は 160 KiB で、このテストは変換ホットパスを 128 KiB に収まるか検査する境界テスト。debug ビルドのマージンが薄い。
  - 試行2: 試行1では前段で止まって実行されていなかった `Sandbox access (AppContainer)` の `the_pipe_is_reachable_from_a_real_appcontainer_token` が `UntrustedServer` で失敗。AppContainer 側はパイプを開けたが、verified connect が親 engine を path/token ポリシーで拒否した（fail-closed 側）。
  - 試行3: 全 step success。
  - いずれも今回の差分が触っていない領域。前回 green だった 1.0.27 の CI（fb626d6）と比べ `sakura-core`／`sakura-engine` は1バイトも差がなく、toolchain も同一（1.96.0 ac68faa20）。つまりコード差分ではなく runner 環境依存。
  - ローカルでは AppContainer テストは「本番の well-known パイプを既存の engine が持っている」ため実行拒否になり、インストール済み engine を止めない限り再現確認できない。
- artifact: `sakura_setup.exe` 24,682,898 bytes、SHA-256 `959fc5db53c73bfd3bc648991465ff6604af2a2bcc13e2bed1ed6519055f888d`、`Get-AuthenticodeSignature` は `NotSigned`（owner 承認済みの未署名リリース）。`signing-status.txt` は `unsigned-owner-approved`。manifest の sha256／size と一致。
- 公開: draft 作成 → 添付2件を再ダウンロードして hash と manifest の一致を確認 → `--draft=false`。読み戻しは `isDraft=false`、`isPrerelease=false`、`publishedAt=2026-08-25T13:11:15Z`、assets 2件。
- 未処理: 上記フレーク2件は別 Issue にして原因調査する。128 KiB テストは debug ビルドでの実 headroom を測ってから閾値か対象を決める。AppContainer は `UntrustedServer` に至った path/token 判定の環境依存要因を特定する。
- 学び: 段階的に止まる CI job では「1回目に落ちなかった step は、通ったのではなく実行されていない」ことがある。再実行で別のテストが落ちたときに新しい回帰と誤認しないため、step 単位の conclusion を見る。
