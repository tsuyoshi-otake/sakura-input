//! Frozen Sakura update-signing v2 contract and rollback state.
//!
//! All network-controlled inputs are bounded and canonical before any
//! cryptographic or persistent-state operation. The keyring is compiled into
//! the settings binary; it is never learned from the release server.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptDestroyKey, BCryptHash, BCryptImportKeyPair,
    BCryptOpenAlgorithmProvider, BCryptVerifySignature, BCRYPT_ALG_HANDLE, BCRYPT_ECCPUBLIC_BLOB,
    BCRYPT_ECDSA_P256_ALGORITHM, BCRYPT_ECDSA_PUBLIC_P256_MAGIC, BCRYPT_FLAGS, BCRYPT_KEY_HANDLE,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH, REPLACE_FILE_FLAGS,
};

pub const MAX_INSTALLER_BYTES: u64 = 200 * 1024 * 1024;
pub const MAX_SIGNATURE_BYTES: u64 = 8 * 1024;
const EMBEDDED_TRUST_EPOCH: u64 = 1;
const MAX_KEY_COUNT: usize = 3;
const MAX_TRUST_STATE_BYTES: u64 = 4 * 1024;
const MANIFEST_DOMAIN: &[u8] = b"Sakura Input update manifest v2\0";
const ALGORITHM_DOMAIN: &[u8] = b"ecdsa-p256-sha256-p1363\0";
const KEY_ID_DOMAIN: &[u8] = b"Sakura Input update key v1\0";
static NEXT_TRUST_TEMP: AtomicU64 = AtomicU64::new(1);
const KEYRING_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/update-signing/public-keys-v1.txt"
));
const RELEASE_SEQUENCE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/update-signing/release-sequence.txt"
));

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut fields = value.split('.');
        let major = parse_version_field(fields.next(), "major")?;
        let minor = parse_version_field(fields.next(), "minor")?;
        let patch = parse_version_field(fields.next(), "patch")?;
        if fields.next().is_some() {
            return Err("version must contain exactly major.minor.patch".to_owned());
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn parse_version_field(value: Option<&str>, name: &str) -> Result<u32, String> {
    let value = value.ok_or_else(|| format!("version is missing its {name} field"))?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "version {name} field must be an unsigned decimal integer"
        ));
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(format!(
            "version {name} field has an ambiguous leading zero"
        ));
    }
    value
        .parse::<u32>()
        .map_err(|_| format!("version {name} field is out of range"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticodePolicy {
    Required,
    Unsigned,
}

impl AuthenticodePolicy {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "required" => Ok(Self::Required),
            "unsigned" => Ok(Self::Unsigned),
            _ => Err("authenticode must be exactly required or unsigned".to_owned()),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Unsigned => "unsigned",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManifest {
    pub trust_epoch: u64,
    pub release_sequence: u64,
    pub version: Version,
    pub source_commit: String,
    pub installer_url: String,
    pub sha256: [u8; 32],
    pub size: u64,
    pub authenticode: AuthenticodePolicy,
    pub minimum_updater_version: Version,
    pub expires_unix: u64,
}

impl ReleaseManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        Self::parse_with_sequence_floor(bytes, embedded_sequence_floor()?)
    }

    fn parse_with_sequence_floor(bytes: &[u8], sequence_floor: u64) -> Result<Self, String> {
        let lines = canonical_lines(bytes, 17, "release manifest", 64 * 1024)?;
        let schema = field(lines[0], "schema", "release manifest")?;
        let product = field(lines[1], "product", "release manifest")?;
        let repository = field(lines[2], "repository", "release manifest")?;
        let channel = field(lines[3], "channel", "release manifest")?;
        let platform = field(lines[4], "platform", "release manifest")?;
        let trust_epoch = parse_decimal(
            field(lines[5], "trust_epoch", "release manifest")?,
            "trust_epoch",
        )?;
        let release_sequence = parse_decimal(
            field(lines[6], "release_sequence", "release manifest")?,
            "release_sequence",
        )?;
        let version = Version::parse(field(lines[7], "version", "release manifest")?)?;
        let tag = field(lines[8], "tag", "release manifest")?;
        let source_commit = field(lines[9], "source_commit", "release manifest")?;
        let asset_name = field(lines[10], "asset_name", "release manifest")?;
        let installer_url = field(lines[11], "installer_url", "release manifest")?;
        let sha256 = decode_hex::<32>(field(lines[12], "sha256", "release manifest")?, "SHA-256")?;
        let size = parse_decimal(field(lines[13], "size", "release manifest")?, "size")?;
        let authenticode =
            AuthenticodePolicy::parse(field(lines[14], "authenticode", "release manifest")?)?;
        let minimum_updater_version = Version::parse(field(
            lines[15],
            "minimum_updater_version",
            "release manifest",
        )?)?;
        let expires_unix = parse_decimal(
            field(lines[16], "expires_unix", "release manifest")?,
            "expires_unix",
        )?;

        if schema != "2" {
            return Err("release manifest schema is unsupported".to_owned());
        }
        if product != "sakura-input"
            || repository != "tsuyoshi-otake/sakura-input"
            || channel != "stable"
            || platform != "windows-x86_64"
        {
            return Err(
                "release manifest product, repository, channel, or platform is not trusted"
                    .to_owned(),
            );
        }
        if trust_epoch != EMBEDDED_TRUST_EPOCH {
            return Err(format!(
                "release manifest trust epoch {trust_epoch} is unsupported"
            ));
        }
        if release_sequence < sequence_floor {
            return Err(format!(
                "release sequence {release_sequence} is below the embedded floor {sequence_floor}"
            ));
        }
        if tag != format!("v{version}") {
            return Err("release manifest tag does not match version".to_owned());
        }
        if !is_lower_hex(source_commit, 40) {
            return Err(
                "source_commit must be exactly 40 lowercase hexadecimal characters".to_owned(),
            );
        }
        if asset_name != "sakura_setup.exe" {
            return Err("release manifest asset_name is not allow-listed".to_owned());
        }
        let expected_url = installer_url_for(version);
        if installer_url != expected_url {
            return Err(format!(
                "installer URL must be the canonical release asset URL {expected_url:?}"
            ));
        }
        if !(1..=MAX_INSTALLER_BYTES).contains(&size) {
            return Err(format!(
                "installer size must be between 1 and {MAX_INSTALLER_BYTES} bytes"
            ));
        }

        let manifest = Self {
            trust_epoch,
            release_sequence,
            version,
            source_commit: source_commit.to_owned(),
            installer_url: installer_url.to_owned(),
            sha256,
            size,
            authenticode,
            minimum_updater_version,
            expires_unix,
        };
        if manifest.canonical_text().as_bytes() != bytes {
            return Err(
                "release manifest bytes are not the exact canonical serialization".to_owned(),
            );
        }
        Ok(manifest)
    }

    pub fn validate_runtime(&self, current: Version, now_unix: u64) -> Result<(), String> {
        if self.minimum_updater_version > current {
            return Err(format!(
                "release requires updater {}, running updater is {current}",
                self.minimum_updater_version
            ));
        }
        if self.expires_unix <= now_unix {
            return Err(format!(
                "release manifest expired at {}, verification time is {now_unix}",
                self.expires_unix
            ));
        }
        Ok(())
    }

    pub fn canonical_text(&self) -> String {
        format!(
            concat!(
                "schema=2\n",
                "product=sakura-input\n",
                "repository=tsuyoshi-otake/sakura-input\n",
                "channel=stable\n",
                "platform=windows-x86_64\n",
                "trust_epoch={}\n",
                "release_sequence={}\n",
                "version={}\n",
                "tag=v{}\n",
                "source_commit={}\n",
                "asset_name=sakura_setup.exe\n",
                "installer_url={}\n",
                "sha256={}\n",
                "size={}\n",
                "authenticode={}\n",
                "minimum_updater_version={}\n",
                "expires_unix={}\n"
            ),
            self.trust_epoch,
            self.release_sequence,
            self.version,
            self.version,
            self.source_commit,
            self.installer_url,
            encode_hex(&self.sha256),
            self.size,
            self.authenticode.as_str(),
            self.minimum_updater_version,
            self.expires_unix,
        )
    }
}

pub fn installer_url_for(version: Version) -> String {
    format!(
        "https://github.com/tsuyoshi-otake/sakura-input/releases/download/v{version}/sakura_setup.exe"
    )
}

fn canonical_lines<'a>(
    bytes: &'a [u8],
    expected: usize,
    name: &str,
    limit: u64,
) -> Result<Vec<&'a str>, String> {
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err(format!("{name} is empty or exceeds the {limit}-byte limit"));
    }
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(format!("{name} contains a UTF-8 BOM"));
    }
    if bytes.last() != Some(&b'\n') || bytes.get(bytes.len().saturating_sub(2)) == Some(&b'\n') {
        return Err(format!("{name} must have exactly one terminal LF"));
    }
    if bytes.contains(&b'\r') || bytes.contains(&0) {
        return Err(format!("{name} contains CR or NUL"));
    }
    let source = std::str::from_utf8(bytes).map_err(|_| format!("{name} is not UTF-8"))?;
    let without_terminal = &source[..source.len() - 1];
    let lines: Vec<_> = without_terminal.split('\n').collect();
    if lines.len() != expected || lines.iter().any(|line| line.is_empty()) {
        return Err(format!(
            "{name} must contain exactly {expected} non-empty fields"
        ));
    }
    if lines
        .iter()
        .any(|line| line.len() > 2_048 || line.trim() != *line)
    {
        return Err(format!("{name} contains a padded or overlong line"));
    }
    Ok(lines)
}

