//! Optional local-confirmation TOTP primitive for Sakura Pad.
//!
//! This module supplies a local confirmation-code gate for whole-Pad unlock.
//! It does not protect encrypted data by itself.
//! In particular, offline TOTP cannot establish a trusted clock or prevent an
//! attacker who can modify this process or its local state from bypassing it.

use std::fmt;

use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptGenRandom, BCryptHashData, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE,
    BCRYPT_ALG_HANDLE_HMAC_FLAG, BCRYPT_HASH_HANDLE, BCRYPT_HASH_LENGTH, BCRYPT_OBJECT_LENGTH,
    BCRYPT_SHA1_ALGORITHM, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};
use windows_core::PCWSTR;
use zeroize::Zeroizing;

const STEP_SECONDS: u64 = 30;
const DIGITS: u32 = 6;
const MIN_SECRET_LEN: usize = 16;
const MAX_SECRET_LEN: usize = 128;
const MAX_FAILURES: u8 = 8;
const MAX_BACKOFF_MS: u64 = 60_000;
pub(crate) const SECRET_LEN: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TotpError {
    InvalidSecret,
    InvalidCode,
    ClockRollback,
    RateLimited,
    Crypto,
}

/// Persist this whole value after every verification attempt, including failures.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TotpState {
    pub last_accepted_step: Option<u64>,
    pub greatest_observed_step: Option<u64>,
    pub failures: u8,
    pub blocked_until_ms: u64,
}

pub(crate) fn new_secret() -> Result<Zeroizing<Vec<u8>>, TotpError> {
    let mut secret = Zeroizing::new(vec![0; SECRET_LEN]);
    // SAFETY: BCryptGenRandom fills only this live output buffer.
    if unsafe { BCryptGenRandom(None, &mut secret, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.0 < 0 {
        return Err(TotpError::Crypto);
    }
    Ok(secret)
}

/// A TOTP authenticator whose secret is cleared when dropped.
pub struct PadTotp {
    secret: Zeroizing<Vec<u8>>,
    last_accepted_step: Option<u64>,
    greatest_observed_step: Option<u64>,
    failures: u8,
    blocked_until_ms: u64,
}

impl fmt::Debug for PadTotp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PadTotp")
            .field("secret", &"[REDACTED]")
            .field("last_accepted_step", &self.last_accepted_step)
            .field("failures", &self.failures)
            .finish()
    }
}

impl PadTotp {
    /// Creates a local verifier from a 16–128 byte shared secret.
    pub fn new(secret: Vec<u8>) -> Result<Self, TotpError> {
        let secret = Zeroizing::new(secret);
        if !(MIN_SECRET_LEN..=MAX_SECRET_LEN).contains(&secret.len()) {
            return Err(TotpError::InvalidSecret);
        }
        Ok(Self {
            secret,
            last_accepted_step: None,
            greatest_observed_step: None,
            failures: 0,
            blocked_until_ms: 0,
        })
    }

    pub(crate) fn from_state(secret: Vec<u8>, state: TotpState) -> Result<Self, TotpError> {
        if state.failures > MAX_FAILURES
            || state.last_accepted_step.is_some_and(|last| {
                state
                    .greatest_observed_step
                    .is_none_or(|floor| last > floor.saturating_add(1))
            })
            || (state.failures == 0 && state.blocked_until_ms != 0)
        {
            return Err(TotpError::InvalidSecret);
        }
        let mut verifier = Self::new(secret)?;
        verifier.last_accepted_step = state.last_accepted_step;
        verifier.greatest_observed_step = state.greatest_observed_step;
        verifier.failures = state.failures;
        verifier.blocked_until_ms = state.blocked_until_ms;
        Ok(verifier)
    }

    pub(crate) fn state(&self) -> TotpState {
        TotpState {
            last_accepted_step: self.last_accepted_step,
            greatest_observed_step: self.greatest_observed_step,
            failures: self.failures,
            blocked_until_ms: self.blocked_until_ms,
        }
    }

    /// Verify six ASCII digits against the current 30-second step and its
    /// immediate neighbors. Time inputs are supplied by the caller in Unix
    /// seconds and Unix milliseconds respectively. Wall time is required so
    /// a persisted backoff remains effective after a process restart.
    pub fn verify(&mut self, code: &str, unix_seconds: u64, unix_ms: u64) -> Result<(), TotpError> {
        let current = unix_seconds / STEP_SECONDS;
        if self
            .greatest_observed_step
            .is_some_and(|greatest| current < greatest)
        {
            return self.failed(TotpError::ClockRollback, unix_ms);
        }
        self.greatest_observed_step = Some(current);
        if unix_ms < self.blocked_until_ms {
            return Err(TotpError::RateLimited);
        }
        if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return self.failed(TotpError::InvalidCode, unix_ms);
        }

