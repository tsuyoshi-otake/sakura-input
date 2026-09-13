#[cfg(any(test, feature = "test-hooks"))]
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::codec::{
    crc32, decode_record, encode_record, header, read_header, record_at, scan_records,
    upgrade_to_current,
};
use super::format::{
    LearningRecord, LearningSnapshot, FORMAT_VERSION_1, FORMAT_VERSION_2, HEADER_LEN,
    LEARNING_FORMAT_VERSION, MAX_LEARNING_LOG_BYTES, MAX_RECORD_BYTES, RECORD_ENVELOPE_LEN,
    REPAIR_SUPPRESS_CONTEXT, REPAIR_SUPPRESS_SURFACE,
};
use super::replay::ReplayView;

const COMPACTION_TRIGGER_BYTES: u64 = 16 * 1024 * 1024;
const COMPACTION_TARGET_BYTES: usize = 8 * 1024 * 1024;
const MAX_LOG_RECORDS: u64 = 50_000;
const TARGET_LOG_RECORDS: u64 = 40_000;
static NEXT_ARTIFACT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OperationReceipt {
    pub maintenance_failure_delta: u64,
    pub recovered_tail_bytes: u64,
}

#[derive(Debug)]
pub struct LearningLogError {
    pub source: io::Error,
    pub receipt: OperationReceipt,
}

impl core::fmt::Display for LearningLogError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for LearningLogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
pub struct LearningLog {
    file: Option<File>,
    path: Option<PathBuf>,
    bytes: u64,
    records: u64,
    dirty_records: u64,
    forget_artifacts: ForgetArtifacts,
}

#[derive(Debug)]
pub enum LogMaintenance<T> {
    NotDue,
    Flushed,
    Compacted(T),
}

#[derive(Debug)]
pub enum LogForget {
    NotFound,
    Unavailable,
    Removed { removed: u64 },
}

impl LearningLog {
    pub fn memory() -> Self {
        Self {
            file: None,
            path: None,
            bytes: 0,
            records: 0,
            dirty_records: 0,
            forget_artifacts: ForgetArtifacts::default(),
        }
    }

