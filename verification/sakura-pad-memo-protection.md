# Sakura Pad: memo-level protection verification

Status: stage3 implementation in progress on Issue #269; PR #270 remains draft.
This page records observable checks for memo-level password, recovery-key, and
WebAuthn PRF paths; it is not a release claim. All test
data is isolated from the user's Pad directory. No live memo is migrated.

| Criterion | Verify | Expect | Status |
| --- | --- | --- | --- |
| Opaque storage | Run `pad_storage::tests` v4 framing cases and inspect a v4 encoded protected memo. | Its serialized record contains no plaintext title/body, and malformed or oversized payloads fail. | Passed in scoped storage and renderer-library tests. |
| Migration and replay | Run `pad_storage::tests` v4 and v3 migration, interruption, second/third memo protection, and floor recovery cases. | Legacy and pre-protection recovery files cannot be selected after a committed protection transition. An interrupted transition is either old state before intent or recoverable new state after intent. | Passed in scoped storage and renderer-library tests. |
| Independent scope | Run ignored `memo_protection::tests::real_worker_memo_scope_round_trip_and_rejection` with the exact built worker. | Wrong password, another memo ID, and another document ID return no plaintext or reusable session. | Passed with real worker; document-ID mismatch also covered by worker envelope tests. |
| Visible lock | Run ignored `pad_ui::one_locked_memo_hides_its_title_from_native_controls_and_search` with an isolated v4 store and real worker; query child HWND text, native list/search/copy, unlock and relock. | The locked title/body never appears in child text or list labels, cannot be found or copied, and relocking removes it again. | Passed in native isolated-process test. Locked rows use content-independent `保護されたメモ 01` labels in both drawing and native LISTBOX text; search shows its locked-content limitation. Forced-close fault cases remain. |
| Save finality | Run v4 actor tests; inject a reseal or disk failure during edit/switch/close. | UI does not report a save or discard an unsaved draft; every operation reaches a visible terminal state. | Actor tests passed. Native fault injection remains. |
| Whole-Pad coexistence | Run v3 composite migration/replay tests, then ignored `pad_protection::tests::real_worker_whole_pad_and_memo_require_independent_passwords`. | Outer v3 remains required, and no v3 backup can restore a pre-memo-protection title/body. | Storage and real-worker tests passed; native v3 screen path remains. |
| Recovery key | Run ignored `memo_protection` v2 recovery test and native confirmation, cancel, password unlock, recovery unlock after Pad reopen; inspect the captured native prompt. | A key saved outside Pad opens only its own memo. Cancel before confirmation leaves no committed protection, and the unwrapped key is never saved to Pad files. The full key is visible and selectable without the obsolete confirmation controls. | Worker and three native tests passed; native screenshot and control-style checks passed. Timeout remains. |
| WebAuthn PRF | Run adapter contract tests and deterministic worker round-trip/rejection cases; separately enroll and unlock with a physical YubiKey 5. | PRF is bound to the document and memo scope; wrong credential, absent user verification, cancel, or timeout leaves the memo locked. | Passed on YubiKey 5 NFC firmware 5.4.3: `pad_hardware::tests::physical_yubikey5_pad_prf_enroll_and_reopen`, including repeated enrollment with the same secret. PRF-only registration first returned `PrfUnavailable`; requesting Windows' legacy `hmac-secret` extension enabled the physical path. The isolated whole-Pad real-worker test also passed. A physical memo-and-worker attempt timed out; an explicit retry passed `memo_protection::tests::physical_yubikey5_seals_and_reopens_isolated_memo` (1 test, 39.39 seconds), including registration, scoped reassertion, wrong-memo rejection, and recovery. The retry's stage-only log showed register and first assertion complete; no serial was recorded. A physical memo UI path remains unverified. |
| TOTP boundary | Run the `pad_totp`/sidecar tests and ignored `pad_ui::memo_totp_setup_gates_password_unlock_until_code` with an isolated worker. | UI calls it local additional confirmation; an enabled memo's plaintext stays out of native controls until code verification. | Passed: setup, code confirmation, password unlock, code gate, and plaintext after accepted code. Same-user sidecar deletion or rollback can bypass this local confirmation, so it is not independent file encryption or online 2FA. |

The v4 store checks DPAPI and document structure without opening every locked
memo. Only the memo's independent worker session can authenticate its inner
AEAD envelope. New or edited envelopes must be authenticated by that session
before the UI submits a document snapshot; authentication of an unrelated
locked memo is deferred until that memo is explicitly unlocked.

The new document ID owns the memo envelope's scope and is immutable. The
storage owner publishes generation floors and recovery copies. The Pad UI owns
which single memo is visible, its in-memory draft, and whether a worker result
still belongs to the current UI epoch. The save actor owns confirmed disk
generations and returns an unsaved snapshot on failure.

The display number is computed from live protected memo IDs. It stays the
same across sort, search, and unlock transitions, but adding or removing a
protected memo can renumber later rows. The number is display-only and never
used as the authenticated memo identity.

Physical-key testing establishes the exercised WebAuthn API, isolated storage,
one complete whole-Pad UI path, and a memo-scoped real-worker path. A physical
memo UI path remains unverified.
Microsoft's [`webauthn.h`](https://github.com/microsoft/webauthn/blob/master/webauthn.h)
documents the PRF-to-HMAC-secret mapping; see the [Yubico firmware capability matrix](https://docs.yubico.com/hardware/yubikey/yk-tech-manual/yk5-firmware-overview.html)
for device firmware support. TOTP confirmation is local to this Windows user
and shares the local trust boundary. The password input now has a visible label
while editing. Visual/DPI/accessibility matrix, spare-key
management, real user Pad migration, and release verification remain open; the
PR remains draft.