        let supplied = code.as_bytes();
        let mut matched = None;
        for step in [
            current.saturating_sub(1),
            current,
            current.saturating_add(1),
        ] {
            let candidate = hotp(&self.secret, step)?;
            let bytes = format!("{candidate:06}");
            if constant_time_eq(supplied, bytes.as_bytes()) {
                matched = Some(step);
            }
        }
        let Some(step) = matched else {
            return self.failed(TotpError::InvalidCode, unix_ms);
        };
        if self.last_accepted_step.is_some_and(|last| step <= last) {
            return self.failed(TotpError::InvalidCode, unix_ms);
        }
        self.last_accepted_step = Some(step);
        self.failures = 0;
        self.blocked_until_ms = 0;
        Ok(())
    }

    fn failed<T>(&mut self, error: TotpError, unix_ms: u64) -> Result<T, TotpError> {
        self.failures = self.failures.saturating_add(1).min(MAX_FAILURES);
        let shift = u32::from(self.failures.saturating_sub(1)).min(6);
        let delay = (1_000_u64 << shift).min(MAX_BACKOFF_MS);
        self.blocked_until_ms = self.blocked_until_ms.max(unix_ms.saturating_add(delay));
        Err(error)
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

pub(crate) fn hotp(secret: &[u8], counter: u64) -> Result<u32, TotpError> {
    let counter = counter.to_be_bytes();
    let digest = hmac_sha1(secret, &counter)?;
    let offset = usize::from(digest[19] & 0x0f);
    let binary = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    Ok(binary % 10_u32.pow(DIGITS))
}

fn hmac_sha1(key: &[u8], message: &[u8]) -> Result<Zeroizing<[u8; 20]>, TotpError> {
    let mut algorithm = BCRYPT_ALG_HANDLE::default();
    // SAFETY: static algorithm identifier and initialized output handle.
    let status = unsafe {
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA1_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_ALG_HANDLE_HMAC_FLAG,
        )
    };
    if status.0 < 0 {
        return Err(TotpError::Crypto);
    }

    let result = (|| {
        let object_len = bcrypt_property(algorithm, BCRYPT_OBJECT_LENGTH)? as usize;
        let digest_len = bcrypt_property(algorithm, BCRYPT_HASH_LENGTH)? as usize;
        if object_len == 0 || object_len > 1024 * 1024 || digest_len != 20 {
            return Err(TotpError::Crypto);
        }
        let mut object = Zeroizing::new(vec![0_u8; object_len]);
        let mut hash = BCRYPT_HASH_HANDLE::default();
        // SAFETY: object and key remain live until hash destruction.
        let status =
            unsafe { BCryptCreateHash(algorithm, &mut hash, Some(&mut object), Some(key), 0) };
        if status.0 < 0 {
            return Err(TotpError::Crypto);
        }
        let hash_result = (|| {
            // SAFETY: live hash and borrowed message for synchronous call.
            if unsafe { BCryptHashData(hash, message, 0) }.0 < 0 {
                return Err(TotpError::Crypto);
            }
            let mut digest = Zeroizing::new([0_u8; 20]);
            // SAFETY: output size matches SHA-1 digest length.
            if unsafe { BCryptFinishHash(hash, &mut *digest, 0) }.0 < 0 {
                return Err(TotpError::Crypto);
            }
            Ok(digest)
        })();
        // SAFETY: this function owns the successfully created hash.
        unsafe {
            let _ = BCryptDestroyHash(hash);
        }
        hash_result
    })();
    // SAFETY: this function owns the successfully opened provider.
    unsafe {
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
    }
    result
}

fn bcrypt_property(algorithm: BCRYPT_ALG_HANDLE, property: PCWSTR) -> Result<u32, TotpError> {
    use windows::Win32::Security::Cryptography::BCryptGetProperty;
    let mut value = [0_u8; 4];
    let mut written = 0_u32;
    // SAFETY: provider is live and the result buffer is exactly four bytes.
    if unsafe {
        BCryptGetProperty(
            algorithm.into(),
            property,
            Some(&mut value),
            &mut written,
            0,
        )
    }
    .0 < 0
        || written != value.len() as u32
    {
        return Err(TotpError::Crypto);
    }
    Ok(u32::from_le_bytes(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 SHA-1 secret at Unix time 59 produces 94287082 (8 digits).
    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn matches_rfc6238_sha1_vector_as_six_digits() {
        assert_eq!(hotp(RFC_SECRET, 1).unwrap(), 287_082);
    }

    #[test]
    fn accepts_adjacent_step_and_rejects_replay() {
        let mut verifier = PadTotp::new(RFC_SECRET.to_vec()).unwrap();
        assert_eq!(verifier.verify("287082", 59, 0), Ok(()));
        assert_eq!(
            verifier.verify("287082", 60, 1_001),
            Err(TotpError::InvalidCode)
        );
    }

    #[test]
    fn rejects_clock_rollback_and_applies_bounded_backoff() {
        let mut verifier = PadTotp::new(RFC_SECRET.to_vec()).unwrap();
        assert_eq!(
            verifier.verify("000000", 90, 0),
            Err(TotpError::InvalidCode)
        );
        assert_eq!(
            verifier.verify("000000", 60, 1_000),
            Err(TotpError::ClockRollback)
        );
        assert_eq!(
            verifier.verify("000000", 90, 2_999),
            Err(TotpError::RateLimited)
        );
        assert_eq!(
            verifier.verify("000000", 90, 3_000),
            Err(TotpError::InvalidCode)
        );
        assert!(verifier.blocked_until_ms <= 63_000);
    }

    #[test]
    fn validates_code_shape() {
        let mut verifier = PadTotp::new(RFC_SECRET.to_vec()).unwrap();
        assert_eq!(
            verifier.verify("１２３４５６", 30, 0),
            Err(TotpError::InvalidCode)
        );
        assert_eq!(
            PadTotp::new(vec![0; 15]).err(),
            Some(TotpError::InvalidSecret)
        );
    }
}
