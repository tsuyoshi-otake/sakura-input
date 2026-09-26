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
    Ok(Zeroizing::new(
        content_cipher
            .decrypt(
                Nonce::from_slice(payload_nonce),
                Payload {
                    msg: &envelope[PAYLOAD_OFFSET..],
                    aad: &with_purpose(header, PURPOSE_CONTENT),
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
}
