//! Bounded password envelope using Argon2id and AES-256-GCM.
//!
//! The format stores a randomly generated data-encryption key encrypted under
//! a password-derived key. Payload encryption uses that random key and a
//! separate nonce. All format metadata is authenticated by both operations.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Block, Params, Version};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"SKRPENV1";
const VERSION: u8 = 1;
const CIPHER_AES_256_GCM: u8 = 1;
const KDF_ARGON2ID: u8 = 1;
const SCOPE_VAULT: u8 = 1;
const SCOPE_MEMO: u8 = 2;
const PURPOSE_DEK_WRAP: &[u8] = b"Sakura Pad DEK wrap v1";
const PURPOSE_CONTENT: &[u8] = b"Sakura Pad content v1";

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const WRAPPED_KEY_LEN: usize = KEY_LEN + TAG_LEN;

// Header offsets are kept in one place so parsing stays fixed-width and
// rejects unsupported KDF settings before allocating KDF memory.
const HEADER_LEN: usize = 89;
const WRAPPED_KEY_OFFSET: usize = HEADER_LEN;
const PAYLOAD_OFFSET: usize = WRAPPED_KEY_OFFSET + WRAPPED_KEY_LEN;

const KDF_MEMORY_KIB: u32 = 64 * 1024;
const KDF_ITERATIONS: u32 = 3;
const KDF_PARALLELISM: u32 = 1;
const KDF_BLOCKS: usize = KDF_MEMORY_KIB as usize;
const MAX_PASSWORD_BYTES: usize = 1024;

// V2 is deliberately a separate API. Policy 1 means either the password or
// the recovery key can open the envelope. Unknown policies (including a
// future AND-factor policy) fail before the password KDF runs.
const V2_MAGIC: &[u8; 8] = b"SKRPENV2";
const V2_VERSION: u8 = 2;
const V2_POLICY_EITHER: u8 = 1;
const V2_HEADER_LEN: usize = 102;
const V2_POLICY_OFFSET: usize = 89;
const V2_RECOVERY_NONCE_OFFSET: usize = 90;
const V2_PASSWORD_WRAP_OFFSET: usize = V2_HEADER_LEN;
const V2_RECOVERY_WRAP_OFFSET: usize = V2_PASSWORD_WRAP_OFFSET + WRAPPED_KEY_LEN;
const V2_PAYLOAD_OFFSET: usize = V2_RECOVERY_WRAP_OFFSET + WRAPPED_KEY_LEN;
const V2_PASSWORD_PURPOSE: &[u8] = b"Sakura Pad password wrap v2";
const V2_RECOVERY_PURPOSE: &[u8] = b"Sakura Pad recovery wrap v2";
const V2_CONTENT_PURPOSE: &[u8] = b"Sakura Pad content v2";
const RECOVERY_PREFIX: &str = "SPRK1";

/// Maximum plaintext accepted by this primitive (8 MiB).
///
/// The caller must enforce tighter domain limits where the format has them.
pub const MAX_PLAINTEXT_BYTES: usize = 8 * 1024 * 1024;

/// Public binding for an envelope. Vault identifiers are always present;
/// memo identifiers make an envelope unusable for another memo.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scope {
    pub vault_id: [u8; 16],
    pub memo_id: Option<u64>,
}

impl Scope {
    pub const fn pad(vault_id: [u8; 16]) -> Self {
        Self {
            vault_id,
            memo_id: None,
        }
    }

    pub const fn memo(vault_id: [u8; 16], memo_id: u64) -> Self {
        Self {
            vault_id,
            memo_id: Some(memo_id),
        }
    }
}

/// Fail-closed envelope errors. Authentication deliberately does not reveal
/// whether the password or scope was wrong.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvelopeError {
    InvalidInput,
    InvalidEnvelope,
    AuthenticationFailed,
    EntropyUnavailable,
    KdfFailed,
    CryptoFailed,
}

/// An authenticated envelope's keys and format binding for repeated saves.
///
/// Constructed only by [`unlock`]. Dropping it clears the owned data and
/// wrapping keys. It does not retain the password or expose either key.
// A Debug implementation, even a redacted one, would weaken the session's
// no-Debug contract by allowing it to be formatted in diagnostic paths.
#[allow(missing_debug_implementations)]
pub struct UnlockedEnvelope {
    header: [u8; HEADER_LEN],
    wrapping_key: Zeroizing<[u8; KEY_LEN]>,
    data_key: Zeroizing<[u8; KEY_LEN]>,
}

impl UnlockedEnvelope {
    /// Authenticate a stored envelope against this unlocked session.
    ///
    /// This accepts the envelope that created the session and any subsequent
    /// `reseal` result. Scope, KDF profile, salt and data key must remain the
    /// same; both AEAD tags must validate before plaintext is returned.
    pub fn open_authenticated(&self, envelope: &[u8]) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
        if envelope.len() < PAYLOAD_OFFSET + TAG_LEN
            || envelope.len() > PAYLOAD_OFFSET + MAX_PLAINTEXT_BYTES + TAG_LEN
        {
            return Err(EnvelopeError::InvalidEnvelope);
        }

        let header = &envelope[..HEADER_LEN];
        let memo_id = if self.header[10] == SCOPE_MEMO {
            Some(u64::from_le_bytes(
                self.header[27..35].try_into().expect("fixed width"),
            ))
        } else {
            None
        };
        let scope = Scope {
            vault_id: self.header[11..27].try_into().expect("fixed width"),
            memo_id,
        };
        validate_header(scope, header)?;
        let payload_len = read_u32(header, 85)? as usize;
        if payload_len > MAX_PLAINTEXT_BYTES
            || envelope.len() != PAYLOAD_OFFSET + payload_len + TAG_LEN
        {
            return Err(EnvelopeError::InvalidEnvelope);
        }
        if header[45..61] != self.header[45..61] {
            return Err(EnvelopeError::AuthenticationFailed);
        }

