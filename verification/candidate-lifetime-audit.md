# Candidate lifetime follow-up audit — Issue #255

## Scope and confirmed findings

The follow-up request was to check for other defects. This audit follows candidate
ownership through TSF focus/composition completion, engine session and pipe
termination, renderer feed recovery, and native popup placement. It is not an
exhaustive audit of unrelated settings, updater, or dictionary functionality.

1. A host connection closing reset its dispatcher sessions but left its candidate
   snapshot and placement on the shared UI board. A healthy renderer connection
   could therefore keep displaying a dead host's candidates indefinitely. A new
   isolated real-process test failed with `dead host retained candidates` (TKW
   `4f2dc18088a9523be3a3de61320a8b66`). Server termination now asks the board to
   clear that connection's ownership before reusing the worker. The board compares
   the current owner while holding its lock, so a newer peer's popup survives.
   Clearing also disposes of any queued click for the dead owner.
2. Invalid DPI placement cleared internal visibility and the click surface while
   leaving the display HWND visible. Related failed placement branches had the
   same inconsistency. A native-window test failed with `invalid DPI left the
   display visible after state became hidden` (TKW
   `bb466b993bdd2aad1eea8120bd5f4425`). Placement failure must hide display, click
   surface, and accessibility state together. A subsequent valid snapshot must
   be able to show the popup again. The invalid DPI input is fault injection,
   not evidence that ordinary monitor switching necessarily triggers this error.
   Invalid overlay-handle fault injection also reproduced the ordinary update
   failure (`IsWindowVisible` still true, TKW `836eeb1c9e87e94b0bdd6b1aeb266717`).
   All terminal placement branches now share the existing popup hide invariant;
   the owned PaintState holds the display/overlay handles used for cleanup.

## Executable acceptance rubric

Use PowerShell 7 from the repository with
`$env:CARGO_HTTP_CHECK_REVOKE = 'false'`. Route each ordinary Cargo test through
`./ci/run-test-quiet.ps1 -Name <name> -Command { cargo test <arguments> }`.
The verification runs use TKW with a 240-second outer timeout.

1. Verify: `cargo test -p sakura-engine --features dev-fixtures --test candidate_lifetime_pipe`.
   Expect: a separate renderer watch observes empty candidates, empty placement,
   and a newer revision when the owning host disconnects. A fresh host can connect.
   The owned isolated engine exits successfully; no installed engine is accessed.
2. Verify: `cargo test -p sakura-engine --features dev-fixtures --lib disconnected_connection`.
   Expect: foreign disconnect and repeat cleanup preserve revision; owner cleanup
   clears candidates, geometry, visibility, and queued click. A newer owner with
   the same session number survives late cleanup of the old connection.
3. Verify: `cargo test -p sakura-renderer --lib candidate::tests`.
   Expect: native placement failure tests hide both HWNDs and accept a later valid
   snapshot. Existing feed-loss, geometry, click-target, and DPI tests pass.
4. Verify: `cargo test -p sakura-engine -p sakura-renderer -p sakura-tsf --features sakura-engine/dev-fixtures --lib`;
   `cargo test -p sakura-engine --features dev-fixtures --test pipe_round_trip`;
   `cargo test -p sakura-renderer`.
   Expect: engine, renderer, TSF focus/completion, and real pipe regressions pass;
   ignored tests remain explicitly reported.
5. Verify: `cargo clippy -p sakura-engine -p sakura-renderer -p sakura-tsf --all-targets --features sakura-engine/dev-fixtures -- -D warnings`;
   `cargo fmt -p sakura-engine -p sakura-renderer -p sakura-tsf -- --check`;
   `./ci/check-dependency-rules.ps1`; `git diff --check`;
   then `./ci/check-process-clean.ps1` after all compilers/tests have exited.
   Expect: no new static violations and no repository-scoped runner remains.

## Understanding scope and architecture

- Actual scope: engine `server.rs` connection finalization, `ui.rs` ownership and
  click intent, dispatcher owner identity contract, and one new private-pipe test;
  renderer candidate placement and native regression tests; TSF focus/completion
  paths and their existing tests. Existing feed-loss changes are preserved.
- Expansion: the healthy-feed case required following state upstream into the
  engine, beyond the earlier renderer-only fix. Review crossed the normal guide
  and source reading triggers because the popup has separate host, engine, and
  renderer lifetimes. It did not justify a broader structural refactor.
