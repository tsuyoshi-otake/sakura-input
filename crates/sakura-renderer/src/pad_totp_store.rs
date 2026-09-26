//! Durable, current-user DPAPI sidecar for optional offline Pad confirmation.
//!
//! This is a UI gate after the password unlock, never a decryption key or an
//! anti-tamper boundary. A user able to replace/delete local files or change
//! this process can bypass it. The caller must bind this API to the verified
//! protected document ID, and must reauthenticate before disabling it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH, REPLACE_FILE_FLAGS,
};
use zeroize::Zeroizing;

use crate::pad_totp::{new_secret, PadTotp, TotpError, TotpState, SECRET_LEN};

const MAGIC: &[u8; 8] = b"SKRTOTP1";
const VERSION: u16 = 1;
const MAX_FILE: u64 = 4096;
const RECORD_LEN: usize = 8 + 2 + 16 + 1 + 8 + 1 + SECRET_LEN + 1 + 8 + 1 + 8 + 1 + 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TotpScope {
    Pad,
    Memo(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnrollmentStatus {
    Disabled,
    Pending,
    Enabled,
}

#[derive(Debug)]
pub enum TotpStoreError {
    InvalidBinding,
    InvalidState,
    Corrupt,
    Busy,
    Io(io::Error),
    Crypto,
    Verify(TotpError),
}

impl From<io::Error> for TotpStoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl std::fmt::Display for TotpStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "TOTP storage I/O failed: {error}"),
            Self::Verify(error) => write!(f, "TOTP verification failed: {error:?}"),
            other => write!(f, "TOTP operation failed: {other:?}"),
        }
    }
}

/// An immutable binding; construct with the verified Pad document ID.
#[derive(Debug)]
pub struct PadTotpStore {
    path: PathBuf,
    temp: PathBuf,
    lock: PathBuf,
    document_id: [u8; 16],
    scope: TotpScope,
}

struct Record {
    status: EnrollmentStatus,
    secret: Zeroizing<Vec<u8>>,
    state: TotpState,
}

impl PadTotpStore {
    pub fn new(
        dir: &Path,
        document_id: [u8; 16],
        scope: TotpScope,
    ) -> Result<Self, TotpStoreError> {
        if document_id == [0; 16] || matches!(scope, TotpScope::Memo(0)) {
            return Err(TotpStoreError::InvalidBinding);
        }
        let id = document_id
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let name = match scope {
            TotpScope::Pad => format!("pad-totp-{id}-pad.v1"),
            TotpScope::Memo(memo) => format!("pad-totp-{id}-memo-{memo:016x}.v1"),
        };
        let path = dir.join(name);
        let temp = append_suffix(&path, ".tmp");
        let lock = append_suffix(&path, ".lock");
        Ok(Self {
            path,
            temp,
            lock,
            document_id,
            scope,
        })
    }

    /// Corruption and inaccessible state are errors, never interpreted as off.
    pub fn status(&self) -> Result<EnrollmentStatus, TotpStoreError> {
        let _guard = self.lock()?;
        Ok(self
            .read()?
            .map_or(EnrollmentStatus::Disabled, |r| r.status))
    }

    /// Persist a pending enrollment and return its one-time Base32 secret.
    /// The caller must show it only during setup and avoid copying it into
    /// long-lived UI state. Calling again replaces an interrupted Pending
    /// enrollment with a fresh secret; Enabled enrollment is never replaced.
    pub fn begin_enrollment(&self) -> Result<Zeroizing<String>, TotpStoreError> {
        let _guard = self.lock()?;
        if self
            .read()?
            .is_some_and(|r| r.status == EnrollmentStatus::Enabled)
        {
            return Err(TotpStoreError::InvalidState);
        }
        let secret = new_secret().map_err(TotpStoreError::Verify)?;
        let provisioning_secret = base32(&secret);
        self.publish(&Record {
            status: EnrollmentStatus::Pending,
            secret,
            state: TotpState::default(),
        })?;
        Ok(provisioning_secret)
    }