        let wrap_cipher = Aes256Gcm::new_from_slice(&self.wrapping_key[..])
            .map_err(|_| EnvelopeError::InvalidEnvelope)?;
        let wrapped_key = Zeroizing::new(
            wrap_cipher
                .decrypt(
                    Nonce::from_slice(&header[61..73]),
                    Payload {
                        msg: &envelope[WRAPPED_KEY_OFFSET..PAYLOAD_OFFSET],
                        aad: &with_purpose(header, PURPOSE_DEK_WRAP),
                    },
                )
                .map_err(|_| EnvelopeError::AuthenticationFailed)?,
        );
        if wrapped_key.len() != KEY_LEN {
            return Err(EnvelopeError::InvalidEnvelope);
        }
        // Accumulate differences across the whole key, without an early exit.
        let mut key_difference = 0u8;
        for (actual, expected) in wrapped_key.iter().zip(self.data_key.iter()) {
            key_difference |= actual ^ expected;
        }
        if key_difference != 0 {
            return Err(EnvelopeError::AuthenticationFailed);
        }

        let content_cipher = Aes256Gcm::new_from_slice(&self.data_key[..])
            .map_err(|_| EnvelopeError::InvalidEnvelope)?;
        Ok(Zeroizing::new(
            content_cipher
                .decrypt(
                    Nonce::from_slice(&header[73..85]),
                    Payload {
                        msg: &envelope[PAYLOAD_OFFSET..],
                        aad: &with_purpose(header, PURPOSE_CONTENT),
                    },
                )
                .map_err(|_| EnvelopeError::AuthenticationFailed)?,
        ))
    }

    /// Encrypt a replacement payload without deriving the password key again.
    ///
    /// Both nonces are freshly generated. Since the v1 wrapping AAD covers the
    /// entire header, the data key is rewrapped for every replacement payload.
    pub fn reseal(&self, plaintext: &[u8]) -> Result<Vec<u8>, EnvelopeError> {
        if plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(EnvelopeError::InvalidInput);
        }

        let mut header = self.header;
        let mut wrap_nonce = [0u8; NONCE_LEN];
        let mut payload_nonce = [0u8; NONCE_LEN];
        fill_random(&mut wrap_nonce)?;
        fill_random(&mut payload_nonce)?;
        header[61..73].copy_from_slice(&wrap_nonce);
        header[73..85].copy_from_slice(&payload_nonce);
        header[85..89].copy_from_slice(&(plaintext.len() as u32).to_le_bytes());

        let wrap_cipher = Aes256Gcm::new_from_slice(&self.wrapping_key[..])
            .map_err(|_| EnvelopeError::CryptoFailed)?;
        let wrapped_key = wrap_cipher
            .encrypt(
                Nonce::from_slice(&wrap_nonce),
                Payload {
                    msg: &self.data_key[..],
                    aad: &with_purpose(&header, PURPOSE_DEK_WRAP),
                },
            )
            .map_err(|_| EnvelopeError::CryptoFailed)?;
        let content_cipher = Aes256Gcm::new_from_slice(&self.data_key[..])
            .map_err(|_| EnvelopeError::CryptoFailed)?;
        let encrypted_payload = content_cipher
            .encrypt(
                Nonce::from_slice(&payload_nonce),
                Payload {
                    msg: plaintext,
                    aad: &with_purpose(&header, PURPOSE_CONTENT),
                },
            )
            .map_err(|_| EnvelopeError::CryptoFailed)?;

        let mut envelope = Vec::with_capacity(PAYLOAD_OFFSET + encrypted_payload.len());
        envelope.extend_from_slice(&header);
        envelope.extend_from_slice(&wrapped_key);
        envelope.extend_from_slice(&encrypted_payload);
        Ok(envelope)
    }
}

/// Encrypt `plaintext` for `scope` using a password and return a versioned
/// envelope. Passwords are UTF-8 bytes supplied by the caller and are never
/// copied or formatted by this API.
pub fn seal(scope: Scope, password: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, EnvelopeError> {
    validate_scope(scope)?;
    validate_password(password)?;
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(EnvelopeError::InvalidInput);
    }

    let mut header = [0u8; HEADER_LEN];
    let mut cursor = 0;
    put(&mut header, &mut cursor, MAGIC);
    put(&mut header, &mut cursor, &[VERSION, CIPHER_AES_256_GCM]);
    match scope.memo_id {
        Some(memo_id) => {
            put(&mut header, &mut cursor, &[SCOPE_MEMO]);
            put(&mut header, &mut cursor, &scope.vault_id);
            put(&mut header, &mut cursor, &memo_id.to_le_bytes());
        }
        None => {
            put(&mut header, &mut cursor, &[SCOPE_VAULT]);
            put(&mut header, &mut cursor, &scope.vault_id);
            put(&mut header, &mut cursor, &[0; 8]);
        }
    }
    put(&mut header, &mut cursor, &[KDF_ARGON2ID]);
    put(&mut header, &mut cursor, &KDF_MEMORY_KIB.to_le_bytes());
    put(&mut header, &mut cursor, &KDF_ITERATIONS.to_le_bytes());
    put(&mut header, &mut cursor, &[KDF_PARALLELISM as u8]);

    let mut salt = [0u8; SALT_LEN];
    let mut wrap_nonce = [0u8; NONCE_LEN];
    let mut payload_nonce = [0u8; NONCE_LEN];
    fill_random(&mut salt)?;
    fill_random(&mut wrap_nonce)?;
    fill_random(&mut payload_nonce)?;
    put(&mut header, &mut cursor, &salt);
    put(&mut header, &mut cursor, &wrap_nonce);
    put(&mut header, &mut cursor, &payload_nonce);
    put(
        &mut header,
        &mut cursor,
        &(plaintext.len() as u32).to_le_bytes(),
    );
    debug_assert_eq!(cursor, HEADER_LEN);

    let wrapping_key = derive_key(password, &salt)?;
    let mut data_key = Zeroizing::new([0u8; KEY_LEN]);
    fill_random(&mut *data_key)?;

    let wrap_cipher =
        Aes256Gcm::new_from_slice(&wrapping_key[..]).map_err(|_| EnvelopeError::CryptoFailed)?;
    let wrapped_key = wrap_cipher
        .encrypt(
            Nonce::from_slice(&wrap_nonce),
            Payload {
                msg: &data_key[..],
                aad: &with_purpose(&header, PURPOSE_DEK_WRAP),
            },
        )
        .map_err(|_| EnvelopeError::CryptoFailed)?;

    let content_cipher =
        Aes256Gcm::new_from_slice(&data_key[..]).map_err(|_| EnvelopeError::CryptoFailed)?;
    let encrypted_payload = content_cipher
        .encrypt(
            Nonce::from_slice(&payload_nonce),
            Payload {
                msg: plaintext,
                aad: &with_purpose(&header, PURPOSE_CONTENT),
            },
        )
        .map_err(|_| EnvelopeError::CryptoFailed)?;

    let mut envelope = Vec::with_capacity(PAYLOAD_OFFSET + encrypted_payload.len());
    envelope.extend_from_slice(&header);
    envelope.extend_from_slice(&wrapped_key);
    envelope.extend_from_slice(&encrypted_payload);
    Ok(envelope)
}