    pub fn open<T>(
        path: &Path,
        mut prepare: impl FnMut(ReplayView<'_>) -> T,
    ) -> Result<(Self, T, OperationReceipt), LearningLogError> {
        let mut receipt = OperationReceipt::default();
        let opened = (|| -> io::Result<(Self, T)> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let forget_artifacts = recover_forget_artifacts_at_startup(path)?;
            create_if_missing(path)?;

            let file_len = fs::metadata(path)?.len();
            let mut bytes = Vec::new();
            File::open(path)?
                .take(MAX_LEARNING_LOG_BYTES)
                .read_to_end(&mut bytes)?;
            let mut over_cap = file_len.saturating_sub(bytes.len() as u64);
            let version = read_header(&bytes)?;
            if matches!(version, FORMAT_VERSION_1 | FORMAT_VERSION_2) {
                bytes = upgrade_to_current(&bytes, version)?;
                let cap = usize::try_from(MAX_LEARNING_LOG_BYTES).unwrap_or(usize::MAX);
                if bytes.len() > cap {
                    over_cap = over_cap.saturating_add((bytes.len() - cap) as u64);
                    bytes.truncate(cap);
                }
                publish_upgrade(path, &bytes, version)?;
            } else if version != LEARNING_FORMAT_VERSION {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported learning format version {version}"),
                ));
            }

            let (last_good, records) = scan_records(&bytes, LEARNING_FORMAT_VERSION)?;
            let prepared = prepare(ReplayView::from_verified(
                &bytes[..last_good],
                LEARNING_FORMAT_VERSION,
            ));
            let recovered = over_cap.saturating_add(
                u64::try_from(bytes.len().saturating_sub(last_good)).unwrap_or(u64::MAX),
            );
            let truncate_owner = OpenOptions::new().read(true).write(true).open(path)?;
            if recovered > 0 {
                truncate_owner.set_len(u64::try_from(last_good).unwrap_or(u64::MAX))?;
            }
            receipt.recovered_tail_bytes = recovered;
            drop(truncate_owner);
            let file = open_append(path)?;
            let mut log = Self {
                file: Some(file),
                path: Some(path.to_owned()),
                bytes: u64::try_from(last_good).unwrap_or(u64::MAX),
                records,
                dirty_records: 0,
                forget_artifacts,
            };
            let prepared = if log.bytes > COMPACTION_TRIGGER_BYTES || log.records > MAX_LOG_RECORDS
            {
                compact_for_open(
                    &mut log,
                    COMPACTION_TARGET_BYTES,
                    TARGET_LOG_RECORDS,
                    &mut prepare,
                )?
            } else {
                prepared
            };
            Ok((log, prepared))
        })();
        opened
            .map(|(log, prepared)| (log, prepared, receipt))
            .map_err(|source| LearningLogError { source, receipt })
    }

    pub fn append(
        &mut self,
        reading: &str,
        surface: &str,
        left_context: u16,
        right_context: u16,
        day: u32,
    ) -> Result<(), LearningLogError> {
        let payload = encode_record(reading, surface, left_context, right_context, day)
            .map_err(LearningLogError::without_receipt)?;
        self.append_payload(&payload)
    }

    pub fn append_repair_suppress(
        &mut self,
        reading: &str,
        day: u32,
    ) -> Result<(), LearningLogError> {
        let payload = encode_record(
            reading,
            REPAIR_SUPPRESS_SURFACE,
            REPAIR_SUPPRESS_CONTEXT,
            REPAIR_SUPPRESS_CONTEXT,
            day,
        )
        .map_err(LearningLogError::without_receipt)?;
        self.append_payload(&payload)
    }

    fn append_payload(&mut self, payload: &[u8]) -> Result<(), LearningLogError> {
        if self.path.is_none() {
            return Ok(());
        }
        let Some(file) = self.file.as_mut() else {
            return Err(LearningLogError::without_receipt(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "learning log writer unavailable",
            )));
        };
        let length = u32::try_from(payload.len()).map_err(|_| {
            LearningLogError::without_receipt(io::Error::new(
                io::ErrorKind::InvalidInput,
                "learning record length overflow",
            ))
        })?;
        let frame_bytes = u64::try_from(RECORD_ENVELOPE_LEN + payload.len()).unwrap_or(u64::MAX);
        if self.bytes.saturating_add(frame_bytes) > MAX_LEARNING_LOG_BYTES {
            return Err(LearningLogError::without_receipt(io::Error::new(
                io::ErrorKind::Other,
                "learning log is at capacity",
            )));
        }
        if let Err(source) = (|| -> io::Result<()> {
            file.write_all(&length.to_le_bytes())?;
            file.write_all(&crc32(payload).to_le_bytes())?;
            file.write_all(payload)
        })() {
            self.file = None;
            return Err(LearningLogError::without_receipt(source));
        }
        self.bytes = self.bytes.saturating_add(frame_bytes);
        self.records = self.records.saturating_add(1);
        self.dirty_records = self.dirty_records.saturating_add(1);
        Ok(())
    }

    pub fn maintain<T>(
        &mut self,
        mut prepare: impl FnMut(ReplayView<'_>) -> T,
    ) -> Result<(LogMaintenance<T>, OperationReceipt), LearningLogError> {
        let mut receipt = OperationReceipt::default();
        let result = (|| -> io::Result<LogMaintenance<T>> {
            let Some(path) = self.path.clone() else {
                return Ok(LogMaintenance::NotDue);
            };
            if let Err(error) = self.forget_artifacts.settle(&path) {
                receipt.maintenance_failure_delta = 1;
                return Err(error);
            }
            if self.file.is_none()
                || self.bytes > COMPACTION_TRIGGER_BYTES
                || self.records > MAX_LOG_RECORDS
            {
                match compact_for_open(
                    self,
                    COMPACTION_TARGET_BYTES,
                    TARGET_LOG_RECORDS,
                    &mut prepare,
                ) {
                    Ok(prepared) => Ok(LogMaintenance::Compacted(prepared)),
                    Err(error) => {
                        receipt.maintenance_failure_delta = 1;
                        Err(error)
                    }
                }
            } else if self.dirty_records > 0 {
                let sync = self
                    .file
                    .as_ref()
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::BrokenPipe, "log writer unavailable")
                    })
                    .and_then(File::sync_data);
                if let Err(error) = sync {
                    self.file = None;
                    receipt.maintenance_failure_delta = 1;
                    return Err(error);
                }
                self.dirty_records = 0;
                Ok(LogMaintenance::Flushed)
            } else {
                Ok(LogMaintenance::NotDue)
            }
        })();
        result
            .map(|outcome| (outcome, receipt))
            .map_err(|source| LearningLogError { source, receipt })
    }

    pub fn forget_exact<T>(
        &mut self,
        reading: &str,
        surface: &str,
        prepare: impl FnOnce(ReplayView<'_>) -> T,
        publish: impl FnOnce(T),
    ) -> Result<(LogForget, OperationReceipt), LearningLogError> {
        let mut receipt = OperationReceipt::default();
        let result = (|| -> io::Result<LogForget> {
            if reading.is_empty() || surface.is_empty() {
                return Ok(LogForget::NotFound);
            }
            let Some(path) = self.path.clone() else {
                return Ok(LogForget::Unavailable);
            };
            if let Err(error) = self.forget_artifacts.settle(&path) {
                receipt.maintenance_failure_delta =
                    receipt.maintenance_failure_delta.saturating_add(1);
                return Err(error);
            }
            let file = self.file.as_ref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "learning log writer unavailable")
            })?;
            file.sync_data()?;
            let source = read_within_bound(&path)?;
            if read_header(&source)? != LEARNING_FORMAT_VERSION {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "learning log version changed during prediction deletion",
                ));
            }
            let (last_good, total_records) = scan_records(&source, LEARNING_FORMAT_VERSION)?;
            if last_good != source.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "learning log has an unverified tail during prediction deletion",
                ));
            }

            let mut rewritten = header(LEARNING_FORMAT_VERSION).to_vec();
            let mut offset = HEADER_LEN;
            let mut removed = 0u64;
            while let Some((next, record)) = record_at(&source, LEARNING_FORMAT_VERSION, offset) {
                if record.reading == reading && record.surface == surface {
                    removed = removed.saturating_add(1);
                } else {
                    rewritten.extend_from_slice(&source[offset..next]);
                }
                offset = next;
            }
            if removed == 0 {
                return Ok(LogForget::NotFound);
            }
            let rebuilt_sequence = total_records.saturating_sub(removed);
            let prepared = prepare(ReplayView::from_verified(
                &rewritten,
                LEARNING_FORMAT_VERSION,
            ));
            let temporary = forget_temporary_path(&path);
            let backup = forget_recovery_path(&path);
            ensure_forget_transaction_paths_are_clear(&temporary, &backup)?;
            if let Err(error) = write_forget_temporary(&temporary, &rewritten) {
                self.forget_artifacts
                    .track_temporary_cleanup(temporary.clone());
                return match self.forget_artifacts.settle(&path) {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => {
                        receipt.maintenance_failure_delta =
                            receipt.maintenance_failure_delta.saturating_add(1);
                        Err(with_follow_up_error(
                            "prediction deletion temporary write failed",
                            error,
                            cleanup_error,
                        ))
                    }
                };
            }
            let replacement_file = match open_forget_replacement(&temporary) {
                Ok(file) => file,
                Err(error) => {
                    self.forget_artifacts
                        .track_temporary_cleanup(temporary.clone());
                    return match self.forget_artifacts.settle(&path) {
                        Ok(()) => Err(error),
                        Err(cleanup_error) => {
                            receipt.maintenance_failure_delta =
                                receipt.maintenance_failure_delta.saturating_add(1);
                            Err(with_follow_up_error(
                                "prediction deletion replacement preparation failed",
                                error,
                                cleanup_error,
                            ))
                        }
                    };
                }
            };

            if let Err(publish_error) = publish_forget_replacement(&path, &temporary, &backup) {
                let ForgetPublishError {
                    confirmed_phase,
                    error,
                } = publish_error;
                let (publish_state, terminal_error, observation_succeeded) =
                    match observe_forget_publish_state(
                        &path, &temporary, &backup, &source, &rewritten,
                    ) {
                        Ok(observed) => {
                            let resolved = if confirmed_phase
                                == ForgetPublishPhase::ReplacementMovedToCanonical
                            {
                                ForgetPublishState::FilteredCanonical
                            } else {
                                observed
                            };
                            (resolved, error, true)
                        }
                        Err(observation_error) => {
                            receipt.maintenance_failure_delta =
                                receipt.maintenance_failure_delta.saturating_add(1);
                            (
                                confirmed_phase.fallback_state(),
                                with_follow_up_error(
                                    "prediction deletion publish failed while observing recovery state",
                                    error,
                                    observation_error,
                                ),
                                false,
                            )
                        }
                    };
                match publish_state {
                    ForgetPublishState::FilteredCanonical => {
                        if observation_succeeded {
                            receipt.maintenance_failure_delta =
                                receipt.maintenance_failure_delta.saturating_add(1);
                        }
                    }
                    ForgetPublishState::OldCanonical { backup_present } => {
                        self.forget_artifacts.track_temporary_cleanup(temporary);
                        if backup_present {
                            self.forget_artifacts.track_backup_cleanup(backup);
                        }
                        return match self.forget_artifacts.settle(&path) {
                            Ok(()) => Err(terminal_error),
                            Err(recovery_error) => {
                                receipt.maintenance_failure_delta =
                                    receipt.maintenance_failure_delta.saturating_add(1);
                                Err(with_follow_up_error(
                                    "prediction deletion publish failed",
                                    terminal_error,
                                    recovery_error,
                                ))
                            }
                        };
                    }
                    ForgetPublishState::RecoveryRequired => {
                        self.forget_artifacts.track_temporary_cleanup(temporary);
                        self.forget_artifacts.restore_backup = Some(backup);
                        return match self.forget_artifacts.settle(&path) {
                            Ok(()) => Err(terminal_error),
                            Err(recovery_error) => {
                                receipt.maintenance_failure_delta =
                                    receipt.maintenance_failure_delta.saturating_add(1);
                                Err(with_follow_up_error(
                                    "prediction deletion publish failed",
                                    terminal_error,
                                    recovery_error,
                                ))
                            }
                        };
                    }
                }
            }

            *self = LearningLog {
                file: Some(replacement_file),
                path: Some(path.clone()),
                bytes: u64::try_from(rewritten.len()).unwrap_or(u64::MAX),
                records: rebuilt_sequence,
                dirty_records: 0,
                forget_artifacts: ForgetArtifacts {
                    restore_backup: None,
                    backup_cleanup: vec![backup],
                    temporary_cleanup: Vec::new(),
                },
            };
            publish(prepared);
            if self.forget_artifacts.settle(&path).is_err() {
                receipt.maintenance_failure_delta =
                    receipt.maintenance_failure_delta.saturating_add(1);
            }
            Ok(LogForget::Removed { removed })
        })();
        result
            .map(|outcome| (outcome, receipt))
            .map_err(|source| LearningLogError { source, receipt })
    }

    pub fn clear<T>(
        &mut self,
        prepare: impl FnOnce(ReplayView<'_>) -> T,
    ) -> Result<(u64, T, OperationReceipt), LearningLogError> {
        let mut receipt = OperationReceipt::default();
        let result = (|| -> io::Result<(u64, T)> {
            let cleared_records = self.records;
            let empty = header(LEARNING_FORMAT_VERSION);
            let Some(path) = self.path.clone() else {
                let prepared = prepare(ReplayView::from_verified(&empty, LEARNING_FORMAT_VERSION));
                *self = LearningLog::memory();
                return Ok((cleared_records, prepared));
            };
            if let Err(error) = self.forget_artifacts.settle(&path) {
                receipt.maintenance_failure_delta = 1;
                return Err(error);
            }
            if let Some(file) = self.file.as_ref() {
                file.sync_data()?;
            }
            let temporary = unique_sibling(&path, "clear.tmp");
            let backup = unique_sibling(&path, "clear.bak");
            write_new_file(&temporary, &empty)?;
            self.file = None;
            if let Err(error) = fs::rename(&path, &backup) {
                self.file = open_append(&path).ok();
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
            if let Err(error) = fs::rename(&temporary, &path) {
                let rollback = fs::rename(&backup, &path);
                self.file = open_append(&path).ok();
                let _ = fs::remove_file(&temporary);
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(io::Error::new(
                        rollback_error.kind(),
                        format!(
                            "clear publish failed ({error}); rollback failed ({rollback_error})"
                        ),
                    )),
                };
            }
            let file = match open_append(&path) {
                Ok(file) => file,
                Err(error) => {
                    let _ = fs::remove_file(&path);
                    let rollback = fs::rename(&backup, &path);
                    self.file = open_append(&path).ok();
                    return match rollback {
                        Ok(()) => Err(error),
                        Err(rollback_error) => Err(io::Error::new(
                            rollback_error.kind(),
                            format!("cleared log could not be opened ({error}); rollback failed ({rollback_error})"),
                        )),
                    };
                }
            };
            let _ = fs::remove_file(&backup);
            let prepared = prepare(ReplayView::from_verified(&empty, LEARNING_FORMAT_VERSION));
            *self = LearningLog {
                file: Some(file),
                path: Some(path),
                bytes: HEADER_LEN as u64,
                records: 0,
                dirty_records: 0,
                forget_artifacts: ForgetArtifacts::default(),
            };
            Ok((cleared_records, prepared))
        })();
        result
            .map(|(removed, prepared)| (removed, prepared, receipt))
            .map_err(|source| LearningLogError { source, receipt })
    }
}

