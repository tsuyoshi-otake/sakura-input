# Sakura Pad protected-store transaction — #269

Status: stage3 whole-Pad password, recovery-key, WebAuthn PRF, and optional
local TOTP paths are implemented in the checkout; the broader protection plan
remains in progress and PR #270 remains draft. Existing
Pads stay in v1/v2 DPAPI storage until their owner explicitly enables
protection. No migration of the user's actual Pad, installation, or release is
claimed here. Per-memo password, recovery-key, and WebAuthn PRF protection has
separate evidence in `sakura-pad-memo-protection.md`. A physical YubiKey 5 NFC
(firmware 5.4.3) successfully enrolled and reopened an isolated whole-Pad store
through the real worker. The physical native Pad UI later completed recovery
confirmation, reopened the correct title/body, and exited its isolated renderer.
Its original long-running
fixture stopped its watch feed after the renderer's 15-second idle deadline;
the Pad correctly vetoed that premature shutdown during registration. A
three-second fixture heartbeat now has an 18-second native regression test.
An earlier heartbeat-enabled physical registration did not complete within
180 seconds even though both Windows Security prompts were completed. Stage-only
diagnostics remain for recurrence; the intermittent stop is unexplained. The
enrollment UI now polls the worker result if a completion window message is
lost and gives preparation, confirmation, cancellation, and postapproval work
explicit deadlines. A native regression passed with both completion messages
deliberately suppressed. A later physical native Pad UI retry passed the whole
enrollment/recovery/unlock/exit path with the timer in place. This does not
prove the earlier physical stall was a lost message.
Whole-Pad password change is implemented for password-backed v2 vaults,
including those with local TOTP confirmation enabled. An enabled TOTP sidecar
requires a fresh code before rewrap; cancellation and a wrong code leave the
encrypted primary unchanged. Changing a hardware-backed vault, adding/removing
spare keys, and unprotecting a Pad remain open. TOTP is local confirmation
backed by a current-user DPAPI sidecar; it is not independent cryptographic
protection or online 2FA.

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
| Enrollment completion does not depend on a posted message | Run `pad_ui::whole_pad_enrollment_polls_when_completion_posts_are_lost` with the debug fixture suppressing both completion messages | The Pad still presents the recovery confirmation and reaches a terminal enrollment result |
| Session lock clears unconfirmed key | Run `pad_ui::windows_lock_clears_unconfirmed_whole_pad_recovery_key` against an isolated renderer | The key vanishes from native child text, no cutover intent is published, and the original editor reopens |
| Password change is transactional | Run `pad_storage::tests::password_rewrap*`, `pad_protection::process_tests::real_worker_password_change_preserves_recovery_and_rejects_old_replay`, and `pad_ui::whole_pad_password_change_rejects_old_password_and_preserves_memo` | Wrong old password changes nothing; an interrupted rewrap has one explicit authoritative generation; a new password and the original recovery key open the same document; replaying the old primary alone does not restore the old password |
| TOTP-gated password change | Run ignored `pad_ui::whole_pad_password_change_requires_fresh_totp_before_rewrap` against an isolated renderer and store | Cancellation and a wrong code leave the primary bytes unchanged; a fresh code permits rewrap; afterward the old password fails and the new password plus a later fresh code opens the unchanged memo |
| Interrupted v3 save retains a protected draft | Run `pad_storage::tests::durable_v3_draft_survives_staging_kill_and_retry_cleans_it` and `pad_ui::v3_draft_requires_password_before_choice_and_restores_after_forced_exit` with isolated storage and renderer | A staged draft survives forced process exit without becoming the primary; no draft choice or plaintext appears before successful password authentication; accepting recovery publishes the new generation and removes the draft |
| Suspend revokes the surface | Run `pad_ui::host_suspend_masks_unlocked_whole_pad_before_reopen` against an isolated renderer and session worker | A simulated host `PBT_APMSUSPEND` removes protected title/body/list HWNDs before the Pad can reopen, which requires authentication |
| Settings observes and locks without document data | Run `pad_ui::settings_protection_and_lock_messages_route_to_isolated_pad` and Settings' `pad_status_wire_values_have_explicit_user_readings` | A status query on a closed Pad does not create its window; the lock reply confirms masking before Settings says complete; unknown state is shown as unavailable |
| Repository checks and process lifetime | Wrapped Cargo tests, fmt, clippy, dependency/audit gates, and `ci/check-process-clean.ps1` | Required suites pass, no runner survives, no package is added younger than seven days |

