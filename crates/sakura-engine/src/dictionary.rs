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

use sakura_core::conversion::{ConversionInput, RawRepairPlan};
use sakura_core::{
    CivilDate, ConversionCandidate, ConversionDiagnostics, ConversionError, ConversionOptions,
    Converter, Dictionary, UserDictionary,
};
use sakura_proto::{FixedStr, MAX_PREEDIT_BYTES};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
};
use windows::Win32::System::SystemInformation::GetLocalTime;

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
        self.with_candidates_input(ConversionInput::ordinary(reading), options, consume)
    }

    /// Input-aware form of [`Self::with_candidates`].  The classified input
    /// reaches the core converter unchanged, so exact literal policies are
    /// enforced before any ranking or repair edges are considered.
    pub fn with_candidates_input<R>(
        &self,
        input: ConversionInput<'_>,
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate]) -> R,
    ) -> Result<R, ConvertFailure> {
        self.with_candidates_input_hints(input, options, &[], consume)
    }

    /// Hint-aware input form of [`Self::with_candidates`].
    pub fn with_candidates_input_hints<R>(
        &self,
        input: ConversionInput<'_>,
        options: ConversionOptions,
        commit_repair_readings: &[&str],
        consume: impl FnOnce(&[ConversionCandidate]) -> R,
    ) -> Result<R, ConvertFailure> {
        self.with_conversion_input_hints(
            input,
            options,
            commit_repair_readings,
            |candidates, _diagnostics| consume(candidates),
        )
    }

    /// Runs one conversion and exposes its text-free bounded-search terminal.
    /// Hard failures remain `Err`, while fallback and budget exhaustion are
    /// successful results with explicit diagnostics.
    pub fn with_conversion<R>(
        &self,
        reading: &str,
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConvertFailure> {
        self.with_conversion_input(ConversionInput::ordinary(reading), options, consume)
    }

    /// Input-aware form of [`Self::with_conversion`].
    pub fn with_conversion_input<R>(
        &self,
        input: ConversionInput<'_>,
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConvertFailure> {
        self.with_conversion_input_hints(input, options, &[], consume)
    }

    /// Like [`Self::with_conversion`], but also installs commit-history repair
    /// readings for the lattice build of this query only.
    pub fn with_conversion_hints<R>(
        &self,
        reading: &str,
        options: ConversionOptions,
        commit_repair_readings: &[&str],
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConvertFailure> {
        self.with_conversion_input_hints(
            ConversionInput::ordinary(reading),
            options,
            commit_repair_readings,
            consume,
        )
    }

    /// Input-aware form of [`Self::with_conversion_hints`].  The immutable
    /// user-dictionary snapshot and civil date are captured once per request;
    /// the selected converter slot remains held until `consume` returns.
    pub fn with_conversion_input_hints<R>(
        &self,
        input: ConversionInput<'_>,
        options: ConversionOptions,
        commit_repair_readings: &[&str],
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConvertFailure> {
        let user_dictionary = self.user_dictionary_snapshot();
        let civil_date = local_civil_date();
        let mut consume = Some(consume);
        for slot in &self.converters {
            let mut converter = match slot.try_lock() {
                Ok(converter) => converter,
                Err(TryLockError::WouldBlock) => continue,
                // `convert` resets all arenas before use. Recovering a slot
                // after a test-only unwind cannot expose half-built state.
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            converter.set_commit_repair_readings(commit_repair_readings);
            converter.set_civil_date(civil_date);
            let result = converter
                .convert_with_user_dictionary_input_detailed(
                    &self.dictionary,
                    (!user_dictionary.is_empty()).then_some(user_dictionary.as_ref()),
                    input,
                    options,
                )
                .map_err(ConvertFailure::Conversion)?;
            let use_candidates = consume
                .take()
                .expect("closure is consumed by one slot only");
            return Ok(use_candidates(result.candidates(), result.diagnostics()));
        }
        Err(ConvertFailure::Busy)
    }

    /// Runs one original conversion and the bounded raw-repair passes while
    /// holding exactly one converter slot.  The converter owns the scratch
    /// used to retain direct candidates, so a corrected pass cannot recurse
    /// through this service or acquire a second slot.  Candidate slices are
    /// only valid for the duration of `consume`, just like
    /// [`Self::with_conversion`].
    pub fn with_raw_repair_conversion<R>(
        &self,
        original_reading: &str,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConvertFailure> {
        self.with_raw_repair_conversion_input(
            ConversionInput::ordinary(original_reading),
            plans,
            options,
            consume,
        )
    }

    /// Input-aware form of [`Self::with_raw_repair_conversion`].  The direct
    /// pass preserves the classified literal policy while all corrected
    /// passes remain bounded and system-only inside this same slot.
    pub fn with_raw_repair_conversion_input<R>(
        &self,
        original_input: ConversionInput<'_>,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConvertFailure> {
        self.with_raw_repair_conversion_input_hints(original_input, plans, options, &[], consume)
    }

    /// Hint-aware form of [`Self::with_raw_repair_conversion`].  Commit
    /// history is installed once on the selected converter; the core raw
    /// conversion API consumes that one-shot state in the direct pass and
    /// never recreates it for corrected readings.
    pub fn with_raw_repair_conversion_hints<R>(
        &self,
        original_reading: &str,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
        commit_repair_readings: &[&str],
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConvertFailure> {
        self.with_raw_repair_conversion_input_hints(
            ConversionInput::ordinary(original_reading),
            plans,
            options,
            commit_repair_readings,
            consume,
        )
    }

    /// Hint-aware input form of [`Self::with_raw_repair_conversion`].
    pub fn with_raw_repair_conversion_input_hints<R>(
        &self,
        original_input: ConversionInput<'_>,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
        commit_repair_readings: &[&str],
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConvertFailure> {
        let user_dictionary = self.user_dictionary_snapshot();
        let civil_date = local_civil_date();
        let mut consume = Some(consume);
        for slot in &self.converters {
            let mut converter = match slot.try_lock() {
                Ok(converter) => converter,
                Err(TryLockError::WouldBlock) => continue,
                // Conversion resets all arenas before use. Recovering a slot
                // after a test-only unwind cannot expose half-built state.
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            converter.set_commit_repair_readings(commit_repair_readings);
            converter.set_civil_date(civil_date);
            let result = converter
                .with_raw_repair_input_conversion(
                    &self.dictionary,
                    (!user_dictionary.is_empty()).then_some(user_dictionary.as_ref()),
                    original_input,
                    plans,
                    options,
                    |candidates, diagnostics| {
                        let use_candidates = consume
                            .take()
                            .expect("closure is consumed by one slot only");
                        use_candidates(candidates, diagnostics)
                    },
                )
                .map_err(ConvertFailure::Conversion)?;
            return Ok(result);
        }
        Err(ConvertFailure::Busy)
    }
}

fn local_civil_date() -> Option<CivilDate> {
    // SAFETY: GetLocalTime has no preconditions; it only fills a SYSTEMTIME on
    // the stack with the calling thread's current local calendar date.
    let local = unsafe { GetLocalTime() };
    CivilDate::from_ymd(
        i32::from(local.wYear),
        u8::try_from(local.wMonth).ok()?,
        u8::try_from(local.wDay).ok()?,
    )
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

    fn raw_plan(
        plan_id: u8,
        original: &str,
        corrected: &str,
        runs: &[sakura_core::conversion::CorrectionRun],
    ) -> RawRepairPlan {
        let map = sakura_core::conversion::CorrectionMap::new(original, corrected, runs)
            .expect("correction map");
        RawRepairPlan::new(
            plan_id,
            corrected,
            map,
            sakura_core::conversion::RepairTier::LocalCompletion,
        )
        .expect("raw repair plan")
    }

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
        let (texts, diagnostics) = service
            .with_conversion(
                "かな",
                ConversionOptions::default(),
                |candidates, diagnostics| {
                    (
                        candidates
                            .iter()
                            .map(|candidate| candidate.text().to_owned())
                            .collect::<Vec<_>>(),
                        diagnostics,
                    )
                },
            )
            .expect("conversion");
        assert_eq!(texts.first().map(String::as_str), Some("仮名"));
        assert!(diagnostics.states_pushed > 0);
    }

    #[test]
    fn raw_repair_uses_one_slot_and_third_conversion_is_busy() {
        use std::sync::{mpsc, Barrier};
        use std::thread;

        let service = Arc::new(ConversionService::from_static_bytes(image()).expect("service"));
        let (entered_tx, entered_rx) = mpsc::channel();
        let release = Arc::new(Barrier::new(CONVERSION_SLOTS + 1));
        let mut workers = Vec::new();
        for _ in 0..CONVERSION_SLOTS {
            let service = Arc::clone(&service);
            let entered_tx = entered_tx.clone();
            let release = Arc::clone(&release);
            workers.push(thread::spawn(move || {
                service.with_raw_repair_conversion(
                    "かな",
                    &[],
                    ConversionOptions::default(),
                    |candidates, _diagnostics| {
                        entered_tx.send(()).expect("entered conversion");
                        release.wait();
                        candidates.len()
                    },
                )
            }));
        }
        drop(entered_tx);

        for _ in 0..CONVERSION_SLOTS {
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("both conversion slots must be held");
        }
        let third = service.with_raw_repair_conversion(
            "かな",
            &[],
            ConversionOptions::default(),
            |_candidates, _diagnostics| (),
        );
        let third_is_busy = matches!(third, Err(ConvertFailure::Busy));

        release.wait();
        for worker in workers {
            worker
                .join()
                .expect("conversion worker")
                .expect("conversion");
        }
        assert!(third_is_busy);
    }

    #[test]
    fn raw_repair_reserves_a_slot_when_direct_candidates_fill_budget() {
        let mut tsv = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        );
        for index in 0..12 {
            tsv.push_str(&format!("あき\t秋{index}\t0\t0\t{index}\t{index}\t\t\n"));
        }
        let entries = dictc::parse_entries("fixture.tsv", &tsv).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");
        let plan = raw_plan(
            1,
            "あき",
            "あきい",
            &[
                sakura_core::conversion::CorrectionRun::equal(0, 3, 0, 3),
                sakura_core::conversion::CorrectionRun::replace(3, 9, 3, 6),
            ],
        );
        let (texts, diagnostics) = service
            .with_raw_repair_conversion(
                "あき",
                &[plan],
                ConversionOptions {
                    max_candidates: 12,
                    ..ConversionOptions::default()
                },
                |candidates, diagnostics| {
                    (
                        candidates
                            .iter()
                            .map(|candidate| candidate.text().to_owned())
                            .collect::<Vec<_>>(),
                        diagnostics,
                    )
                },
            )
            .expect("conversion");

        assert_eq!(texts.len(), 12, "direct candidates must fill the budget");
        assert!(texts.iter().all(|text| text.starts_with('秋')));
        assert_eq!(diagnostics.raw_repair_passes, 1);
        assert_eq!(diagnostics.raw_repair_candidates_added, 0);
    }

    #[test]
    fn raw_repair_admits_full_system_only_completion_and_preserves_direct() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nないか\t内科\t0\t0\t1\t1\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");
        let plan = raw_plan(
            7,
            "ないk",
            "ないか",
            &[
                sakura_core::conversion::CorrectionRun::equal(0, 6, 0, 6),
                sakura_core::conversion::CorrectionRun::replace(6, 9, 6, 7),
            ],
        );
        let (texts, diagnostics) = service
            .with_raw_repair_conversion(
                "ないk",
                &[plan],
                ConversionOptions::default(),
                |candidates, diagnostics| {
                    (
                        candidates
                            .iter()
                            .map(|candidate| {
                                (
                                    candidate.text().to_owned(),
                                    candidate.origin(),
                                    candidate.path_evidence(),
                                )
                            })
                            .collect::<Vec<_>>(),
                        diagnostics,
                    )
                },
            )
            .expect("conversion");

        let repaired = texts
            .iter()
            .find(|(text, _, _)| text == "内科")
            .expect("system-only corrected candidate");
        assert_eq!(
            repaired.1,
            sakura_core::conversion::CandidateOrigin::RawRepair {
                plan_id: 7,
                tier: sakura_core::conversion::RepairTier::LocalCompletion,
            }
        );
        assert!(repaired.2.is_system_only());
        assert!(texts.iter().any(|(text, origin, _)| {
            text == "ないk" && *origin == sakura_core::conversion::CandidateOrigin::Direct
        }));
        assert_eq!(diagnostics.raw_repair_passes, 1);
        assert_eq!(diagnostics.raw_repair_candidates_added, 1);
    }

    #[test]
    fn raw_repair_keeps_direct_fallback_when_corrected_pass_has_no_system_path() {
        let service = ConversionService::from_static_bytes(image()).expect("service");
        let plan = raw_plan(
            3,
            "かな",
            "かに",
            &[sakura_core::conversion::CorrectionRun::replace(0, 6, 0, 6)],
        );
        let (texts, diagnostics) = service
            .with_raw_repair_conversion(
                "かな",
                &[plan],
                ConversionOptions::default(),
                |candidates, diagnostics| {
                    (
                        candidates
                            .iter()
                            .map(|candidate| (candidate.text().to_owned(), candidate.origin()))
                            .collect::<Vec<_>>(),
                        diagnostics,
                    )
                },
            )
            .expect("direct conversion must survive rejected repair");

        assert_eq!(texts.first().map(|(text, _)| text.as_str()), Some("仮名"));
        assert!(texts
            .iter()
            .all(|(_, origin)| { *origin == sakura_core::conversion::CandidateOrigin::Direct }));
        assert_eq!(diagnostics.raw_repair_candidates_added, 0);
        assert_eq!(diagnostics.raw_repair_passes, 1);
    }

    #[test]
    fn raw_repair_input_preserves_mixed_exact_only_direct_before_repair() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nないk\tHOSTILE\t0\t0\t0\t0\t\t\nないか\t内科\t0\t0\t1\t1\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");
        let input = ConversionInput::new(
            "ないk",
            "ないk",
            sakura_core::conversion::ConversionInputClass::MixedUnresolvedLatin,
            sakura_core::conversion::LiteralPolicy::ExactOnly,
        );
        let plan = raw_plan(
            8,
            "ないk",
            "ないか",
            &[
                sakura_core::conversion::CorrectionRun::equal(0, 6, 0, 6),
                sakura_core::conversion::CorrectionRun::replace(6, 9, 6, 7),
            ],
        );
        let (candidates, diagnostics) = service
            .with_raw_repair_conversion_input(
                input,
                &[plan],
                ConversionOptions::default(),
                |candidates, diagnostics| {
                    (
                        candidates
                            .iter()
                            .map(|candidate| {
                                (
                                    candidate.text().to_owned(),
                                    candidate.origin(),
                                    candidate.is_synthetic_exact(),
                                )
                            })
                            .collect::<Vec<_>>(),
                        diagnostics,
                    )
                },
            )
            .expect("classified raw conversion");

        assert_eq!(candidates[0].0, "ないk");
        assert_eq!(
            candidates[0].1,
            sakura_core::conversion::CandidateOrigin::Direct
        );
        assert!(candidates[0].2);
        assert!(!candidates.iter().any(|(text, _, _)| text == "HOSTILE"));
        assert!(candidates.iter().any(|(text, origin, _)| {
            text == "内科"
                && *origin
                    == sakura_core::conversion::CandidateOrigin::RawRepair {
                        plan_id: 8,
                        tier: sakura_core::conversion::RepairTier::LocalCompletion,
                    }
        }));
        assert_eq!(diagnostics.raw_repair_passes, 1);
        assert_eq!(diagnostics.raw_repair_candidates_added, 1);
    }

    #[test]
    fn classified_exact_top1_keeps_literal_at_candidate_zero() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nesp32\tSystemExact\t0\t0\t1\t1\t\t\nesp32\tSpellingExact\t0\t0\t0\t0\tcorrection\t\nesp\tPartial\t0\t0\t0\t0\t\t\n2\tGeneratedLike\t0\t0\t0\t0\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");
        service.replace_user_dictionary(
            UserDictionary::parse_tsv(
                "reading\tsurface\tpos\tcomment\nesp32\tUserExact\talphabet\t\n",
            )
            .expect("user dictionary"),
        );
        let input = ConversionInput::new(
            "esp32",
            "ESP32",
            sakura_core::conversion::ConversionInputClass::OpaqueAsciiIdentifier,
            sakura_core::conversion::LiteralPolicy::ExactTop1,
        );
        let candidates = service
            .with_candidates_input(
                input,
                ConversionOptions {
                    max_candidates: 4,
                    ..ConversionOptions::default()
                },
                |candidates| {
                    candidates
                        .iter()
                        .map(|candidate| {
                            (candidate.text().to_owned(), candidate.is_synthetic_exact())
                        })
                        .collect::<Vec<_>>()
                },
            )
            .expect("exact-top1 conversion");

        assert_eq!(candidates.first(), Some(&(String::from("ESP32"), true)));
        assert!(candidates.iter().any(|(text, _)| text == "SystemExact"));
        assert!(candidates.iter().any(|(text, _)| text == "UserExact"));
        assert!(!candidates.iter().any(|(text, _)| text == "SpellingExact"));
        assert!(!candidates.iter().any(|(text, _)| text == "Partial"));
        assert!(!candidates.iter().any(|(text, _)| text == "GeneratedLike"));
    }

    #[test]
    fn ordinary_input_wrapper_matches_ranked_conversion_input() {
        let service = ConversionService::from_static_bytes(image()).expect("service");
        let legacy = service
            .with_conversion(
                "かな",
                ConversionOptions::default(),
                |candidates, diagnostics| {
                    (
                        candidates
                            .iter()
                            .map(|candidate| candidate.text().to_owned())
                            .collect::<Vec<_>>(),
                        diagnostics,
                    )
                },
            )
            .expect("ordinary conversion");
        let classified = service
            .with_conversion_input(
                ConversionInput::ordinary("かな"),
                ConversionOptions::default(),
                |candidates, diagnostics| {
                    (
                        candidates
                            .iter()
                            .map(|candidate| candidate.text().to_owned())
                            .collect::<Vec<_>>(),
                        diagnostics,
                    )
                },
            )
            .expect("ordinary input conversion");

        assert_eq!(classified, legacy);
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
    fn input_repair_recovers_extra_n_and_skips_when_master_is_off() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nこんにちは\t今日は\t0\t0\t100\t100\t\t\nこんんにちは\t誤\t0\t0\t50\t50\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");

        let repaired = service
            .with_candidates(
                "こんんにちは",
                ConversionOptions::default(),
                |candidates| {
                    candidates
                        .iter()
                        .any(|candidate| candidate.text() == "今日は")
                },
            )
            .expect("conversion");
        assert!(
            repaired,
            "default input support should repair duplicated ん"
        );

        let mut options = ConversionOptions::default();
        options.input_support.enabled = false;
        let unrepaired = service
            .with_candidates("こんんにちは", options, |candidates| {
                candidates
                    .iter()
                    .any(|candidate| candidate.text() == "今日は")
            })
            .expect("conversion");
        assert!(
            !unrepaired,
            "master-off must not add repaired dictionary edges"
        );
    }

    #[test]
    fn conversion_rejects_advanced_reading_only_repair_but_keeps_rule_repairs() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nないか\t内科\t0\t0\t1\t1\t\t\nこんにちは\t今日は\t0\t0\t100\t100\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");

        let advanced_surface = service
            .with_candidates("なぜか", ConversionOptions::default(), |candidates| {
                candidates
                    .iter()
                    .map(|candidate| candidate.text().to_owned())
                    .collect::<Vec<_>>()
            })
            .expect("conversion");
        assert!(
            !advanced_surface.iter().any(|text| text == "内科"),
            "reading-only Advanced must not turn なぜか into 内科: {advanced_surface:?}"
        );

        let rule_surface = service
            .with_candidates("こにちは", ConversionOptions::default(), |candidates| {
                candidates
                    .iter()
                    .map(|candidate| candidate.text().to_owned())
                    .collect::<Vec<_>>()
            })
            .expect("conversion");
        assert!(
            rule_surface.iter().any(|text| text == "今日は"),
            "n-count Rule repair must remain available: {rule_surface:?}"
        );
    }

    #[test]
    fn english_spelling_hint_finds_katakana_loanwords() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nアップル\tアップル\t0\t0\t100\t100\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");
        let found = service
            .with_candidates(
                "あっｐｌｅ",
                ConversionOptions::default(),
                |candidates| {
                    candidates
                        .iter()
                        .any(|candidate| candidate.text() == "アップル")
                },
            )
            .expect("conversion");
        assert!(found);
    }

    #[test]
    fn spelling_correction_entries_follow_the_unified_admission_gate() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nあい\t藍\t0\t0\t50\t50\tcorrection\t\nあい\t愛\t0\t0\t100\t100\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");

        // Positive control: full gate open must surface SPELLING_CORRECTION.
        let allowed = service
            .with_candidates("あい", ConversionOptions::default(), |candidates| {
                candidates.iter().any(|candidate| candidate.text() == "藍")
            })
            .expect("conversion");
        assert!(allowed, "positive control: SPELLING_CORRECTION must appear");

        let skip = ConversionOptions {
            skip_input_repair: true,
            ..ConversionOptions::default()
        };
        let skipped = service
            .with_candidates("あい", skip, |candidates| {
                candidates.iter().any(|candidate| candidate.text() == "藍")
            })
            .expect("conversion");
        assert!(!skipped, "skip_input_repair must drop SPELLING_CORRECTION");

        let mut master_off = ConversionOptions::default();
        master_off.input_support.enabled = false;
        let gated = service
            .with_candidates("あい", master_off, |candidates| {
                candidates.iter().any(|candidate| candidate.text() == "藍")
            })
            .expect("conversion");
        assert!(!gated, "master-off must drop SPELLING_CORRECTION");

        let mut no_fuzzy = ConversionOptions::default();
        no_fuzzy.input_support.fuzzy_proper_nouns = false;
        let fuzzy_off = service
            .with_candidates("あい", no_fuzzy, |candidates| {
                candidates.iter().any(|candidate| candidate.text() == "藍")
            })
            .expect("conversion");
        assert!(
            !fuzzy_off,
            "fuzzy_proper_nouns off must drop SPELLING_CORRECTION"
        );

        // Contract: conversion admission matches the shared InputSupport helper.
        assert!(allows_spelling_for_options(&ConversionOptions::default()));
        let skipped = ConversionOptions {
            skip_input_repair: true,
            ..ConversionOptions::default()
        };
        assert!(!allows_spelling_for_options(&skipped));
    }

    fn allows_spelling_for_options(options: &ConversionOptions) -> bool {
        options
            .input_support
            .allows_spelling_correction(options.skip_input_repair)
    }

    #[test]
    fn commit_repair_hints_only_cover_the_full_query_span() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nこんにちは\t今日は\t0\t0\t100\t100\t\t\nにちは\t日は\t0\t0\t10\t10\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");

        // Isolate commit hints from local rule repair so a suffix n-count fix
        // cannot masquerade as a whole-query commit paste.
        let mut commit_only = ConversionOptions::default();
        commit_only.input_support.advanced = false;
        commit_only.input_support.vowel_count = false;
        commit_only.input_support.consonant_extra = false;
        commit_only.input_support.n_count = false;
        commit_only.input_support.dakuten_swap = false;
        commit_only.input_support.tsu_sokuon = false;
        commit_only.input_support.wa_wo = false;
        commit_only.input_support.small_u = false;
        commit_only.input_support.english_to_katakana = false;

        // Whole-query typed with a commit hint for the repaired reading.
        let with_hint = service
            .with_conversion_hints(
                "こにちは",
                commit_only,
                &["こんにちは"],
                |candidates, _| {
                    candidates
                        .iter()
                        .any(|candidate| candidate.text() == "今日は")
                },
            )
            .expect("conversion");
        assert!(with_hint, "full typed match must accept commit repair");

        // Longer reading that only contains the typo as a suffix must not paste
        // the whole-query hint onto an intermediate start (あ + 今日は).
        let longer = service
            .with_conversion_hints(
                "あこにちは",
                commit_only,
                &["こんにちは"],
                |candidates, _| {
                    candidates
                        .iter()
                        .map(|candidate| candidate.text().to_owned())
                        .collect::<Vec<_>>()
                },
            )
            .expect("conversion");
        assert!(
            longer.iter().all(|text| text != "あ今日は"),
            "commit repair must not attach at an intermediate start: {longer:?}"
        );
    }

    #[test]
    fn gated_spelling_correction_does_not_pollute_exact_edge_budget() {
        // Twelve cheap SPELLING_CORRECTION hits would fill the per-length budget
        // if they were counted before the Issue #63 gate. A single normal entry
        // must still be admitted when the gate is closed.
        let mut tsv = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        );
        for index in 0..12 {
            tsv.push_str(&format!("あい\t藍{index}\t0\t0\t1\t1\tcorrection\t\n"));
        }
        tsv.push_str("あい\t愛\t0\t0\t100\t100\t\t\n");
        let entries = dictc::parse_entries("fixture.tsv", &tsv).expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");

        let skip = ConversionOptions {
            skip_input_repair: true,
            ..ConversionOptions::default()
        };
        let texts = service
            .with_candidates("あい", skip, |candidates| {
                candidates
                    .iter()
                    .map(|candidate| candidate.text().to_owned())
                    .collect::<Vec<_>>()
            })
            .expect("conversion");
        assert!(
            texts.iter().any(|text| text == "愛"),
            "normal entry must keep a slot when SPELLING_CORRECTION is gated off: {texts:?}"
        );
        assert!(
            texts.iter().all(|text| !text.starts_with('藍')),
            "gated SPELLING_CORRECTION must not appear: {texts:?}"
        );
    }

    #[test]
    fn single_segment_repair_budget_tracks_admitted_exact_edges() {
        use sakura_core::ConversionMethod;

        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");

        // exact 0: only the repaired reading is in the dictionary.
        let zero = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nおはよう\tお早う\t0\t0\t50\t50\t\t\n",
        )
        .expect("entries");
        let zero_bytes = Box::leak(
            dictc::compile(&zero, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let zero_service = ConversionService::from_static_bytes(zero_bytes).expect("service");
        let single = ConversionOptions {
            method: ConversionMethod::SingleSegment,
            ..ConversionOptions::default()
        };
        let repaired = zero_service
            .with_candidates("おはよ", single, |candidates| {
                candidates
                    .iter()
                    .any(|candidate| candidate.text() == "お早う")
            })
            .expect("conversion");
        assert!(repaired, "exact 0 must leave full repair budget");

        // exact 1: one exact surface plus a repair target both fit.
        let one = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nおはよ\t御はよ\t0\t0\t80\t80\t\t\nおはよう\tお早う\t0\t0\t50\t50\t\t\n",
        )
        .expect("entries");
        let one_bytes = Box::leak(
            dictc::compile(&one, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let one_service = ConversionService::from_static_bytes(one_bytes).expect("service");
        let texts = one_service
            .with_candidates("おはよ", single, |candidates| {
                candidates
                    .iter()
                    .map(|candidate| candidate.text().to_owned())
                    .collect::<Vec<_>>()
            })
            .expect("conversion");
        assert!(
            texts.iter().any(|text| text == "御はよ"),
            "exact surface must remain: {texts:?}"
        );
        assert!(
            texts.iter().any(|text| text == "お早う"),
            "exact 1 must still allow repair into unused slots: {texts:?}"
        );

        // exact MAX (12): repair must not add a 13th surface.
        let mut max_tsv = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        );
        for index in 0..12 {
            max_tsv.push_str(&format!(
                "おはよ\t御はよ{index}\t0\t0\t{index}\t{index}\t\t\n"
            ));
        }
        max_tsv.push_str("おはよう\tお早う\t0\t0\t1\t1\t\t\n");
        let max = dictc::parse_entries("fixture.tsv", &max_tsv).expect("entries");
        let max_bytes = Box::leak(
            dictc::compile(&max, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let max_service = ConversionService::from_static_bytes(max_bytes).expect("service");
        let max_texts = max_service
            .with_candidates("おはよ", single, |candidates| {
                candidates
                    .iter()
                    .map(|candidate| candidate.text().to_owned())
                    .collect::<Vec<_>>()
            })
            .expect("conversion");
        assert!(
            max_texts.iter().all(|text| text != "お早う"),
            "exact MAX must leave zero local repair slots: {max_texts:?}"
        );
        assert!(
            max_texts.iter().any(|text| text.starts_with("御はよ")),
            "exact surfaces must still convert: {max_texts:?}"
        );
    }

    #[test]
    fn suppress_skip_blocks_every_repair_source_for_the_same_reading() {
        let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nあい\t藍\t0\t0\t10\t10\tcorrection\t\nあい\t愛\t0\t0\t100\t100\t\t\nおはよう\tお早う\t0\t0\t20\t20\t\t\nこんにちは\t今日は\t0\t0\t30\t30\t\t\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("compile")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");

        let suppressed = ConversionOptions {
            skip_input_repair: true,
            ..ConversionOptions::default()
        };

        let spelling = service
            .with_candidates("あい", suppressed, |candidates| {
                candidates.iter().any(|candidate| candidate.text() == "藍")
            })
            .expect("conversion");
        assert!(!spelling, "suppress must drop SPELLING_CORRECTION");

        let rule = service
            .with_candidates("おはよ", suppressed, |candidates| {
                candidates
                    .iter()
                    .any(|candidate| candidate.text() == "お早う")
            })
            .expect("conversion");
        assert!(!rule, "suppress must drop rule repair");

        let commit = service
            .with_conversion_hints(
                "こにちは",
                suppressed,
                &["こんにちは"],
                |candidates, _| {
                    candidates
                        .iter()
                        .any(|candidate| candidate.text() == "今日は")
                },
            )
            .expect("conversion");
        assert!(!commit, "suppress must drop commit repair");

        // A different reading is a new composition and must keep repair.
        let other = service
            .with_candidates("おはよ", ConversionOptions::default(), |candidates| {
                candidates
                    .iter()
                    .any(|candidate| candidate.text() == "お早う")
            })
            .expect("conversion");
        assert!(other, "unsuppressed reading must still repair");
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

    #[test]
    fn converting_today_offers_local_reiwa_and_gregorian_date_surfaces() {
        let entries = dictc::parse_entries(
            "today.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nきょう\t今日\t0\t0\t100\t100\t\tcommon\n",
        )
        .expect("entries");
        let matrix = dictc::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let bytes = Box::leak(
            dictc::compile(&entries, &matrix)
                .expect("image")
                .into_boxed_slice(),
        );
        let service = ConversionService::from_static_bytes(bytes).expect("service");
        let today = local_civil_date().expect("local civil date");
        let texts = service
            .with_candidates("きょう", ConversionOptions::default(), |candidates| {
                assert_eq!(candidates[0].text(), "今日");
                candidates
                    .iter()
                    .map(|candidate| candidate.text().to_owned())
                    .collect::<Vec<_>>()
            })
            .expect("conversion");

        let mut reiwa = String::new();
        sakura_core::DateFormat::JapaneseEra
            .write(today, &mut reiwa)
            .expect("reiwa");
        let mut reiwa_weekday = String::new();
        sakura_core::DateFormat::JapaneseEraWeekday
            .write(today, &mut reiwa_weekday)
            .expect("reiwa weekday");
        let mut gregorian = String::new();
        sakura_core::DateFormat::Gregorian
            .write(today, &mut gregorian)
            .expect("gregorian");
        let mut gregorian_weekday = String::new();
        sakura_core::DateFormat::GregorianWeekday
            .write(today, &mut gregorian_weekday)
            .expect("gregorian weekday");

        for expected in [reiwa, reiwa_weekday, gregorian, gregorian_weekday] {
            assert!(
                texts.iter().any(|text| text == &expected),
                "missing live date candidate {expected} in {texts:?}"
            );
        }
    }
}