impl LearningLogError {
    fn without_receipt(source: io::Error) -> Self {
        Self {
            source,
            receipt: OperationReceipt::default(),
        }
    }
}

pub fn read_snapshot(path: &Path) -> io::Result<LearningSnapshot> {
    let bytes = read_within_bound(path)?;
    let version = read_header(&bytes)?;
    if !matches!(
        version,
        FORMAT_VERSION_1 | FORMAT_VERSION_2 | LEARNING_FORMAT_VERSION
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported learning format version {version}"),
        ));
    }
    let mut offset = HEADER_LEN;
    let mut sequence = 0u64;
    let mut records = Vec::new();
    while let Some((next, record)) = record_at(&bytes, version, offset) {
        sequence = sequence.saturating_add(1);
        if record.left_context != REPAIR_SUPPRESS_CONTEXT
            || record.right_context != REPAIR_SUPPRESS_CONTEXT
            || record.surface != REPAIR_SUPPRESS_SURFACE
        {
            records.push(LearningRecord {
                sequence,
                day: record.day,
                left_context: record.left_context,
                right_context: record.right_context,
                reading: record.reading.to_owned(),
                surface: record.surface.to_owned(),
            });
        }
        offset = next;
    }
    Ok(LearningSnapshot {
        format_version: version,
        records,
        ignored_tail_bytes: u64::try_from(bytes.len().saturating_sub(offset)).unwrap_or(u64::MAX),
    })
}