    /// Cancel only a Pending setup. The disabled tombstone retires its secret
    /// before the caller dismisses the setup UI.
    pub fn cancel_enrollment(&self) -> Result<(), TotpStoreError> {
        let _guard = self.lock()?;
        let mut record = self.read()?.ok_or(TotpStoreError::InvalidState)?;
        if record.status != EnrollmentStatus::Pending {
            return Err(TotpStoreError::InvalidState);
        }
        record.status = EnrollmentStatus::Disabled;
        record.secret.fill(0);
        record.state = TotpState::default();
        self.publish(&record)
    }

    /// Confirmation is required before any unlock gate becomes active.
    pub fn confirm_enrollment(&self, code: &str, unix_seconds: u64) -> Result<(), TotpStoreError> {
        self.verify_transition(code, unix_seconds, EnrollmentStatus::Pending, true)
    }

    /// A successful step is durably published before this grants UI access.
    pub fn verify(&self, code: &str, unix_seconds: u64) -> Result<(), TotpStoreError> {
        self.verify_transition(code, unix_seconds, EnrollmentStatus::Enabled, false)
    }

    /// Caller must first reauthenticate the Pad's selected factor(s). This consumes
    /// a fresh TOTP step before publishing a disabled tombstone.
    pub fn disable_after_reauthentication(
        &self,
        code: &str,
        unix_seconds: u64,
    ) -> Result<(), TotpStoreError> {
        let _guard = self.lock()?;
        let mut record = self.read()?.ok_or(TotpStoreError::InvalidState)?;
        if record.status != EnrollmentStatus::Enabled {
            return Err(TotpStoreError::InvalidState);
        }
        let result = verify_record(&mut record, code, unix_seconds);
        if result.is_ok() {
            record.status = EnrollmentStatus::Disabled;
            record.secret.fill(0);
            record.state = TotpState::default();
        }
        self.publish(&record)?;
        result.map_err(TotpStoreError::Verify)
    }

    fn verify_transition(
        &self,
        code: &str,
        unix_seconds: u64,
        required: EnrollmentStatus,
        enable: bool,
    ) -> Result<(), TotpStoreError> {
        let _guard = self.lock()?;
        let mut record = self.read()?.ok_or(TotpStoreError::InvalidState)?;
        if record.status != required {
            return Err(TotpStoreError::InvalidState);
        }
        let result = verify_record(&mut record, code, unix_seconds);
        if result.is_ok() && enable {
            record.status = EnrollmentStatus::Enabled;
        }
        // Failure state, clock floor, and accepted step all have the same
        // publication obligation. A failed publication cannot grant access.
        self.publish(&record)?;
        result.map_err(TotpStoreError::Verify)
    }

