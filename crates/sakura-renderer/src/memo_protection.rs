//! Independent, password-owned crypto session for one Pad memo.
//!
//! The worker authenticates both the stable nonzero document vault ID and
//! nonzero memo ID in its v1 envelope scope. This module never sends memo ID
//! zero and never uses the whole-Pad session key. The caller owns the
//! `MemoPayloadV1` framing contract: before create/reseal, encode a versioned,
//! length-delimited `(title, body)` frame within Pad's title/body/document
//! limits; after unlock/verify, reject malformed or oversized frames before
//! displaying text. Raw bytes here are only an authenticated crypto boundary.
//!
//! Every method blocks and must run off the UI thread. Owned password and
//! payload buffers are zeroized on drop; OS/pipe/allocator copies are outside
//! that guarantee. The worker child is canceled and reaped on lock, failure,
//! timeout, or Drop.

#[cfg(test)]
use std::path::PathBuf;
use std::time::Duration;

use sakura_pad_session_proto::{
    encode_v3_auth_payload, parse_created_recovery, Operation, Request, SecretBytes, Status,
};
use zeroize::Zeroizing;

use crate::pad_crypto_client::{ClientError, PadCryptoCancellation, PadCryptoClient};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoError {
    InvalidScope,
    Active,
    Locked,
    Authentication,
    Unavailable,
    Stale,
    Protocol,
    Worker,
}

/// Unauthenticated format hint for choosing one worker operation or explaining
/// why an old memo has no recovery key. The worker still authenticates it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoEnvelopeFormat {
    PasswordOnlyV1,
    PasswordRecoveryV2,
    PrfV3,
    PasswordAndPrfV3,
    Unknown,
}

pub fn classify_envelope(envelope: &[u8]) -> MemoEnvelopeFormat {
    if envelope.starts_with(b"SKRPENV1") {
        MemoEnvelopeFormat::PasswordOnlyV1
    } else if envelope.starts_with(b"SKRPENV2") {
        MemoEnvelopeFormat::PasswordRecoveryV2
    } else if envelope.len() > 35 && envelope.starts_with(b"SKRPENV3") {
        match envelope[35] {
            1 => MemoEnvelopeFormat::PrfV3,
            2 => MemoEnvelopeFormat::PasswordAndPrfV3,
            _ => MemoEnvelopeFormat::Unknown,
        }
    } else {
        MemoEnvelopeFormat::Unknown
    }
}

/// Unauthenticated v3 header fields needed to choose a WebAuthn credential.
/// The worker verifies this ID and the complete envelope after the assertion.
#[allow(missing_debug_implementations)]
pub struct MemoHardwareHint<'a> {
    pub credential_id: &'a [u8],
    pub requires_password: bool,
}

pub fn hardware_hint(envelope: &[u8]) -> Option<MemoHardwareHint<'_>> {
    let requires_password = match classify_envelope(envelope) {
        MemoEnvelopeFormat::PrfV3 => false,
        MemoEnvelopeFormat::PasswordAndPrfV3 => true,
        _ => return None,
    };
    let len = u16::from_le_bytes(envelope.get(36..38)?.try_into().ok()?) as usize;
    if !(1..=1024).contains(&len) {
        return None;
    }
    let credential_id = envelope.get(38..38 + len)?;
    Some(MemoHardwareHint {
        credential_id,
        requires_password,
    })
}

/// Stable per-document and per-memo WebAuthn PRF input. The credential itself
/// is unique, while this salt separates a key's output between Pad scopes.
/// A changed scope cannot unlock the old envelope; recovery handles moves.
pub fn memo_prf_salt(document_id: [u8; 16], memo_id: u64) -> Option<[u8; 32]> {
    if memo_id == 0 {
        return None;
    }
    scope_prf_salt(document_id, memo_id)
}

/// Whole-Pad PRF input. The zero memo ID distinguishes the vault from every
/// valid memo scope while retaining the same stable salt construction.
pub fn pad_prf_salt(vault_id: [u8; 16]) -> Option<[u8; 32]> {
    scope_prf_salt(vault_id, 0)
}

