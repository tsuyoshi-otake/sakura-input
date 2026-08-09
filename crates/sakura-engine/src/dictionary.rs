//! Process-wide mapped dictionary and bounded conversion-worker pool.
//!
//! The dictionary image is mapped read-only once and its validated borrowed
//! view is shared by every pipe worker. Conversion arenas are much larger than
//! a dispatcher, so two reusable slots are shared as well: private memory is
//! O(pool size), not O(pipe instances), and a third simultaneous conversion
//! receives an explicit `Busy` result instead of growing memory or waiting
//! without a latency bound.

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, TryLockError};

use sakura_core::{
    ConversionCandidate, ConversionError, ConversionOptions, Converter, Dictionary, UserDictionary,
};
use sakura_proto::{FixedStr, MAX_PREEDIT_BYTES};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
};

/// Release gate for the complete Sakura system image, including all fourteen
/// canonical categories. The image remains a single read-only mapping.
pub const MAX_DICTIONARY_IMAGE_BYTES: usize = 128 * 1024 * 1024;

/// Two conversions may run in parallel while keeping the reusable arenas
/// within the engine's private-working-set budget.
const CONVERSION_SLOTS: usize = 2;

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Mapping(windows::core::Error),
    Empty,
    TooLarge(usize),
    Image(sakura_core::dictionary::Error),
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "dictionary file: {error}"),
            Self::Mapping(error) => write!(f, "dictionary mapping: {error}"),
            Self::Empty => f.write_str("dictionary image is empty"),
            Self::TooLarge(bytes) => write!(
                f,
                "dictionary image is {bytes} bytes (limit {MAX_DICTIONARY_IMAGE_BYTES})"
            ),
            Self::Image(error) => write!(f, "dictionary image: {error}"),
        }
    }
}

impl std::error::Error for LoadError {}

#[derive(Debug)]
pub enum ConvertFailure {
    Busy,
    Conversion(ConversionError),
}