fn field<'a>(line: &'a str, expected: &str, name: &str) -> Result<&'a str, String> {
    let prefix = format!("{expected}=");
    let value = line
        .strip_prefix(&prefix)
        .ok_or_else(|| format!("{name} field is not canonical {expected:?}"))?;
    if value.is_empty() {
        return Err(format!("{name} field {expected:?} is empty"));
    }
    Ok(value)
}

fn parse_decimal(value: &str, name: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{name} must be an unsigned decimal integer"));
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(format!("{name} has an ambiguous leading zero"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("{name} is out of range"))
}

fn parse_release_sequence_floor(bytes: &[u8]) -> Result<u64, String> {
    let lines = canonical_lines(bytes, 1, "embedded release sequence", 64)?;
    let floor = parse_decimal(lines[0], "embedded release sequence")?;
    if floor == 0 {
        return Err("embedded release sequence must be greater than zero".to_owned());
    }
    if format!("{floor}\n").as_bytes() != bytes {
        return Err("embedded release sequence is not canonical".to_owned());
    }
    Ok(floor)
}

fn embedded_sequence_floor() -> Result<u64, String> {
    static FLOOR: OnceLock<Result<u64, String>> = OnceLock::new();
    match FLOOR.get_or_init(|| parse_release_sequence_floor(RELEASE_SEQUENCE_BYTES)) {
        Ok(floor) => Ok(*floor),
        Err(error) => Err(error.clone()),
    }
}

#[cfg(test)]
pub(crate) fn embedded_sequence_floor_for_test() -> u64 {
    embedded_sequence_floor().expect("embedded release sequence must be valid in tests")
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_hex<const N: usize>(value: &str, name: &str) -> Result<[u8; N], String> {
    if !is_lower_hex(value, N * 2) {
        return Err(format!(
            "{name} must be exactly {} lowercase hexadecimal characters",
            N * 2
        ));
    }
    let mut decoded = [0u8; N];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let offset = index * 2;
        *byte =
            (hex_nibble(value.as_bytes()[offset]) << 4) | hex_nibble(value.as_bytes()[offset + 1]);
    }
    Ok(decoded)
}

const fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => 0,
    }
}

pub fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

#[derive(Debug)]
struct SignatureRecord {
    key_id: [u8; 32],
    signature: [u8; 64],
}

#[derive(Debug)]
struct SignatureEnvelope {
    manifest_sha256: [u8; 32],
    records: Vec<SignatureRecord>,
}

