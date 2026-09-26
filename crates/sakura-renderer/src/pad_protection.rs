//! Synchronous Pad protection transaction. Call only from a storage thread.
//!
//! This object owns one isolated worker session, never a key. Its document
//! results are returned only after the worker authenticates the envelope.

use std::fs::File;
use std::io::Read;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sakura_pad_session_proto::{
    parse_created_recovery, Operation, Request, SecretBytes, Status, MAX_PAYLOAD_BYTES,
};
use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
use zeroize::{Zeroize, Zeroizing};

use crate::pad_crypto_client::{ClientError, PadCryptoCancellation, PadCryptoClient};
use crate::pad_storage::{
    LoadOutcome, PadDocument, PadStore, StorageError, WriteOutcome, DEBOUNCE,
};

/// Which durable transaction boundary an unsuccessful operation reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailurePhase {
    BeforeIntent,
    CutoverPending,
    Uncertain,
    Unlock,
    Save,
}

/// Content-free outcome suitable for UI status; never contains a path or text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureReason {
    Storage,
    Worker,
    Authentication,
    Unavailable,
    Locked,
    Stale,
    Protocol,
    Entropy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtectionError {
    pub phase: FailurePhase,
    pub reason: FailureReason,
}

impl ProtectionError {
    const fn new(phase: FailurePhase, reason: FailureReason) -> Self {
        Self { phase, reason }
    }
}

/// Prepared recoverable cutover. No durable intent exists until `confirm_recoverable_enroll`.
/// Dropping this value abandons the attempt; its ciphertext is zeroized.
#[allow(missing_debug_implementations)]
pub struct PreparedPadRecovery {
    expected_legacy: PadDocument,
    vault_id: [u8; 16],
    sealed: SecretBytes,
    epoch: u64,
}

/// One authenticated Pad vault session. Its methods block and must never run
/// on the window/message-pump thread. The caller stops the legacy StorageWorker
/// before calling `enroll`; this engine cannot enforce that other thread's
/// lifecycle.
#[allow(missing_debug_implementations)]
pub struct PadProtectionEngine {
    client: Option<PadCryptoClient>,
    vault_id: Option<[u8; 16]>,
    epoch: u64,
    next_request_id: u64,
    timeout: Duration,
    #[cfg(test)]
    worker_image: Option<PathBuf>,
}

impl PadProtectionEngine {
    pub const fn new(timeout: Duration) -> Self {
        Self {
            client: None,
            vault_id: None,
            epoch: 0,
            next_request_id: 1,
            timeout,
            #[cfg(test)]
            worker_image: None,
        }
    }

    #[cfg(test)]
    fn with_worker_image(mut self, image: PathBuf) -> Self {
        self.worker_image = Some(image);
        self
    }

    /// Test fixture for an older v1 Pad without a recovery key. Production
    /// enrollment uses `prepare_recoverable_enroll` instead.
    /// Seal the exact published legacy document, verify both staged copies,
    /// and publish the protected cutover. An error after intent is terminal for
    /// legacy persistence and requires explicit protected recovery.
    #[cfg(test)]
    pub fn enroll(
        &mut self,
        store: &PadStore,
        expected_legacy: &PadDocument,
        password: SecretBytes,
    ) -> Result<(), ProtectionError> {
        self.lock();
        let loaded = store.load().map_err(|_| {
            ProtectionError::new(FailurePhase::BeforeIntent, FailureReason::Storage)
        })?;
        if loaded.recovered_from_backup || loaded.document != *expected_legacy {
            return Err(ProtectionError::new(
                FailurePhase::BeforeIntent,
                FailureReason::Stale,
            ));
        }
        let vault_id = new_vault_id()?;
        self.fresh_session(FailurePhase::BeforeIntent)?;
        let encoded = match expected_legacy.encode() {
            Ok(bytes) => SecretBytes::new(bytes),
            Err(_) => {
                self.lock();
                return Err(ProtectionError::new(
                    FailurePhase::BeforeIntent,
                    FailureReason::Storage,
                ));
            }
        };
        let sealed = match self.exchange(Operation::Create, vault_id, password, encoded) {
            Ok(bytes) if !bytes.is_empty() => bytes,
            Ok(_) => {
                self.lock();
                return Err(ProtectionError::new(
                    FailurePhase::BeforeIntent,
                    FailureReason::Protocol,
                ));
            }
            Err(reason) => {
                self.lock();
                return Err(ProtectionError::new(FailurePhase::BeforeIntent, reason));
            }
        };
        self.vault_id = Some(vault_id);
        let migration = store.migrate_to_protected(
            expected_legacy,
            expected_legacy.generation,
            vault_id,
            &sealed,
            |path| self.verify_path(path, vault_id),
        );
        if migration.is_err() {
            // The store keeps no public intent query. A successful preflight
            // proved legacy mode earlier; the current legacy gate decides
            // whether legacy work remains possible after this failed call.
            let phase = match store.load() {
                Ok(_) => FailurePhase::BeforeIntent,
                Err(StorageError::ProtectedCutover) => FailurePhase::CutoverPending,
                Err(_) => FailurePhase::Uncertain,
            };
            self.lock();
            return Err(ProtectionError::new(phase, FailureReason::Storage));
        }
        Ok(())
    }