fn scope_prf_salt(document_id: [u8; 16], memo_id: u64) -> Option<[u8; 32]> {
    if document_id == [0; 16] {
        return None;
    }
    let mut salt = [0_u8; 32];
    salt[..16].copy_from_slice(&document_id);
    salt[16..24].copy_from_slice(&memo_id.to_le_bytes());
    salt[24..].copy_from_slice(b"SPadPRF1");
    Some(salt)
}

/// One memo's isolated v1 worker session. No password or key is retained in
/// the renderer after a request; the child owns the live key until lock.
#[allow(missing_debug_implementations)]
pub struct MemoProtectionSession {
    document_vault_id: [u8; 16],
    memo_id: u64,
    client: Option<PadCryptoClient>,
    unlocked: bool,
    epoch: u64,
    next_request_id: u64,
    timeout: Duration,
    #[cfg(test)]
    worker_image: Option<PathBuf>,
}

impl MemoProtectionSession {
    pub fn new(
        document_vault_id: [u8; 16],
        memo_id: u64,
        timeout: Duration,
    ) -> Result<Self, MemoError> {
        if document_vault_id == [0; 16] || memo_id == 0 || timeout.is_zero() {
            return Err(MemoError::InvalidScope);
        }
        Ok(Self {
            document_vault_id,
            memo_id,
            client: None,
            unlocked: false,
            epoch: 0,
            next_request_id: 1,
            timeout,
            #[cfg(test)]
            worker_image: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_worker_image(mut self, image: PathBuf) -> Self {
        self.worker_image = Some(image);
        self
    }

    /// Create a v1 fixture to prove old password-only memos remain readable.
    /// Caller must not publish returned ciphertext before verifying its own
    /// versioned MemoPayloadV1 frame and storage transaction.
    #[cfg(test)]
    pub fn create(
        &mut self,
        password: SecretBytes,
        plaintext: SecretBytes,
    ) -> Result<SecretBytes, MemoError> {
        if self.unlocked {
            return Err(MemoError::Active);
        }
        self.fresh_worker()?;
        self.start_request(Operation::Create, password, plaintext)
    }

    /// Create a recoverable v2 memo envelope and retain its worker session.
    /// The returned key is the canonical one-time display value. The caller
    /// shows it once and asks the user to save it outside Pad before publishing
    /// the envelope; Pad must not persist the unwrapped key.
    pub fn create_with_recovery(
        &mut self,
        password: SecretBytes,
        plaintext: SecretBytes,
    ) -> Result<(SecretBytes, Zeroizing<String>), MemoError> {
        if self.unlocked {
            return Err(MemoError::Active);
        }
        self.fresh_worker()?;
        let result = self.exchange(
            Operation::CreateRecoverable,
            self.document_vault_id,
            self.memo_id,
            password,
            plaintext,
        );
        match result.and_then(|payload| {
            let created = parse_created_recovery(&payload).map_err(|_| MemoError::Protocol)?;
            Ok((
                SecretBytes::new(created.envelope.to_vec()),
                Zeroizing::new(created.recovery_key.to_owned()),
            ))
        }) {
            Ok(created) => {
                self.unlocked = true;
                Ok(created)
            }
            Err(error) => {
                self.lock();
                Err(error)
            }
        }
    }

    /// Create a v3 memo whose ordinary unlock requires this credential's PRF.
    /// `prf` is obtained from a fresh, user-verified WebAuthn assertion. The
    /// caller must persist the returned envelope only after the one-time
    /// recovery key is shown and confirmed; the PRF is never persisted.
    pub fn create_with_prf(
        &mut self,
        credential_id: &[u8],
        prf: Zeroizing<[u8; 32]>,
        plaintext: SecretBytes,
    ) -> Result<(SecretBytes, Zeroizing<String>), MemoError> {
        self.create_v3(
            Operation::CreatePrfV3,
            SecretBytes::new(Vec::new()),
            credential_id,
            prf,
            plaintext,
        )
    }

    /// Create a v3 memo requiring both its password and this credential's PRF.
    pub fn create_with_password_and_prf(
        &mut self,
        password: SecretBytes,
        credential_id: &[u8],
        prf: Zeroizing<[u8; 32]>,
        plaintext: SecretBytes,
    ) -> Result<(SecretBytes, Zeroizing<String>), MemoError> {
        self.create_v3(
            Operation::CreatePasswordAndPrfV3,
            password,
            credential_id,
            prf,
            plaintext,
        )
    }

    fn create_v3(
        &mut self,
        operation: Operation,
        password: SecretBytes,
        credential_id: &[u8],
        prf: Zeroizing<[u8; 32]>,
        plaintext: SecretBytes,
    ) -> Result<(SecretBytes, Zeroizing<String>), MemoError> {
        if self.unlocked {
            return Err(MemoError::Active);
        }
        let payload = encode_v3_auth_payload(credential_id, &prf, &plaintext)
            .map_err(|_| MemoError::Protocol)?;
        self.fresh_worker()?;
        let result = self.exchange(
            operation,
            self.document_vault_id,
            self.memo_id,
            password,
            payload,
        );
        match result.and_then(|payload| {
            let created = parse_created_recovery(&payload).map_err(|_| MemoError::Protocol)?;
            Ok((
                SecretBytes::new(created.envelope.to_vec()),
                Zeroizing::new(created.recovery_key.to_owned()),
            ))
        }) {
            Ok(created) => {
                self.unlocked = true;
                Ok(created)
            }
            Err(error) => {
                self.lock();
                Err(error)
            }
        }
    }

    /// Authenticate a stored v1 or recoverable v2 envelope with its password
    /// using this memo's exact scope. The format marker selects one worker
    /// operation; the worker authenticates it, and unknown markers are never
    /// tried against both.
    /// Rejected password/scope/ciphertext reveals no plaintext or reusable
    /// session; a later retry starts a fresh child with a larger epoch.
    pub fn unlock(
        &mut self,
        password: SecretBytes,
        envelope: SecretBytes,
    ) -> Result<SecretBytes, MemoError> {
        if self.unlocked {
            return Err(MemoError::Active);
        }
        let operation = match classify_envelope(&envelope) {
            MemoEnvelopeFormat::PasswordOnlyV1 => Operation::Unlock,
            MemoEnvelopeFormat::PasswordRecoveryV2 => Operation::UnlockPasswordV2,
            MemoEnvelopeFormat::PrfV3 | MemoEnvelopeFormat::PasswordAndPrfV3 => {
                return Err(MemoError::Authentication)
            }
            MemoEnvelopeFormat::Unknown => return Err(MemoError::Authentication),
        };
        self.fresh_worker()?;
        self.start_request(operation, password, envelope)
    }

    /// Authenticate a stored v2 envelope with its canonical recovery key.
    /// The key is carried as a zeroizing request buffer and is never retained.
    pub fn unlock_with_recovery(
        &mut self,
        canonical_key: SecretBytes,
        envelope: SecretBytes,
    ) -> Result<SecretBytes, MemoError> {
        if self.unlocked {
            return Err(MemoError::Active);
        }
        let operation = match classify_envelope(&envelope) {
            MemoEnvelopeFormat::PasswordRecoveryV2 => Operation::UnlockRecoveryV2,
            MemoEnvelopeFormat::PrfV3 | MemoEnvelopeFormat::PasswordAndPrfV3 => {
                Operation::UnlockRecoveryV3
            }
            _ => return Err(MemoError::Authentication),
        };
        self.fresh_worker()?;
        self.start_request(operation, canonical_key, envelope)
    }

    /// Unlock a PRF-only v3 memo. The caller must obtain a fresh WebAuthn
    /// assertion using the credential ID hinted by this envelope and then let
    /// the worker authenticate that same ID and the full ciphertext.
    pub fn unlock_with_prf(
        &mut self,
        credential_id: &[u8],
        prf: Zeroizing<[u8; 32]>,
        envelope: SecretBytes,
    ) -> Result<SecretBytes, MemoError> {
        self.unlock_v3(
            Operation::UnlockPrfV3,
            SecretBytes::new(Vec::new()),
            credential_id,
            prf,
            envelope,
        )
    }

    /// Unlock a v3 memo only when both its password and WebAuthn PRF match.
    pub fn unlock_with_password_and_prf(
        &mut self,
        password: SecretBytes,
        credential_id: &[u8],
        prf: Zeroizing<[u8; 32]>,
        envelope: SecretBytes,
    ) -> Result<SecretBytes, MemoError> {
        self.unlock_v3(
            Operation::UnlockPasswordAndPrfV3,
            password,
            credential_id,
            prf,
            envelope,
        )
    }

    fn unlock_v3(
        &mut self,
        operation: Operation,
        password: SecretBytes,
        credential_id: &[u8],
        prf: Zeroizing<[u8; 32]>,
        envelope: SecretBytes,
    ) -> Result<SecretBytes, MemoError> {
        if self.unlocked {
            return Err(MemoError::Active);
        }
        if !matches!(
            (operation, classify_envelope(&envelope)),
            (Operation::UnlockPrfV3, MemoEnvelopeFormat::PrfV3)
                | (
                    Operation::UnlockPasswordAndPrfV3,
                    MemoEnvelopeFormat::PasswordAndPrfV3
                )
        ) {
            return Err(MemoError::Authentication);
        }
        let payload = encode_v3_auth_payload(credential_id, &prf, &envelope)
            .map_err(|_| MemoError::Protocol)?;
        self.fresh_worker()?;
        self.start_request(operation, password, payload)
    }

    /// Produce new ciphertext under the already authenticated memo key.
    /// A failure closes this session; the caller retains its unsaved text.
    pub fn reseal(&mut self, plaintext: SecretBytes) -> Result<SecretBytes, MemoError> {
        self.in_session_request(Operation::Reseal, plaintext)
    }

    /// Open a candidate envelope under this active session, useful for
    /// authenticating a staged storage copy before publication.
    pub fn verify(&mut self, envelope: SecretBytes) -> Result<SecretBytes, MemoError> {
        self.in_session_request(Operation::Verify, envelope)
    }

    /// Cancel and reap precisely this child. This never reports a save.
    pub fn lock(&mut self) {
        if let Some(mut client) = self.client.take() {
            client.cancel();
        }
        self.unlocked = false;
    }

    /// May be cloned by an operation owner to abort a blocked exchange.
    pub fn cancellation_handle(&self) -> Option<PadCryptoCancellation> {
        self.client
            .as_ref()
            .map(PadCryptoClient::cancellation_handle)
    }

    fn fresh_worker(&mut self) -> Result<(), MemoError> {
        self.lock();
        self.epoch = self.epoch.checked_add(1).ok_or(MemoError::Stale)?;
        #[cfg(test)]
        let launched = match &self.worker_image {
            Some(image) => PadCryptoClient::start_at(image),
            None => PadCryptoClient::start(),
        };
        #[cfg(not(test))]
        let launched = PadCryptoClient::start();
        self.client = Some(launched.map_err(map_client)?);
        Ok(())
    }

    fn start_request(
        &mut self,
        operation: Operation,
        password: SecretBytes,
        payload: SecretBytes,
    ) -> Result<SecretBytes, MemoError> {
        let result = self.exchange(
            operation,
            self.document_vault_id,
            self.memo_id,
            password,
            payload,
        );
        match result {
            Ok(bytes) => {
                self.unlocked = true;
                Ok(bytes)
            }
            Err(error) => {
                self.lock();
                Err(error)
            }
        }
    }

    fn in_session_request(
        &mut self,
        operation: Operation,
        payload: SecretBytes,
    ) -> Result<SecretBytes, MemoError> {
        if !self.unlocked {
            return Err(MemoError::Locked);
        }
        let result = self.exchange(operation, [0; 16], 0, SecretBytes::new(Vec::new()), payload);
        if result.is_err() {
            self.lock();
        }
        result
    }

    fn exchange(
        &mut self,
        operation: Operation,
        vault_id: [u8; 16],
        memo_id: u64,
        password: SecretBytes,
        payload: SecretBytes,
    ) -> Result<SecretBytes, MemoError> {
        let id = self.next_request_id;
        self.next_request_id = id.checked_add(1).ok_or(MemoError::Stale)?;
        let request = Request {
            id,
            generation: self.epoch,
            operation,
            vault_id,
            memo_id,
            password,
            payload,
        };
        let response = self
            .client
            .as_mut()
            .ok_or(MemoError::Locked)?
            .exchange(request, self.timeout)
            .map_err(map_client)?;
        match response.status {
            Status::Success => Ok(response.payload),
            Status::Rejected => Err(MemoError::Authentication),
            Status::Unavailable => Err(MemoError::Unavailable),
            Status::Locked => Err(MemoError::Locked),
            Status::Stale => Err(MemoError::Stale),
            Status::InvalidRequest => Err(MemoError::Protocol),
        }
    }
}

impl Drop for MemoProtectionSession {
    fn drop(&mut self) {
        self.lock();
    }
}

fn map_client(error: ClientError) -> MemoError {
    match error {
        ClientError::Protocol => MemoError::Protocol,
        ClientError::Launch
        | ClientError::Transport
        | ClientError::Timeout
        | ClientError::Closed => MemoError::Worker,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pad_hardware;
    use crate::pad_webauthn::Cancellation;
    use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

    fn bytes(value: &[u8]) -> SecretBytes {
        SecretBytes::new(value.to_vec())
    }

    #[test]
    fn rejects_zero_document_or_memo_scope() {
        assert_eq!(
            MemoProtectionSession::new([0; 16], 7, Duration::from_secs(1)).err(),
            Some(MemoError::InvalidScope)
        );
        assert_eq!(
            MemoProtectionSession::new([3; 16], 0, Duration::from_secs(1)).err(),
            Some(MemoError::InvalidScope)
        );
    }

    #[test]
    fn envelope_hint_distinguishes_legacy_recovery_and_unknown_without_decrypting() {
        assert_eq!(
            classify_envelope(b"SKRPENV1rest"),
            MemoEnvelopeFormat::PasswordOnlyV1
        );
        assert_eq!(
            classify_envelope(b"SKRPENV2rest"),
            MemoEnvelopeFormat::PasswordRecoveryV2
        );
        assert_eq!(classify_envelope(b"damaged"), MemoEnvelopeFormat::Unknown);
        let mut v3 = vec![0_u8; 42];
        v3[..8].copy_from_slice(b"SKRPENV3");
        v3[35] = 1;
        v3[36..38].copy_from_slice(&4_u16.to_le_bytes());
        v3[38..42].copy_from_slice(b"key1");
        assert_eq!(classify_envelope(&v3), MemoEnvelopeFormat::PrfV3);
        assert_eq!(hardware_hint(&v3).unwrap().credential_id, b"key1");
        v3[35] = 2;
        assert_eq!(classify_envelope(&v3), MemoEnvelopeFormat::PasswordAndPrfV3);
        assert!(hardware_hint(&v3).unwrap().requires_password);
        v3[36..38].copy_from_slice(&1025_u16.to_le_bytes());
        assert!(hardware_hint(&v3).is_none());
        assert_eq!(memo_prf_salt([17; 16], 42), memo_prf_salt([17; 16], 42));
        assert_ne!(memo_prf_salt([17; 16], 42), memo_prf_salt([17; 16], 43));
        assert_ne!(pad_prf_salt([17; 16]), memo_prf_salt([17; 16], 1));
        assert_eq!(pad_prf_salt([0; 16]), None);
    }

    /// Exercises the same per-memo key and worker boundaries used by the Pad
    /// UI with a real YubiKey, including older hmac-secret-only firmware.
    #[test]
    #[ignore = "requires SAKURA_PAD_SESSION_TEST_EXE, connected YubiKey 5, and interactive Windows desktop"]
    fn physical_yubikey5_seals_and_reopens_isolated_memo() {
        let image = PathBuf::from(
            std::env::var_os("SAKURA_PAD_SESSION_TEST_EXE")
                .expect("set SAKURA_PAD_SESSION_TEST_EXE to the built absolute session image"),
        );
        assert!(image.is_absolute() && image.is_file());
        let timeout = Duration::from_secs(20);
        let document_id = [91; 16];
        let memo_id = 23;
        let cancellation = Cancellation::new().expect("Windows WebAuthn support");
        // SAFETY: the desktop window remains live throughout these synchronous
        // Windows Security interactions.
        let parent = unsafe { GetDesktopWindow() };
        let (credential_id, prf) = pad_hardware::register_and_derive(
            parent,
            document_id,
            memo_id,
            Duration::from_secs(120),
            &cancellation,
        )
        .expect("YubiKey 5 registration and scoped memo assertion");
        let mut created = MemoProtectionSession::new(document_id, memo_id, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        let (envelope, recovery_key) = created
            .create_with_prf(
                &credential_id,
                prf,
                bytes(b"isolated YubiKey 5 memo content"),
            )
            .expect("real worker memo seal");
        assert_eq!(
            hardware_hint(&envelope).unwrap().credential_id,
            credential_id
        );
        created.lock();

        let derived = pad_hardware::derive_for_envelope(
            parent,
            document_id,
            memo_id,
            &envelope,
            Duration::from_secs(120),
            &cancellation,
        )
        .expect("YubiKey 5 memo assertion after reopen");
        let mut reopened = MemoProtectionSession::new(document_id, memo_id, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        assert_eq!(
            reopened
                .unlock_with_prf(&derived.credential_id, derived.prf, bytes(&envelope),)
                .expect("real worker memo reopen")
                .as_slice(),
            b"isolated YubiKey 5 memo content"
        );
        reopened.lock();

        let mut wrong_scope = MemoProtectionSession::new(document_id, memo_id + 1, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        assert_eq!(
            wrong_scope
                .unlock_with_recovery(bytes(recovery_key.as_bytes()), bytes(&envelope))
                .err(),
            Some(MemoError::Authentication)
        );
        let mut recovered = MemoProtectionSession::new(document_id, memo_id, timeout)
            .unwrap()
            .with_worker_image(image);
        assert_eq!(
            recovered
                .unlock_with_recovery(bytes(recovery_key.as_bytes()), bytes(&envelope))
                .expect("memo recovery remains scoped")
                .as_slice(),
            b"isolated YubiKey 5 memo content"
        );
    }

    /// Real worker coverage uses deterministic test PRF bytes. Hardware
    /// registration and user verification are checked separately on-device.
    #[test]
    #[ignore = "requires SAKURA_PAD_SESSION_TEST_EXE"]
    fn real_worker_v3_memo_prf_and_recovery_routes() {
        let image = PathBuf::from(
            std::env::var_os("SAKURA_PAD_SESSION_TEST_EXE")
                .expect("set SAKURA_PAD_SESSION_TEST_EXE to the built absolute session image"),
        );
        let timeout = Duration::from_secs(20);
        let scope = [31; 16];
        let id = b"test-fido-credential";
        let prf = Zeroizing::new([53; 32]);
        let mut created = MemoProtectionSession::new(scope, 19, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        let (envelope, key) = created
            .create_with_password_and_prf(bytes(b"password"), id, prf, bytes(b"v3 secret"))
            .unwrap();
        assert_eq!(hardware_hint(&envelope).unwrap().credential_id, id);
        created.lock();

        let mut missing_key = MemoProtectionSession::new(scope, 19, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        assert_eq!(
            missing_key
                .unlock(bytes(b"password"), bytes(&envelope))
                .err(),
            Some(MemoError::Authentication)
        );
        let mut wrong_prf = MemoProtectionSession::new(scope, 19, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        assert_eq!(
            wrong_prf
                .unlock_with_password_and_prf(
                    bytes(b"password"),
                    id,
                    Zeroizing::new([54; 32]),
                    bytes(&envelope),
                )
                .err(),
            Some(MemoError::Authentication)
        );
        let mut unlocked = MemoProtectionSession::new(scope, 19, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        assert_eq!(
            unlocked
                .unlock_with_password_and_prf(
                    bytes(b"password"),
                    id,
                    Zeroizing::new([53; 32]),
                    bytes(&envelope),
                )
                .unwrap()
                .as_slice(),
            b"v3 secret"
        );
        unlocked.lock();
        let mut recovered = MemoProtectionSession::new(scope, 19, timeout)
            .unwrap()
            .with_worker_image(image);
        assert_eq!(
            recovered
                .unlock_with_recovery(bytes(key.as_bytes()), bytes(&envelope))
                .unwrap()
                .as_slice(),
            b"v3 secret"
        );
    }

    /// Another package's binary is not built by `cargo test -p sakura-renderer`.
    /// The dedicated CI gate builds it first and sets an exact absolute path.
    #[test]
    #[ignore = "requires SAKURA_PAD_SESSION_TEST_EXE"]
    fn real_worker_memo_scope_round_trip_and_rejection() {
        let image = PathBuf::from(
            std::env::var_os("SAKURA_PAD_SESSION_TEST_EXE")
                .expect("set SAKURA_PAD_SESSION_TEST_EXE to the built absolute session image"),
        );
        assert!(image.is_absolute(), "worker image must be absolute");
        assert!(image.is_file(), "worker image must exist");
        let timeout = Duration::from_secs(20);
        let vault_id = [17; 16];
        let mut created = MemoProtectionSession::new(vault_id, 42, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        let sealed = created
            .create(bytes(b"memo password"), bytes(b"first"))
            .unwrap();
        assert_eq!(created.verify(bytes(&sealed)).unwrap().as_slice(), b"first");
        created.lock();

        let mut wrong_memo = MemoProtectionSession::new(vault_id, 43, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        assert_eq!(
            wrong_memo
                .unlock(bytes(b"memo password"), bytes(&sealed))
                .err(),
            Some(MemoError::Authentication)
        );
        assert!(wrong_memo.cancellation_handle().is_none());

        let mut reopened = MemoProtectionSession::new(vault_id, 42, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        assert_eq!(
            reopened
                .unlock(bytes(b"wrong password"), bytes(&sealed))
                .err(),
            Some(MemoError::Authentication)
        );
        assert!(reopened.cancellation_handle().is_none());
        assert_eq!(
            reopened
                .unlock(bytes(b"memo password"), bytes(&sealed))
                .unwrap()
                .as_slice(),
            b"first"
        );
        let resealed = reopened.reseal(bytes(b"second")).unwrap();
        reopened.lock();

        let mut final_open = MemoProtectionSession::new(vault_id, 42, timeout)
            .unwrap()
            .with_worker_image(image);
        assert_eq!(
            final_open
                .unlock(bytes(b"memo password"), resealed)
                .unwrap()
                .as_slice(),
            b"second"
        );
        final_open.lock();
    }

    /// Exercises the separately scoped v2 recovery operations against the
    /// actual worker executable built by the dedicated CI gate.
    #[test]
    #[ignore = "requires SAKURA_PAD_SESSION_TEST_EXE"]
    fn real_worker_recovery_key_round_trip_and_rejection() {
        let image = PathBuf::from(
            std::env::var_os("SAKURA_PAD_SESSION_TEST_EXE")
                .expect("set SAKURA_PAD_SESSION_TEST_EXE to the built absolute session image"),
        );
        assert!(image.is_absolute(), "worker image must be absolute");
        assert!(image.is_file(), "worker image must exist");
        let timeout = Duration::from_secs(20);
        let vault_id = [29; 16];
        let mut created = MemoProtectionSession::new(vault_id, 73, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        let (envelope, recovery_key) = created
            .create_with_recovery(bytes(b"memo password"), bytes(b"recoverable text"))
            .unwrap();
        assert!(recovery_key.as_str().starts_with("SPRK1-"));
        assert_eq!(
            created.verify(bytes(&envelope)).unwrap().as_slice(),
            b"recoverable text"
        );
        created.lock();

        let mut wrong_key = Zeroizing::new(recovery_key.to_string());
        let replacement = if wrong_key.as_bytes()[6] == b'0' {
            b'1'
        } else {
            b'0'
        };
        wrong_key.replace_range(6..7, std::str::from_utf8(&[replacement]).unwrap());
        let mut rejected = MemoProtectionSession::new(vault_id, 73, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        assert_eq!(
            rejected
                .unlock_with_recovery(bytes(wrong_key.as_bytes()), bytes(&envelope))
                .err(),
            Some(MemoError::Authentication)
        );
        assert!(rejected.cancellation_handle().is_none());

        let mut password_open = MemoProtectionSession::new(vault_id, 73, timeout)
            .unwrap()
            .with_worker_image(image.clone());
        assert_eq!(
            password_open
                .unlock(bytes(b"memo password"), bytes(&envelope))
                .unwrap()
                .as_slice(),
            b"recoverable text"
        );
        password_open.lock();

        let mut recovered = MemoProtectionSession::new(vault_id, 73, timeout)
            .unwrap()
            .with_worker_image(image);
        assert_eq!(
            recovered
                .unlock_with_recovery(bytes(recovery_key.as_bytes()), bytes(&envelope))
                .unwrap()
                .as_slice(),
            b"recoverable text"
        );
        recovered.lock();
    }
}