impl core::fmt::Display for ConvertFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Busy => f.write_str("all bounded conversion slots are busy"),
            Self::Conversion(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ConvertFailure {}

/// A validated static dictionary view and the only conversion arenas in the
/// process. The mapped bytes outlive this value (see [`open`]).
#[derive(Debug)]
pub struct ConversionService {
    dictionary: Dictionary<'static>,
    converters: Box<[Mutex<Converter>]>,
    /// Atomically replaced after a complete off-thread parse. Conversions
    /// clone the `Arc` while holding the read side, then release the lock
    /// before entering the lattice so a reload never waits on conversion.
    user_dictionary: RwLock<Arc<UserDictionary>>,
}

impl ConversionService {
    /// Builds a service over bytes that the caller guarantees live until
    /// process exit. Tests use a leaked small fixture; production uses
    /// [`open`]'s process-lifetime file mapping.
    pub fn from_static_bytes(bytes: &'static [u8]) -> Result<Self, LoadError> {
        if bytes.is_empty() {
            return Err(LoadError::Empty);
        }
        if bytes.len() > MAX_DICTIONARY_IMAGE_BYTES {
            return Err(LoadError::TooLarge(bytes.len()));
        }
        let dictionary = Dictionary::parse(bytes).map_err(LoadError::Image)?;
        let converters = (0..CONVERSION_SLOTS)
            .map(|_| Mutex::new(Converter::new()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            dictionary,
            converters,
            user_dictionary: RwLock::new(Arc::new(UserDictionary::default())),
        })
    }

    /// Publishes one fully validated snapshot. Callers must parse before this
    /// method so a rejected update cannot disturb the active dictionary.
    pub fn replace_user_dictionary(&self, dictionary: UserDictionary) {
        let mut active = self
            .user_dictionary
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *active = Arc::new(dictionary);
    }

    pub fn user_dictionary_snapshot(&self) -> Arc<UserDictionary> {
        Arc::clone(
            &self
                .user_dictionary
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// The validated immutable system view used to build the one process-wide
    /// prediction index. `Dictionary` is a set of borrowed slices and is Copy;
    /// the process-lifetime mapping still owns every referenced byte.
    pub const fn dictionary(&self) -> Dictionary<'static> {
        self.dictionary
    }

    /// Recovers a reading for an exact committed surface. This is a deliberate
    /// cold-path O(N) scan: reconversion is an explicit user command, while a
    /// permanent reverse index would consume several MiB of the 15 MiB working
    /// set every minute the IME is idle. User entries take precedence.
    pub fn reconversion_reading(
        &self,
        surface: &str,
        output: &mut FixedStr<MAX_PREEDIT_BYTES>,
    ) -> Result<bool, sakura_core::dictionary::Error> {
        output.clear();
        let user_dictionary = self.user_dictionary_snapshot();
        if let Some(entry) = user_dictionary
            .entries()
            .iter()
            .find(|entry| entry.surface == surface)
        {
            return Ok(output.push_str(&entry.reading).is_ok());
        }

        let mut decoded = FixedStr::<MAX_PREEDIT_BYTES>::new();
        let mut found = false;
        let mut decode_error = None;
        self.dictionary.visit_entries(|reading, entry| {
            decoded.clear();
            if let Err(error) = self.dictionary.write_surface(entry, &mut decoded) {
                decode_error = Some(error);
                return false;
            }
            if decoded.as_str() == surface {
                found = output.push_str(reading).is_ok();
                return false;
            }
            true
        })?;
        match decode_error {
            Some(error) => Err(error),
            None => Ok(found),
        }
    }

    /// Runs one conversion without waiting or allocating another arena.
    /// `consume` executes while the selected slot is held, so candidate slices
    /// cannot escape and no candidate strings need to be copied into a second
    /// unbounded structure.
    pub fn with_candidates<R>(
        &self,
        reading: &str,
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate]) -> R,
    ) -> Result<R, ConvertFailure> {
        let user_dictionary = self.user_dictionary_snapshot();
        let mut consume = Some(consume);
        for slot in &self.converters {
            let mut converter = match slot.try_lock() {
                Ok(converter) => converter,
                Err(TryLockError::WouldBlock) => continue,
                // `convert` resets all arenas before use. Recovering a slot
                // after a test-only unwind cannot expose half-built state.
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            let candidates = converter
                .convert_with_user_dictionary(
                    &self.dictionary,
                    (!user_dictionary.is_empty()).then_some(user_dictionary.as_ref()),
                    reading,
                    options,
                )
                .map_err(ConvertFailure::Conversion)?;
            let use_candidates = consume
                .take()
                .expect("closure is consumed by one slot only");
            return Ok(use_candidates(candidates));
        }
        Err(ConvertFailure::Busy)
    }
}

/// Maps and validates a read-only dictionary for the lifetime of this engine
/// process. The view is deliberately not unmapped on success: that is what
/// makes its borrowed slices genuinely `'static`, and the OS reclaims the
/// single view atomically when this resident process exits. All handles are
/// closed immediately after the view is established.
pub fn open(path: &Path) -> Result<Arc<ConversionService>, LoadError> {
    let file = File::open(path).map_err(LoadError::Io)?;
    let bytes = usize::try_from(file.metadata().map_err(LoadError::Io)?.len())
        .map_err(|_| LoadError::TooLarge(usize::MAX))?;
    if bytes == 0 {
        return Err(LoadError::Empty);
    }
    if bytes > MAX_DICTIONARY_IMAGE_BYTES {
        return Err(LoadError::TooLarge(bytes));
    }

    let file_handle = HANDLE(file.as_raw_handle());
    // SAFETY: `file_handle` is a live read-only file handle, the mapping has
    // no name or writable protection, and both size halves are zero so Windows
    // derives the exact mapping length from the file.
    let mapping = unsafe { CreateFileMappingW(file_handle, None, PAGE_READONLY, 0, 0, None) }
        .map_err(LoadError::Mapping)?;
    // SAFETY: `mapping` is a live read-only file mapping. Offset zero and the
    // validated file length map exactly the image checked above.
    let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, bytes) };
    // SAFETY: closing the mapping object does not invalidate an established
    // view; the view keeps the mapped pages alive independently.
    let close_result = unsafe { CloseHandle(mapping) };
    if let Err(error) = close_result {
        if !view.Value.is_null() {
            // SAFETY: `view` came from the successful call above and has not
            // been unmapped yet.
            let _ = unsafe { UnmapViewOfFile(view) };
        }
        return Err(LoadError::Mapping(error));
    }
    if view.Value.is_null() {
        return Err(LoadError::Mapping(windows::core::Error::from_thread()));
    }
    drop(file);

    // SAFETY: the view covers exactly `bytes` readable bytes and remains
    // mapped for process lifetime on the success path described above.
    let image: &'static [u8] =
        unsafe { core::slice::from_raw_parts(view.Value.cast::<u8>(), bytes) };
    match ConversionService::from_static_bytes(image) {
        Ok(service) => Ok(Arc::new(service)),
        Err(error) => {
            // SAFETY: validation failed before any borrowed dictionary escaped,
            // so the live view can and should be released on this error path.
            let _ = unsafe { UnmapViewOfFile(view) };
            Err(error)
        }
    }
}