/// Authenticate and decrypt an envelope for exactly `scope`.
///
/// No plaintext is returned unless both the wrapped key and payload tags
/// validate. The returned plaintext is zeroized when dropped.
pub fn open(
    scope: Scope,
    password: &[u8],
    envelope: &[u8],
) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    let (plaintext, _session) = unlock(scope, password, envelope)?;
    Ok(plaintext)
}

/// Authenticate and decrypt once, returning plaintext and an opaque session
/// for subsequent saves. Failed authentication returns neither value.
pub fn unlock(
    scope: Scope,
    password: &[u8],
    envelope: &[u8],
) -> Result<(Zeroizing<Vec<u8>>, UnlockedEnvelope), EnvelopeError> {
    validate_scope(scope)?;
    validate_password(password)?;
    if envelope.len() < PAYLOAD_OFFSET + TAG_LEN
        || envelope.len() > PAYLOAD_OFFSET + MAX_PLAINTEXT_BYTES + TAG_LEN
    {
        return Err(EnvelopeError::InvalidEnvelope);
    }

    let header = &envelope[..HEADER_LEN];
    validate_header(scope, header)?;
    let payload_len = read_u32(header, 85)? as usize;
    if payload_len > MAX_PLAINTEXT_BYTES || envelope.len() != PAYLOAD_OFFSET + payload_len + TAG_LEN
    {
        return Err(EnvelopeError::InvalidEnvelope);
    }

    let salt = &header[45..45 + SALT_LEN];
    let wrap_nonce = &header[61..61 + NONCE_LEN];
    let payload_nonce = &header[73..73 + NONCE_LEN];
    let wrapping_key = derive_key(password, salt)?;
    let wrap_cipher =
        Aes256Gcm::new_from_slice(&wrapping_key[..]).map_err(|_| EnvelopeError::InvalidEnvelope)?;
    let wrapped_key = Zeroizing::new(
        wrap_cipher
            .decrypt(
                Nonce::from_slice(wrap_nonce),
                Payload {
                    msg: &envelope[WRAPPED_KEY_OFFSET..PAYLOAD_OFFSET],
                    aad: &with_purpose(header, PURPOSE_DEK_WRAP),
                },
            )
            .map_err(|_| EnvelopeError::AuthenticationFailed)?,
    );
    if wrapped_key.len() != KEY_LEN {
        return Err(EnvelopeError::InvalidEnvelope);
    }

    let content_cipher =
        Aes256Gcm::new_from_slice(&wrapped_key).map_err(|_| EnvelopeError::InvalidEnvelope)?;
    let plaintext = Zeroizing::new(
        content_cipher
            .decrypt(
                Nonce::from_slice(payload_nonce),
                Payload {
                    msg: &envelope[PAYLOAD_OFFSET..],
                    aad: &with_purpose(header, PURPOSE_CONTENT),
                },
            )
            .map_err(|_| EnvelopeError::AuthenticationFailed)?,
    );
    let mut data_key = Zeroizing::new([0u8; KEY_LEN]);
    data_key.copy_from_slice(&wrapped_key);
    let mut authenticated_header = [0u8; HEADER_LEN];
    authenticated_header.copy_from_slice(header);
    Ok((
        plaintext,
        UnlockedEnvelope {
            header: authenticated_header,
            wrapping_key,
            data_key,
        },
    ))
}

/// A generated 256-bit recovery secret. Its display encoding has 64 hex
/// digits, grouped for transcription; the owned bytes are cleared on drop.
/// The displayed string and any caller copies must be protected separately.
#[allow(missing_debug_implementations)]
pub struct RecoveryKey(Zeroizing<[u8; KEY_LEN]>);

impl RecoveryKey {
    pub fn generate() -> Result<Self, EnvelopeError> {
        let mut bytes = Zeroizing::new([0; KEY_LEN]);
        fill_random(&mut *bytes)?;
        Ok(Self(bytes))
    }

    /// Encode as `SPRK1-XXXXXXXX-...` (eight groups of eight hex digits).
    pub fn encode(&self) -> Zeroizing<String> {
        let mut encoded = Zeroizing::new(String::with_capacity(RECOVERY_PREFIX.len() + 72));
        encoded.push_str(RECOVERY_PREFIX);
        for (index, byte) in self.0.iter().enumerate() {
            if index % 4 == 0 {
                encoded.push('-');
            }
            use std::fmt::Write;
            write!(encoded, "{byte:02X}").expect("writing into String cannot fail");
        }
        encoded
    }

