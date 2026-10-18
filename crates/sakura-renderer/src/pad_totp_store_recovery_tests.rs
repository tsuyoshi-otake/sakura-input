//! Recovery tests for interrupted `pad_totp_store` publications (#360 A5).
//!
//! Contract IDs used in doc comments:
//! - S1: a failed in-process publication never leaves a temp behind while the
//!   canonical file exists; the canonical file stays byte-identical.
//! - S2: a complete temp is never deleted; it is rolled forward so persisted
//!   replay and brute-force state are never rolled back.
//! - S3: the canonical-missing replace failure never loses the record and
//!   never turns an enabled gate into `Disabled`.
//! - S4: a foreign temp is reported `Corrupt` and nothing is deleted
//!   (fail closed).
//! - S5: recovery touches only `self.path` and `self.temp`, under the lock.
//! - L1: after one transient replace failure the store works again with no
//!   manual file deletion.
//! - L2: all existing `pad_totp_store` and `pad_totp` tests keep passing.
//!
//! T1–T6 use only APIs that exist at `0070110` so they can run against the
//! pre-fix code for fail-before evidence. T7–T8 drive the private
//! `resolve_replace_failure` helper directly because
//! `ERROR_UNABLE_TO_MOVE_REPLACEMENT` cannot be caused on demand.

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

const FILE_SHARE_READ_WRITE: u32 = 0x0001 | 0x0002;

struct DirGuard(PathBuf);
impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> (DirGuard, PadTotpStore) {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("sakura-totp-recovery-{}-{n}", std::process::id()));
    let store = PadTotpStore::new(&dir, [7; 16], TotpScope::Pad).unwrap();
    (DirGuard(dir), store)
}

/// Enroll and confirm at step 10 (unix 300 s); returns the raw secret.
fn enroll(store: &PadTotpStore) -> Vec<u8> {
    store.begin_enrollment().unwrap();
    let rec = store.read().unwrap().unwrap();
    let code = format!("{:06}", crate::pad_totp::hotp(&rec.secret, 10).unwrap());
    store.confirm_enrollment(&code, 300).unwrap();
    rec.secret.to_vec()
}

/// A six-digit code that does not match any of `steps`.
fn wrong_code(secret: &[u8], steps: &[u64]) -> String {
    let active: Vec<u32> = steps
        .iter()
        .map(|&step| crate::pad_totp::hotp(secret, step).unwrap())
        .collect();
    let wrong = (0..1_000_000).find(|c| !active.contains(c)).unwrap();
    format!("{wrong:06}")
}

/// T1 / S1 + L1: a transient `ReplaceFileW` sharing violation must not wedge
/// the store. The temp is deleted, the canonical record is untouched, and a
/// later valid code verifies with no manual cleanup.
#[test]
fn transient_replace_failure_leaves_store_usable() {
    let (_guard, store) = fixture();
    let secret = enroll(&store);
    let canonical_before = fs::read(&store.path).unwrap();

    // Hold the canonical file without FILE_SHARE_DELETE so ReplaceFileW fails.
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ_WRITE)
        .open(&store.path)
        .unwrap();
    let wrong = wrong_code(&secret, &[11, 12, 13]);
    let result = store.verify(&wrong, 360);
    assert!(matches!(result, Err(e) if !matches!(e, TotpStoreError::Verify(_))));
    drop(held);

    assert!(!store.temp.exists());
    assert_eq!(fs::read(&store.path).unwrap(), canonical_before);
    assert_eq!(store.status().unwrap(), EnrollmentStatus::Enabled);

    // Liveness: a fresh valid code at step 12 must unlock.
    let code = format!("{:06}", crate::pad_totp::hotp(&secret, 12).unwrap());
    store.verify(&code, 360).unwrap();
    assert_eq!(
        store.read().unwrap().unwrap().state.last_accepted_step,
        Some(12)
    );
}

/// T2 / S2: a complete leftover temp with the canonical file present is
/// rolled forward; the accepted step is never rolled back.
#[test]
fn complete_temp_with_canonical_rolls_forward() {
    let (_guard, store) = fixture();
    let secret = enroll(&store);
    let s1 = fs::read(&store.path).unwrap();
    let code = format!("{:06}", crate::pad_totp::hotp(&secret, 11).unwrap());
    store.verify(&code, 330).unwrap();
    let s2 = fs::read(&store.path).unwrap();
    assert_ne!(s1, s2);

    // Simulate a crash after the temp was synced but before the replace:
    // canonical = S1, temp = S2.
    fs::write(&store.temp, &s2).unwrap();
    fs::write(&store.path, &s1).unwrap();

    let record = store.read().unwrap().unwrap();
    assert_eq!(record.state.last_accepted_step, Some(11));
    assert_eq!(fs::read(&store.path).unwrap(), s2);
    assert!(!store.temp.exists());
}

