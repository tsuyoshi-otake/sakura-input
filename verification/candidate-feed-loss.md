# Candidate popup lifetime after feed loss — Issue #255

The subsequent [candidate lifetime audit](candidate-lifetime-audit.md) covers
additional healthy-feed host-disconnect and native-placement failure defects.
The scope and counts below record the earlier feed-loss fix on its own.

## Contract and scope

The user reported a conversion candidate list remaining beside the text. The
application and triggering action are not yet identified. Investigation found a
specific reproducible defect: `watch::follow` returned after a disconnected,
timed-out, or invalid `WatchUi` response without invalidating the last displayed
snapshot. Production recovery then waited/reconnected while that popup remained
visible. This establishes a faulty path, not the cause of every reported occurrence.

The watcher owns feed validity and bounded recovery. The renderer UI thread owns
candidate windows, row click targets, accessibility visibility, and the transient
mode indicator. `Signal::Unavailable` transfers invalidation to that thread before
the watcher returns to recovery. Invalidation and snapshots use the same latest-value
mailbox, so queued notification order cannot replay a superseded snapshot or hide a
newer recovered one. Pad preferences and content are not reset by invalidation.

There are no protocol, candidate ordering, composition, persistence, or dependency
changes. The existing 15-second watch budget and reconnect backoff remain intact;
a stalled feed can therefore remain visible until its existing timeout fires.

## Executable acceptance rubric

Run commands from the repository in PowerShell 7 with
`$env:CARGO_HTTP_CHECK_REVOKE = 'false'`. Ordinary Cargo tests must pass through
`ci/run-test-quiet.ps1`. The verified runs additionally used TKW with a 240-second
timeout to retain output and bound test lifetime.

1. Invalidate on a disconnected/timed-out/invalid feed, before returning to recovery.
   - Verify: `./ci/run-test-quiet.ps1 -Name 'feed loss' -Command { cargo test -p sakura-renderer --lib watch::tests::lost_feed_invalidates_candidates_before_returning_to_recovery }`
   - Expect: one passing test covering all three outcomes; an unchanged heartbeat
     does not emit a second display update and no internal retry occurs.
2. Preserve handshake rejection, reconnect revision reset, and deliberate shutdown.
   - Verify: `./ci/run-test-quiet.ps1 -Name 'watch contracts' -Command { cargo test -p sakura-renderer --lib watch::tests }`
   - Expect: rejection invalidates; a fresh revision 1 is accepted; stopping returns
     the deliberate terminal outcome without retrying.
3. Hide actual owned popup and click-target HWNDs, and honor latest state ordering.
   - Verify: `./ci/run-test-quiet.ps1 -Name 'native popup lifetime' -Command { cargo test -p sakura-renderer --lib lost_feed_hides_native_popup_and_click_targets_without_replaying_stale_updates }`
   - Expect: both HWNDs become invisible, stale queued updates do not restore them,
     a fresh recovered snapshot shows them, and the appearance preference survives.
     The fixture uses its own windows and no installed engine or user data.
4. Preserve renderer regressions and static contracts.
   - Verify: `./ci/run-test-quiet.ps1 -Name 'renderer regression suite' -Command { cargo test -p sakura-renderer }`
   - Verify: `cargo clippy -p sakura-renderer --all-targets -- -D warnings`
   - Verify: `cargo fmt -p sakura-renderer -- --check`
   - Verify: `./ci/check-dependency-rules.ps1`; `git diff --check`
   - Expect: no failures or new enforced architecture violations. Existing advisory
     architecture warnings and pending migration gates are not claimed as resolved.
5. Leave no repository test processes behind.
   - Verify: `./ci/check-process-clean.ps1` after tests and compilation have exited.
   - Expect: no repository-scoped Sakura process or test runner remains.

## Evidence (2026-09-22)

- Before adding feed invalidation, the new feed-loss test failed with one `Ui`
  signal instead of the required `Ui, Unavailable` sequence (TKW run
  `6175db8cc286a48ab6b4d10cad3729d8`).
- After the fix, renderer tests passed: 150 passed, 0 failed, 11 ignored across
  all targets. All four new tests executed, including the native HWND test
  (TKW run `01dd3def57a971c15b77cbe6cb90d104`).
- Clippy with warnings denied passed after removing a now test-only import from
  production imports (TKW run `66dc3bf634c86d7ee4bae08515887a0d`).
- The final rerun after that import fix passed with the same test counts (TKW
  run `04934bd7c08eabbdfe5b7106a9aaaec3`). Format/diff checks passed, and the
  sequential process audit reported no repository-scoped test runners remaining.
- Installed engine and renderer were observed under version
  `2.0.3-7b18d10759a438c0`. They were neither stopped nor replaced.

Ignored integration tests remain ignored; no full installed-host end-to-end
reproduction or release/install is claimed. Focus-loss or host lifecycle causes
with an otherwise healthy feed remain outside this verified fix.

## Understanding scope and future IRV

- Actual scope: production changes in renderer `watch.rs` and `lib.rs`; regression
  assertions in their unit tests and `candidate_tests.rs`; contracts from `UiState`
  and candidate `hide`; candidate-popup and test-output decisions.
- Expansion: initially inspected TSF candidate/focus teardown and the engine UI
  board to locate the owner, then narrowed the demonstrated defect to feed validity.
  Mandatory machine/re-entrancy guides exceeded the three-guide review trigger;
  this did not require architectural restructuring or edits to TSF/engine.
- Architecture: feed validity remains owned by the watcher; native visibility by
  the UI thread. The internal signal contract adds an unavailable state. No new
  crate dependency or wire guarantee is introduced.
- Future IRV: a search for `lost_feed` now finds both the deterministic feed
  regression and native-window regression. Similar feed-loss reports can start
  here without first opening TSF composition internals. They must distinguish
  feed unavailability from a healthy feed retaining a stale engine snapshot.
