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

## 2026-08-27 — #102 push 後の CI 赤は AppContainer フレークの再現、原因箇所を1関数まで絞った（#102）

- 経緯: f8a80c7 を main へ push した直後、`CI` の `Build and test` が赤。落ちたのは `Sandbox access (AppContainer)` step で、`the_pipe_is_reachable_from_a_real_appcontainer_token` が `Fault::UntrustedServer { process_id }`。ローカルの隔離 worktree では audit／fmt／clippy／`cargo test --workspace` の4ゲートが全て真の exit 0 だったが、**この step はその4つに含まれない**（`--ignored` 指定で単独実行されるため `cargo test --workspace` では走らない）。
- 結論: **既知フレークの再現**であり、f8a80c7 の内容が原因ではない。attempt 3 で全 step 緑になり、`Installer` は1回目から緑。最終的に f8a80c7 の CI は success。
- 2026-08-25（1.0.28 リリース）の記録と**発生パターンが完全に一致**する。あのときも同じ test が `UntrustedServer` で落ち、3回目の再実行で緑になった。今回も attempt 1 失敗 / attempt 2 失敗 / attempt 3 成功。
- 途中で自分が誤判定した（記録として残す）: attempt 2 まで見た時点で「2連続失敗だから一過性フレークではない」と owner に報告した。**3つ目の標本を取る前に結論を出したのが誤り。** さらに runner image を突き合わせて matched pair を作り、
  - `20260818.207.1`: control(4eb5d31)=PASS / f8a80c7=FAIL
  - `20260824.214.3`: control(4eb5d31)=PASS / f8a80c7=FAIL
  という「image を統制しても差が出る」表を作って因果の傍証にした。この表自体は正しいが、**既知フレークに対して n=2 の matched pair は何も証明しない**。統制変数を増やしても標本数が足りなければ結論は出ない。
- 収穫（ここが今回の実質的な前進）: 失敗箇所を1関数の**2ステップまで**絞れた。
  - 子（AppContainer 側）は失敗の直前に `classify_client_process(pid)` を呼んで `Ok(MediumOrHigher)` を**成功**で表示している。これは `ProcessHandle::open` → `ProcessToken::open_process` → `classify_token` の3つ。
  - `verify_server_process`（`crates/sakura-ipc/src/security.rs:222`）はその3つに `image_path()` と `policy.matches_image_path()` を足しただけ。
  - つまり **失敗しうるのは `QueryFullProcessImageNameW` による image path 取得か、`matches_image_path` の突き合わせのどちらかだけ**。DACL／mandatory label／token integrity／pipe 名解決は、同じ子プロセスの数ミリ秒前の呼び出しで健全だと実証されている。
  - 親の `engine.cleanup()` assertion は通っているので、engine プロセスは生きていて正常終了した。「engine が死んでいて image path が引けなかった」線も同時に否定される。
- 診断上の欠陥（次に直すならここ）: `crates/sakura-ipc/src/client.rs:138` は `verify_server_process(...).is_err()` と**理由を捨てて** `Fault::UntrustedServer { process_id }` にしている。そのため CI で3回失敗しても「拒否された」以上の情報がログに1文字も残らない。上の絞り込みも、失敗ログではなく `security.rs` を読んで2つの呼び出しの差分を取ることでしか得られなかった。**理由を保持した fault を返すようにすれば、次の1回の失敗で原因が確定する。**
- ローカル再現は不可: この test は本番の well-known パイプが空いていることを要求するため、インストール済み engine が動いているこの機械では実行拒否になる。CI でしか観測できない。
- 未処理（2026-08-25 から持ち越し、今回も未着手）: このフレークの tracking Issue はまだ存在しない（`gh issue list` で該当なし）。発生は2回目。上の絞り込みと `client.rs:138` の診断欠陥を添えて Issue 化する。
- 学び1: **「ローカルで CI と同じコマンドを回した」の"同じ"を数えるときは、job の step 全部を見る。** 4ゲートを緑にして安心したが、`Build and test` job は17 step あり、`--ignored` の単独実行という**定義上 `cargo test --workspace` に含まれない** step がその後ろにいた。workflow を読むときは「自分が回した集合」と「job が回す集合」の差を明示的に列挙する。
- 学び2: **フレークが疑われる対象に対して、n=2 で因果を宣言しない。** 統制（同一 runner image、同一 toolchain、diff が該当領域に触れていないことの確認）をどれだけ積んでも、標本が2つなら既知フレークと区別できない。先に3つ目を取る。
- 学び3: **失敗の再現性が低いときほど、コードを読んで"失敗しうる箇所"を機械的に差分で絞るほうが速い。** 今回は成功パスと失敗パスが共有する呼び出し（`classify_client_process`）を見つけたことで、5つの候補ステップが2つになった。ログを増やさずに絞れた分がそのまま次回の調査コストを下げる。

## 2026-08-27 — #102 の teardown latch を実機へ導入、新ビルド稼働を確認（#102, #104）

- owner 承認（「ビルドして再インストール」）に基づき、#102 の修正を実機へ入れた。`cargo build --workspace --release` exit 0（41.42s）、`scripts/build-installer.ps1` exit 0。
- 成果物 identity: version 1.0.28 / build_id `e45b10ede7a589ee` / Inno Setup 6.7.3 warnings 0 / 24,822,311 bytes / SHA-256 `64c091702fb71aab7904bad1197ba45c01e34f84f7dbacc445870c036afe9185`。
- 再インストール: `installer/out/reinstall-20260827-issue102-teardown-latch.log` に `Installation process succeeded` と `Log closed` の両方あり。wrapper pid 22048 は待たずに自然終了済み（既知の「wrapper が本体より長生きする」パターンには当たらなかった）。`sakura_regtool.exe --configure-diagnostics` と `--enable-profile` はいずれも exit 0、再起動要求なし。
- **新ビルドが実際に稼働していることの確認方法**: `sakura_settings.exe history stats` の `history-release-label` が `1.0.28-e45b10ede7a589ee`。あわせて `live-engine yes` / `history-service-active yes` / `dropped-events 0` / `persistence-failures 0`。`update status` は `current-version 1.0.28`。
- 稼働確認で詰まった点（次回のために記録）: 非 elevated shell からは engine の image path が引けない。`Get-Process -Name sakura_engine | Select Path` も CIM の `ExecutablePath` も**空文字**で返る。`C:\Program Files\Sakura Input\versions\` には旧 `1.0.28-afd34a06422849de` と新 `1.0.28-e45b10ede7a589ee` が**両方**残るので、ディレクトリの存在は稼働の証拠にならない。`history stats` の release label が唯一の安価な確証。
- プロセス構成の観察: 再インストール直後 17:18:30 / 17:18:38 に `sakura_engine.exe` が3個（42188, 39932, 43680）現れ、数十秒で 1 個に収束した。親はいずれも `sakura_renderer.exe`（pid 43476, 17:18:30 起動）。定常状態は engine 1 + renderer 1。
- IPC timeout counters の前後比較（`diagnostics show text`、再インストール前 16:13 → 後 17:22）: 合計 103 → 105、**key 44 → 44（増分ゼロ）**、ui-placement 53 → 54、administration 5 → 6。administration の増分は下記の自分の `history export` 失敗そのもの。観測窓は数分なので key timeout の頻度低下を主張する根拠にはならない。#102 の修正は被害を限定するものであって発生頻度を下げるものではない、という整理は変わらない。
- 別件の新規発見（#102 とは無関係、未 Issue 化）: `history export` が `flush input history through engine: the engine did not answer in time` で安定して失敗する。一方 `history stats`（atomics のみ）と `history mine`（read-only snapshot）は成功する。切り分けの結果、engine 不調ではなく**store のサイズによる予算超過**である。
  - `%LOCALAPPDATA%\SakuraInput\history\input.bin` は 16,395,636 bytes。read-only の `history mine` ですら **8.7 秒**かかる。
  - `crates/sakura-settings/src/input_history.rs:22` の `ADMIN_CALL_BUDGET` は **2 秒**。`export` だけが `Request::FlushInputHistory` を経由し、engine 側 `input_history.rs:911` の `flush()` は writer thread への `receiver.recv()` を**無期限で**待つ。writer thread の作業が 2 秒に収まらなくなった時点で export だけが落ちる。
  - 同ファイルの `mine` には「read-only なので live engine を塞がない」という既存コメントがあり、設計上その区別は意図されている。今回はその境界のおかげで切り分けが 3 コマンドで済んだ。
  - 2026-08-27 16:06 時点では export は成功していた（5,674,299 bytes の TSV を出力）。store 成長 → 予算超過という順序と整合する。
- AppContainer フレークは Issue #104 として登録した（https://github.com/tsuyoshi-otake/sakura-input/issues/104）。同日の前エントリにある「tracking Issue 未着手」はこれで解消。Issue 本文には `client.rs:138` の診断欠陥と「失敗しうるのは image path 取得か path 突き合わせの2つだけ」という絞り込みを添えた。
- 直後の push `9d5f0e6`（journal のみの変更）の CI は **1回目で success**、Installer も success。f8a80c7 も最終的に両方 success。
- 学び: **「新しいバイナリを入れた」と「新しいバイナリが動いている」は別の主張で、後者には専用の確認経路が要る。** この製品では versions ディレクトリもプロセスの image path も答えにならず、engine 自身が申告する release label だけが確証になる。導入作業の完了条件に、その申告値の突き合わせを含める。
- 学び: **同じ subsystem に read-only 経路と同期経路の両方があるなら、失敗時はまず read-only を叩く。** 「engine が死んでいる」と「同期の予算が足りない」は症状が同じ timeout だが、read-only が通るかどうかで一発で分かれる。

## 2026-08-27 — 1.0.29 リリース（同音語・単漢字・候補数上限・句読点・IT用語の読み）（#94, #95, #96, #97, #99, #101, #102, #103）

- 内容: de20699 をリリース化した。版は `Cargo.toml`、`Cargo.lock`、`installer/setup.iss`、`release.yml` 既定値の4か所に加えて、**`tools/candidate-sweep` の5か所目**。ノートは `docs/release-notes-v1.0.29.md`（1.0.28 のノートを rename）。
- 5か所目の理由（1.0.28 では不要だった）: `tools/candidate-sweep` は shipping workspace の members に入っていない入れ子 workspace だが、その `Cargo.lock` は `sakura-core` と `sakura-proto` を path で pin している。本体だけ上げると lock が 1.0.28 のまま stale になり、次に sweep を走らせた瞬間に cargo が自分の lock を書き換えて作業ツリーを汚す。**members に入っていない = 版を上げなくてよい、ではない。path 依存を持つ入れ子 lock は release 時に一緒に上げる。**
- ローカルゲート: 隔離 worktree `C:\Codes\_relgate29` で実行（作業中の `crates/sakura-ipc/src/diagnostics.rs` を除外するため）。fmt exit 0、`clippy --workspace --all-targets -- -D warnings` exit 0、`cargo test --workspace` exit 0 で **1,715 passed / 0 failed / 82 ignored**（93 test binary）、`git diff --check` 0、dep-policy 73 packages（disallowed なし）、release-workflow-policy 7 action 参照。実行後の cargo/rustc 残存 0。
- artifact: `sakura_setup.exe` 24,820,776 bytes、SHA-256 `fe15e04344b32a507e50d5cc9cca7e0af150e13e2b275424745c2cc06b76b2a4`、build_id `020f3e59d16e8ff6`、`Get-AuthenticodeSignature` は `NotSigned`、`signing-status.txt` は `unsigned-owner-approved`（owner 承認済みの未署名リリース）。`Build v1 release candidate` は 9分50秒、`Sign and package` は 54秒。
- 公開手順の検証: manifest の sha256／size、ビルド成果物の実測値、**Release へ添付したあと再ダウンロードした実測値**の3者が一致することを確認してから `--draft=false`。bundle の `release-notes.md` がリポジトリの `docs/release-notes-v1.0.29.md` と完全一致することも `diff` で確認した。
- CI: `Release candidate` と `Installer` は1回目で success。`CI` は1回目が step 14 `Sandbox access (AppContainer)` で失敗（#104 のフレーク）、`gh run rerun --failed` で全 job success。公開は 2026-08-27T09:26:32Z、tag `v1.0.29`、assets 2件（`sakura_setup.exe`／`release-manifest.txt`）。公開後、manifest の `installer_url` が返す実ファイルの SHA-256 が manifest と一致することも確認した（updater が実際に取りに行く URL の確認）。
- **#104（AppContainer フレーク）の発生が4回目になった。今回いちばん重要な観測**: 直前の `63b77ea` は **journal の .md 1ファイルしか触っていない commit** なのに、同じ step 14 `Sandbox access (AppContainer)` で落ちた。コード差分ゼロで再現したので、「差分が該当領域に触れていない」どころか「差分が Rust コードを1バイトも含まない」条件での再現であり、環境依存であることのこれまでで最も強い証拠になる。2026-08-27 の前エントリで作った matched-pair 表（n=2）よりこの1件のほうが情報量が多い。
- ノートに書かなかったことも記録する: 50 ms のキー予算を超える原因そのものは未解明で、#102 は超えたときに全角スペースが入る被害を止めるだけである。候補数上限の設計（読み長 clamp か遅延 N-best 展開か）は #100 / #103 で未決のままで、このリリースは読み長 clamp を採っている。いずれもリリースノートの「含まれないもの」に明記した。
- 学び: **リリース前の版の書き換え漏れは、grep 対象をリポジトリ全体にすれば機械的に見つかる。** 今回 `1.0.28` を全ファイル grep して初めて入れ子 workspace の1件が出た。「4か所」という手順の記憶ではなく、毎回 grep する。

## 2026-08-27 — #102 の engine セッション破棄経路を全件命名し、計器を追加（#102）

- 背景と root: `TextService::disconnect()` は**引数なし**で、production 21 経路とテスト fixture 2 経路が同じ呼び出しを共有していた。#102 は「変換直後に engine セッションが消え、次の Space が全角になる」問題だが、実際にどの経路が走ったのかを製品側で区別する材料が1バイトも残っていなかった。1.0.29 の teardown latch は被害を止めるだけで、原因特定の材料は増えていない。
- 対処: `fn disconnect(&self, reason: DisconnectReason)` へ変更し、23 か所すべてを周辺コードから命名した。**任意注釈ではなく必須引数にしたのが要点**で、新しい reset 経路を足すときに名前を決めないとコンパイルが通らない。
- 記録先: 既存 timeout log の隣に2本目の bounded log（`ipc-disconnects.bin`、1 MiB 上限、32 byte 固定長）。両者で record codec を共有したので、record 形式の変更が片方だけに入って気付かれない事態がなくなった。
- **設計上の落とし穴（テストで固定した）**: wire code 1..10 は timeout / disconnect の**両方で有効**な値である。分けているのは magic（`SKTO` / `SKDC`）だけで、誤って相手のリーダに読ませると「別の意味のカウンタが黙って増える」。両方向を固定するテストを入れた。
- 表示: `sakura_settings.exe diagnostics show` と設定 GUI が2表を**別々に**出す。合算しない — reset の大半はホストのライフサイクルに対する正しい応答なので、timeout に足すと fault 件数として読まれて誤りになる。件数 0 の理由も全部印字する（「一度も走っていない」と「計測していない」は別の答えである）。
- テスト隔離: `note_disconnect` に `#[cfg(test)]` 版を付け、`%LOCALAPPDATA%` ではなくシステム temp へ書かせた。これを忘れると `cargo test --workspace` の 1,721 テストが実ユーザーの診断プロファイルへ追記し、**これから測ろうとしている当の数値を汚す**。既存の `note_timeout` が同じ形をしていたことだけが手掛かりだった。
- 命名変更: settings 側の `diagnostics::load` / `clear` は2種類を扱うようになったため `load_timeouts` / `clear_timeouts` へ改名。同じ関数を GUI (`ui.rs`) も使っていたので同時に更新した。CLI だけ直して GUI を見落とすと、片方だけ reset が見えないビルドになる。
- `cargo fmt --all` を意図的に dirty な作業ツリーで走らせた。直後に `git diff --stat` で**触れたファイルが自分の7件だけであること**を確認している。この確認をしないと fmt が owner の既存 WIP を巻き込んで整形しうる。
- 検証: `cargo fmt --all -- --check` 0、`cargo clippy --workspace --all-targets -- -D warnings` 0、`cargo test --workspace` **1,721 passed / 0 failed / 82 ignored**、`git diff --check` 0。実行後の cargo/rustc/テストランナー残存 0（残っていたのは実機稼働中の `sakura_engine` pid 34596 と `sakura_renderer` pid 44148 のみ）。commit `58bb70c`。
- 未解決（このコミットは前進させていない）: 50 ms の `KEY_BUDGET` を**なぜ**超えるのかは依然不明で、key timeout は累計 44 件のまま。今回入れたのは原因を絞るための計器であって、発生頻度を下げる変更ではない。実データは次版を実機へ入れて初めて集まる。
- 未解決: `crates/sakura-ipc/src/client.rs:138` が `verify_server_process` の理由を捨てている件（#104 の最短経路）は今回も未着手。
- 学び: **同じ終了処理を複数の理由で呼ぶ関数は、理由を必須引数にすると計測が「後付け」でなくなる。** optional な注釈にしていたら 23 か所のうち何か所かは必ず「あとで埋める」まま残った。型で強制すると埋め忘れが commit 前にコンパイルエラーとして出る。
- 学び: **本番コードが per-user の永続ファイルへ書くなら、その writer にテスト用の差し替え経路が要る。** これは「テストが汚れる」問題ではなく「本番の観測データが汚れる」問題であり、被害はテスト実行が終わったあとに残る。

## 2026-08-27 — #104 の拒否理由を保持する（4回失敗して証拠ゼロだった件の是正）（#104, #102）

- 症状: AppContainer sandbox test が CI で **4回**落ちているのに、ログに残るのは `UntrustedServer { process_id }` だけで、5つある検査step のどれが拒否したのか分からない。前エントリで「失敗しうるのは image path 取得か path 突き合わせの2つだけ」まで絞ったが、それは失敗ログからではなく `security.rs` を読んで導いたものだった。
- root cause: `crates/sakura-ipc/src/client.rs:138` が `verify_server_process(...).is_err()` で理由を捨てていた。**さらに悪いことに、理由を捨てなくても足りなかった**: `verify_server_process` の5 step はすべて `ERROR_ACCESS_DENIED` になり得て、うち2つ（path 不一致・integrity 不足）は自分でその HRESULT を**合成**していた。つまり HRESULT を通しても step は区別できない。
- 対処: `verify_server_process` の戻りを `Result<(), ServerRejection>` に変更。variant は1 call ないし1 policy 判断に対応し、OS error code 以外の情報を持たない（そのまま印字して安全）。`Fault::UntrustedServer` が理由を運び、Display に出す。
- **この enum が引きたい線**: 「no と答えられた質問」と「質問できなかった質問」を分ける。`ImagePathRejected` は別のプログラムが pipe を提供している証拠であり、`ImagePathUnreadable` は**何が提供しているのか分からなかった**という意味でしかない。security finding なのは前者だけで、#104 が環境フレークだという仮説と整合するのは後者。この区別が付かない状態で4回調査していた。
- appcontainer test の panic message に理由と**その読み方**を書いた。次の CI 失敗1回で #104 は決着するはずである。
- 副産物（別の誤用を発見）: `crates/sakura-renderer/src/watch.rs` の2か所が、`current_exe()` の失敗と install root への親辿り失敗に対して `Fault::UntrustedServer { process_id: 0 }` を返していた。**どちらも peer の判定ではない** — policy がまだ作れておらず、server に何も尋ねていない段階の失敗である。`PolicyUnavailable` に変更した。放置すると「untrusted server process 0」というログが出て、実際は自分の install layout が読めなかっただけ、という誤誘導が起きる。
- 検証: fmt 0、`clippy --workspace --all-targets -- -D warnings` 0、`cargo test --workspace` **1,723 passed / 0 failed / 82 ignored**（新規2件）、`git diff --check` 0、cargo/rustc 残存 0。commit `99f5674`。
- 注意: appcontainer test 自体は ignored 集合にあり、CI の `--ignored` 単独 step でしか走らない。ローカルの 1,723 件には**含まれていない**。新しい message はここではコンパイルされただけで、実行されるのは CI である。
- 学び: **理由を捨てているコードを見つけたら、理由を通すだけでは足りないことがある。** 今回は `.is_err()` が最初の容疑だったが、上流が5つの異なる失敗に同じ HRESULT を割り当てていたので、呼び出し側を直しても何も分からないままだった。「情報が失われているのはどこか」を、捨てている行だけでなく**生成している行まで**遡って確認する。
- 学び: **エラー型を使い回すと、無関係な失敗が security 判定に化ける。** watch.rs の2か所は「他に手近な Fault variant がなかった」だけで untrusted 判定を名乗っていた。variant 名が主張になっている型では、便宜的な流用がそのまま虚偽のログになる。