/// T3 / S2 + S3: a complete leftover temp with no canonical file is moved
/// into place; the enabled gate is preserved.
#[test]
fn complete_temp_without_canonical_rolls_forward() {
    let (_guard, store) = fixture();
    enroll(&store);
    let canonical = fs::read(&store.path).unwrap();
    fs::rename(&store.path, &store.temp).unwrap();

    assert_eq!(store.status().unwrap(), EnrollmentStatus::Enabled);
    assert_eq!(fs::read(&store.path).unwrap(), canonical);
    assert!(!store.temp.exists());
}

/// T4: a torn temp with the canonical file present is deleted and the
/// canonical record is returned.
#[test]
fn torn_temp_with_canonical_is_deleted() {
    let (_guard, store) = fixture();
    enroll(&store);
    fs::write(&store.temp, b"garbage").unwrap();

    let record = store.read().unwrap().unwrap();
    assert_eq!(record.state.last_accepted_step, Some(10));
    assert!(!store.temp.exists());
}

/// T5: a torn temp with no canonical file means no record was ever
/// published; it is deleted and the store reports `Disabled`.
#[test]
fn torn_temp_without_canonical_reports_disabled() {
    let (guard, store) = fixture();
    fs::create_dir_all(&guard.0).unwrap();
    fs::write(&store.temp, b"garbage").unwrap();

    assert_eq!(store.status().unwrap(), EnrollmentStatus::Disabled);
    assert!(!store.temp.exists());
}

/// T6 / S4: a foreign temp (valid DPAPI record bound to another document ID
/// and scope) fails closed and nothing is deleted or replaced.
#[test]
fn foreign_temp_fails_closed_and_is_kept() {
    let (_guard, store) = fixture();
    enroll(&store);
    let canonical_before = fs::read(&store.path).unwrap();
    let other_dir = std::env::temp_dir().join(format!(
        "sakura-totp-recovery-foreign-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _other_guard = DirGuard(other_dir.clone());
    let other = PadTotpStore::new(&other_dir, [8; 16], TotpScope::Memo(2)).unwrap();
    other.begin_enrollment().unwrap();
    let foreign = fs::read(&other.path).unwrap();
    fs::write(&store.temp, &foreign).unwrap();

    assert!(matches!(store.status(), Err(TotpStoreError::Corrupt)));
    assert_eq!(fs::read(&store.path).unwrap(), canonical_before);
    assert_eq!(fs::read(&store.temp).unwrap(), foreign);
}

/// T7 / S3: after a replace failure with the canonical file gone (the
/// `ERROR_UNABLE_TO_MOVE_REPLACEMENT` disk state), the complete temp is
/// rolled forward instead of deleted.
#[test]
fn replace_failure_with_missing_canonical_rolls_forward() {
    let (_guard, store) = fixture();
    enroll(&store);
    let canonical = fs::read(&store.path).unwrap();
    fs::rename(&store.path, &store.temp).unwrap();

    store
        .resolve_replace_failure(TotpStoreError::Corrupt)
        .unwrap();
    assert_eq!(fs::read(&store.path).unwrap(), canonical);
    assert!(!store.temp.exists());
    assert_eq!(store.status().unwrap(), EnrollmentStatus::Enabled);
}

/// T8 / S1: after a replace failure with the canonical file present, the
/// temp is deleted and the canonical record is byte-identical.
#[test]
fn replace_failure_with_canonical_deletes_temp() {
    let (_guard, store) = fixture();
    enroll(&store);
    let canonical = fs::read(&store.path).unwrap();
    fs::write(&store.temp, &canonical).unwrap();

    assert!(matches!(
        store.resolve_replace_failure(TotpStoreError::Corrupt),
        Err(TotpStoreError::Corrupt)
    ));
    assert!(!store.temp.exists());
    assert_eq!(fs::read(&store.path).unwrap(), canonical);
}