#[derive(Debug, Default)]
struct ForgetArtifacts {
    restore_backup: Option<PathBuf>,
    backup_cleanup: Vec<PathBuf>,
    temporary_cleanup: Vec<PathBuf>,
}

impl ForgetArtifacts {
    fn track_backup_cleanup(&mut self, path: PathBuf) {
        if !self.backup_cleanup.iter().any(|known| known == &path) {
            self.backup_cleanup.push(path);
        }
    }

    fn track_temporary_cleanup(&mut self, path: PathBuf) {
        if !self.temporary_cleanup.iter().any(|known| known == &path) {
            self.temporary_cleanup.push(path);
        }
    }

    fn settle(&mut self, canonical: &Path) -> io::Result<()> {
        if let Some(backup) = self.restore_backup.clone() {
            restore_forget_backup(canonical, &backup)?;
            self.restore_backup = None;
        }
        settle_forget_cleanup(&mut self.backup_cleanup, ForgetArtifactKind::Backup)?;
        settle_forget_cleanup(&mut self.temporary_cleanup, ForgetArtifactKind::Temporary)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForgetArtifactKind {
    Backup,
    Temporary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "test-hooks", doc(hidden))]
#[cfg(any(test, feature = "test-hooks"))]
pub enum LearningFaultPoint {
    ReplacementOwnerOpen,
    Publish,
    PublishMovesOldToRecovery,
    PublishCommitsThenErrors,
    PublishObservation,
    RecoveryRestore,
    BackupCleanup,
    TemporaryCleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(not(any(test, feature = "test-hooks")))]
enum LearningFaultPoint {
    ReplacementOwnerOpen,
    Publish,
    PublishMovesOldToRecovery,
    PublishCommitsThenErrors,
    PublishObservation,
    RecoveryRestore,
    BackupCleanup,
    TemporaryCleanup,
}

#[cfg(any(test, feature = "test-hooks"))]
thread_local! {
    static LEARNING_FAULTS: RefCell<Vec<LearningFaultPoint>> = const { RefCell::new(Vec::new()) };
}

#[cfg(feature = "test-hooks")]
#[derive(Debug)]
#[doc(hidden)]
pub struct LearningFaultScope;

#[cfg(feature = "test-hooks")]
impl LearningFaultScope {
    pub fn new(points: &[LearningFaultPoint]) -> Self {
        LEARNING_FAULTS.with(|faults| {
            let mut faults = faults.borrow_mut();
            assert!(
                faults.is_empty(),
                "learning fault queue leaked from another test phase"
            );
            faults.extend_from_slice(points);
        });
        Self
    }
}

#[cfg(feature = "test-hooks")]
impl Drop for LearningFaultScope {
    fn drop(&mut self) {
        LEARNING_FAULTS.with(|faults| faults.borrow_mut().clear());
    }
}

#[cfg(any(test, feature = "test-hooks"))]
fn take_learning_fault(point: LearningFaultPoint) -> bool {
    LEARNING_FAULTS.with(|faults| {
        let mut faults = faults.borrow_mut();
        if faults.first().copied() == Some(point) {
            faults.remove(0);
            true
        } else {
            false
        }
    })
}

#[cfg(not(any(test, feature = "test-hooks")))]
fn take_learning_fault(_point: LearningFaultPoint) -> bool {
    false
}

fn injected_learning_error(point: LearningFaultPoint) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("injected exact-prediction deletion fault at {point:?}"),
    )
}