impl SignatureEnvelope {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_SIGNATURE_BYTES {
            return Err(format!(
                "signature envelope is empty or exceeds the {MAX_SIGNATURE_BYTES}-byte limit"
            ));
        }
        if bytes.starts_with(&[0xef, 0xbb, 0xbf])
            || bytes.last() != Some(&b'\n')
            || bytes.contains(&b'\r')
            || bytes.contains(&0)
        {
            return Err("signature envelope bytes are not canonical".to_owned());
        }
        let source =
            std::str::from_utf8(bytes).map_err(|_| "signature envelope is not UTF-8".to_owned())?;
        let lines: Vec<_> = source[..source.len() - 1].split('\n').collect();
        if lines.len() < 5
            || lines
                .iter()
                .any(|line| line.is_empty() || line.trim() != *line)
        {
            return Err("signature envelope has empty, padded, or missing fields".to_owned());
        }
        if field(lines[0], "schema", "signature envelope")? != "1"
            || field(lines[1], "algorithm", "signature envelope")? != "ecdsa-p256-sha256-p1363"
        {
            return Err("signature envelope schema or algorithm is unsupported".to_owned());
        }
        let manifest_sha256 = decode_hex::<32>(
            field(lines[2], "manifest_sha256", "signature envelope")?,
            "manifest_sha256",
        )?;
        let count = parse_decimal(
            field(lines[3], "signature_count", "signature envelope")?,
            "signature_count",
        )? as usize;
        if !(1..=MAX_KEY_COUNT).contains(&count) || lines.len() != 4 + count {
            return Err(
                "signature_count must be 1 through 3 and match the record count".to_owned(),
            );
        }
        let mut records = Vec::with_capacity(count);
        let mut previous: Option<[u8; 32]> = None;
        for index in 0..count {
            let value = field(
                lines[4 + index],
                &format!("signature.{index}"),
                "signature envelope",
            )?;
            let (key_id, signature) = value
                .split_once(':')
                .ok_or_else(|| format!("signature.{index} has no key/signature separator"))?;
            if signature.contains(':') {
                return Err(format!("signature.{index} has an extra separator"));
            }
            let key_id = decode_hex::<32>(key_id, "signature key id")?;
            let signature = decode_hex::<64>(signature, "P1363 signature")?;
            if previous.as_ref().is_some_and(|last| last >= &key_id) {
                return Err("signature key IDs must be strictly ascending".to_owned());
            }
            previous = Some(key_id);
            records.push(SignatureRecord { key_id, signature });
        }
        Ok(Self {
            manifest_sha256,
            records,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyRole {
    Active,
    Standby,
    Retired,
    Revoked,
}

#[derive(Debug)]
struct PinnedKey {
    id: [u8; 32],
    role: KeyRole,
    x: [u8; 32],
    y: [u8; 32],
    trust_epoch: u64,
    not_before_sequence: u64,
    not_after_sequence: u64,
}

impl PinnedKey {
    fn authorizes(&self, manifest: &ReleaseManifest) -> bool {
        self.role != KeyRole::Revoked
            && self.trust_epoch == manifest.trust_epoch
            && (self.not_before_sequence..=self.not_after_sequence)
                .contains(&manifest.release_sequence)
    }
}

#[derive(Debug)]
struct Keyring {
    keys: Vec<PinnedKey>,
}

impl Keyring {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty()
            || bytes.len() > 16 * 1024
            || bytes.starts_with(&[0xef, 0xbb, 0xbf])
            || bytes.last() != Some(&b'\n')
            || bytes.contains(&b'\r')
            || bytes.contains(&0)
        {
            return Err("embedded keyring bytes are not canonical".to_owned());
        }
        let source =
            std::str::from_utf8(bytes).map_err(|_| "embedded keyring is not UTF-8".to_owned())?;
        let lines: Vec<_> = source[..source.len() - 1].split('\n').collect();
        if lines.len() < 9 || field(lines[0], "schema", "embedded keyring")? != "1" {
            return Err("embedded keyring schema is unsupported".to_owned());
        }
        let count = parse_decimal(
            field(lines[1], "key_count", "embedded keyring")?,
            "key_count",
        )? as usize;
        if !(1..=MAX_KEY_COUNT).contains(&count) || lines.len() != 2 + count * 7 {
            return Err("embedded keyring key_count is invalid".to_owned());
        }
        let mut keys = Vec::with_capacity(count);
        for index in 0..count {
            let offset = 2 + index * 7;
            let id = decode_hex::<32>(
                field(
                    lines[offset],
                    &format!("key.{index}.id"),
                    "embedded keyring",
                )?,
                "key id",
            )?;
            let role = match field(
                lines[offset + 1],
                &format!("key.{index}.role"),
                "embedded keyring",
            )? {
                "active" => KeyRole::Active,
                "standby" => KeyRole::Standby,
                "retired" => KeyRole::Retired,
                "revoked" => KeyRole::Revoked,
                _ => return Err(format!("embedded key {index} has an unknown role")),
            };
            let x = decode_hex::<32>(
                field(
                    lines[offset + 2],
                    &format!("key.{index}.x"),
                    "embedded keyring",
                )?,
                "P-256 X coordinate",
            )?;
            let y = decode_hex::<32>(
                field(
                    lines[offset + 3],
                    &format!("key.{index}.y"),
                    "embedded keyring",
                )?,
                "P-256 Y coordinate",
            )?;
            let trust_epoch = parse_decimal(
                field(
                    lines[offset + 4],
                    &format!("key.{index}.trust_epoch"),
                    "embedded keyring",
                )?,
                "key trust_epoch",
            )?;
            let not_before_sequence = parse_decimal(
                field(
                    lines[offset + 5],
                    &format!("key.{index}.not_before_sequence"),
                    "embedded keyring",
                )?,
                "key not_before_sequence",
            )?;
            let not_after_sequence = parse_decimal(
                field(
                    lines[offset + 6],
                    &format!("key.{index}.not_after_sequence"),
                    "embedded keyring",
                )?,
                "key not_after_sequence",
            )?;
            if not_before_sequence > not_after_sequence {
                return Err(format!("embedded key {index} has an empty sequence window"));
            }
            let mut id_input = Vec::with_capacity(KEY_ID_DOMAIN.len() + 64);
            id_input.extend_from_slice(KEY_ID_DOMAIN);
            id_input.extend_from_slice(&x);
            id_input.extend_from_slice(&y);
            if sha256_bytes(&id_input)? != id {
                return Err(format!("embedded key {index} ID does not match its point"));
            }
            if keys.iter().any(|key: &PinnedKey| key.id == id) {
                return Err("embedded keyring contains a duplicate key ID".to_owned());
            }
            keys.push(PinnedKey {
                id,
                role,
                x,
                y,
                trust_epoch,
                not_before_sequence,
                not_after_sequence,
            });
        }
        Ok(Self { keys })
    }
}

fn embedded_keyring() -> Result<&'static Keyring, String> {
    static KEYRING: OnceLock<Result<Keyring, String>> = OnceLock::new();
    match KEYRING.get_or_init(|| Keyring::parse(KEYRING_BYTES)) {
        Ok(keyring) => Ok(keyring),
        Err(error) => Err(error.clone()),
    }
}

pub fn verify_signed_manifest(
    manifest_bytes: &[u8],
    manifest: &ReleaseManifest,
    envelope_bytes: &[u8],
) -> Result<[u8; 32], String> {
    let envelope = SignatureEnvelope::parse(envelope_bytes)?;
    let manifest_sha256 = sha256_bytes(manifest_bytes)?;
    if envelope.manifest_sha256 != manifest_sha256 {
        return Err("signature envelope does not bind the received manifest bytes".to_owned());
    }
    let mut signed_input =
        Vec::with_capacity(MANIFEST_DOMAIN.len() + ALGORITHM_DOMAIN.len() + manifest_bytes.len());
    signed_input.extend_from_slice(MANIFEST_DOMAIN);
    signed_input.extend_from_slice(ALGORITHM_DOMAIN);
    signed_input.extend_from_slice(manifest_bytes);
    let signed_digest = sha256_bytes(&signed_input)?;
    let keyring = embedded_keyring()?;
    for record in &envelope.records {
        let key = keyring
            .keys
            .iter()
            .find(|key| key.id == record.key_id)
            .ok_or_else(|| format!("signature uses unknown key {}", encode_hex(&record.key_id)))?;
        if !key.authorizes(manifest) {
            return Err(format!(
                "signature key {} is not authorized for epoch {} sequence {}",
                encode_hex(&key.id),
                manifest.trust_epoch,
                manifest.release_sequence
            ));
        }
        verify_p256_signature(key, &signed_digest, &record.signature)?;
    }
    Ok(manifest_sha256)
}

pub(crate) fn sha256_bytes(input: &[u8]) -> Result<[u8; 32], String> {
    let algorithm = AlgorithmHandle::open(BCRYPT_SHA256_ALGORITHM)?;
    let mut digest = [0u8; 32];
    // SAFETY: the provider handle is live and CNG borrows both bounded slices
    // only for this synchronous one-shot hash operation.
    let status = unsafe { BCryptHash(algorithm.0, None, input, &mut digest) };
    nt_success(status.0, "BCryptHash(SHA-256)")?;
    Ok(digest)
}

fn verify_p256_signature(
    key: &PinnedKey,
    digest: &[u8; 32],
    signature: &[u8; 64],
) -> Result<(), String> {
    let algorithm = AlgorithmHandle::open(BCRYPT_ECDSA_P256_ALGORITHM)?;
    let mut blob = Vec::with_capacity(8 + 64);
    blob.extend_from_slice(&BCRYPT_ECDSA_PUBLIC_P256_MAGIC.to_ne_bytes());
    blob.extend_from_slice(&32u32.to_ne_bytes());
    blob.extend_from_slice(&key.x);
    blob.extend_from_slice(&key.y);
    let mut imported = BCRYPT_KEY_HANDLE::default();
    // SAFETY: the algorithm is an ECDSA P-256 provider, the blob has the
    // documented native header followed by exactly 32-byte big-endian X/Y,
    // and the output handle is initialized before inspection.
    let status = unsafe {
        BCryptImportKeyPair(
            algorithm.0,
            None,
            BCRYPT_ECCPUBLIC_BLOB,
            &mut imported,
            &blob,
            0,
        )
    };
    nt_success(status.0, "BCryptImportKeyPair(ECDSA P-256)")?;
    let imported = KeyHandle(imported);
    // SAFETY: the imported public key is live and both fixed-size inputs are
    // borrowed for the synchronous verification call.
    let status = unsafe {
        BCryptVerifySignature(imported.0, None, digest, signature, BCRYPT_FLAGS::default())
    };
    nt_success(status.0, "BCryptVerifySignature(ECDSA P-256)")
}

struct AlgorithmHandle(BCRYPT_ALG_HANDLE);

impl AlgorithmHandle {
    fn open(identifier: PCWSTR) -> Result<Self, String> {
        let mut handle = BCRYPT_ALG_HANDLE::default();
        // SAFETY: identifier is a static CNG algorithm string and the output
        // handle is initialized before inspection.
        let status = unsafe {
            BCryptOpenAlgorithmProvider(
                &mut handle,
                identifier,
                PCWSTR::null(),
                BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS::default(),
            )
        };
        nt_success(status.0, "BCryptOpenAlgorithmProvider")?;
        Ok(Self(handle))
    }
}

impl Drop for AlgorithmHandle {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: this wrapper owns the provider handle exactly once.
            unsafe {
                let _ = BCryptCloseAlgorithmProvider(self.0, 0);
            }
        }
    }
}

