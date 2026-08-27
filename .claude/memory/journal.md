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

## 2026-08-26 — 同音異義語のカバレッジ欠落と TOP-1 順位（#94）

- 症状: owner 報告「対案って入力して大安が TOP-1 ってどうなのよ、使用頻度低いでしょ。他にもありそうだよね」。たいあん の第1候補が 大安 になる。実際に調べると同種の問題は他にもあり、しかも原因が3層に分かれていた。
- 根本原因（3つ、独立）:
  - (1) ビルド時 trim の欠落。`mozc_trim` の `TrimPolicy.max_surfaces_per_reading = 12` が、1読みあたり distinct surface を12で打ち切っていた。`mozc-system.tsv` から独立に数え直すと、**963読みで12を超え、13番目以降の 10,781 surface** が同梱辞書へ入っていなかった（旧 trim report の `capped_surfaces` と一致）。きかん の 気管（コスト順15位）・旗艦（13位）・季刊（20位）、かん の 関（16位）・環（17位）、こう の 光（14位）・行（17位）は、辞書に載る前に消えていた。
  - (2) 実行時 surface 予算の欠落。`conversion.rs` の `MAX_DICTIONARY_SURFACES_PER_READING` も別に12で、辞書が持っている語にも到達しきれなかった。きゅう は12枠を使い切って稀な人名漢字 邱 を出し、数字表記 9 を落としていた（9 はコスト順5位で、ビルド時 trim では**残っていた**。落としたのは実行時側）。つまり同じ「12」が2か所にあり、症状が似ているので一方だけ直すと不完全になる。
  - (3) 同音異義語の価格。上流 Mozc の `word_cost` が、8読みで使用頻度の低い表記を先頭に置いていた（たいあん、がいちゅう、きんそく、こうねつ、とうばん、どうてん、しれい、たいせき）。
  - 付随: 平仮名1字を先頭に切り出す分割候補（た慰安、き澗、が意中）が上位に混ざる。
- 修正:
  - `TrimPolicy.max_surfaces_per_reading` を `Option<usize>` にし、同梱 policy を `None` にした。同時に **`legacy_row_evidence_cap: 12` を別概念として凍結**した。legacy evidence は「出荷済み行がどの経路で入ったか」の来歴であり、カバレッジが増えたからといって過去の分類が変わってはいけない。CLI フラグは足さない。report は schema 2→3。
  - `conversion.rs` の12を2定数へ分離した。`BASE_DICTIONARY_EDGES_PER_READING = 12`（従来どおりの baseline edge 数、POS 変種の経路を保つ）と `MAX_DICTIONARY_SURFACES_PER_READING = MAX_CANDIDATES`（=18、protocol が運べる上限）。baseline を動かさずに surface 多様性だけ増やす。
  - `drop_kana_fragment_prefix_splits`（コスト窓 1,500、お／ご／み は接頭辞として除外）を後段フィルタへ追加した。
  - `data/conversion-priorities.tsv` に6行を追加。価格は同ファイル既存の数字行と同じ規約（対象の lattice total を現行首位のちょうど60下に置く）。頻度判定は**言語的判断であって corpus 計測ではない**ことを `#` コメントに明記した（外部頻度 corpus は既存の手当て層と同様に参照しない）。がいちゅう（外注＞害虫）と しれい（指令＞司令）が最も際どく、その旨も記録した。
  - しれい／たいせき だけは `data/curated-general-details.tsv` 側で価格を直した（下記の落とし穴）。
- 検証:
  - 辞書再ビルド: `deterministic_repeat: true`、623,291 entries、30,906 detail records、`system.dic` 47,292,360 bytes、SHA-256 `95e98dfffed1b10518015eb20ea337e61daaf1df12ab6f693ce7c942a0c177ff`。trim report は `capped_entries` 11,451→0、`capped_surfaces` 10,781→0、`surface_cap_rescued_entries` 6,563→18,014、`output_entries` 448,278→459,729。
  - 実辞書に対する csnap（`--it-bias on`）で8読みすべて TOP-1 を確認し、設計どおり差が60（たいあん: 対案 7,144 / 大安 7,204、たいせき: 体積 6,343 / 堆積 6,403）。
  - カバレッジ: きかん が 旗艦(6)・気管(7) を出し、きゅう は18候補で 9(15)・窮(13)・久(17)・亀(18)、かん は18候補で 関(16)・環(18)。
  - `shipped_dictionary_ranking.rs` へ `#[ignore]` 回帰テスト3件（再価格8読みの TOP-1、到達可能同音異義語のページ内存在、かな断片分割の不在）。
  - `phase3-editing.snap` の候補数 14→16。実行時 surface 予算が広がり、かな が仮名・カタカナの literal も出せるようになったため。プローブで中身（仮名／加奈／候補03..14／かな／カナ）を確認してから更新した。
  - fmt、`clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（1,630 passed / 0 failed / 77 ignored）、ignored の shipped-dictionary スイート、`git diff --check` すべて成功。
- 落とし穴1（overlay の所有権）: `dictc` が exit 2 で `reviewed detail source no longer matches the final entry for reading 'しれい' and surface '指令'`。`conversion-priorities.tsv` は overlay 列の**最後**にあり、同じ lattice edge を後勝ちで**置換**する。一方 `curated-general-details.tsv` の審査済み説明は `(reading, surface, left_id, right_id)` で `word_cost`／`prediction_cost`／`flags` の一致を fail-closed で要求する。つまり説明を持つ edge を priorities 側で再価格すると必ず壊れる。対処は「1 edge に owner は1つ」。該当2行を priorities から外し、説明ファイル側のコスト列を直し、理由を `#` コメントへ残した。
- 落とし穴2（prediction 不変条件の書き方）: `every_row_stays_reachable_from_prediction` が 禁則 で落ちた。テストが全行に `predict` を無条件要求していたが、Mozc importer はもともと `word_cost > 6,000` の語を prediction へ出さない。対案(6,078)・禁則(6,776) は再価格後もその線の上にあり、上流でも非 predictive だった。テストをコスト基準の規則へ広げて `every_row_keeps_the_prediction_status_its_own_cost_earns` に改名し、62行すべてで違反0を確認した。
- 128 KiB stack テストの実測（2026-08-25 の未処理項目への回答）: `cargo test --workspace` 1回目で `raw_multi_pass_core_path_fits_128_kib_thread_stack` が `STATUS_STACK_OVERFLOW`。今回の変更が `DictionaryEdgeBudget` の配列を `[u32; 12]`→`[u32; 18]`（+24 bytes／frame、live な frame は2つ）にしているため疑ったが、**単体では 32 KiB でも通り**（32/48/56/64/72/80/96/112/120/128/144/160 KiB すべて exit 0）、`-p sakura-core --lib` 全体も3回連続で overflow 0、`--workspace` 再実行も 0。したがって overflow は frame 深さでは説明できず、この差分が原因でもない。debug ビルドの headroom は薄いどころか実測で4倍以上あり、閾値を下げる話ではない。原因は別（環境／タイミング依存）として残る。
- 既知の残渣（許容）: た遺跡 は 1,500 窓の内側に残り たいあん の4位（体積／堆積／退席の下）。ごせん の 五線 は辞書には入っているが実行時 n-best 窓で落ちる。どちらも別問題。
- commit: 191ca67（branch `fix/94-homophone-coverage-and-ranking`、未リリース。1.0.28 のインストール済みビルドにはこの変更は入っていない）。
- 学び: 同じ数値定数が「ビルド時」と「実行時」の両方にあると、症状が同一に見えて片方だけ直しても半分しか治らない。カバレッジの数字は report を信じず原本（`mozc-system.tsv`）から数え直すこと。今回それで「きゅう の 9 は実行時側の損失」と判明し、テストの doc コメントの誤りを直せた。

## 2026-08-26 — 候補数上限の実測（#95、単漢字取り込みの前段）

- 依頼: owner が ATOK の「ひ」で 2/210 のスクリーンショットを示し「ATOK に仕様をあわせようか」。スコープは4件（単漢字辞書の取り込み／異体字の注記／候補数上限の引き上げ／学習コストの lattice 注入）を全採用し、進め方は「先に実測してから決める」を指定。
- 前提の訂正（重要）: `MAX_CANDIDATES = 18` は**表示上限ではない**。`ConversionOptions::max_candidates` は `search_n_best(..., wanted)` へそのまま渡るので、背後に大きな候補プールがあってページングで見せられる、という構造ではない。上限は探索量そのものを決める。したがって「上限を上げる」は表示の話ではなく 10 ms の worker 予算に対する価格の話になる。
- 計測器: `tools/candidate-sweep`（nested Cargo workspace、registry 依存ゼロ、出荷 crate へリンクしない）。実辞書イメージに対し (読み × 上限) ごとに候補数・1文字 surface 数・lattice nodes・states pushed・探索終端・min/median/p95・TOP-1 を TSV で出す。`sakura-core` に `research-wide-candidates` feature を足し、sweep のときだけ `MAX_CONVERSION_CANDIDATES` を 512 にする（出荷ターゲットでは無効）。
- 決定的だったのは `terminal` 列。`exhausted` の行は辞書を使い切っており、**どんな上限でもこれ以上は増えない**。これがあるので「上限が足りないのか、辞書が足りないのか」を推測せずに切り分けられた。
- 実測（40読み × 上限 9〜512、`--it-bias on`、repeats 25 / warmups 5）:
  - **TOP-1 は全読み・全上限で完全に不変**。上限引き上げ自体に順位回帰リスクはなく、純粋な追加である。
  - **1文字読みは上限に非依存**（p95 14〜36 µs、20 µs スケールなのでこの幅は計測ノイズ）。しかも上限 512 で全て `exhausted`: か 24 / き 31 / こ 25 / し 23 / ひ 19 / て 2。つまり**上限を上げても「ひ」は 19 のまま**で、ATOK の 210 との差は 100% 単漢字辞書のカバレッジ差である。上限の話と単漢字の話は独立していた。
  - **コストは上限ではなく読み長で決まる**。読み長クラス別 p95 最大（µs）: 1文字 23→23、2-4文字 215→389、5-8文字 581→1,002、9文字以上 1,748→3,739（いずれも上限 18→108）。上限 256 で 9文字以上が 10,085 µs に達し、states は 162 以上で 65,536 の budget に張り付く。
  - **長文追加プローブ**（26/52/78/104文字）では、上限は倍率ではなくほぼ**定数オフセット**として効く。104文字で 18→3,197 µs、108→4,952 µs（+1.8 ms）。
  - 辞書の飽和点: ひ 19 / たいあん 21 / きかん 21 / かん 51 / しょう 62 / こう 80 / ゆうき 166。512 まで候補を出し続けるのは文長読みだけで、中身は分割違いのゴミ。
- 結論: 一律上限は形が間違っている。短い読みは上限を大きくしてもタダ、長い読みは大きくしても価値がない。読み長に応じた可変上限が実測に合う。
- 副産物として見つけた既存問題: 長文の遅延は約 30 µs/文字で伸びるので、`MAX_PREEDIT_BYTES = 1536`（ひらがな512文字）の最大長は**現行の上限 18 でも約 15 ms** に外挿され、10 ms 予算を超える。今回の変更が作る問題ではなく既存の穴。Issue #95 に記録した。
- 自分が壊していたもの: `cargo check -p sakura-core --features research-top32` が E0425 で失敗していた。#94 で `MAX_DICTIONARY_SURFACES_PER_READING` を `MAX_CANDIDATES` 基準にしたが、その import は `#[cfg(not(feature = "research-top32"))]` で feature 時に消えていた。既定ビルドとテストは feature を通らないので誰も検出できない壊れ方。`MAX_CONVERSION_CANDIDATES` 基準へ直した（これは意味的にも正しい: 上限を上げるビルドは読みあたりの辞書 surface 幅も一緒に広げないと、増えた枠が同音異義語ではなく複数形態素の経路で埋まり、sweep が測りたいものが測れない）。3構成すべて `cargo check` clean を確認。
- 検証: fmt、`clippy --workspace --all-targets -- -D warnings`、sweep 側 fmt/clippy（`--features wide` あり・なし両方）、`cargo test --workspace`（476 passed / 0 failed / 28 ignored、exit 0）、`git diff --check`。cargo／rustc の残存プロセスなし（残っていたのは実環境で稼働中の `sakura_engine` 等）。
- commit: a8e5a4e（branch `feat/95-single-kanji-and-candidate-cap`、出荷挙動の変更なし）。
- 学び: 「上限」と名前がついた定数を、表示上限だと決めつけて設計を進めない。この case では探索上限だったので、ページングで見せる案は最初から成立しなかった。そして探索終端を計測項目に入れておくと、「足りないのは予算か在庫か」を推測せずに分離できる。ATOK との差が単漢字辞書由来だと確定したのは、上限を 512 まで振っても `exhausted` が動かなかったからである。