## 2026-08-27 — key timeout 44件の実データ解析：安易な内部lock仮説を2つ否定（#102, #106）

- 動機: #102 の計器を入れたので、既存の 109 件（6.33日分）を実際に読んだ。集計だけでなく**生レコードを時刻でクラスタリング**した。
- 内訳: key 44 / ui-placement 54 / administration 10 / handshake 1。件数最多は key ではなく **ui-placement**（budget は 10 ms と小さい）。
- **決定的だったのは形**（1秒以内のレコードをクラスタ化、83クラスタ）:
  - `key` の burst は **tids=1** で 139〜**1777 ms** に跨り、同一スレッドが2〜4回連続で 50 ms を超える。→ 単発の 50 ms 超過ではなく**秒級の停止**である。
  - `ui-placement` の burst は **tids=4 で +0 ms**（4スレッドが同一ミリ秒）。→ engine 内部の lock 待ちの形ではなく、複数接続が同時に待たされている形。
  - 種別が混在するクラスタは 83 中 **2 件のみ**。→ key と ui-placement は**別現象**であり、まとめて1つの原因を探してはいけない。
- 否定した仮説1: **learning の mutex**。`LearningService` は `Mutex<State>` を持ち、5秒周期の maintenance thread がそれを保持したまま fsync/compaction する。key path（`dispatch.rs` 1827/2417/4646/5298 の `is_repair_suppressed` / `learn` / `preference`）は blocking `.lock()` を取るので、構造としては競合しうる。しかし実機の `log.bin` は **419 KB** で compaction trigger（16 MiB）に遠く届かず、走るのは 419 KB の `sync_data` だけ。数 ms であり **1777 ms を説明しない**。`maintain()` が `try_lock` なのは取得を譲るだけで保持時間は縛らない、という指摘自体は正しいが、実機では発火していない。
- 否定した仮説2: **neural reranker の 500 ms**。`WORKER_RESPONSE_TIMEOUT` は 500 ms で KEY_BUDGET の10倍だが、`score()` の呼び出しは `long_conversion.rs:311`、すなわち `worker_loop`（background thread）内である。key path はここで待たない。
- **判明した計器の限界（これが本題）**: 32 byte レコードは「budget を超えた事実」しか持たず、**超過量を持たない**。51 ms と 1800 ms が同じ 1 件として記録される。今回クラスタ間隔から秒級だと推定できたのは偶然 burst していたからで、単発 68 件については何も言えない。原因候補の絞り込みが推測で止まる直接の理由がこれである。
- 提案（未実施・owner 判断待ち）: レコードの **bytes 24..28（現在 reserved、コメントは「future request-id discriminator」）に実測 elapsed ms を書く**。現行 reader はこの4バイトを無視して 0 を読むので、**FORMAT_VERSION を上げずに後方互換にできる**。上げると既存 109 件（6.33日分）が invalid 扱いになり、いま解析した実データを失う。
- 作業ツリーの状況: 解析中に owner が同ツリーで `AiStyle::English` 系の WIP（`sakura-ai-proto` / `sakura-ai-worker` / `ai_text.rs` / `user_preferences.rs` / `ui.rs` / `README.md`）を進行させていた。永続フォーマットを変える変更を並行して勝手に入れない判断をした。
- 学び: **診断カウンタは「何回」だけでなく「どれだけ」を持たないと、原因候補を1つも消せない。** 今回は生レコードの時刻クラスタリングという副次的な情報で辛うじて秒級と分かったが、それは burst したときだけ効く手であり、単発事象には無力だった。閾値超過を記録する計器を作るときは、閾値と実測値の差を同じレコードに入れる。
- 学び: **「構造上ありうる競合」と「実機で発火している競合」を混同しない。** learning mutex は設計としては本当に危険な形（保持時間が無制限の critical section をキー経路と共有）だが、実機の log サイズを見た瞬間に今回の原因ではないと確定した。コードだけ読んで直していたら、無関係な箇所を触って「直った」と誤認していた。

## 2026-08-27 — key timeout の原因候補は辞書マッピングの trim と再フォールト（#107, #102）

- 経緯: owner から「複数スレッド／SIMD／O(n)／非同期／バッファ・メモリは使えないのか」と順に問われ、それぞれ実測で答えた。結果、**最後のメモリ案だけが「機構が実在して数字が異常」な方向**だった。
- 非同期・複数スレッド: 既に入っている。接続ごと worker、prediction/reranker 専用スレッド、maintenance は `try_lock` で譲る、設定は `RwLock` でキー処理中に握らない。伸びしろではない。
- SIMD・アルゴリズム: 狙う場所が違う。**計算のない `ui-placement`（キャレット矩形を渡すだけ、budget 10 ms）が4スレッド同時・同一ミリ秒で落ちている**。最適化対象の algorithm がそもそも存在しない。SIMD の 2〜8倍では 1777 ms を説明できない。
- 実測（稼働中 engine pid 34596、起動 19:29:27、稼働1時間未満）:
  - `system.dic` = 47,561,532 bytes（45.4 MiB）を `dictionary.rs:476` の `MapViewOfFile` / `PAGE_READONLY` でマップ
  - WorkingSet **80.8 MB** / Peak **112.6 MB** → **31.8 MB が trim 済み**
  - 累計 PageFaults **666,548**（45.4 MiB ÷ 4 KB ≈ 11,600 ページの**約57倍**）
  - アイドル時 **0 fault/sec**（1秒×6サンプル）→ 背景の垂れ流しではなく**活動中にまとまって**発生
- 仮説: clean な read-only ファイルマッピングは Windows が真っ先に回収する（書き戻し不要）。アイドル中に trim → 入力再開で再フォールト → hard fault がまとまると数百ms〜秒級 → engine が応答せず待機中の全スレッドが budget 超過。稀・1スレッドで burst して収まる・計算のない処理まで巻き込む・アイドル明け、という観測4点すべてと整合する。
- **未確定（Issue に明記した）**: 666,548 のうち hard fault の割合が未測定。soft fault ならマイクロ秒で痛くない。timeout との時刻一致も未証明。よって原因と断定していない。
- 打ち手候補: (1) hot/cold でマッピング分割（45 MiB には #28 の detail 29,229件、関連語、#48 の boundary table が同居。ホットパスが index+entries だけなら常駐セットが大幅に縮む）、(2) `PrefetchVirtualMemory` で数千回の個別フォールトを1回の I/O に、(3) `SetProcessWorkingSetSizeEx` は効くが 15 MiB 方針と衝突、(4) large pages は権限・非ページング常駐のため IME に不適。
- 成果物: #107 起票、#102 へ相互参照コメント。owner が検討する。
- 学び: **owner の「〜は使えないのか」という提案は、否定するにも実測で答える。** 3案のうち2案は実測で否定できたが、否定の根拠は毎回コードではなく数字だった（既に入っている構造 / 計算のない処理が落ちている事実）。そして3案目は owner が正しく、私はそこを見ていなかった。**提案を評価する順序を「ありそうか」ではなく「機構が実在するか・数字が異常か」にすると、当たりが速く出る。**
- 学び: **プロファイルを取る前に、プロセスのワーキングセットと page fault 数を見る。** 今回は 2コマンドで peak−current の trim 跡と 57倍の fault 数が出て、それだけで探索範囲が計算系から memory 系へ移った。CPU プロファイラを先に回していたら、0 fault/sec のアイドルと burst 型の停止を見落としていた。

## 2026-08-28 — 1.0.30 を診断専用リリースとして公開（#102, #104, #106, #107）

- 内容: 1.0.29 以降の製品コード変更は `58bb70c`（#102 セッション破棄21経路の命名と2本目の bounded log）と `99f5674`（#104 拒否した検査step の保持）の2件だけ。**入力の挙動は一切変わらず、不具合修正も含まない。** それでも出す理由は、#102 の disconnect カウンタは実機へ入らないとデータが1件も集まらないからである。
- リリースノートはこの性質を冒頭で明言した（「診断のためのリリースです。入力の挙動は 1.0.29 から変えていません」）。「含まれないもの」に、50 ms 超過の原因が未解明であること、#107 は**候補であって原因ではない**こと（hard fault 比率が未測定）、timeout 記録が超過量を持たないこと、#106 の traceability 判定が現在のコードについて何も述べていないことを列挙した。
- bump 手順（4箇所＋1）: workspace `Cargo.toml` / `Cargo.lock` / `installer/setup.iss`（product version と versioned payload dir の2行）/ `.github/workflows/release.yml` の default tag、加えて `tools/candidate-sweep`（`Cargo.toml` と `Cargo.lock`。sakura-core と sakura-proto を path で pin しているため置いていくと次回実行時に自分の lockfile を書き換える）。`docs/release-notes-v<version>.md` はワークフローが版一致を要求するので旧版を削除して新版を作る。
- ロック再生成は `cargo update --workspace` を両ワークスペースで実行。差分は 32 行と 6 行で前回リリースと同一の形、**version 行以外の変更 0**。依存を1つも採用していないので7日ルールに抵触しない。この確認を `git diff -U0 | grep -vc 'version = ...'` で数値として取った。
- ゲート: fmt 0、clippy `--workspace --all-targets -D warnings` 0、`cargo test --workspace` **1,727 passed / 0 failed**、`git diff --check` clean、cargo/rustc 残存 0。commit `8a91b24`、tag `v1.0.30`。
- 公開手順の実際: `release.yml` はタグ push で起動するが **GitHub Release を作らない**（`contents: write` を持たず、artifact を upload するだけ）。したがって Release は `gh release create` で手動作成する。成果物 `sakura-input-1.0.30-release-candidate` から `sakura_setup.exe` と `release-manifest.txt` を取り出して添付した。
- 署名: `signing-status.txt` は `unsigned-owner-approved`。**未署名**であることを確認したうえでノートにその旨とSHA-256照合手順を記載している。manifest の `sha256=fad57ac0…` が実ファイルのハッシュと完全一致、size 24,815,818 も一致することを添付前に検証した。
- 作業ツリー: owner の WIP 7ファイル（`AiStyle::English` 系）は未ステージのまま保全。`git add` はリリース対象8ファイルを明示列挙し、staged 一覧が前回リリースと同一構成であることを確認してから commit した。
- 学び: **リリース前の `git add` は `-A` ではなくファイル名の明示列挙にする。** owner が同じツリーで WIP を進めている状況では、`-A` は無関係な未完成コードをリリースコミットへ巻き込む。staged 一覧を前回リリースの `--stat` と見比べる確認が有効だった（8ファイル・同一構成）。
- 学び: **ロック再生成後は「version 行以外が変わっていない」を数えて確認する。** `cargo update --workspace` は workspace member だけを更新する建前だが、それを目視ではなく `grep -vc` の 0 で確認しておくと、依存の混入と7日ルール違反を同時に排除できる。

## 2026-08-28 — 1.0.30 を実機へ再インストール、#107 の trim を同一プロセスで追跡（#102, #107）

- 目的: #102 の engine link reset カウンタは実機へ入らないとデータが1件も溜まらない。1.0.29 → 1.0.30 へ入れ替えた。
- 手順: Release の `sakura_setup.exe` を `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /LOG=` で起動し、ログに `Installation process succeeded.` と `Log closed.` の両方が出るまで待つ。Exception/Error/failed 行は 0。`Start-Process -Wait` を使わずに `-PassThru` で PID を取り、後からラッパー残存を確認する形にした（今回は残らず）。導入先 `versions.0.30-c9626adf450a3db9`、旧 1.0.29 は side-by-side で残置。
- 導入後の確認: `diagnostics show` に **Engine link resets の21経路がすべて表示され、全部 0、timestamp は `none`**。件数 0 を省略せず印字する設計どおりに動いている。IPC timeouts は版をまたいで保持され 109 → 110 件（key 44 → 45）。
- **測定（#107）**: インストールで消える直前に、旧 engine（pid 34596、稼働4時間36分）を最後にもう一度測った。peak との差が 31.8 → 38.3 → **42.0 MB** と単調に開き、直近14分で fault が **+134,823**。peak は 130.0 MB で頭打ち。「一度削られて終わり」ではなく削り直しのサイクルが回り続けている証拠になる。hard fault 比率は依然未測定なので**原因は未確定のまま**であり、Issue コメントにもそう書いた。
- 導入直後の観察: sakura_engine が一時的に **5プロセス**、合計 working set 310 MB まで増え、**約2分で1プロセスへ収束**した。各ホストが DLL を読み直して一斉に engine を起動し、パイプ争奪に負けた側が退出する挙動。収束するので不具合とは判断していないが、その間 45.4 MiB のマッピングがプロセス数だけ重複する。#107 に参考として記録。
- 学び: **プロセスを終了させる操作の前に、そのプロセスからしか取れない計測を先に取る。** 再インストールは engine を殺すので、4時間36分ぶんの trim 追跡データはインストールした瞬間に永久に失われるところだった。稼働の長いプロセスは、それ自体が再現の難しい観測装置である。
- 学び: **プロセス数の異常は、増加を見た瞬間に報告せず収束を確認してから判断する。** 5プロセスを見た時点で「増え続けている」と書いたが誤りで、実際は2分で収束した。10秒間隔で7サンプル取るだけで正しい結論に変わった。

## 2026-09-09 — #148 高負荷時の入力ロスレス化 Phase 0–1: 経路マップと engine 内部計装（#148, #102, #107, #134, #141, #142）

- 症状: 高負荷（CPU / メモリ / ディスク I/O / OS scheduler）下で入力文字・preedit・変換状態が失われる、という報告群（#102 / #107 / #141 / #142）。個別 Issue はあるが「高負荷でも入力を失わない」という上位の信頼性 Issue が存在しなかったため、親 Issue **#148** を作成した。既存 Issue を「解決済み」「原因確定」へ書き換えることはしていない。
- baseline HEAD を `5eb40e3`（main, v1.0.38, clean worktree）に固定し、Issue と `verification/high-load-input-integrity.md` の双方へ記録した。**過去の調査結果を再検証なしに現行コードへ適用しない**方針を明記。
- **`57a0b8d` Phase 0**: 物理キー1打の経路を TSF 側 / engine 側の両方について静的に読み取り、段階表・最悪ケースの同期 IPC 往復・`CallbackDeadline` を持たない入口・write journal 満杯時の終端・既存の sequence/generation/revision・`disconnect(reason)` の21経路を文書化。仮説 H-A〜H-F を「仮説」として明示。
- **`3592204` Phase 1(1)**: `Client::last_call_elapsed()` を追加し、timeout 診断レコードが**実際に待った時間**を保持するようにした。TSF 側 16 箇所の `note_timeout` が渡す。これで `Fault::DeadlineExpired`（budget 切れ）と `Fault::Timeout`（engine 無応答）が事後に区別できる。従来はどちらも「timeout 1件」で、50 ms の内訳が復元不能だった。
- **`fe0b95f` Phase 1(2)**: `crates/sakura-engine/src/timing.rs` に固定 9 site の lock-free accumulator（`AtomicU64` × 3、RAII `Span`）を追加。`request-total` / `runtime-services-lock-wait` / `runtime-services-total` / `configuration-snapshot` / `dispatch` / `encoding` / `reply-write` / `learning-lock-wait` / `debug-trace-emit`。`Request::EngineTiming` で読み出し（`PROTOCOL_VERSION` 20 → 21）、`sakura_settings.exe diagnostics timing [text|tsv]`。
- 仮説分離の狙い: `runtime-services-lock-wait` と `-total` の差が **H-E**（process-wide `dynamic_runtimes` mutex による他コネクション待ち）を、`debug-trace-emit` が **H-F**（developer mode が reply path に per-key ファイル書き込みを足す）を測る。H-F は #102/#107/#141/#142 のレポート採取時に有効だった設定そのものなので、**レポートが engine ではなく計測器を測っていた可能性**を検定する。
- 併せて `LearningService` の state mutex 取得を 9 箇所から `lock_state()` 1 箇所へ集約（`compact_state` が log 全体の replay+rewrite の間 lock を保持するため、その後ろの待ちは外から「遅い変換」と区別できない）。`sakura-settings` の administrative connect/handshake/fault 分類の重複を `engine_admin.rs` へ集約。
- 検証: fmt / clippy `-D warnings` / `./ci/run-test-quiet.ps1` workspace tests / `git diff --check` すべて PASS。cargo・rustc の残存 0（稼働中の `sakura_engine` / `sakura_renderer` は `C:\Program Files\Sakura Input\versions\1.0.37-...` の実機導入分でテスト残骸ではないことを Path で確認）。
- **未実施を明示**: 負荷下での実測採取（測定値は現時点 0 件）、ETW、fault injection、stress harness、PBT、mutation、TLC、release build、実ホスト E2E。`queue_wait_us` / `conversion_us` / `dictionary_us` / TSF 側 15 タイムスタンプ / `key_seq`・`document_revision`・`composition_generation` の protocol 露出も未実施。**#107 の page-fault 仮説は ETW 実測なしに原因確定としない**、を Issue コメントと文書の両方に書いた。
- 学び: **計装は「常時オン・saturating・`samples` 独立・累積」の4点で設計する。** 有効化してから記録する診断は稀な最初の 1 回を取り逃す。wrap した総和は停止した engine を高速な engine として報告する。`samples` を持たないと「到達しなかった」と「0 µs だった」が同じ 0 になる。リセットしないので stress 前後の差分がそのまま使える。
- 学び: **「engine 不在」は値であって、エラーでもゼロ表でもない。** `TimingSnapshot::EngineNotRunning` を独立の variant にしないと、stress harness が「何も測れなかった」を「何も遅くなかった」として記録してしまう。
- 学び: **計装 site の集合が partition でないなら、それを型のドキュメントに書く。** `request-total` は他を含み `runtime-services-total` は自分の lock wait を含むので、合計に意味はない。後から読む人が足し算をする前に止める必要がある。
- 学び（作業環境）: **このマシンでは `perl -0pi -e 's|...|...|'` による複数行 in-place 置換が信用できない。** 置換ではなくファイル先頭への追記になったり、`format_args!` の内部へ差し込まれたりした（2回発生）。行番号指定の `sed` か、`head`/`cat`/`tail` による splice、または `sed -i 'Nr file'` を使う。`sed -z` の複数行パターンも、同じ形の別箇所（今回は導入した helper 自身の本体）を巻き込んで無限再帰を作ったので、置換対象が一意であることを確認してから使う。

## 2026-09-09 — #148 高負荷時の入力ロスレス化 Phase 2（決定論的遅延注入 + stress harness）

- **Issue / commit**: #148 / `ad47c1b`（Phase 1 は `fe0b95f`, `c7c435a`）
- **症状（未解決）**: 高負荷下で入力文字・preedit・変換状態が失われる報告。
  Phase 1 で engine 側 per-stage timing は入れたが、「engine を実際に止めたとき
  入力が生き残るか」は測れていなかった。
- **根本原因**: 未確定。本 Phase は原因を確定していない。
- **やったこと**:
  - `crates/sakura-engine/src/fault_injection.rs`（新規）。4 点
    （before-dispatch / during-conversion / after-mutation / during-reply）へ
    release ビルドに残る sleep 注入点を追加。
  - arming gate: `--fault-injection <spec>` は `--test-pipe`（既存の
    `validate_test_pipe()` で `\\.\pipe\SakuraInputEngineTest-` に限定）が
    同時にあるときだけ受理。なければ **起動拒否**。env var / file 経路なし。
    `install()` は 2 回目を拒否（one-shot）。
  - `Request::FaultStatus` / `Response::FaultStatus` を追加し
    `PROTOCOL_VERSION` 21 → 22。payload は 4 件固定のカウンタのみで content-free。
    `sakura_settings.exe diagnostics faults [text|tsv]`（`engine_faults.rs`）で
    ユーザーが自分の engine が disarmed であることを確認できる。
  - `Engine::spawn_isolated_with_faults(spec)` と
    `tests/high_load_key_integrity.rs`（7 tests）。実行ごとに一意な
    `LOCALAPPDATA` / private pipe / 辞書 fixture、synthetic input のみ。