struct KeyHandle(BCRYPT_KEY_HANDLE);

impl Drop for KeyHandle {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: this wrapper owns the imported key handle exactly once.
            unsafe {
                let _ = BCryptDestroyKey(self.0);
            }
        }
    }
}

fn nt_success(status: i32, operation: &str) -> Result<(), String> {
    if status >= 0 {
        Ok(())
    } else {
        Err(format!(
            "{operation} failed with NTSTATUS 0x{:08x}",
            status as u32
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    Current,
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrustState {
    trust_epoch: u64,
    highest_sequence: u64,
    highest_version: Version,
    manifest_sha256: [u8; 32],
}

impl TrustState {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let lines = canonical_lines(bytes, 5, "update trust state", MAX_TRUST_STATE_BYTES)?;
        if field(lines[0], "schema", "update trust state")? != "1" {
            return Err("update trust state schema is unsupported".to_owned());
        }
        let trust_epoch = parse_decimal(
            field(lines[1], "trust_epoch", "update trust state")?,
            "trust_epoch",
        )?;
        let highest_sequence = parse_decimal(
            field(lines[2], "highest_sequence", "update trust state")?,
            "highest_sequence",
        )?;
        let highest_version =
            Version::parse(field(lines[3], "highest_version", "update trust state")?)?;
        let manifest_sha256 = decode_hex::<32>(
            field(lines[4], "manifest_sha256", "update trust state")?,
            "manifest_sha256",
        )?;
        let state = Self {
            trust_epoch,
            highest_sequence,
            highest_version,
            manifest_sha256,
        };
        let sequence_floor = embedded_sequence_floor()?;
        if state.trust_epoch != EMBEDDED_TRUST_EPOCH || state.highest_sequence < sequence_floor {
            return Err("update trust state is below the embedded trust floor".to_owned());
        }
        if state.canonical_text().as_bytes() != bytes {
            return Err("update trust state is not canonical".to_owned());
        }
        Ok(state)
    }

    fn canonical_text(&self) -> String {
        format!(
            "schema=1\ntrust_epoch={}\nhighest_sequence={}\nhighest_version={}\nmanifest_sha256={}\n",
            self.trust_epoch,
            self.highest_sequence,
            self.highest_version,
            encode_hex(&self.manifest_sha256)
        )
    }
}

#[derive(Debug)]
struct TrustPaths {
    state: PathBuf,
    state_lock: PathBuf,
    apply_lock: PathBuf,
}

impl TrustPaths {
    fn adjacent_to(installer: &Path) -> Result<Self, String> {
        let parent = installer
            .parent()
            .ok_or_else(|| "staged installer path has no parent directory".to_owned())?;
        Ok(Self {
            state: parent.join("trust-state.txt"),
            state_lock: parent.join("trust-state.lock"),
            apply_lock: parent.join("apply.lock"),
        })
    }
}

pub(crate) struct ExclusiveLock {
    _file: File,
}

pub(crate) fn acquire_apply_lock(
    installer: &Path,
    timeout: Duration,
) -> Result<ExclusiveLock, String> {
    let paths = TrustPaths::adjacent_to(installer)?;
    acquire_exclusive_lock(&paths.apply_lock, timeout)
        .map_err(|error| format!("could not acquire update single-flight lock: {error}"))
}

pub fn authorize_manifest(
    installer: &Path,
    current: Version,
    manifest: &ReleaseManifest,
    manifest_sha256: [u8; 32],
    timeout: Duration,
) -> Result<TrustDecision, String> {
    let sequence_floor = embedded_sequence_floor()?;
    if manifest.version < current {
        return Err(format!(
            "rollback rejected: release {} is older than running version {current}",
            manifest.version
        ));
    }
    if manifest.release_sequence < sequence_floor {
        return Err(format!(
            "rollback rejected: sequence {} is below embedded floor {sequence_floor}",
            manifest.release_sequence
        ));
    }

    let paths = TrustPaths::adjacent_to(installer)?;
    let _lock = acquire_exclusive_lock(&paths.state_lock, timeout)
        .map_err(|error| format!("could not acquire update trust-state lock: {error}"))?;
    let previous = read_trust_state(&paths.state)?
        .map(|bytes| TrustState::parse(&bytes))
        .transpose()?;

    let decision = if let Some(previous) = previous {
        if previous.trust_epoch != EMBEDDED_TRUST_EPOCH
            || manifest.trust_epoch != previous.trust_epoch
        {
            return Err("update trust-state epoch does not match the embedded keyring".to_owned());
        }
        if manifest.release_sequence < previous.highest_sequence {
            return Err(format!(
                "replay rejected: sequence {} is below observed sequence {}",
                manifest.release_sequence, previous.highest_sequence
            ));
        }
        if manifest.release_sequence == previous.highest_sequence {
            if manifest.version != previous.highest_version
                || manifest_sha256 != previous.manifest_sha256
            {
                return Err(
                    "equivocation rejected: an observed sequence changed identity".to_owned(),
                );
            }
            if manifest.version > current {
                TrustDecision::Available
            } else {
                TrustDecision::Current
            }
        } else {
            if manifest.version <= previous.highest_version {
                return Err(
                    "inconsistent release rejected: sequence increased without advancing the observed version"
                        .to_owned(),
                );
            }
            write_trust_state(
                &paths.state,
                TrustState {
                    trust_epoch: manifest.trust_epoch,
                    highest_sequence: manifest.release_sequence,
                    highest_version: manifest.version,
                    manifest_sha256,
                },
            )?;
            if manifest.version == current {
                TrustDecision::Current
            } else {
                TrustDecision::Available
            }
        }
    } else {
        write_trust_state(
            &paths.state,
            TrustState {
                trust_epoch: manifest.trust_epoch,
                highest_sequence: manifest.release_sequence,
                highest_version: manifest.version,
                manifest_sha256,
            },
        )?;
        if manifest.version == current {
            TrustDecision::Current
        } else {
            TrustDecision::Available
        }
    };
    Ok(decision)
}

fn write_trust_state(path: &Path, state: TrustState) -> Result<(), String> {
    atomic_replace_trust_state(path, state.canonical_text().as_bytes())
        .map_err(|error| format!("could not atomically write update trust state: {error}"))
}

fn read_trust_state(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not open update trust state: {error}")),
    };
    let length = file
        .metadata()
        .map_err(|error| format!("could not inspect update trust state: {error}"))?
        .len();
    if length > MAX_TRUST_STATE_BYTES {
        return Err(format!(
            "update trust state exceeds the {MAX_TRUST_STATE_BYTES}-byte limit"
        ));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_TRUST_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read update trust state: {error}"))?;
    if bytes.len() as u64 > MAX_TRUST_STATE_BYTES {
        return Err(format!(
            "update trust state exceeds the {MAX_TRUST_STATE_BYTES}-byte limit"
        ));
    }
    Ok(Some(bytes))
}

/// Publish sequence state without ever renaming the last accepted state away.
///
/// The repository-wide settings helper uses a backup/rename sequence with a
/// crash gap. Update rollback state cannot tolerate that gap: forgetting a
/// previously accepted sequence would reopen replay. The replacement is
/// therefore fully written and flushed in the same directory, then exchanged
/// with `ReplaceFileW`; first publication uses a write-through move.
fn atomic_replace_trust_state(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "state path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temporary_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("trust-state"))
        .to_os_string();
    temporary_name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        NEXT_TRUST_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = parent.join(temporary_name);

    let write_result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    let destination_exists = match fs::metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
    };
    let destination_wide = wide_os(path.as_os_str());
    let temporary_wide = wide_os(temporary.as_os_str());
    let publish_result = if destination_exists {
        // SAFETY: both same-directory paths are live, NUL-terminated UTF-16
        // buffers. No backup path is used, so the old state remains the named
        // destination unless the atomic replacement succeeds.
        unsafe {
            ReplaceFileW(
                PCWSTR(destination_wide.as_ptr()),
                PCWSTR(temporary_wide.as_ptr()),
                PCWSTR::null(),
                REPLACE_FILE_FLAGS::default(),
                None,
                None,
            )
        }
    } else {
        // SAFETY: both same-directory paths are live, NUL-terminated UTF-16
        // buffers. The destination is created by the filesystem rename and
        // WRITE_THROUGH requests durable publication before return.
        unsafe {
            MoveFileExW(
                PCWSTR(temporary_wide.as_ptr()),
                PCWSTR(destination_wide.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    match publish_result {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(io::Error::other(error))
        }
    }
}

fn wide_os(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(core::iter::once(0)).collect()
}

fn acquire_exclusive_lock(path: &Path, timeout: Duration) -> Result<ExclusiveLock, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "lock path has no parent directory".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create update staging directory: {error}"))?;
    let started = Instant::now();
    let mut delay = Duration::from_millis(10);
    let mut attempt = 0u32;
    loop {
        let result = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(path);
        match result {
            Ok(file) => return Ok(ExclusiveLock { _file: file }),
            Err(error)
                if matches!(error.raw_os_error(), Some(32 | 33)) && started.elapsed() < timeout =>
            {
                let jitter_seed =
                    std::process::id().wrapping_add(attempt.wrapping_mul(0x9e37_79b9));
                let jitter = Duration::from_millis(u64::from(jitter_seed % 7));
                let remaining = timeout.saturating_sub(started.elapsed());
                thread::sleep((delay + jitter).min(remaining));
                delay = (delay * 2).min(Duration::from_millis(100));
                attempt = attempt.wrapping_add(1);
            }
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => {
                return Err(format!(
                    "timed out after {} ms waiting for {}",
                    timeout.as_millis(),
                    path.display()
                ));
            }
            Err(error) => return Err(format!("could not open {}: {error}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PATH: AtomicU64 = AtomicU64::new(1);
    const FIXTURE_TIME: u64 = 1_800_000_000;

    fn fixture(name: &str) -> Vec<u8> {
        fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../verification/fixtures/update-signing-v2")
                .join(name),
        )
        .unwrap()
    }

    fn temp_installer(name: &str) -> PathBuf {
        let id = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "sakura-update-trust-{}-{name}-{id}",
                std::process::id()
            ))
            .join("sakura_setup.pending.exe")
    }

    #[test]
    fn embedded_sequence_floor_is_single_line_canonical_and_nonzero() {
        let floor = embedded_sequence_floor().unwrap();
        assert!(floor > 0);
        assert_eq!(RELEASE_SEQUENCE_BYTES, format!("{floor}\n").as_bytes());

        let malformed: &[&[u8]] = &[
            b"",
            b"1",
            b"\xef\xbb\xbf1\n",
            b"1\r\n",
            b"1\n2\n",
            b"1\n\n",
            b"01\n",
            b"0\n",
            b"+1\n",
            b" 1\n",
            b"1 \n",
            b"18446744073709551616\n",
            &[0xff, b'\n'],
        ];
        for bytes in malformed {
            assert!(parse_release_sequence_floor(bytes).is_err());
        }
    }

    #[test]
    fn positive_fixture_verifies_with_embedded_cng_key() {
        let manifest_bytes = fixture("manifest-positive.txt");
        let envelope_bytes = fixture("signature-positive.txt");
        // This immutable public cryptographic vector is sequence 1 by design.
        // Parse it against its frozen floor; production parsing still uses the
        // compiled floor through `ReleaseManifest::parse`.
        let manifest = ReleaseManifest::parse_with_sequence_floor(&manifest_bytes, 1).unwrap();
        manifest
            .validate_runtime(Version::parse("1.0.33").unwrap(), FIXTURE_TIME)
            .unwrap();
        assert_eq!(
            encode_hex(
                &verify_signed_manifest(&manifest_bytes, &manifest, &envelope_bytes).unwrap()
            ),
            "b90f4862b54c5643fac5f0188d2dbd0fae79feb7975f18620fdd731b33978340"
        );
        assert_eq!(embedded_keyring().unwrap().keys.len(), 2);
    }

    #[test]
    fn canonical_manifest_rejects_identity_policy_runtime_and_byte_ambiguity() {
        let bytes = fixture("manifest-positive.txt");
        let source = String::from_utf8(bytes.clone()).unwrap();
        for changed in [
            source.replace("schema=2", "schema=1"),
            source.replace("product=sakura-input", "product=other"),
            source.replace(
                "repository=tsuyoshi-otake/sakura-input",
                "repository=other/repo",
            ),
            source.replace("channel=stable", "channel=preview"),
            source.replace("platform=windows-x86_64", "platform=windows-arm64"),
            source.replace("trust_epoch=1", "trust_epoch=2"),
            source.replace("release_sequence=1", "release_sequence=0"),
            source.replace("tag=v1.0.33", "tag=v1.0.34"),
            source.replace("asset_name=sakura_setup.exe", "asset_name=other.exe"),
            source.replace("v1.0.33/sakura_setup.exe", "v1.0.34/sakura_setup.exe"),
            source.replace(
                "source_commit=964baf06d0ac451e319a847a2aabe796df7ea95f",
                "source_commit=NOT-A-COMMIT",
            ),
            source.replace(
                "sha256=3b5eb14b11efbd16920de1e0dd80d161adafb35d858c22bbcff18edf4a0367c5",
                "sha256=BAD",
            ),
            source.replace("size=56", "size=0"),
            source.replace("authenticode=unsigned", "authenticode=optional"),
            source.replace(
                "minimum_updater_version=1.0.33",
                "minimum_updater_version=01.0.33",
            ),
            source.replace("expires_unix=1893456000\n", ""),
            source.replace("size=56\n", "size=56\nsize=56\n"),
            format!("{source}comment=untrusted\n"),
            source.replace("product=sakura-input", "product=sakura-input "),
        ] {
            assert!(ReleaseManifest::parse_with_sequence_floor(changed.as_bytes(), 1).is_err());
        }
        let manifest = ReleaseManifest::parse_with_sequence_floor(&bytes, 1).unwrap();
        assert!(manifest
            .validate_runtime(Version::parse("1.0.32").unwrap(), FIXTURE_TIME)
            .is_err());
        assert!(manifest
            .validate_runtime(Version::parse("1.0.33").unwrap(), manifest.expires_unix)
            .is_err());

        let mut bom = vec![0xef, 0xbb, 0xbf];
        bom.extend_from_slice(&bytes);
        assert!(ReleaseManifest::parse_with_sequence_floor(&bom, 1).is_err());
        assert!(ReleaseManifest::parse_with_sequence_floor(&bytes[..bytes.len() - 1], 1).is_err());
        assert!(ReleaseManifest::parse_with_sequence_floor(
            &source.replace('\n', "\r\n").into_bytes(),
            1
        )
        .is_err());
        let mut reordered: Vec<_> = source.lines().collect();
        reordered.swap(1, 2);
        assert!(ReleaseManifest::parse_with_sequence_floor(
            format!("{}\n", reordered.join("\n")).as_bytes(),
            1
        )
        .is_err());
    }

    #[test]
    fn envelope_rejects_tamper_unknown_duplicate_unsorted_and_malformed_records() {
        let manifest_bytes = fixture("manifest-positive.txt");
        let manifest = ReleaseManifest::parse_with_sequence_floor(&manifest_bytes, 1).unwrap();
        let envelope = String::from_utf8(fixture("signature-positive.txt")).unwrap();
        let tampered = fixture("manifest-tampered.txt");
        let tampered_manifest = ReleaseManifest::parse_with_sequence_floor(&tampered, 1).unwrap();
        assert!(
            verify_signed_manifest(&tampered, &tampered_manifest, envelope.as_bytes()).is_err()
        );

        let unknown = envelope.replace(
            "178bc99d4699cde4b78c0169655d3b165140a62173812867a8a66b1a608b6c47",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        );
        assert!(verify_signed_manifest(&manifest_bytes, &manifest, unknown.as_bytes()).is_err());

        let record = envelope
            .lines()
            .find(|line| line.starts_with("signature.0="))
            .unwrap();
        let duplicate = envelope
            .replace("signature_count=1", "signature_count=2")
            .replace(
                record,
                &format!("{record}\n{}", record.replace("signature.0", "signature.1")),
            );
        assert!(SignatureEnvelope::parse(duplicate.as_bytes()).is_err());

        let (_, signature) = record.split_once(':').unwrap();
        let unsorted = format!(
            concat!(
                "schema=1\nalgorithm=ecdsa-p256-sha256-p1363\n",
                "manifest_sha256=b90f4862b54c5643fac5f0188d2dbd0fae79feb7975f18620fdd731b33978340\n",
                "signature_count=2\n",
                "signature.0=44e075680f1155c911119d9e039858a828757e37b901c2afe49fde3c4a0af92f:{}\n",
                "signature.1=178bc99d4699cde4b78c0169655d3b165140a62173812867a8a66b1a608b6c47:{}\n"
            ),
            signature, signature
        );
        assert!(SignatureEnvelope::parse(unsorted.as_bytes()).is_err());
        let dual_with_invalid_standby = envelope
            .replace("signature_count=1", "signature_count=2")
            .replace(
                record,
                &format!(
                    "{record}\nsignature.1=44e075680f1155c911119d9e039858a828757e37b901c2afe49fde3c4a0af92f:{signature}"
                ),
            );
        assert!(SignatureEnvelope::parse(dual_with_invalid_standby.as_bytes()).is_ok());
        assert!(verify_signed_manifest(
            &manifest_bytes,
            &manifest,
            dual_with_invalid_standby.as_bytes()
        )
        .is_err());
        assert!(SignatureEnvelope::parse(
            envelope
                .replace("signature_count=1", "signature_count=0")
                .as_bytes()
        )
        .is_err());
        assert!(SignatureEnvelope::parse(
            envelope
                .replace("signature_count=1", "signature_count=4")
                .as_bytes()
        )
        .is_err());
        assert!(SignatureEnvelope::parse(envelope.replace("dfdc", "DFDC").as_bytes()).is_err());
        assert!(SignatureEnvelope::parse(
            envelope
                .replace(
                    "dfdcffde2288af4ca6140a5103cb2b3ead1c60f890e988b050cdf72f7d79027521b840882b3281a39e020ca97a9655ba086534f79b62209e84cfa1f7ebbfa742",
                    "3006020101020101",
                )
                .as_bytes()
        )
        .is_err());
        assert!(SignatureEnvelope::parse(envelope.trim_end().as_bytes()).is_err());
    }

    #[test]
    fn trust_state_rejects_rollback_replay_equivocation_corruption_and_lock_contention() {
        let installer = temp_installer("state");
        let paths = TrustPaths::adjacent_to(&installer).unwrap();
        let current = Version::parse("1.0.33").unwrap();
        let mut manifest =
            ReleaseManifest::parse_with_sequence_floor(&fixture("manifest-positive.txt"), 1)
                .unwrap();
        manifest.release_sequence = embedded_sequence_floor().unwrap();
        let manifest_bytes = manifest.canonical_text().into_bytes();
        let digest = sha256_bytes(&manifest_bytes).unwrap();
        assert_eq!(
            authorize_manifest(
                &installer,
                current,
                &manifest,
                digest,
                Duration::from_secs(1)
            )
            .unwrap(),
            TrustDecision::Current
        );
        assert_eq!(
            authorize_manifest(
                &installer,
                current,
                &manifest,
                digest,
                Duration::from_secs(1)
            )
            .unwrap(),
            TrustDecision::Current
        );
        manifest.sha256[0] ^= 1;
        let different_digest = sha256_bytes(manifest.canonical_text().as_bytes()).unwrap();
        assert!(authorize_manifest(
            &installer,
            current,
            &manifest,
            different_digest,
            Duration::from_secs(1)
        )
        .is_err());

        fs::write(&paths.state, b"corrupt\n").unwrap();
        assert!(authorize_manifest(
            &installer,
            current,
            &manifest,
            digest,
            Duration::from_secs(1)
        )
        .is_err());

        fs::write(
            &paths.state,
            vec![b'x'; (MAX_TRUST_STATE_BYTES + 1) as usize],
        )
        .unwrap();
        assert!(authorize_manifest(
            &installer,
            current,
            &manifest,
            digest,
            Duration::from_secs(1)
        )
        .unwrap_err()
        .contains("exceeds"));

        let _held = acquire_exclusive_lock(&paths.state_lock, Duration::from_secs(1)).unwrap();
        assert!(acquire_exclusive_lock(&paths.state_lock, Duration::from_millis(20)).is_err());
        drop(_held);
        let _ = fs::remove_dir_all(installer.parent().unwrap());
    }

    #[test]
    fn manually_installed_current_version_advances_sequence_state() {
        let installer = temp_installer("manual-current");
        let mut first =
            ReleaseManifest::parse_with_sequence_floor(&fixture("manifest-positive.txt"), 1)
                .unwrap();
        first.release_sequence = embedded_sequence_floor().unwrap();
        let manifest_bytes = first.canonical_text().into_bytes();
        authorize_manifest(
            &installer,
            Version::parse("1.0.33").unwrap(),
            &first,
            sha256_bytes(&manifest_bytes).unwrap(),
            Duration::from_secs(1),
        )
        .unwrap();

        let mut next = first.clone();
        next.release_sequence = first.release_sequence + 1;
        next.version = Version::parse("1.0.34").unwrap();
        next.installer_url = installer_url_for(next.version);
        let next_bytes = next.canonical_text();
        assert_eq!(
            authorize_manifest(
                &installer,
                Version::parse("1.0.34").unwrap(),
                &next,
                sha256_bytes(next_bytes.as_bytes()).unwrap(),
                Duration::from_secs(1),
            )
            .unwrap(),
            TrustDecision::Current
        );
        let paths = TrustPaths::adjacent_to(&installer).unwrap();
        let state = TrustState::parse(&fs::read(&paths.state).unwrap()).unwrap();
        assert_eq!(state.highest_sequence, first.release_sequence + 1);
        assert_eq!(state.highest_version, Version::parse("1.0.34").unwrap());
        assert!(authorize_manifest(
            &installer,
            Version::parse("1.0.34").unwrap(),
            &first,
            sha256_bytes(&manifest_bytes).unwrap(),
            Duration::from_secs(1),
        )
        .is_err());
        let _ = fs::remove_dir_all(installer.parent().unwrap());
    }

    #[test]
    fn trust_state_atomic_replace_preserves_old_state_and_cleans_temporary() {
        let installer = temp_installer("atomic-replace");
        let paths = TrustPaths::adjacent_to(&installer).unwrap();
        atomic_replace_trust_state(&paths.state, b"old state\n").unwrap();
        assert_eq!(fs::read(&paths.state).unwrap(), b"old state\n");

        let held = OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001 | 0x0000_0002)
            .open(&paths.state)
            .unwrap();
        assert!(atomic_replace_trust_state(&paths.state, b"new state\n").is_err());
        assert_eq!(fs::read(&paths.state).unwrap(), b"old state\n");
        drop(held);

        atomic_replace_trust_state(&paths.state, b"new state\n").unwrap();
        assert_eq!(fs::read(&paths.state).unwrap(), b"new state\n");
        assert!(fs::read_dir(paths.state.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")));
        let _ = fs::remove_dir_all(installer.parent().unwrap());
    }

    #[test]
    fn trust_state_write_failure_is_terminal() {
        let installer = temp_installer("write-failure");
        let paths = TrustPaths::adjacent_to(&installer).unwrap();
        let mut first =
            ReleaseManifest::parse_with_sequence_floor(&fixture("manifest-positive.txt"), 1)
                .unwrap();
        first.release_sequence = embedded_sequence_floor().unwrap();
        let manifest_bytes = first.canonical_text().into_bytes();
        let first_digest = sha256_bytes(&manifest_bytes).unwrap();
        authorize_manifest(
            &installer,
            Version::parse("1.0.33").unwrap(),
            &first,
            first_digest,
            Duration::from_secs(1),
        )
        .unwrap();
        let old_state = fs::read(&paths.state).unwrap();

        let held = OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001 | 0x0000_0002)
            .open(&paths.state)
            .unwrap();
        let mut next = first;
        next.release_sequence += 1;
        next.version = Version::parse("1.0.34").unwrap();
        next.installer_url = installer_url_for(next.version);
        let next_digest = sha256_bytes(next.canonical_text().as_bytes()).unwrap();
        let error = authorize_manifest(
            &installer,
            Version::parse("1.0.33").unwrap(),
            &next,
            next_digest,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.contains("atomically write"));
        assert_eq!(fs::read(&paths.state).unwrap(), old_state);
        drop(held);

        assert_eq!(
            authorize_manifest(
                &installer,
                Version::parse("1.0.33").unwrap(),
                &next,
                next_digest,
                Duration::from_secs(1),
            )
            .unwrap(),
            TrustDecision::Available
        );
        let _ = fs::remove_dir_all(installer.parent().unwrap());
    }
}