/// `SAKURA_DICTIONARY` is a diagnostic/developer override. Installed builds
/// use `dict\system.dic` below the directory containing `sakura_engine.exe`.
pub fn default_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("SAKURA_DICTIONARY") {
        return Ok(PathBuf::from(path));
    }
    Ok(installed_path(&std::env::current_exe()?))
}

fn installed_path(executable: &Path) -> PathBuf {
    executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("dict")
        .join("system.dic")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> &'static [u8] {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t0\t100\t100\tit\tIT用語\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        )
    }

    #[test]
    fn static_image_converts_with_a_bounded_slot() {
        let service = ConversionService::from_static_bytes(image()).expect("service");
        let texts = service
            .with_candidates("かな", ConversionOptions::default(), |candidates| {
                candidates
                    .iter()
                    .map(|candidate| candidate.text().to_owned())
                    .collect::<Vec<_>>()
            })
            .expect("conversion");
        assert_eq!(texts.first().map(String::as_str), Some("仮名"));
    }

    #[test]
    fn a_validated_user_dictionary_snapshot_is_visible_to_the_next_conversion() {
        let service = ConversionService::from_static_bytes(image()).expect("service");
        service.replace_user_dictionary(
            UserDictionary::parse_tsv(
                "reading\tsurface\tpos\tcomment\nさくら\tSakura Input\tproper-noun\tproject\n",
            )
            .expect("user dictionary"),
        );

        let texts = service
            .with_candidates("さくら", ConversionOptions::default(), |candidates| {
                candidates
                    .iter()
                    .map(|candidate| candidate.text().to_owned())
                    .collect::<Vec<_>>()
            })
            .expect("conversion");

        assert_eq!(texts.first().map(String::as_str), Some("Sakura Input"));
        assert_eq!(service.user_dictionary_snapshot().len(), 1);
    }

    #[test]
    fn reconversion_recovers_system_readings_and_prefers_an_exact_user_surface() {
        let service = ConversionService::from_static_bytes(image()).expect("service");
        let mut reading = FixedStr::<MAX_PREEDIT_BYTES>::new();
        assert!(service
            .reconversion_reading("仮名", &mut reading)
            .expect("system reverse scan"));
        assert_eq!(reading.as_str(), "かな");

        service.replace_user_dictionary(
            UserDictionary::parse_tsv(
                "reading\tsurface\tpos\tcomment\nかめい\t仮名\tproper-noun\tuser override\n",
            )
            .expect("user dictionary"),
        );
        assert!(service
            .reconversion_reading("仮名", &mut reading)
            .expect("user reverse scan"));
        assert_eq!(reading.as_str(), "かめい");
    }

    #[test]
    fn empty_and_oversized_images_are_rejected_before_mapping() {
        assert!(matches!(
            ConversionService::from_static_bytes(&[]),
            Err(LoadError::Empty)
        ));
        let oversized = vec![0; MAX_DICTIONARY_IMAGE_BYTES + 1].into_boxed_slice();
        let oversized = Box::leak(oversized);
        assert!(matches!(
            ConversionService::from_static_bytes(oversized),
            Err(LoadError::TooLarge(_))
        ));
    }

    #[test]
    fn default_path_honours_the_explicit_override() {
        const KEY: &str = "SAKURA_DICTIONARY";
        let previous = std::env::var_os(KEY);
        std::env::set_var(KEY, r"C:\fixture\system.dic");
        assert_eq!(
            default_path().expect("path"),
            Path::new(r"C:\fixture\system.dic")
        );
        match previous {
            Some(value) => std::env::set_var(KEY, value),
            None => std::env::remove_var(KEY),
        }
    }

    #[test]
    fn installed_dictionary_matches_the_packaged_layout() {
        assert_eq!(
            installed_path(Path::new(
                r"C:\Program Files\Sakura Input\sakura_engine.exe"
            )),
            Path::new(r"C:\Program Files\Sakura Input\dict\system.dic")
        );
    }
}