- **検証**: `cargo fmt --all -- --check` / `cargo clippy --workspace
  --all-targets -- -D warnings` / `./ci/run-test-quiet.ps1 -Name 'workspace
  tests' -Command { cargo test --workspace }` / `git diff --check` すべて成功。
  cargo・rustc・test child プロセスの残存なし（残っていた `sakura_engine.exe`
  PID 23540 は 1.0.37 のユーザー実環境 engine で、`--test-pipe` を持たない別物）。
- **学び（実測で得た事実。原因確定ではない）**:
  1. **engine 側の入力欠落は 0。** `during-reply=400` を arm し、クライアント
     budget を 50 ms（TSF の `KEY_BUDGET`）にすると client は諦めるが、
     次のキーの reply には諦めたキーの結果が含まれている。engine は適用済み。
  2. **stale reply は配送されない。** `sakura-ipc` の `client.rs` は
     `header.request_id` を照合し、古い id の frame をループで読み飛ばす
     （新しすぎる id は `Fault::Desynchronized`）。したがって「古い結果を
     新しい request の答えとして適用する」経路は IPC 層には無い。
  3. ⇒ ホストで文字が消えるなら、それは **engine が失ったのではなく TSF 層が
     諦めた結果**。Phase 3 / Phase 5 の対象。
- **設計判断の記録**:
  - 4 点を分けたのは経過時間ではなく **世界の状態** が違うから。特に
    `after-mutation` は「クライアントが諦めたキーを engine は適用済み」という
    唯一の順序で、欠落と重複の双方が起こり得る窓。
  - `fired` / `slept_us` は要求値でなく **実測値**。負荷下の scheduler は
    寝過ごすので、harness が assert すべきは起きたことであって頼んだことではない。
  - spec の parse は fail-closed。黙って何も arm しないと、stress 実行が
    「入力は失われなかった」を、何も注入していない実行について報告してしまう。
  - `during-reply` は key/output の reply 経路だけに置いた。administrative
    reply は遅らせない。よって `fired` は key reply 件数であり全 request 数ではない。
    types.rs の doc comment に明記済み（実装時に test が fired=2 で落ちて判明）。
- **テスト作成時の誤り（自分側）**: `"sakura"` の逐次 preedit を
  `["さ", ...]` と予想したが実際は `["s", "さ", "さk", ...]`（単独子音は
  母音が来るまで roman のまま）。また変換中の `Escape` は composition を
  クリアせず変換前の読みへ戻す。どちらも engine の正しい挙動で、期待値の側が
  誤り。実挙動を確認してから expectation を書くこと。
- **残り**: 負荷下の実採取、ETW 因果確定（H-E / H-F）、correlation ID、
  TSF 側 15 タイムスタンプ、`queue_wait_us` / `conversion_us` /
  `dictionary_us`、Pending/ACK 設計、PBT / mutation / TLC、実ホスト E2E。

## 2026-09-09 — #148 高負荷時の入力ロスレス化 Phase 3 入口（諦めたキーの行方を実測）

- **Issue / commit**: #148 / `8c66824`（Phase 1: `fe0b95f` `c7c435a`、Phase 2: `ad47c1b` `4cb22a9`）
- **症状（未解決）**: 高負荷下で入力文字が失われる報告。Phase 2 で
  「engine 側の入力欠落は 0」まで分かったが、では誰が捨てているのかが不明だった。
- **根本原因**: **未確定**（頻度の原因は ETW 前で断定不可）。ただし
  「timeout が起きたとき何が失われるか」は本コミットで確定した。
- **やったこと（観測のみ。動作変更なし）**:
  - `crates/sakura-engine/tests/high_load_key_integrity.rs` に
    `the_revert_after_an_abandoned_key_discards_a_reading_the_client_never_saw`。
    実 `sakura_engine.exe` に `during-reply=200` を arm し、同一 session へ
    同じ 4 キーを 2 周。3 打目を 50 ms クライアントとして諦める。
    `Revert` の有無だけが違う。
  - `crates/sakura-tsf/src/engine.rs` に
    `the_key_after_a_timeout_sends_revert_and_never_retries_the_abandoned_key`。
    scripted peer が受け取ったリクエスト列を記録する。
  - `Link::resync` の doc comment を訂正。
  - `verification/high-load-input-integrity.md` §5.5 を追加、status を
    Phase 3 に更新、§5.4.3 の「7 tests」誤記（実際は 5）を訂正。
- **検証**: `cargo fmt --all -- --check` / `cargo clippy --workspace
  --all-targets --offline -- -D warnings` / `./ci/run-test-quiet.ps1 -Name
  'workspace tests' -Command { cargo test --workspace --offline }` /
  `git diff --check` すべて成功。cargo・rustc・test child の残存なし
  （`sakura_engine.exe` PID 23540 は前回同様ユーザーの 1.0.37 実環境 engine）。
- **学び（実測）**:
  1. **`Link::resync` の doc comment は誤りだった。** 「捨てるのは engine の
     now-duplicate copy」と書かれていたが、実測は
     `k a (諦めた i) u` → `かいう` / `k a (諦めた i) Revert u` → `う`。
     差分 `かい` のうち `か` は duplicate だが **`い` は違う**。その reply は
     クライアントに一度も届いていないので、ホストが表示も commit もしていない。
  2. **諦めたキーは再送されない。** peer が見る列は
     `SendKey(k)` → `Revert` → `SendKey(u)`、session id は不変。
     engine 側からその効果を取り戻す経路は存在しない。
- **学び（静的読取りのみ。未実測と明記した）**:
  3. 実キー経路は resync に到達しない。`text_service.rs:5486` の
     `Answer::Unavailable` → `recover_from_engine_unavailable`
     (`:5536`) → `disconnect(EngineUnavailableRecovery)` (`:5550`) が
     `self.engine` を `Engine::new()` で置換する（§2.6）。desynchronized な
     link は resync される前に破棄され、次キーは新接続・新 session になる。
     観測した `Revert` 列は administrative timeout（`engine.rs` 286/321/461/
     550/585/698/728）から到達する。
  4. 可視テキストを救う `enqueue_finalization_for_visible(...,
     composition_projection())` (`text_service.rs:5571`) の入力は TSF 自身の
     projection なので、**定義上** reply が届かなかったキーを含み得ない。
- **設計判断の記録**:
  - 反例は「Revert あり／なし」の 1 リクエスト差で作った。負荷や
    タイミングではなく **1 リクエストだけ** が違うので、差分の帰属が一意になる。
  - 同一 engine 内で 2 session を使う案は取りやめた。同一接続の 2 本目の
    session は 1 本目に live composition があると `consumed: true,
    preedit: None` を返す（未調査。#148 とは別件として task に切り出し済み）。
    1 session 2 周に変更した。
  - doc comment の訂正は「修正を先に入れない」に反しない。挙動は変えておらず、
    反証済みの主張を残す方が有害と判断した。
- **残り**: §5.5.3 の実ホスト E2E 実測、§6 の全遅延値行列（49/50/51 ms 境界と
  並行接続）、ETW 因果確定（H-E / H-F）、correlation ID、TSF 側 15 タイムスタンプ、
  `queue_wait_us` / `conversion_us` / `dictionary_us`、Pending/ACK 設計、
  PBT / mutation / TLC。

## 2026-09-09 — 1.0.39 リリース準備（#148 Phase 1–3 の観測手段を出荷）

- **Issue / commit / PR**: #148 / `a93a695` / PR #149
- **内容**: 修正ではなく観測手段だけのリリース。症状（高負荷時の入力ロス）は
  未解決であり、リリースノートの冒頭でそう明記した。
  - `diagnostics timing`（段階別の所要時間、timeout の実待ち時間）
  - 出荷 engine に 4 つの遅延注入点（before-dispatch / during-conversion /
    after-mutation / during-reply）
  - `diagnostics faults`（利用者が自分の engine で全点 disarmed を確認できる）
  - `PROTOCOL_VERSION` 21 → 22
- **バージョン 5 箇所**: `Cargo.toml` + `Cargo.lock` 再生成、`installer/setup.iss`
  の `AppProductVersion` と `AppVersionedDir`、`.github/workflows/release.yml` の
  既定タグ、`data/update-signing/release-sequence.txt` 6 → 7。
- **失敗と根本原因（今回）**: `release-sequence.txt` を Python の
  `Path.write_text` で書いたところ Windows の既定改行変換で `7\r\n` になり、
  `sakura-settings` の `update_trust::*` / `updater::*` 14 件が
  `embedded release sequence contains CR or NUL` で失敗した。このファイルは
  `include_bytes!` で埋め込まれ `format!("{floor}\n")` とバイト比較される。
  `printf '7\n' >` で書き直し、`xxd` で `370a` を確認して解消。
  `.gitattributes` には既に `/data/update-signing/** text eol=lf` があり
  （24 行目）、原因は git ではなく自分の書き込みだった。誤って追加した
  `.gitattributes` の行は HEAD とバイト同一に戻した。
- **学び**: このリポジトリは `core.autocrlf=true` で worktree は CRLF、
  commit 時に LF へ正規化される。**バイト列が意味を持つファイルを書くときは
  `newline=""` かバイト書き込みを使う。** テキストとして書くと環境依存になる。
  もう一点、`bash` ツール経由の heredoc では Python ソース中のバックスラッシュが
  半分に潰れることがある。パス文字列を扱うときは `chr(92)` で組むのが確実。
- **検証**: `cargo fmt --all -- --check` / `cargo clippy --workspace
  --all-targets --offline -- -D warnings` / `./ci/run-test-quiet.ps1 -Name
  'workspace tests' -Command { cargo test --workspace --offline }`
  （1,893 passed / 91 ignored / 94 binaries）/ release build
  `x86_64-pc-windows-msvc` / `scripts/build-installer.ps1`（version 1.0.39、
  warnings 0、`sakura_setup.exe` 24,544,545 bytes sha256
  `c126112893a68c58f55ae6868f03583b69ec856715ee8bce377e4255d03f39b7`）/
  `git diff --check`。辞書 payload は 1.0.38 とバイト同一
  （`system.dic` sha256 `ca1b24fc...febb63f`）で、本ブランチに辞書入力の変更なし。
- **署名**: owner 判断（2026-08-22）どおり Authenticode 未署名。ノートで未署名と
  明記し、`release-manifest-v2.txt` との SHA-256 照合を案内。updater の
  `WinVerifyTrust` fail-closed は変更していない。
- **公開まで（追記）**: PR #149 は CI 3 件 pass（Build and test 6m58s / Build
  installer 6m33s / Dependency policy 1m1s、Fuzz は対象外で skip）。CodeRabbit は
  必須でないため owner 指示により待たずに merge（`7940d04`）。タグ `v1.0.39` を
  push し `release.yml` run 34337385228 が success（build 18m15s + package 50s）。
  成果物 `sakura-input-1.0.39-release-candidate` の
  `sakura_setup.exe` は 24,530,941 bytes / sha256 `b74ab1b5…4c5120` で、
  同梱 `release-manifest-v2.txt`（`release_sequence=7`、`source_commit=7940d04…`、
  `authenticode=unsigned`）の値と一致。`signing-status.txt` は
  `unsigned-owner-approved`。両ファイルの `gh attestation verify` は
  `--signer-workflow release.yml` と `--source-digest 7940d04…` で成功
  （誤 digest による negative control は exit 1 で失敗することも確認）。
- **未完（owner 作業）**: **GitHub Release は未作成。** `scripts/publish-release.ps1`
  は DPAPI 保護された Sakura 更新署名の秘密鍵（`-ProtectedPrivateKey` と
  `-KeyId`、active key `178bc99d…b6c47`）を要求し、これは owner の資格情報なので
  エージェントは扱わない。候補 2 点は `release-candidate/`（untracked）へ配置済みで、
  同スクリプトの既定入力ディレクトリと一致する。

## 2026-09-09 1.0.39 公開完了と、更新確認が恒久的に失敗する欠陥の修正（#148 / #150）

- **訂正**: 直前の項の「GitHub Release は未作成（owner 作業）」は解消済み。DPAPI
  保護鍵は**パスを渡すだけ**でよく、鍵の値をエージェントが見ることはないため、
  `gh` の保存済みトークンと同じ扱いにできる（owner 指摘）。
  `scripts/publish-release.ps1` を実行し
  https://github.com/tsuyoshi-otake/sakura-input/releases/tag/v1.0.39 を公開した
  （`isDraft=false`、`publishedAt=2026-09-09T10:23:25Z`、asset 3 点、
  検証は `1 valid pinned signature`）。実行時に `ConvertFrom-Json: Invalid
  property identifier character` で落ちたのは、`gh api .../commits/v1.0.39` の
  diff に含まれる日本語リリースノートをコンソールの既定エンコーディングが壊した
  ため。`pwsh -NoProfile` で `[Console]::OutputEncoding` と `$OutputEncoding` を
  UTF-8 に設定して再実行し成功。スクリプトは変更していない。

### #150 更新確認が古い trust state で恒久的に失敗する

- **症状**: 新規インストールした 1.0.39 の設定アプリが、更新確認のたびに
  `更新情報の検証に失敗しました: update trust state is below the embedded trust
  floor` を返し、回復手段が無い。
- **実測**: `%LOCALAPPDATA%\SakuraInput\update\trust-state.txt`
  は `trust_epoch=1` / `highest_sequence=4` / `highest_version=1.0.36`
  （mtime 2026-09-06 14:22）。インストール済みバイナリの埋め込み floor は 7。
  `release-sequence.txt` の履歴は 1.0.34→2、1.0.35→3、1.0.36→4、1.0.37→5、
  1.0.38→6、1.0.39→7。手動インストールは state を進めないので 4 のまま固定され、
  4 < 7 が manifest 取得前の `TrustState::parse` で終端エラーになっていた。
  同じ条件は 1.0.37（floor 5）、1.0.38（floor 6）でも成立しており、**1.0.39 の
  リリース作業が原因ではない**。
- **根本原因**: state が「無い」場合は正常系として受理されるのに、「古い」場合だけ
  恒久エラーだった。しかも state はユーザー書込可能な場所にあるため、削除すれば
  受理される。厳しく落としてもセキュリティ上の利得は無く、正規利用者だけが恒久的に
  更新確認を失う。実効的な anti-rollback 境界はバイナリ埋め込みの floor であり、
  これはバイナリを差し替えない限り動かせない。
- **修正**: `TrustState::parse` から floor / epoch 判定を外し、
  `TrustState::bounds_future_manifests(sequence_floor)` を新設。
  `authorize_manifest` は下限として使えない state を `None` と同じ既存経路へ落とし、
  署名検証済み manifest から state を書き直す。形式不正・上限超過は従来どおり終端
  エラーだが、メッセージに state ファイルのフルパスを付けた。
  **レビュー指摘による訂正（PR #151、CodeRabbit と Codex が独立に指摘）**: 最初の
  修正はパスを `TrustState::parse` の失敗にしか付けておらず、
  `MAX_TRUST_STATE_BYTES` 超過は `read_trust_state` が parse の前に返すため
  パスが入らないままだった。`name_trust_state_file` を切り出して reader の
  失敗（open／metadata／oversize／short read）にも適用し、サイズ超過の
  回帰アサーションを追加した。`CLAUDE.md` と `rules.md` の表現も、
  「形式は正しいが使えない state」と「形式が壊れた state」を混同しないよう、
  また埋め込み floor（動かせない下限）と永続 state（使える場合の追加 replay
  境界）を 2 層として書き分けるよう直した。replay／equivocation／
  rollback の拒否と `WinVerifyTrust` fail-closed は変更していない。
  `verification/update-signing-v2.md` の v2 契約は trust state に言及していないため
  契約変更ではない。
- **検証**: `cargo fmt --all -- --check`、
  `cargo clippy -p sakura-settings --all-targets --offline -- -D warnings`、
  `./ci/run-test-quiet.ps1 -Name 'workspace tests' -Command { cargo test --workspace
  --offline }` がすべて成功。回帰テスト
  `update_trust::tests::a_trust_state_below_the_embedded_floor_is_treated_as_absent_and_rebuilt`
  を追加し、実機と同じ値（state 4 / 1.0.36、floor、manifest 1.0.39）で更新確認が
  成功して state が floor へ書き直されること、epoch 不一致 state も同じ経路を通る
  こと、書き直し後に replay が再び拒否されること、壊れた state のエラーがパスを
  含むことを固定した。cargo／rustc の残存プロセスなし。
- **学び**: `.claude/memory/rules.md` と `CLAUDE.md` に「ユーザーが削除できる記録は
  その不在より厳しく失敗させない」「埋め込み値より弱い永続値は『境界なし』であって
  破損ではない」「手動インストールは trust state を進めない」を追加した。
- **既存インストールの回復**: 1.0.39 のバイナリにはこの修正が入っていないため、
  利用者側では state ファイルを削除するのが唯一の回復手段（次回の更新確認で
  署名検証済み manifest から再構築される）。実行は owner の判断に委ねており、
  エージェントからは削除していない。

## 2026-09-12 エージェント指向リファクタリング計画の作成（#152）

- **依頼**: Clean Architecture の形合わせではなく、High Cohesion / Low Coupling /
  One-way Dependency / Change Locality / Small Agent Context / Formal Verification
  Boundary などを最大化し、Issue 1 件あたりにエージェントが読むコード・文書・
  テストを最小化する計画を作る。実装はしない。基準は origin/main `a370477`。
- **成果物**: `docs/architecture/agent-refactor-plan.md`（約 54 KB、日本語）と
  根拠の静的解析 6 本 `docs/architecture/analysis/*.md`（engine、core-proto、tsf、
  contracts-dictc-tools、docs-verification-ci、renderer-settings）。解析は Opus
  subagent 6 体を読み取り専用（cargo 実行なし）で並列に走らせた。Issue #152 に
  進捗コメント 5 本を小刻みに投稿。
- **計画の骨子**: Phase 0 ゲート整備（DLL 1 MiB、crate 別テスト基準値、`rg` 依存
  規則 R1〜R11、TLC workflow 非ブロッキング、CLAUDE.md ≤8 KB）→ 1 テスト分離
  （sibling `*_tests.rs`、renderer `lib.rs`）→ 2 葉 crate（`sakura-values`、
  `sakura-store`、`sakura-rerank-proto`、`sakura-oracles`、`sakura-context`）→
  3 core/proto/ipc/reg/dictc 分割 → 4 engine → 5 renderer/settings → 6 TSF
  （`session/` に `use windows` 禁止＝形式検証境界）→ 7 文書・verification 再編
  （`docs/contracts`、`docs/decisions`、`correspondence.json` の CI 検査）。
  全 82 ステップに `Verify:`/`Expect:`。TSF の write journal／candidate board に
  触る 6.8〜6.10 は **#7 のクラッシュ証拠が添付されるまで着手しない**。
- **検証**: 独立 `rubric-verifier`（Opus、fresh context）で 12 基準を採点。初回は
  C2（`long_conversion.rs` の rerank magic 引用が 2 行ずれ、実際は :28-29）と
  C4（ステップ 7.13 の Verify/Expect が空）で FAIL。両方修正して再採点で PASS。
  他 10 基準（行数 12 件が `wc -l` と一致、12 原則の指標化、R1〜R11 が実行可能、
  owner 判断 D1〜D11、読解セット表 16 行、命名統一、リポジトリ変更が
  `docs/architecture/` 配下のみ）は初回から PASS。
- **学び**:
  - 解析レポートを subagent に書かせても harness がファイル書き込みを遮断し
    `tasks/*.output` が 0 byte になる。レポートは親が Write で永続化する。
  - 自分で要約した数値は投稿前に元レポートと突き合わせる。comment-03 の初稿は
    存在しない `session/` ディレクトリを既存扱いし、読解セット数値もレポートと
    ずれていた（投稿前に発見・修正）。
  - 行番号引用は `sed -n Np` で 1 件ずつ確認する。`sed -n 20,34p` の出力から
    目視で数えた行番号は 2 行ずれた。verifier がこれを拾った。
  - 複数レポートが同じ概念に別名を付ける（`sakura-limits`／`sakura-values`、
    `sakura-stores`／`sakura-store`）。統合時に命名表を 1 節設けて固定する。
- **未了（owner 判断待ち）**: D1〜D11（CLAUDE.md の停止中調査の扱い、DESIGN.md
  分割、`wire.rs` の置き場、`Session` 分割、desktop test runner、TLC ブロッキング化
  など）。コミットは未実施（依頼があれば行う）。

## 2026-09-13 リファクタリング計画のレビュー対応：IRV を中心 KPI に（#152、PR #153、0ca477c）