    /// Decode only the canonical display form, so transcription mistakes do
    /// not silently change the key. A well-formed wrong key fails AEAD auth.
    pub fn decode(encoded: &str) -> Result<Self, EnvelopeError> {
        let bytes = encoded.as_bytes();
        if bytes.len() != 5 + 8 * 9 || !encoded.starts_with(RECOVERY_PREFIX) {
            return Err(EnvelopeError::InvalidInput);
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        for (index, item) in key.iter_mut().enumerate() {
            let offset = 6 + index * 2 + index / 4;
            if index % 4 == 0 && bytes[offset - 1] != b'-' {
                return Err(EnvelopeError::InvalidInput);
            }
            let high = hex_digit(bytes[offset]).ok_or(EnvelopeError::InvalidInput)?;
            let low = hex_digit(bytes[offset + 1]).ok_or(EnvelopeError::InvalidInput)?;
            *item = high << 4 | low;
        }
        Ok(Self(key))
    }
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

/// V2 session for the explicit either-credential recovery policy. Both wraps
/// are retained as authenticated ciphertext; only the DEK is kept in memory.
#[allow(missing_debug_implementations)]
pub struct UnlockedRecoveryEnvelope {
    header: [u8; V2_HEADER_LEN],
    password_wrap: [u8; WRAPPED_KEY_LEN],
    recovery_wrap: [u8; WRAPPED_KEY_LEN],
    data_key: Zeroizing<[u8; KEY_LEN]>,
}

impl UnlockedRecoveryEnvelope {
    /// Re-encrypt content after a successful password OR recovery unlock.
    pub fn reseal(&self, plaintext: &[u8]) -> Result<Vec<u8>, EnvelopeError> {
        if plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(EnvelopeError::InvalidInput);
        }
        let mut header = self.header;
        fill_random(&mut header[73..85])?;
        header[85..89].copy_from_slice(&(plaintext.len() as u32).to_le_bytes());
        encrypt_v2(
            &header,
            &self.password_wrap,
            &self.recovery_wrap,
            &self.data_key,
            plaintext,
        )
    }

    /// Authenticate an envelope against this session, including both wraps.
    pub fn open_authenticated(&self, envelope: &[u8]) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
        let scope = scope_from_v2_header(&self.header);
        let header = validate_v2(scope, envelope)?;
        if header[..73] != self.header[..73]
            || header[89..] != self.header[89..]
            || envelope[V2_PASSWORD_WRAP_OFFSET..V2_RECOVERY_WRAP_OFFSET] != self.password_wrap
            || envelope[V2_RECOVERY_WRAP_OFFSET..V2_PAYLOAD_OFFSET] != self.recovery_wrap
        {
            return Err(EnvelopeError::AuthenticationFailed);
        }
        decrypt_v2_payload(header, envelope, &self.data_key)
    }
}

/// Create a v2 envelope with an explicit password-OR-recovery policy.
/// Recovery material is generated independently of the password; callers must
/// persist the returned envelope and show the key before discarding it.
pub fn seal_with_recovery(
    scope: Scope,
    password: &[u8],
    plaintext: &[u8],
) -> Result<(Vec<u8>, RecoveryKey), EnvelopeError> {
    validate_scope(scope)?;
    validate_password(password)?;
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(EnvelopeError::InvalidInput);
    }
    let recovery_key = RecoveryKey::generate()?;
    let mut header = [0u8; V2_HEADER_LEN];
    header[..8].copy_from_slice(V2_MAGIC);
    header[8] = V2_VERSION;
    header[9] = CIPHER_AES_256_GCM;
    header[10] = if scope.memo_id.is_some() {
        SCOPE_MEMO
    } else {
        SCOPE_VAULT
    };
    header[11..27].copy_from_slice(&scope.vault_id);
    header[27..35].copy_from_slice(&scope.memo_id.unwrap_or(0).to_le_bytes());
    header[35] = KDF_ARGON2ID;
    header[36..40].copy_from_slice(&KDF_MEMORY_KIB.to_le_bytes());
    header[40..44].copy_from_slice(&KDF_ITERATIONS.to_le_bytes());
    header[44] = KDF_PARALLELISM as u8;
    fill_random(&mut header[45..61])?;
    fill_random(&mut header[61..73])?;
    fill_random(&mut header[73..85])?;
    header[85..89].copy_from_slice(&(plaintext.len() as u32).to_le_bytes());
    header[V2_POLICY_OFFSET] = V2_POLICY_EITHER;
    fill_random(&mut header[V2_RECOVERY_NONCE_OFFSET..V2_HEADER_LEN])?;
    let password_key = derive_key(password, &header[45..61])?;
    let mut data_key = Zeroizing::new([0u8; KEY_LEN]);
    fill_random(&mut *data_key)?;
    let password_wrap = wrap_v2(
        &password_key,
        &data_key,
        &header,
        &header[61..73],
        V2_PASSWORD_PURPOSE,
    )?;
    let recovery_wrap = wrap_v2(
        &recovery_key.0,
        &data_key,
        &header,
        &header[V2_RECOVERY_NONCE_OFFSET..V2_HEADER_LEN],
        V2_RECOVERY_PURPOSE,
    )?;
    let envelope = encrypt_v2(
        &header,
        &password_wrap,
        &recovery_wrap,
        &data_key,
        plaintext,
    )?;
    Ok((envelope, recovery_key))
}

/// V2 password route. An unsupported policy is rejected before Argon2 runs.
pub fn unlock_with_password_v2(
    scope: Scope,
    password: &[u8],
    envelope: &[u8],
) -> Result<(Zeroizing<Vec<u8>>, UnlockedRecoveryEnvelope), EnvelopeError> {
    validate_password(password)?;
    let header = validate_v2(scope, envelope)?;
    let key = derive_key(password, &header[45..61])?;
    unlock_v2_with_key(scope, envelope, &key, false)
}

/// V2 recovery route. The recovery key never passes through Argon2.
pub fn unlock_with_recovery_v2(
    scope: Scope,
    recovery_key: &RecoveryKey,
    envelope: &[u8],
) -> Result<(Zeroizing<Vec<u8>>, UnlockedRecoveryEnvelope), EnvelopeError> {
    unlock_v2_with_key(scope, envelope, &recovery_key.0, true)
}

pub fn open_with_password_v2(
    scope: Scope,
    password: &[u8],
    envelope: &[u8],
) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    unlock_with_password_v2(scope, password, envelope).map(|(plaintext, _)| plaintext)
}

pub fn open_with_recovery_v2(
    scope: Scope,
    recovery_key: &RecoveryKey,
    envelope: &[u8],
) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    unlock_with_recovery_v2(scope, recovery_key, envelope).map(|(plaintext, _)| plaintext)
}

