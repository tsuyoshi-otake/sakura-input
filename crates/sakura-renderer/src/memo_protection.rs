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

use sakura_pad_session_proto::{parse_created_recovery, Operation, Request, SecretBytes, Status};
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
    Unknown,
}

pub fn classify_envelope(envelope: &[u8]) -> MemoEnvelopeFormat {
    if envelope.starts_with(b"SKRPENV1") {
        MemoEnvelopeFormat::PasswordOnlyV1
    } else if envelope.starts_with(b"SKRPENV2") {
        MemoEnvelopeFormat::PasswordRecoveryV2
    } else {
        MemoEnvelopeFormat::Unknown
    }
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
        self.fresh_worker()?;
        self.start_request(Operation::UnlockRecoveryV2, canonical_key, envelope)
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