- **依頼**: owner レビュー 14 項目。IRV（Issue Reading Volume）を正式な fitness
  function にし、Physical／Semantic を区別、固定 ID の benchmark suite と baseline、
  回帰ゲート、新 crate 憲章、`sakura-values` の依存方向修正、`sakura-store` 境界、
  main 保護、文書前倒し、PR 境界の再評価、Phase 別受け入れ基準、fresh context の
  verifier 再採点、Issue の最終報告。「ローカル作成済み」を完了条件にしない。
- **成果物**: 計画 §1.1〜§1.1.2（IRV 定義・benchmark 10 件・baseline・gate）、
  §3.1.1 憲章（5 crate × 6 項目）、R12〜R14、Phase 0 を 5→9 ステップ（ruleset、
  baseline、irv-regression job、文書前倒し）、Phase 2 再設計（2.0 golden fixture、
  2.1a〜2.1c、2.2a〜2.2d）、§4 PR 境界の原則、§4.1 受け入れ基準 20 行、D12〜D14。
  `scripts/measure-irv.ps1`（`-SelfTest`／`-Out`／`-Compare`／`-DocsBudgetMode`）、
  `verification/irv/benchmarks.json`、`baseline.json`（97705a5 で計測）。
  Issue 本文を GitHub 実体基準のチェックリストに更新、PR #153 本文更新、最終報告
  コメント投稿。
- **検証**: `rubric-verifier`（Opus、fresh context）で C1〜C25 を採点し 25/25 PASS。
  初稿 rubric 12 件に加え、IRV 定義、Physical/Semantic、benchmark ID の plan／JSON
  一致、baseline 表 10/10 一致、self-test PASS と compare の exit code、gate の
  閾値と根拠、憲章 6 項目、values/wire 方向、store 境界、ruleset 計画、文書前倒し、
  PR 境界、GitHub 上の PR／Issue／remote HEAD 一致を検査。
- **学び**:
  - Bash heredoc に日本語の長文を入れると `unexpected EOF while looking for
    matching` で失敗した（2 回）。文書は Write tool でファイル化し、挿入は
    `perl splice.pl <plan> <insert> <after-line>` で行う。
  - perl で全角記号（`）`、`、`）を扱うときは `-CSD -Mutf8` が必須。ASCII の
    `)` では一致しない。
  - Issue コメントに計画の数値を「要約」すると捏造が混ざる。今回 §4.1 の Phase 別
    目標を記憶から書いて `900→≤600` という存在しない値を作りかけた。投稿前に
    `awk '/^#### 4\.1/,/^### Phase 0/'` で表を引き直して置換した。
  - `measure-irv.ps1 -Compare` は FAIL で exit 1 を返す。パイプで受けると
    exit code が消えるので、CI では直接呼ぶ。docs 予算超過が既知の間は
    `-DocsBudgetMode Warn` で運用し、FAIL 化の時期は D14 で owner が決める。
  - ステップ ID に `2.1a` のような接尾辞を入れたら、rubric の正規表現を
    `^\| [0-7]\.[0-9]+[a-z]? \|` に合わせて更新する必要がある。
- **未了（owner 判断）**: D1〜D14。PR #153 のレビューと merge。実装は Phase 0 から
  別 Issue。

## 2026-09-13 D12〜D14 の判断委任と計画への反映（#152、PR #153）

- **依頼と範囲**: owner が D12〜D14 の判断を委任。Sol Medium subagent が計画書の
  編集を担当し、親 Codex が実差分とコマンド結果を直接検証した。独立 verifier の
  再採点ではない。実装・ruleset の適用・PR の merge は今回の範囲に含めない。
- **決定**: D12 は Phase 0 実装担当が admin 認証済み `gh` で ruleset を適用する。
  bypass actor は無し、緊急時も PR と必須 check を通す。D13 は研究用 crate を
  `context-research` feature 限定で残し、1 年間の未使用を確認した場合だけ別 PR
  で削除する。D14 は 0.5 と 7.11 が揃う最後の PR で文書予算内と Fail mode の
  成功を確認して CI を切り替え、以後の PR で予算超過を FAIL にする。
- **変更**: 計画 §1.1.2、crate 概要、Phase 0.6／0.8／2.5、§7 を整合させた。
  main への直接 push による拒否試験は、ruleset 詳細・main の実効規則・検証 PR
  の BLOCKED 状態の確認へ置換。権限の読み取り確認は `permissions.admin=true`、
  ruleset は `[]` で、保護の実適用済みとは扱わない。
- **検証チェックリスト（親が実行、すべて期待どおり）**:
  - Verify: `git show f228a36:docs/architecture/agent-refactor-plan.md` と現行の
    D 行・Phase ID を照合。Expect: D1〜D11 は同一、D12〜D14 は日付付きで決定済み、
    ID は 14 件一意、Phase ID 不変、0.6／0.8／2.5 の Verify／Expect は非空。
  - Verify: `pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/measure-irv.ps1
    -Compare verification/irv/baseline.json -DocsBudgetMode Warn` を直接実行。
    Expect: exit 0／PASS、10 benchmark の Physical LOC はすべて増減 0%。確認済み。
  - Verify: 同コマンドで `-DocsBudgetMode Warn` を省略して直接実行。
    Expect: 無条件文書 85,770 B > 24,576 B により exit 1／FAIL。確認済み。
  - Verify: IRV script／benchmarks／baseline の `git diff --exit-code f228a36`、
    `git diff --check`、実行後の repository／measure-irv に対応する process 確認。
    Expect: IRV 資材不変、空白エラー無し、検査自身の PID を除いて残存無し。確認済み。
- **確認した注意点**: Warn mode は既知超過が増えていなければ note／PASS、Fail
  mode は増加の有無に関係なく超過を拒否する。計画にあった「300 行追加 → WARN」
  は 21,664 LOC の 10% に届かないため、JSON の基準値と閾値から最小超過の
  2,167 行を計算して訂正した。過去の 25/25 PASS を今回の再検証とは表現しない。
- **残件**: D1〜D2・D4〜D11（D3 は従来から決定済み扱い）、PR #153 のレビューと
  merge。実装は Phase 0 の別 Issue で扱う。

## 2026-09-13 全 Phase 実装の承認と計画の実行可能性修正（#152、#154）

- owner は Phase 0〜7 の全実装を明示承認。実装用 Issue #154 を Phase 0 専用に作成した。
  計画 PR #153 には実装を混ぜず、計画書と分析 6 本だけを更新する。
- D1、D2、D4、D5、D8〜D11 は委任された既定案で決定。D6 は実 desktop runner の
  環境証拠、D7 は #7 の実 dump／report の取得・検証が残る。過去コメント中の filename を
  添付取得済みとみなさず、Phase 6.8〜6.10 の開始条件を維持した。D12〜D14 は変更無し。
- 計画修正は metadata 由来の package／target 基準、実 DLL 出力先、TLC の正常／期待反例分類、
  shared path の CI fallback、依存と symbol の実解決、sibling test 許可、store の runtime 非所有、
  renderer 逆向き依存、研究 feature 維持、完全な契約 inventory と履歴保持を対象にした。
- Verify: 親が 7 文書の実差分を直接確認し、step／D ID 集合の不変、R1〜R14 の一意性、
  契約 filename 23 件の一意性、D6／D7 未決、D12〜D14 行不変、IRV 資材不変、
  `git diff --check` を実行。Expect: 全条件を満たす。結果 PASS。
- Sol Medium は編集を担当。検証責任は親が持ち、外部／subagent レビューは依頼していない。
  今回の検証を過去の Opus rubric 25/25 の再採点とは表現しない。

## 2026-09-13 Phase 0.1 TSF DLL サイズゲート（#154）

- `ci/check-dll-size.ps1` は Cargo metadata の target directory と build target から
  release DLL を特定する。1,048,576 bytes は許可し、1 byte 超過と欠落はフルパス付きで失敗する。
  CI の既存 workspace release build の直後に自己テストと実 DLL 検査を追加した。
- Verify: 自己テスト、`cargo build -p sakura-tsf --release --locked`、実 DLL 検査、
  CARGO_TARGET_DIR 変更先の fixture と欠落検査、`ci/check-process-clean.ps1`。
  Expect: 境界／超過／欠落の判定が正しく、実 DLL は予算内、残存 runner 無し。結果 PASS。
  実 DLL は 437,760 bytes。製品コード、依存バージョン、wire 契約は変更していない。
- 検証で得た学び: TKW で native Cargo を直接起動すると rustup proxy の toolchain 継承を
  失い、repository 外の build-script 作業ディレクトリで既定 stable を選ぶ場合がある。
  初回 build はその経路の rustup component conflict で失敗した。親が repository の
  active toolchain 1.96.0 を確認し、同じ一時実行へ RUSTUP_TOOLCHAIN を明示した再実行は成功。
  グローバル stable の修復やバージョン変更は行っていない。


## 2026-09-13 Phase 0.2 実測テスト基準（#154）

- Cargo metadata から workspace package と default target／feature 範囲を取得し、
  各 package を quiet wrapper 経由で実行する `ci/record-test-baseline.ps1` を追加した。
  結果は package／suite 単位で記録し、欠落した summary、失敗、件数不整合を拒否する。
- Verify: 実 workspace の記録と、その JSON を `-Compare` に渡した再実行。
  Expect: 16 packages、94 suites、1,894 passed、91 ignored、0 failed が一致する。
  両実行とも PASS。これは package ごとの default features の集合であり、ignored を
  実行済みと数えたり、workspace 一括の feature 統合結果と同一視したりしない。
- Verify: parser／target selection／比較の SelfTest と `ci/check-process-clean.ps1`。
  Expect: 失敗結果・欠落 suite・test 件数減少を拒否し、commit／timestamp の差は許容、
  実行後の runner 残存無し。結果 PASS。
- 初回検証では OrderedDictionary の pipeline 集計と空行を含む実 libtest 出力で失敗した。
  記録型を PSCustomObject に固定し、入力に空文字列を許容して fixture に実際の空行を追加した。
  修正後に全 package の記録・比較を実行済み。製品コードと依存は変更していない。

## 2026-09-13 Phase 0.3 依存規則の advisory 検査（#154）

- R1〜R9 を列挙する検査と CI job を追加。既存違反は advisory として可視化し、
  `-Enforce R1,R2` のように移行完了した規則だけを blocking に切り替えられる。
  R1／R2 は Cargo metadata と Rust source の両方、R8 は全 wire 定数の所有先を確認する。
  未作成の対象は PENDING、R5 は AST による最終検査前の暫定 regex と明示する。
- Verify: SelfTest、実 repository の advisory 検査、`-Enforce R1`、`-Enforce R99`。
  Expect: fixture が成功し、現状の違反は WARN／PENDING、R1 強制と未知規則は非ゼロ終了。
  親が直接実行し、すべて期待どおり。advisory 成功を依存違反ゼロとは表現しない。
- SelfTest 初回失敗は PowerShell 自動変数 `$Matches` との衝突と、遅延 ReadLines の
  ファイル handle が後始末まで残ったため。別名と読み取り完了済み配列へ変更し、
  主例外を cleanup 例外で隠さないようにした後、SelfTest と実検査が成功した。

## 2026-09-13 Phase 0.4 形式検証の分類と手動 workflow（#154）

- tracked cfg 48 本を manifest と照合し、通常 33 構成と期待する invariant 反例 15 構成を
  分類する runner を追加した。TLC 1.7.4 の JAR hash を固定し、終了コード・完了 marker・
  trace・期待 invariant を検証する。非同期 stdout／stderr、時間上限、子 process tree の
  後始末と hash 付き receipt を runner が所有する。
- Verify: 親が全 48 構成を実 TLC で実行、SelfTest 9 分類と子 process の失敗／timeout を実行。
  Expect: 正常探索 33 と意図した反例 15 が PASS、期待しない失敗は成功扱いされない。
  全条件 PASS。実行後、48 receipt の model／cfg hash が現在のファイルと一致することと
  所有 Java process の残存無しを確認した。局所探索を実装全体の証明とは表現しない。
- Windows の反例出力は CRLF のため初回の厳密 invariant 行判定が不一致になった。
  正規表現を CRLF 対応にし、その fixture を追加後、全構成で再検証した。
- workflow は tracked cfg から matrix を生成し、最大 4 job、各 job 20 分、子 17 分で制限する。
  現段階は workflow_dispatch のみで continue-on-error を維持。Phase 7.9 での blocking 化は
  別工程であり、この記録は GitHub workflow の実行済みを意味しない。

## 2026-09-13 Phase 0.5／0.9 文書の入口と所有境界（#154）

- commit `9297343` で元 CLAUDE.md を先に `docs/history/CLAUDE.pre-154.md` へ移動し、
  40,118 bytes／SHA-256 `0f50a454c692373d4060960e6f3624498979921c7bf51b1bae9928640360613c`
  の不変を確認した。新しい入口は CLAUDE 3,978 bytes、AGENTS 739 bytes。
  owner 判断 12 件と VS Code 調査の 13 抽出が原文の連続部分に完全一致することを検証した。
- 現行 code と将来の設計を区別する案内、全 14 依存規則、8 見出しの crate 憲章テンプレート、
  CODEOWNERS、契約 23 件の所有者 inventory を追加した。契約本文は Phase 7.5 の未完了項目。
  architecture README は 5,418 bytes。実在する 45 個の Markdown link を直接検査した。
- Verify: 文書予算、原本 hash、抽出原文一致、stale 行番号と編集ツール名の CLAUDE からの除去、
  R1〜R14／8 見出し／23 契約の一意性、全案内先の存在、IRV SelfTest、diff check。
  Expect: すべて PASS。新設 AGENTS.md の読解量を除外しないよう IRV の無条件文書へ追加した。
  元 baseline の数値は更新していない。rules.md の縮約は Phase 7.11 として別に扱う。

- 追検証: 初回の staged diff check は抽出文書の EOF 空行を指摘したが、shell が後続 commit
  を継続していた。空行だけを除去し、非ゼロ終了で直ちに止まる呼び出しで再検証して PASS。
  最終の LF 作業ファイルは CLAUDE 3,977 bytes／AGENTS 738 bytes。原本 hash と抽出本文一致は不変。

## 2026-09-13 Phase 0.8 IRV の CI 導入（#154）

- PR ごとの CI に IRV SelfTest と baseline 比較を追加した。比較は pipeline で受けず直接呼び、
  元の終了コードを保つ。D14 に従い 0.5／7.11 完了までは文書予算を Warn とする。
  PR テンプレートへ `IRV:` 行と変更理由・検証・crate 憲章の記載欄を追加した。
- Verify: IRV SelfTest と各既 commit の archive に対する比較を親が直接実行。
  Expect: 既知の文書超過は非増加なら note、CI verifier 追加は WARN、他の基準値は維持。
  結果は期待どおり。CI 上のダミー PR による WARN／FAIL はこの後の検証であり未完了。

## 2026-09-13 Phase 7.11 適用条件を保持した rules 縮約（#161）

- Phase 7 の実装 Issue #161 を作成。commit `eedded5` で旧 rules.md を先に
  `docs/history/rules.pre-154.md` へ移動した。45,652 bytes／SHA-256
  `e4016aa130b536fc168c8b12317fbca1ca4277f90afed9d8f3c5f769de74a687` は不変。
  全 section を 7 トピックへ原文のまま抽出し、該当時は必読とする条件表を入口へ置いた。
- Verify: 原本 hash、7 抽出の完全一致、全見出し集合、必読先の存在、元 benchmark coverage 不変、
  IRV SelfTest、Fail mode 比較。Expect: 元規則を失わず合計 ≤24,576 bytes。すべて PASS。
  LF 作業ファイルの無条件合計は 9,565 bytes。CI と PR テンプレートを D14 の Fail mode へ変更。
  この PR は Phase 0.5 の文書縮約 PR より後に merge する。
- 条件付き規則と owner 判断の読解量も各 Issue benchmark の docs へ追加した。source／test／
  contract の既存集合と元 baseline は不変。全 Issue 共通を表す IRV-DOCS は従来の
  DESIGN／README に新 architecture entry を加え、Issue 固有トピックは該当 benchmark に数える。
  初回は CI／Windows 固有トピックまで全 Issue 共通へ加えて +25.6% FAIL となったため、
  issue_shape と必読条件を照合して分類を訂正した。閾値や基準を緩めてはいない。

## 2026-09-13 Hosted dependency scanner portability (#154)

- Hosted run 34704979746 failed on absent rg.exe .Source under StrictMode. Native command discovery is now null-safe and absent rg uses the built-in scanner with the same case-sensitive rules. Single-file rg results now include filenames.
- Parent Verify: both scanner SelfTests, complete advisory finding equivalence after path/whitespace normalization, and forced-no-rg R1 enforcement. Expect: matching findings and nonzero enforcement of the existing violation. Passed. This identical correction is applied directly to PR #157, superseding the late-stack delivery in #166 so its prerequisite job can pass before merging.

## 2026-09-13 Real PR IRV controls (#154)

- Verify: PR #163 irv-regression job 103586235440 and PR #164 job 103586240195 logs. Expect: WARN succeeds, critical benchmark threshold breach fails. Both matched: WARN KEYMODE 24,418 versus 21,664 (+12.7%); FAIL TSF reentrancy 17,259 versus 13,543 (+27.4%) and dual candidate 17,024 versus 13,015 (+30.8%).
- These are IRV job results, not whole-PR successes. Both synthetic PRs were closed without merge, remote probe branches deleted, and all four owned CI/installer runs confirmed completed/cancelled after the relevant job evidence was captured.
- Original baseline remains unchanged. The dependency scanner prerequisite was propagated by merge, retaining both journal histories.

## 2026-09-13 Versioned main ruleset and named checks (#154)

- Added the Phase 0.6 main-only ruleset definition with no bypass actors, required PRs, deletion/force-push prohibition and six strict required checks. Three bounded alias jobs expose existing aggregate fmt/workspace/DLL work and fail unless the aggregate job succeeded; they do not duplicate builds or claim future crate-tests coverage.
- Verify: parent parsed the JSON, exact required contexts, ref/enforcement/bypass boundaries and all three bounded fail-closed aliases; diff check. Expect: complete definition without applying it prematurely. Result: PASS.
- GitHub application and effective-rule verification remain pending until the named checks actually succeed. A committed definition is not evidence that main is protected.

## 2026-09-13 Native image namespace support (#104)

- Hosted runs 34704517571 and 34705378113 again rejected an owned engine; later diagnostics reported native-device image paths. Original admission data remains unobserved, so historical causality is not claimed.
- Added bounded current-drive mapping before the existing exact/canonical/reparse/layout checks, with no process requery, retries or integrity exception. Five new tests include real Windows mapping and negative policy controls.
- Parent verification on the implementation stack: IPC security tests, locked IPC clippy with warnings denied, wrapped locked workspace tests, process cleanup and diff checks passed. The identical source patch is carried here onto main so this prerequisite can land independently of the architecture gates. Hosted sandbox evidence remains pending.

## 2026-09-13 Hosted namespace rejection persists (#104)

- PR #167 job 103586927461 still rejects the owned engine before Hello after the mapping proposal. The local private-pipe AppContainer test passes, so local success does not establish hosted compatibility.
- Added test-only, content-free current-drive diagnostics: API status, returned length/termination, mapping namespace shape, exact device-prefix boundary and mapped lexical equality. No diagnostic authorizes a connection or changes production policy.
- Verify: wrapped diagnostic unit test and private-pipe real AppContainer test; repository process cleanup and diff checks. Expect: pass and no surviving owned processes. Result: PASS. An initial compile failure used the wrong windows-result API name; corrected to the pinned version's Error::from_thread before rerunning.
- Hosted diagnostics remain required before selecting another fix. #104 and architecture Phase 0 delivery remain incomplete.

## 2026-09-13 Deterministic sandbox namespace counterexample (#104)

- Hosted job 103588260690 passed, including the original ignored AppContainer test. That intermittent success did not exercise native normalization reliably.
- Added a test-only PROCESS_NAME_NATIVE query after verified connection and owned-PID equality, before Hello, requiring the existing policy to accept the resulting native path. Local private sandbox now fails deterministically: drive_mapping_query=error(code=HRESULT(0x80070005)), policy_recheck=false. QueryDosDeviceW cannot supply the proposed mapping inside this AppContainer token. Diagnostic unit test passed; owned processes exited after the failing integration test.
- This is direct evidence against the mapping proposal, not proof of every historical #104 failure's cause. Replace the proposal with an explicit trusted native identity contract for the sandbox test; do not weaken production InstalledRoot checks.

## 2026-09-13 Exact native identity for owned sandbox tests (#104)