## 2026-08-26 — 単漢字辞書の取り込みと変換後追加（#95、commit 8983139）

- 症状/課題: a8e5a4e の実測で「候補数上限を上げても `ひ` は 19 件で `exhausted`」と確定した。足りないのは探索予算ではなく在庫であり、その在庫は Mozc が別ソース（`src/data/single_kanji/single_kanji.tsv`／`variant_rule.txt`）に持ち、`rewriter/single_kanji_rewriter` で**変換後に追加**している単漢字だった。
- 設計判断（重要）: 単漢字を lattice edge にしない。`こう` だけで 315 文字あるため、edge 化すると1モーラの読みで `MAX_LATTICE_NODES = 32_768` を食い潰し、さらに単漢字を跨ぐ全経路のコストを動かす。Mozc と同じ「ランキング済みリストの末尾に追加する」位置に置いた。この判断により、TOP-1 も既存の並びも構造的に動かない。
- 実装:
  - `crates/dictc/src/single_kanji.rs`（新規）— 2ソースを sorted lookup へコンパイル。解釈できない行は skip ではなく error。
  - `crates/sakura-core/src/dictionary.rs` — optional table 4本（`SKIX`／`SKRD`／`SKCH`／`SKVR`）。reader は未知タグを読み飛ばすので **version bump 不要**（`BNDR` と同じ経路）。昇順厳密・span 範囲・UTF-8・scalar 妥当性・padding ゼロ・関係コード復号を fail-closed 検証。
  - `Converter::append_single_kanji` — 最終 sort の後段。空いた枠だけを埋め、既にランクインしている文字は重複追加しない。コストは**ランキング全体の上限より上**に置くので、後段で再 sort されても末尾から上がってこない。cross-commit bridge は anchor／transfer の両方で `path_evidence().is_system_only()` を要求するため、追加行は最初から対象外。literal-policy 経路には付けない。
  - 注記: 異体字規則がある文字は `異体字（高）` のように関係名＋元字、他は `単漢字`。
- 上流ソースの既知欠陥2件: `はｎ`（`はん` の誤字。判は正しい読みにも載っているので損失なし）と `びん(表外)`（表外マーカーが読みに混入し入力不能）。`MAX_REJECTED_READINGS = 2` で許容し、**拒否した読みを報告に名前で出す**。3件目が増えたらビルドが止まる。1文字が複数規則に載る場合はソース先着優先（Mozc の generator と同じ）で、衝突数を報告。
- 実データ検証: 実ビルドで 3724 readings / 23688 characters / 787 variant notes、拒否2件を報告。sweep 実測で `ひ` 19→**121**、`こう`→330、`し`→220、`き`→205、`か`→199。
- 実測（新 sweep、読み長クラス別 p95 最大 / 候補最大 / うち単漢字最大）:
  - 1文字: 18→16 µs、108→70 µs/108件、256→202 µs/220件（辞書が先に尽きる）
  - 2文字: 108→347 µs/108件、256→797 µs/256件
  - 3-4文字: 108→369 µs、256→1,638 µs
  - 5-8文字: 108→1,045 µs、256→3,002 µs、**単漢字 0 件**
  - 9文字以上: 108→3,454 µs、256→14,556 µs、**単漢字 0 件**
- 結論: 上限引き上げで得をするクラス（1-4文字）が最も安く、高いクラス（5文字以上）は上限を上げても単漢字を1件も得ない。読み長に応じた可変上限という前回の推奨が、今回のデータで独立に裏付けられた。
- 自分が作った不具合と修正: 衝突処理を最初 `if variants.insert(..).is_some() { variants.insert(variant, variants[&variant]); }` と書いたが、`insert` は既に値を置換済みなので再挿入は no-op で「後着優先」になっていた。`Entry::Vacant`／`Entry::Occupied` に置換。
- 検証: `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（exit 0。sakura-core lib 242→249、dictc に `single_kanji` unit 13 + integration 10）、`git diff --check`。cargo／rustc の残存プロセスなし。
- 学び1: optional table を足す変更は、既存の `BNDR` と同じ「version bump 不要」経路に必ず乗せる。reader が未知タグを読み飛ばす設計が既にあるのに version を上げると、古いイメージを不必要に失効させる。
- 学び2: 上流データの欠陥は「件数で許容」ではなく「**内容を名前で固定して件数で許容**」する。`MAX_REJECTED_READINGS = 2` だけなら別の2件と入れ替わっても気づけない。報告に読みを出しているので、ソース bump 時に差分が見える。
- 学び3: ランキング済みリストへ後段追加するときは、追加行のコストを「直前の追加行」ではなく「**ランキング全体の上限**」を基準に置く。安いリストの末尾が高いリストの本体を追い越す事故を、後段の再 sort に依存せず構造で防げる。
- 未決: 候補数上限そのものの数値は owner 判断待ち（推奨は読み長可変: 1-2文字 256／3-4文字 108／5文字以上 36）。上限を 18 から動かすと `MAX_CANDIDATES`（= `CANDIDATE_PAGE_SIZE * 2`）と `MAX_CANDIDATE_TEXT_BYTES`（= `MAX_PREEDIT_BYTES * CANDIDATE_PAGE_SIZE`）という wire 側の定数に波及する。


## 2026-08-26 — 句読点を「2つの独立した役割」へ作り替え、半角 `,` `.` を追加（#96、commit 5261ab8）

- 症状/課題: owner から「設定画面の句点と読点だと論文が書きにくい」。当てずっぽうで直さず何が足りないかを訊いたところ、答えは**半角の `,` `.` が選べない**だった。既存の4種（`、。` `，．` `、．` `，。`）は全部到達可能だったので、欠けていたのは組み合わせではなく**半角という選択肢そのもの**。LaTeX や Markdown から組版する日本語論文では全角 `．` は誤りで、ASCII の `.` でなければならない。
- 根本原因: `PunctuationStyle` が4 variant の enum で、内部表現も `parts() -> (bool, bool)` / `from_parts(bool, bool)` という**役割あたり2択の bool**だった。3択目を足す余地が型に無かった。設定画面は既に句点・読点を独立した2つのコンボで見せているのに、型だけが4通りの直積を bool で表していた。
- 修正: `PunctuationStyle` を「2つの役割 enum を持つ struct」へ。`CommaMark { Touten, FullWidth, HalfWidth }` と `PeriodMark { Kuten, FullWidth, HalfWidth }`、`PunctuationStyle { comma, period }` で 3×3=9通り。`parts()`／`from_parts()` は削除し、`ALL`（9件）と名前付き const（`KUTEN_TOUTEN`／`COMMA_PERIOD`／`MIXED`／`COMMA_KUTEN`／`ASCII`）に置換。
- 設計判断（重要）: 半角は**出すが取らない**（emit but never reclaim）片方向にした。`punct_role()` が所有するコードポイントは従来どおり `、` `，` `。` `．` の4点だけで、ASCII `,` `.` は所有しない。所有してしまうと、直接入力で打った `.` が既定スタイルの下で `。` に化け、`foo(a, b)` の `,` が読点として再解釈される。規則4（句読点は幅分類より先に解決する）はそのまま維持され、`width.symbol = Full` でも半角 `,` `.` は広げられない。
- 波及して確認したこと:
  - SIMD passthrough LUT（`simd.rs`）は変更不要。所有する4文字はすべて3バイト UTF-8 で、1バイト LUT に現れない。3バイト→1バイト置換が ASCII の連続コピー区間の途中に落ちることを `normalize_into` のテストで固定した。
  - ATOK 由来の数字直後スワップ（`input_repair::contextual_punctuation_swap`）は `．`／`，` を返すだけで、最終字形はチョークポイントに委ねている。つまり半角設定なら自動的に `1.5` になる。テストで固定。
- 設定ファイル互換: 既存4名（`kuten-touten`／`comma-period`／`mixed`／`comma-kuten`）を正規名のまま維持し、新5組に規則的な名前（`touten-half-period`／`full-comma-half-period`／`half-comma-kuten`／`half-comma-full-period`／`half-comma-half-period`）を追加。旧4名にも規則形の別名を読み取り専用で受ける。
- 設定画面: 句点 `。`／`．（全角）`／`.（半角）`、読点 `、`／`，（全角）`／`,（半角）`。コンボ幅は据え置き（隣の `入力モードに合わせる` の方が長いので広げる必要がない）。
- 分担: `preferences.rs`（parse/serialize）と `ui.rs`（コンボ）を並行 agent へ、型本体・テスト・DESIGN.md・機械的リネーム6ファイルは本体で実施。
- 検証: `cargo fmt --all -- --check` clean、`cargo check --workspace --all-targets` clean、`cargo test -p sakura-core -p sakura-settings` すべて ok、`git diff --check` clean。cargo／rustc の残存プロセスなし。
- `--workspace` で見えた失敗2種は**どちらも本変更由来ではない**:
  1. `dispatch::tests::history_*` 2件 — 単体でも `-p sakura-engine --lib` 3回連続でも通る。複数テストバイナリ同時実行時だけ落ちる負荷依存フレークで、journal 2026-08-13 の TSF ハンドシェイクと同種。
  2. `quality_limit_matches_production_protocol_without_narrowing_generic_capture_loading` — `sakura_proto::MAX_CANDIDATES` が 256、`QUALITY_CANDIDATE_LIMIT` が 18 で不一致。同一 worktree で並行している別セッションの #95（`MAX_CANDIDATES` 引き上げ）の途中状態であり、こちらの担当外なので触らない。
- commit: その後 #95 が先に commit して `dispatch.rs` などの共有ファイルを解放したため、残る差分は #96 の const リネームだけになった。#97（表記スタイルのプリセット）と**同一 commit 5261ab8** に入っている。分けなかった理由は下の #97 エントリに書いた。ブランチ作成（`feat/96-half-width-punctuation`）は権限で拒否されたので `feat/95-single-kanji-and-candidate-cap` 上に置いてある。
- 学び: 「4通りある」を「2つの bool」で表すと、片方の軸に3つ目が来た瞬間に型ごと作り直しになる。UI が最初から2つの独立したコンボで見せていたのだから、型もその日から2つの役割 enum であるべきだった。直積を bool 対で潰さない。

## 2026-08-26 — 候補数上限を256へ引き上げ、読み長で配分（#95、commit 8183f0e、相談 Issue #100）

- 症状/課題: 8983139 で単漢字の在庫を入れた結果、律速が在庫から**上限そのもの**へ移った。`MAX_CANDIDATES` は wire を書いた当時から `CANDIDATE_PAGE_SIZE * 2 = 18`（2ページ分）で、変換器が2ページ分しか出せなかった頃の値のままだった。
- 実測（`tools/candidate-sweep`、release、同梱辞書、読み1件あたり p95）:
  - 1-4文字: 162 µs → 1,638 µs（**単漢字が増えるのはここだけ**）
  - 5-8文字: 595 µs → 3,002 µs（単漢字 0件）
  - 9文字以上: 1,694 µs → 14,556 µs（単漢字 0件）
- 決定的だったコスト分解: `lattice_nodes` は limit 18 と 256 で**完全に同一**（946 / 2,990 / 7,078 / 15,254）。増分は全部 `states_pushed`（29文字 4,158→63,849、93文字 13,672→65,536 飽和）で、候補14倍に対し states 15倍のほぼ線形、1 state 約180 ns。つまり「上限を上げると遅い」はアルゴリズムの病理ではなく N-best 抽出の素直な線形コストだった。**先に分解しなければ、存在しない病理を最適化していた。**
- 修正: `MAX_CANDIDATES = 256`（wire が運べる天井）と `conversion::candidate_budget`（この入力が実際に使ってよい額）を**別の概念として分離**した。予算は 256 / 108 / 18 を 4文字・8文字で切り替える。得をするクラスが最も安く、高いクラスは上限を上げても1件も得しないという実測に、境界をそのまま合わせている。長い読みは従来の上限も従来のレイテンシもそのまま。
- `MAX_CANDIDATE_TEXT_BYTES` は**引き上げない**。256件×最悪長は 1 MB 級の arena になる一方、実際に見えるのは常に1ページ。代わりに emit ループ側で2種類の失敗を分けた。選択候補が builder に入った後の arena 枯渇はそこで打ち切り、選択位置以前での失敗は従来どおり fail closed。後者を打ち切ると誤った選択位置を表示してしまう。
- 却下した仮説（実測で否定）: 「`MAX_CANDIDATES` 18→256 でスタックが溢れる」という並行エージェントの報告。128 KiB スレッドに対し wanted ∈ {18,256} × stack ∈ {64,96,128,192,256,384,512,768} KiB × 3回 = **48条件すべて成功**し、256候補は 64 KiB に収まった。既存の `raw_multi_pass_core_path_fits_128_kib_thread_stack` も単独25回・フルスイート6回で失敗なし。報告された 2/5 の失敗は、そのエージェントが clamp を書いている**途中のコード状態**で起きたものだった。
- 並行作業の分離: 同一 worktree で別セッションが #96（半角句読点）を実装中で、`dispatch.rs` と `sakura-core/src/lib.rs` が両方の変更を共有していた。`git apply --cached` でハンク単位に #95 だけを索引へ入れ、その索引ツリーを `git commit-tree` + `git worktree add` で別ディレクトリに実体化し、**単体でビルドとテストが通ること（921 passed / 0 failed）を実証してから**コミットした。
- 検証: `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（1,676 passed / 0 failed / 77 ignored）、`git diff --check`。cargo／rustc の残存プロセスなし。エージェントが書いた `assert!` が定数同士の比較で `clippy::assertions_on_constants` に触れたため `const _: () = assert!(...)` へ変更した（そのエージェント自身が「実行検証していない」と申告していた箇所）。
- 学び1: 「上限を上げたら遅くなった」は、**どこが増えたのかを分解するまで原因ではない**。lattice と N-best を分けて数えた時点で、最適化すべき対象が存在しないことが確定した。分解の前に高速化へ着手していたら、線形コストを相手に時間を溶かしていた。
- 学び2: 天井（wire が運べる最大）と予算（この入力が使ってよい額）は別概念であり、1つの定数に兼任させると片方だけを動かせない。分離して初めて「短い読みだけ豊かにする」が書けた。
- 学び3: 並行セッションと worktree を共有しているときは、ファイル単位の `git add` では切り分けられない。`git apply --cached` でハンクを選び、`commit-tree` した索引ツリーを別 worktree で実際にビルドして自己完結性を確認する。相手の作業ツリー内容には一切触れずに済む。
- 未決: 学習コストの lattice 注入と「履歴に出てくる候補は通常候補から外す」（#95 の残りスコープ）。上限設計そのもの（読み長clampで足りるか、遅延N-best展開まで作るか）は #100 で相談中。

