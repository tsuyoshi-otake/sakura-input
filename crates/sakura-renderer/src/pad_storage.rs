//! Bounded, current-user protected storage for Sakura Pad.
//!
//! The on-disk document is intentionally separate from the existing history
//! or settings formats.  It has its own magic/version, a monotonically
//! increasing generation, and an explicit tombstone per memo.  The complete
//! document is DPAPI protected with the current Windows user and is written
//! through a flushed temporary file before the documented first-write or
//! replacement operation.
//!
//! Version 2 holds a bounded *list* of memos.  The Issue #91 single-memo
//! document (`SKRLPAD\0`, version 1) still decodes and migrates in as the
//! first memo, so an existing pad is never lost and never overwritten blind.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH, REPLACE_FILE_FLAGS,
};

pub const MAX_TITLE_UTF16_UNITS: usize = 256;
pub const MAX_BODY_UTF16_UNITS: usize = 65_536;
/// A notepad, not a note store.  The list stays short enough that the whole
/// document can be re-encoded and re-protected on every save without the UI
/// noticing, and short enough that a full sync is a bounded number of GitHub
/// requests.
pub const MAX_MEMOS: usize = 200;
/// Whole-document ceiling, independent of how the text is distributed across
/// memos.  This is what actually bounds the protected blob; the per-memo
/// limits alone would allow far more.
pub const MAX_DOCUMENT_UTF16_UNITS: usize = 4_000_000;
/// A Git blob object id is 40 hexadecimal characters today.  The field is
/// sized for a SHA-256 object id so a future GitHub object format does not
/// require a document format change.
pub const MAX_REMOTE_SHA_LEN: usize = 64;
pub const DEBOUNCE: Duration = Duration::from_millis(300);
pub const SHUTDOWN_FLUSH_BUDGET: Duration = Duration::from_secs(2);

const MAGIC: [u8; 8] = *b"SKRLPAD2";
const VERSION: u16 = 2;
const DOCUMENT_HEADER_LEN: usize = 8 + 2 + 2 + 8 + 4 + 4;
const MEMO_HEADER_LEN: usize = 8 + 8 + 8 + 4 + 4 + 4 + 4 + 4;
const MEMO_TOMBSTONE: u32 = 0x0000_0001;

/// Issue #91's single-memo document.  Read-only: this build never writes it.
const LEGACY_MAGIC: [u8; 8] = *b"SKRLPAD\0";
const LEGACY_VERSION: u16 = 1;
const LEGACY_TOMBSTONE: u16 = 0x0001;
const LEGACY_HEADER_LEN: usize = 8 + 2 + 2 + 8 + 4 + 4;
const LEGACY_MEMO_ID: u64 = 1;

const MAX_PROTECTED_BYTES: u64 = 24 * 1024 * 1024;
const PROTECTED_MAGIC: [u8; 8] = *b"SKRLPAD3";
const PROTECTED_VERSION: u16 = 3;
const VAULT_ID_LEN: usize = 16;

/// A newer `SKRLPADn` magic in DPAPI plaintext identifies a future document
/// format and must not be treated as corruption during recovery. This only
/// detects visible versioned plaintext; raw protected v3 needs a separate
/// durable marker or versioned path because older builds cannot inspect it.
fn newer_document_magic_version(bytes: &[u8]) -> Option<u16> {
    let prefix = b"SKRLPAD";
    let version = *bytes.get(prefix.len())?;
    if bytes.starts_with(prefix) && version.is_ascii_digit() {
        let version = u16::from(version - b'0');
        (version > VERSION).then_some(version)
    } else {
        None
    }
}

/// Wall-clock milliseconds since the Unix epoch, used only for the memo's own
/// created/updated stamps.  A clock before the epoch reports 0, which the UI
/// renders the same way it renders a migrated v1 memo: as unknown.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// The order the list is presented in.  It belongs to the document because it
/// is a property of the pad the user arranged, not of this window instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PadSort {
    #[default]
    Updated,
    Created,
    Title,
}

impl PadSort {
    /// The order the single sort control cycles through. One control with a
    /// visible label is the whole affordance: three orders do not earn a menu.
    pub const fn next(self) -> Self {
        match self {
            Self::Updated => Self::Created,
            Self::Created => Self::Title,
            Self::Title => Self::Updated,
        }
    }

    /// What the sort control says it is currently doing.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Updated => "更新順",
            Self::Created => "作成順",
            Self::Title => "名前順",
        }
    }

    const fn code(self) -> u32 {
        match self {
            Self::Updated => 0,
            Self::Created => 1,
            Self::Title => 2,
        }
    }

    fn from_code(code: u32) -> Result<Self, StorageError> {
        match code {
            0 => Ok(Self::Updated),
            1 => Ok(Self::Created),
            2 => Ok(Self::Title),
            _ => Err(StorageError::InvalidFormat),
        }
    }
}

/// A user-visible memo, always kept within the UTF-16 limits before it enters
/// the worker mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadMemo {
    /// Stable within this document, and the key the GitHub file path is built
    /// from.  Never 0, and never reused while a tombstone still holds it.
    pub id: u64,
    pub title: String,
    pub body: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// Explicit user arrangement; ties fall back to the sort in effect.
    pub order: u32,
    pub tombstone: bool,
    /// Blob object id of the last revision this memo was synced at.  Empty
    /// until the memo has been pushed once.  ASCII hexadecimal only.
    pub remote_sha: String,
}

impl PadMemo {
    pub fn new(id: u64, title: impl AsRef<str>, body: impl AsRef<str>, now_ms: u64) -> Self {
        Self {
            id,
            title: truncate_utf16(title.as_ref(), MAX_TITLE_UTF16_UNITS),
            body: truncate_utf16(body.as_ref(), MAX_BODY_UTF16_UNITS),
            created_ms: now_ms,
            updated_ms: now_ms,
            order: 0,
            tombstone: false,
            remote_sha: String::new(),
        }
    }

    /// Replace the content and stamp the update, keeping identity and history.
    pub fn edit(&mut self, title: impl AsRef<str>, body: impl AsRef<str>, now_ms: u64) {
        self.title = truncate_utf16(title.as_ref(), MAX_TITLE_UTF16_UNITS);
        self.body = truncate_utf16(body.as_ref(), MAX_BODY_UTF16_UNITS);
        self.tombstone = false;
        self.updated_ms = now_ms;
    }

    /// Clear the content in place, keeping identity so the delete can sync.
    pub fn retire(&mut self, now_ms: u64) {
        self.title.clear();
        self.body.clear();
        self.tombstone = true;
        self.updated_ms = now_ms;
    }
}

/// The complete persisted pad: a bounded memo list plus the presentation
/// order, carried by one monotonically increasing generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PadDocument {
    pub generation: u64,
    pub sort: PadSort,
    pub memos: Vec<PadMemo>,
}