fn forget_temporary_path(path: &Path) -> PathBuf {
    path.with_extension("forget.tmp")
}

fn forget_recovery_path(path: &Path) -> PathBuf {
    path.with_extension("forget.recovery")
}

fn legacy_forget_backup_path(path: &Path) -> PathBuf {
    path.with_extension("forget.bak")
}

fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn scan_repairable_current_recovery_log(bytes: &[u8]) -> io::Result<usize> {
    if read_header(bytes)? != LEARNING_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exact-prediction recovery log has an unsupported format",
        ));
    }
    let mut offset = HEADER_LEN;
    while offset < bytes.len() {
        if bytes.len() - offset < RECORD_ENVELOPE_LEN {
            return Ok(offset);
        }
        let length = usize::try_from(u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("checked complete record envelope"),
        ))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "record length overflow"))?;
        if length > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exact-prediction recovery log has an invalid complete record length",
            ));
        }
        let expected_crc = u32::from_le_bytes(
            bytes[offset + 4..offset + RECORD_ENVELOPE_LEN]
                .try_into()
                .expect("checked complete record envelope"),
        );
        let payload_start = offset + RECORD_ENVELOPE_LEN;
        let payload_end = payload_start
            .checked_add(length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "record length overflow"))?;
        if payload_end > bytes.len() {
            return Ok(offset);
        }
        let payload = &bytes[payload_start..payload_end];
        if crc32(payload) != expected_crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exact-prediction recovery log has an invalid complete record checksum",
            ));
        }
        decode_record(payload, LEARNING_FORMAT_VERSION).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("exact-prediction recovery log has an invalid complete record ({error})"),
            )
        })?;
        offset = payload_end;
    }
    Ok(offset)
}

