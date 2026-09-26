# Sakura Pad protected-store transaction — #269

Status: whole-Pad password and one-time recovery-key enrollment are connected
in the checkout; the broader protection plan remains in progress. Existing
Pads stay in v1/v2 DPAPI storage until their owner explicitly enables
protection. No migration of the user's actual Pad, installation, or release is
claimed here. Per-memo password and recovery-key protection has separate
evidence in `sakura-pad-memo-protection.md`; YubiKey and TOTP are not connected
to the product.

## Rubric

| Criterion | Verify | Expect |
| --- | --- | --- |
| Preparation preserves legacy | Scoped `pad_storage` tests inject failure into each protected-copy write/read/verification step | Legacy primary/backup behavior remains available; no cutover marker is published |
| Committed intent is fail closed | Scoped tests inject failure after durable intent and before each retirement/publication step | New `PadStore::load` and `write` never read or publish v1/v2; recovery selects only verified v3 copies or reports an error |
| Successful cutover has no old-readable copy | Scoped tests inspect primary, `.bak`, `.tmp`, protected primary/backup and markers after commit | Old primary is a newer-version tombstone; old backups/temp are absent; protected copies open and match the original document |
| Writer and migration serialize | Contract test holds a legacy write while migration tries to commit, then reverses ordering | Neither a stale v2 write nor a stale prepare result can overwrite the committed protected state |
| Password-free reseal within a session | Worker envelope tests unlock once, modify content, reseal repeatedly, and reopen from password | Fresh nonces, authenticated scope, no raw password reuse for each save, wrong credential/tamper yields no session/plaintext |
| Native lock never exposes memo controls | Run `pad_ui::migrated_protected_pad_starts_and_reopens_without_legacy_text` and `pad_ui::session_lock_removes_unlocked_protected_memo_hwnds` with isolated storage and the real session worker | Locked startup and Windows session lock have no title/body/list HWNDs; reopening stays locked and topmost |
| Enrollment is a distinct transaction | Run `pad_ui::enrollment_prompt_can_cancel_without_cutover`, `pad_ui::whole_pad_recovery_key_is_confirmed_before_cutover_and_unlocks_after_reopen`, and the real-worker recovery test | The key is displayed before durable intent; confirmation cuts over; cancellation keeps legacy storage and restores its writer; both password and recovery unlock survive reopening |
| Session lock clears unconfirmed key | Run `pad_ui::windows_lock_clears_unconfirmed_whole_pad_recovery_key` against an isolated renderer | The key vanishes from native child text, no cutover intent is published, and the original editor reopens |
| Repository checks and process lifetime | Wrapped Cargo tests, fmt, clippy, dependency/audit gates, and `ci/check-process-clean.ps1` | Required suites pass, no runner survives, no package is added younger than seven days |

The storage transaction owns the durable protected cutover and version guard.
The worker owns password KDF, authenticated encryption, and the unlocked
session. The renderer owns UI state, save epochs, confirmation before cutover,
and the native locked surface. These contracts establish the whole-Pad
password and recovery-key paths. UI Automation client traversal,
forced-process-exit recovery of an uncertain unsaved edit, YubiKey PRF, TOTP,
and migration of real user data remain separate acceptance criteria in
`docs/plans/sakura-pad-protection.md`.

## Evidence recorded so far

- Wrapped `cargo test --locked --workspace --features
  sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture -- --test-threads=1`:
  pass; process check found no repository-scoped Sakura or test runner afterward.
- Isolated real-worker enrollment, reopen, save, lock, and second unlock:
  pass. The executable is supplied through `SAKURA_PAD_SESSION_TEST_EXE`.
- Native real-worker unlock followed by simulated `WTS_SESSION_LOCK`: pass;
  protected title/body/list HWNDs were destroyed and the same topmost Pad
  reopened locked. Enrollment cancellation: pass. The tests use unique
  temporary application data, not the user's Pad.
- An isolated renderer accepted Settings' registered, data-free activation
  message and opened a topmost Pad: pass. No production renderer or Pad was
  contacted by this test.
- The isolated native HWND suite passed all 10 non-manual ignored tests,
  including existing title/scroll/layout regressions, protected startup,
  Windows lock, enrollment cancel, and Settings activation.
- Recovery v2 envelope and session protocol tests pass for password and
  recovery-key routes, authentication, and reseal. New whole-Pad enrollment
  uses v2, while earlier v1 vaults still open through their password route.
  The isolated native UI passed key display/confirmation before cutover,
  recovery unlock, password unlock after reopening, cancellation with legacy
  writer restoration, and key erasure on a simulated Windows session lock.
- Storage cutover/fault tests: 31 pass; protected actor tests: four pass.
- The quiet runner's workflow self-test passed after the new CI step was
  written in the form it recognizes.
- Workspace Clippy with CI features and `-D warnings`, `cargo fmt --all
  -- --check`, dependency-policy self-test and full check, and
  `cargo audit --file Cargo.lock` passed. The IRV regression gate returned
  success with warnings for the intentionally broad storage and CI scope.

The native confirmation button records the user's assertion that the recovery
key was saved. The plan's stronger in-product proof (masking the displayed key
and asking the user to enter a portion from the saved copy) is still open.
The isolated test verifies that the displayed key actually unlocks after a
fresh session; it does not prove a user stored it externally.