- Architecture: the server owns connection termination; UiBoard owns candidate
  identity and atomic invalidation; the renderer thread owns native visibility.
  There are no protocol, crate dependency, persistence, candidate ranking, or
  TSF composition specification changes.
- Future IRV: `candidate_lifetime_pipe` and `disconnected_connection` locate the
  previously untested teardown contract directly. Native placement failure tests
  locate the display/state invariant. Future investigations must distinguish host
  disconnection, renderer-feed loss, and native positioning failure. This does
  not reduce the knowledge needed for unrelated TSF reentrancy bugs.

## Limits

No release, installation, or installed-host reproduction is included. The user's
original triggering application remains unidentified. Passing these regressions
does not prove that every way of retaining a candidate popup has been eliminated.

## Verification evidence (2026-09-22)

- Engine and TSF library suites: 512 and 200 passed respectively; engine has two
  existing ignored tests (TKW `e90653dc64bbe9e6246e9382887fb64c`). This includes
  the new owner/peer/repeated-cleanup regression and existing focus, completion,
  and deferred-work tests. The renderer library also passed at this intermediate
  point; its final verification is the next entry.
- Final renderer suite after the shared placement cleanup: 152 passed, zero
  failed, 11 ignored across all targets (TKW `768dc8e065c36df3137a1a78d3ea0e7a`).
  Both new native fault-injection tests and prior feed-loss regression executed.
- Final real-process suites: candidate lifetime 3 passed (including two fixture
  self-tests), pipe round-trip 11 passed, zero failed/ignored (TKW
  `05e8505087ac59703854523ab8bfd740`). The positive disconnect test completed
  without waiting for the five-second unchanged-state heartbeat seen pre-fix.
- The latest runs of these targets total 878 passed and 13 ignored; intermediate
  reruns are not added to that total. No ignored integration test is claimed run.
- Clippy for all three crates/all targets with warnings denied passed (TKW
  `07e8ddac1270ac500be4d7a59e9e8971`). Format and diff checks passed. Dependency
  checks passed with pre-existing transitional warnings/pending gates unchanged.
- The sequential process audit reported `clean: no repository-scoped Sakura or
  test runner remains`. Tests used private pipes, owned child PIDs, and isolated
  profiles. No installed IME process was stopped or replaced.
- This evidence records local verification. Release preparation is below;
  publication and installed-host verification remain separate gates.

## Version 2.0.4 release preparation (2026-09-22)

The workspace, installer version, release workflow default, and lockfile now
select 2.0.4. The update release sequence advances from 11 to 12. No external
dependency version changes are included. Release notes describe the three
confirmed failure paths and the remaining reproduction limits.

- Verify: wrapped `cargo test --workspace --locked --features
  sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture` with a 900-second
  outer timeout. Expect: all ordinary workspace targets pass. Result: 1,943
  passed, zero failed, 91 ignored across 113 libtest results (TKW
  `07090d315389d25e2747541a5e40111a`). This supersedes the narrower total above;
  ignored desktop, dictionary, and benchmark targets are not claimed executed.
- Verify: `cargo clippy --offline --workspace --all-targets --features
  sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture -- -D warnings`;
  workspace format/diff checks; facade check; dependency policy and its self-test;
  IRV comparison with `verification/irv/baseline.json`. Expect: no new violation.
  Result: all passed. No architecture baseline was relaxed.
- Verify: `cargo audit --file Cargo.lock`. Expect: no known vulnerability in the
  current advisory database. Result: 83 dependencies checked, no finding (TKW
  `2739828afdbe55566402b320a835cc9e`).
- Verify: `cargo build --workspace --release --locked`, then
  `ci/check-dll-size.ps1 -DllPath
  target/x86_64-pc-windows-msvc/release/sakura_tsf.dll`. Expect: successful build
  and DLL below 1 MiB. Result: passed, 440,832 bytes (build TKW
  `ebd3525a081cc98b20ee7e7b476d1c35`). An initial size-check invocation named the
  wrong, unqualified target directory; the configured target above resolved it
  without another build.
- Verify: `ci/check-process-clean.ps1` after the completed runs. Expect: no
  repository-scoped runner or Sakura process. Result: clean.

The mandatory publication check identified pre-existing author metadata in Git
history. The owner requested an anonymization assessment before publishing.
The read-only assessment is kept outside the public repository. No remote
history, branch protection, tag, published release, or installed binary was
changed during this preparation. Release workflow dictionary gates, published
asset verification, and reinstallation still require completion after that
decision; a local release-profile build is not a published installer.