fn repair_forget_recovery_log(path: &Path) -> io::Result<()> {
    let bytes = fs::read(path)?;
    let last_good = scan_repairable_current_recovery_log(&bytes)?;
    if last_good == bytes.len() {
        return Ok(());
    }
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    file.set_len(u64::try_from(last_good).unwrap_or(u64::MAX))?;
    file.sync_all()
}

fn restore_forget_backup(canonical: &Path, backup: &Path) -> io::Result<()> {
    if path_exists(canonical)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot restore exact-prediction recovery over an existing canonical log",
        ));
    }
    repair_forget_recovery_log(backup)?;
    if take_learning_fault(LearningFaultPoint::RecoveryRestore) {
        return Err(injected_learning_error(LearningFaultPoint::RecoveryRestore));
    }
    fs::rename(backup, canonical)
}

fn recover_forget_artifacts_at_startup(path: &Path) -> io::Result<ForgetArtifacts> {
    let mut artifacts = ForgetArtifacts::default();
    let temporary = forget_temporary_path(path);
    if path_exists(&temporary)? {
        artifacts.track_temporary_cleanup(temporary);
    }
    let mut backups = Vec::with_capacity(2);
    for backup in [forget_recovery_path(path), legacy_forget_backup_path(path)] {
        if path_exists(&backup)? {
            backups.push(backup);
        }
    }
    if path_exists(path)? {
        for backup in backups {
            artifacts.track_backup_cleanup(backup);
        }
        return Ok(artifacts);
    }
    match backups.as_slice() {
        [] => Ok(artifacts),
        [backup] => {
            restore_forget_backup(path, backup)?;
            Ok(artifacts)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "multiple exact-prediction recovery logs exist while canonical is absent",
        )),
    }
}