fn unlock_v2_with_key(
    scope: Scope,
    envelope: &[u8],
    key: &[u8; KEY_LEN],
    recovery: bool,
) -> Result<(Zeroizing<Vec<u8>>, UnlockedRecoveryEnvelope), EnvelopeError> {
    let header = validate_v2(scope, envelope)?;
    let (nonce, purpose, wrap) = if recovery {
        (
            &header[V2_RECOVERY_NONCE_OFFSET..V2_HEADER_LEN],
            V2_RECOVERY_PURPOSE,
            &envelope[V2_RECOVERY_WRAP_OFFSET..V2_PAYLOAD_OFFSET],
        )
    } else {
        (
            &header[61..73],
            V2_PASSWORD_PURPOSE,
            &envelope[V2_PASSWORD_WRAP_OFFSET..V2_RECOVERY_WRAP_OFFSET],
        )
    };
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| EnvelopeError::CryptoFailed)?;
    let unwrapped = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: wrap,
                    aad: &v2_wrap_aad(header, nonce, purpose),
                },
            )
            .map_err(|_| EnvelopeError::AuthenticationFailed)?,
    );
    if unwrapped.len() != KEY_LEN {
        return Err(EnvelopeError::InvalidEnvelope);
    }
    let mut data_key = Zeroizing::new([0u8; KEY_LEN]);
    data_key.copy_from_slice(&unwrapped);
    let plaintext = decrypt_v2_payload(header, envelope, &data_key)?;
    let mut authenticated_header = [0u8; V2_HEADER_LEN];
    authenticated_header.copy_from_slice(header);
    let mut password_wrap = [0u8; WRAPPED_KEY_LEN];
    password_wrap.copy_from_slice(&envelope[V2_PASSWORD_WRAP_OFFSET..V2_RECOVERY_WRAP_OFFSET]);
    let mut recovery_wrap = [0u8; WRAPPED_KEY_LEN];
    recovery_wrap.copy_from_slice(&envelope[V2_RECOVERY_WRAP_OFFSET..V2_PAYLOAD_OFFSET]);
    Ok((
        plaintext,
        UnlockedRecoveryEnvelope {
            header: authenticated_header,
            password_wrap,
            recovery_wrap,
            data_key,
        },
    ))
}

fn validate_v2(scope: Scope, envelope: &[u8]) -> Result<&[u8], EnvelopeError> {
    validate_scope(scope)?;
    if envelope.len() < V2_PAYLOAD_OFFSET + TAG_LEN
        || envelope.len() > V2_PAYLOAD_OFFSET + MAX_PLAINTEXT_BYTES + TAG_LEN
    {
        return Err(EnvelopeError::InvalidEnvelope);
    }
    let header = &envelope[..V2_HEADER_LEN];
    if &header[..8] != V2_MAGIC
        || header[8] != V2_VERSION
        || header[9] != CIPHER_AES_256_GCM
        || header[35] != KDF_ARGON2ID
        || read_u32(header, 36)? != KDF_MEMORY_KIB
        || read_u32(header, 40)? != KDF_ITERATIONS
        || header[44] != KDF_PARALLELISM as u8
        || header[V2_POLICY_OFFSET] != V2_POLICY_EITHER
    {
        return Err(EnvelopeError::InvalidEnvelope);
    }
    let kind = if scope.memo_id.is_some() {
        SCOPE_MEMO
    } else {
        SCOPE_VAULT
    };
    if header[10] != kind || header[11..27] != scope.vault_id {
        return Err(EnvelopeError::AuthenticationFailed);
    }
    if header[27..35] != scope.memo_id.unwrap_or(0).to_le_bytes() {
        return Err(EnvelopeError::AuthenticationFailed);
    }
    let payload_len = read_u32(header, 85)? as usize;
    if payload_len > MAX_PLAINTEXT_BYTES
        || envelope.len() != V2_PAYLOAD_OFFSET + payload_len + TAG_LEN
    {
        return Err(EnvelopeError::InvalidEnvelope);
    }
    Ok(header)
}

fn scope_from_v2_header(header: &[u8; V2_HEADER_LEN]) -> Scope {
    let vault_id = header[11..27].try_into().expect("fixed width");
    let memo_id = (header[10] == SCOPE_MEMO)
        .then(|| u64::from_le_bytes(header[27..35].try_into().expect("fixed width")));
    Scope { vault_id, memo_id }
}

fn v2_wrap_aad(header: &[u8], nonce: &[u8], purpose: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(61 + 1 + NONCE_LEN + purpose.len());
    aad.extend_from_slice(&header[..61]);
    aad.push(header[V2_POLICY_OFFSET]);
    aad.extend_from_slice(nonce);
    aad.extend_from_slice(purpose);
    aad
}

fn v2_content_aad(header: &[u8], password_wrap: &[u8], recovery_wrap: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(V2_PAYLOAD_OFFSET + V2_CONTENT_PURPOSE.len());
    aad.extend_from_slice(header);
    aad.extend_from_slice(password_wrap);
    aad.extend_from_slice(recovery_wrap);
    aad.extend_from_slice(V2_CONTENT_PURPOSE);
    aad
}

fn wrap_v2(
    wrapping_key: &[u8; KEY_LEN],
    data_key: &[u8; KEY_LEN],
    header: &[u8; V2_HEADER_LEN],
    nonce: &[u8],
    purpose: &[u8],
) -> Result<[u8; WRAPPED_KEY_LEN], EnvelopeError> {
    let cipher =
        Aes256Gcm::new_from_slice(wrapping_key).map_err(|_| EnvelopeError::CryptoFailed)?;
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: data_key,
                aad: &v2_wrap_aad(header, nonce, purpose),
            },
        )
        .map_err(|_| EnvelopeError::CryptoFailed)?;
    encrypted
        .try_into()
        .map_err(|_| EnvelopeError::CryptoFailed)
}

fn encrypt_v2(
    header: &[u8; V2_HEADER_LEN],
    password_wrap: &[u8; WRAPPED_KEY_LEN],
    recovery_wrap: &[u8; WRAPPED_KEY_LEN],
    data_key: &[u8; KEY_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, EnvelopeError> {
    let cipher = Aes256Gcm::new_from_slice(data_key).map_err(|_| EnvelopeError::CryptoFailed)?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&header[73..85]),
            Payload {
                msg: plaintext,
                aad: &v2_content_aad(header, password_wrap, recovery_wrap),
            },
        )
        .map_err(|_| EnvelopeError::CryptoFailed)?;
    let mut envelope = Vec::with_capacity(V2_PAYLOAD_OFFSET + ciphertext.len());
    envelope.extend_from_slice(header);
    envelope.extend_from_slice(password_wrap);
    envelope.extend_from_slice(recovery_wrap);
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

fn decrypt_v2_payload(
    header: &[u8],
    envelope: &[u8],
    data_key: &[u8; KEY_LEN],
) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    let cipher = Aes256Gcm::new_from_slice(data_key).map_err(|_| EnvelopeError::CryptoFailed)?;
    Ok(Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(&header[73..85]),
                Payload {
                    msg: &envelope[V2_PAYLOAD_OFFSET..],
                    aad: &v2_content_aad(
                        header,
                        &envelope[V2_PASSWORD_WRAP_OFFSET..V2_RECOVERY_WRAP_OFFSET],
                        &envelope[V2_RECOVERY_WRAP_OFFSET..V2_PAYLOAD_OFFSET],
                    ),
                },
            )
            .map_err(|_| EnvelopeError::AuthenticationFailed)?,
    ))
}