- Removed the rejected production mapping proposal. Added ExactNative for trusted callers owning the expected engine; the sandbox parent captures its native image path before launch and transports exact UTF-16 separately. Admission queries PROCESS_NAME_NATIVE once and checks strict shape plus complete equality. Existing Exact and InstalledRoot paths remain unchanged from main. Exact pipe PID verification still precedes Hello.
- Verify: wrapped IPC security tests, diagnostic unit test, real private-pipe AppContainer integration, locked IPC/engine all-target clippy with warnings denied, wrapped locked workspace tests, scoped process cleanup and diff check. Expect: all pass with no owned runners surviving. Result: PASS. TKW request c724738b2e6548c1910ae77b6127caf1 captures clippy; parent directly ran all checks. Hosted sandbox verification remains pending.
- Reusable finding: an unrestricted parent API succeeding does not establish availability under AppContainer; validate the actual namespace and API under the sandbox token before choosing an admission mechanism. The old mapping counterexample is retained above.

## 2026-09-13 Refresh observed baseline after prerequisite tests (#154)

- Re-ran record-test-baseline.ps1 against 86371f4 before Phase 1 test extraction. All 16 packages and 94 suites completed: 1,988 discovered, 1,897 passed, 91 ignored, 0 failed. Only sakura-ipc changed from the prior inventory (43 to 46), exactly the three new #104 native identity tests; every other package total is unchanged. Scoped process cleanup passed.
- The baseline is regenerated from actual per-package default-feature runs, not an edited historical count. It remains distinct from workspace feature-unified execution. Parent owns verification.

## 2026-09-13 Extract dispatch tests without changing production (#168)

- Phase 1.1 moves the trailing inline tests to dispatch_tests.rs after a git mv operation, retaining dispatch::tests and private-parent access. Production is 6,457 lines with the canonical cfg/path/module declaration; the plan's 6,455-line estimate omitted two declaration lines.
- Verify: parent compared the complete verbatim body before formatting and the unchanged production prefix, 1,768 string literals and 168 test declarations after formatting. Wrapped dispatch-filter tests passed (220 total, including exactly 168 dispatch::tests); wrapped engine library tests passed with the baseline's 573 passed / 2 ignored. Diff whitespace and scoped process cleanup passed. A first literal scanner incorrectly included quoted prose in doc comments; excluding comments removed that false positive without editing Rust.
- The broad dispatch filter also selects 52 tests outside the moved module. Do not confuse its count with the moved module's 168 tests. Runtime test identities remain unchanged.

## 2026-09-13 Deliver Phase 0 gates and activate protection (#154)

- PRs #155, #156, #157, #158, #159, #160, #162 and #169 are merged after their current-head checks succeeded. Prerequisite #167 passed hosted CI 34707773916, including the actual production-pipe AppContainer test, before merge. This establishes the new native-identity contract, not every historical #104 failure's cause.
- Parent verified main ruleset 23073135 is active, main-only, with no bypass actors: required PRs, deletion/force-push prohibition and strict fmt, dependency-rules, workspace-tests, irv-regression, dll-size and Dependency policy contexts. The effective-rule API agrees with the committed definition. An incomplete-check PR BLOCKED observation remains pending.
- A parent-run desktop capability probe obtained read and limited create/write-object access to WinSta0/Default in interactive session 1 and closed every owned handle. No UI operation occurred. This is access evidence only; D6 still needs the specified desktop tests' runtime and stability evidence.

## 2026-09-13 Complete hosted Phase 0 acceptance (#154)

- Verify: downloaded all receipts from hosted formal run 34708409971 and checked manifest identities, model/configuration/JAR hashes, stdout/stderr hashes, specified negative-control classifications, timeout and child-exit fields. Expect: 33 completed safety configurations and 15 specified counterexamples. Result: 48/48 PASS, all children exited without timeout; the workflow's 49 jobs all succeeded.
- Verify: PR #170 at 4dd4bd4 with required checks IN_PROGRESS. Expect: effective main ruleset blocks merge even when the diff is mergeable. Result: mergeStateStatus BLOCKED, mergeable MERGEABLE. No bypass was used.
- All Phase 0 checklist items are complete and #154 is closed with the evidence table. Phase 1 proceeds under #168; later phases and the D6/D7 gates are not declared complete.

## 2026-09-13 Extract five engine test modules (#168)

- Phase 1.2 moves input_history, server, session, prediction and learning inline test bodies into their respective sibling files after git mv. Production prefixes and module identities are retained.
- Verify: parent ran the pre-format verbatim checker, then formatted and compared all five production prefixes and their 251/192/178/225/561 string literals. Wrapped engine library execution matched all 575 prior runtime identities and outcomes: 573 passed / 2 ignored. Diff whitespace and scoped process cleanup passed.
- Production line counts are now 1,983 / 1,982 / 1,878 / 1,113 / 2,439 respectively. This is structural separation only; persistence, prediction, learning and server behavior remain unchanged.

## 2026-09-13 Verify formatting reaches a stable result (#168)

- Hosted run 34709081162 rejected one leading blank line in the extracted dispatch_tests.rs. The initial rustfmt pass had left this blank line while unindenting the verbatim inline body; a later pass removed it. Rust tests had passed, but formatting had not been checked for stability.
- Removed that single blank line. Parent Verify: cargo fmt --all -- --check and git diff --check. Expect: no further change required. Result: PASS. For subsequent extractions, always run the explicit check after formatting; do not equate a successful formatting command with check success.

## 2026-09-13 Extract core test modules and test-only reverse references (#168)

- Phase 1.3 moves conversion, dictionary, width, romaji, keymap, preferences and simd tests to sibling files after git mv. Parent checked the actual files against the byte-verified draft before formatting, allowing only line-ending normalization and the removed opening-brace newline; after formatting all production files and string literals match. Source-lock drift for dictionary/romaji was traced to CRLF normalization with identical HEAD/working Git blobs before locks were refreshed.
- Verify: wrapped core library before/after execution, complete runtime identity/outcome comparison, explicit stable fmt check, SIMD assembly gate and scoped cleanup. Expect: baseline 296 passed / 3 ignored, all 299 identities unchanged, valid scalar/AVX/AVX2/AVX-512 code generation, no owned survivors. Result: PASS. Assembly capture request f38b91a843d94ca7ae4917b2734c6f0d used the existing gate and fresh artifact directory.
- Phase 1.4 is satisfied by this same extraction: the two allows_system_entry calls and two width references cited by the plan now exist only in preferences_tests.rs/simd_tests.rs. Parent directly searched both production and test files; forward input_repair-to-preferences and width-to-simd dependencies remain. No additional relocation or public API change is necessary.

## 2026-09-13 Separate TSF tests and fake-engine support (#168)

- Phase 1.5 uses git mv before extracting the 58 text_service tests into a sibling module and four observed fake-engine helpers into cfg(test)-only testing/fake_engine.rs. engine_recovery.rs stays unchanged; its test source moves to tests/unit with the same root child-module identity and private access.
- Verify: parent checked production prefix, retained test/helper code, string literals, recovery source bytes and all 200 runtime identities/outcomes against the before log. Wrapped TSF library execution passed 200 tests with no ignored tests; explicit fmt check, diff check and scoped process cleanup passed. The test-only output(None, None) call is replaced by an equivalent empty_output helper whose fields were compared directly.
- The application script stopped after all patches because lib.rs raw bytes differed from the LF draft through line-ending handling. Parent inspected the three module-wiring additions, completed formatting, and verified the intended source content and test outcomes; no rollback or production fix was needed.
- PR #171 passed all refreshed checks at 33b2086a and merged normally. Issue #173 records a separate measurement defect: first-cfg(test)-to-EOF accounting incorrectly counts later production as tests. Total physical LOC regression checks remain valid; historical excluding-tests estimates are not accepted as proof of Phase 1 completion.

## 2026-09-13 Expose the renderer library entrypoint (#168)

- Phase 1.6 moves renderer main.rs to lib.rs with git mv and exposes run(); a minimal binary preserves the Windows subsystem attribute and calls run exactly once. The actual 15 module declarations retain their order and directory. CLI handling, COM/DPI setup, watcher, process exits and window lifecycle bodies are unchanged.
- Verify: parent reconstructed the original entrypoint from the draft, compared applied source (one removed separator blank line), and ran wrapped library and binary tests. Expect: the former binary test identities run once under the library. Result: all 146 identities/outcomes match (145 passed, one ignored); binary harness has zero tests. Formatting, diff check and scoped process cleanup pass.
- Parent added lib.rs to the renderer IRV production read set alongside the wrapper. Relocating implementation cannot count as a reading-volume reduction by leaving the new owner out of the manifest; the original baseline remains unchanged.

## 2026-09-13 Separate renderer and settings test modules (#168)

- Phase 1.7 extracts candidate, pad, settings ui, updater and update_trust trailing test modules after git mv (102 test declarations). The separate middle pad::placeholder_tests module also moves to a sibling (two declarations), retaining its original namespace. pad's include_str!("pad.rs") still reads the same production source.
- Verify: parent compared production prefixes, every retained test token and literal payload, with only rustfmt's trailing parameter comma removal in one helper signature. Wrapped renderer library tests retain all 146 outcomes (145 passed, one ignored); settings library and payload retain all 92 outcomes (55 and 37 passed). settings ui belongs to the payload binary, so --lib alone would omit its tests. Stable formatting, whitespace and scoped process cleanup pass.
- The incremental placeholder patch's post-check expected no final newline while apply_patch added one. Parent confirmed the EOF-only difference and resumed verification without reapplying the patch or changing behavior. PR #172 passed refreshed checks at e5200a6d and merged normally.

## 2026-09-13 Move engine cross-module tests and verify ignore reasons (#168)

- Phase 1.8 moves developer_history_order, shift_latin_order and space_key_dispatch test sources to tests/unit using git mv. lib.rs keeps their original cfg(test) child-module identities through explicit paths. Ten sibling test files and three oracles remain under src as planned until Phase 2.4. CARGO_MANIFEST_DIR fixture locations do not change.
- The prepared move script had a PowerShell interpolation parse error before execution. Parent applied the module-path patch, then completed the three explicit git mv operations after creating the destination directory. This reverses the preferred declaration/move ordering for this small step; moved source bytes were unchanged and no reset or rollback was used.
- Verify: exact moved-file SHA-256 and cfg/path/module declarations, wrapped engine library, all runtime identities/outcomes, fmt check, diff check and scoped process cleanup. Result: all 575 identities match the prior run (573 passed, two ignored), and all checks pass.
- Phase 1.9 Verify: parent searched all Rust files and ran the ignore inventory checker. Result: 94 ignored attributes already have explicit reasons, zero bare #[ignore]. No Rust edit is needed for that step. Full R9 enforcement remains ordered after Phase 2.4 because current R9 also flags the intentionally retained oracles.

## 2026-09-13 Pin history payload bytes before store extraction (#178)

- Added three tests to the existing input_history sibling test module. Four literal plaintext payloads cover Key, Commit, AiText and Engine tags, all three durable scope tags, multibyte UTF-8, and little-endian numeric fields. These are synthetic payload fixtures, not captured input.bin files or encrypted user records. Parent compared their field order with the unchanged pre-extraction encoder and decoder.
- Verify: wrapped input_history library filter passed 56 tests (53 existing plus three new), zero failed/ignored; literal encode/decode equality, malformed scope/bool/truncation/trailing bytes, and 16,384/16,385-byte encode boundaries passed. Explicit fmt check, diff check and repository process cleanup passed. Production source is unchanged.
- Preparation exposed that durable ScopeClass has tags 0/1/2 while InputScope has six different tags. Store extraction must preserve a separate durable representation and keep classification/exclusion in engine; replacing the stored enum with InputScope would change the format. The tracked tree has no existing input.bin fixture, so these pre-extraction literals provide payload-level provenance without claiming encrypted-file compatibility yet.
- PR #177 passed every required latest-head check and installer at c41a6640 and merged normally. Issue #168 now tracks steps 1.1 through 1.9 as complete, with desktop evidence and oracle-dependent R9 acceptance explicitly open. PRs #179 and #180 were retargeted and refreshed against main.

## 2026-09-13 Correct inline-test accounting and Phase 1 estimates (#173)

- Replaced first-cfg-to-EOF counting with a bounded lexical/item scanner. Added nine fixed fixture cases for mid-file items, fields, attributes, literals, declarations and malformed input. Exact cfg(test) is the metric boundary; arbitrary cfg predicates and macro grammar are not claimed as supported.
- Verify: direct SelfTest and parent controls passed. The regression fixture returns old=25, corrected=15. Same-line union, nested cfg, sibling declarations and missing final newline also pass. Same archived after source measured by old/new scanners has identical physical LOC/bytes/file counts for all ten benchmarks; corrected KEY-MODE inline LOC is 30 rather than 4,568.
- Recorded corrected committed-tree measurements at 758c93f and ed712b4 with scanner/manifest/source provenance, preserving the original baseline hash d8a4fe2f3f1452fb654daa27851c64c2e16bc7b2e268452cda1bc4a71345b889. The only read-set membership change between those trees retains renderer lib.rs beside its entry wrapper. Plan Phase 1 limits now derive from measured after values rounded to 100 LOC; semantic and later-phase targets are unchanged. Direct strict-docs regression comparison exits zero with only the pre-existing CI WARN.
- Initial snapshot invocation exposed that the existing -Out implementation joins its argument to RepositoryRoot even for an absolute path. That invocation failed before writing; reran with a relative output path, then used isolated committed archives. This path-interface issue is separate from the inline metric correction and was not silently patched into it.
- PR #175 passed all required latest-head checks and installer at 52f26f3e and merged normally. PR #176 was refreshed on main. Phase 2 is tracked by #178; its golden fixture is PR #179. Desktop stability and R9/oracle ordering remain explicit outstanding Phase 1 work.
- PR #176 subsequently passed every required check and installer at 12eaf474 and merged normally. PR #177 was retargeted to main for refreshed checks.

## 2026-09-13 Pin protocol v22 frames before value extraction (#178)

- Phase 2 tracking Issue #178 records all 15 executable table rows. Phase 1 desktop evidence and the oracle/R9 transition remain explicit outstanding conditions. PR #174 passed all latest-head required checks and installer build at 46fede0d and merged normally; PR #175 was updated to main for fresh checks.
- Add six golden test functions with literal full frames for Hello request/response, representative Output, three KeyInput values, every Mode and InputScope, and two appearance/pad combinations. Expected bytes are independent constants, decoded as well as compared with current encoders. Parent inspected framing, LE widths, tags, discriminants, UTF-8 payload and field ordering before value/codec extraction.
- Verify: wrapped full sakura-proto package tests, explicit cargo fmt check, git diff --check and repository-scoped process cleanup. Result: 102 passed, one ignored, zero failed; the existing 96 passed/one ignored remain unchanged and six golden tests are added. No production files, protocol version or dependencies changed. The golden PR must merge before the values extraction.

- PR #179 subsequently passed latest-main CI and installer at 3955ac3f and merged. Refreshing #180 required resolving only the append-only journal conflict; both complete entries were retained. No source conflict occurred.

## 2026-09-13 Extract shared values without moving wire ownership (#178)

- Phase 2.1a introduces the dependency-free sakura-values leaf. Moved fixed.rs with git mv, preserving its contents and eleven tests. Nine value codecs remain in proto as direct Wire implementations; proto retains compatibility re-exports, compound codecs, framing, and protocol version 22. Added the eight-section crate charter and R12 source/dependency audit. Cargo.lock adds only the local workspace package and proto edge; no external package changed.
- Verify: offline full-workspace build, wrapped values/proto suites, dependency-rule SelfTest and enforced R12, dependency tree, fmt check, IRV comparison and process cleanup all passed. Combined package results remain 102 passed/one ignored: eleven container tests moved from proto to values. All six literal golden tests, roundtrip, robustness and zero-allocation suites pass. Parent directly compared fixed source, compound codecs/tests, nine moved codec bodies (only trait receiver adaptation), KeyCode lenient map, and moved fields/discriminants with the pre-extraction source; unchanged.
- Corrected the plan's Phase 2 heading to its actual fifteen rows and the 2.1a Verify criterion: compound codecs legitimately remain in types.rs; only the nine extracted value codecs move. IRV now includes the values input definition in KEY-MODE (10,270 LOC/12 files); comparison against the original baseline passes with the existing CI-only warning. This does not claim the final Phase 2 budgets have passed.
- Draft application guards detected the newer #173 rationale, which was retained. The isolated syntax check initially followed fixed.rs before its planned move; bounded syntax checking now skips child resolution and the full build verifies it after application. Native apply_patch rejected unified-diff numeric hunk headers before changing types.rs; parent removed only those headers and resumed from that unapplied chunk. No rollback or reapplication of completed chunks occurred.
- PR #180 passed fresh CI/installer at 443e88e and merged. PR #181 was refreshed onto that protected main, preserving both journal histories; fresh checks are pending.

## 2026-09-13 Measure the local desktop prerequisite without admitting D6 (#168)

- Parent ran the exact ignored settings_topic_user32 tab_focus_order_skips_hidden_topics_and_ends_at_actions test three sequential times on the authorized current interactive desktop. The harness isolated user/app-data paths, checked for existing settings ownership, retained raw test output, bounded the owned process job, and restored cursor/foreground. Each round proved exactly one passing test; no test process survived. Source SHA-256: 42a6a740f21dbf285127a9a04882be9b1526ae88c196917040955198bd434907.
- Evidence: C:/Users/developer/tmp/sakura-desktop-real-preflight/evidence/20260912T185021Z-7f781f0dcf3e49d69f50c33661667161/summary.json and round raw logs. Session 1, UserInteractive true, three zero exits, no timeouts, cursor/foreground restoration true. This verifies one test on this local desktop only; it does not admit a registered or hosted runner, execute the full ignored inventory, close D6, or complete Phase 1.10.

## 2026-09-13 Remove the core-to-protocol dependency (#178)

- Phase 2.1b changes twenty-one Rust references across eighteen files from proto to values and replaces the direct manifest/lockfile dependency. Parent mechanically compared every changed core file against only that name substitution; no algorithm, constant, test or behavior changed. The plan's older eighteen-reference inventory predates the Phase 1 test moves.
- Verify: wrapped full core package tests passed 302 tests with six existing ignores; all 299 library identities/outcomes match the Phase 1 source exactly (296 pass/three ignores). The integration zero-allocation tests pass. Enforced R1/R12 and cargo tree show core -> values only. The mandatory SIMD assembly gate passed scalar, AVX/SSSE3, AVX2 and shared AVX-512 symbol checks at target/simd-assembly-gate/run-9b7414841de64937ba85949a6b42228f/. fmt, diff, IRV baseline comparison and process cleanup passed; the existing IRV-CI warning remains.

- PR #180 passed all required checks and installer at 443e88e and merged. Refreshed #181 onto that main; the only conflict was this append-only journal, resolved by retaining both histories. No Rust source conflict occurred.

## 2026-09-13 Add an opt-in hosted desktop prerequisite diagnostic (#168)

- Added a default-off input to existing CI workflow_dispatch and a conditional windows-latest diagnostic. It records source identity and User32/session capabilities, discovers the settings target through Cargo metadata, retains ignored inventory, and runs one exact tab-focus test three sequential times through the quiet wrapper. Raw Cargo logs independently prove one passing requested identity per round. Isolated user/app-data paths preserve original Rust tool homes; a 30-minute job limit, repository diagnostic concurrency and always-run cleanup bound ownership.
- Parent verified YAML parsing, all five embedded PowerShell steps, exact default-off condition, timeout/runner selection and structural equality of every pre-existing CI job. The original temp execution patch expected an absent blank line; parent removed that context-only blank and applied the previously unapplied chunk. IRV comparison exits zero with the existing CI warning (4,277 physical LOC versus the original 1,749). No desktop runner admission is claimed before the hosted artifact is inspected.
- Clarified that this dispatch-only prerequisite measurement may precede D6. The weekly desktop workflow, full ignored suite and Phase 5 remain gated on evidence and admission. Local single-test success alone is insufficient.
- PR #181 passed fresh required CI and installer at 68dd044 and merged normally; literal history payload fixtures are now available before store extraction.
- GitHub rejected the first dispatch before any job started: runner.temp is unavailable in job-level env expressions. Parent moved those three derived paths into the preparation step using RUNNER_TEMP and GITHUB_ENV. YAML/PowerShell syntax checks alone do not validate GitHub expression-context availability; the corrected dispatch is the required executable check.
- Hosted diagnostic 34713351422 reached the interactive default desktop (session 2, WinSta0, 1024x768). Parent inspected all three raw logs: the exact tab-focus identity passed once per round, zero failures/ignores. Process cleanup passed. The job failed only when recursive isolated-profile deletion reached OS-created Content.IE5 with access denied; it is not a test failure or runner-admission success.
- Cleanup now removes only task-owned SakuraInput data and settings fixture directories after absolute containment and reparse-root guards. Immediate residual OS profile entries are recorded without traversal and assigned to ephemeral hosted VM teardown. Parent parsed YAML and every embedded PowerShell block and confirmed all existing jobs are unchanged. A fresh hosted dispatch must verify cleanup before admission.
- PR #181 subsequently merged at 68dd044. Refreshed values extraction onto main with both journal histories retained and no Rust conflict; fresh CI covers the combined prerequisites.
- Refreshed the stacked core dependency PR from the values prerequisite after its main update; both journal entries retained, no Rust conflict.