The storage transaction owns the durable protected cutover, version guard, and
password-change digest marker. The marker is DPAPI-protected and qualifies the
authoritative encrypted primary or backup after rewrap. A same-user adversary
who can roll back the complete Pad directory, including that marker, can still
restore an old password; this local format has no trusted monotonic counter.
The worker owns password KDF, authenticated encryption, and the unlocked
session. The renderer owns UI state, save epochs, confirmation before cutover,
and the native locked surface. The WebAuthn adapter requests user verification
and derives scope-bound PRF material. The physical test first reproduced
`PrfUnavailable` with a PRF-only registration request; requesting Windows'
legacy `hmac-secret` extension during credential registration enabled the
tested YubiKey 5 NFC path. Microsoft documents that its `webauthn.h` maps PRF
values to the HMAC-secret extension ([header](https://github.com/microsoft/webauthn/blob/master/webauthn.h));
Yubico documents the firmware capability matrix in its [technical manual](https://docs.yubico.com/hardware/yubikey/yk-tech-manual/yk5-firmware-overview.html).
The TOTP sidecar owns its local secret, replay floor, and failure state. The
store owns `memo.draft.bin`: it stages a DPAPI-protected, verified candidate
before v3 primary publication and binds recovery to the exact authenticated
base document, generation, and envelope digest. A stale or corrupt draft blocks
normal publication until it is recovered or explicitly discarded after
authentication. The UI owns the restore/defer choice. An edit that has not
reached the UI's 100 ms debounce or the actor's 300 ms mailbox is outside this
durable-draft guarantee. Physical power-loss durability, spare-key management,
broader TOTP rollback assurance, and migration of real user data remain separate acceptance criteria in
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
- Password change passed an isolated real-worker test: incorrect old password
  left the primary unchanged; a successful change rejected the old password,
  preserved the original recovery key, repaired a replayed old primary from
  the new backup, and allowed a later save. Storage fault/retry tests cover
  precommit abort and postcommit recovery. A native UI test drove the settings
  chooser and masked change dialog, then reopened the exact title/body with
  the new password after rejecting the old one. Both tests use isolated data.
- The isolated native `pad_ui::whole_pad_password_change_requires_fresh_totp_before_rewrap`
  passed 1 test in 41.77 seconds. Closing the code dialog and submitting an
  invalid code each left the protected primary byte-identical; the invalid
  code also kept the content masked. A fresh code then changed the primary,
  invalidated the old password, and required the new password plus another
  fresh code to reopen the original title/body. Process cleanup found no
  surviving test runner.
- Interrupted password change recovery passed a storage test with the memo
  generation floor present: unauthenticated recovery left the pending state
  untouched; authenticated recovery restored the old backup; the next normal
  protected save succeeded without resurrecting an older memo generation.
- The native enrollment fixture passed with both recovery and final completion
  window messages deliberately suppressed. The timer consumed the waiting
  worker results and reached the same terminal state without duplicate cutover.
- Physical YubiKey 5 NFC, firmware 5.4.3: the PRF adapter and isolated whole-Pad worker seal/reopen tests passed after requesting Windows' legacy `hmac-secret` extension at registration. The native UI test `pad_ui::physical_yubikey5_whole_pad_ui_enroll_and_unlock` later passed through two Windows Security PIN/touch prompts, recovery-key confirmation, exact title/body reappearance, and isolated renderer exit; a process check found no survivor. Earlier attempts reached the same UI content but could not exit because the fixture's 15-second watch deadline fired mid-authentication. Another heartbeat-enabled attempt stayed at registration for 180 seconds despite both prompts being completed. Stage-only diagnostics now report future stalls; that intermittent stop is not yet explained. No device serial is recorded.
- With the enrollment timer in place, a fresh isolated `pad_ui::physical_yubikey5_whole_pad_ui_enroll_and_unlock` run passed again (1 test, 36.12 seconds). The complete workspace CI-feature regression also passed, and process checks found no survivor. The previous 180-second stop was not reproduced, so its exact cause remains unknown.
- `pad_ui::long_pad_fixture_keeps_watch_feed_alive` passed: the isolated renderer stayed live for 18 seconds of otherwise idle Pad time with the three-second heartbeat, then exited cleanly on the deliberate engine stop. This bounds the test-fixture cause of the prior teardown failures, not the separate hardware registration timeout.
- The isolated native whole-Pad TOTP setup/password/code gate test passed after a retry without concurrent desktop typing; it confirmed the protected title/body HWNDs remained absent between password entry and code acceptance.
- The isolated native host-suspend test passed: a simulated `WM_POWERBROADCAST/PBT_APMSUSPEND` masked title/body/list controls before reopening, and the Pad reopened locked. This is a message-path check, not a physical sleep/resume measurement.
- The Settings-to-renderer contract now has a data-free scalar reply for whole-Pad protection method and locked/unlocked state. `pad_ui::settings_protection_and_lock_messages_route_to_isolated_pad` passed: querying an isolated closed Pad returned its state without constructing a Pad HWND; the synchronous lock reply confirmed masking, the window hid, and a later status query again reported its persisted unprotected state. Settings' label mapping passed its unit test. An unreadable or incomplete persisted state is reported as unavailable; a failed or uncertain lock reply is not shown as completion.
- `pad_storage::tests` covered v3 drafts surviving interrupted publication, absent drafts, corrupt and wrong-scope drafts, advanced primaries, uncertain cleanup after primary publication, and credential rewrap blocking until an authenticated explicit discard. The complete renderer-library regression passed 249 tests with 15 ignored; a process check found no surviving test runner.
- `pad_ui::v3_draft_requires_password_before_choice_and_restores_after_forced_exit` passed against an isolated renderer and store. The test stopped that renderer after the encrypted draft was staged and before primary publication, then proved startup and wrong-password attempts displayed neither plaintext controls nor a recovery choice. After correct password, accepting the draft published the new generation, removed `memo.draft.bin`, and reopened the restored title/body after another restart. The focused run passed 1 test in 13.10 seconds and its process check was clean. A pre-debounce or pre-mailbox edit and real power loss were not tested.
- During WebAuthn calls the Pad parent releases its topmost z-order, restores it on completion, and rejects overlapping requests. A focused native-window test passed. The user confirmed seeing and completing both Windows Security PIN/touch prompts over the physical Pad UI test.
- The internal Version 8-M TOTP QR encoder passed dependency policy. Independent ZXing-cpp 2.3.0 decoding recovered the exact dummy Pad otpauth URI and a maximum-length 152-byte payload; no real TOTP secret left the test process.
- Password-entry labeling was improved so the field remains visibly identified while editing; this is a focused UX change, not completion of the planned visual/accessibility matrix.
- Storage cutover/fault tests: 31 pass; protected actor tests: four pass.
- The quiet runner's workflow self-test passed after the new CI step was
  written in the form it recognizes.
- Workspace Clippy with CI features and `-D warnings`, `cargo fmt --all
  -- --check`, dependency-policy self-test and full check, and
  `cargo audit --file Cargo.lock` passed. The IRV regression gate returned
  success with warnings for the intentionally broad storage and CI scope.

Native onboarding now shows the recovery key once, hides it, and asks the user
to enter its final six alphanumeric characters before the durable cutover.
The isolated test verifies that the displayed key unlocks after a fresh
session. This check cannot prove that a user saved the key outside Pad.