fn settle_forget_cleanup(paths: &mut Vec<PathBuf>, kind: ForgetArtifactKind) -> io::Result<()> {
    let index = 0;
    while index < paths.len() {
        match remove_forget_artifact(&paths[index], kind) {
            Ok(()) => {
                paths.swap_remove(index);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForgetPublishState {
    FilteredCanonical,
    OldCanonical { backup_present: bool },
    RecoveryRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForgetPublishPhase {
    BeforeFirstRename,
    OldMovedToRecovery,
    ReplacementMovedToCanonical,
}

impl ForgetPublishPhase {
    fn fallback_state(self) -> ForgetPublishState {
        match self {
            Self::BeforeFirstRename => ForgetPublishState::OldCanonical {
                backup_present: false,
            },
            Self::OldMovedToRecovery => ForgetPublishState::RecoveryRequired,
            Self::ReplacementMovedToCanonical => ForgetPublishState::FilteredCanonical,
        }
    }
}

#[derive(Debug)]
struct ForgetPublishError {
    confirmed_phase: ForgetPublishPhase,
    error: io::Error,
}

fn with_follow_up_error(operation: &str, primary: io::Error, follow_up: io::Error) -> io::Error {
    io::Error::new(
        primary.kind(),
        format!("{operation} ({primary}); follow-up failed ({follow_up})"),
    )
}

fn ensure_forget_transaction_paths_are_clear(temporary: &Path, backup: &Path) -> io::Result<()> {
    for (kind, path) in [("temporary", temporary), ("recovery", backup)] {
        if path_exists(path)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("exact-prediction deletion {kind} artifact still requires settlement"),
            ));
        }
    }
    Ok(())
}

fn observe_forget_publish_state(
    canonical: &Path,
    temporary: &Path,
    backup: &Path,
    source: &[u8],
    filtered: &[u8],
) -> io::Result<ForgetPublishState> {
    if take_learning_fault(LearningFaultPoint::PublishObservation) {
        return Err(injected_learning_error(
            LearningFaultPoint::PublishObservation,
        ));
    }
    let backup_present = path_exists(backup)?;
    let temporary_present = path_exists(temporary)?;
    let canonical = match fs::read(canonical) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    match canonical.as_deref() {
        Some(bytes) if bytes == filtered && !temporary_present => {
            Ok(ForgetPublishState::FilteredCanonical)
        }
        Some(bytes) if bytes == source => Ok(ForgetPublishState::OldCanonical { backup_present }),
        None if backup_present => Ok(ForgetPublishState::RecoveryRequired),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exact-prediction deletion publish ended in an unrecognised filesystem state",
        )),
    }
}

fn write_forget_temporary(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn open_forget_replacement(path: &Path) -> io::Result<File> {
    if take_learning_fault(LearningFaultPoint::ReplacementOwnerOpen) {
        return Err(injected_learning_error(
            LearningFaultPoint::ReplacementOwnerOpen,
        ));
    }
    open_append(path)
}

fn publish_forget_replacement(
    canonical: &Path,
    replacement: &Path,
    backup: &Path,
) -> Result<(), ForgetPublishError> {
    let mut confirmed_phase = ForgetPublishPhase::BeforeFirstRename;
    if take_learning_fault(LearningFaultPoint::Publish) {
        return Err(ForgetPublishError {
            confirmed_phase,
            error: injected_learning_error(LearningFaultPoint::Publish),
        });
    }
    fs::rename(canonical, backup).map_err(|error| ForgetPublishError {
        confirmed_phase,
        error,
    })?;
    confirmed_phase = ForgetPublishPhase::OldMovedToRecovery;
    if take_learning_fault(LearningFaultPoint::PublishMovesOldToRecovery) {
        return Err(ForgetPublishError {
            confirmed_phase,
            error: injected_learning_error(LearningFaultPoint::PublishMovesOldToRecovery),
        });
    }
    fs::rename(replacement, canonical).map_err(|error| ForgetPublishError {
        confirmed_phase,
        error,
    })?;
    confirmed_phase = ForgetPublishPhase::ReplacementMovedToCanonical;
    if take_learning_fault(LearningFaultPoint::PublishCommitsThenErrors) {
        return Err(ForgetPublishError {
            confirmed_phase,
            error: injected_learning_error(LearningFaultPoint::PublishCommitsThenErrors),
        });
    }
    Ok(())
}

fn remove_forget_artifact(path: &Path, kind: ForgetArtifactKind) -> io::Result<()> {
    let fault = match kind {
        ForgetArtifactKind::Backup => LearningFaultPoint::BackupCleanup,
        ForgetArtifactKind::Temporary => LearningFaultPoint::TemporaryCleanup,
    };
    if take_learning_fault(fault) {
        return Err(injected_learning_error(fault));
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(feature = "test-hooks")]
impl LearningLog {
    #[doc(hidden)]
    pub fn test_compact<T>(
        &mut self,
        target_bytes: usize,
        target_records: u64,
        mut prepare: impl FnMut(ReplayView<'_>) -> T,
    ) -> Result<T, LearningLogError> {
        let receipt = OperationReceipt::default();
        compact_for_open(self, target_bytes, target_records, &mut prepare)
            .map_err(|source| LearningLogError { source, receipt })
    }

    #[doc(hidden)]
    pub fn test_records(&self) -> u64 {
        self.records
    }

    #[doc(hidden)]
    pub fn test_bytes(&self) -> u64 {
        self.bytes
    }

    #[doc(hidden)]
    pub fn test_set_bytes(&mut self, bytes: u64) {
        self.bytes = bytes;
    }

    #[doc(hidden)]
    pub fn test_disable_writer(&mut self) {
        self.file = None;
    }

    #[doc(hidden)]
    pub fn test_dirty_records(&self) -> u64 {
        self.dirty_records
    }

    #[doc(hidden)]
    pub fn test_sync_append_owner(&self) -> io::Result<()> {
        self.file
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "learning log writer unavailable")
            })?
            .sync_data()
    }
}