## 2026-09-13 Verify the hosted desktop prerequisite (#168)

- Hosted run 34713825191 job 103607261803 at 98ef18c passed capability, all three exact tab-focus rounds, process checks and owned-data cleanup. Parent inspected raw logs: exactly one pass per round, zero failures or ignored tests; session 2, interactive WinSta0/Default, 1024x768. Artifact 10304825327 has archive SHA-256 18cbbd1af15c2c14b901022d497370d441ecbc16ec64c56146b2f39fef5a7e91. Residual metadata contains only OS-owned Microsoft/AppData and the empty fixture-parent directory; ephemeral VM teardown owns those roots.
- This supplies the runner prerequisite for preparing the weekly desktop suite. It proves one exact test on the hosted environment, not the full ignored inventory. The separate scheduled workflow must enumerate and execute all admitted tests and preserve explicit manual-only exclusions before Phase 1.10 is complete. Refreshed the diagnostic PR onto merged values/core main, retaining both journal histories; no production-source conflict occurred.

## 2026-09-13 Name the shared candidate fingerprint representation (#178)

- Phase 2.1c adds the dependency-free Fingerprint = u64 alias to values and uses it for candidate/model score tokens and candidate-set results in engine. Hash algorithms, inputs, cost/text/evidence tags and byte representation remain unchanged. Owner/session/generation/reading hashes retain their existing types. The type documentation explicitly excludes cryptographic trust use.
- Verify: parent inspected the complete production diff and ran wrapped engine library tests: 576 passed, two existing ignores. All 575 earlier identities/outcomes remain unchanged; the only three additions are the already merged history payload goldens. Enforced R1/R12, formatting, diff check, IRV baseline comparison and scoped process cleanup pass. The existing CI reading-volume warning remains; this step does not claim final Phase 2 budget completion.

## 2026-09-13 Schedule the admitted desktop test inventory (#168)

- Added weekly and manual all/settings/renderer runs on the admitted hosted Windows desktop. The manifest fixes twenty-one settings and eleven renderer ignored identities, excluding only the nine-hundred-second manual pad review harness. Each automatic test runs serially through the quiet wrapper with exact identity/one-pass proof, incremental results, raw logs, screenshots/reports, process checks and bounded job lifetime. Existing diagnostic and scheduled workflows share a non-cancelling concurrency group. D6 evidence and eight verified artifact-member digests are recorded in the admission document.
- Parent executed current Cargo ignored enumeration through the wrapper and confirmed all thirty-two exact identities; renderer indicator belongs to the library, not the now-empty binary. This is enumeration only, not a full UI-suite pass. Parent parsed both changed YAML files and all six embedded PowerShell blocks, compared existing CI jobs excluding the intended concurrency change, checked diffs and ran IRV. The new workflow/inventory are explicitly included: CI 4,828 physical LOC/22 files, existing warning retained. The baseline and later budgets are unchanged.
- Integration corrected the draft's binary target, interpolated-variable colon, and CI concurrency indentation. Tests that print diagnostics would interleave with the identity line under --nocapture; --show-output keeps the identity proof intact and retains captured output in the artifact. The initial application command used an obsolete parameter name and failed before mutation; the corrected driver applied ten bounded patches. Full hosted suite execution remains required before completing Phase 1.10.
## 2026-09-13 Extract the history record format and codec (#178)

- Phase 2.2a adds sakura-store/input_history format and codec with a values-only dependency, eight-section charter, and R13 metadata/source checks. Durable HistoryScope preserves tags 0/1/2; engine keeps InputScope classification, privacy exclusion, writer queue, ownership and DPAPI. Snapshot retention and TSV methods remain in an engine extension trait during this intermediate step. Store exposes nine types and fifteen public methods; exclusion counters are read through the existing snapshot API. Cargo.lock adds only the local package and dependency edge.
- Verify: offline workspace check --all-targets, wrapped engine/settings/store library suites and settings payload pass: 576 engine pass/two existing ignores, 55 settings library, 37 payload, and two store tests. Parent directly compared 24 function bodies, the complete Reader, all existing test literals including the four plaintext goldens, and all 578 engine identities/outcomes. Codec, scope classification, retention/export, DPAPI and relevant file-operation bodies remain equivalent. Dependency SelfTest and enforced R1/R12/R13, formatting, diff check, original-baseline IRV comparison and process cleanup pass.
- Integration caught draft defects before delivery: a subprocess pipeline hid LASTEXITCODE, numeric hunk headers were unsupported, source-order-free replacement hit the wrong scope expression, trait defaults referenced concrete fields, and codec helpers/consumer imports were missing. Parent retained completed edits, corrected only the failed or invalid seams, and reran checks. Future extracted-method drafts must include transitive private helpers and inspect all consumers; a patch applying successfully is not evidence of structural correctness.
- IRV includes both new definition files: history 6,771 physical LOC/23 files and CI 4,052 LOC/20 files, with the existing CI warning. The original baseline and final Phase 2 budget remain unchanged; later consolidation is still required.

## 2026-09-13 Specify the persistence API before implementation (#178)

- The current store has nine public types and fifteen public functions. The planned persistence extraction needs six named entry points (cutoff, retained-record selection, encrypted-size predicate, canonical path, transaction path, atomic replacement); the following crypto step needs Sealer::seal/open. A separate charter change sets the function bound to twenty-three, retains the twelve-type bound, and limits Windows filesystem bindings to Windows targets. This resolves the original charter's crypto-only Windows allowance conflicting with its required atomic replacement ownership. It does not change implementation, manifest, lockfile, or IRV acceptance thresholds.
- Parent inspected the current declarations, proposed six-function seam, and unchanged ReplaceFileW primitive before accepting the bounded allowance. Pure retention remains portable; later crypto has its own feature. Learning extraction must inventory its own API before extending this charter. Documentation consistency, diff check, and original-baseline IRV comparison are the verification for this documentation-only prerequisite.

## 2026-09-13 Correct research crate dependency charter from actual consumers (#178)

- Before Phase 2.5, parent searched all four dormant research modules: they consume neural-proto CandidateAuthority, its 32-byte Fingerprint and prediction limits; none consumes sakura-core. FixedStr/InputScope already have equivalent definitions in values. The charter now allows values plus neural-proto, retaining existing research semantics and the six-public-item ceiling. This is a documentation-only prerequisite, with no implementation or dependency-manifest changes.
- Neural-proto's 32-byte Fingerprint is distinct from the reranker values alias; similarly named types are not interchangeable. Preserve the actual caller type and verify definition identity before changing imports. Parent checked source imports and definitions, scoped diff and original-baseline IRV; implementation/feature tests belong to the subsequent extraction.
## 2026-09-13 Preserve desktop Cargo selectors as string arrays (#168)

- Hosted desktop run 34716341126 passed User32 admission and the settings inventory, then failed renderer-lib inventory before desktop test execution: Cargo received a standalone dash. Parent reproduced the native argv exactly: an untyped single-result PowerShell switch became a scalar string, and splatting split --lib/--release into characters. Both inventory and execution now declare selector/release as string arrays, including empty and singleton cases.
- Parent executed twelve native-argv combinations from the actual YAML assignments (both call sites, lib/bin/test, debug/release), plus the wrapped renderer-library ignored inventory. All passed; the old untyped reproduction demonstrably produced character arguments. YAML and six embedded script parses, unchanged existing CI jobs, 32 inventory identities, diff/IRV and process cleanup passed. Full hosted desktop tests still require a successful rerun; prior failure is not desktop coverage.

- Rerun 34716628856 passed all inventory collection, then dictionary source acquisition failed because gh required GH_TOKEN on the fresh hosted runner. The downloaded dictionary-build.raw.log explicitly identifies the missing variable. The build step now receives only the workflow's existing contents-read token; no stored credential or permission expansion is needed. Other steps do not receive this variable. Static workflow verification passed again; a fresh hosted run owns final acceptance.

## 2026-09-13 Extract history retention and replacement policy (#178)

- Phase 2.2b moves inclusive thirty-day retention, sequence ordering, the actual-size overflow predicate, canonical/transaction paths and the exact Windows replacement primitive into store after charter PR187 merged. Compaction consumes its record vector without cloning content. Engine retains protected-frame preparation, writer/queue/privacy, locking, transaction admission, publication checkpoints and recovery. Cargo.lock adds only the existing Windows binding dependency to store.
- Parent directly inspected the full extraction diff and compared test identities: engine 576 pass/two existing ignores, settings 55 pass and store six pass; the only four added identities are persistence boundary tests. Default-feature-off store tests also pass six. Offline workspace all-target check and non-Windows wasm32-unknown-unknown compile pass; the latter is compile evidence, not non-Windows test execution. Formatting, R1/R12/R13, IRV and scoped process cleanup pass. An unused Windows import was removed, followed by a warning-free engine all-target check.
- Direct TLC runs passed all four unchanged DeveloperHistory configurations (small, boundary, concurrent, crash), each with process_exited=true; a final Java process query found no matching survivor. Correspondence explicitly excludes age ordering, DPAPI and filesystem atomicity from TLC claims. Those boundaries remain covered by Rust tests and direct diff inspection.
- Integration corrected an invalid Add File envelope before mutation, resumed after the draft assumed multiline IRV JSON, and preserved all completed edits. A malformed TKW request ID was rejected before spawning; the corrected identifier completed once. Final IRV includes persistence and correspondence: history 6,961 physical LOC/25 files, CI 4,458 LOC/20 files with the existing warning. Final Phase 2 budgets and original baseline are unchanged and remain unmet.

## 2026-09-13 Extract current-user DPAPI behind Sealer (#178)

- Phase 2.2c moves protect/unprotect into store::crypto with the default-on dpapi feature. Engine keeps framing, CRC, queue, writer lifecycle and privacy admission. Parent inspected the complete diff: flags 0, absent entropy/prompt, null description, exact errors, copied return bytes and matching LocalFree remain unchanged. No zeroization behavior was added or claimed. Existing Windows bindings move ownership; no package version changes.
- Wrapped engine/settings/store tests pass 576/55/9 with the same two engine ignores; feature-off store tests pass seven. Offline workspace all-target check, wasm32-unknown-unknown feature-off compile, R1/R12/R13, formatting and scoped process cleanup pass. The portable result is compile evidence, not non-Windows runtime execution.
- Parent read back the two exact synthetic records created by the pre-extraction b1a5adf engine writer. Fixture SHA-256 remains 2f2ed513025ffb4ec8fc9232ce9b72b3c0ac4c72999926ee8c496c27eba9602c. The fixture stays under the task temporary directory and is bound to its creating Windows user. The temporary reader example was removed after execution; no real user history was accessed. Default-feature tests alone do not prove old encrypted-file compatibility, so preserve this provenance check for future crypto refactors.

## 2026-09-13 Specify the synchronous learning-store boundary (#178)

- Parent inventoried the current store surface (11 types/23 functions) and the learning migration before implementation. Pure codec extraction temporarily adds LearningRecord, LearningSnapshot, DecodedRecord and eight functions (14/31). Atomic transaction extraction replaces the temporary decoded type with seven named log/replay/receipt/error/result types and makes all eight low-level functions private while exposing eight log operations, yielding the final 20/31 bound. README and plan now specify the same explicit allowance; no runtime code changes in this prerequisite.
- Store owns synchronous durable state and exact operation ordering; engine retains runtime index, prediction history, sequence allocation, Busy admission, counters, mutex and maintenance thread. Open publishes upgrades before replay/truncation; compact/forget build runtime before publication; clear builds empty runtime after publication. Receipts must preserve maintenance-failure deltas on success and error, including committed FilteredCanonical recovery. TSV presentation moves to settings, not store; all repository callers must migrate because an inherent method cannot remain in engine once the type is store-owned.
- The parent-created pre-extraction learning fixture at bfa5eeb8 binds learning.bin SHA-256 7da8cb554c501bffe551e1f94a7501f1bb94e57cae9664a09d81494d3a0bc249 and exact TSV SHA-256 7fdd5700feebc5a08ff7ce0caa93ba065fcbe4e2225312fda01fce67956288b8. It contains two synthetic records around a repair-suppression record; retain it unchanged for post-extraction readback. This documentation prerequisite is checked by source/API inspection, diff check and original-baseline IRV.

## 2026-09-13 Extract pure learning format and codec (#178)

- Store now owns the existing learning v1/v2/v3 record layouts, bounds, CRC, framing and byte upgrade. Engine retains synchronous Log ownership, replay, index, thread, compaction and forget/clear transactions. Settings owns the exact TSV formatter; its public function is needed by the separate settings payload binary, so crate-private visibility is insufficient. Existing engine data exports remain compatible; the old inherent to_tsv method is replaced at every repository caller.
- Parent inspected and corrected draft patch contexts and the omitted removal of the old inherent implementation before compiling. The wrapped suites pass engine 576 (same two ignores), settings 56 and store 9; the new standalone old-writer golden passes one. Offline workspace all-target check, R1/R12/R13, formatting, diff check and process cleanup pass. All 37 existing learning test identities remain; the presentation assertion moves to an exact settings escaping test.
- The committed synthetic hex fixture records the original writer blob and reachable merged PR #190 source revision. Parent readback verifies exact old TSV, sequences 1/3, suppression replay and zero ignored tail. The original binary SHA-256 remains 7da8cb554c501bffe551e1f94a7501f1bb94e57cae9664a09d81494d3a0bc249. No user history is accessed. Original-baseline IRV passes with the existing CI growth warning (4883 LOC); the fixed history benchmark remains 7030 LOC. This slice does not complete Phase 2.2 or the final reading-volume target.
- PR195 review corrections make synchronous learning ownership and receipt-bearing success/error terminals authoritative in README and the plan. Phase 2.2d is explicitly three ordered changes: codec, atomic log/runtime adapter, then settings cutover including offline clear and TSV. The fixture's actual local execution revision is distinguished from remotely reachable ca19035d source; parent verified both use learning.rs blob 9abadc46f7885103c9a664811d2f2030dad8bef1. The following codec PR196 carries the literal fixtures. No runtime behavior is changed by this charter revision.
## 2026-09-13 Include the existing offline reranker consumer (#178)

- Parent located independent SKNR/SKNS definitions and encode_request/read_response in dictc neural_eval. Phase 2.3 must migrate this existing offline consumer to the shared protocol to satisfy R8; the charter now explicitly permits that consumer. No runtime consumer, protocol bytes, implementation or dependency manifest changes in this prerequisite.
- Verified the exact consumer row and source entry points, scoped diff, and original-baseline IRV comparison. The draft whole-document blob guard rejected unrelated merged desktop-plan updates before mutation; parent inspected the intervening plan diff and applied only the unchanged consumer row.
- Parent confirmed the three review findings against source: Phase 2.3's executable row, the contract inventory and original analysis still omitted the offline codec, and the allowed consumer must survive Phase 3.9's planned move to tools. Updated those records and added explicit offline evaluator verification without inventing its future package name. The research SCV1 protocol remains a separate contract.

- Correction: the first merge resolver assumed one plan conflict although two existed. A shell sequence continued after its failure and pushed unresolved markers to this unmerged PR. The follow-up preserves both the learning budget/step and approved reranker consumers/step, scans all tracked source for conflict markers, and requires explicit exit checks before committing. No runtime code changed and main was not merged from the invalid revision.

## 2026-09-13 Diagnose exact candidate appearance sampling (#168)

- Hosted run34716880545 failed the initial Dark sample (expected COLORREF 0x252525, observed 0x0c0c0c) before the Light publish. The log did not prove a repaint fault or high-contrast cause. This test-only correction records DPI, high-contrast query/result, system highlight, coordinates and exact expected/observed colors.
- Parent inspected selected-row fill, the rail, padding and border geometry. Candidate-only sampling uses logical (6,14) scaled with existing rounding, beyond the rail and before text at the middle of the first 28px row. Indicator sampling remains unchanged. The expected color follows the renderer's actual high-contrast override; both Dark and Light states are still published and asserted. Under high contrast equal pixels cannot prove processing of the second update, and the diagnostic explicitly records that limit. No timeout, tolerance, or production rendering behavior changes.
- Targeted offline compilation, formatting, diff check and process cleanup pass after correcting GetSysColor's u32 return to COLORREF. This is compilation evidence only; the ignored desktop suite must execute on the hosted desktop before acceptance. No existing user desktop state was changed locally.