fn validate_scope(scope: Scope) -> Result<(), EnvelopeError> {
    if scope.vault_id == [0; 16] || scope.memo_id == Some(0) {
        Err(EnvelopeError::InvalidInput)
    } else {
        Ok(())
    }
}

fn validate_password(password: &[u8]) -> Result<(), EnvelopeError> {
    if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
        Err(EnvelopeError::InvalidInput)
    } else {
        Ok(())
    }
}

fn validate_header(scope: Scope, header: &[u8]) -> Result<(), EnvelopeError> {
    if header.len() != HEADER_LEN || &header[..8] != MAGIC {
        return Err(EnvelopeError::InvalidEnvelope);
    }
    if header[8] != VERSION
        || header[9] != CIPHER_AES_256_GCM
        || header[35] != KDF_ARGON2ID
        || read_u32(header, 36)? != KDF_MEMORY_KIB
        || read_u32(header, 40)? != KDF_ITERATIONS
        || header[44] != KDF_PARALLELISM as u8
    {
        return Err(EnvelopeError::InvalidEnvelope);
    }

    let expected_kind = if scope.memo_id.is_some() {
        SCOPE_MEMO
    } else {
        SCOPE_VAULT
    };
    if header[10] != expected_kind || header[11..27] != scope.vault_id {
        return Err(EnvelopeError::AuthenticationFailed);
    }
    match scope.memo_id {
        Some(memo_id) if header[27..35] == memo_id.to_le_bytes() => {}
        Some(_) => return Err(EnvelopeError::AuthenticationFailed),
        None if header[27..35] == [0; 8] => {}
        None => return Err(EnvelopeError::InvalidEnvelope),
    }
    Ok(())
}

