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
diagnostics remain for recurrence; the intermittent stop is unexplained.
Spare-key management remains
open. TOTP is local confirmation backed by a current-user DPAPI
sidecar; it is not independent cryptographic protection or online 2FA.

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
| Suspend revokes the surface | Run `pad_ui::host_suspend_masks_unlocked_whole_pad_before_reopen` against an isolated renderer and session worker | A simulated host `PBT_APMSUSPEND` removes protected title/body/list HWNDs before the Pad can reopen, which requires authentication |
| Repository checks and process lifetime | Wrapped Cargo tests, fmt, clippy, dependency/audit gates, and `ci/check-process-clean.ps1` | Required suites pass, no runner survives, no package is added younger than seven days |

The storage transaction owns the durable protected cutover and version guard.
The worker owns password KDF, authenticated encryption, and the unlocked
session. The renderer owns UI state, save epochs, confirmation before cutover,
and the native locked surface. The WebAuthn adapter requests user verification
and derives scope-bound PRF material. The physical test first reproduced
`PrfUnavailable` with a PRF-only registration request; requesting Windows'
legacy `hmac-secret` extension during credential registration enabled the
tested YubiKey 5 NFC path. Microsoft documents that its `webauthn.h` maps PRF
values to the HMAC-secret extension ([header](https://github.com/microsoft/webauthn/blob/master/webauthn.h));
Yubico documents the firmware capability matrix in its [technical manual](https://docs.yubico.com/hardware/yubikey/yk-tech-manual/yk5-firmware-overview.html).
The TOTP sidecar owns its local secret,
replay floor, and failure state. UI Automation client traversal,
forced-process-exit recovery of an uncertain unsaved edit, full Pad UI key
flow, spare-key management, broader TOTP rollback assurance, and migration of real user
data remain separate acceptance criteria in
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
- Physical YubiKey 5 NFC, firmware 5.4.3: the PRF adapter and isolated whole-Pad worker seal/reopen tests passed after requesting Windows' legacy `hmac-secret` extension at registration. The native UI test `pad_ui::physical_yubikey5_whole_pad_ui_enroll_and_unlock` later passed through two Windows Security PIN/touch prompts, recovery-key confirmation, exact title/body reappearance, and isolated renderer exit; a process check found no survivor. Earlier attempts reached the same UI content but could not exit because the fixture's 15-second watch deadline fired mid-authentication. Another heartbeat-enabled attempt stayed at registration for 180 seconds despite both prompts being completed. Stage-only diagnostics now report future stalls; that intermittent stop is not yet explained. No device serial is recorded.
- `pad_ui::long_pad_fixture_keeps_watch_feed_alive` passed: the isolated renderer stayed live for 18 seconds of otherwise idle Pad time with the three-second heartbeat, then exited cleanly on the deliberate engine stop. This bounds the test-fixture cause of the prior teardown failures, not the separate hardware registration timeout.
- The isolated native whole-Pad TOTP setup/password/code gate test passed after a retry without concurrent desktop typing; it confirmed the protected title/body HWNDs remained absent between password entry and code acceptance.
- The isolated native host-suspend test passed: a simulated `WM_POWERBROADCAST/PBT_APMSUSPEND` masked title/body/list controls before reopening, and the Pad reopened locked. This is a message-path check, not a physical sleep/resume measurement.
- Settings' data-free registered protection and lock messages reached an isolated renderer. The former opened the Pad protection prompt; the latter hid it. The sender only reports successful queueing, not a completed lock.
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