## 2026-08-26 — 表記スタイルのプリセットを設定画面へ（#97、commit 5261ab8）

- 症状/課題: owner から「このあたりを補助して UX を上げるための設定画面なのよ。スタイルに併せて事前設定できるのが良いのよ」＋日本語技術論文の表記・スペーシングルール（29節）。#96 で半角 `,` `.` は**選べる**ようになったが、「半角句読点・和欧境界は半角スペース」という社内規則を実現するには**2ページに散った7つのコントロール**を人間が翻訳して1つずつ合わせる必要があり、合わせ終わったあとに正しいか確認する手段も無かった。
- 修正: `NotationStyle`（`preferences.rs`）が7値の組み合わせ全体に名前を与える。英字幅・数字幅・記号幅・句点・読点・括弧・スペース幅。同梱4種は 標準（日本語）／日本語技術論文（半角句読点）／学術（全角コンマ・ピリオド）／公用文 で、7値上で**相互に相異**であることをテストで固定した。
- 設計判断1（最重要）: プリセットは**設定ではなく近道**。config key を持たず、`Preferences` はどのスタイルが選ばれたかを一切記録しない。7つの葉が唯一の真実源のままなので、**このコントロールを一度も開かない人の config ファイルは1バイトも変わらない**。選ぶと7つへ書き込み、7つのどれかを手で触ると再導出して、どのスタイルも作らない組み合わせなら `カスタム` へ落ちる。`apply_to` / `of` の往復がこの2方向を一致させている。
- 設計判断2: 双方向同期が再帰しない根拠は `CB_SETCURSEL` が `CBN_SELCHANGE` を**発火しない**こと。つまり「プリセット→7コンボ」の書き込みが「7コンボ→プリセット」を呼び返さない。フラグやガード変数を持たずに済んでいるのはこの Win32 の仕様に乗っているからで、ここを `SendMessage` 以外の経路へ替えるなら再帰対策が必要になる。
- 設計判断3: 7つ目のスペース幅だけ `入力補助` ページにある。読者が見ていないページのコントロールを黙って動かさないよう、適用時のステータス行でどのスペース幅にしたかを**名指し**する。文言とコンボ行が食い違わないようラベルは `space_width_label` 1か所から供給し、ユニットテストで固定した。
- 発見（テスト基盤のバグ）: 閉じた `CBS_DROPDOWNLIST` は**作成時の背の高いウィンドウ矩形をそのまま保持する**（ドロップダウンの伸び代を含む）。最初に書いた `combo_beside_label` はラベル中心の垂直包含で行を特定していたが、これだと `表記スタイル`（top=72、height=150）が下の全行を飲み込み、`英字` を探しても常にプリセットが返る。**最も近い上端**で照合する方式へ書き換えた。行ピッチ28px はコンボとラベルの4px オフセットに対して十分大きいので、どの DPI でも一意に決まる。
- 副産物: 位置インデックスをラベル照合へ替えた過程で、基本ページの `basic_combos.len() == 2` が **d8db8d4（Sakura Pad 行の追加）以降ずっと陳腐化していた**ことが判明。件数は古いまま、`[1]` はたまたま意図した コントロールを指し続けていたので誰も気づけなかった。件数と位置の両方を名前へ置き換えて両方の失敗モードを消した。
- commit 判断: #96 と #97 を**同一 commit 5261ab8** にした。6ファイルが両方の変更を持つうえ、`ui.rs` の `sakura_core` import 行、`punctuation_from_indices` 周辺、`preferences.rs` のテストモジュール、`docs/settings-user32-e2e.md` の SET-U32-012 行は**同じ数行の中で両方が混ざる**。#95 が使ったハンク単位分割（`git apply --cached` → `commit-tree` → 別 worktree でビルド実証）も検討したが、この4か所は行単位で混ざっているため、**誰もビルドしていない中間状態を手で捏造する**ことになる。捏造した中間 commit より、両 Issue を名指しした1 commit のほうが履歴として正直だと判断した。
- 検証: `cargo fmt --all -- --check` clean、`git diff --check` clean、`cargo check --workspace --all-targets` clean、`cargo test -p sakura-core -p sakura-settings` で 340 passed / 0 failed / 21 ignored。cargo・rustc・`sakura_settings` の残存プロセスなし。実画面での目視確認は未実施で、`notation_style_preset_writes_both_pages_and_persists` は対話デスクトップ必須の `#[ignore]` のまま（`docs/settings-user32-e2e.md` の SET-U32-121 に「実画面未実行」と明記済み）。
- 学び1: 「散らばった設定をまとめるプリセット」を**保存する設定**として足すと、プリセットと個別値のどちらが正なのかという同期問題を新規に作り込む。保存せず**導出する読み取り専用の表示**にすれば、真実源は増えず、既存 config も不変のまま済む。まとめる価値と保存する必要は別物。
- 学び2: Win32 のコントロールを矩形で特定するときは、**見えている大きさと `GetWindowRect` が一致しない種類がある**ことを先に疑う。閉じた `CBS_DROPDOWNLIST` はその代表。垂直包含は直感的だが、この一族には使えない。
- 学び3: `len() == N` を書いたテストは、N が増えたときに**壊れずに嘘をつく**ことがある。今回は位置インデックスがたまたま当たり続けたので誰も気づかなかった。並び順ではなく**画面上の意味（隣のラベル）**で対象を掴む。
- 未決: #99（句読点設定は既定を選ぶだけで候補一覧は網羅的にする＝ATOK 挙動、`conversion.rs` の `append_punctuation_family` + `synthetic_exact`。→ 2026-08-26 commit 4d265e7 で対応済み）と #98（和欧・和数境界の半角スペース自動挿入、URL・ファイル名・`CI/CD`・`GPT-5.6` 等の除外リスト付き）は未着手。

## 2026-08-26 — 句読点の変換候補を網羅化（#99、commit 4d265e7）