    /// Prepare a v2 whole-Pad vault and return its one-time display key.
    /// The caller must present the key and obtain explicit confirmation before
    /// calling `confirm_recoverable_enroll`. This method writes no durable Pad
    /// bytes or cutover signal. A new prepare invalidates the old worker session.
    pub fn prepare_recoverable_enroll(
        &mut self,
        store: &PadStore,
        expected_legacy: &PadDocument,
        password: SecretBytes,
    ) -> Result<(PreparedPadRecovery, Zeroizing<String>), ProtectionError> {
        self.lock();
        let loaded = store.load().map_err(|_| {
            ProtectionError::new(FailurePhase::BeforeIntent, FailureReason::Storage)
        })?;
        if loaded.recovered_from_backup || loaded.document != *expected_legacy {
            return Err(ProtectionError::new(
                FailurePhase::BeforeIntent,
                FailureReason::Stale,
            ));
        }
        let vault_id = new_vault_id()?;
        self.fresh_session(FailurePhase::BeforeIntent)?;
        let encoded = match expected_legacy.encode() {
            Ok(bytes) => SecretBytes::new(bytes),
            Err(_) => {
                self.lock();
                return Err(ProtectionError::new(
                    FailurePhase::BeforeIntent,
                    FailureReason::Storage,
                ));
            }
        };
        let result = self
            .exchange(Operation::CreateRecoverable, vault_id, password, encoded)
            .map_err(|reason| ProtectionError::new(FailurePhase::BeforeIntent, reason))
            .and_then(|payload| {
                let created = parse_created_recovery(&payload).map_err(|_| {
                    ProtectionError::new(FailurePhase::BeforeIntent, FailureReason::Protocol)
                })?;
                Ok((
                    SecretBytes::new(created.envelope.to_vec()),
                    Zeroizing::new(created.recovery_key.to_owned()),
                ))
            });
        match result {
            Ok((sealed, key)) => {
                self.vault_id = Some(vault_id);
                Ok((
                    PreparedPadRecovery {
                        expected_legacy: expected_legacy.clone(),
                        vault_id,
                        sealed,
                        epoch: self.epoch,
                    },
                    key,
                ))
            }
            Err(error) => {
                self.lock();
                Err(error)
            }
        }
    }

    /// Commit only after the caller confirms that the one-time key was saved.
    /// The store rechecks the exact legacy document while holding its writer
    /// lock, verifies both protected copies, and then publishes cutover intent.
    pub fn confirm_recoverable_enroll(
        &mut self,
        store: &PadStore,
        prepared: PreparedPadRecovery,
    ) -> Result<(), ProtectionError> {
        if self.vault_id != Some(prepared.vault_id) || self.epoch != prepared.epoch {
            return Err(ProtectionError::new(
                FailurePhase::BeforeIntent,
                FailureReason::Stale,
            ));
        }
        let migration = store.migrate_to_protected(
            &prepared.expected_legacy,
            prepared.expected_legacy.generation,
            prepared.vault_id,
            &prepared.sealed,
            |path| self.verify_path(path, prepared.vault_id),
        );
        if let Err(error) = migration {
            let phase = match store.load() {
                Ok(_) => FailurePhase::BeforeIntent,
                Err(StorageError::ProtectedCutover) => FailurePhase::CutoverPending,
                Err(_) => FailurePhase::Uncertain,
            };
            self.lock();
            let reason = if matches!(error, StorageError::LegacyChanged) {
                FailureReason::Stale
            } else {
                FailureReason::Storage
            };
            return Err(ProtectionError::new(phase, reason));
        }
        Ok(())
    }

    /// Open primary or backup under the authenticated vault scope. An active
    /// marker allows backup fallback, each with a fresh worker and epoch. A
    /// pending cutover instead requires authentication of *both* protected
    /// copies before publishing the final marker; legacy bytes are never read.
    /// A document is returned only after authentication and format decoding.
    pub fn unlock(
        &mut self,
        store: &PadStore,
        password: SecretBytes,
    ) -> Result<LoadOutcome, ProtectionError> {
        self.unlock_with_credential(store, password, false)
    }

    /// Open only a v2 whole-Pad envelope using its canonical recovery key.
    /// V1 and unknown formats are rejected; authentication failure never
    /// authorizes a backup with another envelope format.
    pub fn unlock_with_recovery(
        &mut self,
        store: &PadStore,
        canonical_key: SecretBytes,
    ) -> Result<LoadOutcome, ProtectionError> {
        self.unlock_with_credential(store, canonical_key, true)
    }

    fn unlock_with_credential(
        &mut self,
        store: &PadStore,
        credential: SecretBytes,
        recovery: bool,
    ) -> Result<LoadOutcome, ProtectionError> {
        self.lock();
        let (vault_id, pending_cutover) = match store.protected_vault_id() {
            Ok(id) => (id, false),
            Err(StorageError::ProtectedCutover) => (
                store.pending_vault_id().map_err(|_| {
                    ProtectionError::new(FailurePhase::Unlock, FailureReason::Storage)
                })?,
                true,
            ),
            Err(_) => {
                return Err(ProtectionError::new(
                    FailurePhase::Unlock,
                    FailureReason::Storage,
                ));
            }
        };
        let mut password = credential;
        let mut attempt_failure = None;
        let mut auth_rejected = false;
        let memo_cutover = store
            .has_protected_memo_cutover()
            .map_err(|_| ProtectionError::new(FailurePhase::Unlock, FailureReason::Storage))?;
        let loaded = if pending_cutover {
            let mut primary_opened = false;
            store
                .recover_protected_cutover(|path| {
                    if auth_rejected {
                        return Err(StorageError::ProtectedVerification);
                    }
                    if primary_opened {
                        self.verify_path(path, vault_id)
                    } else {
                        let result = self.unlock_path(
                            path,
                            vault_id,
                            &password,
                            recovery,
                            &mut attempt_failure,
                        );
                        auth_rejected = attempt_failure == Some(FailureReason::Authentication);
                        primary_opened = result.is_ok();
                        result
                    }
                })
                .map(|document| LoadOutcome {
                    document,
                    recovered_from_backup: false,
                })
        } else if memo_cutover {
            // A completed memo-protection transition forbids reopening a
            // pre-protection v3 backup. Authenticate candidates with a fresh
            // password session, enforce the durable generation floor, and
            // repair an interrupted primary before exposing an editable Pad.
            store
                .recover_protected_memo_cutover(|path| {
                    if auth_rejected {
                        return Err(StorageError::ProtectedVerification);
                    }
                    let result =
                        self.unlock_path(path, vault_id, &password, recovery, &mut attempt_failure);
                    auth_rejected = attempt_failure == Some(FailureReason::Authentication);
                    result
                })
                .map(|document| LoadOutcome {
                    document,
                    recovered_from_backup: false,
                })
        } else {
            store.load_protected(|path| {
                if auth_rejected {
                    return Err(StorageError::ProtectedVerification);
                }
                let result =
                    self.unlock_path(path, vault_id, &password, recovery, &mut attempt_failure);
                auth_rejected = attempt_failure == Some(FailureReason::Authentication);
                result
            })
        };
        password.zeroize();
        match loaded {
            Ok(value) => Ok(value),
            Err(error) => {
                self.lock();
                Err(ProtectionError::new(
                    FailurePhase::Unlock,
                    if matches!(error, StorageError::UnsupportedVersion(_)) {
                        FailureReason::Storage
                    } else {
                        attempt_failure.unwrap_or(FailureReason::Storage)
                    },
                ))
            }
        }
    }