    fn lock(&self) -> Result<File, TotpStoreError> {
        fs::create_dir_all(self.path.parent().ok_or(TotpStoreError::InvalidBinding)?)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&self.lock)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    TotpStoreError::Busy
                } else {
                    TotpStoreError::Io(e)
                }
            })
    }

    fn read(&self) -> Result<Option<Record>, TotpStoreError> {
        // Any interrupted publication needs explicit recovery; never silently
        // use an older accepted-step snapshot while a temp is present.
        if self.temp.exists() {
            return Err(TotpStoreError::Corrupt);
        }
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if file.metadata()?.len() > MAX_FILE {
            return Err(TotpStoreError::Corrupt);
        }
        let mut encrypted = Vec::new();
        file.read_to_end(&mut encrypted)?;
        let clear = unprotect(&encrypted)?;
        self.decode(&clear).map(Some)
    }

    fn decode(&self, clear: &[u8]) -> Result<Record, TotpStoreError> {
        if clear.len() != RECORD_LEN
            || &clear[..8] != MAGIC
            || u16::from_le_bytes(clear[8..10].try_into().unwrap()) != VERSION
            || clear[10..26] != self.document_id
        {
            return Err(TotpStoreError::Corrupt);
        }
        let scope = match clear[26] {
            0 if clear[27..35] == [0; 8] => TotpScope::Pad,
            1 => TotpScope::Memo(u64::from_le_bytes(clear[27..35].try_into().unwrap())),
            _ => return Err(TotpStoreError::Corrupt),
        };
        if scope != self.scope {
            return Err(TotpStoreError::Corrupt);
        }
        let status = match clear[35] {
            0 => EnrollmentStatus::Disabled,
            1 => EnrollmentStatus::Pending,
            2 => EnrollmentStatus::Enabled,
            _ => return Err(TotpStoreError::Corrupt),
        };
        let secret = Zeroizing::new(clear[36..56].to_vec());
        let last_accepted_step = option_u64(clear[56], &clear[57..65])?;
        let greatest_observed_step = option_u64(clear[65], &clear[66..74])?;
        let failures = clear[74];
        let blocked_until_ms = u64::from_le_bytes(clear[75..83].try_into().unwrap());
        let state = TotpState {
            last_accepted_step,
            greatest_observed_step,
            failures,
            blocked_until_ms,
        };
        PadTotp::from_state(secret.to_vec(), state).map_err(|_| TotpStoreError::Corrupt)?;
        Ok(Record {
            status,
            secret,
            state,
        })
    }

    fn publish(&self, record: &Record) -> Result<(), TotpStoreError> {
        let mut clear = Zeroizing::new(Vec::with_capacity(RECORD_LEN));
        clear.extend_from_slice(MAGIC);
        clear.extend_from_slice(&VERSION.to_le_bytes());
        clear.extend_from_slice(&self.document_id);
        match self.scope {
            TotpScope::Pad => {
                clear.push(0);
                clear.extend_from_slice(&0_u64.to_le_bytes());
            }
            TotpScope::Memo(id) => {
                clear.push(1);
                clear.extend_from_slice(&id.to_le_bytes());
            }
        }
        clear.push(match record.status {
            EnrollmentStatus::Disabled => 0,
            EnrollmentStatus::Pending => 1,
            EnrollmentStatus::Enabled => 2,
        });
        clear.extend_from_slice(&record.secret);
        put_option_u64(&mut clear, record.state.last_accepted_step);
        put_option_u64(&mut clear, record.state.greatest_observed_step);
        clear.push(record.state.failures);
        clear.extend_from_slice(&record.state.blocked_until_ms.to_le_bytes());
        let encrypted = protect(&clear)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.temp)?;
        file.write_all(&encrypted)?;
        file.sync_all()?;
        drop(file);
        let temp = wide(&self.temp);
        let target = wide(&self.path);
        if self.path.exists() {
            // SAFETY: both NUL-terminated buffers remain live during the call.
            unsafe {
                ReplaceFileW(
                    windows_core::PCWSTR(target.as_ptr()),
                    windows_core::PCWSTR(temp.as_ptr()),
                    windows_core::PCWSTR::null(),
                    REPLACE_FILE_FLAGS(0),
                    None,
                    None,
                )
            }
            .map_err(|_| TotpStoreError::Corrupt)?;
        } else {
            // SAFETY: both NUL-terminated buffers remain live during the call.
            unsafe {
                MoveFileExW(
                    windows_core::PCWSTR(temp.as_ptr()),
                    windows_core::PCWSTR(target.as_ptr()),
                    MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(|_| TotpStoreError::Corrupt)?;
        }
        Ok(())
    }
}

fn verify_record(record: &mut Record, code: &str, unix_seconds: u64) -> Result<(), TotpError> {
    let mut verifier = PadTotp::from_state(record.secret.to_vec(), record.state)?;
    let result = verifier.verify(code, unix_seconds, unix_seconds.saturating_mul(1000));
    record.state = verifier.state();
    result
}