fn derive_key(password: &[u8], salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>, EnvelopeError> {
    let params = Params::new(
        KDF_MEMORY_KIB,
        KDF_ITERATIONS,
        KDF_PARALLELISM,
        Some(KEY_LEN),
    )
    .map_err(|_| EnvelopeError::KdfFailed)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut memory = Zeroizing::new(vec![Block::new(); KDF_BLOCKS]);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon2
        .hash_password_into_with_memory(password, salt, &mut *key, &mut *memory)
        .map_err(|_| EnvelopeError::KdfFailed)?;
    Ok(key)
}

fn fill_random(output: &mut [u8]) -> Result<(), EnvelopeError> {
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };

    // SAFETY: BCryptGenRandom writes exactly the provided mutable buffer and
    // the system-preferred provider requires no algorithm handle.
    let status = unsafe { BCryptGenRandom(None, output, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_ok() {
        Ok(())
    } else {
        Err(EnvelopeError::EntropyUnavailable)
    }
}

fn put<const N: usize>(output: &mut [u8; HEADER_LEN], cursor: &mut usize, bytes: &[u8; N]) {
    output[*cursor..*cursor + N].copy_from_slice(bytes);
    *cursor += N;
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, EnvelopeError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or(EnvelopeError::InvalidEnvelope)?;
    Ok(u32::from_le_bytes(value.try_into().expect("fixed width")))
}

fn with_purpose(header: &[u8], purpose: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + purpose.len());
    aad.extend_from_slice(header);
    aad.extend_from_slice(purpose);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &[u8] = b"correct horse battery staple";
    const SCOPE: Scope = Scope {
        vault_id: *b"sakura-vault-001",
        memo_id: None,
    };
    const MEMO_SCOPE: Scope = Scope {
        vault_id: *b"sakura-vault-001",
        memo_id: Some(7),
    };

    #[test]
    fn recovery_key_round_trips_canonical_display_form() {
        let key = RecoveryKey::generate().unwrap();
        let display = key.encode();
        assert_eq!(display.len(), 77);
        assert_eq!(&*RecoveryKey::decode(&display).unwrap().0, &*key.0);
        let mut bad = display.to_string();
        bad.replace_range(6..7, "g");
        assert_eq!(
            RecoveryKey::decode(&bad).err(),
            Some(EnvelopeError::InvalidInput)
        );
    }

    #[test]
    fn v2_both_credentials_survive_reseal_and_reopen() {
        let (first, recovery) = seal_with_recovery(MEMO_SCOPE, PASSWORD, b"first").unwrap();
        assert_eq!(
            &*open_with_password_v2(MEMO_SCOPE, PASSWORD, &first).unwrap(),
            b"first"
        );
        assert_eq!(
            &*open_with_recovery_v2(MEMO_SCOPE, &recovery, &first).unwrap(),
            b"first"
        );
        let (_, password_session) = unlock_with_password_v2(MEMO_SCOPE, PASSWORD, &first).unwrap();
        let second = password_session.reseal(b"second").unwrap();
        let (_, recovery_session) =
            unlock_with_recovery_v2(MEMO_SCOPE, &recovery, &second).unwrap();
        let third = recovery_session.reseal(b"third").unwrap();
        assert_eq!(
            &*open_with_password_v2(MEMO_SCOPE, PASSWORD, &third).unwrap(),
            b"third"
        );
        let decoded = RecoveryKey::decode(&recovery.encode()).unwrap();
        assert_eq!(
            &*open_with_recovery_v2(MEMO_SCOPE, &decoded, &third).unwrap(),
            b"third"
        );
        assert_eq!(
            &*recovery_session.open_authenticated(&third).unwrap(),
            b"third"
        );
        assert_eq!(
            open(MEMO_SCOPE, PASSWORD, &third).err(),
            Some(EnvelopeError::InvalidEnvelope)
        );
    }

    #[test]
    fn v2_rejects_wrong_credentials_scope_policy_wrap_tampering_and_bounds() {
        let (envelope, recovery) = seal_with_recovery(MEMO_SCOPE, PASSWORD, b"secret").unwrap();
        let wrong_recovery = RecoveryKey::generate().unwrap();
        assert_eq!(
            open_with_password_v2(MEMO_SCOPE, b"wrong", &envelope).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
        assert_eq!(
            open_with_recovery_v2(MEMO_SCOPE, &wrong_recovery, &envelope).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
        assert_eq!(
            open_with_recovery_v2(Scope::memo(MEMO_SCOPE.vault_id, 8), &recovery, &envelope).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
        for offset in [
            V2_PASSWORD_WRAP_OFFSET,
            V2_RECOVERY_WRAP_OFFSET,
            V2_PAYLOAD_OFFSET,
            73,
            89,
            90,
        ] {
            let mut changed = envelope.clone();
            changed[offset] ^= 1;
            assert!(
                open_with_password_v2(MEMO_SCOPE, PASSWORD, &changed).is_err(),
                "password offset {offset}"
            );
            assert!(
                open_with_recovery_v2(MEMO_SCOPE, &recovery, &changed).is_err(),
                "recovery offset {offset}"
            );
        }
        let mut policy = envelope.clone();
        policy[V2_POLICY_OFFSET] = 2; // hypothetical AND policy must never fall back to password OR
        assert_eq!(
            open_with_password_v2(MEMO_SCOPE, PASSWORD, &policy).err(),
            Some(EnvelopeError::InvalidEnvelope)
        );
        let mut oversize = envelope.clone();
        oversize[85..89].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            open_with_password_v2(MEMO_SCOPE, PASSWORD, &oversize).err(),
            Some(EnvelopeError::InvalidEnvelope)
        );
        assert_eq!(
            open_with_recovery_v2(MEMO_SCOPE, &recovery, &oversize).err(),
            Some(EnvelopeError::InvalidEnvelope)
        );
    }

    #[test]
    fn argon2id_matches_rfc_9106_section_5_3_known_answer() {
        // Independent published vector, not an expected value generated by
        // this implementation: https://www.rfc-editor.org/rfc/rfc9106#section-5.3
        let params = argon2::ParamsBuilder::new()
            .m_cost(32)
            .t_cost(3)
            .p_cost(4)
            .output_len(32)
            .data(argon2::AssociatedData::new(&[4; 12]).unwrap())
            .build()
            .unwrap();
        let secret = [3; 8];
        let algorithm =
            Argon2::new_with_secret(&secret, Algorithm::Argon2id, Version::V0x13, params).unwrap();
        let mut actual = Zeroizing::new([0; 32]);
        let mut memory = Zeroizing::new(vec![Block::new(); 32]);
        algorithm
            .hash_password_into_with_memory(&[1; 32], &[2; 16], &mut *actual, &mut *memory)
            .unwrap();
        assert_eq!(
            *actual,
            [
                0x0d, 0x64, 0x0d, 0xf5, 0x8d, 0x78, 0x76, 0x6c, 0x08, 0xc0, 0x37, 0xa3, 0x4a, 0x8b,
                0x53, 0xc9, 0xd0, 0x1e, 0xf0, 0x45, 0x2d, 0x75, 0xb6, 0x5e, 0xb5, 0x25, 0x20, 0xe9,
                0x6b, 0x01, 0xe6, 0x59
            ]
        );
    }

    #[test]
    fn new_salt_and_nonces_are_generated_and_payload_substitution_fails() {
        let first = seal(MEMO_SCOPE, PASSWORD, b"first!").unwrap();
        let mut second = seal(MEMO_SCOPE, PASSWORD, b"second").unwrap();
        assert_ne!(&first[45..61], &second[45..61]);
        assert_ne!(&first[61..73], &second[61..73]);
        assert_ne!(&first[73..85], &second[73..85]);
        second[PAYLOAD_OFFSET..].copy_from_slice(&first[PAYLOAD_OFFSET..]);
        assert_eq!(
            open(MEMO_SCOPE, PASSWORD, &second).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
    }

    #[test]
    fn vault_and_memo_payloads_round_trip_and_are_scope_bound() {
        for scope in [SCOPE, MEMO_SCOPE] {
            let plaintext = b"private memo body";
            let envelope = seal(scope, PASSWORD, plaintext).expect("seal");
            assert_eq!(&*open(scope, PASSWORD, &envelope).expect("open"), plaintext);
            assert_eq!(
                open(
                    Scope {
                        vault_id: *b"other-vault-0000",
                        memo_id: scope.memo_id,
                    },
                    PASSWORD,
                    &envelope
                )
                .err(),
                Some(EnvelopeError::AuthenticationFailed)
            );
            if let Some(id) = scope.memo_id {
                assert_eq!(
                    open(
                        Scope {
                            memo_id: Some(id + 1),
                            ..scope
                        },
                        PASSWORD,
                        &envelope
                    )
                    .err(),
                    Some(EnvelopeError::AuthenticationFailed)
                );
            }
        }
    }

    #[test]
    fn wrong_password_and_authenticated_field_changes_return_no_plaintext() {
        let envelope = seal(SCOPE, PASSWORD, b"secret").expect("seal");
        assert_eq!(
            open(SCOPE, b"wrong password", &envelope).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
        for index in [
            8,
            9,
            10,
            11,
            35,
            36,
            45,
            61,
            73,
            85,
            HEADER_LEN,
            PAYLOAD_OFFSET,
        ] {
            let mut changed = envelope.clone();
            changed[index] ^= 0x40;
            assert!(open(SCOPE, PASSWORD, &changed).is_err(), "offset {index}");
        }
    }

    #[test]
    fn rejects_truncation_trailing_data_and_unsupported_kdf_before_derivation() {
        let envelope = seal(SCOPE, PASSWORD, b"secret").expect("seal");
        for length in [0, 1, HEADER_LEN - 1, PAYLOAD_OFFSET - 1, envelope.len() - 1] {
            assert_eq!(
                open(SCOPE, PASSWORD, &envelope[..length]).err(),
                Some(EnvelopeError::InvalidEnvelope)
            );
        }
        let mut trailing = envelope.clone();
        trailing.push(0);
        assert_eq!(
            open(SCOPE, PASSWORD, &trailing).err(),
            Some(EnvelopeError::InvalidEnvelope)
        );
        let mut unsupported = envelope;
        unsupported[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            open(SCOPE, PASSWORD, &unsupported).err(),
            Some(EnvelopeError::InvalidEnvelope)
        );
    }

    #[test]
    fn input_limits_are_checked_before_randomness_or_kdf() {
        assert_eq!(
            seal(Scope::pad([0; 16]), PASSWORD, b"x").unwrap_err(),
            EnvelopeError::InvalidInput
        );
        assert_eq!(
            seal(Scope::memo(SCOPE.vault_id, 0), PASSWORD, b"x").unwrap_err(),
            EnvelopeError::InvalidInput
        );
        assert_eq!(
            seal(SCOPE, b"", b"x").unwrap_err(),
            EnvelopeError::InvalidInput
        );
        assert_eq!(
            seal(SCOPE, PASSWORD, &vec![0; MAX_PLAINTEXT_BYTES + 1]).unwrap_err(),
            EnvelopeError::InvalidInput
        );
        assert_eq!(
            seal(SCOPE, &vec![b'x'; MAX_PASSWORD_BYTES + 1], b"x").unwrap_err(),
            EnvelopeError::InvalidInput
        );
    }

    #[test]
    fn unlocked_session_reseals_changed_payloads_with_new_wraps_and_nonces() {
        let first = seal(MEMO_SCOPE, PASSWORD, b"original").unwrap();
        let (plaintext, session) = unlock(MEMO_SCOPE, PASSWORD, &first).unwrap();
        assert_eq!(&*plaintext, b"original");

        let second = session.reseal(b"edited once").unwrap();
        let third = session
            .reseal(b"edited twice with different length")
            .unwrap();
        for (envelope, expected) in [
            (&second, b"edited once".as_slice()),
            (&third, b"edited twice with different length".as_slice()),
        ] {
            assert_eq!(&*open(MEMO_SCOPE, PASSWORD, envelope).unwrap(), expected);
            assert_eq!(&envelope[11..61], &first[11..61]);
        }
        for (left, right) in [(&first, &second), (&second, &third)] {
            assert_ne!(&left[61..73], &right[61..73]);
            assert_ne!(&left[73..85], &right[73..85]);
            assert_ne!(
                &left[WRAPPED_KEY_OFFSET..PAYLOAD_OFFSET],
                &right[WRAPPED_KEY_OFFSET..PAYLOAD_OFFSET]
            );
        }
    }

    #[test]
    fn unlock_failure_never_returns_a_session_and_reseal_checks_bounds() {
        let first = seal(MEMO_SCOPE, PASSWORD, b"original").unwrap();
        assert_eq!(
            unlock(MEMO_SCOPE, b"wrong password", &first).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
        assert_eq!(
            unlock(Scope::memo(MEMO_SCOPE.vault_id, 8), PASSWORD, &first).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
        let mut tampered = first.clone();
        tampered[PAYLOAD_OFFSET] ^= 1;
        assert_eq!(
            unlock(MEMO_SCOPE, PASSWORD, &tampered).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
        let mut oversized = first.clone();
        oversized[85..89].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            unlock(MEMO_SCOPE, PASSWORD, &oversized).err(),
            Some(EnvelopeError::InvalidEnvelope)
        );

        let (_, session) = unlock(MEMO_SCOPE, PASSWORD, &first).unwrap();
        assert_eq!(
            session.reseal(&vec![0; MAX_PLAINTEXT_BYTES + 1]).err(),
            Some(EnvelopeError::InvalidInput)
        );
        assert_eq!(
            &*open(
                MEMO_SCOPE,
                PASSWORD,
                &session.reseal(b"still usable").unwrap()
            )
            .unwrap(),
            b"still usable"
        );
    }

    #[test]
    fn unlocked_session_authenticates_original_and_resealed_envelopes() {
        let original = seal(MEMO_SCOPE, PASSWORD, b"original").unwrap();
        let (_, session) = unlock(MEMO_SCOPE, PASSWORD, &original).unwrap();
        assert_eq!(
            &*session.open_authenticated(&original).unwrap(),
            b"original"
        );
        let changed = session.reseal(b"changed").unwrap();
        assert_eq!(&*session.open_authenticated(&changed).unwrap(), b"changed");

        let other_scope = seal(Scope::memo(MEMO_SCOPE.vault_id, 8), PASSWORD, b"other").unwrap();
        assert_eq!(
            session.open_authenticated(&other_scope).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
        let other_envelope = seal(MEMO_SCOPE, PASSWORD, b"other session").unwrap();
        let (_, other_session) = unlock(MEMO_SCOPE, PASSWORD, &other_envelope).unwrap();
        assert_eq!(
            other_session.open_authenticated(&changed).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );

        let mut altered_key = Zeroizing::new(*session.data_key);
        altered_key[0] ^= 1;
        let alternate_key_session = UnlockedEnvelope {
            header: session.header,
            wrapping_key: Zeroizing::new(*session.wrapping_key),
            data_key: altered_key,
        };
        let alternate_key_envelope = alternate_key_session.reseal(b"different key").unwrap();
        assert_eq!(
            session.open_authenticated(&alternate_key_envelope).err(),
            Some(EnvelopeError::AuthenticationFailed)
        );
    }

    #[test]
    fn session_open_rejects_tampering_and_bounds_before_decryption() {
        let original = seal(MEMO_SCOPE, PASSWORD, b"original").unwrap();
        let (_, session) = unlock(MEMO_SCOPE, PASSWORD, &original).unwrap();
        for index in [8, 35, 45, 61, 73, 85, WRAPPED_KEY_OFFSET, PAYLOAD_OFFSET] {
            let mut changed = original.clone();
            changed[index] ^= 1;
            assert!(
                session.open_authenticated(&changed).is_err(),
                "offset {index}"
            );
        }
        let mut oversized_length = original.clone();
        oversized_length[85..89].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            session.open_authenticated(&oversized_length).err(),
            Some(EnvelopeError::InvalidEnvelope)
        );
        assert_eq!(
            session
                .open_authenticated(&vec![0; PAYLOAD_OFFSET + MAX_PLAINTEXT_BYTES + TAG_LEN + 1])
                .err(),
            Some(EnvelopeError::InvalidEnvelope)
        );
    }
}