- Hosted run34719573295 passes the appearance test and reaches selected-detail UIA, where the original 880-character geometry fixture shrinks from 620 to 260 pixels. Parent reproduced legitimate detail omission with the actual placement functions at 600 logical pixels and four DPIs: an oversized opaque pane would cover the composition. A bounded 160-character geometry fixture stays within candidate height and preserves exact width; the original 880-character payload remains a separate complete UIA assertion. Added monitor/DPI/rectangle diagnostics and a bounded initial geometry wait because UIA publication precedes SetWindowPos. Production rendering is unchanged. Wrapped renderer tests, formatting, diff checks and scoped process cleanup pass; hosted acceptance remains pending.
- Research charter follow-up: corrected the target graph from core to the SCV1 neural/context protocol owner, retaining the compiler-to-core edge and label. Merged current main including PR191 and PR193; conflict resolution keeps both journal entries and both independent charter corrections. Direct graph/plan inspection and diff checks verify this documentation-only change (#178).
## 2026-09-13 Consolidate reranker protocol v1 (#178)

- Phase 2.3 adds values-only sakura-rerank-proto as the owner of SKNR/SKNS layout and bounds, used by engine, neural worker and offline dictc. Candidate text is borrowed during encoding; allocation remains bounded by six descriptors and the existing frame ceiling. Scoring, process lifecycle, privacy and fallback remain consumer-owned. Cargo.lock changes only local dependency edges. Six top-level public items fit the eight-item charter.
- Parent inspected all implementation patches and rejected an earlier draft that removed worker tests. All nine existing worker protocol identities and their 26 assertions remain; four shared tests pin literal request/response bytes, nonfinite decode/encode behavior and transport-versus-payload truncation errors. Parent corrected test-helper placement, retained the worker self-test decoder caller, and preserved InvalidInput validation before allocating response descriptors. A complete malformed request remains InvalidData while truncated transport remains UnexpectedEof.
- Offline workspace all-target compilation passes without warnings. Wrapped engine suite (576 library passes/two existing ignores plus integrations), worker (26 passes), offline evaluator (six passes), and shared codec (four passes) pass. Existing real-model two-candidate IPC passes using the newly built worker and pinned FP32/ONNX Runtime payload under the task temp directory; Gate A remains failed, so this is protocol evidence only. No existing integration tests were edited. R1/R8/R12/R13, formatting, diff check and process cleanup pass.
- IRV now includes the shared definition and actual reranker consumers: candidate 15,993 physical LOC, history 7,030, CI 4,883 with the existing warning. The original baseline and final Phase 2 limits remain unchanged. Main CI still owns the full workspace aggregate; hosted desktop acceptance is tracked separately.

- PR194 follow-up: parent confirmed the previous worker decoder short-circuited unknown magic before version parsing. Shared decoding now preserves InvalidData for complete four/five-byte unknown headers, with a dedicated regression; valid frame bytes and transport truncation behavior remain unchanged. Shared/worker, engine long-conversion and offline evaluator wrapped tests pass, as do R1/R8/R12/R13, fmt/diff and scoped process cleanup. README restores the crate template's Issue types/Test commands including cleanup; canonical inventory points at the shared codec. Candidate IRV now includes the actual worker protocol adapter, measuring 16,259 LOC/17 files. Baseline unchanged; existing CI warning remains. Current main containing the approved charter is merged before fresh CI.

## 2026-09-13 Extract development-only reference oracles (#178)

- Phase 2.4 moves three independent oracle algorithms and three self-test modules to zero-dependency sakura-oracles. The three algorithm blobs are unchanged. Engine retains runtime correspondence tests and consumes the new crate only through a dev-dependency. Generator paths and the bounded mutation configuration follow the new owner. The original plan incorrectly counted twelve oracle modules; the executable plan now records the actual three algorithms and three self-test modules.
- R9 now permits oracle source only under the exact owner directory, rejects normal/build oracle dependencies, and blocks CI after migration. Positive/negative ownership and dependency fixtures pass. The existing store tests.rs was mechanically renamed to codec_tests.rs with a path attribute preserving its test namespace, so R9 also passes for the already-extracted store.
- Parent verification: offline all-target workspace compile; 530 engine library tests plus 46 oracle tests preserve the prior 576 passes and two engine ignores; nine store tests pass after the filename change. Fresh release engine dependency file under target/x86_64-pc-windows-msvc/release and normal Cargo tree exclude oracle. This is dependency evidence, not a symbol scan. R1/R8/R9/R12/R13 and dependency self-tests pass; no scoped runners remain.
- A source-hash-checked isolated copy ran cargo-mutants with the existing space-key selection and 60-second build/test bounds: 33 mutants, 32 caught, one unviable, zero missed/timeouts, successful baseline. This is an oracle-only score; the historical 28-mutant report remains historical evidence. Current raw outcomes are under C:/Users/developer/tmp/sakura-oracle-mutation-current/results/mutants.out. Source algorithms were not rewritten to improve the score.
- Original-baseline IRV comparison passes with the existing CI warning: history 7,030 LOC/26 files and CI 4,909 LOC. Final history limits remain unmet. Preexisting verification working-tree changes, including reports regenerated by correspondence tests, remain unstaged. PR190/191/193 are merged; hosted desktop appearance acceptance and subsequent phases remain open.

## 2026-09-13 Isolate dormant context research (#178)

- Phase 2.5 moves the four existing context modules (1,846 source lines before the move) and their allocation/latency integration suite into sakura-context-research. Normal dependencies are exactly values and the distinct SCV1 neural protocol. Engine has an optional context-research edge, no compatibility facade, and a feature-gated Session diagnostic retaining the actual MAX_SUGGESTIONS == 9 assertion. The research crate has zero externally public items; per-module dead_code allowances document its intentionally dormant status without widening its API.
- Parent compared the four production bodies after normalizing only visibility and the values import: all are identical. The 19 original unit tests and two allocation tests pass; the release-only microbenchmark remains ignored. Default engine library regression, feature engine diagnostic, offline workspace all-target compilation, feature clippy with warnings denied, formatting, R1/R8/R9/R12/R13, and process cleanup pass. Default release build succeeds and its fresh depfile and normal dependency graph exclude research. Cargo.lock adds only the local crate and optional edge.
- Application preflight exposed a .NET wildcard mistake: Directory.EnumerateFiles does not interpret bracket character classes. The draft now enumerates *.patch and validates exact anchored filenames. Its engine patch also omitted the existing timing module from context; parent preserved timing and resumed only the unapplied patches. A formatting-only diagnostic change was corrected after successful tests/clippy; no algorithm was changed to satisfy a check.
- IRV baseline remains unchanged: history 7,030 LOC/26 files and CI 4,909 with its existing warning. Final history target remains unmet. Current test-generated verification seed/report changes remain unstaged. PR194 and research-charter PR192 merged after all current-head required CI and installer checks; full desktop appearance suite run34719573295 is still pending, so it is not claimed as acceptance.

## 2026-09-13 Separate evaluator fixtures from shipping engine (#178)

- Phase 2.6 moves the real-engine candidate capture integration to ime-eval. Engine has no evaluator development dependency; dictc is optional behind dev-fixtures. Seven fixture-backed unit modules and eight integration targets retain explicit ownership. CI/release and contributor commands enable both fixture features. A uniquely named evaluator-only engine binary compiles the existing shipping entry point; default-run preserves the evaluator CLI.
- Parent verification: 21 package baseline capture, 107 suites, 1,920 passed, 91 ignored, zero failed; feature workspace all-target compilation and Clippy with warnings denied passed. Additional default-feature all-target checking exposed two fixture-hook references and unused helpers. Their cfg predicates now match their fixture callers; default checking passes without warnings. Engine/evaluator fixture regressions are rerun after this correction. Scoped process cleanup is mandatory and passed after completed runs.
- Recorder resolves target source labels against the owning manifest directory: two src/main.rs suffixes are not the same target. Regression fixtures cover both paths and reject unknown sources. Existing-file atomic replacement requires [NullString]::Value in PowerShell because an untyped null binds to an empty backup pathname; direct existing-file replacement and complete baseline publication passed after correction.
- Original IRV baseline is unchanged. Current physical readings: history 7,030 LOC/26 files, candidate 16,259 LOC/17 files, CI 5,029 LOC with existing warning. Final history acceptance remains unmet. Oracle PR197 and research PR199 are merged. Desktop run34720449429 now passes bounded detail-width assertions but fails its later caret-placement assertion; acceptance remains open. Test-generated verification changes remain unstaged.

## 2026-09-13 Move synchronous LearningLog into sakura-store (#178)

- Phase 2.2d step 2 moves durable open/append/maintain/forget/clear into store. Engine keeps the writer loop, queue, mutex, Busy admission, index, prediction history, sequence, generation and counters. Empty forget still terminates before the engine mutex. Exact forget publishes the replacement log, then the engine runtime callback, then cleanup; cleanup failure remains Removed with a receipt. Clear increments generation while the mutex is held. ReplayView::from_verified is crate-private and does not add a second CRC terminal.
- Learning publication keeps the existing two fs::rename steps and File::sync_all on temporary/replacement files. It does not sync the parent directory and does not use ReplaceFileW. The production store surface is 20 types and 31 functions; test-hooks stay default-off and reach engine only as a dev-dependency. The low-level codec module is crate-private. The pre-extraction golden stays a store library unit test so default package tests still decode it.
- Parent read back the original writer blob under C:/Users/developer/tmp/sakura-learning-before-extraction/learning.bin. SHA-256 remains 7da8cb554c501bffe551e1f94a7501f1bb94e57cae9664a09d81494d3a0bc249 and matches the committed hex decode. The original LF TSV SHA-256 remains 7fdd5700feebc5a08ff7ce0caa93ba065fcbe4e2225312fda01fce67956288b8; the committed fixture text matches after newline normalization. The writer blob was not regenerated. Settings still depends on engine; R2 remains an advisory warning until the later cutover.
- Parent verification: wrapped store/engine/settings tests PASS; store --no-default-features PASS; offline workspace all-target check PASS; R13 PASS; fmt/diff/process cleanup PASS. Original-baseline IRV comparison exits 0 with the existing CI warning: history 7,030 physical LOC/26 files, candidate 16,259/17, CI 5,029. Final history acceptance remains unmet. Test-generated verification seed/report changes remain unstaged. This slice does not complete Phase 2.2d.
- Hosted Clippy with `-D warnings` rejected `io::Error::new(ErrorKind::Other, ...)` at the capacity terminal. Parent replaced that one construction with `io::Error::other` and re-ran the same fixture-enabled workspace Clippy locally; the capacity message is unchanged.

## 2026-09-13 Point settings learning and history at sakura-store (#178)

- Phase 2.2d step 3 removes the settings `sakura-engine` Cargo edge. Learning view/export uses store `read_snapshot`; offline clear uses `LearningLog::open`/`clear` and discards receipts. History record types come from store. Settings owns TSV, viewing retention, `last_engine_identity`, and a DPAPI-framed `input.bin` scan. Store gained only the existing `SKIH` / 8-byte header constants. A store history `read_snapshot` was not added because that would exceed the 31-function budget. Engine keeps the writer lock, frame admission, and `ReplaceFileW`.
- Parent verification: wrapped `sakura-settings` + `sakura-store` tests PASS (56 settings identities after the isolated write fixture). R2 PASS; `cargo tree -p sakura-settings -e normal` has no `sakura-engine`; `rg sakura_engine crates/sakura-settings/src` is empty. The first retention-view fixture used timestamp 1 and made `view()` expire current records; the isolated write now uses wall-clock milliseconds so `view()` and `view_at(0)` keep the same two records while `view_at(u64::MAX)` still expires them and raw bytes stay unchanged.
- Residual: settings duplicates engine history frame walking. R1/R8/R12/R13 remain PASS. Test-generated verification seed/report changes remain unstaged. Hosted desktop acceptance and IRV-HISTORY-STORE 7,061/26 remain open. This slice completes the settings cutover only.

## 2026-09-13 Rename sakura-neural-proto to sakura-context-proto (#178)

- Phase 2.7 is a package/path rename only. SCV1 magic, WIRE_VERSION 2, and the existing 7 unit tests stay in the same lib.rs blob. Active manifests, source, CI baseline labels, and Cargo.lock drop the old package name. The plan's 2.7 verify command still searches for the old name; docs/history and that migration row keep it as provenance.
- Research and dictc dataset imports now use sakura_context_proto. Engine keeps the unused-by-source dependency after the rename. No protocol bytes were edited.

## 2026-09-13 Enforce R1/R2/R8/R12/R13 in CI (#178)

- Phase 2.8 makes the completed leaf-boundary rules blocking. CI keeps `-Advisory` for the still-open R3–R7 paths and now enforces R1, R2, R8, R9, R12, and R13. SelfTest rejects a workflow that drops that Enforce list.

## 2026-09-13 Record major version for the architecture product release

- Owner instructed that the product release of this architecture series is a major bump to 2.0.0, not 1.0.40. Workspace version stays 1.0.39 on intermediate PRs. The release job must align Cargo version, notes, installer, tag, and update-signing sequence. Signing policy is unchanged.

## 2026-09-13 Move conversion.rs into conversion/ (#206)

- Phase 3.1 starts with `git mv` only: `conversion.rs` → `conversion/mod.rs` and the sibling tests follow. Public names stay `Converter` / `ConversionOptions` / `ConversionCandidate`. IRV production paths for CORE-CONVERSION and ENGINE-CANDIDATE now point at `conversion/mod.rs` so the missing-file gate does not fire.
- A second commit moves candidate authority/evidence/origin/path-evidence into `conversion/evidence.rs` with `pub use` so facade paths stay. `Surface` is `pub(super)` for `PathEvidence::add_surface`. ENGINE-CANDIDATE IRV now includes `evidence.rs`. Search, ranking, and synthesis stay in `mod.rs`.

## 2026-09-13 Extract conversion candidate assembly (#206)

- Move `ConversionCandidate` / `ConversionSegment` and their methods into `conversion/candidates/assembly.rs`. Facade paths stay via `pub use`. Parent materializers keep writing fields through `pub(in crate::conversion)`. Search, ranking, synthesis, and raw-repair algorithms stay in `mod.rs`. ENGINE-CANDIDATE IRV now includes `assembly.rs`.

## 2026-09-13 Extract conversion synthesis so CORE-CONVERSION can drop mod.rs (#206)

- Adding type leaves to the same IRV file set while `conversion/mod.rs` remains required does not lower Semantic IRV and can raise Physical IRV. Phase 3 acceptance is `IRV-CORE-CONVERSION` ≤1,200 and `IRV-ENGINE-CANDIDATE` ≤4,000. The next extract is therefore the #99 change reason, not more options/input/result files.
- Move post-search synthesis into `conversion/synthesis/{punctuation,numerals,dates,single_kanji}.rs`. Lattice-time `add_numeric_forms` stays in `mod.rs` because it writes search nodes. Public names and research candidate ceilings are unchanged.
- `IRV-CORE-CONVERSION` production now lists the synthesis files plus `assembly.rs` / `evidence.rs` / `numerals.rs` / `calendar.rs` / `width.rs`, and no longer lists `conversion/mod.rs`. Measured Physical IRV 7,071 → 3,442. Phase 3 target ≤1,200 remains open (width split is 3.3; do not update `baseline.json`). ENGINE-CANDIDATE is not given the synthesis files; it fell 16,282 → 15,837 only because `mod.rs` shrank.

## 2026-09-13 Extract conversion ranking so ENGINE-CANDIDATE can drop mod.rs (#206)

- Move #94/#108 filters into `conversion/ranking/{coherence,quality_gate,it_terms}.rs`. Search, raw-repair, and synthesis stay out of that directory. `char_class` remains with lattice because build_lattice still uses it.
- `IRV-ENGINE-CANDIDATE` production replaces `conversion/mod.rs` with the ranking files plus `evidence.rs` and `assembly.rs`. Measured Physical IRV 15,837 → 11,958. Phase 3 target ≤4,000 remains open because `dispatch.rs` is still in the set (Phase 4). Do not update `baseline.json`.

## 2026-09-13 Point HISTORY-STORE IRV at store ownership after 2.2d (#178)

- Phase 2.9 measures acceptance; it does not hide source. The previous inventory still listed `sakura-proto/src/message.rs` (IME wire), `sakura-settings/src/cli.rs` (the whole settings CLI), `sakura-settings/src/input_history.rs` (view/export/review consumer), the `verification/history-*.md` dated H2 notes, and generic rules already counted as unconditional docs. After 2.2d those are other Issue types.
- The store/compaction/stats reading set is now the engine writer, the four store modules, `docs/decisions/developer-history.md`, DeveloperHistory TLA, and correspondence. File count and Physical LOC are re-measured against the previous baseline before any `-Out`. Do not drop a store module to make the budget.
- Compare against the previous snapshot: `IRV-HISTORY-STORE` 7,797 → 2,672 physical LOC / 8 files (−65.7%). Meets ≤3,000 and ≤8. Production stays the engine writer plus the four store modules; proto, settings CLI, settings view/export, dated `verification/history-*.md`, and unconditional-doc duplicates are other Issue types after 2.2d.
- On `be3b48c` (before merging #210), `IRV-CORE-CONVERSION` production was `calendar.rs`, `conversion.rs`, `numerals.rs`, `width.rs` — no proto. `-Out` recorded HISTORY-STORE 2,672/8, CORE-CONVERSION 7,511/12, CANDIDATE 16,259/17, CI 5,034.
- After merging origin/main (#210), `benchmarks.json` keeps the 2.9 HISTORY-STORE set and the 3.1 CORE/CANDIDATE sets. Re-measure: HISTORY-STORE 2,672/8 (unchanged), CORE-CONVERSION 3,442/17 (no proto), ENGINE-CANDIDATE 11,958/21. Compare against the `be3b48c` snapshot is PASS. Then `-Out` records this combined inventory. Phase 3 targets ≤1,200 / ≤4,000 stay open; do not hide a miss.

## 2026-09-13 Require caret-adjacent popup placement after detail follow (#168)

- Hosted all-mode run 34720449429 passed appearance sampling and bounded/oversized detail-width checks, then failed the later caret-follow assertion in `selected_detail_is_fresh_complete_and_noninteractive_over_an_owned_pipe`. The raw log printed work `RECT {0,0,1024,720}` at 96 DPI and the initial/short/long/oversized rectangles; it did not print the failed `after` rectangle. The assertion `after.left >= moved_anchor.left || after.top >= moved_anchor.bottom` rejects a permitted above-plus-left result.
- Parent compared `place`, `place_candidates`, and `popup_placement` at 6952d45 with the failing fixture. `document` is `None`, so placement uses `place`: below first (`top == anchor.bottom + gap`), then above (`bottom == anchor.top - gap`). `GAP_96` is 8 and is scaled with the existing test helper. Detail attaches right, left, then below; the painted HWND is the union, so a left-side pane moves `left` without changing the caret-adjacent top or bottom. Existing unit coverage already asserts `window.bottom == anchor.top - gap` when the list flips above, and `desktop_detail_width_fixture_keeps_the_pane_within_the_candidate_height` keeps short/long panes within candidate height at 96/120/144/192 DPI.
- Replaced the generic "position changed" waiter with a bounded waiter that requires both movement from the prior rectangle and exact one-gap adjacency above or below the moved caret. The timeout now reports previous/anchor/current rectangles, DPI, gap, and both placement predicates. Production rendering is unchanged. This is not a "coordinates changed" weakening.
- Verify: wrapped `sakura-renderer` package tests PASS; `cargo fmt --all -- --check` PASS; `git diff --check` PASS; scoped process cleanup PASS. The ignored desktop identity and hosted all-mode suite remain the acceptance gate. Preexisting verification working-tree reports stay unstaged.

## 2026-09-13 Sample candidate appearance from PrintWindow client bits (#168)

- Hosted all-mode run 34729225854 failed `appearance_switch_repaints_a_visible_candidate_popup` at the initial Dark assertion. Probe was `dpi=96 high_contrast_enabled=false sample=(6,14) expected=0x252525 actual=0x0C0C0C`. `0x0C0C0C` is not a palette color. The same assertion passed on hosted run 34720449429 at 6952d45; the only renderer test change after that run was the caret waiter.
- GetDC/GetPixel on the hosted DWM composition is the flaky instrument. The test now RedrawWindow(UPDATENOW)s, PrintWindow(PW_CLIENTONLY|PW_RENDERFULLCONTENT, then PW_RENDERFULLCONTENT)s into a client-sized DIB, and GetPixel from that memory DC. Expected Dark/Light selected colors stay `0x252525` / `0xE2E5E8`. Production painting is unchanged.

## 2026-09-13 Extract conversion options, input, and result leaves (#206)

- Move `candidate_budget` / `ConversionOptions`, the classified `ConversionInput` pair, and `ConversionResult` / diagnostics / error into `conversion/{options,input,result}.rs`. Facade `pub use` keeps public names. Repair, bridge, search, and ranking stay in `mod.rs`.
- These files are not added to `IRV-CORE-CONVERSION` or `IRV-ENGINE-CANDIDATE`. Adding type leaves to the same reading set while the god file remains does not lower Semantic IRV. Measured CORE-CONVERSION stays 3,442. Do not update `baseline.json`.
- Wrapped `cargo test -p sakura-core --lib conversion` PASS. fmt check and process cleanup PASS.

## 2026-09-13 Extract conversion bridge and repair-plan leaves (#206)

- Move cross-commit IDs/tails and the correction map / `RawRepairPlan` into `conversion/{bridge,repair_plan}.rs`. Facade `pub use` keeps public names. Search and raw-repair algorithms stay in `mod.rs`.
- These files are not added to `IRV-CORE-CONVERSION` or `IRV-ENGINE-CANDIDATE`. Measured CORE-CONVERSION stays 3,442. Do not update `baseline.json`.
- Wrapped `cargo test -p sakura-core --lib conversion` PASS. fmt check and process cleanup PASS.

## 2026-09-13 Extract conversion search lattice and Viterbi types (#206)

- Move `DictionaryEdgeBudget` / `Node` / `NodeSpec` / `CharClass` / `char_run` into `conversion/search/lattice.rs`, and `SearchState` / `HeapItem` / `PathClass` / `SearchRun` into `conversion/search/viterbi.rs`. Facade `pub(in crate::conversion)` keeps ranking and tests on the conversion names. `build_lattice` / `search_n_best` stay on `Converter` in `mod.rs`.
- These files are not added to `IRV-CORE-CONVERSION` or `IRV-ENGINE-CANDIDATE`. Adding search type leaves while `mod.rs` still owns the algorithms does not lower Semantic IRV. Do not update `baseline.json`.
- Wrapped `cargo test -p sakura-core --lib conversion` PASS. fmt check and process cleanup PASS.

## 2026-09-13 Move conversion search algorithms onto lattice and Viterbi (#206)

- Move `reset` / `build_lattice` / repair edges / `add_node` / suffix costs onto `conversion/search/lattice.rs`, and `search_n_best` / Viterbi materialization onto `conversion/search/viterbi.rs`. `add_numeric_forms` stays on `Converter` in `mod.rs` because it writes generated day edges after search.
- These files are not added to `IRV-CORE-CONVERSION` or `IRV-ENGINE-CANDIDATE`. Measured CORE-CONVERSION stays 3,442. Do not update `baseline.json`.
- Wrapped `cargo test -p sakura-core --lib conversion` PASS. clippy `-D warnings`, fmt check, and process cleanup PASS.

## 2026-09-13 Extract conversion ranking cost leaves (#206)

- Move `connection_cost` / `numeric_form_cost` / `synthetic_run_cost` into `conversion/ranking/cost.rs`. Facade `pub(in crate::conversion)` keeps search, assembly, and lattice-time numeric edges on the conversion names.
- These files are not added to `IRV-CORE-CONVERSION` or `IRV-ENGINE-CANDIDATE`. Measured CORE-CONVERSION stays 3,442. Do not update `baseline.json`.
- Wrapped `cargo test -p sakura-core --lib conversion` PASS. clippy `-D warnings`, fmt check, and process cleanup PASS.

## 2026-09-13 Extract conversion raw-repair algorithms (#206)

- Move the one-slot corrected-pass orchestrator and its sibling admit/merge frames into `conversion/raw_repair.rs`. Public `convert_input_with_raw_repair_plans` / wrapper names stay on `Converter`.
- This file is not added to `IRV-CORE-CONVERSION` or `IRV-ENGINE-CANDIDATE`. Measured CORE-CONVERSION stays 3,442. Do not update `baseline.json`.
- Wrapped `cargo test -p sakura-core --lib conversion` PASS. clippy `-D warnings`, fmt check, and process cleanup PASS.

## 2026-09-13 Extract conversion candidate dedup and ranking tie-break (#206)

- Move same-surface admission into `conversion/candidates/dedup.rs` and stable cost order into `conversion/ranking/tie_break.rs`. This completes the named Phase 3.1 module tree.
- These files are not added to `IRV-CORE-CONVERSION` or `IRV-ENGINE-CANDIDATE`. Measured CORE-CONVERSION stays 3,442. Do not update `baseline.json`.
- Wrapped `cargo test -p sakura-core --lib conversion` PASS. clippy `-D warnings`, fmt check, and process cleanup PASS.

## 2026-09-15 Phase 3.1 post-merge acceptance and stale C2 report (#206, #219)

- Symptom: on main `b8f9314`, `cargo test -p sakura-engine` passed but rewrote tracked `verification/developer-history/coverage/c2-report.md`, so a "tree unchanged after validation" guard failed.
- Root cause: Phase 2.4 (#178) moved the oracle to `crates/sakura-oracles/src/`, but the committed report still named `crates/sakura-engine/src/`. The test regenerates the report from the current path.
- Fix: commit the regenerated one-line scope path (#219, PR #220). The condition table is unchanged. `pbt-seed.txt` and `pbt-shrunk-counterexample.md` differed only in CRLF.
- Fixed-corpus verification (plan row 3.1): `quality-core-capture` → `quality-score` → `quality-compare --side candidate` on `conversion-quality-stage1` (50 cases), same `system.dic` (SHA-256 `ca1b24fc…`) on both sides. Before = `5746a0d` (parent of the first split PR #210, not #213). After = `b8f9314`. top-1 12→12, recall@18 38→38, MRR@18 0.39413→0.39413, segment_exact 22→22, changed_cases 0, rank_improved/regressed 0. The capture and report JSON are identical once provenance (git SHA, evaluator hash, fingerprint) is excluded.
- Other gates on `b8f9314`: C1 golden/roundtrip v22, R1/R2/R8/R12/R13, store/engine/settings/conversion tests, and IRV compare (all 0%) PASS. Desktop tests all-mode run 34910287304 had 31/31 passed and a clean cleanup.
- Learning: `quality-compare` exits 0 even when metrics differ. Judge bit identity from the `summary` deltas and `changed_cases`, never from the exit code. The CI Installer artifact embeds the dictionary, so there is no standalone `system.dic` in it. Any gate that runs engine tests should also check that the tracked tree is unchanged, because regenerated verification reports go stale silently.

## 2026-09-15 Prepare Sakura Input 2.0.0 release (#221)

- Owner chose to publish now as 2.0.0 instead of waiting for Phases 4–7. `docs/decisions/architecture-major-release.md` forbids 1.0.40, and this release is the first to ship the Phase 0–3.1 architecture work.
- Version sites moved together: workspace version and `Cargo.lock` (21 packages), `installer/setup.iss` `AppProductVersion` and `AppVersionedDir`, the `release.yml` default tag, and `release-sequence.txt` 7 → 8 (LF, `eol: lf` attribute confirmed). No dictionary input changed since v1.0.39, and protocol stays 22.
- Symptom: after the workspace test run, three more tracked verification files had rewritten themselves: `shift-latin-order` and `space-key-dispatch` c2-report, and `space-key-dispatch` oracle-provenance. Root cause is the same as #219: the oracles moved to `crates/sakura-oracles/src/` in #178, but the committed files still named `crates/sakura-engine/src/`. #220 fixed only the developer-history report. Fix: commit the regenerated path lines.
- Verified: fmt check, clippy `-D warnings` with the release.yml feature set, wrapped `cargo test --workspace` with the same features PASS, `git diff --check`. No test processes remained. The running `sakura_engine` belongs to the installed 1.0.39.
- Learning: `ci/run-test-quiet.ps1` needs `pwsh -Command`. Under `pwsh -File` the `-Command` argument arrives as a string, the wrapper fails to bind it, and the outer exit code can still read 0. When one generated report goes stale, regenerate and check all its siblings in the same fix.
- Review (Codex on PR #222, both confirmed in code). (1) The notes said an unsigned build is never fetched by auto-update. This was false, and 1.0.39's notes said the same. `updater.rs:565` accepts `(AuthenticodePolicy::Unsigned, AuthenticodeStatus::Unsigned)` once the manifest signature and SHA-256 pass. This has been so since #90 (bb2d2c6). (2) The CPU floor is AVX + SSSE3 (`setup.iss` `InitializeSetup`, `+avx,+ssse3`), not AVX alone. Learning: take release-note boilerplate from the code and the installer checks, not from the previous notes.

## 2026-09-15 2.0.0 published (#221)

- Merged PR #222 at exact head `d09cc52` → `2eac402`; annotated tag `v2.0.0` → `2eac402`. `release.yml` run 34925429911 success. Candidate: `sakura_setup.exe` 24,532,452 bytes, sha256 `8bc4059b94a3711159cf8377bcbf0c65e8a4048af1f921f6ca792ba1f0bc9c1c`, manifest `release_sequence=8`, `authenticode=unsigned`, `signing-status.txt` `unsigned-owner-approved`; both `gh attestation verify` with source/signer digest `2eac402` PASS.
- Published https://github.com/tsuyoshi-otake/sakura-input/releases/tag/v2.0.0 (`isDraft=false`, `publishedAt=2026-09-15T03:57:44Z`, 3 assets; local and readback `1 valid pinned signature`). The candidate was staged in `~/tmp/sakura-release-2.0.0` via `-ArtifactDirectory`, leaving the 1.0.39 files in `release-candidate/` untouched.
- Symptom: the first `publish-release.ps1` run hung >10 min with 0 CPU in the child `verify-update-manifest.ps1` right after signing, before any GitHub mutation. `Get-AuthenticodeSignature` alone took 0.3 s. Cause not proven; the run was launched from Git Bash with stdin as an open pipe. Fix: killed only that exact tree (56008/86764), confirmed no release existed, removed the partial `.sig` (the script requires exactly two candidate files), reran with `pwsh -NonInteractive ... < /dev/null` — completed in about a minute.
- Learning: run release PowerShell from Git Bash with `-NonInteractive` and stdin closed; a stale `.sig` from an interrupted run blocks the rerun by design.
- Open: the old handoff text said the updater refuses unsigned auto-update, but `updater.rs` accepts `authenticode=unsigned` + NotSigned since #90; decision docs and 1.0.39 notes should be reconciled separately.

## 2026-09-15 Correct the stale "unsigned builds are not auto-updated" statement

- Symptom: Codex review on PR #222 found that the release notes said an unsigned build is never fetched or run by automatic update.
- Root cause: `docs/decisions/release-signing.md` (2026-08-22) says so, and the 1.0.38 and 1.0.39 notes copied it. But since #90 (bb2d2c6) the v2 contract makes an unsigned release eligible. The eligibility row is at `verification/update-signing-v2.md:177`, and `updater.rs:565` accepts it. The conditions are all three of: the pinned manifest signature declares `authenticode=unsigned`; the size and SHA-256 match; `WinVerifyTrust` returns exactly `TRUST_E_NOSIGNATURE`. DESIGN.md and the 1.0.36 notes were already correct.
- Fix: add a dated correction to the decision file that cites the contract. The fail-closed rule stays. Correct the install paragraph of the 1.0.38 and 1.0.39 notes in the repository and in their published GitHub Release bodies. The 1.0.21–1.0.32 notes predate #90 and were accurate when written, so they are left as history.
- Learning: when an owner-decision file and a later contract disagree, check the code and the contract table first, then correct the decision file with a date. Do not let release-note boilerplate propagate from one release to the next.
- Review (CodeRabbit on PR #224): the note sentence omitted the size check and the exact `TRUST_E_NOSIGNATURE` condition. Both notes and both published Release bodies now state all conditions and link `verification/update-signing-v2.md` (75c7961). This resolves the open item in the entry above.

## 2026-09-15 Move dictionary.rs into dictionary/ (#206)

- Phase 3.2 starts with `git mv` only, as 3.1 did: `dictionary.rs` -> `dictionary/mod.rs`, and `dictionary_tests.rs` moves beside it under the same name so `#[path = "dictionary_tests.rs"]` still resolves without an edit.
- `IRV-DICTIONARY-FORMAT` entry point, production, and semantic range in `benchmarks.json` now name `dictionary/mod.rs`. `measure-irv.ps1 -Compare` compares only physical LOC per id, so `baseline.json` is not updated. The `docs/contracts/README.md` link follows the move. Other `dictionary.rs` hits (`verification/high-load-input-integrity.md`, `space-key-dispatch/mutants-files.txt`) are `sakura-engine/src/dictionary.rs`, not this file.
- Verified: wrapped `cargo test -p sakura-core --lib dictionary` PASS, IRV compare PASS (all 0%), fmt check, `git diff --check`, `check-process-clean.ps1` PASS.

## 2026-09-15 Extract dictionary image layout into dictionary/format.rs (#206)

- The inline `pub mod image_format { ... }` body moves verbatim into `dictionary/format.rs`, loaded as `#[path = "format.rs"] pub mod image_format;`. Public path `sakura_core::dictionary::image_format::*` is unchanged for dictc, sakura-ipc, and tests; no re-export was added. The module doc now cites `verification/dictionary-format-v2.md`.
- No other crate defines the format constants (SKRADIC/LOUD/MSP1/SBD1 grep: only sakura-core; dictc tests search for `SBD1` bytes only). `ImageVersion`, parse, validate, LOUDS, lookup, and detail stay in `mod.rs`.
- `IRV-DICTIONARY-FORMAT` enters at `format.rs`, lists it in production beside `mod.rs`, and its semantic range is `format.rs` 1-87. Physical 5,202 -> 5,207 (+0.1%, the new header lines). Do not update `baseline.json`.
- Verified: wrapped `cargo test -p sakura-core --lib dictionary` (22 tests listed) and `cargo test -p dictc` PASS, clippy `-D warnings` for both, fmt, `git diff --check`, IRV compare PASS, process cleanup PASS.

## 2026-09-15 Extract dictionary header and directory parsing into dictionary/parse.rs (#206)

- `Dictionary::parse` plus `validate_directory` / `required_table` / `optional_table` / `directory_table` / `expect_fixed_count` move into `dictionary/parse.rs` as a second `impl Dictionary` block. Grep proved the five helpers have no caller outside parse and each other. The child module reaches the parent-private table views, byte readers, and table validators through `super::`, so no visibility widened.
- The little-endian readers (`read_u16` 35 uses, `read_u32` 52, `to_usize` 35) stay in `mod.rs` because lookup, detail, and validate share them; moving them into parse would make lookup depend on parse.
- Line ranges were spliced from one snapshot of the original (1-16, `mod parse;`, 17-359, 611-1790, 1873-end), so no later range went stale. mod.rs 2,107 -> 1,775; parse.rs 344.
- `IRV-DICTIONARY-FORMAT` production adds `parse.rs` because a #109-type change now reads the header parser there; omitting it would hide reading. Physical 5,202 -> 5,219 (+0.3%). Do not update `baseline.json`.
- Verified: wrapped `cargo test -p sakura-core --lib dictionary` (22 listed) and `cargo test -p dictc` PASS, clippy `-D warnings`, fmt, `git diff --check`, IRV compare PASS, process cleanup PASS.

## 2026-09-15 Extract dictionary record and table validation into dictionary/validate.rs (#206)

- `validate_tables`, `validate_v2_surfaces`, `validate_v2_annotations`, `validate_v2_annotation_index`, `validate_details`, `detail_text`, `validate_offsets`, and the optional-table validators (`validate_boundary_table`, `validate_single_kanji_table`, `validate_matrix_table`) move into `dictionary/validate.rs`.
- Sibling visibility: `parse.rs` calls `validate_tables` and the three table validators, so only those four became `pub(super)`. `text_record` stays in `mod.rs` because `write_surface` / `write_annotation` also read through it (grep: mod.rs 782, 878).
- Imports were generated from the identifiers present in each extracted body, then proven minimal by clippy `-D warnings` (unused imports would fail). Ranges were spliced from one snapshot (1-17, `mod validate;`, 18-930, 1287-1540, 1740-end). mod.rs 1,775 -> 1,221; validate.rs 571; parse.rs 347.
- `IRV-DICTIONARY-FORMAT` production adds `validate.rs` (a #109-type change reads the validators). Physical 5,202 -> 5,239 (+0.7%). Do not update `baseline.json`.
- Verified: wrapped `cargo test -p sakura-core --lib dictionary` (22 listed) and `cargo test -p dictc` PASS, clippy, fmt, `git diff --check`, IRV compare PASS, process cleanup PASS.

## 2026-09-15 Phase 3.2: extract LOUDS trie navigation into dictionary/louds.rs (#206)

- Change: moved `Node` and `node`/`label`/`find_child`/`louds_bit` from `dictionary/mod.rs` into `dictionary/louds.rs`; the four accessors and Node fields are `pub(super)` (callers: lookup in mod.rs, validate.rs). `entry` stays in mod.rs (ENTR record decode, not trie). mod.rs 1,221 -> 1,154 lines; louds.rs 77 lines.
- Method: spliced from one snapshot with line-content markers asserted before extraction; imports generated from identifiers in the body; clippy -D warnings proves none unused.
- Verification: wrapped `cargo test -p sakura-core --lib dictionary` PASS, `cargo test -p dictc` PASS, clippy -D warnings, fmt check, git diff --check, IRV IRV-DICTIONARY-FORMAT 5202 -> 5249 (+0.9%) PASS, check-process-clean PASS.
- Learning: asserting the expected text at each boundary line before a sed splice turns stale line numbers into a hard stop instead of a silent mis-cut.

## 2026-09-15 Phase 3.2: extract reviewed dictionary details into dictionary/detail.rs (#206)

- Change: moved `Dictionary::detail_at` and `impl DictionaryDetail` (Issue #30 reader over DIDX/DREC/DREL/DTOF/DTXT) from `dictionary/mod.rs` into `dictionary/detail.rs`. No visibility widened (child module reaches private fields/readers via `super::`). mod.rs 1,154 -> 989 lines; detail.rs 178 lines. Branch `claude/phase32-dictionary-detail`, stacked on PR #227.
- Verification: clippy -D warnings, wrapped `cargo test -p sakura-core --lib dictionary` PASS, `cargo test -p dictc` PASS, fmt check, git diff --check, IRV IRV-DICTIONARY-FORMAT 5202 -> 5262 (+1.2%) PASS, check-process-clean PASS.
- Learning: my first module doc named the detail tables DETI/DETR/DETT from memory; `format.rs` defines DIDX/DREC/DREL/DTOF/DTXT. Take table tags in docs from the constants by grep, not recall.

## 2026-09-15 Phase 3.2: extract dictionary lookup queries into dictionary/lookup.rs (#206)

- Change: moved bunsetsu/connection/single-kanji/trie-search/prediction/visitor/surface/annotation queries from `dictionary/mod.rs` into `dictionary/lookup.rs` (522 lines). mod.rs keeps shared types, count accessors, `text_record`/`entry` (used by lookup and validate) and byte readers: 989 -> 480 lines. Leaf layout format/parse/validate/louds/lookup/detail is now complete. Branch `claude/phase32-dictionary-lookup`, stacked on detail branch.
- Verification: clippy -D warnings, wrapped `cargo test -p sakura-core --lib dictionary` PASS, `cargo test -p dictc` PASS, fmt check, git diff --check, IRV IRV-DICTIONARY-FORMAT 5202 -> 5275 (1.4%) PASS, check-process-clean PASS.
- Learning: splice header line numbers shifted because `cargo fmt` reordered `mod` items; the boundary assertion stopped the cut. Locate header anchors by grep, not by remembered line number. Also, an earlier Edit to benchmarks.json left `"detail.rs","mod.rs"` without a space; fixed in this commit.

## 2026-09-15 Phase 3.2: shipped dictionary through the split reader; #206 identity is stale (#206)

- Evidence: on `claude/phase32-dictionary-lookup` (c77bb11), `cargo test -p sakura-engine --features dev-fixtures --test shipped_dictionary_ranking -- --ignored --skip issue_83_cross_commit_bridge_release_percentiles` PASS (36 tests) against `artifacts/release/system.dic` 37,383,196 bytes, SHA-256 ca1b24fc7f3113998fc73e2722a10865ce36f1eae7f39d7e16170a333febb63f. check-process-clean PASS.
- Symptom on first runs: the target needs `--features dev-fixtures`; `issue_83_cross_commit_bridge_release_percentiles` panics by design outside `--release` (timing guard), not a reader failure.
- Finding: the 39,349,040-byte / b7d08643... identity quoted in #206 matches release notes v1.0.2-v1.0.6 only; v1.0.7 changed the data. The dictc writer is unchanged by #226-#228, so compiled bytes cannot change from this split; a rebuild comparison needs the private `-SystemCategoryDirectory` source.

## 2026-09-15 Phase 3.3: move width.rs and simd.rs under width/ and width/scan/ (#206)

- Change: `git mv` only: width.rs -> width/mod.rs, simd.rs -> width/scan/mod.rs, tests beside them. `pub mod scan;` in width/mod.rs; lib.rs `pub use width::scan as simd;` keeps `sakura_core::simd::startup` for engine main.rs and width_bench.
- Also closed #206 checkboxes 3.1 (acceptance evidence already in the 2026-09-15 post-merge entry, box was never ticked) and 3.2 (identity note: the 39,349,040-byte hash is the v1.0.2-v1.0.6 image).
- Risk checked: kernel symbols become `_ZN11sakura_core5width4scan...`. `ci/check-simd-assembly.ps1` Get-FunctionBody matches `_ZN[^
]*<name>\d+h...E`, so it is path-agnostic. Local -SelfTest 5/5 mutants rejected; real gate passed with the new symbols.
- Verification: fmt, git diff --check, clippy -D warnings (core with simd-assembly-audit, engine), wrapped `cargo test -p sakura-core --lib` and `--features simd-assembly-audit` PASS (299 lib tests), IRV CORE-CONVERSION 3442 -> 3444 PASS, check-process-clean PASS.
- Learning: before moving a module that a CI gate audits by symbol, read the matcher; a path-anchored regex would have failed only in CI.

## 2026-09-15 Phase 3.3 follow-up: SIMD test filter silently matched zero tests (#206)

- Symptom: after moving simd.rs to width/scan/, `cargo test -p sakura-core --lib -- simd:: --list` listed 0 tests (width::scan:: lists 16). The CI step "Exercise the SIMD kernels this runner supports" and scripts/verify-phase1.ps1 would have passed without running any kernel-agreement test.
- Root cause: `pub use width::scan as simd;` keeps the API path, but libtest filters match the module path where tests are defined, not re-export paths.
- Fix: filter changed to `width::scan::` in ci.yml, verify-phase1.ps1 and docs/rules/ci-verification.md. Caught before merging PR #229.
- Verification: run-test-quiet SIMD kernel agreement PASS; IRV PASS; process-clean clean.
- Learning: when moving a module, grep CI, scripts and rules for test filters naming the old path, and compare `--list` counts before and after. Exit 0 does not prove any test ran.

## 2026-09-15 Phase 3.4 keymap split (#206)
- Change: keymap.rs -> keymap/{mod,vocabulary,key_spec}.rs; tests moved beside. benchmarks.json IRV-ENGINE-KEY-MODE lists the three files.
- Symptom: the planned name config.rs shadowed crate::config inside keymap; tests calling config::parse failed with E0425. Also Trigger is used by non-test KeyMap::find, so a cfg(test)-only import failed.
- Fix: named the syntax module key_spec; Trigger imported unconditionally.
- Verification: clippy -D warnings core+engine; core lib 299 tests listed (unchanged, 63 keymap); dependency rules self-test+enforced PASS; IRV PASS; process-clean clean.
- Learning: before naming a child module, check that its name does not collide with a crate-root module the parent already imports (use crate::X::{self,..}).
