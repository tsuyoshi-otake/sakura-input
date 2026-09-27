//! Orchestration for a Pad memo's external WebAuthn PRF credential.
//!
//! These operations are synchronous and must run on a background
//! thread. The UI owns consent, the HWND, and cancellation. This module does
//! not persist credentials or trust an unauthenticated envelope header.

use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, GetWindowThreadProcessId, IsWindow, SetWindowPos, GWL_EXSTYLE,
    HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_TOPMOST,
};
use zeroize::Zeroizing;

use crate::memo_protection::{hardware_hint, memo_prf_salt, pad_prf_salt};
use crate::pad_webauthn::{self, Cancellation, WebAuthnError};

const MAX_OPERATION_TIMEOUT_MS: u128 = 120_000;
const MIN_REGISTRATION_TIMEOUT_MS: u128 = 2;
static AUTH_DIALOG_LEASE: Mutex<()> = Mutex::new(());

/// Windows Security must remain reachable while the Pad itself is topmost.
/// Keep the normal Pad z-order outside the bounded native authentication call.
struct AuthDialogZOrder {
    _lease: MutexGuard<'static, ()>,
    parent: HWND,
    thread_id: u32,
    process_id: u32,
    restore_topmost: bool,
}

impl AuthDialogZOrder {
    fn enter(parent: HWND) -> Result<Self, WebAuthnError> {
        // A second request must not restore the Pad above the first request's
        // native dialog. Reject overlap instead of queueing behind a PIN wait.
        let lease = match AUTH_DIALOG_LEASE.try_lock() {
            Ok(lease) => lease,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(WebAuthnError::Busy),
        };
        // SAFETY: IsWindow only queries the supplied owner HWND.
        if !unsafe { IsWindow(Some(parent)).as_bool() } {
            return Err(WebAuthnError::InvalidInput);
        }
        let mut process_id = 0;
        // SAFETY: the live parent is the HWND supplied to Windows WebAuthn.
        let thread_id = unsafe { GetWindowThreadProcessId(parent, Some(&mut process_id)) };
        if thread_id == 0 {
            return Err(WebAuthnError::InvalidInput);
        }
        // SAFETY: the live window's extended style is read without mutation.
        let style = unsafe { GetWindowLongPtrW(parent, GWL_EXSTYLE) } as u32;
        let restore_topmost = style & WS_EX_TOPMOST.0 != 0;
        if restore_topmost {
            // SAFETY: release only the authenticating parent. The native OS
            // dialog remains owned by that HWND and can appear above it.
            unsafe {
                SetWindowPos(
                    parent,
                    Some(HWND_NOTOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
            }
            .map_err(|error| WebAuthnError::Windows(error.code()))?;
        }
        Ok(Self {
            _lease: lease,
            parent,
            thread_id,
            process_id,
            restore_topmost,
        })
    }
}

impl Drop for AuthDialogZOrder {
    fn drop(&mut self) {
        // SAFETY: IsWindow only queries the previously captured owner HWND.
        if !self.restore_topmost || !unsafe { IsWindow(Some(self.parent)).as_bool() } {
            return;
        }
        let mut process_id = 0;
        // SAFETY: IsWindow just confirmed this HWND is live; identity is
        // checked before affecting its z-order after a potentially long call.
        let thread_id = unsafe { GetWindowThreadProcessId(self.parent, Some(&mut process_id)) };
        if thread_id != self.thread_id || process_id != self.process_id {
            return;
        }
        // SAFETY: restore the Pad's previous topmost state after native UI
        // completion, cancellation, error, or timeout.
        unsafe {
            let _ = SetWindowPos(
                self.parent,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

/// A hint-derived credential and fresh PRF result. The credential ID and
/// password flag are untrusted until the memo worker authenticates the whole
/// envelope. `prf` is zeroized when this value is dropped.
#[allow(missing_debug_implementations)]
pub struct DerivedHardware {
    pub credential_id: Vec<u8>,
    pub requires_password: bool,
    pub prf: Zeroizing<[u8; 32]>,
}

/// Registers one external authenticator credential, then proves PRF support
/// with a fresh user-verified assertion for this memo's stable salt.
///
/// Registration is a persistent authenticator side effect. Call only after
/// explicit user consent, on a background thread. A failed assertion leaves
/// the caller with no derived key and must not publish a protected envelope.
pub fn register_and_derive(
    parent: HWND,
    document_id: [u8; 16],
    memo_id: u64,
    timeout: Duration,
    cancellation: &Cancellation,
) -> Result<(Vec<u8>, Zeroizing<[u8; 32]>), WebAuthnError> {
    let salt = scope_salt(document_id, memo_id)?;
    register_for_salt(parent, salt, timeout, cancellation)
}

/// Register and derive for the whole Pad's newly generated vault ID. A zero
/// memo ID has its own stable PRF scope and cannot alias a valid memo.
pub fn register_pad_and_derive(
    parent: HWND,
    vault_id: [u8; 16],
    timeout: Duration,
    cancellation: &Cancellation,
) -> Result<(Vec<u8>, Zeroizing<[u8; 32]>), WebAuthnError> {
    let salt = pad_prf_salt(vault_id)
        .map(Zeroizing::new)
        .ok_or(WebAuthnError::InvalidInput)?;
    register_for_salt(parent, salt, timeout, cancellation)
}

fn register_for_salt(
    parent: HWND,
    salt: Zeroizing<[u8; 32]>,
    timeout: Duration,
    cancellation: &Cancellation,
) -> Result<(Vec<u8>, Zeroizing<[u8; 32]>), WebAuthnError> {
    validate_timeout(timeout, MIN_REGISTRATION_TIMEOUT_MS)?;
    ensure_active(cancellation)?;
    let _auth_dialog_z_order = AuthDialogZOrder::enter(parent)?;
    let started = Instant::now();
    let user_id = random_32()?;
    let register_challenge = random_32()?;
    // Reserve half the caller's total budget for the second touch. Any
    // unused registration time remains available to the assertion.
    crate::pad_debug("hardware:register:start");
    let credential = pad_webauthn::register(
        parent,
        &user_id,
        &register_challenge,
        timeout / 2,
        cancellation,
    );
    crate::pad_debug(if credential.is_ok() {
        "hardware:register:ok"
    } else {
        "hardware:register:error"
    });
    let credential = credential?;
    ensure_active(cancellation)?;
    let assertion_challenge = random_32()?;
    let remaining = remaining_timeout(timeout, started)?;
    crate::pad_debug("hardware:assert:start");
    let prf = pad_webauthn::assert_prf(
        parent,
        &credential.id,
        &salt,
        &assertion_challenge,
        remaining,
        cancellation,
    );
    crate::pad_debug(if prf.is_ok() {
        "hardware:assert:ok"
    } else {
        "hardware:assert:error"
    });
    let prf = prf?;
    ensure_active(cancellation)?;
    Ok((credential.id, prf))
}

/// Assert the saved whole-Pad credential selected by a bounded storage hint.
/// The worker must still authenticate the complete v3 envelope and vault ID.
pub fn derive_pad_for_hint(
    parent: HWND,
    vault_id: [u8; 16],
    credential_id: &[u8],
    timeout: Duration,
    cancellation: &Cancellation,
) -> Result<Zeroizing<[u8; 32]>, WebAuthnError> {
    validate_timeout(timeout, 1)?;
    if credential_id.is_empty() || credential_id.len() > 1024 {
        return Err(WebAuthnError::InvalidInput);
    }
    let salt = pad_prf_salt(vault_id)
        .map(Zeroizing::new)
        .ok_or(WebAuthnError::InvalidInput)?;
    ensure_active(cancellation)?;
    let _auth_dialog_z_order = AuthDialogZOrder::enter(parent)?;
    let challenge = random_32()?;
    let prf = pad_webauthn::assert_prf(
        parent,
        credential_id,
        &salt,
        &challenge,
        timeout,
        cancellation,
    )?;
    ensure_active(cancellation)?;
    Ok(prf)
}

/// Uses a bounded, unauthenticated v3 header only to select the credential.
/// The caller must pass the returned PRF and original envelope to the memo
/// worker for complete authentication before exposing plaintext.
pub fn derive_for_envelope(
    parent: HWND,
    document_id: [u8; 16],
    memo_id: u64,
    envelope: &[u8],
    timeout: Duration,
    cancellation: &Cancellation,
) -> Result<DerivedHardware, WebAuthnError> {
    validate_timeout(timeout, 1)?;
    let salt = scope_salt(document_id, memo_id)?;
    let (credential_id, requires_password) = bounded_hint(envelope)?;
    ensure_active(cancellation)?;
    let _auth_dialog_z_order = AuthDialogZOrder::enter(parent)?;
    let challenge = random_32()?;
    let prf = pad_webauthn::assert_prf(
        parent,
        &credential_id,
        &salt,
        &challenge,
        timeout,
        cancellation,
    )?;
    ensure_active(cancellation)?;
    Ok(DerivedHardware {
        credential_id,
        requires_password,
        prf,
    })
}

fn bounded_hint(envelope: &[u8]) -> Result<(Vec<u8>, bool), WebAuthnError> {
    let hint = hardware_hint(envelope).ok_or(WebAuthnError::InvalidInput)?;
    // hardware_hint bounds this slice to 1..=1024 before we copy it. This
    // copy is solely for the synchronous assertion; it is not trusted state.
    Ok((hint.credential_id.to_vec(), hint.requires_password))
}

fn ensure_active(cancellation: &Cancellation) -> Result<(), WebAuthnError> {
    if cancellation.is_cancelled() {
        Err(WebAuthnError::CancelledOrTimedOut)
    } else {
        Ok(())
    }
}

fn scope_salt(document_id: [u8; 16], memo_id: u64) -> Result<Zeroizing<[u8; 32]>, WebAuthnError> {
    memo_prf_salt(document_id, memo_id)
        .map(Zeroizing::new)
        .ok_or(WebAuthnError::InvalidInput)
}

fn validate_timeout(timeout: Duration, minimum_ms: u128) -> Result<(), WebAuthnError> {
    if (minimum_ms..=MAX_OPERATION_TIMEOUT_MS).contains(&timeout.as_millis()) {
        Ok(())
    } else {
        Err(WebAuthnError::InvalidInput)
    }
}

fn remaining_timeout(timeout: Duration, started: Instant) -> Result<Duration, WebAuthnError> {
    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or(WebAuthnError::CancelledOrTimedOut)?;
    if remaining.as_millis() == 0 {
        Err(WebAuthnError::CancelledOrTimedOut)
    } else {
        Ok(remaining)
    }
}

fn random_32() -> Result<Zeroizing<[u8; 32]>, WebAuthnError> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    // SAFETY: BCryptGenRandom writes only to this live, fixed-size buffer.
    let status = unsafe { BCryptGenRandom(None, &mut *bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.0 < 0 {
        Err(WebAuthnError::EntropyUnavailable)
    } else {
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetDesktopWindow, WS_OVERLAPPEDWINDOW,
    };

    fn is_topmost(window: HWND) -> bool {
        // SAFETY: the test owns this live HWND during each style inspection.
        (unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) }) as u32 & WS_EX_TOPMOST.0 != 0
    }

    #[test]
    fn os_auth_dialog_temporarily_releases_and_restores_topmost_parent() {
        // SAFETY: STATIC is a registered Win32 class, and the test owns this
        // hidden top-level HWND until DestroyWindow below.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST,
                windows::core::w!("STATIC"),
                windows::core::w!("Sakura Pad auth z-order test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                200,
                120,
                None,
                None,
                None,
                None,
            )
        }
        .expect("create isolated topmost owner");
        assert!(is_topmost(window));
        {
            let _auth_dialog = AuthDialogZOrder::enter(window).unwrap();
            assert!(!is_topmost(window));
            assert!(matches!(
                AuthDialogZOrder::enter(window),
                Err(WebAuthnError::Busy)
            ));
        }
        assert!(is_topmost(window));
        // SAFETY: the HWND is this test's live window and is no longer used.
        unsafe { DestroyWindow(window) }.expect("destroy isolated topmost owner");
    }

    #[test]
    fn scope_and_timeout_reject_invalid_work_before_registration() {
        assert_eq!(
            scope_salt([0; 16], 1).map(|_| ()),
            Err(WebAuthnError::InvalidInput)
        );
        assert_eq!(
            scope_salt([1; 16], 0).map(|_| ()),
            Err(WebAuthnError::InvalidInput)
        );
        assert!(scope_salt([1; 16], 1).is_ok());
        assert_eq!(
            validate_timeout(Duration::from_millis(1), 2),
            Err(WebAuthnError::InvalidInput)
        );
        assert_eq!(validate_timeout(Duration::from_millis(2), 2), Ok(()));
        assert_eq!(
            validate_timeout(Duration::from_secs(121), 1),
            Err(WebAuthnError::InvalidInput)
        );
    }

    #[test]
    fn untrusted_hint_is_bounded_before_credential_copy() {
        assert_eq!(
            bounded_hint(b"not-an-envelope"),
            Err(WebAuthnError::InvalidInput)
        );
        let mut envelope = vec![0_u8; 38 + 1025];
        envelope[..8].copy_from_slice(b"SKRPENV3");
        envelope[35] = 1;
        envelope[36..38].copy_from_slice(&1025_u16.to_le_bytes());
        assert_eq!(bounded_hint(&envelope), Err(WebAuthnError::InvalidInput));
        envelope[36..38].copy_from_slice(&1_u16.to_le_bytes());
        envelope[38] = 42;
        assert_eq!(bounded_hint(&envelope), Ok((vec![42], false)));
        envelope[35] = 2;
        assert_eq!(bounded_hint(&envelope), Ok((vec![42], true)));
    }

    /// Opt-in device gate: Windows displays PIN/touch UI for a connected
    /// YubiKey 5. The non-resident credential ID is discarded after the test.
    #[test]
    #[ignore = "requires a connected YubiKey 5 and an interactive Windows desktop"]
    fn physical_yubikey5_pad_prf_enroll_and_reopen() {
        let random = random_32().expect("fresh vault ID");
        let mut vault_id = [0_u8; 16];
        vault_id.copy_from_slice(&random[..16]);
        let cancellation = Cancellation::new().expect("Windows WebAuthn PRF support");
        // SAFETY: the desktop window is a live HWND for Windows Security UI.
        let parent = unsafe { GetDesktopWindow() };
        let user_id = random_32().expect("fresh authenticator user ID");
        let challenge = random_32().expect("fresh registration challenge");
        let credential = pad_webauthn::register(
            parent,
            &user_id,
            &challenge,
            Duration::from_secs(120),
            &cancellation,
        )
        .expect("YubiKey 5 credential registration with PRF or hmac-secret");
        let enrolled_prf = derive_pad_for_hint(
            parent,
            vault_id,
            &credential.id,
            Duration::from_secs(120),
            &cancellation,
        )
        .expect("first YubiKey 5 scoped PRF assertion");
        let reopened_prf = derive_pad_for_hint(
            parent,
            vault_id,
            &credential.id,
            Duration::from_secs(120),
            &cancellation,
        )
        .expect("YubiKey 5 PRF assertion after reopen");
        assert_eq!(*enrolled_prf, *reopened_prf);
    }
}