fn option_u64(flag: u8, bytes: &[u8]) -> Result<Option<u64>, TotpStoreError> {
    let value = u64::from_le_bytes(bytes.try_into().unwrap());
    match flag {
        0 if value == 0 => Ok(None),
        1 => Ok(Some(value)),
        _ => Err(TotpStoreError::Corrupt),
    }
}
fn put_option_u64(out: &mut Vec<u8>, value: Option<u64>) {
    out.push(u8::from(value.is_some()));
    out.extend_from_slice(&value.unwrap_or(0).to_le_bytes());
}
fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn base32(bytes: &[u8]) -> Zeroizing<String> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = Zeroizing::new(String::new());
    let mut bits = 0_u32;
    let mut count = 0_u8;
    for &byte in bytes {
        bits = (bits << 8) | u32::from(byte);
        count += 8;
        while count >= 5 {
            count -= 5;
            out.push(ALPHABET[((bits >> count) & 31) as usize] as char);
        }
    }
    if count > 0 {
        out.push(ALPHABET[((bits << (5 - count)) & 31) as usize] as char);
    }
    out
}

fn protect(clear: &[u8]) -> Result<Vec<u8>, TotpStoreError> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: clear.len() as u32,
        pbData: clear.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input is live and DPAPI owns the output until LocalFree.
    unsafe {
        CryptProtectData(
            &input,
            windows_core::PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|_| TotpStoreError::Crypto)?;
    take_blob(&mut output)
}
fn unprotect(ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, TotpStoreError> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: ciphertext.len() as u32,
        pbData: ciphertext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input is live and DPAPI owns the output until LocalFree.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|_| TotpStoreError::Corrupt)?;
    take_blob(&mut output).map(Zeroizing::new)
}
fn take_blob(blob: &mut CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, TotpStoreError> {
    let result = if blob.cbData as u64 > MAX_FILE || blob.pbData.is_null() {
        Err(TotpStoreError::Corrupt)
    } else {
        // SAFETY: DPAPI allocated cbData bytes at this pointer.
        Ok(unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec())
    };
    // SAFETY: DPAPI allocated the blob with LocalAlloc.
    unsafe {
        if !blob.pbData.is_null() {
            std::ptr::write_bytes(blob.pbData, 0, blob.cbData as usize);
            let _ = LocalFree(Some(HLOCAL(blob.pbData.cast())));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    fn fixture() -> (PathBuf, PadTotpStore) {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("sakura-totp-{}-{n}", std::process::id()));
        let store = PadTotpStore::new(&dir, [7; 16], TotpScope::Pad).unwrap();
        (dir, store)
    }
    fn enroll(store: &PadTotpStore) -> Zeroizing<String> {
        let secret = store.begin_enrollment().unwrap();
        let rec = store.read().unwrap().unwrap();
        let code = format!("{:06}", crate::pad_totp::hotp(&rec.secret, 10).unwrap());
        store.confirm_enrollment(&code, 300).unwrap();
        secret
    }
    #[test]
    fn enrollment_requires_confirmation_and_replay_survives_restart() {
        let (dir, store) = fixture();
        assert_eq!(store.status().unwrap(), EnrollmentStatus::Disabled);
        let secret = enroll(&store);
        assert_eq!(secret.len(), 32);
        let reopened = PadTotpStore::new(&dir, [7; 16], TotpScope::Pad).unwrap();
        assert_eq!(reopened.status().unwrap(), EnrollmentStatus::Enabled);
        let rec = reopened.read().unwrap().unwrap();
        let same = format!("{:06}", crate::pad_totp::hotp(&rec.secret, 10).unwrap());
        assert!(matches!(
            reopened.verify(&same, 300),
            Err(TotpStoreError::Verify(TotpError::InvalidCode))
        ));
        let reopened = PadTotpStore::new(&dir, [7; 16], TotpScope::Pad).unwrap();
        let next = format!("{:06}", crate::pad_totp::hotp(&rec.secret, 11).unwrap());
        assert!(matches!(
            reopened.verify(&same, 300),
            Err(TotpStoreError::Verify(TotpError::RateLimited))
        ));
        reopened.verify(&next, 330).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn wrong_code_rollback_and_corruption_fail_closed() {
        let (dir, store) = fixture();
        enroll(&store);
        let rec = store.read().unwrap().unwrap();
        let active = [11, 12, 13].map(|step| crate::pad_totp::hotp(&rec.secret, step).unwrap());
        let wrong = (0..1_000_000)
            .find(|candidate| !active.contains(candidate))
            .unwrap();
        let wrong = format!("{wrong:06}");
        assert!(matches!(
            store.verify(&wrong, 360),
            Err(TotpStoreError::Verify(TotpError::InvalidCode))
        ));
        assert!(matches!(
            store.verify("000000", 360),
            Err(TotpStoreError::Verify(TotpError::RateLimited))
        ));
        assert!(matches!(
            store.verify("000000", 330),
            Err(TotpStoreError::Verify(TotpError::ClockRollback))
        ));
        fs::write(&store.path, b"corrupt").unwrap();
        assert!(matches!(store.status(), Err(TotpStoreError::Corrupt)));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn binding_rejects_swapped_record() {
        let (dir, store) = fixture();
        enroll(&store);
        let other = PadTotpStore::new(&dir, [8; 16], TotpScope::Memo(2)).unwrap();
        fs::copy(&store.path, &other.path).unwrap();
        assert!(matches!(other.status(), Err(TotpStoreError::Corrupt)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disable_consumes_fresh_step_and_retains_tombstone() {
        let (dir, store) = fixture();
        enroll(&store);
        let rec = store.read().unwrap().unwrap();
        let next = format!("{:06}", crate::pad_totp::hotp(&rec.secret, 11).unwrap());
        store.disable_after_reauthentication(&next, 330).unwrap();
        let reopened = PadTotpStore::new(&dir, [7; 16], TotpScope::Pad).unwrap();
        assert_eq!(reopened.status().unwrap(), EnrollmentStatus::Disabled);
        assert!(matches!(
            reopened.verify(&next, 330),
            Err(TotpStoreError::InvalidState)
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_setup_can_be_cancelled_but_enabled_gate_cannot() {
        let (dir, store) = fixture();
        let _secret = store.begin_enrollment().unwrap();
        assert_eq!(store.status().unwrap(), EnrollmentStatus::Pending);
        store.cancel_enrollment().unwrap();
        let reopened = PadTotpStore::new(&dir, [7; 16], TotpScope::Pad).unwrap();
        assert_eq!(reopened.status().unwrap(), EnrollmentStatus::Disabled);
        assert!(reopened
            .read()
            .unwrap()
            .unwrap()
            .secret
            .iter()
            .all(|&b| b == 0));
        assert!(matches!(
            reopened.confirm_enrollment("000000", 300),
            Err(TotpStoreError::InvalidState)
        ));
        enroll(&reopened);
        assert!(matches!(
            reopened.cancel_enrollment(),
            Err(TotpStoreError::InvalidState)
        ));
        assert_eq!(reopened.status().unwrap(), EnrollmentStatus::Enabled);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restart_replaces_pending_secret_and_requires_new_confirmation() {
        let (dir, store) = fixture();
        let first_base32 = store.begin_enrollment().unwrap();
        let first = store.read().unwrap().unwrap();
        let first_code = format!("{:06}", crate::pad_totp::hotp(&first.secret, 10).unwrap());
        drop(store);

        let reopened = PadTotpStore::new(&dir, [7; 16], TotpScope::Pad).unwrap();
        assert_eq!(reopened.status().unwrap(), EnrollmentStatus::Pending);
        let second_base32 = reopened.begin_enrollment().unwrap();
        let second = reopened.read().unwrap().unwrap();
        assert_ne!(first_base32.as_str(), second_base32.as_str());
        assert_ne!(first.secret.as_slice(), second.secret.as_slice());
        assert_eq!(reopened.status().unwrap(), EnrollmentStatus::Pending);
        assert!(matches!(
            reopened.verify(&first_code, 300),
            Err(TotpStoreError::InvalidState)
        ));
        let second_code = format!("{:06}", crate::pad_totp::hotp(&second.secret, 10).unwrap());
        reopened.confirm_enrollment(&second_code, 300).unwrap();
        assert_eq!(reopened.status().unwrap(), EnrollmentStatus::Enabled);
        assert!(matches!(
            reopened.begin_enrollment(),
            Err(TotpStoreError::InvalidState)
        ));
        fs::remove_dir_all(dir).unwrap();
    }
}