- 症状/課題: owner から ATOK のスクリーンショット付きで「設定画面でこれらを設定すると特定の記号しか出てこないけど、ATOK なんかだと設定しても変換候補は網羅的なんだよね」。#96 で半角 `,` `.` を**選べる**ようにしたが、選ぶと他の3つが**到達不能**になっていた。`，．` にした人は引用文1つのために設定画面を開き直す必要があり、`、。` の人は原稿1行のために ASCII へ戻せない。ATOK は設定が**既定順位だけ**を決め、候補一覧は4件のまま。
- 根本原因: 候補の中身ではなく**表示直前の正規化**にある。§5.6 の choke point は Rule 4 の4コードポイントに対し、候補がどの family member を持っていても設定した字へ書き換える。したがって `、` `､` `，` `,` の4候補を作っても、画面には同じ字が4回並ぶ。「候補が無い」のではなく「候補が潰れる」問題だった。
- 修正: `PunctuationStyle::family_for`（`width.rs`）が、どちらの役割の文字に対しても4件の family を「自分の設定字が先頭、残りは固定表順」で返す。表を `width.rs` の `CommaMark`／`PeriodMark` の隣に置いたのは、製品が知っている句読点の字形を**1ファイルに集約**するため（`width` はクレート内依存を持たないので循環しない）。`conversion.rs` の `append_punctuation_family` が、**読み全体が単一の句読点1文字のときだけ**この family を候補列へ差し込む。
- 設計判断1: 4行に `synthetic_exact` を立てる。この bit は `append_candidate_surface`（表示＋既定 commit ループ）と、commit 専用の2経路（`commit_candidate_surface` の `meta.synthetic_exact`、`commit_converted_segments` の `CandidateOverride` 分岐）が `normalize_into` より**手前**で見る。normalizer 側には一切触らないので、Rule 4 の所有4字は4字のまま、ASCII `,` `.` の「emit するが reclaim しない」一方向も維持される。`｡`（U+FF61）と `､`（U+FF64）は表に載せるが `punct_role` は主張しない＝**候補に出せるが奪わない**。9スタイル×記号幅2×モード2の総当たりで `normalize_char` が両字を素通しすることを固定した。
- 設計判断2: `synthetic_exact` は学習と exact cache も抑止する。これは副作用ではなく**要件**。引用符の中で1度 `、` を選んだことが、以後すべてのコンマで設定を上書きするようランカーを訓練してはいけない。durable な選好は設定だけ。
- 設計判断3: この appender だけが TOP-1 を置き換える。既存の appender（`append_single_kanji` など）は ranked ceiling より上に積む規約なので、例外の根拠を明示した。**読みが単一の句読点1文字**という条件下では、置き換えられる旧 TOP-1 は既に設定字として**描画されていた**ので、画面上はバイト単位で同一。条件を外せば成立しない議論であり、2文字以上・句読点を含むだけの読みは触らないことをテストで固定した。
- 設計判断4: 注釈は `NumericStyle::annotation` の裸名詞（算用数字／全角数字／漢数字）に倣い、全角読点／半角読点／全角コンマ／半角コンマ とした。ATOK の `[全] 読点` 形式は**他社製品の文字列なので写さない**。
- 設計判断5: `ConversionOptions` に `punctuation` を足し、`conversion_options()` は dispatcher ではなく **session** の `normalizer.punctuation` を読む。`Session` は既に `normalizer` を持ち、アプリ別プロファイル解決（`session.rs:1825`）がそこへ書き込むので、#97 のアプリ別表記スタイルが**追加配線ゼロで**変換器まで届く。8か所の呼び出し側は無変更。
- 却下した仮説（実測で否定）: 「この変更が 128 KiB 変換ワーカースタック契約を破った」。`cargo test -p sakura-core --lib` が `raw_multi_pass_core_path_fits_128_kib_thread_stack` で `STATUS_STACK_OVERFLOW` を出したため、`ConversionCandidate` 約4 KiB＋`FixedStr` 2本で約8 KiB のフレームを疑い、`#[inline(never)]` の別関数へ切り出した。**これは誤診**。(a) 同じ関数に `black_box([0u8; N])` を入れて N=0/256/1024/4096/**16384** すべて成功＝フレーム余裕は16 KiB 以上ある。(b) 自分の変更3ファイルを `git stash` して **HEAD 単体で8回**回すと **2回 overflow**。つまり既知のフレークで、`journal.md` の 2026-08-25／08-26 のエントリ（実測で 32 KiB でも通ると記録済み）と同じもの。切り出しは根拠のないコメントごと撤回し、`append_single_kanji` と同じ1関数の形に戻した。**再現率の実測値（HEAD で約25%）が、未解決だったこのフレークへの新しいデータ点。**
- 検証: `cargo fmt --all -- --check` clean、`git diff --check` clean、`cargo test --workspace` 全 93 スイート 0 failed（`sakura-core` 273 passed、`sakura-engine` 474 passed）。新規テストは `width.rs` 3件（9スタイル×2 family の総当たり順序、family の非交差と注釈の一意性、半角カナ記号の非主張）、`conversion.rs` 3件（9スタイル×2 family で先頭・全4件・重複なし・注釈・`synthetic_exact`、通常読みが全スタイルで不変、句読点を含むだけの読みは非対象）、`dispatch.rs` 2件（`,` と `.` を実際に打って変換し、**読者が見る文字列**で4件が別字であることと、`，` 設定のまま2行目を確定すると `、` が入ることを確認）。残存 cargo／rustc プロセスなし（動作中の `sakura_engine` 22864 等は導入済み IME 本体）。
- 学び1: 「候補が出ない」と「候補が潰れる」は別の故障で、直す層が違う。今回は候補生成ではなく**表示直前の正規化**が原因だったので、辞書にも探索にも触らずに済んだ。choke point を持つ設計の代償として、**候補の意図を choke point へ伝える bit**が要る。
- 学び2: 変換器レベルのテストでは今回のバグは**捕まらない**。4候補は変換器の中では正しく別物で、潰れるのは `normalize_into` を通った後だから。読者が見る文字列を検証するには dispatch レベルまで上げる必要がある。テストの層は「何が壊れうるか」で決める。
- 学び3: スタックのような資源制約を疑ったら、**まず余裕を測る**。padding probe（`black_box([0u8; N])` を増やす）で16 KiB 通ることが分かった時点で仮説は死に、次の一手（HEAD での baseline 測定）が決まった。測らずに `#[inline(never)]` へ逃げていたら、根拠のないコメント付きの構造をコードベースに残していた。
- 学び4（ツール）: このセッションの bash ツールは、quoted heredoc の中でも `'` を数える。所有格アポストロフィを含む長い Rust／散文を `<<'EOF'` で流すと `unexpected EOF while looking for matching '` で**ターン全体が落ちる**。長文ファイルは Write ツールで作り、Bash は splice だけに使う。
- 未決: #98（和欧・和数境界の半角スペース自動挿入、URL・ファイル名・`CI/CD`・`GPT-5.6`・`config.toml`・バージョン番号・`client-server`・`KEY=value` の除外リスト付き）は未着手。#99 の実画面確認（インストール後に実際の候補ポップアップで4件を目視）も未実施。

## 2026-08-26 — 実画面確認で見つけた「既定候補が選ばれない」欠陥（#99、commit 4580fcf）

- 症状: 前エントリ（4d265e7）の #99 は unit／dispatch テスト全緑で完了扱いだったが、インストール済み 1.0.28-f117469c43d401dd の**実画面**で `,` を打って変換すると、一覧は `1. 、 2. ､ 3. ， 4. ,` と設定どおりなのに**選択が3行目**にあり、そのまま Enter すると `，` が入った。設定画面は `、` と表示している。#99 の主張「設定は既定順位を決める」が、**順序では守られ選択では破られていた**。
- 根本原因（2つ、どちらも同梱既定スタイル `、`／`。` で最も効く。この場合だけ**設定字＝reading** になるため）:
  1. `preferred_candidate_index`（`dispatch.rs`）は `requested == 0` のとき **text が reading と一致する候補を全経路で除外**する。通常語では正しい（読みそのままの行は既に preedit として見えている）が、句読点 family では「読者が打った記号を返す」ことが目的なので、TOP-1 の設定字が必ず弾かれる。学習が空でも `､`（index 1）が選ばれる。
  2. 学習ストアが**過去の設定で学んだ surface** を持っている。検証機には `、`→`，` 150件、`。`→`．` 74件があり、`，．` を使っていた時期の commit で貯まったもの。exact context の strength が Strong になるため index 2 まで届き、実機の「3行目」を正確に説明する。
- なぜ #99 以前は見えなかったか: 以前は4候補すべてが choke point で設定字へ書き戻されていたため、**どの行を選んでも画面と文書は同じ字**だった。行を別字にした #99 が、既存の誤選択を初めて可視化した。つまりこれは #99 が作った欠陥ではなく、#99 が露出させた欠陥だが、**#99 の契約を破る**ので #99 で直す。
- 修正: `width.rs` に `PunctuationStyle::family_reading(&str)` を追加し、「読み全体が句読点1文字」という**唯一の受理判定**にした。`append_punctuation_family` の入口をこれに差し替え、`dispatch.rs` の `preserve_exact_initial` を `literal_policy != Ranked || punctuation_family` へ広げた。これで exact literal と同じ扱いになり、learning／exact cache／任意 reranker のすべてが初期選択に触れなくなる。appender と pin が同じ述語を共有するので、片方だけ条件が動く事故を構造的に防ぐ。
- 学習データは**消していない**。所有者の資産であり、この修正は「これらの行では参照しない」だけで足りる。`learning clear` は提案も実行もしていない。
- 検証: 失敗するテストを**先に**書いた。`the_shipped_punctuation_style_stays_selected_when_it_equals_the_reading` は修正前 `Some(1)`、`a_stale_learned_mark_cannot_override_the_configured_punctuation` は `、`→`，` を4回学習させて修正前 `Some(2)` と、実機の症状を数値まで再現した（1回の learn は Weak で index 1 までしか届かず再現しない。4回で Strong）。修正後は両方 `Some(0)`。後者は続けて2行目へ移動して `､` を確定し、**既定を固定しても一覧は固定していない**ことを押さえる。`width.rs` に `family_reading` の直接テスト（単一記号では `family_for` と一致、空・2文字・記号を含むだけの読みでは `None`）。`cargo fmt --all -- --check`、`git diff --check`、`cargo test --workspace` すべて成功（`sakura-core` 274、`sakura-engine` 476、0 failed）。残存 cargo／rustc なし。
- 実画面での最終確認: release ビルド → `scripts/build-installer.ps1` → silent install で `1.0.28-afd34a06422849de` を導入。展開ポップアップは `1. 、 全角読点 / 2. ､ 半角読点 / 3. ， 全角コンマ / 4. , 半角コンマ`、footer `変換 1–4 / 4`、**1行目が選択**。句点側も `1. 。 全角句点 / 2. ｡ 半角句点 / 3. ． 全角ピリオド / 4. . 半角ピリオド`。1〜4行目を順に確定させると文書に `、､，,` と `。｡．.` が入ることを host の TextChanged ログで確認した（画素ではなく**文字列**で確認）。
- 学び1: **テストが緑でも実画面で確認する理由がこれ**。自分で書いた dispatch テストは9スタイルのうち `FullWidth`／`HalfWidth` を使い、既定の `Touten`／`Kuten` を**避けていた**。設定字と reading が一致するのは既定スタイルだけで、そこにだけ欠陥があった。網羅したつもりのテーブル駆動テストでも、**既定値を明示的に含めたか**は別に確認する。
- 学び2: 候補の「順序」と「選択」は別の契約で、別の場所が決める。順序は converter、選択は dispatcher の `preferred_candidate_index`／`preserve_exact_initial`。片方だけ直すと「一覧は正しいのに入る字が違う」という、ユーザーから見て最も分かりにくい壊れ方になる。
- 学び3: 「学習に書かない」と「学習を読まない」は別。`synthetic_exact` は書き込みだけを抑止していたので、**過去の設定で貯まった学習**が新機能の既定を上書きし続けていた。durable な preference を1つに決めた機能では、入口と出口の両方を塞ぐ。
- 学び4（実画面検証の手法）: 候補ポップアップは非activate・click-through で、通常のスクリーンショットでは前面ウィンドウに隠れる。`SakuraInputCandidates` クラスの HWND を `EnumWindows` で探し、`PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT=2)` で**ウィンドウ自身を描かせる**と遮蔽に関係なく確実に撮れる。ポップアップは既定で compact（選択行＋footer だけ、高さ50 px）で、MS-IME キーマップでは Tab（`candidate_expand`）で全行に開く。`VK_CONVERT` は `keybd_event` では届かないので Space を使う。合成入力の到達確認は host の `KeyDown` が `ProcessKey (229)` を出すかで見る。
- 学び5（環境）: このリポジトリの PowerShell スクリプトは **pwsh 7 必須**。Windows PowerShell 5.1 は `[IO.EnumerationOptions]`（.NET Core 専用型）を解決できず `build-dictionary.ps1` が落ちる。CI も `shell: pwsh`。また `tail` にパイプすると非0終了が飲まれるので、成否は終了コードではなく**ログ本文**で確認する。
- 未決: #98（和欧・和数境界の半角スペース自動挿入、URL・ファイル名・`CI/CD`・`GPT-5.6`・`config.toml`・バージョン番号・`client-server`・`KEY=value` の除外リスト付き）は未着手。#99 は実画面確認まで完了。

## 2026-08-26 — #98（和欧境界の自動スペース挿入）を実装前に取りやめ

- 経緯: owner 選択により #99 の実画面確認後 #98 へ着手。実装前の入力経路調査で前提が崩れ、owner 判断で機能ごと取りやめ、Issue #98 を `not planned` でクローズした（コメント: issues/98#issuecomment-5426413585）。コード変更なし。
- 私の誤り 1: 「除外リストは全て純 ASCII なので規則が到達しない」と結論し、和欧境界は 1 確定内の走査で扱えると考えた。owner から「それらは Shift 押しながら入力できないでしょ」と指摘され、`dispatch.rs:3028` の `starts_shifted_ascii` を読んで確認した。一時英字入力は **composition の 1 文字目が Shift+英字のときだけ**始まるので先頭は必ず大文字になり、`config.toml`／`3.14.1`／`git push` はこの経路で打てない。さらに shifted_ascii 中は ASCII が全て英字として溜まる（3055 行）ため `AWS` + `wo` は `を` にならず、`AWSを利用する` という composition は存在しない。
- 私の誤り 2: 上を受けて今度は「ASCII は必ず別コミット。だから cross-commit 専用機能だ」と結論した。これも誤り。owner が `git push` は打てる、`IT` は打てないと実機で示したので `data/it-terms.tsv` を直接引いたところ、**かな読み → 純 ASCII 表記のエントリが 7,365 件**あった（`えーだぶりゅーえす`→`AWS`、`ぱいそん`→`Python`、`ぎっとはぶ`→`GitHub`）。ASCII は通常のひらがな composition のセグメントとしても出る。
- 取りやめの理由: 規則が実際に到達する境界は「確定と確定の間」「1 確定内のセグメント間」「1 エントリ表記の内部（`X線` 型）」の 3 種に分かれ、正しい振る舞いが各々異なる。確定文字列の走査だけでは区別できず、左文脈保持（§5.8 carryover は明示的モード切替でリセットされる）と辞書引きの両方が要る。誤爆の損失に対し確実に自動化できる範囲が狭い。
- 学び: **入力経路を先に確定させてから機能を設計する。** 2 回続けて、コードを読む前の「たぶんこう打つはず」で設計の重心を置き、2 回とも owner の実機知識に否定された。IME は「どの文字列が出力されうるか」ではなく「利用者がどの順でキーを押すか」で決まる。同種の機能では最初に (a) その文字列を打つ実際のキー列、(b) それが 1 composition か複数確定か、(c) 辞書変換で出るのか直接入力かを、実装コードと同梱辞書で確認してから仕様を書く。
- 副産物（次の作業へ）: 実カバレッジの穴を確認した。読み `it` は `IT Control`／`IT Governance`／`IT Phase` だけで**単体の `IT` が無い**。`あいてぃー`・`ipv6`・`sha256` は該当なし、`config` は `Config Rule` のみ。`IPv6`・`SHA-256`・`config.toml` は同梱辞書から出せない。owner 指示により辞書欠落の修正へ着手する。

## 2026-08-26 — 「候補は出るのに Space が空白になる」の実測調査（Issue 未作成、コード変更なし）

- 症状（owner 報告）: 「変換候補がでるけど、スペースキーを押してもスペースが入力されるだけで、変換されない」。以前にもあり最近再発した、原因は最近のコミットだろう、との申告。owner 自身のメッセージに未変換かなが残っており、それ自体が実発生の証拠になっていた。
- 調査手段: 開発者履歴（`%LOCALAPPDATA%\SakuraInput\history\input.bin`、developer-mode 有効）と IPC タイムアウト診断（`diagnostics\ipc-timeouts.bin`、32 byte/レコード、magic `SKTO`）を復号して突き合わせた。`history export` は engine 応答がタイムアウトして失敗するため、`history show` のダンプ（48,867 行）を時刻順に並べ替えて使った。履歴は**書き込み順であって時刻順ではない**ので、ソートせずに差分を取ると負の経過時間が出る。
- 再現データ（22:54–22:58、1.0.28-afd34a06422849de、6回連続で同一パターン）:
  `seq=310 sess=7 st=1->2 key=Space pre=[か]->[化] act=convert`（変換成功・候補表示）→ **354 ms 後** → `seq=311 sess=8 st=0->0 key=Space commit=[　] act=unbound`。
  変換の直後に **engine セッションが作り直され**、新セッションは Idle・composition 空。そこへ来た Space は未束縛キーとして扱われ、全角スペース `　` が確定される。これが報告された症状そのもの。
- 確定した機構: セッションを作り直す経路は 2 つある。`Engine::drop_link()`（IPC 失敗）は `blocked_until = now + RETRY_INTERVAL(2 s)` を張るので、**350 ms 後のキーは engine に届かず履歴にも残らない**。届いて記録されている以上 drop_link ではない。残るのは `TextService::disconnect()`（`text_service.rs:2814`）で、これは `*engine = Engine::new()` により**待機時間なしで**リンクを捨てる。呼び出し元は約20か所あり、すべて write journal の検証失敗（`RevisionMismatch`／`StaleCallback`／`PredecessorFailed`／`EngineUnavailable`／`ContextReplaced`）である。
- 除外できたもの:
  - IPC タイムアウト: `key` の最終記録は 19:53:15、`ui` は 19:53:58。22:54–22:58 の失敗には**診断が1件も残っていない**。42件の `key` タイムアウトは別の（先行する）問題。
  - #69 の Dual TSF 同時配送: 80 ms 以内の別セッション Space ペアは 0 件。ただし本件は 350 ms の**逐次**発生なので、この測定は #69 を否定していない（窓が狭すぎた）。
  - 候補上限（8183f0e、#95）: 読み長別の切断率は 1–4 字 56%、5–8 字 50%、9+ 字 42% で**差がない**。9+ 字は budget 18 のまま（変更前と同じ）なのに同率で壊れるので、256 への引き上げは discriminator ではない。
  - IPC フレーム 8 KiB 説: `PIPE_BUFFER_BYTES = 8 KiB` はカーネルバッファであって上限ではない。パイプは `PIPE_TYPE_BYTE|PIPE_READMODE_BYTE` で長さ前置フレーム＋`read_exact`。分割読みは正しく処理される。仮説を立てて**確認して破棄した**。
- owner の仮説は支持された。ビルド別の「変換直後にセッションが切れた率」:
  1.0.17 = 1/1349（0.07%）、1.0.27 = 3/629（0.5%）、**1.0.28 = 26/56（46%）**、うち `　` 確定 9件。桁が違う回帰であり「最近のコミットが原因」は正しい。
  ただし 1.0.27 → 1.0.28 で **`crates/sakura-tsf` は1行も変わっていない**（`git log --name-only` で確認）。TSF 側の検証コードではなく、**engine／proto が送る内容**が変わって TSF の検証を落としている。窓内のコミットは #94 191ca67、#95 8983139・8183f0e、#96/#97 5261ab8、#99 4d265e7・4580fcf。
- 併発する別問題（本症状とは独立、こちらも実測）: `input.bin` が 14.8 MB まで肥大している。`InputHistoryService::open` は `repair_file` → `compact_file`（全復号＋全再暗号化＋一時ファイル書き換え）→ `next_sequence`（再度全復号）→ `next_session_id`（再度全復号）を**パイプ生成前に同期実行**する。1パスの復号だけで **14.8 s**（`history show` の実測）。結果、engine の `ready elapsed_ms` は 4,536 ms → 27,000–45,000 ms（08-24 のピーク 102,083 ms）に悪化した。この間 TSF DLL は素通し（pass-through）でキーがそのままアプリへ入る。さらに renderer watchdog は `RETRY_FLOOR 250 ms`／`RELAUNCH_GAP 5 s`／`WATCH_BUDGET 15 s` と起動時間より桁で短く、engine を3–4個同時に起動して互いを遅くする。実行時の 60 秒／256 追記ごとの再 compaction は writer スレッド上で `try_send`（非ブロッキング）なので**キー応答を直接は止めない**。CPU/IO 競合の間接影響は未証明として扱う。
- 未了: 原因コミットの特定。`disconnect()` は理由コードを一切残さないため、静的解析ではこれ以上絞れない。次手は (a) 既存の `ipc-timeouts.bin` と同じ仕組みで `disconnect()` に理由コードを記録する（挙動を変えない観測のみ）、(b) 1.0.27→1.0.28 の6コミットを bisect、の2つ。
- 交絡の明示: フォーカス移動やコンテキスト置換でも `disconnect()` は正当に起きる。今回の計測は owner が実際に打鍵していた時間帯で、ウィンドウ切り替えの混入を完全には排除できない。(a) の理由コードはこの交絡を切り分けるためにも必要。
- 学び1: **「新しいセッションが来た」は、どの経路で来たかで意味が正反対になる。** `drop_link()` は 2 秒の retry block を張るので、次のキーが engine に**届いているかどうか**が経路の判別子になる。届いていれば `disconnect()` 側。タイムアウト診断の有無より、この時間差のほうが強い証拠だった。
- 学び2: 相関を見るときは**変更されていない側**を対照に置く。読み長 9+ 字は候補上限が変わっていないので、そこが同率で壊れた時点で候補上限説は死んだ。ビルド別の率（0.07% / 0.5% / 46%）も同じ形で、片方だけ見ていたら「最近のコミット」を否定しかけた。実際、日中 18:47 の `key` タイムアウトだけを見て一度は否定に傾いており、これは誤りだった。
- 学び3: 開発者履歴は**書き込み順**で保存される。時系列解析の前に必ず `timestamp-ms` でソートする。最初のスキャンでは `dt=-67029682ms` という不可能な値が出た。
- 学び4（環境）: PowerShell の `$pid` は読み取り専用の自動変数。バイナリ診断のデコードスクリプトで変数名に使うと `VariableNotWritable` で全レコードが落ちる。

## 2026-08-27 — 用語集由来の読み欠落を全件監査で埋める（#101、commit 9a5ab8a／rules ed13f7d）

- 症状: ITエンジニアが実際に打つ読みからIT用語が出ない。しかも「出ない」より悪い形で、`じーぴーゆー` の第1候補が `CPU` だった。見ずに確定される位置に誤答が座っていた。
- 根本原因: `data/it-terms.tsv` は固定した用語集から生成される。用語集が持っているのは各語の**語義（日本語の説明・展開）**であって、**ユーザーが押す読み**ではない。そのため `ACM` は `えーだぶりゅーえすさーてぃふぃけーとまねーじゃー` という読みだけを持って同梱されていた。実測すると ASCII の IT surface 9,599件のうち 2,234件（23%）にかな読みが一切なく、略語（大文字2–6字）687件のうち字読みで引けるのは 190件だけだった。用語集は複合見出し語を並べる一方、その素の頭語を持たないことも多い。
- 修正: 生成物 `data/it-terms.tsv` には触れず、プロジェクト著作のMITオーバーレイ `data/curated-terms.tsv` を 92行 → 1,006行（かな読み757、Shift+ASCII読み249）へ拡張した。略語の字読みは 190→662／687、かな読みなしの surface は 2,234→1,877（19%）。Shift+ASCII 行は予測を維持し、かな行は変換専用にした。かな読みは普通の語と接頭辞を共有するため、`え` から 470件の略語が予測に出る状態は「IT改善のために一般日本語を劣化させない」という製品規約に反する。
- 検証（主張ではなく実測）: オーバーレイの全754かな読みについて、**候補リストを全件** HEAD ビルドの辞書と diff した。実在語の消滅 0、IT語の到達不能 0。内訳は、既存エントリと衝突した読み32件（うち11件は先頭を奪ったのでコスト9000へ、`IA`／`ACID` は9000でも足りず16000へ譲歩。32件すべてを `ISSUE_101_CONTESTED_READINGS` へ固定）、fuzzy辺で**別の略語**を誤って返していた字読み16件の訂正（`ISSUE_101_LETTER_READING_CORRECTIONS`）、再価格付けではなくオーバーレイから落とした読み8件（`しすく`/CISC、`ぞっど`/Zod、`ぴんぐ`/ping、`ふぁいど`/FIDO、`ぶりん`/BRIN、`ぐろっく`/Grok、`へろく`/Heroku、`での`/Deno。いずれもASCII読みで到達可能）。刈られた34経路は全部、自分自身の読みから到達することを確認した（`あぶろう`→`炙ろう`。`ISSUE_101_PRUNED_FUZZY_PATHS`）。辞書は2パス決定的に 624,205 entries／47,561,532 bytes／SHA-256 `85d94aecd966a10f43aeb87b5109c3d0b92c6eade798cf7b553d4d5cb476d1eb`。`cargo test --workspace` は 1,698 passed／0 failed／82 ignored、`git diff --check` OK、cargo/rustc/テストランナーの残存プロセスなし。
- 対象外として明示: `issue_83_shipped_path_uses_a_costed_typed_frontier`（`cross_commit_bridge_spanning_paths == 0` を期待して6を得る）は**オーバーレイを変えずHEADから作った辞書でも同一に失敗する**。既存不具合であり本件の回帰ではない。別タスクへ切り出し済み。製品名バッチ2は担当agentが出力トークン上限（64,000）で落ちて未納品のため、その分の網羅は未着手のまま残っている。
- 学び1（最も高くついた）: **rank 1 の比較では足りない。exact entry はその下の fuzzy 展開ごと刈る。** 1行足した瞬間に `じーぴーゆー` の候補は108件から2件へ落ちた。だから `ぐろっく`/Grok は `クロック` と `黒く` を、`ぴんぐ`/ping は `ピンク` を、先頭チェックが緑のまま黙って消していた。先頭比較を捨て、「刈られた語は自分の読みから到達できること」を不変条件に置き換えるまで、この損失は見えなかった。
- 学び2: **監査の窓を先に検算する。** プローブが候補を `.take(8)` で切っていたため、754件中718件のベースラインが truncate されており、消滅語の監査はほぼ空だった。500へ広げたら truncate 0、消滅語34件が初めて出てきた。緑の監査結果は、監査の視野を測るまで信用してはいけない。
- 学び3: **生成辞書の読みは、その語の意味であって打鍵ではない。** 生成元が用語集・辞典・要約のとき、reading 列は「読み」に見えて別物が入っている。カバレッジは surface ではなく「ユーザーが押す読み」の側から数える。
- 学び4: 少数のcurated overlayで個別語を救済して終わりにしない、という製品規約は正しかった。全件監査に切り替えて初めて、衝突32件・誤答16件・削除8件という実際の分布が出た。個別対応では前2つしか見えない。

## 2026-08-27 — #103 の敵対的セルフレビュー（3ビルド sweep 実測、Issue 追記3）

- 対象: #103 本文と追記1-2（候補上限256の器コスト・sweep測定妥当性）。owner 指示で敵対的レビューを実施し、静的読解だけだった主柱を実測で検証した。
- 方法: `tools/candidate-sweep` を default（表層予算256＝出荷）／`research-top32`（32）／`wide`（512）の3ビルドで同一読み・同一 `--limits 18` で実行し、`lattice_nodes`／`states_pushed`／`p95_us` を比較。`research-top32` は `MAX_CONVERSION_CANDIDATES` 1定数のみ変えるため差分を表層予算へ帰属できる。
- 棄却: 「wide の 512 が両腕を汚染し #100 は出荷構成を測っていない」→ 256→512 で格子は全読み同一。辞書 image が1スパン256超の異表層を出さないため。#100 の表は格子に関して出荷相当。
- 確定: 「@18 列は #95 以前のベースラインではない」→ 表層予算 32→256 だけで limit=18 のまま こうしょう p95 +97%（格子+41%）、36字文 +15%（格子+16%）。案Aの「9+字は回帰ゼロ」は #100 の表から導けない。効きの軸は読み長ではなくスパンの同音語密度。
- 訂正2件: ワイヤ最悪値 34,867→解析上限≈36.6 KB（`delete_before` 1,536 B が無条件エンコードされるのを見落とし）。「8 MB=辞書インデックス」→ 正しくは学習ストア packed index の上限（DESIGN.md:727-729）。
- 新発見3件: `read_frame` の `resize(len)` で per-worker heap worst は 203 KB（×64=12.4 MiB）。候補1件長が `push_candidate`（アリーナ合計のみ検査）と `write_str`（4,096 B 上限）の間で未強制。アリーナ+1 B のメモリ係数は ×4アリーナ×64worker。
- 反映: 追記3コメント（issuecomment-5432379027）＋本文冒頭に棄却/確定の注記を前置。提案は「sweep に `--surface-budget` 実行時引数」「36.6 KB を回帰テスト固定」「DESIGN §10 に pipe worker heap の行を追加」「多接続 PWS テスト」に改訂し、根拠の無かった「25%取り分」は取り下げ。
- 学び1: 敵対的レビューの最短路は「自分の主張が偽なら安く反証できる実験」を先に組むこと。256↔512 の1比較で主柱の半分が10分で落ち、残り半分は 32↔256 で確定した。静的読解の連鎖（flag→定数→格子）は、どの環が実データで飽和しているかを1つ測るだけで壊れうる。
- 学び2: ビルド時定数を動かす実験は、その定数「だけ」を動かす既存 feature を探すのが先（`research-top32` がまさにそれだった）。ソース編集による対照ビルドは帰属が汚れる。
- 学び3: 「最悪ケース測定」を名乗る前に encode 経路の**無条件フィールド**を列挙する。`delete_before` は has_* フラグ無しで常に write_str される設計で、飽和形から漏れた。

## 2026-08-27 — 外部レビューの反映と、回帰窓の自己訂正（#100 / #102 / #103、コード変更なし）

- 経緯: #103 に外部の敵対的レビュー（判定「条件付き OK」）が入り、P0-1 として「Measured source SHA・branch・`git status --porcelain`・feature set・辞書 SHA を provenance ブロックに書け」「#102 の理由コードを実装する前に対象コミットを bisect 可能にしておけ」を要求された。provenance ブロックを埋めようとした作業そのものが、自分の分析の誤りを暴いた。
- 根本原因（自分の誤り）: 成果物ラベル `1.0.28` を tag `v1.0.28` と同一視していた。installer は crate version でラベルするので、branch が tag より先へ進んでも成果物は "1.0.28" のままになる。`scripts/build-installer.ps1:355-356` の `build_id` は payload の `path|bytes|sha256` を並べた内容ハッシュで、**git SHA も branch も dirty フラグも含まない**。`installer-build.report.json` の最上位キーにも git 由来のフィールドが1つも無い。**出荷成果物からコミットへ遡る手段が存在しない。**
- 訂正1（窓）: tag `v1.0.28` = `361a330` は `MAX_CANDIDATES = CANDIDATE_PAGE_SIZE * 2`(=18) / `MAX_DICTIONARY_SURFACES_PER_READING = 12` で、疑っていたコミット群より**前**。`v1.0.27..v1.0.28` は #92 の pad 4件のみ。正しい窓は `v1.0.28..HEAD`（20件、うちコード9件）。08-26 の journal 記述「1.0.27 → 1.0.28 の窓」「窓内のコミットは #94 191ca67、#95 8983139・8183f0e、#96/#97 5261ab8、#99 4d265e7・4580fcf」は誤りで、この6件は全て `v1.0.28..HEAD` に属する。
- 訂正2（ビルド分離）: 開発者履歴を version 文字列ではなく `engine` 起動レコードで区切り直したところ、**「1.0.28」は2ビルドあった**。`f117469c43d401dd`（08-26 22:06:39 ビルド / 22:08:14 起動、変換6・切断3 = 50%）と `afd34a06422849de`（22:37:17 ビルド / 22:38:30 起動、変換50・切断23 = 46%、`　` 確定9）。08-26 に記録した「1.0.28 = 26/56 = 46%」は**この2ビルドの合算**だった。owner の 22:54–22:58 の失敗は後者。2ビルド間で変わった payload は `sakura_engine.exe`(+512 B) と設定系2本だけで、`sakura_tsf.dll`(429,568 B / `362e1543…`) と `system.dic`(47,471,564 B / `b9394927…`) はバイト一致。**両ビルドとも壊れている**ので、この実測ではコード9件を絞れない。
- 検証（provenance の副産物・良い方）: sweep に使った `artifacts/release/system.dic`（47,471,564 B / `b9394927972c5042…`）は、`installer-build.report.json` の payload 記録により**出荷された 1.0.28 の2ビルドと同一辞書**であることが確認できた。#103 の測定は辞書について出荷等価である。
- レビュー指摘の採否: P0-1 採用（構造的欠陥として確定）。P0-2 は半分（lattice 回帰は決定的カウンタで既に確定済み）、残る admission カウンタ・フェーズ分割時間・5構成行列・`admit()` の順序入れ替えは採用。P0-3 は算術は既出だが**枠組みの訂正**（「アリーナが件数に追随していない」→「件数とバイトは独立の契約であり、欠陥は超過が観測不能なこと」）を採用、`CandidateBuildStats` も採用、「切り捨てを致命化するな」も採用（encode 失敗は既に `Fault::Protocol` で接続を落とし、それが #102 の症状の形そのもの）。P1-1 は**最良の新規発見**として採用。P1-2 / P1-3 採用。3層アルゴリズム案は保留（測定より先に再設計するのは順序が逆）。
- 確認したコード（HEAD `20d5088`）: `conversion.rs:180-196` の `admit()` は `known_surface` の線形 `.contains()` が上限判定より前に走り、上限到達後も全拒否呼び出しが配列全体を走査する（12 → 256 で拒否経路が21倍）。`ui.rs:133-158` の `matches_output` は早期 return ブロックで `selected` を比較するため、**選択を1行動かすだけで** `copy_from_output`(745-805) が全候補の text + annotation + history identity を複写する（256件で矢印1打あたり最大 ~41 KiB の memcpy がキー経路に乗る）。
- bisect の前提が欠落: `191ca67` / `a8e5a4e` / `8983139` / `8183f0e` / `5261ab8` / `4d265e7` / `4580fcf` と HEAD `20d5088` は `git branch -r --contains` が全て空。branch `feat/95-single-kanji-and-candidate-cap` に upstream 未設定。リモートにあるのは tag `v1.0.28` まで（08-26 に「remote tags は v1.0.9 まで」と書いたのも誤り）。**現状このマシン以外で bisect できない。** push は owner の明示承認待ちで、未取得。
- 反映先: #103 本文を全面改訂（provenance ブロック / 訂正 / 再構成した §2 / `admit()` / 複写コスト / メモリ×64 / u16 化 / 測定行列 / 完了条件 / push 未解決）＋レビュー採否コメント（issuecomment-5432650413）。#100 に結論2件の撤回・保留コメント（issuecomment-5432659309）。#102 に窓の訂正コメント（issuecomment-5432665714）。
- 学び1: **成果物のバージョン文字列を git の位置と同一視しない。** installer が crate version でラベルする限り、tag と成果物は別物になりうる。`build_id` が内容ハッシュで git を含まない設計だと、そこに気付く手掛かりが1つも残らない。ビルド成果物には必ず `git_sha` / `branch` / `dirty` を焼き込む。
- 学び2: **「provenance を書け」という指摘は形式要件ではなく、実質的なバグ検出器である。** 出所を1行ずつ埋める作業が、窓の誤りとビルドの二重計上を同時に暴いた。分析結果を書く前に provenance ブロックを埋める順序にすれば、誤った窓のまま bisect を始めずに済んだ。
- 学び3: **集計はラベルではなく境界イベントで区切る。** version 文字列で group by すると同名の別ビルドが合算される。開発者履歴には `engine` 起動レコードがあり、これで区切れば n=6 と n=50 に分離できた。08-26 の集計はこれを怠って 56件に混ぜていた。
- 学び4: 上限定数を上げる変更では、**その定数に比例して重くなる経路を全部数える**。今回 `admit()` の拒否走査と `copy_from_output` の全件複写を見落としており、どちらも外部レビューの側から指摘された。「上限を上げた」だけでなく「上限が掛かる場所」を列挙する手順にする。

## 2026-08-27 — 未 push の9コミットが CI 3ゲートすべて赤だった／raw-repair のスタック超過を実測で確定（#94 / #95 / #99 / #103、commit 2da1261）

- 経緯: owner の「main に反映して全部」「というか main で作業しないとだめよな」を受けて push する前に、CI が実際に回すゲート（`.github/workflows/ci.yml`、windows runner・debug）をローカルで回した。**3つとも赤だった。** branch `feat/95-single-kanji-and-candidate-cap` に upstream が無かったため（08-27 の前エントリで確認済み）、`v1.0.28..HEAD` の9コミットは**一度も CI を通っていない**。push していれば main が赤で着地していた。
  - `cargo test --workspace` → `raw_multi_pass_core_path_fits_128_kib_thread_stack` が `STATUS_STACK_OVERFLOW`（0xc00000fd）
  - `cargo clippy --workspace --all-targets -- -D warnings` → `clippy::field_reassign_with_default` 2件（`dispatch.rs:6949` / `:7025`、#99 のテスト）
  - `cargo fmt --all -- --check` → 修正後の新規呼び出しで不整合（自分の変更由来）
- 根本原因（推測ではなく逆アセンブルの実測）: `ConversionCandidate` は **4,152 バイト**。debug ビルドは move も一時オブジェクトも別スロットに置くため、`convert_input_with_raw_repair_plans` が約10個を1フレームに抱え、**41,456 バイト**のフレームになっていた。しかもそのフレームは corrected pass の変換サブツリー全体が動いている間ずっと生きている。実測必要量は約 136 KiB、予約は 128 KiB。**8 KiB 足りないだけ**なので、環境ノイズで通ったり落ちたりする（実測フレーク率 約30%）。
- 測定手法（再利用可能）: リンク済み `.exe` には COFF シンボルが無い（シンボルは PDB 側）ので**オブジェクトファイルを見る**。`cargo rustc -p sakura-core --lib --profile dev -- --emit=obj=<path> -C codegen-units=1` → `llvm-objdump -d -C`（`~/.rustup/toolchains/1.96.0-x86_64-pc-windows-msvc/lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-objdump.exe`）でプロローグを読む。**awk は2形を両方拾うこと**: 通常の `subq $0xN, %rsp` と、MSVC の大フレーム形 `movl $0xN, %eax` … `callq <__chkstk 再配置>` … `subq %rax, %rsp`。後者は逆アセンブルテキストに `__chkstk` という文字列が出ない（再配置先なので）。最初これを取りこぼし、**一番大きいフレームだけが見えていなかった**。
- 修正1（スタック）: 候補を move する2ブロックを `#[inline(never)]` の兄弟関数 `admit_repair_pass`（21,488 B）と `merge_repair_scratch`（12,600 B）へ出した。深い呼び出しが走っている間は生きていない位置へスロットを移すのが要点で、合計サイズを減らしたのではない。オーケストレータのフレームは 41,456 → **7,576 B**、必要量は 136 KiB → **64〜68 KiB の間**（68 KiB で 20/20 成功、64 KiB で 20/20 失敗）。128 KiB のテストに対して約2倍、本番の `WORKER_STACK_BYTES = 160 KiB` に対しても十分な余裕になった。128 KiB ガード2本（`raw_multi_pass_core_path_fits_128_kib_thread_stack` / `cross_commit_bridge_fits_128_kib_thread_stack`）を各20回、計40回すべて成功。
- 修正2（`DictionaryEdgeBudget`）: #94 191ca67 と #95 a8e5a4e が `MAX_DICTIONARY_SURFACES_PER_READING` を 12 → `MAX_CONVERSION_CANDIDATES`（256）へ結び付けたことで、インライン配列 `[u32; N]` が **64 B → 1,040 B** に膨らんだ。それが `Copy` のまま、`build_lattice` の**読み開始位置ごとに新規構築**されていた。これを `Converter` 所有のスクラッチにして開始位置ごとに `reset()` する形へ変更。
- ここで自分のミス: 最初 `Vec::with_capacity` をローカルに持たせた。スタックガードは通るが `conversion_into_reused_candidate_buffers_allocates_nothing` と `cross_commit_bridge_conversion_allocates_nothing` が落ちる。変換パスは**アロケーション 0** が不変条件なので、容量はプロセス寿命のアリーナ（`Converter::new()`）側に持たせるしかない。**スタック制約とアロケーション制約は同時に満たす必要があり、片方だけ見た解は2回とも間違いだった。**
- 挙動不変の確認: `admit_repair_pass` は、インライン時代に加算していた拒否数をそのまま返す（`accepted` が空のときの `rejected += 1; continue;` に対応する 1 を含む）。`reset()` は新規構築時と同じ状態にする。
- 訂正（過去の journal 3エントリ）: 08-25／08-26 の「スタック説は実測で否定」は**誤り**だった。
  - 「32/48/…/160 KiB すべて exit 0」「48条件すべて成功」→ 今回の実測（64 KiB で 20/20 失敗）と両立しない。
  - `aad57d1`「the reported stack overflow ... was falsified — 48 of 48 ... passed and 256 candidates fit in 64 KiB」も同様に成立しない。
  - 最も高くついたのは 3件目。あるセッションは `#[inline(never)]` への切り出し、つまり**今回と同じ正しい修正**を一度実装し、padding probe（`black_box([0u8; N])` が N=16384 でも通る）を根拠に「誤診」として**撤回**していた。修正が消え、代わりに「既知のフレーク」という誤った結論が残った。
  - なぜ過去の測定が緑だったかは**再実行して決着させていない**ので断定しない。ただし今回、自分自身が同じ形の偽陽性を出した: `--exact` に**完全パスでないフィルタ**を渡すと 0 件実行で exit 0 になり、「全サイズ成功」に見える。48条件が全部緑という結果は、まさにこの失敗が作る形である。
- 学び1: **資源制約は「余裕を測る」のではなく「使用量を測る」。** padding probe は「N バイト足しても通る」しか言わない。ピークがどのフレームで起きているかを特定しないと、死んでいるスロットに padding が乗って無意味な緑が出る。逆アセンブルでフレームサイズを直接読むほうが速く、答えが一意だった。
- 学び2: **全条件が成功したスイープは、まず「本当に実行されたか」を疑う。** 実行件数（`running N tests`）を読まずに exit code だけ見た結果を証拠にしない。境界付近では 1 回の実行が確率的なので、**サイズごとに複数回**（今回は20回）回さないと通過／失敗の境界が出ない。
- 学び3: **上限定数を上げる変更のレビュー観点に「その定数がインライン配列やスタックスロットの寸法に入っていないか」を入れる。** #94/#95 の 12 → 256 は、候補数の話に見えて実際には 1,040 バイトの構造体を毎文字スタックに積む変更だった。08-27 の前エントリで「上限に比例して重くなる経路を全部数える」と書いたが、**時間**の経路だけを数えて**空間**の経路を数えていなかった。
- 学び4: **upstream の無い branch は CI が存在しないのと同じ。** 9コミット分の赤が溜まっていた。push 前に手元で CI と同じ3コマンドを回す手順を常に踏む。
- 検証: `cargo fmt --all -- --check` exit 0、`cargo clippy --workspace --all-targets -- -D warnings` exit 0、`cargo test --workspace` **1,698 passed / 0 failed / 82 ignored**、`git diff --check` exit 0、cargo／rustc／テストランナーの残存プロセス 0。
- 未了: `crates/sakura-ipc/src/diagnostics.rs`（#102 の `DisconnectReason`）は未使用アイテム2件が `-D warnings` で落ちるため**意図的に未コミットのまま**残した。#103 の完了条件（provenance フィールド、5構成の測定行列、admission カウンタ、`admit()` の順序、`copy_from_output` の選択のみ更新、多接続 PWS テスト、`CandidateSpan` u16 化）も未着手。

## 2026-08-27 — 4つ目の赤ゲート: Installer が単漢字表を checkout していなかった（#95、commit 9dd959a）

- 症状: 上のエントリの3ゲートを緑にして push した直後、**Installer workflow が 39 秒で失敗**した。`scripts/build-dictionary.ps1:579`「Mozc single-kanji table is missing: `.sources\mozc\src\data\single_kanji\single_kanji.tsv`」。
- 根本原因: #95（`8983139`）が単漢字表の依存をスクリプトへ追加したとき、スクリプト自身の `$SparsePaths`（`build-dictionary.ps1:529`）と必須ファイル検査（578-583行）には `src/data/single_kanji` を入れたが、**workflow 側の `sparse-checkout` に入れていなかった**。`installer.yml` / `release.yml` のリストは初回コミット `b3107aa` 以来一度も変わっていない。branch に upstream が無かったため、この2 workflow も #95 を一度も見ていない。
- 見えにくかった理由（非対称性）: `Resolve-PinnedSource` が sparse プロファイルを張り直すのは**自分が clone した場合だけ**（`else` 分岐、178-180行）。`-MozcSource` でディレクトリを渡された場合はリビジョンを検証して return するだけなので、workflow のリストから漏れたパスは修復されずハードエラーになる。
- 影響範囲: `release.yml` も同じ古いリストを持つ。**リリースビルドも同じ場所で落ちる**が、tag 起動なので実際にリリースを切るまで表面化しない位置だった。
- 修正: 両 workflow の sparse-checkout に `src/data/single_kanji` を追加。あわせて `src/data/rules/segmenter.def` → `src/data/rules` に統一し、スクリプト側のリストと**文字列として比較できる**形にした。cone mode ではファイルパス指定が祖先ディレクトリの直下エントリを引き込むため、取得内容は変わらない。非対称性の説明を両ステップの上にコメントとして残した。
- 検証: pinned revision `3f235b4e` に `single_kanji.tsv`（108,795 B）と `variant_rule.txt`（9,050 B）が両方存在。スクリプトが Mozc ツリーから必要とするのは `LICENSE` / `src/data/dictionary_oss` / `src/data/rules/segmenter.def` / `src/data/single_kanji` の4つだけで漏れなし。`ci/release-workflow-policy.ps1` は self-test 込みで通過（7 reviewed action references）。push 後、**HEAD `9dd959a` で CI・Installer とも success**（CI 13m45s、Installer 7m3s）。
- 学び1: **`ci.yml` を緑にしても「CI が緑」ではない。** 手元で回せる3コマンドだけを根拠に「green にしてから push」と宣言したが、Installer は赤のままだった。push が起動する workflow を `ls .github/workflows` で列挙してから宣言する。手元で回せるかどうかはゲートの範囲と無関係。
- 学び2: **tag 起動の workflow は最も遅く壊れが見つかる位置にある。** `release.yml` の同じ欠落は、リリースを切るその瞬間まで出てこなかった。push 起動の workflow が同じコードパスを持っているのは幸運で、設計として頼れるものではない。
- 学び3: **同じリストが2箇所にあるなら、まず「文字列として比較できる形」に揃える。** `src/data/rules` と `src/data/rules/segmenter.def` は cone mode では等価だが、目でも grep でも一致判定できなかった。等価だが表記の違うリストは、ドリフトを隠す。

## 2026-08-27 — 「再起動で自然復旧」を開発者ログで検証: 復旧は確認できるが修正の証拠にはならない（#102）

- 依頼: ownerが「ドラえもんはどら焼きが大好きです。変換は問題なく出来ています。何故か再起動したら自然復旧しました」と報告し、開発者ログでの確認を求めた。
- 確認できたこと: ownerの文章はログに全件ある（08-27 15:36:23–15:36:55、`どらえもん`→`ドラえもん`、`どらやき`→`どら焼き`、`だいすき`→`大好き` ほか31変換）。**すべて session=1** で、run全体310レコードを通じてsession変更が1回も無い。症状（`commit=　` かつ `action=unbound`）は0件。
- ただし**修正の証拠にはならない**。理由3点。
  1. **バイナリが同一。** 今日のengine起動（08-27 12:06:36）のrelease labelは `1.0.28-afd34a06422849de` で、19件の症状を出した run とまったく同じビルド。再起動でコードは何も変わっていない。
  2. **標本が小さい。** 壊れていた run の発生率は 19/313変換 = 6.1%。今日の 0/31変換 が偶然である確率は 0.939^31 ≈ **14%**。棄却できる水準ではない。
  3. **今日は誘発条件が無かった。** 症状19件は全件 session 番号の増加を伴う。session churn は壊れていた run が 27.7/1000keys、今日は 3.6/1000keys で**7.7倍の差**。今日はアプリを切り替えずに1文打っただけで、トリガを引いていない。
- 新しく分かったこと（過去の集計の訂正）: 症状を「変換の直後、次のキーが**別session**で `　` を `unbound` として確定する」形で定義し直して数えると、
  - `1.0.27-bd8c8ec0717ee03b`（08-26 11:41 run、1,355変換）: `　`確定は12件あるが、**直前が別sessionのconvertである件数は0**。つまり1.0.27の12件は通常の全角スペース入力であり、**本症状は1.0.27では発生していない**。
  - `1.0.28-afd34a06422849de`（08-26 22:38 run、313変換）: 症状19件（6.1%）、うち14件が「変換直後・別session」。
  - 08-26 に記録した「1.0.27 = 0.5%」は、この区別をしていない緩い判定だった。`v1.0.28..HEAD` への回帰帰属は、この訂正でむしろ**強くなった**。
- 壊れていた run は**今朝まで生きていた**: 症状の発生時刻は 08-26 22:54:23 から **08-27 10:47:19** まで伸びており、最後の1件は 12:06 の再起動の**1時間19分前**。22:54–22:58 に12件の burst、その後 07:49 / 08:03 / 08:39 / 08:53 / 10:35 / 10:40 / 10:47 に散発。bursty である。
- 副次の観測: `history export` が `FlushInputHistory` で4回連続 timeout（`ipc-timeouts.bin` が 3,040→3,200 bytes に増加、Administration timeout 4件として記録された）。一方 `history stats` は即答するので、engine が死んでいるのではなく **flush 呼び出しだけが `ADMIN_CALL_BUDGET` を超えている**。`input.bin` は 15,947,916 bytes まで肥大している（08-26 の 14.8 MB から更に増加）。`dropped-events` と `persistence-failures` は 0。読み取り専用の `history show` は 8.5 秒で完走する。
- 学び1: **「同じ症状が出なくなった」を「直った」と読まない。** バイナリ同一・標本31・誘発条件（session churn）が7.7倍低い、の3つが揃っている以上、今日のログは「トリガを引かなかった」以上のことを言っていない。復旧の主張には、壊れていた run と**同等の使い方**（アプリ切り替えを含む）での標本が要る。
- 学び2: **症状の定義を緩くすると、無関係な事象を回帰に混ぜる。** `commit=　` だけで数えると 1.0.27 に12件付いてしまい、回帰の窓がぼやける。「直前が別sessionのconvert」まで含めて初めて 1.0.27 = 0 / 1.0.28 = 14 という分離が出た。カウンタは症状の**機構**で定義する。
- 学び3: Windows の console 経由で日本語を含む TSV を取ると、リダイレクト前に code page 変換で壊れる。`[Console]::OutputEncoding` を UTF-8 にして `Out-File -Encoding utf8` で取り、読む側も `PYTHONIOENCODING=utf-8` を立てる。最初の dump は全文字が置換文字になっていた。

## 2026-08-27 — #102 の再発を同一 run 内で捕捉し、機構を2つのログの突き合わせで確定・修正（#102）

- 経緯: owner の「あー現象が再発したかもしれない?」を受けて確認したところ、**再発は事実**であり、しかも**同じ engine run の中**で起きていた。engine 起動は 08-27 12:06:36.378（`1.0.28-afd34a06422849de`）の1回だけで、再発は 16:03:03–16:03:05。前エントリの「再起動で自然復旧したように見える」は、これで完全に否定された（再起動していないのに再発している）。
- 症状の生データ（開発者履歴、16:03:03–16:03:07）:
  - `16:03:03.064` seq 622 sess=**1** `にt`→`にち`（composing）
  - `16:03:03.171` **`key` IPC timeout**（`ipc-timeouts.bin`、pid 36336 / tid 37696）
  - `16:03:03.329` seq 623 sess=**7** — 158 ms 後に別 session へ切り替わっている
  - `16:03:03.589` seq 625 sess=**1** `にち`→`日` convert（session 1 はまだ生きている）
  - `16:03:05.161` **2回目の `key` IPC timeout**（同一 pid / tid）
  - `16:03:05.195` seq 633 sess=**8** key=2(Space) st=0→0 `act=unbound` **commit 空** ← 既存 fence が吸収できた
  - `16:03:05.235` seq 634 sess=**7** `にt`→`にち`（session 7 はまだ composing）
  - `16:03:05.658` seq 635 sess=**8** key=2(Space) st=0→0 `act=unbound` **commit=`　`** ← **これが症状**
  - `16:03:06.121`–`16:03:07.202` seq 636–645 Backspace 10連打（owner が手で消している）
- 根本原因: **50 ms の `KEY_BUDGET`（`crates/sakura-tsf/src/engine.rs:51`）を超えた SendKey で DLL が pipe を落とし、reconnect が新しい空 session を作る。このとき composing 中の session は「読みが生きたまま」破棄され、`CompositionFence` の claim が解放される。** 次の Space は idle な新 session に着地し、そこでは「かな入力中の idle Space は全角スペースを確定する」という**正しい**規則が走って `　` になる。engine の1キー単位の判定はどれも正しく、壊れているのは**経路の切り替えと fence の解放の組み合わせ**だった。
- 決定的だったのは**2つの独立したログの突き合わせ**である。開発者履歴だけでは「session 番号が飛ぶ」しか見えず、`ipc-timeouts.bin` だけでは「timeout があった」しか見えない。両方を時刻で並べて初めて、timeout → 158 ms 後の session 切り替え → 破棄 → `　` という因果が1本につながった。
- 既存 fence が半分は効いていたことも同時に分かった: `05.195` の1発目は session 7 が生きていたので吸収され、`05.658` の2発目は session 7 が消えた後なので素通りした。つまり**重なり期間は元から守られており、守られていなかったのは破棄そのものの瞬間**だった。
- 修正: `CompositionFence` に**一発限りのラッチ**を足した。読みが生きたまま claim が壊れた場合（`release_after_teardown`）だけ、その host に「次の idle Space を1回だけ吸収する権利」を立てる。commit / cancel / replace のような正常終了は従来どおり `release` で即座に解放し、何も立てない。
  - **壁時計を使わない。** 最初は 1,000 ms の grace で実装したが、独立オラクル（`space_key_dispatch_oracle.rs`）は logical time しか持たないため、実装とモデルが「2イベント間に1秒以上空いたとき」だけ食い違う。PBT がその形の flake を出す。回数で切ると壁時計が消え、モデルと実装が**厳密に**一致する。
  - **境界**: 1 teardown につき 1 Space。2発目は普通に入る。文字は絶対に食わない。再び composing すれば（`acquire`）ラッチは解除される。
- 途中で自分が入れたバグ（新テストが検出した）: ラッチを `any_active` から見えるようにしてしまった。`any_active` の呼び出し側は **idle の全キーを吸収する**（Electron 二重配送用）ので、reconnect 後の session が打った文字まで消えた。`production_typing_after_a_crash_restart_still_composes` と `production_only_absorbs_the_first_space_after_a_crash_restart` が落ちて発覚。**2つの fence は読み取り口を分ける**のが正しい設計だった。
  - `any_active` = 生きた claim のみ。全キー抑止に使う。
  - `consume_teardown` = 壊れた claim。**聞いた時点で消費する**。Space が全角スペースになる、その1打鍵にだけ使う。
  - probe 経路（`dispatch.rs` の `probe_session`）は「適用しないキーが何をするか」を答えるだけなので、`absorb_teardown_space: false` を明示で渡し、ラッチを消費させない。
  - 実キー側の述語は `Space` かつ ctrl/alt なし・`State::Idle`・`keymap.lookup(State::Idle, key).is_none()` で、生データの `key=2 / st=0→0 / act=unbound` と**完全に一致**する。既定・MS-IME 双方の keymap で idle の Space は未束縛（`henkan` だけが `reconvert` に束縛）なので、この述語は出荷 profile 全部で成立する。
- 仕様変更を明示した（テストの assertion を黙って書き換えていない）: `crash_restart_forgets_composition_and_does_not_convert_later` は「crash/restart 後の Space は**文書に入る**」を要求していた。これは #102 の症状そのものなので、要求のほうを変えて `REQ-SPACE-09` として `verification/space-key-dispatch/requirements.md` に追記し、テスト名と doc comment に反転の理由を書いた。元の安全性（「破棄された composition は後から convert しない」）は無傷。
- 検証: `cargo fmt --all -- --check` exit 0、`cargo test --workspace` **1,715 passed / 0 failed / 82 ignored**、`git diff --check` exit 0、cargo／rustc／テストランナーの残存プロセス 0。オラクル側 C2 は atom を 11 に増やし（`ATOM-PENDING-TEARDOWN`）、固定 seed の PBT walk が両極性を自力で踏んだ（補助シーケンス不要、`coverage/c2-report.md` 再生成済み）。`oracle_and_production_agree_on_single_connection_sequences` も緑。
- 学び1: **モデルに載らない機構は選ばない。** 壁時計の grace は production 単体では正しく書けるが、独立オラクルが表現できない。表現できない差分は「PBT がたまに落ちる」という形でしか現れず、原因を追う側からは flake に見える。**モデル化可能性を実装方式の選択基準に入れる**と、この種の負債を最初から作らない。
- 学び2: **緩い fence と厳しい fence を1つの述語に相乗りさせない。** 「composing 中は全キー抑止」と「破棄直後は Space 1発だけ抑止」は、抑止する対象の広さがまったく違う。同じ `any_active` から読ませた瞬間、後者が前者の広さを継承して文字を食った。**読み取り口の分離がそのまま被害範囲の分離**になる。
- 学び3: **仕様と衝突したテストは、まず「どちらが正しいか」を言語化する。** `production_crash_restart_does_not_keep_the_old_composition` が落ちたとき、assertion を反転すれば緑にはなった。だがその assertion は #102 の症状を要求していたので、**要求カタログに新しい ID を起こして反転理由を残す**のが正しい。緑にする手段としては同じでも、記録が残るかどうかで次の人の判断が変わる。
- 学び4: **単独のログで因果を主張しない。** 開発者履歴の「session 番号が飛ぶ」は結果であって原因ではなく、`ipc-timeouts.bin` の「timeout があった」も同じ。**別々の機構が別々のファイルに書いた記録を時刻で突き合わせる**と、片方だけでは推測でしかなかった経路が1本に決まる。今回はこれで仮説検証を1周で終えられた。
- 副次対応: `%LOCALAPPDATA%\SakuraInput\logs\debug.tsv` が上限 2 MiB（2,097,110 B）に達して **2026-08-19 12:41 から fail-closed** で書き込みを止めていた。そのため今回の再発時刻に `idle_fence` trace が1件も無く、機構の特定を履歴と timeout ログだけでやる羽目になった。`debug-full-20260819-124100.tsv` へ改名して退避（**削除していない**）し、次回の発生では trace が残るようにした。あわせて trace の decision に `absorb_teardown` を追加したので、再発時にどちらの fence が効いたかがログから直接読める。
- 未了: (1) **なぜ 50 ms `KEY_BUDGET` を超えるのか**は未調査。累計 `key` timeout は 44 件で、今回の修正は**超過したときの被害を止めるだけ**であり、超過そのものは減らない。(2) `crates/sakura-ipc/src/diagnostics.rs` の `DisconnectReason` WIP は未使用アイテム2件が `-D warnings` で落ちるため**引き続き未コミット**。今回の実測で原因が「原因不明の disconnect」ではなく「key IPC timeout」と判明したので、この診断が今も最適な次の一手かは要再検討。(3) `verification/space-key-dispatch/` の TLA+ spec・TLC 構成・cargo-mutants・`traceability.json` の hash は **#102 以前のオラクル**を指したままで、別途回し直しが要る（`requirements.md` の冒頭に明記した）。