impl PadDocument {
    /// One past the highest id ever used here.  Tombstones stay in the list,
    /// so an id is never handed out twice while its deletion is unsynced.
    pub fn next_id(&self) -> u64 {
        self.memos
            .iter()
            .map(|memo| memo.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    pub fn live(&self) -> impl Iterator<Item = &PadMemo> {
        self.memos.iter().filter(|memo| !memo.tombstone)
    }

    pub fn find(&self, id: u64) -> Option<&PadMemo> {
        self.memos.iter().find(|memo| memo.id == id)
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut PadMemo> {
        self.memos.iter_mut().find(|memo| memo.id == id)
    }

    /// Return the memo with this id, inserting an empty one when the caller is
    /// editing a memo that has not been persisted yet.
    pub fn entry(&mut self, id: u64, now_ms: u64) -> Result<&mut PadMemo, StorageError> {
        if self.find(id).is_none() {
            if self.memos.len() >= MAX_MEMOS {
                return Err(StorageError::LimitExceeded);
            }
            self.memos.push(PadMemo::new(id, "", "", now_ms));
        }
        Ok(self.find_mut(id).expect("just inserted"))
    }

    pub fn encode(&self) -> Result<Vec<u8>, StorageError> {
        // Validation before encoding keeps every u32 length cast below exact.
        self.validate()?;
        let mut output = Vec::new();
        output.extend_from_slice(&MAGIC);
        output.extend_from_slice(&VERSION.to_le_bytes());
        output.extend_from_slice(&0u16.to_le_bytes());
        output.extend_from_slice(&self.generation.to_le_bytes());
        output.extend_from_slice(&(self.memos.len() as u32).to_le_bytes());
        output.extend_from_slice(&self.sort.code().to_le_bytes());
        for memo in &self.memos {
            let title = memo.title.encode_utf16().collect::<Vec<_>>();
            let body = memo.body.encode_utf16().collect::<Vec<_>>();
            let sha = memo.remote_sha.as_bytes();
            let flags = if memo.tombstone { MEMO_TOMBSTONE } else { 0 };
            output.extend_from_slice(&memo.id.to_le_bytes());
            output.extend_from_slice(&memo.created_ms.to_le_bytes());
            output.extend_from_slice(&memo.updated_ms.to_le_bytes());
            output.extend_from_slice(&memo.order.to_le_bytes());
            output.extend_from_slice(&flags.to_le_bytes());
            output.extend_from_slice(&(title.len() as u32).to_le_bytes());
            output.extend_from_slice(&(body.len() as u32).to_le_bytes());
            output.extend_from_slice(&(sha.len() as u32).to_le_bytes());
            for unit in title.into_iter().chain(body) {
                output.extend_from_slice(&unit.to_le_bytes());
            }
            output.extend_from_slice(sha);
        }
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, StorageError> {
        if bytes.len() >= LEGACY_MAGIC.len() && bytes[..LEGACY_MAGIC.len()] == LEGACY_MAGIC {
            return decode_legacy(bytes);
        }
        if bytes.len() < DOCUMENT_HEADER_LEN || bytes[..MAGIC.len()] != MAGIC {
            return Err(StorageError::InvalidFormat);
        }
        let version = read_u16(bytes, 8)?;
        if version != VERSION {
            return Err(StorageError::UnsupportedVersion(version));
        }
        // Reserved: an unknown document flag means a newer writer, not a hint.
        if read_u16(bytes, 10)? != 0 {
            return Err(StorageError::InvalidFormat);
        }
        let generation = read_u64(bytes, 12)?;
        let count = read_u32(bytes, 20)? as usize;
        let sort = PadSort::from_code(read_u32(bytes, 24)?)?;
        if count > MAX_MEMOS {
            return Err(StorageError::LimitExceeded);
        }
        let mut memos = Vec::with_capacity(count);
        let mut offset = DOCUMENT_HEADER_LEN;
        for _ in 0..count {
            let header_end = offset
                .checked_add(MEMO_HEADER_LEN)
                .ok_or(StorageError::InvalidFormat)?;
            if header_end > bytes.len() {
                return Err(StorageError::InvalidFormat);
            }
            let id = read_u64(bytes, offset)?;
            let created_ms = read_u64(bytes, offset + 8)?;
            let updated_ms = read_u64(bytes, offset + 16)?;
            let order = read_u32(bytes, offset + 24)?;
            let flags = read_u32(bytes, offset + 28)?;
            let title_units = read_u32(bytes, offset + 32)? as usize;
            let body_units = read_u32(bytes, offset + 36)? as usize;
            let sha_len = read_u32(bytes, offset + 40)? as usize;
            if flags & !MEMO_TOMBSTONE != 0 {
                return Err(StorageError::InvalidFormat);
            }
            if title_units > MAX_TITLE_UTF16_UNITS
                || body_units > MAX_BODY_UTF16_UNITS
                || sha_len > MAX_REMOTE_SHA_LEN
            {
                return Err(StorageError::LimitExceeded);
            }
            // Every length is bounded above, so these additions cannot wrap.
            let title_end = header_end + title_units * 2;
            let body_end = title_end + body_units * 2;
            let sha_end = body_end + sha_len;
            if sha_end > bytes.len() {
                return Err(StorageError::InvalidFormat);
            }
            let title = decode_utf16_units(&bytes[header_end..title_end])?;
            let body = decode_utf16_units(&bytes[title_end..body_end])?;
            let remote_sha = std::str::from_utf8(&bytes[body_end..sha_end])
                .map_err(|_| StorageError::InvalidFormat)?
                .to_owned();
            memos.push(PadMemo {
                id,
                title,
                body,
                created_ms,
                updated_ms,
                order,
                tombstone: flags & MEMO_TOMBSTONE != 0,
                remote_sha,
            });
            offset = sha_end;
        }
        if offset != bytes.len() {
            return Err(StorageError::InvalidFormat);
        }
        let document = Self {
            generation,
            sort,
            memos,
        };
        document.validate()?;
        Ok(document)
    }

    /// Every invariant the format promises, checked on both encode and decode
    /// so a document can never be written that this build refuses to read.
    fn validate(&self) -> Result<(), StorageError> {
        if self.memos.len() > MAX_MEMOS {
            return Err(StorageError::LimitExceeded);
        }
        let mut seen = Vec::with_capacity(self.memos.len());
        let mut total = 0usize;
        for memo in &self.memos {
            if memo.id == 0 || seen.contains(&memo.id) {
                return Err(StorageError::InvalidFormat);
            }
            seen.push(memo.id);
            if memo.remote_sha.len() > MAX_REMOTE_SHA_LEN
                || !memo.remote_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(StorageError::InvalidFormat);
            }
            if memo.tombstone && (!memo.title.is_empty() || !memo.body.is_empty()) {
                return Err(StorageError::InvalidFormat);
            }
            let title = memo.title.encode_utf16().count();
            let body = memo.body.encode_utf16().count();
            if title > MAX_TITLE_UTF16_UNITS || body > MAX_BODY_UTF16_UNITS {
                return Err(StorageError::LimitExceeded);
            }
            total = total
                .checked_add(title)
                .and_then(|used| used.checked_add(body))
                .ok_or(StorageError::InvalidFormat)?;
            if total > MAX_DOCUMENT_UTF16_UNITS {
                return Err(StorageError::LimitExceeded);
            }
        }
        Ok(())
    }
}

/// Decode the Issue #91 document.  A cleared v1 pad migrates to an empty list;
/// anything else becomes the first memo.  Version 1 recorded no memo identity
/// and no timestamps, so the migrated memo carries zero stamps, which the list
/// renders the same way it renders any unknown time.
fn decode_legacy(bytes: &[u8]) -> Result<PadDocument, StorageError> {
    if bytes.len() < LEGACY_HEADER_LEN {
        return Err(StorageError::InvalidFormat);
    }
    let version = read_u16(bytes, 8)?;
    if version != LEGACY_VERSION {
        return Err(StorageError::UnsupportedVersion(version));
    }
    let flags = read_u16(bytes, 10)?;
    if flags & !LEGACY_TOMBSTONE != 0 {
        return Err(StorageError::InvalidFormat);
    }
    let generation = read_u64(bytes, 12)?;
    let title_units = read_u32(bytes, 20)? as usize;
    let body_units = read_u32(bytes, 24)? as usize;
    if title_units > MAX_TITLE_UTF16_UNITS || body_units > MAX_BODY_UTF16_UNITS {
        return Err(StorageError::LimitExceeded);
    }
    // Both lengths are bounded above, so neither product can wrap.
    let title_bytes = title_units * 2;
    let body_bytes = body_units * 2;
    if LEGACY_HEADER_LEN + title_bytes + body_bytes != bytes.len() {
        return Err(StorageError::InvalidFormat);
    }
    let title = decode_utf16_units(&bytes[LEGACY_HEADER_LEN..LEGACY_HEADER_LEN + title_bytes])?;
    let body = decode_utf16_units(&bytes[LEGACY_HEADER_LEN + title_bytes..])?;
    let cleared = flags & LEGACY_TOMBSTONE != 0;
    if cleared && (!title.is_empty() || !body.is_empty()) {
        return Err(StorageError::InvalidFormat);
    }
    let memos = if cleared {
        Vec::new()
    } else {
        vec![PadMemo {
            id: LEGACY_MEMO_ID,
            title,
            body,
            created_ms: 0,
            updated_ms: 0,
            order: 0,
            tombstone: false,
            remote_sha: String::new(),
        }]
    };
    Ok(PadDocument {
        generation,
        sort: PadSort::default(),
        memos,
    })
}

/// Truncate at a UTF-16 unit boundary without leaving a lone surrogate in the
/// encoded document.  A Rust `char` is either one or two UTF-16 units.
pub fn truncate_utf16(value: &str, max_units: usize) -> String {
    let mut used = 0usize;
    let mut end = 0usize;
    for (index, character) in value.char_indices() {
        let units = character.len_utf16();
        if used + units > max_units {
            break;
        }
        used += units;
        end = index + character.len_utf8();
    }
    value[..end].to_owned()
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, StorageError> {
    let end = offset.checked_add(2).ok_or(StorageError::InvalidFormat)?;
    bytes
        .get(offset..end)
        .and_then(|slice| slice.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(StorageError::InvalidFormat)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, StorageError> {
    let end = offset.checked_add(4).ok_or(StorageError::InvalidFormat)?;
    bytes
        .get(offset..end)
        .and_then(|slice| slice.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(StorageError::InvalidFormat)
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, StorageError> {
    let end = offset.checked_add(8).ok_or(StorageError::InvalidFormat)?;
    bytes
        .get(offset..end)
        .and_then(|slice| slice.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or(StorageError::InvalidFormat)
}

fn decode_utf16_units(bytes: &[u8]) -> Result<String, StorageError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(StorageError::InvalidFormat);
    }
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
    String::from_utf16(&units.collect::<Vec<_>>()).map_err(|_| StorageError::InvalidFormat)
}

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    Win(windows::core::Error),
    InvalidFormat,
    UnsupportedVersion(u16),
    LimitExceeded,
    MissingLocalAppData,
    CryptoBufferTooLarge,
    TempConflict,
    LegacyChanged,
    ProtectedCutover,
    ProtectedVerification,
    StaleProtectedDocument,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "pad storage I/O failed: {error}"),
            Self::Win(error) => write!(f, "pad storage Windows operation failed: {error}"),
            Self::InvalidFormat => f.write_str("pad storage format is invalid"),
            Self::UnsupportedVersion(version) => {
                write!(f, "pad storage version {version} is unsupported")
            }
            Self::LimitExceeded => f.write_str("pad document exceeds its bounds"),
            Self::MissingLocalAppData => f.write_str("LOCALAPPDATA is not available"),
            Self::CryptoBufferTooLarge => f.write_str("DPAPI buffer is too large"),
            Self::TempConflict => {
                f.write_str("a newer or unreadable pad recovery file is already present")
            }
            Self::LegacyChanged => {
                f.write_str("pad changed while protected migration was prepared")
            }
            Self::ProtectedCutover => {
                f.write_str("pad protected cutover requires protected recovery")
            }
            Self::ProtectedVerification => {
                f.write_str("protected pad copy failed open verification")
            }
            Self::StaleProtectedDocument => f.write_str("protected pad changed before this save"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<io::Error> for StorageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<windows::core::Error> for StorageError {
    fn from(error: windows::core::Error) -> Self {
        Self::Win(error)
    }
}

/// The primary path and its single recovery backup.  `load` tries the
/// primary first, then the backup; a corrupt primary is deliberately left in
/// place for diagnosis, while a successful backup read is reported through
/// `recovered_from_backup` so callers can surface the partial failure.
#[derive(Debug, Clone)]
pub struct PadStore {
    path: PathBuf,
    backup: PathBuf,
    temp: PathBuf,
    lock: PathBuf,
    protected_path: PathBuf,
    protected_backup: PathBuf,
    protected_temp: PathBuf,
    intent: PathBuf,
    marker: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadOutcome {
    pub document: PadDocument,
    pub recovered_from_backup: bool,
}

impl PadStore {
    pub fn default() -> Result<Self, StorageError> {
        let root = std::env::var_os("LOCALAPPDATA").ok_or(StorageError::MissingLocalAppData)?;
        Ok(Self::at(Path::new(&root).join("SakuraInput").join("pad")))
    }

    pub fn at(directory: impl AsRef<Path>) -> Self {
        let directory = directory.as_ref();
        Self {
            path: directory.join("memo.bin"),
            backup: directory.join("memo.bin.bak"),
            temp: directory.join("memo.bin.tmp"),
            lock: directory.join("memo.bin.lock"),
            protected_path: directory.join("memo.v3.bin"),
            protected_backup: directory.join("memo.v3.bin.bak"),
            protected_temp: directory.join("memo.v3.bin.tmp"),
            intent: directory.join("memo.v3.committing"),
            marker: directory.join("memo.v3.marker"),
        }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<LoadOutcome, StorageError> {
        let _lock = self.exclusive_writer()?;
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> Result<LoadOutcome, StorageError> {
        self.require_legacy_mode()?;
        match read_document(&self.path) {
            Ok(document) => {
                return Ok(LoadOutcome {
                    document,
                    recovered_from_backup: false,
                });
            }
            Err(error @ StorageError::UnsupportedVersion(_)) => return Err(error),
            Err(_) => {}
        }
        match read_document(&self.backup) {
            Ok(document) => {
                return Ok(LoadOutcome {
                    document,
                    recovered_from_backup: true,
                });
            }
            Err(error @ StorageError::UnsupportedVersion(_)) => return Err(error),
            Err(_) => {}
        }
        // A flushed temp is considered only after both published copies have
        // failed. A valid primary (including an empty list) always wins, so an
        // unpublished older edit can never resurrect deleted content.
        match read_document(&self.temp) {
            Ok(document) => {
                return Ok(LoadOutcome {
                    document,
                    recovered_from_backup: true,
                });
            }
            Err(error @ StorageError::UnsupportedVersion(_)) => return Err(error),
            Err(_) => {}
        }
        if self.path.try_exists()? || self.backup.try_exists()? || self.temp.try_exists()? {
            // Existing but unreadable data is a partial failure, not an empty
            // document.  The UI may still start empty, but the worker's caller
            // can display the error/recovery state and must not save over it.
            return Err(StorageError::InvalidFormat);
        }
        Ok(LoadOutcome {
            document: PadDocument::default(),
            recovered_from_backup: false,
        })
    }

    pub fn write(&self, document: &PadDocument) -> Result<WriteOutcome, StorageError> {
        let _lock = self.exclusive_writer()?;
        self.require_legacy_mode()?;
        let mut encoded = document.encode()?;
        let protected_result = protect(&encoded);
        encoded.fill(0);
        let protected = protected_result?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        prepare_temp(&self.temp, document.generation)?;
        write_flushed_temp(&self.temp, &protected)?;
        if !self.path.try_exists()? {
            // First write: the target does not exist, so MoveFileExW is the
            // only operation and it gets WRITE_THROUGH for the directory
            // entry.  If a concurrent writer wins, leave the temp file for
            // recovery rather than silently replacing it.
            move_first_write(&self.temp, &self.path)?;
            Ok(WriteOutcome::FirstWrite)
        } else {
            // Updates use exactly one backup and flags=0.  The replacement is
            // atomic from the reader's point of view; a failed call leaves
            // `memo.bin.tmp` available for a later recovery/diagnostic pass.
            replace_update(&self.path, &self.temp, &self.backup)?;
            Ok(WriteOutcome::Replaced)
        }
    }

    fn exclusive_writer(&self) -> Result<File, StorageError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        // A persistent lock pathname is harmless. The exclusive Windows handle
        // is released on process death, unlike a create_new lockfile.
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&self.lock)?)
    }

    fn require_legacy_mode(&self) -> Result<(), StorageError> {
        // Presence is authoritative, including a damaged marker. A tombstone
        // also blocks fallback if both independent markers are lost.
        if self.intent.try_exists()? || self.marker.try_exists()? {
            return Err(StorageError::ProtectedCutover);
        }
        for path in [&self.path, &self.backup, &self.temp] {
            if path.try_exists()? {
                if let Err(error @ StorageError::UnsupportedVersion(_)) = read_document(path) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn require_active_protected(&self) -> Result<[u8; VAULT_ID_LEN], StorageError> {
        // Both independently protected signals are required. Their presence
        // blocks legacy readers even if either signal is damaged.
        if !self.intent.try_exists()? || !self.marker.try_exists()? {
            return Err(StorageError::ProtectedCutover);
        }
        let intent_id = read_protected_signal(&self.intent, b"committing")?;
        let marker_id = read_protected_signal(&self.marker, b"committed")?;
        if intent_id != marker_id {
            return Err(StorageError::ProtectedCutover);
        }
        Ok(intent_id)
    }

    pub fn protected_vault_id(&self) -> Result<[u8; VAULT_ID_LEN], StorageError> {
        let _lock = self.exclusive_writer()?;
        self.require_active_protected()
    }

    /// Read only the durable cutover intent's identity. This is a recovery
    /// hint, never authorization to display content: the caller must still
    /// authenticate both protected copies before publishing a final marker.
    pub fn pending_vault_id(&self) -> Result<[u8; VAULT_ID_LEN], StorageError> {
        let _lock = self.exclusive_writer()?;
        if !self.intent.try_exists()? {
            return Err(StorageError::ProtectedCutover);
        }
        read_protected_signal(&self.intent, b"committing")
    }

    /// Open authenticated v3 primary first, then its published backup. The
    /// callback must return the vault ID authenticated by that envelope, not
    /// an ID supplied separately by the caller. No legacy or unpublished temp
    /// bytes are consulted in protected mode.
    pub fn load_protected<F>(&self, mut open_v3: F) -> Result<LoadOutcome, StorageError>
    where
        F: FnMut(&Path) -> Result<([u8; VAULT_ID_LEN], PadDocument), StorageError>,
    {
        let _lock = self.exclusive_writer()?;
        self.load_protected_unlocked(&mut open_v3)
    }

    fn load_protected_unlocked<F>(&self, open_v3: &mut F) -> Result<LoadOutcome, StorageError>
    where
        F: FnMut(&Path) -> Result<([u8; VAULT_ID_LEN], PadDocument), StorageError>,
    {
        let vault_id = self.require_active_protected()?;
        match open_v3(&self.protected_path) {
            Ok((opened_id, document)) if opened_id == vault_id => Ok(LoadOutcome {
                document,
                recovered_from_backup: false,
            }),
            Ok(_) => Err(StorageError::ProtectedVerification),
            Err(error @ StorageError::UnsupportedVersion(_)) => Err(error),
            Err(_) => match open_v3(&self.protected_backup) {
                Ok((opened_id, document)) if opened_id == vault_id => Ok(LoadOutcome {
                    document,
                    recovered_from_backup: true,
                }),
                Ok(_) => Err(StorageError::ProtectedVerification),
                Err(error @ StorageError::UnsupportedVersion(_)) => Err(error),
                Err(_) => Err(StorageError::ProtectedVerification),
            },
        }
    }

    /// Compare the authenticated published document with the caller's exact
    /// expected value, verify the staged ciphertext opens as `next`, then
    /// atomically publish it while retaining one protected recovery copy.
    pub fn write_protected<F>(
        &self,
        expected: &PadDocument,
        next: &PadDocument,
        encrypted_v3: &[u8],
        open_v3: F,
    ) -> Result<WriteOutcome, StorageError>
    where
        F: FnMut(&Path) -> Result<([u8; VAULT_ID_LEN], PadDocument), StorageError>,
    {
        self.write_protected_with_hook(expected, next, encrypted_v3, open_v3, |_| Ok(()))
    }

    fn write_protected_with_hook<F, H>(
        &self,
        expected: &PadDocument,
        next: &PadDocument,
        encrypted_v3: &[u8],
        mut open_v3: F,
        mut hook: H,
    ) -> Result<WriteOutcome, StorageError>
    where
        F: FnMut(&Path) -> Result<([u8; VAULT_ID_LEN], PadDocument), StorageError>,
        H: FnMut(ProtectedWritePoint) -> Result<(), StorageError>,
    {
        if next.generation <= expected.generation
            || encrypted_v3.is_empty()
            || encrypted_v3.len() as u64 > MAX_PROTECTED_BYTES
        {
            return Err(StorageError::InvalidFormat);
        }
        let mut validated = next.encode()?;
        validated.fill(0);
        let _lock = self.exclusive_writer()?;
        let vault_id = self.require_active_protected()?;
        let loaded = self.load_protected_unlocked(&mut open_v3)?;
        if loaded.recovered_from_backup {
            return Err(StorageError::ProtectedVerification);
        }
        if loaded.document != *expected {
            return Err(StorageError::StaleProtectedDocument);
        }
        if self.protected_temp.try_exists()? {
            return Err(StorageError::TempConflict);
        }
        write_flushed_temp(&self.protected_temp, encrypted_v3)?;
        hook(ProtectedWritePoint::Staged)?;
        let (opened_id, opened) =
            open_v3(&self.protected_temp).map_err(|_| StorageError::ProtectedVerification)?;
        if opened_id != vault_id || opened != *next {
            return Err(StorageError::ProtectedVerification);
        }
        hook(ProtectedWritePoint::Verified)?;
        replace_update(
            &self.protected_path,
            &self.protected_temp,
            &self.protected_backup,
        )?;
        hook(ProtectedWritePoint::Published)?;
        Ok(WriteOutcome::Replaced)
    }

    /// Stage already-encrypted v3 bytes, open *both* durable copies through the
    /// caller's future-format reader, and cut over only if the exact legacy
    /// document and generation still match. The caller owns future-format
    /// encryption and recovery; this store never decrypts the staged bytes.
    /// An error after publishing `intent` is terminal for legacy load/write.
    pub fn migrate_to_protected<F>(
        &self,
        expected_legacy: &PadDocument,
        expected_generation: u64,
        vault_id: [u8; VAULT_ID_LEN],
        encrypted_v3: &[u8],
        open_v3: F,
    ) -> Result<(), StorageError>
    where
        F: FnMut(&Path) -> Result<([u8; VAULT_ID_LEN], PadDocument), StorageError>,
    {
        self.migrate_to_protected_with_hook(
            expected_legacy,
            expected_generation,
            vault_id,
            encrypted_v3,
            open_v3,
            |_| Ok(()),
        )
    }

    fn migrate_to_protected_with_hook<F, H>(
        &self,
        expected_legacy: &PadDocument,
        expected_generation: u64,
        vault_id: [u8; VAULT_ID_LEN],
        encrypted_v3: &[u8],
        mut open_v3: F,
        mut hook: H,
    ) -> Result<(), StorageError>
    where
        F: FnMut(&Path) -> Result<([u8; VAULT_ID_LEN], PadDocument), StorageError>,
        H: FnMut(MigrationPoint) -> Result<(), StorageError>,
    {
        if vault_id == [0; VAULT_ID_LEN]
            || expected_legacy.generation != expected_generation
            || encrypted_v3.is_empty()
            || encrypted_v3.len() as u64 > MAX_PROTECTED_BYTES
        {
            return Err(StorageError::InvalidFormat);
        }
        let _lock = self.exclusive_writer()?;
        self.require_legacy_mode()?;
        self.require_expected_legacy(expected_legacy)?;
        // An interrupted pre-intent attempt can reuse only byte-identical
        // staged copies. Unknown protected evidence is never overwritten.
        stage_protected_copy(&self.protected_temp, &self.protected_path, encrypted_v3)?;
        hook(MigrationPoint::FirstProtectedCopy)?;
        stage_protected_copy(&self.protected_temp, &self.protected_backup, encrypted_v3)?;
        if self.protected_temp.try_exists()? {
            return Err(StorageError::TempConflict);
        }
        hook(MigrationPoint::SecondProtectedCopy)?;
        for path in [&self.protected_path, &self.protected_backup] {
            let (opened_id, opened) =
                open_v3(path).map_err(|_| StorageError::ProtectedVerification)?;
            if opened_id != vault_id || opened != *expected_legacy {
                return Err(StorageError::ProtectedVerification);
            }
        }
        hook(MigrationPoint::CopiesVerified)?;
        // The exclusive writer handle makes this the final legacy comparison.
        // Before intent exists, any failure leaves legacy publication intact.
        self.require_expected_legacy(expected_legacy)?;
        remove_if_present(&signal_temp_path(&self.intent))?;
        publish_protected_signal(&self.intent, b"committing", vault_id)?;
        hook(MigrationPoint::IntentPublished)?;
        // Retirement starts only after durable intent. Every subsequent error
        // leaves new-build load/write closed to v1/v2, with v3 copies intact.
        remove_if_present(&self.backup)?;
        remove_if_present(&self.temp)?;
        hook(MigrationPoint::LegacyRecoveryRetired)?;
        let mut tombstone = Vec::from(PROTECTED_MAGIC);
        tombstone.extend_from_slice(&PROTECTED_VERSION.to_le_bytes());
        let protected_tombstone = protect(&tombstone)?;
        let retire_temp = self.path.with_extension("bin.retire.tmp");
        write_flushed_temp(&retire_temp, &protected_tombstone)?;
        if self.path.try_exists()? {
            replace_without_backup(&self.path, &retire_temp)?;
        } else {
            // A never-saved empty Pad has no legacy primary to replace.
            // Intent is already durable, so first publication cannot expose
            // a v2 document or leave an old backup behind.
            move_first_write(&retire_temp, &self.path)?;
        }
        hook(MigrationPoint::LegacyTombstoned)?;
        publish_protected_signal(&self.marker, b"committed", vault_id)?;
        hook(MigrationPoint::FinalMarkerPublished)?;
        Ok(())
    }

    fn require_expected_legacy(&self, expected: &PadDocument) -> Result<(), StorageError> {
        let primary_exists = self.path.try_exists()?;
        let loaded = self.load_unlocked()?;
        if loaded.recovered_from_backup
            || loaded.document != *expected
            || (!primary_exists && *expected != PadDocument::default())
        {
            return Err(StorageError::LegacyChanged);
        }
        Ok(())
    }

    /// Resume only an interrupted protected cutover. Recovery consults v3
    /// copies exclusively; no legacy document can authorize the transition.
    pub fn recover_protected_cutover<F>(&self, mut open_v3: F) -> Result<PadDocument, StorageError>
    where
        F: FnMut(&Path) -> Result<([u8; VAULT_ID_LEN], PadDocument), StorageError>,
    {
        let _lock = self.exclusive_writer()?;
        if !self.intent.try_exists()? {
            return Err(StorageError::ProtectedCutover);
        }
        let vault_id = read_protected_signal(&self.intent, b"committing")?;
        if self.marker.try_exists()?
            && read_protected_signal(&self.marker, b"committed")? != vault_id
        {
            return Err(StorageError::ProtectedCutover);
        }
        let (primary_id, primary) =
            open_v3(&self.protected_path).map_err(|_| StorageError::ProtectedVerification)?;
        let (backup_id, backup) =
            open_v3(&self.protected_backup).map_err(|_| StorageError::ProtectedVerification)?;
        if primary_id != vault_id || backup_id != vault_id || primary != backup {
            return Err(StorageError::ProtectedVerification);
        }
        remove_if_present(&self.backup)?;
        remove_if_present(&self.temp)?;
        let mut tombstone = Vec::from(PROTECTED_MAGIC);
        tombstone.extend_from_slice(&PROTECTED_VERSION.to_le_bytes());
        let protected_tombstone = protect(&tombstone)?;
        let retire_temp = self.path.with_extension("bin.retire.tmp");
        // A previous replacement may already have completed. Do not create a
        // legacy backup or touch the tombstone on that path.
        let already_tombstoned = match read_document(&self.path) {
            Err(StorageError::UnsupportedVersion(PROTECTED_VERSION)) => true,
            Err(error @ StorageError::UnsupportedVersion(_)) => return Err(error),
            _ => false,
        };
        if !already_tombstoned {
            remove_if_present(&retire_temp)?;
            write_flushed_temp(&retire_temp, &protected_tombstone)?;
            if self.path.try_exists()? {
                replace_without_backup(&self.path, &retire_temp)?;
            } else {
                move_first_write(&retire_temp, &self.path)?;
            }
        }
        if !self.marker.try_exists()? {
            // A crash during final marker creation may leave only its flushed
            // temp. Intent already commits this path to v3-only recovery.
            remove_if_present(&signal_temp_path(&self.marker))?;
            publish_protected_signal(&self.marker, b"committed", vault_id)?;
        }
        Ok(primary)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationPoint {
    FirstProtectedCopy,
    SecondProtectedCopy,
    CopiesVerified,
    IntentPublished,
    LegacyRecoveryRetired,
    LegacyTombstoned,
    FinalMarkerPublished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtectedWritePoint {
    Staged,
    Verified,
    Published,
}

fn remove_if_present(path: &Path) -> Result<(), StorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn stage_protected_copy(temp: &Path, target: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    if target.try_exists()? {
        if fs::metadata(target)?.len() != bytes.len() as u64 || fs::read(target)? != bytes {
            return Err(StorageError::TempConflict);
        }
        return Ok(());
    }
    if temp.try_exists()? {
        return Err(StorageError::TempConflict);
    }
    write_flushed_temp(temp, bytes)?;
    move_first_write(temp, target)
}

fn publish_protected_signal(
    path: &Path,
    state: &[u8],
    vault_id: [u8; VAULT_ID_LEN],
) -> Result<(), StorageError> {
    if vault_id == [0; VAULT_ID_LEN] {
        return Err(StorageError::InvalidFormat);
    }
    let mut plaintext = Vec::from(PROTECTED_MAGIC);
    plaintext.extend_from_slice(&PROTECTED_VERSION.to_le_bytes());
    plaintext.extend_from_slice(state);
    plaintext.extend_from_slice(&vault_id);
    let protected = protect(&plaintext)?;
    let temp = signal_temp_path(path);
    write_flushed_temp(&temp, &protected)?;
    move_first_write(&temp, path)
}

fn read_protected_signal(path: &Path, state: &[u8]) -> Result<[u8; VAULT_ID_LEN], StorageError> {
    let plaintext = read_decrypted(path).map_err(|_| StorageError::ProtectedCutover)?;
    let prefix_len = PROTECTED_MAGIC.len() + 2 + state.len();
    if plaintext.len() != prefix_len + VAULT_ID_LEN
        || plaintext[..PROTECTED_MAGIC.len()] != PROTECTED_MAGIC
        || plaintext[PROTECTED_MAGIC.len()..PROTECTED_MAGIC.len() + 2]
            != PROTECTED_VERSION.to_le_bytes()
        || &plaintext[PROTECTED_MAGIC.len() + 2..prefix_len] != state
    {
        return Err(StorageError::ProtectedCutover);
    }
    let vault_id: [u8; VAULT_ID_LEN] = plaintext[prefix_len..]
        .try_into()
        .map_err(|_| StorageError::ProtectedCutover)?;
    if vault_id == [0; VAULT_ID_LEN] {
        return Err(StorageError::ProtectedCutover);
    }
    Ok(vault_id)
}

fn signal_temp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

fn write_flushed_temp(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn prepare_temp(path: &Path, next_generation: u64) -> Result<(), StorageError> {
    if !path.exists() {
        return Ok(());
    }
    match read_document(path) {
        Ok(document) if document.generation < next_generation => {
            fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) | Err(_) => Err(StorageError::TempConflict),
    }
}

fn path_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn move_first_write(temp: &Path, target: &Path) -> Result<(), StorageError> {
    let temp_wide = path_wide(temp);
    let target_wide = path_wide(target);
    // SAFETY: both buffers are NUL-terminated and live through the call.
    unsafe {
        MoveFileExW(
            windows::core::PCWSTR(temp_wide.as_ptr()),
            windows::core::PCWSTR(target_wide.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )?
    }
    Ok(())
}

fn replace_update(target: &Path, temp: &Path, backup: &Path) -> Result<(), StorageError> {
    let target_wide = path_wide(target);
    let temp_wide = path_wide(temp);
    let backup_wide = path_wide(backup);
    // SAFETY: all three buffers are NUL-terminated and remain live for the
    // synchronous ReplaceFileW call.  Flags are deliberately zero: the
    // first-write durability contract belongs to MoveFileExW above.
    unsafe {
        ReplaceFileW(
            windows::core::PCWSTR(target_wide.as_ptr()),
            windows::core::PCWSTR(temp_wide.as_ptr()),
            windows::core::PCWSTR(backup_wide.as_ptr()),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )?
    }
    Ok(())
}

fn replace_without_backup(target: &Path, temp: &Path) -> Result<(), StorageError> {
    let target_wide = path_wide(target);
    let temp_wide = path_wide(temp);
    // SAFETY: both NUL-terminated buffers live for the synchronous call.
    // Null backup deliberately prevents publishing readable legacy bytes.
    unsafe {
        ReplaceFileW(
            windows::core::PCWSTR(target_wide.as_ptr()),
            windows::core::PCWSTR(temp_wide.as_ptr()),
            windows::core::PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )?
    }
    Ok(())
}

fn read_decrypted(path: &Path) -> Result<Vec<u8>, StorageError> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_PROTECTED_BYTES {
        return Err(StorageError::CryptoBufferTooLarge);
    }
    let mut protected = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut protected)?;
    unprotect(&protected)
}

fn read_document(path: &Path) -> Result<PadDocument, StorageError> {
    let mut plaintext = read_decrypted(path)?;
    let decoded = match newer_document_magic_version(&plaintext) {
        Some(version) => Err(StorageError::UnsupportedVersion(version)),
        None => PadDocument::decode(&plaintext),
    };
    plaintext.fill(0);
    decoded
}

fn protect(bytes: &[u8]) -> Result<Vec<u8>, StorageError> {
    let cb_data = u32::try_from(bytes.len()).map_err(|_| StorageError::CryptoBufferTooLarge)?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: cb_data,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input points at immutable bytes for the duration of this call;
    // DPAPI allocates its output with LocalAlloc, released below.
    unsafe {
        CryptProtectData(
            &input,
            windows::core::PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
    }
    let result = copy_blob(&output);
    free_blob(&mut output, false);
    result
}

fn unprotect(bytes: &[u8]) -> Result<Vec<u8>, StorageError> {
    let cb_data = u32::try_from(bytes.len()).map_err(|_| StorageError::CryptoBufferTooLarge)?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: cb_data,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input is immutable for the call; the output is owned by DPAPI
    // and released after copying it into an ordinary bounded Vec.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
    }
    let result = copy_blob(&output);
    free_blob(&mut output, true);
    result
}

fn copy_blob(blob: &CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, StorageError> {
    if blob.cbData == 0 {
        return Ok(Vec::new());
    }
    if blob.pbData.is_null() || blob.cbData as u64 > MAX_PROTECTED_BYTES {
        return Err(StorageError::CryptoBufferTooLarge);
    }
    // SAFETY: DPAPI returned a valid allocation of cbData bytes.
    Ok(unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec())
}

fn free_blob(blob: &mut CRYPT_INTEGER_BLOB, clear: bool) {
    if !blob.pbData.is_null() {
        // SAFETY: CryptProtect/UnprotectData allocate output with LocalAlloc.
        unsafe {
            if clear {
                std::ptr::write_bytes(blob.pbData, 0, blob.cbData as usize);
            }
            let _ = LocalFree(Some(HLOCAL(blob.pbData.cast())));
        }
        blob.pbData = std::ptr::null_mut();
        blob.cbData = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    FirstWrite,
    Replaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStatus {
    Written(WriteOutcome),
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveCompletion {
    pub generation: u64,
    pub status: SaveStatus,
}

#[derive(Debug)]
struct MailboxState {
    pending: Option<PadDocument>,
    last_generation: u64,
    closing: bool,
}

#[derive(Debug)]
struct Mailbox {
    state: Mutex<MailboxState>,
    wake: Condvar,
}

/// One worker and one latest-value mailbox.  A newer generation replaces an
/// older pending value, while a stale generation is rejected before it can
/// overwrite the mailbox.  Completion carries the generation so a UI that
/// has already moved on cannot apply an old result.
#[derive(Debug)]
pub struct StorageWorker {
    mailbox: Arc<Mailbox>,
    completions: mpsc::Receiver<SaveCompletion>,
    done: mpsc::Receiver<()>,
    join: Option<JoinHandle<()>>,
    closed: bool,
}

impl StorageWorker {
    pub fn spawn(store: PadStore) -> Result<Self, StorageError> {
        let mailbox = Arc::new(Mailbox {
            state: Mutex::new(MailboxState {
                pending: None,
                last_generation: 0,
                closing: false,
            }),
            wake: Condvar::new(),
        });
        let (completion_sender, completions) = mpsc::channel();
        let (done_sender, done) = mpsc::channel();
        let worker_mailbox = Arc::clone(&mailbox);
        let join = thread::Builder::new()
            .name("sakura-pad-storage".to_owned())
            .spawn(move || worker_loop(worker_mailbox, store, completion_sender, done_sender))?;
        Ok(Self {
            mailbox,
            completions,
            done,
            join: Some(join),
            closed: false,
        })
    }

    /// Submit only newer generations.  The UI may call this for every edit;
    /// no unbounded queue is created and the worker sleeps through the 300 ms
    /// debounce until the latest value is stable.
    pub fn submit(&self, document: PadDocument) -> bool {
        let mut state = self
            .mailbox
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.closing
            || document.generation <= state.last_generation
            || state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.generation >= document.generation)
        {
            return false;
        }
        state.pending = Some(document);
        self.mailbox.wake.notify_one();
        true
    }

    pub fn try_completion(&self) -> Option<SaveCompletion> {
        self.completions.try_recv().ok()
    }

    /// Signal shutdown and wait only for the bounded flush budget.  A worker
    /// that has not acknowledged within the budget is detached; the UI does
    /// not block process teardown on an unbounded filesystem call.
    pub fn shutdown(&mut self, budget: Duration) -> bool {
        if self.closed {
            return true;
        }
        self.closed = true;
        {
            let mut state = self
                .mailbox
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.closing = true;
            self.mailbox.wake.notify_one();
        }
        let finished = self.done.recv_timeout(budget).is_ok();
        if finished {
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
        finished
    }
}

impl Drop for StorageWorker {
    fn drop(&mut self) {
        let _ = self.shutdown(SHUTDOWN_FLUSH_BUDGET);
    }
}

fn worker_loop(
    mailbox: Arc<Mailbox>,
    store: PadStore,
    completions: mpsc::Sender<SaveCompletion>,
    done: mpsc::Sender<()>,
) {
    loop {
        let document = {
            let mut state = mailbox
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if state.pending.is_some() {
                    break;
                }
                if state.closing {
                    let _ = done.send(());
                    return;
                }
                state = mailbox
                    .wake
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }

            // Debounce until the current pending generation remains unchanged
            // for 300 ms.  A shutdown cuts this wait short and flushes the
            // latest value immediately.
            loop {
                let generation = state
                    .pending
                    .as_ref()
                    .map(|document| document.generation)
                    .unwrap_or(0);
                let deadline = Instant::now() + DEBOUNCE;
                while Instant::now() < deadline && !state.closing {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let (next, timeout) = mailbox
                        .wake
                        .wait_timeout(state, remaining)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state = next;
                    if timeout.timed_out() {
                        break;
                    }
                    if state
                        .pending
                        .as_ref()
                        .is_none_or(|document| document.generation == generation)
                    {
                        continue;
                    }
                    // Newer value arrived: restart the full debounce window.
                    break;
                }
                let changed = state
                    .pending
                    .as_ref()
                    .is_some_and(|document| document.generation != generation);
                if changed && !state.closing {
                    continue;
                }
                break;
            }
            state.pending.take()
        };

        if let Some(document) = document {
            let generation = document.generation;
            let status = match store.write(&document) {
                Ok(outcome) => SaveStatus::Written(outcome),
                Err(_) => SaveStatus::Failed,
            };
            {
                let mut state = mailbox
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.last_generation = state.last_generation.max(generation);
            }
            let _ = completions.send(SaveCompletion { generation, status });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Barrier;

    static NEXT: AtomicU64 = AtomicU64::new(1);
    const TEST_VAULT_ID: [u8; VAULT_ID_LEN] = [7; VAULT_ID_LEN];

    fn temp_dir() -> PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "sakura-pad-storage-test-{}-{id}",
            std::process::id()
        ))
    }

    fn document(generation: u64, memos: Vec<PadMemo>) -> PadDocument {
        PadDocument {
            generation,
            sort: PadSort::default(),
            memos,
        }
    }

    fn one(title: &str, body: &str, generation: u64) -> PadDocument {
        document(
            generation,
            vec![PadMemo::new(1, title, body, 1_700_000_000)],
        )
    }

    /// The Issue #91 document, byte for byte, so migration is tested against
    /// the real format rather than against this build's encoder.
    fn legacy_bytes(title: &str, body: &str, generation: u64, cleared: bool) -> Vec<u8> {
        let title: Vec<u16> = title.encode_utf16().collect();
        let body: Vec<u16> = body.encode_utf16().collect();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&LEGACY_MAGIC);
        bytes.extend_from_slice(&LEGACY_VERSION.to_le_bytes());
        bytes.extend_from_slice(&(if cleared { LEGACY_TOMBSTONE } else { 0 }).to_le_bytes());
        bytes.extend_from_slice(&generation.to_le_bytes());
        bytes.extend_from_slice(&(title.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        for unit in title.into_iter().chain(body) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn document_roundtrip_has_dedicated_header_generation_and_per_memo_tombstone() {
        let mut deleted = PadMemo::new(2, "消した", "本文", 1_700_000_001);
        deleted.retire(1_700_000_002);
        deleted.order = 3;
        let source = PadDocument {
            generation: 42,
            sort: PadSort::Title,
            memos: vec![PadMemo::new(1, "題名", "本文🙂", 1_700_000_000), deleted],
        };
        let encoded = source.encode().expect("encode");
        assert_eq!(&encoded[..8], &MAGIC);
        assert_eq!(u16::from_le_bytes([encoded[8], encoded[9]]), VERSION);
        assert_eq!(u64::from_le_bytes(encoded[12..20].try_into().unwrap()), 42);
        assert_eq!(u32::from_le_bytes(encoded[20..24].try_into().unwrap()), 2);
        assert_eq!(PadDocument::decode(&encoded).unwrap(), source);
    }

    #[test]
    fn utf16_limits_do_not_split_surrogate_pairs() {
        let title = truncate_utf16(&"a🙂b".repeat(200), MAX_TITLE_UTF16_UNITS);
        assert!(title.encode_utf16().count() <= MAX_TITLE_UTF16_UNITS);
        assert!(std::str::from_utf8(title.as_bytes()).is_ok());
        let decoded = String::from_utf16(&title.encode_utf16().collect::<Vec<_>>());
        assert!(decoded.is_ok());
    }

    #[test]
    fn malformed_headers_and_lengths_fail_closed() {
        let source = one("title", "body", 1);
        let mut bytes = source.encode().unwrap();
        bytes[8] = 0xff;
        assert!(matches!(
            PadDocument::decode(&bytes),
            Err(StorageError::UnsupportedVersion(_))
        ));
        // A reserved document flag is a newer writer, never a hint to ignore.
        let mut bytes = source.encode().unwrap();
        bytes[10] = 0x01;
        assert!(matches!(
            PadDocument::decode(&bytes),
            Err(StorageError::InvalidFormat)
        ));
        // Declared count beyond the bound, and a title length beyond the bound.
        let mut bytes = source.encode().unwrap();
        bytes[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(PadDocument::decode(&bytes).is_err());
        let mut bytes = source.encode().unwrap();
        let title_len = DOCUMENT_HEADER_LEN + 32;
        bytes[title_len..title_len + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(PadDocument::decode(&bytes).is_err());
        // Trailing bytes after the last memo are a truncation/append attempt.
        let mut bytes = source.encode().unwrap();
        bytes.push(0);
        assert!(matches!(
            PadDocument::decode(&bytes),
            Err(StorageError::InvalidFormat)
        ));
        // An unknown sort code is refused rather than silently defaulted.
        let mut bytes = source.encode().unwrap();
        bytes[24..28].copy_from_slice(&7u32.to_le_bytes());
        assert!(matches!(
            PadDocument::decode(&bytes),
            Err(StorageError::InvalidFormat)
        ));
    }

    #[test]
    fn document_invariants_are_rejected_on_encode_and_never_reach_disk() {
        let zero_id = document(1, vec![PadMemo::new(0, "a", "b", 1)]);
        assert!(matches!(zero_id.encode(), Err(StorageError::InvalidFormat)));
        let duplicate = document(
            1,
            vec![PadMemo::new(3, "a", "b", 1), PadMemo::new(3, "c", "d", 1)],
        );
        assert!(matches!(
            duplicate.encode(),
            Err(StorageError::InvalidFormat)
        ));
        let mut carrying = PadMemo::new(1, "still here", "content", 1);
        carrying.tombstone = true;
        assert!(matches!(
            document(1, vec![carrying]).encode(),
            Err(StorageError::InvalidFormat)
        ));
        let mut bad_sha = PadMemo::new(1, "a", "b", 1);
        bad_sha.remote_sha = "not-hex".to_owned();
        assert!(matches!(
            document(1, vec![bad_sha]).encode(),
            Err(StorageError::InvalidFormat)
        ));
        let too_many = document(
            1,
            (1..=MAX_MEMOS as u64 + 1)
                .map(|id| PadMemo::new(id, "", "", 1))
                .collect(),
        );
        assert!(matches!(
            too_many.encode(),
            Err(StorageError::LimitExceeded)
        ));
    }

    #[test]
    fn the_whole_document_ceiling_bounds_the_protected_blob() {
        let body = "a".repeat(MAX_BODY_UTF16_UNITS);
        // Enough full-size bodies to cross 4,000,000 UTF-16 units.
        let count = MAX_DOCUMENT_UTF16_UNITS / MAX_BODY_UTF16_UNITS + 1;
        let memos = (1..=count as u64)
            .map(|id| PadMemo::new(id, "", &body, 1))
            .collect::<Vec<_>>();
        assert!(count <= MAX_MEMOS);
        assert!(matches!(
            document(1, memos).encode(),
            Err(StorageError::LimitExceeded)
        ));
    }

    #[test]
    fn a_version_one_document_migrates_in_as_the_first_memo() {
        let migrated = PadDocument::decode(&legacy_bytes("旧題名", "旧本文🙂", 9, false)).unwrap();
        assert_eq!(migrated.generation, 9);
        assert_eq!(migrated.sort, PadSort::Updated);
        assert_eq!(migrated.memos.len(), 1);
        let memo = &migrated.memos[0];
        assert_eq!(memo.id, LEGACY_MEMO_ID);
        assert_eq!(memo.title, "旧題名");
        assert_eq!(memo.body, "旧本文🙂");
        assert!(!memo.tombstone);
        // Version 1 had no timestamps; unknown is recorded as unknown.
        assert_eq!((memo.created_ms, memo.updated_ms), (0, 0));
        assert!(memo.remote_sha.is_empty());
        assert_eq!(migrated.next_id(), LEGACY_MEMO_ID + 1);
    }

    #[test]
    fn a_version_one_tombstone_migrates_to_an_empty_list() {
        let migrated = PadDocument::decode(&legacy_bytes("", "", 4, true)).unwrap();
        assert_eq!(migrated.generation, 4);
        assert!(migrated.memos.is_empty());
        assert_eq!(migrated.next_id(), 1);
        // A cleared v1 document that still carries content is tampering.
        let mut tampered = legacy_bytes("kept", "", 4, false);
        tampered[10] = LEGACY_TOMBSTONE as u8;
        assert!(matches!(
            PadDocument::decode(&tampered),
            Err(StorageError::InvalidFormat)
        ));
    }

    #[test]
    fn an_existing_version_one_file_is_migrated_by_the_store_and_saved_as_v2() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        fs::create_dir_all(&directory).unwrap();
        let protected = protect(&legacy_bytes("引き継ぎ", "本文", 5, false)).unwrap();
        fs::write(store.path(), protected).unwrap();

        let loaded = store.load().unwrap();
        assert!(!loaded.recovered_from_backup);
        assert_eq!(loaded.document.memos.len(), 1);
        assert_eq!(loaded.document.memos[0].title, "引き継ぎ");

        let mut next = loaded.document.clone();
        next.generation = 6;
        assert_eq!(store.write(&next).unwrap(), WriteOutcome::Replaced);
        let reloaded = store.load().unwrap();
        assert_eq!(reloaded.document, next);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn dpapi_store_uses_primary_then_single_backup_recovery() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let first = one("one", "first", 1);
        assert_eq!(store.write(&first).unwrap(), WriteOutcome::FirstWrite);
        assert_eq!(store.load().unwrap().document, first);
        let second = one("two", "second", 2);
        assert_eq!(store.write(&second).unwrap(), WriteOutcome::Replaced);
        assert_eq!(store.load().unwrap().document, second);
        fs::write(store.path(), b"not a pad document").unwrap();
        let recovered = store.load().unwrap();
        assert!(recovered.recovered_from_backup);
        assert_eq!(recovered.document, first);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn newer_primary_refuses_fallback_to_valid_v2_backup() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(&store.path, protect(b"SKRLPAD3future").unwrap()).unwrap();
        fs::write(
            &store.backup,
            protect(&one("old", "backup", 2).encode().unwrap()).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            store.load(),
            Err(StorageError::UnsupportedVersion(3))
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn newer_backup_refuses_fallback_to_valid_v2_temp() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(&store.path, b"corrupt primary").unwrap();
        fs::write(&store.backup, protect(b"SKRLPAD3future").unwrap()).unwrap();
        fs::write(
            &store.temp,
            protect(&one("old", "temp", 2).encode().unwrap()).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            store.load(),
            Err(StorageError::UnsupportedVersion(3))
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn corrupt_primary_still_recovers_from_valid_v2_backup() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let backup = one("recover", "valid v2 backup", 2);
        fs::create_dir_all(&directory).unwrap();
        fs::write(&store.path, b"corrupt primary").unwrap();
        fs::write(&store.backup, protect(&backup.encode().unwrap()).unwrap()).unwrap();

        let loaded = store.load().unwrap();
        assert!(loaded.recovered_from_backup);
        assert_eq!(loaded.document, backup);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn a_third_save_replaces_an_existing_backup() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        store.write(&one("one", "first", 1)).unwrap();
        store.write(&one("two", "second", 2)).unwrap();
        assert_eq!(
            store.write(&one("three", "third", 3)).unwrap(),
            WriteOutcome::Replaced
        );
        assert_eq!(store.load().unwrap().document.generation, 3);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn valid_temp_recovers_only_when_no_published_copy_exists() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let source = one("recovery", "flushed temp", 7);
        store.write(&source).unwrap();
        fs::rename(&store.path, &store.temp).unwrap();
        let recovered = store.load().unwrap();
        assert!(recovered.recovered_from_backup);
        assert_eq!(recovered.document, source);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn unreadable_temp_is_preserved_and_blocks_overwrite() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(&store.temp, b"unreadable recovery evidence").unwrap();
        assert!(matches!(
            store.write(&one("new", "content", 1)),
            Err(StorageError::TempConflict)
        ));
        assert_eq!(
            fs::read(&store.temp).unwrap(),
            b"unreadable recovery evidence"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn a_published_deletion_prevents_backup_resurrection() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        store.write(&one("old", "must stay deleted", 1)).unwrap();
        let mut deleted = one("old", "must stay deleted", 2);
        deleted.memos[0].retire(1_700_000_002);
        store.write(&deleted).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.document, deleted);
        assert_eq!(loaded.document.live().count(), 0);
        assert!(!loaded.recovered_from_backup);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn latest_generation_mailbox_rejects_stale_submit_and_flushes_shutdown() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let mut worker = StorageWorker::spawn(store.clone()).expect("worker starts");
        assert!(worker.submit(one("one", "one", 1)));
        assert!(worker.submit(one("two", "two", 2)));
        assert!(!worker.submit(one("stale", "stale", 1)));
        assert!(worker.shutdown(Duration::from_secs(2)));
        assert_eq!(store.load().unwrap().document.generation, 2);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn ids_are_never_reused_while_a_tombstone_still_holds_them() {
        let mut source = one("first", "body", 1);
        let next = source.next_id();
        assert_eq!(next, 2);
        source.memos[0].retire(1_700_000_005);
        // The deleted memo stays in the list, so its id stays taken.
        assert_eq!(source.next_id(), 2);
        source.entry(next, 1_700_000_006).unwrap();
        assert_eq!(source.next_id(), 3);
        assert_eq!(source.live().count(), 1);
    }

    fn open_staged(
        expected: &PadDocument,
        path: &Path,
    ) -> Result<([u8; VAULT_ID_LEN], PadDocument), StorageError> {
        if fs::read(path)? != b"future encrypted payload" {
            return Err(StorageError::ProtectedVerification);
        }
        Ok((TEST_VAULT_ID, expected.clone()))
    }

    #[test]
    fn a_never_saved_empty_pad_can_cut_over_without_publishing_v2() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let empty = PadDocument::default();
        assert_eq!(store.load().unwrap().document, empty);
        assert!(!store.path.exists());
        store
            .migrate_to_protected(
                &empty,
                empty.generation,
                TEST_VAULT_ID,
                b"future encrypted payload",
                |path| open_staged(&empty, path),
            )
            .unwrap();
        assert!(!store.backup.exists());
        assert!(!store.temp.exists());
        assert_eq!(read_decrypted(&store.path).unwrap()[..8], PROTECTED_MAGIC);
        assert_eq!(store.protected_vault_id().unwrap(), TEST_VAULT_ID);
        assert_eq!(
            store
                .load_protected(|path| open_staged(&empty, path))
                .unwrap()
                .document,
            empty
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn protected_migration_verifies_both_copies_and_retires_legacy_recovery() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let old = one("legacy", "private", 5);
        store.write(&old).unwrap();
        store.write(&one("legacy", "private", 6)).unwrap();
        let expected = one("legacy", "private", 6);
        fs::write(&store.temp, protect(&old.encode().unwrap()).unwrap()).unwrap();
        store
            .migrate_to_protected(
                &expected,
                6,
                TEST_VAULT_ID,
                b"future encrypted payload",
                |path| open_staged(&expected, path),
            )
            .unwrap();
        assert!(!store.backup.exists());
        assert!(!store.temp.exists());
        assert!(store.intent.exists());
        assert!(store.marker.exists());
        assert_eq!(read_decrypted(&store.path).unwrap()[..8], PROTECTED_MAGIC);
        assert!(matches!(store.load(), Err(StorageError::ProtectedCutover)));
        assert!(matches!(
            store.write(&expected),
            Err(StorageError::ProtectedCutover)
        ));
        fs::write(&store.marker, b"corrupt marker").unwrap();
        fs::remove_file(&store.protected_path).unwrap();
        fs::remove_file(&store.protected_backup).unwrap();
        assert!(matches!(store.load(), Err(StorageError::ProtectedCutover)));
        fs::remove_file(&store.intent).unwrap();
        fs::remove_file(&store.marker).unwrap();
        assert!(matches!(
            store.load(),
            Err(StorageError::UnsupportedVersion(3))
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn protected_migration_refuses_changed_legacy_and_backup_recovery() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let expected = one("original", "body", 1);
        store.write(&expected).unwrap();
        let next = one("edit", "body", 2);
        assert!(matches!(
            store.migrate_to_protected(
                &expected,
                1,
                TEST_VAULT_ID,
                b"future encrypted payload",
                |path| {
                    let _ = fs::read(path)?;
                    fs::write(&store.path, protect(&next.encode().unwrap()).unwrap())?;
                    Ok((TEST_VAULT_ID, expected.clone()))
                }
            ),
            Err(StorageError::LegacyChanged)
        ));
        assert_eq!(store.load().unwrap().document, next);
        assert!(!store.intent.exists());
        assert!(!store.marker.exists());
        let _ = fs::remove_dir_all(&directory);

        let directory = temp_dir();
        let store = PadStore::at(&directory);
        store.write(&expected).unwrap();
        store.write(&next).unwrap();
        fs::write(&store.path, b"corrupt primary").unwrap();
        assert!(matches!(
            store.migrate_to_protected(
                &expected,
                1,
                TEST_VAULT_ID,
                b"future encrypted payload",
                |_| { Ok((TEST_VAULT_ID, expected.clone())) }
            ),
            Err(StorageError::LegacyChanged)
        ));
        assert!(!store.intent.exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn failed_open_of_second_protected_copy_preserves_legacy_publication() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let expected = one("legacy", "body", 1);
        store.write(&expected).unwrap();
        let mut opened = Vec::new();
        let result = store.migrate_to_protected(
            &expected,
            1,
            TEST_VAULT_ID,
            b"future encrypted payload",
            |path| {
                opened.push(path.to_path_buf());
                if path == store.protected_backup.as_path() {
                    Err(StorageError::ProtectedVerification)
                } else {
                    open_staged(&expected, path)
                }
            },
        );
        assert!(matches!(result, Err(StorageError::ProtectedVerification)));
        assert_eq!(
            opened,
            vec![store.protected_path.clone(), store.protected_backup.clone()]
        );
        assert_eq!(store.load().unwrap().document, expected);
        assert!(!store.intent.exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn interruption_boundaries_keep_legacy_before_intent_and_recover_only_v3_after() {
        let points = [
            MigrationPoint::FirstProtectedCopy,
            MigrationPoint::SecondProtectedCopy,
            MigrationPoint::CopiesVerified,
            MigrationPoint::IntentPublished,
            MigrationPoint::LegacyRecoveryRetired,
            MigrationPoint::LegacyTombstoned,
            MigrationPoint::FinalMarkerPublished,
        ];
        for point in points {
            let directory = temp_dir();
            let store = PadStore::at(&directory);
            let previous = one("previous", "backup", 1);
            let expected = one("current", "body", 2);
            store.write(&previous).unwrap();
            store.write(&expected).unwrap();
            let result = store.migrate_to_protected_with_hook(
                &expected,
                2,
                TEST_VAULT_ID,
                b"future encrypted payload",
                |path| open_staged(&expected, path),
                |reached| {
                    if reached == point {
                        Err(StorageError::Io(io::Error::other("injected interruption")))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err(), "{point:?}");
            let before_intent = matches!(
                point,
                MigrationPoint::FirstProtectedCopy
                    | MigrationPoint::SecondProtectedCopy
                    | MigrationPoint::CopiesVerified
            );
            if before_intent {
                assert!(matches!(
                    store.pending_vault_id(),
                    Err(StorageError::ProtectedCutover)
                ));
                assert_eq!(store.load().unwrap().document, expected, "{point:?}");
                assert!(store.backup.exists(), "{point:?}");
                store
                    .migrate_to_protected(
                        &expected,
                        2,
                        TEST_VAULT_ID,
                        b"future encrypted payload",
                        |path| open_staged(&expected, path),
                    )
                    .unwrap();
                assert!(matches!(store.load(), Err(StorageError::ProtectedCutover)));
            } else {
                assert_eq!(store.pending_vault_id().unwrap(), TEST_VAULT_ID);
                assert!(matches!(store.load(), Err(StorageError::ProtectedCutover)));
                assert!(matches!(
                    store.write(&expected),
                    Err(StorageError::ProtectedCutover)
                ));
                assert!(matches!(
                    store.recover_protected_cutover(|path| {
                        let (_, document) = open_staged(&expected, path)?;
                        Ok(([9; VAULT_ID_LEN], document))
                    }),
                    Err(StorageError::ProtectedVerification)
                ));
                assert_eq!(
                    store
                        .recover_protected_cutover(|path| open_staged(&expected, path))
                        .unwrap(),
                    expected,
                    "{point:?}"
                );
                assert!(!store.backup.exists(), "{point:?}");
                assert!(!store.temp.exists(), "{point:?}");
                assert!(store.marker.exists(), "{point:?}");
            }
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn load_and_write_cannot_observe_legacy_across_migration_cutover() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let expected = one("before", "private", 1);
        store.write(&expected).unwrap();
        let at_verification = Arc::new(Barrier::new(2));
        let finish_verification = Arc::new(Barrier::new(2));
        let thread_store = store.clone();
        let thread_expected = expected.clone();
        let arrived = Arc::clone(&at_verification);
        let release = Arc::clone(&finish_verification);
        let migration = thread::spawn(move || {
            thread_store.migrate_to_protected_with_hook(
                &thread_expected,
                1,
                TEST_VAULT_ID,
                b"future encrypted payload",
                |path| open_staged(&thread_expected, path),
                |point| {
                    if point == MigrationPoint::CopiesVerified {
                        arrived.wait();
                        release.wait();
                    }
                    Ok(())
                },
            )
        });
        at_verification.wait();
        // While migration owns the writer handle, neither operation may
        // return a legacy document or publish a legacy save.
        assert!(store.load().is_err());
        assert!(store.write(&expected).is_err());
        finish_verification.wait();
        migration.join().unwrap().unwrap();
        assert!(matches!(store.load(), Err(StorageError::ProtectedCutover)));
        assert!(matches!(
            store.write(&expected),
            Err(StorageError::ProtectedCutover)
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn protected_signal_temp_paths_do_not_overlap() {
        let store = PadStore::at(temp_dir());
        assert_ne!(
            signal_temp_path(&store.intent),
            signal_temp_path(&store.marker)
        );
        assert_ne!(signal_temp_path(&store.intent), store.protected_temp);
    }

    fn open_protected_test(
        old: &PadDocument,
        next: &PadDocument,
        path: &Path,
    ) -> Result<([u8; VAULT_ID_LEN], PadDocument), StorageError> {
        let encrypted = fs::read(path)?;
        if encrypted == b"future encrypted payload" {
            Ok((TEST_VAULT_ID, old.clone()))
        } else if encrypted == b"new encrypted payload" {
            Ok((TEST_VAULT_ID, next.clone()))
        } else {
            Err(StorageError::ProtectedVerification)
        }
    }

    fn migrated_store() -> (PathBuf, PadStore, PadDocument) {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let old = one("protected", "first", 1);
        store.write(&old).unwrap();
        store
            .migrate_to_protected(
                &old,
                1,
                TEST_VAULT_ID,
                b"future encrypted payload",
                |path| open_staged(&old, path),
            )
            .unwrap();
        (directory, store, old)
    }

    #[test]
    fn protected_vault_identity_rejects_swapped_envelopes_and_old_signals() {
        let (directory, store, old) = migrated_store();
        let next = one("next", "body", 2);
        assert_eq!(store.protected_vault_id().unwrap(), TEST_VAULT_ID);
        // A valid envelope from another vault is not backup-recoverable.
        assert!(matches!(
            store.load_protected(|path| {
                let (_, document) = open_protected_test(&old, &next, path)?;
                Ok(([9; VAULT_ID_LEN], document))
            }),
            Err(StorageError::ProtectedVerification)
        ));
        assert!(matches!(
            store.write_protected(&old, &next, b"new encrypted payload", |path| {
                let (_, document) = open_protected_test(&old, &next, path)?;
                Ok(([9; VAULT_ID_LEN], document))
            }),
            Err(StorageError::ProtectedVerification)
        ));
        // The interim marker shape lacked an identity. Presence still blocks
        // legacy load, but it can no longer authorize a protected open.
        let mut old_signal = Vec::from(PROTECTED_MAGIC);
        old_signal.extend_from_slice(&PROTECTED_VERSION.to_le_bytes());
        old_signal.extend_from_slice(b"committed");
        fs::write(&store.marker, protect(&old_signal).unwrap()).unwrap();
        assert!(matches!(
            store.protected_vault_id(),
            Err(StorageError::ProtectedCutover)
        ));
        assert!(matches!(store.load(), Err(StorageError::ProtectedCutover)));
        assert!(matches!(
            store.load_protected(|path| open_protected_test(&old, &next, path)),
            Err(StorageError::ProtectedCutover)
        ));
        let mut other_signal = old_signal;
        other_signal.extend_from_slice(&[9; VAULT_ID_LEN]);
        fs::write(&store.marker, protect(&other_signal).unwrap()).unwrap();
        assert!(matches!(
            store.protected_vault_id(),
            Err(StorageError::ProtectedCutover)
        ));
        assert!(matches!(
            store.recover_protected_cutover(|path| open_staged(&old, path)),
            Err(StorageError::ProtectedCutover)
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn migration_requires_nonzero_and_matching_authenticated_vault_id() {
        let directory = temp_dir();
        let store = PadStore::at(&directory);
        let old = one("legacy", "body", 1);
        store.write(&old).unwrap();
        assert!(matches!(
            store.migrate_to_protected(
                &old,
                1,
                [0; VAULT_ID_LEN],
                b"future encrypted payload",
                |path| { open_staged(&old, path) }
            ),
            Err(StorageError::InvalidFormat)
        ));
        assert!(matches!(
            store.migrate_to_protected(
                &old,
                1,
                TEST_VAULT_ID,
                b"future encrypted payload",
                |path| {
                    let (_, document) = open_staged(&old, path)?;
                    Ok(([9; VAULT_ID_LEN], document))
                }
            ),
            Err(StorageError::ProtectedVerification)
        ));
        assert_eq!(store.load().unwrap().document, old);
        assert!(!store.intent.exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn protected_load_uses_primary_then_authenticated_backup_only() {
        let (directory, store, old) = migrated_store();
        let next = one("later", "second", 2);
        assert_eq!(
            store
                .load_protected(|path| open_protected_test(&old, &next, path))
                .unwrap(),
            LoadOutcome {
                document: old.clone(),
                recovered_from_backup: false,
            }
        );
        fs::write(&store.protected_path, b"corrupt primary").unwrap();
        assert_eq!(
            store
                .load_protected(|path| open_protected_test(&old, &next, path))
                .unwrap(),
            LoadOutcome {
                document: old.clone(),
                recovered_from_backup: true,
            }
        );
        fs::write(&store.protected_backup, b"corrupt backup").unwrap();
        fs::write(&store.protected_temp, b"future encrypted payload").unwrap();
        assert!(matches!(
            store.load_protected(|path| open_protected_test(&old, &next, path)),
            Err(StorageError::ProtectedVerification)
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn protected_write_rejects_stale_and_recovery_then_keeps_protected_backup() {
        let (directory, store, old) = migrated_store();
        let next = one("later", "second", 2);
        let stale = one("stale", "first", 1);
        assert!(matches!(
            store.write_protected(&stale, &next, b"new encrypted payload", |path| {
                open_protected_test(&old, &next, path)
            }),
            Err(StorageError::StaleProtectedDocument)
        ));
        assert!(!store.protected_temp.exists());
        assert!(matches!(
            store.write_protected(&old, &next, b"new encrypted payload", |path| {
                let (id, document) = open_protected_test(&old, &next, path)?;
                Ok((
                    if path == store.protected_temp.as_path() {
                        [9; VAULT_ID_LEN]
                    } else {
                        id
                    },
                    document,
                ))
            }),
            Err(StorageError::ProtectedVerification)
        ));
        assert_eq!(
            store
                .load_protected(|path| open_protected_test(&old, &next, path))
                .unwrap()
                .document,
            old
        );
        fs::remove_file(&store.protected_temp).unwrap();
        assert_eq!(
            store
                .write_protected(&old, &next, b"new encrypted payload", |path| {
                    open_protected_test(&old, &next, path)
                })
                .unwrap(),
            WriteOutcome::Replaced
        );
        assert_eq!(
            store
                .load_protected(|path| open_protected_test(&old, &next, path))
                .unwrap()
                .document,
            next
        );
        assert_eq!(
            fs::read(&store.protected_backup).unwrap(),
            b"future encrypted payload"
        );
        assert!(matches!(store.load(), Err(StorageError::ProtectedCutover)));
        fs::write(&store.protected_path, b"corrupt primary").unwrap();
        assert!(matches!(
            store.write_protected(&old, &next, b"new encrypted payload", |path| {
                open_protected_test(&old, &next, path)
            }),
            Err(StorageError::ProtectedVerification)
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn protected_write_faults_have_explicit_before_and_after_publication_states() {
        for point in [
            ProtectedWritePoint::Staged,
            ProtectedWritePoint::Verified,
            ProtectedWritePoint::Published,
        ] {
            let (directory, store, old) = migrated_store();
            let next = one("later", "second", 2);
            let result = store.write_protected_with_hook(
                &old,
                &next,
                b"new encrypted payload",
                |path| open_protected_test(&old, &next, path),
                |reached| {
                    if reached == point {
                        Err(StorageError::Io(io::Error::other("injected interruption")))
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(result.is_err(), "{point:?}");
            let loaded = store
                .load_protected(|path| open_protected_test(&old, &next, path))
                .unwrap();
            if point == ProtectedWritePoint::Published {
                assert_eq!(loaded.document, next);
                assert_eq!(
                    fs::read(&store.protected_backup).unwrap(),
                    b"future encrypted payload"
                );
            } else {
                assert_eq!(loaded.document, old);
                assert_eq!(
                    fs::read(&store.protected_temp).unwrap(),
                    b"new encrypted payload"
                );
            }
            assert!(matches!(store.load(), Err(StorageError::ProtectedCutover)));
            let _ = fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn protected_write_serializes_reads_and_other_writes() {
        let (directory, store, old) = migrated_store();
        let next = one("later", "second", 2);
        let at_verified = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let thread_store = store.clone();
        let thread_old = old.clone();
        let thread_next = next.clone();
        let arrived = Arc::clone(&at_verified);
        let finish = Arc::clone(&release);
        let writer = thread::spawn(move || {
            thread_store.write_protected_with_hook(
                &thread_old,
                &thread_next,
                b"new encrypted payload",
                |path| open_protected_test(&thread_old, &thread_next, path),
                |point| {
                    if point == ProtectedWritePoint::Verified {
                        arrived.wait();
                        finish.wait();
                    }
                    Ok(())
                },
            )
        });
        at_verified.wait();
        assert!(store
            .load_protected(|path| open_protected_test(&old, &next, path))
            .is_err());
        assert!(store
            .write_protected(&old, &next, b"new encrypted payload", |path| {
                open_protected_test(&old, &next, path)
            })
            .is_err());
        release.wait();
        writer.join().unwrap().unwrap();
        assert_eq!(
            store
                .load_protected(|path| open_protected_test(&old, &next, path))
                .unwrap()
                .document,
            next
        );
        let _ = fs::remove_dir_all(directory);
    }
}