    fn unlock_path(
        &mut self,
        path: &Path,
        vault_id: [u8; 16],
        password: &SecretBytes,
        recovery: bool,
        attempt_failure: &mut Option<FailureReason>,
    ) -> Result<([u8; 16], PadDocument), StorageError> {
        if let Err(error) = self.fresh_session(FailurePhase::Unlock) {
            record_failure(attempt_failure, error.reason);
            return Err(StorageError::ProtectedVerification);
        }
        let bytes = match read_envelope(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                record_failure(attempt_failure, FailureReason::Storage);
                self.lock();
                return Err(error);
            }
        };
        // Request owns a zeroizing copy; the caller's password is erased at
        // the end of unlock, including failed primary/backup attempts.
        let operation = if bytes.starts_with(b"SKRPENV2") {
            if recovery {
                Operation::UnlockRecoveryV2
            } else {
                Operation::UnlockPasswordV2
            }
        } else if bytes.starts_with(b"SKRPENV1") && !recovery {
            Operation::Unlock
        } else {
            record_failure(attempt_failure, FailureReason::Authentication);
            self.lock();
            return Err(StorageError::ProtectedVerification);
        };
        let opened = self.exchange(
            operation,
            vault_id,
            SecretBytes::new(password.to_vec()),
            bytes,
        );
        match opened {
            Ok(plain) => match PadDocument::decode(&plain) {
                Ok(document) => {
                    self.vault_id = Some(vault_id);
                    Ok((vault_id, document))
                }
                Err(error) => {
                    record_failure(attempt_failure, FailureReason::Storage);
                    self.lock();
                    Err(error)
                }
            },
            Err(reason) => {
                record_failure(attempt_failure, reason);
                self.lock();
                Err(StorageError::ProtectedVerification)
            }
        }
    }

    /// Reseal the next document, authenticate the currently published and
    /// staged copies, and publish only on exact expected-document match.
    /// Every error is terminal for this attempt. The caller still owns `next`
    /// and must retain its unsaved plaintext; locking this worker never saves
    /// or discards the caller's document. Failed staged files stay on disk.
    pub fn save(
        &mut self,
        store: &PadStore,
        expected: &PadDocument,
        next: &PadDocument,
    ) -> Result<WriteOutcome, ProtectionError> {
        let vault_id = self
            .vault_id
            .ok_or_else(|| ProtectionError::new(FailurePhase::Save, FailureReason::Locked))?;
        let encoded = SecretBytes::new(
            next.encode()
                .map_err(|_| ProtectionError::new(FailurePhase::Save, FailureReason::Storage))?,
        );
        let sealed = match self.exchange(
            Operation::Reseal,
            [0; 16],
            SecretBytes::new(Vec::new()),
            encoded,
        ) {
            Ok(bytes) => bytes,
            Err(reason) => {
                self.lock();
                return Err(ProtectionError::new(FailurePhase::Save, reason));
            }
        };
        if sealed.is_empty() {
            self.lock();
            return Err(ProtectionError::new(
                FailurePhase::Save,
                FailureReason::Protocol,
            ));
        }
        let result = store
            .write_protected(expected, next, &sealed, |path| {
                self.verify_path(path, vault_id)
            })
            .map_err(|error| {
                let reason = if matches!(error, StorageError::StaleProtectedDocument) {
                    FailureReason::Stale
                } else {
                    FailureReason::Storage
                };
                ProtectionError::new(FailurePhase::Save, reason)
            });
        if result.is_err() {
            self.lock();
        }
        result
    }

    /// Cancel and reap the exact worker child, dropping its key session.
    pub fn lock(&mut self) {
        if let Some(mut client) = self.client.take() {
            client.cancel();
        }
        self.vault_id = None;
    }

    fn cancellation_handle(&self) -> Option<PadCryptoCancellation> {
        self.client
            .as_ref()
            .map(PadCryptoClient::cancellation_handle)
    }

    fn fresh_session(&mut self, phase: FailurePhase) -> Result<(), ProtectionError> {
        self.lock();
        self.epoch = self
            .epoch
            .checked_add(1)
            .ok_or_else(|| ProtectionError::new(phase, FailureReason::Stale))?;
        #[cfg(test)]
        let launched = match &self.worker_image {
            Some(image) => PadCryptoClient::start_at(image),
            None => PadCryptoClient::start(),
        };
        #[cfg(not(test))]
        let launched = PadCryptoClient::start();
        self.client =
            Some(launched.map_err(|_| ProtectionError::new(phase, FailureReason::Worker))?);
        Ok(())
    }

    fn exchange(
        &mut self,
        operation: Operation,
        vault_id: [u8; 16],
        password: SecretBytes,
        payload: SecretBytes,
    ) -> Result<SecretBytes, FailureReason> {
        let id = self.next_request_id;
        self.next_request_id = id.checked_add(1).ok_or(FailureReason::Stale)?;
        let request = Request {
            id,
            generation: self.epoch,
            operation,
            vault_id,
            memo_id: 0,
            password,
            payload,
        };
        let response = self
            .client
            .as_mut()
            .ok_or(FailureReason::Locked)?
            .exchange(request, self.timeout)
            .map_err(map_client)?;
        match response.status {
            Status::Success => Ok(response.payload),
            Status::Rejected => Err(FailureReason::Authentication),
            Status::Unavailable => Err(FailureReason::Unavailable),
            Status::Locked => Err(FailureReason::Locked),
            Status::Stale => Err(FailureReason::Stale),
            Status::InvalidRequest => Err(FailureReason::Protocol),
        }
    }

    fn verify_path(
        &mut self,
        path: &Path,
        vault_id: [u8; 16],
    ) -> Result<([u8; 16], PadDocument), StorageError> {
        if self.vault_id != Some(vault_id) {
            return Err(StorageError::ProtectedVerification);
        }
        let opened = self
            .exchange(
                Operation::Verify,
                [0; 16],
                SecretBytes::new(Vec::new()),
                read_envelope(path)?,
            )
            .map_err(|_| StorageError::ProtectedVerification)?;
        let document =
            PadDocument::decode(&opened).map_err(|_| StorageError::ProtectedVerification)?;
        // Verify authenticates the envelope against a session whose scope was
        // established with `vault_id`; its result binds this ID to plaintext.
        Ok((vault_id, document))
    }
}