#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn test_read_within_bound(path: &Path) -> io::Result<Vec<u8>> {
    read_within_bound(path)
}

fn read_within_bound(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_LEARNING_LOG_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LEARNING_LOG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "learning log exceeds its hard size bound",
        ));
    }
    Ok(bytes)
}

fn create_if_missing(path: &Path) -> io::Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(&header(LEARNING_FORMAT_VERSION))?;
            file.sync_all()
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        OpenOptions::new()
            .append(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        OpenOptions::new().append(true).open(path)
    }
}

fn unique_sibling(path: &Path, suffix: &str) -> PathBuf {
    let preferred = path.with_extension(suffix);
    if !preferred.exists() {
        return preferred;
    }
    let id = NEXT_ARTIFACT.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("{suffix}.{}.{id}", std::process::id()))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn publish_upgrade(path: &Path, bytes: &[u8], source_version: u16) -> io::Result<()> {
    let temporary = unique_sibling(path, "upgrade.tmp");
    let backup = unique_sibling(path, &format!("v{source_version}.bak"));
    write_new_file(&temporary, bytes)?;
    if let Err(error) = fs::rename(path, &backup) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let rollback = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(io::Error::new(
                rollback_error.kind(),
                format!("upgrade publish failed ({error}); rollback failed ({rollback_error})"),
            )),
        };
    }
    Ok(())
}

fn compact_for_open<T>(
    log: &mut LearningLog,
    target_bytes: usize,
    target_records: u64,
    prepare: &mut impl FnMut(ReplayView<'_>) -> T,
) -> io::Result<T> {
    let path = log
        .path
        .clone()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "learning log has no path"))?;
    log.forget_artifacts.settle(&path)?;
    if let Some(file) = log.file.as_ref() {
        file.sync_data()?;
    }
    let source = read_within_bound(&path)?;
    if read_header(&source)? != LEARNING_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "learning log version changed during compaction",
        ));
    }
    let (last_good, total_records) = scan_records(&source, LEARNING_FORMAT_VERSION)?;
    let mut first = HEADER_LEN;
    let mut retained_records = total_records;
    while retained_records > 1
        && (retained_records > target_records || last_good.saturating_sub(first) > target_bytes)
    {
        let Some((next, _)) = record_at(&source, LEARNING_FORMAT_VERSION, first) else {
            break;
        };
        first = next;
        retained_records -= 1;
    }
    let retained_len = last_good.saturating_sub(first);
    let mut compacted = Vec::with_capacity(HEADER_LEN + retained_len);
    compacted.extend_from_slice(&header(LEARNING_FORMAT_VERSION));
    compacted.extend_from_slice(&source[first..last_good]);
    let prepared = prepare(ReplayView::from_verified(
        &compacted,
        LEARNING_FORMAT_VERSION,
    ));

    let temporary = unique_sibling(&path, "compact.tmp");
    let backup = unique_sibling(&path, "compact.bak");
    write_new_file(&temporary, &compacted)?;
    log.file = None;
    if let Err(error) = fs::rename(&path, &backup) {
        log.file = open_append(&path).ok();
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        let rollback = fs::rename(&backup, &path);
        log.file = open_append(&path).ok();
        let _ = fs::remove_file(&temporary);
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(io::Error::new(
                rollback_error.kind(),
                format!("compaction publish failed ({error}); rollback failed ({rollback_error})"),
            )),
        };
    }
    let file = match open_append(&path) {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&path);
            let _ = fs::rename(&backup, &path);
            log.file = open_append(&path).ok();
            return Err(error);
        }
    };
    let _ = fs::remove_file(&backup);
    *log = LearningLog {
        file: Some(file),
        path: Some(path),
        bytes: u64::try_from(compacted.len()).unwrap_or(u64::MAX),
        records: retained_records,
        dirty_records: 0,
        forget_artifacts: ForgetArtifacts::default(),
    };
    Ok(prepared)
}