impl Drop for PadProtectionEngine {
    fn drop(&mut self) {
        self.lock();
    }
}

fn read_envelope(path: &Path) -> Result<SecretBytes, StorageError> {
    let file = File::open(path).map_err(|_| StorageError::ProtectedVerification)?;
    let mut bytes = SecretBytes::new(Vec::new());
    file.take((MAX_PAYLOAD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| StorageError::ProtectedVerification)?;
    if bytes.is_empty() || bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(StorageError::ProtectedVerification);
    }
    Ok(bytes)
}

fn new_vault_id() -> Result<[u8; 16], ProtectionError> {
    let mut id = [0u8; 16];
    // SAFETY: BCryptGenRandom writes only the supplied fixed-size buffer.
    let status = unsafe { BCryptGenRandom(None, &mut id, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_err() || id == [0; 16] {
        return Err(ProtectionError::new(
            FailurePhase::BeforeIntent,
            FailureReason::Entropy,
        ));
    }
    Ok(id)
}

fn map_client(error: ClientError) -> FailureReason {
    match error {
        ClientError::Protocol => FailureReason::Protocol,
        ClientError::Launch
        | ClientError::Transport
        | ClientError::Timeout
        | ClientError::Closed => FailureReason::Worker,
    }
}

fn record_failure(previous: &mut Option<FailureReason>, reason: FailureReason) {
    // A missing/corrupt recovery copy or worker failure must not be reported
    // as merely a wrong password when another candidate rejected it.
    let priority = |reason| match reason {
        FailureReason::Storage => 5,
        FailureReason::Worker | FailureReason::Protocol | FailureReason::Unavailable => 4,
        FailureReason::Stale | FailureReason::Locked | FailureReason::Entropy => 3,
        FailureReason::Authentication => 1,
    };
    if previous.is_none_or(|old| priority(reason) > priority(old)) {
        *previous = Some(reason);
    }
}

/// The worker reports only the newest completed generation. Earlier written
/// generations can be superseded because each snapshot is a complete Pad.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectedSaveStatus {
    Written(WriteOutcome),
    Failed(ProtectionError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtectedSaveCompletion {
    pub generation: u64,
    pub status: ProtectedSaveStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitRejectReason {
    Stale,
    Closing,
    Failed,
}

/// Rejected snapshots return to the caller; they are never silently dropped.
#[allow(missing_debug_implementations)]
pub struct RejectedSnapshot {
    pub reason: SubmitRejectReason,
    pub document: PadDocument,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtectedLockStatus {
    /// Worker exited and every accepted snapshot was published.
    Saved,
    /// Worker exited after a terminal save failure; no retry was attempted.
    Unsaved(ProtectionError),
    /// Deadline expired while storage may still be finishing. Never claim a
    /// successful save from this state, even if the child was cancelled.
    Uncertain,
}

/// `latest_unsaved` shares the submitted document allocation; the UI must
/// retain it for explicit recovery. It can be None for an uncertain shutdown
/// when no edit was ever submitted.
#[allow(missing_debug_implementations)]
pub struct ProtectedLockOutcome {
    pub status: ProtectedLockStatus,
    pub confirmed_generation: u64,
    /// Exact last published base for comparing a draft after a later unlock.
    pub confirmed_document: Arc<PadDocument>,
    pub latest_unsaved: Option<Arc<PadDocument>>,
}

#[allow(missing_debug_implementations)]
struct SaveMailboxState {
    pending: Option<Arc<PadDocument>>,
    latest: Option<Arc<PadDocument>>,
    last_submitted_generation: u64,
    confirmed_generation: u64,
    confirmed_document: Arc<PadDocument>,
    completion: Option<ProtectedSaveCompletion>,
    terminal_failure: Option<ProtectionError>,
    closing: bool,
}

#[allow(missing_debug_implementations)]
struct SaveMailbox {
    state: Mutex<SaveMailboxState>,
    wake: Condvar,
}

/// A single protected writer, a one-slot latest-value mailbox, and one
/// completion slot. The worker alone mutates its engine and confirmed document.
/// `submit` and `try_completion` are nonblocking with respect to crypto/I/O.
#[allow(missing_debug_implementations)]
pub struct ProtectedSaveActor {
    mailbox: Arc<SaveMailbox>,
    done: mpsc::Receiver<()>,
    join: Option<JoinHandle<()>>,
    cancellation: Option<PadCryptoCancellation>,
    finished: bool,
}

impl ProtectedSaveActor {
    /// Write a mixed plain/protected v4 document on the same bounded save
    /// mailbox used by the whole-Pad vault. The caller authenticates any
    /// newly created or resealed memo with its own worker session before it
    /// submits the snapshot; locked memo envelopes stay opaque to this actor.
    pub fn spawn_v4(store: PadStore, confirmed_doc: PadDocument) -> Result<Self, FailureReason> {
        if confirmed_doc.document_id == [0; 16] {
            return Err(FailureReason::Stale);
        }
        let confirmed_doc = Arc::new(confirmed_doc);
        let mailbox = new_mailbox(Arc::clone(&confirmed_doc));
        let worker_mailbox = Arc::clone(&mailbox);
        let (done_tx, done) = mpsc::channel();
        let join = thread::Builder::new()
            .name("sakura-pad-v4-save".into())
            .spawn(move || v4_save_loop(worker_mailbox, store, confirmed_doc, done_tx))
            .map_err(|_| FailureReason::Worker)?;
        Ok(Self {
            mailbox,
            done,
            join: Some(join),
            cancellation: None,
            finished: false,
        })
    }

    /// Move an already authenticated engine into the storage thread. The
    /// caller must have stopped the legacy StorageWorker before constructing
    /// this actor. A failed spawn leaves `engine` to Drop and reap its child.
    pub fn spawn_unlocked(
        store: PadStore,
        engine: PadProtectionEngine,
        confirmed_doc: PadDocument,
    ) -> Result<Self, FailureReason> {
        if engine.vault_id.is_none() {
            return Err(FailureReason::Locked);
        }
        let cancellation = engine.cancellation_handle().ok_or(FailureReason::Locked)?;
        let confirmed_doc = Arc::new(confirmed_doc);
        let mailbox = new_mailbox(Arc::clone(&confirmed_doc));
        let worker_mailbox = Arc::clone(&mailbox);
        let (done_tx, done) = mpsc::channel();
        let join = thread::Builder::new()
            .name("sakura-pad-protected-save".into())
            .spawn(move || {
                protected_save_loop(worker_mailbox, store, engine, confirmed_doc, done_tx)
            })
            .map_err(|_| FailureReason::Worker)?;
        Ok(Self {
            mailbox,
            done,
            join: Some(join),
            cancellation: Some(cancellation),
            finished: false,
        })
    }

    /// Accept only a newer complete snapshot. An accepted snapshot is held in
    /// one Arc allocation until confirmed or returned as unsaved. Rejection
    /// returns the supplied document intact to its caller.
    pub fn submit(&self, next: PadDocument) -> Result<(), RejectedSnapshot> {
        let mut state = self.mailbox.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return Err(RejectedSnapshot {
                reason: SubmitRejectReason::Closing,
                document: next,
            });
        }
        if state.terminal_failure.is_some() {
            return Err(RejectedSnapshot {
                reason: SubmitRejectReason::Failed,
                document: next,
            });
        }
        let binding_id = state
            .latest
            .as_ref()
            .map_or(state.confirmed_document.document_id, |doc| doc.document_id);
        if next.generation <= state.last_submitted_generation
            || (binding_id != [0; 16] && next.document_id != binding_id)
        {
            return Err(RejectedSnapshot {
                reason: SubmitRejectReason::Stale,
                document: next,
            });
        }
        let generation = next.generation;
        let next = Arc::new(next);
        state.pending = Some(Arc::clone(&next));
        state.latest = Some(next);
        state.last_submitted_generation = generation;
        self.mailbox.wake.notify_one();
        Ok(())
    }

    pub fn try_completion(&self) -> Option<ProtectedSaveCompletion> {
        self.mailbox
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .completion
            .take()
    }

    /// Stop accepting edits and ask the worker to flush the latest snapshot
    /// without waiting for the debounce. Call while the UI still owns the
    /// editor controls so their last text has already been submitted.
    pub fn begin_lock(&self) {
        let mut state = self.mailbox.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closing = true;
        self.mailbox.wake.notify_one();
    }

    /// Bound UI waiting. On timeout cancel the exact crypto child; a storage
    /// system call may still be in flight, so return Uncertain with the latest
    /// snapshot and never imply durable success. Call only after begin_lock.
    pub fn finish_lock(&mut self, budget: Duration) -> ProtectedLockOutcome {
        self.begin_lock();
        let finished = self.finished || self.done.recv_timeout(budget).is_ok();
        if finished {
            self.finished = true;
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        } else {
            if let Some(cancellation) = &self.cancellation {
                cancellation.cancel();
            }
            // A hung filesystem call cannot be joined on the UI deadline.
            // Keep the handle so a later finish_lock can observe completion.
        }
        let state = self.mailbox.state.lock().unwrap_or_else(|e| e.into_inner());
        let latest_unsaved = state
            .latest
            .as_ref()
            .filter(|doc| doc.generation > state.confirmed_generation)
            .cloned();
        let status = if !finished {
            ProtectedLockStatus::Uncertain
        } else if let Some(error) = state.terminal_failure {
            ProtectedLockStatus::Unsaved(error)
        } else if latest_unsaved.is_some() {
            ProtectedLockStatus::Uncertain
        } else {
            ProtectedLockStatus::Saved
        };
        ProtectedLockOutcome {
            status,
            confirmed_generation: state.confirmed_generation,
            confirmed_document: Arc::clone(&state.confirmed_document),
            latest_unsaved,
        }
    }
}

impl Drop for ProtectedSaveActor {
    fn drop(&mut self) {
        self.begin_lock();
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
    }
}

fn new_mailbox(confirmed_document: Arc<PadDocument>) -> Arc<SaveMailbox> {
    let confirmed_generation = confirmed_document.generation;
    Arc::new(SaveMailbox {
        state: Mutex::new(SaveMailboxState {
            pending: None,
            latest: None,
            last_submitted_generation: confirmed_generation,
            confirmed_generation,
            confirmed_document,
            completion: None,
            terminal_failure: None,
            closing: false,
        }),
        wake: Condvar::new(),
    })
}

fn protected_save_loop(
    mailbox: Arc<SaveMailbox>,
    store: PadStore,
    mut engine: PadProtectionEngine,
    confirmed: Arc<PadDocument>,
    done: mpsc::Sender<()>,
) {
    run_save_loop(mailbox, confirmed, |expected, next| {
        engine.save(&store, expected, next)
    });
    engine.lock();
    let _ = done.send(());
}

fn v4_save_loop(
    mailbox: Arc<SaveMailbox>,
    store: PadStore,
    confirmed: Arc<PadDocument>,
    done: mpsc::Sender<()>,
) {
    run_save_loop(mailbox, confirmed, |expected, next| {
        store.write_v4(expected, next).map_err(|error| {
            let reason = match error {
                StorageError::StaleProtectedDocument | StorageError::LegacyChanged => {
                    FailureReason::Stale
                }
                StorageError::ProtectedVerification => FailureReason::Authentication,
                _ => FailureReason::Storage,
            };
            ProtectionError::new(FailurePhase::Save, reason)
        })
    });
    let _ = done.send(());
}

fn run_save_loop(
    mailbox: Arc<SaveMailbox>,
    confirmed: Arc<PadDocument>,
    mut save: impl FnMut(&PadDocument, &PadDocument) -> Result<WriteOutcome, ProtectionError>,
) {
    let mut confirmed = confirmed;
    loop {
        let next = {
            let mut state = mailbox.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.terminal_failure.is_some() || (state.closing && state.pending.is_none()) {
                    return;
                }
                if state.pending.is_some() {
                    break;
                }
                state = mailbox.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            // A close takes the current latest value immediately. Otherwise
            // wait until its generation stays unchanged for one debounce.
            while !state.closing {
                let generation = state
                    .pending
                    .as_ref()
                    .map(|doc| doc.generation)
                    .unwrap_or(0);
                let (next, timed) = mailbox
                    .wake
                    .wait_timeout(state, DEBOUNCE)
                    .unwrap_or_else(|e| e.into_inner());
                state = next;
                if state.closing
                    || (timed.timed_out()
                        && state
                            .pending
                            .as_ref()
                            .is_some_and(|doc| doc.generation == generation))
                {
                    break;
                }
            }
            state.pending.take().expect("pending before debounce")
        };
        let generation = next.generation;
        let result = save(&confirmed, &next);
        let mut state = mailbox.state.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(outcome) => {
                confirmed = next;
                state.confirmed_generation = generation;
                state.confirmed_document = Arc::clone(&confirmed);
                state.completion = Some(ProtectedSaveCompletion {
                    generation,
                    status: ProtectedSaveStatus::Written(outcome),
                });
            }
            Err(error) => {
                state.terminal_failure = Some(error);
                state.completion = Some(ProtectedSaveCompletion {
                    generation,
                    status: ProtectedSaveStatus::Failed(error),
                });
            }
        }
    }
}

#[cfg(test)]
mod actor_tests {
    use super::*;

    fn document(generation: u64) -> PadDocument {
        PadDocument {
            generation,
            ..PadDocument::default()
        }
    }

    fn scripted_actor(
        save: impl FnMut(&PadDocument, &PadDocument) -> Result<WriteOutcome, ProtectionError>
            + Send
            + 'static,
    ) -> ProtectedSaveActor {
        let confirmed = Arc::new(document(1));
        let mailbox = new_mailbox(Arc::clone(&confirmed));
        let worker_mailbox = Arc::clone(&mailbox);
        let (done_tx, done) = mpsc::channel();
        let join = thread::spawn(move || {
            run_save_loop(worker_mailbox, confirmed, save);
            let _ = done_tx.send(());
        });
        ProtectedSaveActor {
            mailbox,
            done,
            join: Some(join),
            cancellation: None,
            finished: false,
        }
    }

    #[test]
    fn superseded_pending_snapshot_writes_only_latest_generation() {
        let (written, observed) = mpsc::channel();
        let mut actor = scripted_actor(move |expected, next| {
            written
                .send((expected.generation, next.generation))
                .unwrap();
            Ok(WriteOutcome::Replaced)
        });
        assert!(actor.submit(document(2)).is_ok());
        assert!(actor.submit(document(3)).is_ok());
        actor.begin_lock();
        let locked = actor.finish_lock(Duration::from_secs(2));
        assert_eq!(locked.status, ProtectedLockStatus::Saved);
        assert_eq!(locked.confirmed_generation, 3);
        assert_eq!(locked.confirmed_document.generation, 3);
        assert!(locked.latest_unsaved.is_none());
        assert_eq!(observed.try_recv().unwrap(), (1, 3));
        assert!(observed.try_recv().is_err());
        assert_eq!(
            actor.try_completion(),
            Some(ProtectedSaveCompletion {
                generation: 3,
                status: ProtectedSaveStatus::Written(WriteOutcome::Replaced),
            })
        );
    }

    #[test]
    fn stale_submission_returns_the_owned_snapshot() {
        let mut actor = scripted_actor(|_, _| Ok(WriteOutcome::Replaced));
        let rejected = actor.submit(document(1)).err().unwrap();
        assert_eq!(rejected.reason, SubmitRejectReason::Stale);
        assert_eq!(rejected.document.generation, 1);
        let locked = actor.finish_lock(Duration::from_secs(2));
        assert_eq!(locked.status, ProtectedLockStatus::Saved);
    }

    #[test]
    fn first_memo_scope_binding_cannot_be_dropped_by_a_newer_pending_snapshot() {
        let mut actor = scripted_actor(|_, _| Ok(WriteOutcome::Replaced));
        let mut first_protected = document(2);
        first_protected.document_id = [7; 16];
        assert!(actor.submit(first_protected).is_ok());
        let rejected = actor.submit(document(3)).err().unwrap();
        assert_eq!(rejected.reason, SubmitRejectReason::Stale);
        assert_eq!(rejected.document.document_id, [0; 16]);
        let locked = actor.finish_lock(Duration::from_secs(2));
        assert_eq!(locked.status, ProtectedLockStatus::Saved);
        assert_eq!(locked.confirmed_document.document_id, [7; 16]);
    }

    #[test]
    fn failure_keeps_latest_unsaved_snapshot_and_stops_retries() {
        let (started, observed) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let failure = ProtectionError::new(FailurePhase::Save, FailureReason::Storage);
        let mut actor = scripted_actor(move |_, next| {
            started.send(next.generation).unwrap();
            gate.recv_timeout(Duration::from_secs(2)).unwrap();
            Err(failure)
        });
        assert!(actor.submit(document(2)).is_ok());
        assert_eq!(observed.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        assert!(actor.submit(document(3)).is_ok());
        release.send(()).unwrap();
        let locked = actor.finish_lock(Duration::from_secs(2));
        assert_eq!(locked.status, ProtectedLockStatus::Unsaved(failure));
        assert_eq!(locked.confirmed_generation, 1);
        assert_eq!(locked.confirmed_document.generation, 1);
        assert_eq!(locked.latest_unsaved.unwrap().generation, 3);
        assert_eq!(
            actor.try_completion(),
            Some(ProtectedSaveCompletion {
                generation: 2,
                status: ProtectedSaveStatus::Failed(failure),
            })
        );
    }

    #[test]
    fn finish_lock_timeout_reports_uncertain_and_can_later_finish() {
        let (started, observed) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut actor = scripted_actor(move |_, next| {
            started.send(next.generation).unwrap();
            gate.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(WriteOutcome::Replaced)
        });
        assert!(actor.submit(document(2)).is_ok());
        actor.begin_lock();
        assert_eq!(observed.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        let uncertain = actor.finish_lock(Duration::from_millis(10));
        assert_eq!(uncertain.status, ProtectedLockStatus::Uncertain);
        assert_eq!(uncertain.confirmed_generation, 1);
        assert_eq!(uncertain.confirmed_document.generation, 1);
        assert_eq!(uncertain.latest_unsaved.unwrap().generation, 2);
        release.send(()).unwrap();
        let completed = actor.finish_lock(Duration::from_secs(2));
        assert_eq!(completed.status, ProtectedLockStatus::Saved);
        assert_eq!(completed.confirmed_generation, 2);
        assert_eq!(completed.confirmed_document.generation, 2);
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;
    use crate::memo_protection::MemoProtectionSession;
    use crate::pad_storage::{MemoPayloadV1, PadMemo};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    struct IsolatedPad(PathBuf);

    impl IsolatedPad {
        fn new() -> Self {
            let home = std::env::var_os("USERPROFILE").expect("USERPROFILE for isolated Pad test");
            let root = PathBuf::from(home).join("tmp");
            std::fs::create_dir_all(&root).expect("create ~/tmp");
            loop {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let path = root.join(format!(
                    "sakura-pad-protection-test-{}-{id}",
                    std::process::id()
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create isolated Pad directory: {error}"),
                }
            }
        }
    }

    impl Drop for IsolatedPad {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn password() -> SecretBytes {
        SecretBytes::new(b"test-only Pad password, never persisted".to_vec())
    }

    fn engine(image: &Path) -> PadProtectionEngine {
        PadProtectionEngine::new(Duration::from_secs(20)).with_worker_image(image.to_path_buf())
    }

    #[test]
    fn v4_actor_persists_plain_edit_after_migration_with_exact_base() {
        let isolated = IsolatedPad::new();
        let store = PadStore::at(&isolated.0);
        let mut legacy = PadDocument {
            generation: 1,
            ..PadDocument::default()
        };
        legacy.memos.push(PadMemo::new(1, "plain", "first", 1));
        store.write(&legacy).unwrap();
        let migrated = store
            .migrate_to_v4(
                &legacy,
                |id, old| {
                    let mut next = old.clone();
                    next.document_id = id;
                    Ok(next)
                },
                |_, _, _| Ok(()),
            )
            .unwrap();
        let mut actor = ProtectedSaveActor::spawn_v4(store.clone(), migrated.clone()).unwrap();
        let mut changed = migrated.clone();
        changed.generation += 1;
        changed
            .find_mut(1)
            .unwrap()
            .edit("plain edited", "second", 2)
            .unwrap();
        assert!(actor.submit(changed.clone()).is_ok());
        let lock = actor.finish_lock(Duration::from_secs(2));
        assert_eq!(lock.status, ProtectedLockStatus::Saved);
        assert_eq!(*lock.confirmed_document, changed);
        assert_eq!(store.load_v4().unwrap().document, changed);
    }

    /// Cargo does not build another package's binary for renderer unit tests.
    /// CI builds the worker first and supplies its exact absolute artifact path.
    #[test]
    #[ignore = "requires SAKURA_PAD_SESSION_TEST_EXE"]
    fn real_worker_enroll_reopen_save_and_relock() {
        let image = PathBuf::from(
            std::env::var_os("SAKURA_PAD_SESSION_TEST_EXE")
                .expect("set SAKURA_PAD_SESSION_TEST_EXE to the built absolute session image"),
        );
        assert!(image.is_absolute(), "worker test image must be absolute");
        assert!(image.is_file(), "worker test image must exist");
        let isolated = IsolatedPad::new();
        let store = PadStore::at(&isolated.0);
        let original = store.load().unwrap().document;
        assert_eq!(original, PadDocument::default());

        let mut first = engine(&image);
        first.enroll(&store, &original, password()).unwrap();
        first.lock();
        drop(first);

        let mut reopened = engine(&image);
        let loaded = reopened.unlock(&store, password()).unwrap();
        assert_eq!(loaded.document, original);
        assert!(!loaded.recovered_from_backup);

        let mut edited = loaded.document.clone();
        edited.generation += 1;
        edited
            .memos
            .push(PadMemo::new(1, "protected", "body after save", 1));
        assert_eq!(
            reopened.save(&store, &loaded.document, &edited).unwrap(),
            WriteOutcome::Replaced
        );
        reopened.lock();

        let mut final_open = engine(&image);
        let final_loaded = final_open.unlock(&store, password()).unwrap();
        assert_eq!(final_loaded.document, edited);
        assert!(!final_loaded.recovered_from_backup);
        final_open.lock();
    }

    #[test]
    #[ignore = "requires SAKURA_PAD_SESSION_TEST_EXE"]
    fn real_worker_recovery_prepare_confirm_reopen_and_save() {
        let image = PathBuf::from(
            std::env::var_os("SAKURA_PAD_SESSION_TEST_EXE")
                .expect("set SAKURA_PAD_SESSION_TEST_EXE to the built absolute session image"),
        );
        assert!(image.is_absolute() && image.is_file());
        let isolated = IsolatedPad::new();
        let store = PadStore::at(&isolated.0);
        let mut original = PadDocument {
            generation: 1,
            ..PadDocument::default()
        };
        original
            .memos
            .push(PadMemo::new(1, "title", "private body", 1));
        store.write(&original).unwrap();

        let mut enrollment = engine(&image);
        let (abandoned, abandoned_key) = enrollment
            .prepare_recoverable_enroll(&store, &original, password())
            .unwrap();
        assert!(abandoned_key.starts_with("SPRK1-"));
        assert_eq!(store.load().unwrap().document, original);
        drop(abandoned);
        drop(abandoned_key);
        enrollment.lock();
        assert_eq!(store.load().unwrap().document, original);

        let (stale, stale_key) = enrollment
            .prepare_recoverable_enroll(&store, &original, password())
            .unwrap();
        let mut changed = original.clone();
        changed.generation += 1;
        store.write(&changed).unwrap();
        assert_eq!(
            enrollment.confirm_recoverable_enroll(&store, stale),
            Err(ProtectionError::new(
                FailurePhase::BeforeIntent,
                FailureReason::Stale
            ))
        );
        drop(stale_key);
        assert_eq!(store.load().unwrap().document, changed);

        let (prepared, recovery_key) = enrollment
            .prepare_recoverable_enroll(&store, &changed, password())
            .unwrap();
        assert_eq!(store.load().unwrap().document, changed);
        enrollment
            .confirm_recoverable_enroll(&store, prepared)
            .unwrap();
        enrollment.lock();
        assert!(matches!(store.load(), Err(StorageError::ProtectedCutover)));

        let mut wrong = engine(&image);
        let mut wrong_key = recovery_key.as_bytes().to_vec();
        wrong_key[6] = if wrong_key[6] == b'A' { b'B' } else { b'A' };
        let error = wrong
            .unlock_with_recovery(&store, SecretBytes::new(wrong_key))
            .unwrap_err();
        assert_eq!(error.reason, FailureReason::Authentication);
        let mut recovered = engine(&image);
        let loaded = recovered
            .unlock_with_recovery(&store, SecretBytes::new(recovery_key.as_bytes().to_vec()))
            .unwrap();
        assert_eq!(loaded.document, changed);
        assert!(!loaded.recovered_from_backup);
        let mut next = loaded.document.clone();
        next.generation += 1;
        next.memos
            .push(PadMemo::new(2, "second", "after recovery", 2));
        recovered.save(&store, &loaded.document, &next).unwrap();
        recovered.lock();

        let mut reopened = engine(&image);
        assert_eq!(reopened.unlock(&store, password()).unwrap().document, next);
        reopened.lock();
        let mut recovery_again = engine(&image);
        assert_eq!(
            recovery_again
                .unlock_with_recovery(&store, SecretBytes::new(recovery_key.as_bytes().to_vec()))
                .unwrap()
                .document,
            next
        );
        recovery_again.lock();

        // A replayed v1 primary must not let either credential silently open
        // the valid older v2 backup after authentication of the primary fails.
        let foreign = IsolatedPad::new();
        let foreign_store = PadStore::at(&foreign.0);
        let mut old_vault = engine(&image);
        let foreign_document = foreign_store.load().unwrap().document;
        old_vault
            .enroll(&foreign_store, &foreign_document, password())
            .unwrap();
        old_vault.lock();
        std::fs::copy(
            foreign.0.join("memo.v3.bin"),
            isolated.0.join("memo.v3.bin"),
        )
        .unwrap();
        assert!(isolated.0.join("memo.v3.bin.bak").exists());
        let mut replay = engine(&image);
        assert_eq!(
            replay.unlock(&store, password()).unwrap_err().reason,
            FailureReason::Authentication
        );
        assert_eq!(
            replay
                .unlock_with_recovery(&store, SecretBytes::new(recovery_key.as_bytes().to_vec()))
                .unwrap_err()
                .reason,
            FailureReason::Authentication
        );
    }

    #[test]
    #[ignore = "requires SAKURA_PAD_SESSION_TEST_EXE"]
    fn real_worker_v1_envelope_rejects_recovery_route() {
        let image = PathBuf::from(
            std::env::var_os("SAKURA_PAD_SESSION_TEST_EXE")
                .expect("set SAKURA_PAD_SESSION_TEST_EXE to the built absolute session image"),
        );
        assert!(image.is_absolute() && image.is_file());
        let isolated = IsolatedPad::new();
        let store = PadStore::at(&isolated.0);
        let original = store.load().unwrap().document;
        let mut enrollment = engine(&image);
        enrollment.enroll(&store, &original, password()).unwrap();
        enrollment.lock();
        let mut recovery = engine(&image);
        assert_eq!(
            recovery
                .unlock_with_recovery(&store, SecretBytes::new(b"SPRK1-invalid".to_vec()))
                .unwrap_err()
                .reason,
            FailureReason::Authentication
        );
        assert_eq!(
            recovery.unlock(&store, password()).unwrap().document,
            original
        );
    }

    #[test]
    #[ignore = "requires SAKURA_PAD_SESSION_TEST_EXE"]
    fn real_worker_whole_pad_and_memo_require_independent_passwords() {
        let image = PathBuf::from(
            std::env::var_os("SAKURA_PAD_SESSION_TEST_EXE")
                .expect("set SAKURA_PAD_SESSION_TEST_EXE to the built absolute session image"),
        );
        assert!(image.is_absolute() && image.is_file());
        let isolated = IsolatedPad::new();
        let store = PadStore::at(&isolated.0);
        let mut legacy = PadDocument {
            generation: 1,
            ..PadDocument::default()
        };
        legacy
            .memos
            .push(PadMemo::new(1, "private title", "private body", 1));
        legacy
            .memos
            .push(PadMemo::new(2, "ordinary", "still readable", 1));
        store.write(&legacy).unwrap();

        let mut outer = engine(&image);
        outer.enroll(&store, &legacy, password()).unwrap();
        outer.lock();
        let loaded = outer.unlock(&store, password()).unwrap();
        let vault_id = store.protected_vault_id().unwrap();
        let mut memo = MemoProtectionSession::new(vault_id, 1, Duration::from_secs(20))
            .unwrap()
            .with_worker_image(image.clone());
        let frame = MemoPayloadV1::encode("private title", "private body").unwrap();
        let envelope = memo
            .create(
                SecretBytes::new(b"independent memo password".to_vec()),
                SecretBytes::new(frame),
            )
            .unwrap();
        memo.lock();
        let mut next = loaded.document.clone();
        next.document_id = vault_id;
        next.generation += 1;
        next.find_mut(1)
            .unwrap()
            .protect_with_envelope(envelope.to_vec())
            .unwrap();
        assert_eq!(
            outer.save(&store, &loaded.document, &next).unwrap(),
            WriteOutcome::Replaced
        );
        outer.lock();

        let mut reopened_outer = engine(&image);
        let reopened = reopened_outer.unlock(&store, password()).unwrap();
        assert_eq!(reopened.document, next);
        assert!(reopened.document.find(1).unwrap().plain_content().is_none());
        assert_eq!(
            reopened.document.find(2).unwrap().plain_content(),
            Some(("ordinary", "still readable"))
        );
        reopened_outer.lock();

        let mut wrong_memo = MemoProtectionSession::new(vault_id, 1, Duration::from_secs(20))
            .unwrap()
            .with_worker_image(image.clone());
        assert!(wrong_memo
            .unlock(password(), SecretBytes::new(envelope.to_vec()),)
            .is_err());
        let mut right_memo = MemoProtectionSession::new(vault_id, 1, Duration::from_secs(20))
            .unwrap()
            .with_worker_image(image);
        let opened = right_memo
            .unlock(
                SecretBytes::new(b"independent memo password".to_vec()),
                envelope,
            )
            .unwrap();
        assert_eq!(
            MemoPayloadV1::decode(&opened).unwrap(),
            ("private title".to_owned(), "private body".to_owned())
        );
        right_memo.lock();
    }
}
