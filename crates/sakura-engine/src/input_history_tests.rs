thread_local! {
    // Only unit-test binaries contain this rendezvous; release/default
    // library artifacts cannot pause a producer through this hook.
    pub(super) static BEFORE_ENQUEUE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temporary_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "sakura-input-history-{}-{name}-{}.bin",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn key_record(sequence: u64, timestamp_ms: u64) -> InputHistoryRecord {
    InputHistoryRecord::Key(KeyHistoryRecord {
        sequence,
        timestamp_ms,
        session: 1,
        scope: ScopeClass::Normal,
        key_code: 1,
        character: Some('x'),
        modifiers: 0,
        repeat: false,
        consumed: true,
        state_before: 0,
        state_after: 1,
        mode_before: 1,
        mode_after: 1,
        preedit_before: String::new(),
        preedit_after: "x".to_owned(),
        commit: String::new(),
        delete_before: 0,
        beep: false,
        action: "char".to_owned(),
        dropped_before: 0,
    })
}

fn append_records(path: &Path, records: &[InputHistoryRecord]) {
    ensure_file(path).expect("ensure history file");
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open history file");
    for record in records {
        let protected = protect(&record.encode().expect("encode record")).expect("protect");
        append_encrypted(&mut file, &protected).expect("append record");
    }
}

struct ReadFailureFixture(PathBuf);

impl Drop for ReadFailureFixture {
    fn drop(&mut self) {
        for path in [&self.0, &self.0.with_extension("compact.tmp")] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove synthetic history fixture: {error}"),
            }
        }
    }
}

fn assert_complete_frame_failure_preserves_store(encrypted: &[u8]) {
    let fixture = ReadFailureFixture(temporary_path("read-failure"));
    let path = &fixture.0;
    append_records(path, &[key_record(1, now_ms())]);
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    append_encrypted(&mut file, encrypted).unwrap();
    drop(file);
    append_records(path, &[key_record(3, now_ms())]);
    let original = fs::read(path).unwrap();

    // Repair must not transform a complete opaque frame into a torn tail.
    let repair = repair_file(path);
    assert_eq!(
        fs::read(path).unwrap(),
        original,
        "repair erased opaque data"
    );
    assert!(repair.is_err(), "repair must report unavailable content");
    assert!(
        read_snapshot(path).is_err(),
        "snapshot must not claim completeness"
    );
    assert!(
        compact_file(path).is_err(),
        "compaction must not omit opaque data"
    );
    assert_eq!(fs::read(path).unwrap(), original);
    let service = InputHistoryService::open(path);
    if let Ok(service) = &service {
        service.stop().unwrap();
    }
    assert!(
        service.is_err(),
        "startup must fail before spawning the writer"
    );
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn complete_frame_decryption_failure_preserves_store() {
    let ciphertext = b"synthetic invalid DPAPI blob";
    assert!(unprotect(ciphertext).is_err());
    assert_complete_frame_failure_preserves_store(ciphertext);
}

#[test]
fn complete_frame_unknown_record_preserves_store() {
    // Valid DPAPI and frame checksum; the record type is unsupported.
    // Include all common fields so decoding reaches the unknown-kind arm,
    // rather than failing earlier while reading a truncated header.
    let mut payload = vec![0; 1 + 8 + 8 + 8 + 1];
    payload[0] = 255;
    payload[25] = ScopeClass::Normal as u8;
    assert_eq!(
        InputHistoryRecord::decode(&payload)
            .unwrap_err()
            .to_string(),
        "unknown input history record"
    );
    assert_complete_frame_failure_preserves_store(&protect(&payload).unwrap());
}

#[test]
fn complete_frame_malformed_record_preserves_store() {
    assert_complete_frame_failure_preserves_store(&protect(&[RECORD_KEY]).unwrap());
}

#[test]
fn future_history_format_preserves_store() {
    let fixture = ReadFailureFixture(temporary_path("future-format"));
    append_records(&fixture.0, &[key_record(1, now_ms())]);
    let mut original = fs::read(&fixture.0).unwrap();
    original[4..6].copy_from_slice(&(INPUT_HISTORY_FORMAT_VERSION + 1).to_le_bytes());
    fs::write(&fixture.0, &original).unwrap();
    assert!(repair_file(&fixture.0).is_err());
    assert!(compact_file(&fixture.0).is_err());
    assert!(InputHistoryService::open(&fixture.0).is_err());
    assert_eq!(fs::read(&fixture.0).unwrap(), original);
}

#[test]
fn structural_tail_damage_still_repairs_to_verified_prefix() {
    for tail in [vec![1, 2, 3], vec![1, 0, 0, 0, 0, 0, 0, 0, 42]] {
        let fixture = ReadFailureFixture(temporary_path("structural-tail"));
        append_records(&fixture.0, &[key_record(1, now_ms())]);
        let original = fs::read(&fixture.0).unwrap();
        let mut file = OpenOptions::new().append(true).open(&fixture.0).unwrap();
        file.write_all(&tail).unwrap();
        drop(file);
        repair_file(&fixture.0).unwrap();
        assert_eq!(fs::read(&fixture.0).unwrap(), original);
        assert_eq!(read_snapshot(&fixture.0).unwrap().records.len(), 1);
    }
}

fn assert_control_barrier_preserves_bytes(flush_first: bool) {
    let fixture = ReadFailureFixture(temporary_path("control-barrier"));
    let now = now_ms();
    let old = now.saturating_sub(RETENTION.as_millis() as u64 + 1);
    append_records(&fixture.0, &[key_record(1, old), key_record(2, now)]);
    let original = fs::read(&fixture.0).unwrap();
    let (sender, receiver) = mpsc::sync_channel(2);
    let (flush_reply, flush_result) = mpsc::channel();
    if flush_first {
        sender.send(Command::Flush { reply: flush_reply }).unwrap();
    }
    let (reply, stopped) = mpsc::channel();
    sender.send(Command::Shutdown { reply }).unwrap();
    // Prequeued controls execute deterministically, without a sleep or
    // spawned thread whose ownership could be lost on assertion failure.
    writer_loop_with_interval(
        fixture.0.clone(),
        receiver,
        Arc::new(InputHistoryStats::default()),
        Duration::from_secs(3600),
    );
    if flush_first {
        flush_result.try_recv().unwrap().unwrap();
    }
    stopped.try_recv().unwrap().unwrap();
    assert!(
        fs::read(&fixture.0).unwrap() == original,
        "a control barrier must not rewrite or expire existing frames"
    );
}

#[test]
fn flush_barrier_preserves_existing_ciphertext() {
    assert_control_barrier_preserves_bytes(true);
}

#[test]
fn shutdown_barrier_preserves_existing_ciphertext() {
    assert_control_barrier_preserves_bytes(false);
}

#[test]
fn startup_preserves_ciphertext_and_recovers_ids_before_retention() {
    let fixture = ReadFailureFixture(temporary_path("startup-ids"));
    let now = now_ms();
    let mut expired = key_record(900, now.saturating_sub(RETENTION.as_millis() as u64 + 1));
    if let InputHistoryRecord::Key(record) = &mut expired {
        record.session = 700;
    }
    append_records(&fixture.0, &[expired, key_record(3, now)]);
    let original = fs::read(&fixture.0).unwrap();
    let service = InputHistoryService::open(&fixture.0).unwrap();
    let session = service.allocate_session_id().unwrap();
    service.stop().unwrap();
    let after = fs::read(&fixture.0).unwrap();
    let snapshot = read_snapshot(&fixture.0).unwrap();
    let sequence = snapshot.records.last().unwrap().sequence();
    let prefix_preserved = after.starts_with(&original);
    assert!(prefix_preserved && session == 701 && sequence == 901,
            "startup prefix={prefix_preserved}, session={session} (expected 701), sequence={sequence} (expected 901)");
}

thread_local! {
    pub(super) static FAIL_AFTER_APPEND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static AFTER_MAINTENANCE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn after_maintenance_check() {
    AFTER_MAINTENANCE.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

#[test]
fn maintenance_expires_record_after_uncertain_append_failure() {
    let fixture = PublicationFixture(ReadFailureFixture(temporary_path("uncertain-expiry")));
    let path = fixture.0 .0.clone();
    ensure_file(&path).unwrap();
    let file = open_append(&path).unwrap();
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
    let (checked, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    sender
        .send(Command::Append {
            epoch: 0,
            timestamp_ms: 1,
            payload: key_record(1, 1).encode().unwrap(),
        })
        .unwrap();
    let worker = thread::spawn(move || {
        FAIL_AFTER_APPEND.with(|fail| fail.set(true));
        AFTER_MAINTENANCE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                checked.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
            }));
        });
        writer_loop_with_file(
            path,
            receiver,
            Arc::new(InputHistoryStats::default()),
            Some(file),
            Duration::from_millis(1),
            RetentionPlan::default(),
        );
    });
    let checked = observed.recv_timeout(Duration::from_secs(10));
    let snapshot = read_snapshot(&fixture.0 .0);
    let (reply, stopped) = mpsc::channel();
    let sent = sender.send(Command::Shutdown { reply });
    let _ = release.send(());
    let joined = worker.join();
    assert!(checked.is_ok() && sent.is_ok() && joined.is_ok());
    assert!(
        stopped.recv().unwrap().is_err(),
        "append loss remains observable"
    );
    assert!(
        snapshot.unwrap().records.is_empty(),
        "uncertain expired append must be removed"
    );
}

#[test]
fn maintenance_prewrite_rejection_does_not_schedule_expiry() {
    let fixture = ReadFailureFixture(temporary_path("rejected-expiry"));
    let now = now_ms();
    append_records(&fixture.0, &[key_record(1, now)]);
    let mut file = Some(open_append(&fixture.0).unwrap());
    let mut retention = RetentionPlan::default();
    retention.observe(now);
    assert!(append_payload(
        &fixture.0,
        &mut file,
        &vec![0; MAX_RECORD_BYTES + 1],
        1,
        &mut retention
    )
    .is_err());
    assert_eq!(retention.oldest_timestamp_ms, Some(now));
    assert!(!retention.is_due(now));
}

#[test]
fn maintenance_unexpired_idle_store_preserves_ciphertext() {
    let fixture = ReadFailureFixture(temporary_path("clean-idle"));
    append_records(&fixture.0, &[key_record(1, now_ms())]);
    let original = fs::read(&fixture.0).unwrap();
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
    let (checked, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let path = fixture.0.clone();
    let worker = thread::spawn(move || {
        AFTER_MAINTENANCE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                checked.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
            }))
        });
        writer_loop_with_interval(
            path,
            receiver,
            Arc::new(InputHistoryStats::default()),
            Duration::from_millis(1),
        );
    });
    let checked = observed.recv_timeout(Duration::from_secs(10));
    let unchanged = fs::read(&fixture.0).unwrap() == original;
    let (reply, stopped) = mpsc::channel();
    let sent = sender.send(Command::Shutdown { reply });
    let _ = release.send(());
    let joined = worker.join();
    assert!(checked.is_ok());
    assert!(sent.is_ok());
    assert!(joined.is_ok());
    stopped.recv().unwrap().unwrap();
    assert!(
        unchanged,
        "idle unexpired store was re-encrypted and rewritten"
    );
}

#[test]
fn maintenance_plan_has_inclusive_retention_and_observes_older_appends() {
    let now = RETENTION.as_millis() as u64 + 10;
    let mut plan = RetentionPlan::default();
    assert!(!plan.is_due(now));
    plan.observe(10);
    assert!(!plan.is_due(now), "record on inclusive cutoff is retained");
    assert!(plan.is_due(now + 1));
    plan.observe(5);
    assert!(plan.is_due(now));
    assert!(!plan.is_due(0), "clock rollback cannot underflow cutoff");
    plan = RetentionPlan::default();
    assert!(
        !plan.is_due(u64::MAX),
        "successful empty Clear leaves no expiry work"
    );
    plan.observe(u64::MAX);
    assert!(!plan.is_due(u64::MAX));
}

#[test]
fn maintenance_plan_is_recovered_in_scan_and_refreshed_by_compaction() {
    let fixture = ReadFailureFixture(temporary_path("retention-hint"));
    let now = now_ms();
    append_records(&fixture.0, &[key_record(2, now), key_record(1, 1)]);
    let recovered = repair_file(&fixture.0).unwrap();
    assert_eq!(recovered.retention.oldest_timestamp_ms, Some(1));
    let retained = compact_file(&fixture.0).unwrap();
    assert_eq!(retained.oldest_timestamp_ms, Some(now));
    assert!(!retained.is_due(now));
    clear_path(&fixture.0).unwrap();
    let cleared = compact_file(&fixture.0).unwrap();
    assert_eq!(cleared.oldest_timestamp_ms, None);
}

fn shutdown_fixture(
    action: impl FnOnce(Receiver<Command>) + Send + 'static,
) -> InputHistoryService {
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
    InputHistoryService {
        path: temporary_path("shutdown-only"),
        sender,
        stats: Arc::new(InputHistoryStats::default()),
        next_sequence: AtomicU64::new(0),
        next_session_id: AtomicU64::new(0),
        epoch: AtomicU64::new(0),
        worker: Mutex::new(WriterShutdown {
            handle: Some(thread::spawn(move || action(receiver))),
            outcome: None,
        }),
    }
}

#[test]
fn stop_outcome_keeps_durable_failure_for_later_callers() {
    let service = shutdown_fixture(|receiver| {
        let Command::Shutdown { reply } = receiver.recv().unwrap() else {
            panic!("shutdown expected")
        };
        reply
            .send(Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "synthetic sync failure",
            )))
            .unwrap();
    });
    let first = service.stop();
    let second = service.stop();
    assert_eq!(first.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(second.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn stop_outcome_joins_worker_after_reply_disconnect() {
    let service = shutdown_fixture(|receiver| {
        let Command::Shutdown { reply } = receiver.recv().unwrap() else {
            panic!("shutdown expected")
        };
        drop(reply);
    });
    let result = service.stop();
    let joined = service.worker.lock().unwrap().handle.is_none();
    // Clean up the old implementation before asserting its counterexample.
    let _ = service.stop();
    assert!(result.is_err());
    assert!(
        joined,
        "lost reply returned before worker join was owned/completed"
    );
}

#[test]
fn stop_outcome_rejects_success_reply_followed_by_worker_panic() {
    let service = shutdown_fixture(|receiver| {
        let Command::Shutdown { reply } = receiver.recv().unwrap() else {
            panic!("shutdown expected")
        };
        reply.send(Ok(())).unwrap();
        panic!("synthetic worker exit failure");
    });
    assert!(
        service.stop().is_err(),
        "worker panic was reported as a successful stop"
    );
    assert!(
        service.stop().is_err(),
        "later stop lost the worker failure"
    );
}

#[test]
fn stop_outcome_concurrent_callers_share_one_shutdown_and_failure() {
    let (entered, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let service = Arc::new(shutdown_fixture(move |receiver| {
        let Command::Shutdown { reply } = receiver.recv().unwrap() else {
            panic!("shutdown expected")
        };
        entered.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(10)).unwrap();
        reply.send(Err(io::Error::from_raw_os_error(5))).unwrap();
        assert!(receiver.try_recv().is_err(), "duplicate Shutdown enqueued");
    }));
    let start = Arc::new(std::sync::Barrier::new(9));
    let callers: Vec<_> = (0..8)
        .map(|_| {
            let service = Arc::clone(&service);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                service.stop().unwrap_err().raw_os_error()
            })
        })
        .collect();
    start.wait();
    let ready = observed.recv_timeout(Duration::from_secs(10));
    let _ = release.send(());
    let results: Vec<_> = callers.into_iter().map(|caller| caller.join()).collect();
    assert!(ready.is_ok());
    for result in results {
        assert_eq!(result.unwrap(), Some(5));
    }
    assert!(service.worker.lock().unwrap().handle.is_none());
}

thread_local! {
    static LOCK_REPLACEMENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(super) static PARTIAL_REPLACE_ERROR: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
    static PUBLICATION_CUT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

pub(super) fn publication_cut(point: &'static str) -> io::Result<()> {
    PUBLICATION_CUT.with(|cut| {
        if cut.get() == Some(point) {
            cut.set(None);
            Err(io::Error::from_raw_os_error(112))
        } else {
            Ok(())
        }
    })
}

pub(super) fn lock_replacement_if_requested(path: &Path) -> Option<File> {
    LOCK_REPLACEMENT.with(|requested| {
        requested.replace(false).then(|| {
            OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(path)
                .unwrap()
        })
    })
}

struct PublicationFixture(ReadFailureFixture);

impl PublicationFixture {
    fn new(label: &str) -> Self {
        Self(ReadFailureFixture(temporary_path(label)))
    }
    fn path(&self) -> &Path {
        &self.0 .0
    }
    fn transaction(&self) -> PathBuf {
        let mut name = self.path().file_name().unwrap().to_os_string();
        name.push(".compaction");
        self.path().with_file_name(name)
    }
}

impl Drop for PublicationFixture {
    fn drop(&mut self) {
        let directory = self.transaction();
        for name in ["replacement.bin", "previous.bin"] {
            match fs::remove_file(directory.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove synthetic transaction file: {error}"),
            }
        }
        match fs::remove_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove synthetic transaction directory: {error}"),
        }
    }
}

#[test]
fn publication_partial_windows_outcomes_preserve_candidates_and_refuse_reopen() {
    for code in [1175, 1176, 1177, 5] {
        let fixture = PublicationFixture::new(&format!("partial-{code}"));
        append_records(fixture.path(), &[key_record(1, now_ms())]);
        let original = fs::read(fixture.path()).unwrap();
        PARTIAL_REPLACE_ERROR.with(|failure| failure.set(Some(code)));
        assert_eq!(
            compact_file(fixture.path()).unwrap_err().raw_os_error(),
            Some(code)
        );
        assert!(fixture.transaction().join("replacement.bin").exists());
        if code == 1177 {
            assert!(!fixture.path().exists());
            assert!(fs::read(fixture.transaction().join("previous.bin")).unwrap() == original);
        } else {
            assert!(fs::read(fixture.path()).unwrap() == original);
        }
        assert!(open_append(fixture.path()).is_err());
        assert!(InputHistoryService::open(fixture.path()).is_err());
        assert!(clear_path(fixture.path()).is_err());
        assert_eq!(fixture.path().exists(), code != 1177);
    }
}

#[test]
fn publication_injected_stage_errors_remain_unavailable_without_cleanup() {
    for point in [
        "temp_written",
        "temp_synced",
        "replaced",
        "canonical_synced",
        "backup_removed",
    ] {
        let fixture = PublicationFixture::new(point);
        append_records(fixture.path(), &[key_record(1, now_ms())]);
        PUBLICATION_CUT.with(|cut| cut.set(Some(point)));
        let mut writer = Some(open_append(fixture.path()).unwrap());
        assert!(compact_writer_file(fixture.path(), &mut writer).is_err());
        assert!(
            writer.is_none(),
            "failed transaction reopened an append handle"
        );
        assert!(fixture.transaction().exists());
        assert!(fixture.path().exists());
        validate_compaction_file(fixture.path()).unwrap();
        if point == "backup_removed" {
            assert!(!fixture.transaction().join("previous.bin").exists());
            let bytes = fs::read(fixture.path()).unwrap();
            assert_eq!(scan_snapshot(&bytes).unwrap().records.len(), 1);
        }
        assert!(read_snapshot(fixture.path()).is_err());
        assert!(clear_path(fixture.path()).is_err());
    }
}

#[test]
fn publication_success_removes_transaction_for_empty_and_retained_stores() {
    for retained in [false, true] {
        let fixture = PublicationFixture::new("publish-success");
        let timestamp = if retained { now_ms() } else { 1 };
        append_records(fixture.path(), &[key_record(1, timestamp)]);
        compact_file(fixture.path()).unwrap();
        assert!(!fixture.transaction().exists());
        assert_eq!(
            read_snapshot(fixture.path()).unwrap().records.len(),
            usize::from(retained)
        );
        clear_path(fixture.path()).unwrap();
        assert!(read_snapshot(fixture.path()).unwrap().records.is_empty());
    }
}

#[test]
fn publication_real_sharing_failure_preserves_canonical() {
    let fixture = PublicationFixture::new("publication-sharing");
    append_records(fixture.path(), &[key_record(1, now_ms())]);
    let original = fs::read(fixture.path()).unwrap();
    LOCK_REPLACEMENT.with(|requested| requested.set(true));
    let result = compact_file(fixture.path());
    assert!(
        result.is_err(),
        "real replacement sharing conflict must fail"
    );
    assert!(
        fs::read(fixture.path()).ok().as_deref() == Some(original.as_slice()),
        "failed publication lost the original canonical"
    );
}

#[test]
fn publication_pending_transaction_prevents_empty_recreation() {
    let fixture = PublicationFixture::new("pending-missing");
    fs::create_dir(fixture.transaction()).unwrap();
    fs::write(fixture.transaction().join("replacement.bin"), header()).unwrap();
    let opened = InputHistoryService::open(fixture.path());
    let rejected = opened.is_err();
    if let Ok(service) = opened {
        service.stop().unwrap();
    }
    assert!(
        rejected,
        "unresolved publication was treated as a new store"
    );
    assert!(!fixture.path().exists(), "empty canonical was manufactured");
}

#[test]
fn publication_pending_transaction_prevents_false_clear_success() {
    let fixture = PublicationFixture::new("pending-clear");
    append_records(fixture.path(), &[key_record(1, now_ms())]);
    let original = fs::read(fixture.path()).unwrap();
    fs::create_dir(fixture.transaction()).unwrap();
    fs::write(fixture.transaction().join("previous.bin"), &original).unwrap();
    let cleared = clear_path(fixture.path());
    assert!(
        cleared.is_err(),
        "Clear succeeded while recoverable history remained"
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
}

#[test]
fn publication_preserves_preexisting_legacy_temp() {
    let fixture = PublicationFixture::new("legacy-temp-collision");
    append_records(fixture.path(), &[key_record(1, now_ms())]);
    let temp = fixture.path().with_extension("compact.tmp");
    fs::write(&temp, b"synthetic unrelated legacy temp").unwrap();
    let result = compact_file(fixture.path());
    assert!(result.is_ok());
    assert_eq!(
        fs::read(&temp).ok().as_deref(),
        Some(b"synthetic unrelated legacy temp".as_slice())
    );
}

#[test]
fn store_owner_rejects_second_service_without_mutation() {
    let fixture = ReadFailureFixture(temporary_path("two-writers"));
    let owner = InputHistoryService::open(&fixture.0).unwrap();
    owner.flush().unwrap();
    let original = fs::read(&fixture.0).unwrap();
    let second = InputHistoryService::open(&fixture.0);
    let rejected = second.is_err();
    if let Ok(second) = second {
        second.stop().unwrap();
    }
    owner.stop().unwrap();
    assert!(rejected, "second history writer was admitted");
    assert_eq!(fs::read(&fixture.0).unwrap(), original);
    // A stopped service may still have callers holding its Arc. Only
    // the worker's actual lifetime owns the store, so handoff now works.
    let successor = InputHistoryService::open(&fixture.0).unwrap();
    successor.stop().unwrap();
}

#[test]
fn store_owner_excludes_offline_clear() {
    let fixture = ReadFailureFixture(temporary_path("offline-clear-owner"));
    let owner = InputHistoryService::open(&fixture.0).unwrap();
    owner.flush().unwrap();
    let original = fs::read(&fixture.0).unwrap();
    let cleared = clear_path(&fixture.0);
    owner.stop().unwrap();
    assert!(cleared.is_err(), "offline Clear bypassed the live writer");
    assert_eq!(fs::read(&fixture.0).unwrap(), original);
    clear_path(&fixture.0).unwrap();
    assert!(read_snapshot(&fixture.0).unwrap().records.is_empty());
}

#[test]
fn store_owner_survives_canonical_replacement_and_paths_are_independent() {
    let fixture = ReadFailureFixture(temporary_path("replace-owner"));
    let previous = ReadFailureFixture(temporary_path("previous-owner"));
    let owner = InputHistoryService::open(&fixture.0).unwrap();
    owner.flush().unwrap();
    let original = fs::read(&fixture.0).unwrap();
    fs::rename(&fixture.0, &previous.0).unwrap();
    fs::write(&fixture.0, original).unwrap();
    let alias = fixture
        .0
        .parent()
        .unwrap()
        .join(".")
        .join(fixture.0.file_name().unwrap());
    assert!(InputHistoryService::open(&alias).is_err());
    let independent = ReadFailureFixture(temporary_path("independent-owner"));
    let other = InputHistoryService::open(&independent.0).unwrap();
    other.stop().unwrap();
    owner.stop().unwrap();
    let successor = InputHistoryService::open(&fixture.0).unwrap();
    successor.stop().unwrap();
}

#[test]
fn store_owner_does_not_delete_a_preexisting_lock_path() {
    let fixture = ReadFailureFixture(temporary_path("lock-collision"));
    append_records(&fixture.0, &[key_record(1, now_ms())]);
    let original = fs::read(&fixture.0).unwrap();
    let mut name = fixture.0.file_name().unwrap().to_os_string();
    name.push(".writer.lock");
    let lock_path = fixture.0.with_file_name(name);
    fs::write(&lock_path, b"synthetic preexisting file").unwrap();
    let opened = InputHistoryService::open(&fixture.0);
    let rejected = opened.is_err();
    if let Ok(service) = opened {
        service.stop().unwrap();
    }
    let lock_contents = fs::read(&lock_path).ok();
    let _ = fs::remove_file(&lock_path);
    assert!(
        rejected,
        "existing lock path must not be adopted for deletion"
    );
    assert_eq!(
        lock_contents.as_deref(),
        Some(b"synthetic preexisting file".as_slice())
    );
    assert_eq!(fs::read(&fixture.0).unwrap(), original);
}

#[test]
fn store_owner_preserves_preexisting_symlink_and_target() {
    let fixture = ReadFailureFixture(temporary_path("lock-symlink"));
    let target = ReadFailureFixture(temporary_path("lock-target"));
    fs::write(&target.0, b"synthetic target").unwrap();
    let mut name = fixture.0.file_name().unwrap().to_os_string();
    name.push(".writer.lock");
    let lock_path = fixture.0.with_file_name(name);
    std::os::windows::fs::symlink_file(&target.0, &lock_path).unwrap();
    let opened = InputHistoryService::open(&fixture.0);
    let rejected = opened.is_err();
    if let Ok(service) = opened {
        service.stop().unwrap();
    }
    let link_preserved = fs::symlink_metadata(&lock_path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false);
    let target_contents = fs::read(&target.0).ok();
    let _ = fs::remove_file(&lock_path);
    assert!(rejected);
    assert!(link_preserved);
    assert_eq!(
        target_contents.as_deref(),
        Some(b"synthetic target".as_slice())
    );
    assert!(!fixture.0.exists());
}

#[test]
fn clear_rejects_content_prepared_before_its_epoch() {
    let mut resurrected = Vec::new();
    for kind in 0..3 {
        let fixture = ReadFailureFixture(temporary_path("paused-clear"));
        let service = InputHistoryService::open(&fixture.0).unwrap();
        let producer_service = Arc::clone(&service);
        let (paused, pause_observed) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let producer = thread::spawn(move || {
            BEFORE_ENQUEUE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    paused.send(()).unwrap();
                    resumed.recv_timeout(Duration::from_secs(10)).unwrap();
                }));
            });
            match kind {
                0 => producer_service.record_key(
                    1,
                    ScopeClass::Normal,
                    1,
                    Some('x'),
                    0,
                    false,
                    false,
                    true,
                    0,
                    1,
                    1,
                    1,
                    "",
                    "x",
                    "",
                    0,
                    false,
                    "char",
                ),
                1 => producer_service.record_commit(1, ScopeClass::Normal, "x", "X", 0, 0),
                _ => producer_service.record_ai_text(
                    1,
                    ScopeClass::Normal,
                    AiTextOperation::Transform,
                    AiTextStatus::Applied,
                    "x",
                    "X",
                    "synthetic",
                    "synthetic",
                    "",
                    "",
                    1,
                    1,
                    1,
                    0,
                    1,
                    false,
                ),
            }
        });
        let paused_ok = pause_observed.recv_timeout(Duration::from_secs(10)).is_ok();
        let first_clear = service.clear();
        let second_clear = service.clear();
        // Always release/join the owned producer before assertions.
        let _ = resume.send(());
        let producer_result = producer.join();
        service.record_commit(2, ScopeClass::Normal, "new", "NEW", 0, 0);
        let flushed = service.flush();
        let stopped = service.stop();
        assert!(paused_ok);
        first_clear.unwrap();
        second_clear.unwrap();
        producer_result.unwrap();
        flushed.unwrap();
        stopped.unwrap();
        let snapshot = read_snapshot(&fixture.0).unwrap();
        assert!(snapshot.records.iter().any(|record| record.session() == 2));
        if snapshot.records.iter().any(|record| record.session() == 1) {
            resurrected.push(kind);
        }
    }
    assert!(
        resurrected.is_empty(),
        "old content survived Clear for variants {resurrected:?}"
    );
}

#[test]
fn startup_rejects_exhausted_stored_ids_without_mutating_history() {
    for session_exhausted in [false, true] {
        let fixture = ReadFailureFixture(temporary_path("exhausted-ids"));
        let mut record = key_record(if session_exhausted { 1 } else { u64::MAX }, now_ms());
        if session_exhausted {
            if let InputHistoryRecord::Key(record) = &mut record {
                record.session = u64::MAX;
            }
        }
        append_records(&fixture.0, &[record]);
        let original = fs::read(&fixture.0).unwrap();
        assert_eq!(
            InputHistoryService::open(&fixture.0).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&fixture.0).unwrap(), original);
    }
}

#[test]
fn legacy_startup_migrates_before_appending_current_markers() {
    let fixture = ReadFailureFixture(temporary_path("legacy-startup"));
    append_records(&fixture.0, &[key_record(17, now_ms())]);
    let mut bytes = fs::read(&fixture.0).unwrap();
    bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
    fs::write(&fixture.0, bytes).unwrap();
    let service = InputHistoryService::open(&fixture.0).unwrap();
    service.stop().unwrap();
    let snapshot = read_snapshot(&fixture.0).unwrap();
    assert_eq!(snapshot.format_version, INPUT_HISTORY_FORMAT_VERSION);
    assert_eq!(snapshot.records[0].sequence(), 17);
    assert_eq!(snapshot.records.last().unwrap().sequence(), 18);
}

#[test]
fn viewing_retention_has_inclusive_cutoff_and_keeps_order() {
    let now = RETENTION.as_millis() as u64 + 100;
    let mut snapshot = InputHistorySnapshot {
        format_version: INPUT_HISTORY_FORMAT_VERSION,
        records: vec![key_record(3, 101), key_record(1, 99), key_record(2, 100)],
        ignored_tail_bytes: 7,
    };
    snapshot.retain_current_records(now);
    assert_eq!(
        snapshot.records,
        vec![key_record(3, 101), key_record(2, 100)]
    );
    assert_eq!(snapshot.ignored_tail_bytes, 7);
    let before = snapshot.clone();
    snapshot.retain_current_records(0);
    assert_eq!(
        snapshot, before,
        "early clock must not underflow the cutoff"
    );
}

#[test]
fn barriers_report_missing_handle_and_real_sync_failure() {
    assert_eq!(
        sync_writer_file(&None, false).unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
    let fixture = ReadFailureFixture(temporary_path("sync-denied"));
    ensure_file(&fixture.0).unwrap();
    // Windows FlushFileBuffers requires write access. Exercise the real
    // failure, not an injected helper that simply returns its expectation.
    let read_only = File::open(&fixture.0).unwrap();
    assert!(sync_writer_file(&Some(read_only), false).is_err());
}

#[test]
fn barriers_keep_failed_append_observable_until_explicit_clear() {
    let fixture = ReadFailureFixture(temporary_path("barrier-loss"));
    ensure_file(&fixture.0).unwrap();
    let (sender, receiver) = mpsc::sync_channel(6);
    sender
        .send(Command::Append {
            epoch: 0,
            timestamp_ms: now_ms(),
            payload: vec![0; MAX_RECORD_BYTES + 1],
        })
        .unwrap();
    let (first, first_result) = mpsc::channel();
    sender.send(Command::Flush { reply: first }).unwrap();
    let (retry, retry_result) = mpsc::channel();
    sender.send(Command::Flush { reply: retry }).unwrap();
    let (clear, clear_result) = mpsc::channel();
    sender
        .send(Command::Clear {
            epoch: 1,
            reply: clear,
        })
        .unwrap();
    let (after, after_result) = mpsc::channel();
    sender.send(Command::Flush { reply: after }).unwrap();
    let (stop, stop_result) = mpsc::channel();
    sender.send(Command::Shutdown { reply: stop }).unwrap();
    writer_loop_with_interval(
        fixture.0.clone(),
        receiver,
        Arc::new(InputHistoryStats::default()),
        Duration::from_secs(3600),
    );
    assert!(first_result.try_recv().unwrap().is_err());
    assert!(retry_result.try_recv().unwrap().is_err());
    clear_result.try_recv().unwrap().unwrap();
    after_result.try_recv().unwrap().unwrap();
    stop_result.try_recv().unwrap().unwrap();
}

#[test]
fn control_barrier_size_matrix() {
    // Repeated synthetic frames isolate storage-size cost, not ID or
    // language quality. Build outside the timed region, sync first, and
    // measure the same Flush+Shutdown pair at all three sizes.
    let timestamp = now_ms();
    let protected = protect(&key_record(1, timestamp).encode().unwrap()).unwrap();
    let mut frame = Vec::new();
    frame.extend_from_slice(&(protected.len() as u32).to_le_bytes());
    frame.extend_from_slice(&crc32(&protected).to_le_bytes());
    frame.extend_from_slice(&protected);
    let block = frame.repeat(128);
    for mib in [0u64, 16, 64] {
        let fixture = ReadFailureFixture(temporary_path("barrier-size"));
        ensure_file(&fixture.0).unwrap();
        let mut file = OpenOptions::new().append(true).open(&fixture.0).unwrap();
        let mut remaining = (mib * 1024 * 1024).saturating_sub(HEADER_LEN as u64);
        while remaining >= block.len() as u64 {
            file.write_all(&block).unwrap();
            remaining -= block.len() as u64;
        }
        while remaining >= frame.len() as u64 {
            file.write_all(&frame).unwrap();
            remaining -= frame.len() as u64;
        }
        file.sync_all().unwrap();
        let bytes = file.metadata().unwrap().len();
        drop(file);
        let (sender, receiver) = mpsc::sync_channel(2);
        let (flushed, flush_result) = mpsc::channel();
        let (stopped, stop_result) = mpsc::channel();
        sender.send(Command::Flush { reply: flushed }).unwrap();
        sender.send(Command::Shutdown { reply: stopped }).unwrap();
        // Actor/control benchmark: fixture encoding already establishes
        // this hint; do not time the test helper's startup scan.
        let append = open_append(&fixture.0).unwrap();
        let retention = RetentionPlan {
            oldest_timestamp_ms: (mib != 0).then_some(timestamp),
        };
        let started = Instant::now();
        writer_loop_with_file(
            fixture.0.clone(),
            receiver,
            Arc::new(InputHistoryStats::default()),
            Some(append),
            Duration::from_secs(3600),
            retention,
        );
        let elapsed = started.elapsed();
        flush_result.try_recv().unwrap().unwrap();
        stop_result.try_recv().unwrap().unwrap();
        assert_eq!(fs::metadata(&fixture.0).unwrap().len(), bytes);
        println!(
            "history-control-size target_mib={mib} bytes={bytes} flush_shutdown_us={}",
            elapsed.as_micros()
        );
    }
}

#[test]
fn key_and_commit_records_roundtrip_through_dpapi() {
    let path = temporary_path("roundtrip");
    let service = InputHistoryService::open(&path).expect("open");
    service.record_key(
        7,
        ScopeClass::Normal,
        1,
        Some('\t'),
        0,
        false,
        false,
        true,
        0,
        1,
        1,
        1,
        "",
        "か",
        "",
        0,
        false,
        "char",
    );
    service.record_commit(7, ScopeClass::Normal, "かな", "仮名", 3, 4);
    service.flush().expect("flush");
    let snapshot = read_snapshot(&path).expect("snapshot");
    assert_eq!(snapshot.records.len(), 3);
    assert!(matches!(snapshot.records[0], InputHistoryRecord::Engine(_)));
    assert!(matches!(snapshot.records[1], InputHistoryRecord::Key(_)));
    assert!(matches!(snapshot.records[2], InputHistoryRecord::Commit(_)));
    let InputHistoryRecord::Engine(engine) = &snapshot.records[0] else {
        panic!("expected engine record");
    };
    assert_eq!(engine.package_version, ENGINE_PACKAGE_VERSION);
    assert!(!engine.release_label.is_empty());
    let InputHistoryRecord::Key(key) = &snapshot.records[1] else {
        panic!("expected key record");
    };
    assert_eq!(key.sequence, 2);
    assert_eq!(key.session, 7);
    assert_eq!(key.scope, ScopeClass::Normal);
    assert_eq!(key.key_code, 1);
    assert_eq!(key.character, Some('\t'));
    assert!(key.consumed);
    assert_eq!(key.state_before, 0);
    assert_eq!(key.state_after, 1);
    assert_eq!(key.mode_before, 1);
    assert_eq!(key.mode_after, 1);
    assert!(key.preedit_before.is_empty());
    assert!(!key.preedit_after.is_empty());
    assert!(key.commit.is_empty());
    assert_eq!(key.action, "char");
    let InputHistoryRecord::Commit(commit) = &snapshot.records[2] else {
        panic!("expected commit record");
    };
    assert_eq!(commit.sequence, 3);
    assert_eq!(commit.session, 7);
    assert_eq!(commit.scope, ScopeClass::Normal);
    assert_eq!(commit.left_context, 3);
    assert_eq!(commit.right_context, 4);
    assert!(!commit.reading.is_empty());
    assert!(!commit.surface.is_empty());
    let tsv = snapshot.to_tsv();
    assert!(tsv.contains(&format!("# package-version: {ENGINE_PACKAGE_VERSION}")));
    assert!(tsv.contains("# release-label:"));
    assert!(tsv.contains("engine-package-version\tengine-release-label"));
    let tsv_lines: Vec<_> = tsv.lines().collect();
    // 4 comment lines + header + engine + key + commit
    assert_eq!(tsv_lines.len(), 8);
    assert_eq!(tsv_lines[4].split('\t').count(), 40);
    assert!(tsv_lines[6].contains("\t\\t\t"));
    for line in &tsv_lines[5..] {
        assert_eq!(line.split('\t').count(), 40);
    }
    service.stop().expect("stop");
    let _ = fs::remove_file(path);
}

#[test]
fn ai_records_roundtrip_and_aggregate_logical_requests_attempts_and_tokens() {
    let path = temporary_path("ai-roundtrip");
    let service = InputHistoryService::open(&path).expect("open");
    service.record_ai_text(
        9,
        ScopeClass::Normal,
        AiTextOperation::Proofread,
        AiTextStatus::Applied,
        "元",
        "結果",
        "gpt-5.6-luna",
        "openai",
        "technical",
        "",
        123,
        17,
        5,
        3,
        2,
        false,
    );
    let stats = service.stats().snapshot();
    assert_eq!(stats.ai_requests, 1);
    assert_eq!(stats.ai_attempts, 2);
    assert_eq!(stats.ai_input_tokens, 17);
    assert_eq!(stats.ai_output_tokens, 5);
    assert_eq!(stats.ai_cached_tokens, 3);
    service.flush().expect("flush");
    let snapshot = read_snapshot(&path).expect("read");
    let ai = snapshot
        .records
        .iter()
        .find_map(|record| match record {
            InputHistoryRecord::AiText(record) => Some(record),
            _ => None,
        })
        .expect("AI record");
    assert_eq!(ai.operation, AiTextOperation::Proofread);
    assert_eq!(ai.status, AiTextStatus::Applied);
    assert_eq!(ai.model, "gpt-5.6-luna");
    assert_eq!(ai.attempts, 2);
    assert_eq!(
        snapshot
            .to_tsv()
            .lines()
            .last()
            .expect("TSV")
            .split('\t')
            .count(),
        40
    );
    service.stop().expect("stop");
    let _ = fs::remove_file(path);
}

#[test]
fn repair_reads_at_most_the_size_cap_and_truncates_an_over_cap_history_file() {
    // Appends enforce MAX_INPUT_HISTORY_BYTES, so an over-cap file can
    // only come from corruption or external tampering. Repair must not
    // load the oversized tail (the read is capped) and must still
    // truncate the file back to its last valid frame.
    let path = temporary_path("overcap");
    append_records(&path, &[key_record(1, 1), key_record(2, 2)]);
    let valid_len = fs::metadata(&path).expect("metadata").len();
    let file = OpenOptions::new().write(true).open(&path).expect("open");
    file.set_len(MAX_INPUT_HISTORY_BYTES + 4096).expect("grow");
    drop(file);

    repair_file(&path).expect("repair");

    assert_eq!(fs::metadata(&path).expect("metadata").len(), valid_len);
    let snapshot = read_snapshot(&path).expect("snapshot");
    assert_eq!(snapshot.records.len(), 2);
    assert_eq!(snapshot.ignored_tail_bytes, 0);
    let _ = fs::remove_file(path);
}

#[test]
fn scope_classification_is_fail_closed_for_unclassified_and_sensitive_scopes() {
    assert_eq!(
        ScopeClass::from_scope(InputScope::Normal, false),
        ScopeClass::Unclassified
    );
    assert_eq!(
        ScopeClass::from_scope(InputScope::Normal, true),
        ScopeClass::Normal
    );
    for scope in [
        InputScope::Password,
        InputScope::Url,
        InputScope::Email,
        InputScope::Digits,
    ] {
        assert_eq!(ScopeClass::from_scope(scope, true), ScopeClass::Sensitive);
        assert_eq!(ScopeClass::from_scope(scope, false), ScopeClass::Sensitive);
    }
}

#[test]
fn counter_exhaustion_last_session_id_is_allocated_once_concurrently() {
    let fixture = ReadFailureFixture(temporary_path("last-session"));
    let service = InputHistoryService::open(&fixture.0).unwrap();
    service
        .next_session_id
        .store(u64::MAX - 1, Ordering::Relaxed);
    let results = thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| service.allocate_session_id()))
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    service.stop().unwrap();
    assert_eq!(
        results.iter().filter(|id| **id == Some(u64::MAX)).count(),
        1
    );
    assert_eq!(results.iter().filter(|id| id.is_none()).count(), 7);
    assert_eq!(service.allocate_session_id(), None);
    assert_eq!(service.stats.persistence_failures(), 8);
    assert_eq!(service.next_session_id.load(Ordering::Relaxed), u64::MAX);
}

#[test]
fn counter_exhaustion_last_sequence_and_clear_epoch_are_terminal() {
    let fixture = ReadFailureFixture(temporary_path("last-sequence-epoch"));
    let service = InputHistoryService::open(&fixture.0).unwrap();
    service.next_sequence.store(u64::MAX - 1, Ordering::Relaxed);
    service.record_commit(1, ScopeClass::Normal, "a", "a", 0, 0);
    service.record_commit(1, ScopeClass::Normal, "b", "b", 0, 0);
    service.flush().unwrap();
    let snapshot = read_snapshot(&fixture.0).unwrap();
    service.epoch.store(u64::MAX - 1, Ordering::Release);
    let last_clear = service.clear();
    let rejected_clear = service.clear();
    service.record_commit(1, ScopeClass::Normal, "c", "c", 0, 0);
    service.stop().unwrap();
    assert_eq!(snapshot.records.len(), 2);
    assert_eq!(snapshot.records[1].sequence(), u64::MAX);
    assert_eq!(last_clear.unwrap(), 2);
    assert!(rejected_clear.is_err());
    assert!(read_snapshot(&fixture.0).unwrap().records.is_empty());
    assert_eq!(service.next_sequence.load(Ordering::Relaxed), u64::MAX);
    assert_eq!(service.epoch.load(Ordering::Acquire), u64::MAX);
    assert_eq!(service.stats.persistence_failures(), 3);
}

#[test]
fn counter_exhaustion_unavailable_session_rejects_all_content_variants() {
    let fixture = ReadFailureFixture(temporary_path("unavailable-session"));
    let service = InputHistoryService::open(&fixture.0).unwrap();
    service.record_commit(0, ScopeClass::Normal, "a", "a", 0, 0);
    service.record_key(
        0,
        ScopeClass::Normal,
        1,
        Some('a'),
        0,
        false,
        false,
        true,
        1,
        1,
        1,
        1,
        "",
        "a",
        "",
        0,
        false,
        "char",
    );
    service.record_ai_text(
        0,
        ScopeClass::Normal,
        AiTextOperation::Proofread,
        AiTextStatus::Applied,
        "a",
        "b",
        "model",
        "provider",
        "style",
        "",
        1,
        1,
        1,
        0,
        1,
        false,
    );
    service.stop().unwrap();
    let snapshot = read_snapshot(&fixture.0).unwrap();
    assert_eq!(
        snapshot.records.len(),
        1,
        "only engine marker may use session zero"
    );
    assert!(matches!(snapshot.records[0], InputHistoryRecord::Engine(_)));
    assert_eq!(service.stats.dropped_events(), 3);
    assert_eq!(service.next_sequence.load(Ordering::Relaxed), 1);
}

#[test]
fn counter_exhaustion_session_never_panics_or_wraps() {
    let fixture = ReadFailureFixture(temporary_path("session-exhausted"));
    let service = InputHistoryService::open(&fixture.0).unwrap();
    service.next_session_id.store(u64::MAX, Ordering::Relaxed);
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service.allocate_session_id()
    }));
    let after = service.next_session_id.load(Ordering::Relaxed);
    service.stop().unwrap();
    assert!(attempt.is_ok(), "optional history allocation panicked");
    assert_eq!(after, u64::MAX, "session ID counter wrapped");
}

#[test]
fn counter_exhaustion_record_variants_never_panic_or_wrap() {
    let fixture = ReadFailureFixture(temporary_path("sequence-exhausted"));
    let service = InputHistoryService::open(&fixture.0).unwrap();
    service.flush().unwrap();
    let before = fs::read(&fixture.0).unwrap();
    let mut outcomes = Vec::new();
    for variant in 0..4 {
        service.next_sequence.store(u64::MAX, Ordering::Relaxed);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match variant {
            0 => service.record_engine_start(),
            1 => service.record_commit(1, ScopeClass::Normal, "a", "a", 0, 0),
            2 => service.record_key(
                1,
                ScopeClass::Normal,
                1,
                Some('a'),
                0,
                false,
                false,
                true,
                1,
                1,
                1,
                1,
                "",
                "a",
                "",
                0,
                false,
                "char",
            ),
            _ => service.record_ai_text(
                1,
                ScopeClass::Normal,
                AiTextOperation::Proofread,
                AiTextStatus::Applied,
                "a",
                "b",
                "model",
                "provider",
                "style",
                "",
                1,
                1,
                1,
                0,
                1,
                false,
            ),
        }));
        outcomes.push((
            result.is_ok(),
            service.next_sequence.load(Ordering::Relaxed),
        ));
    }
    service.stop().unwrap();
    assert!(
        outcomes.iter().all(|&(ok, n)| ok && n == u64::MAX),
        "record counter outcomes {outcomes:?}"
    );
    assert_eq!(fs::read(&fixture.0).unwrap(), before);
}

#[test]
fn counter_exhaustion_clear_preserves_bytes_and_epoch() {
    let fixture = ReadFailureFixture(temporary_path("epoch-exhausted"));
    let service = InputHistoryService::open(&fixture.0).unwrap();
    service.flush().unwrap();
    let before = fs::read(&fixture.0).unwrap();
    service.epoch.store(u64::MAX, Ordering::Release);
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| service.clear()));
    let after = service.epoch.load(Ordering::Acquire);
    service.stop().unwrap();
    assert!(
        matches!(attempt, Ok(Err(_))),
        "exhausted Clear must return an error"
    );
    assert_eq!(after, u64::MAX);
    assert_eq!(fs::read(&fixture.0).unwrap(), before);
}

#[test]
fn session_ids_are_shared_by_the_history_service() {
    let path = temporary_path("session");
    let service = InputHistoryService::open(&path).expect("open");
    let first = service.allocate_session_id();
    let second = service.allocate_session_id();
    assert_eq!(first, Some(1));
    assert_eq!(second, Some(2));
    service.stop().expect("stop");
    let _ = fs::remove_file(path);
}

#[test]
fn sensitive_scope_is_rejected_and_clear_is_race_safe() {
    let path = temporary_path("clear");
    let service = InputHistoryService::open(&path).expect("open");
    service.record_key(
        1,
        ScopeClass::Unclassified,
        1,
        Some('u'),
        0,
        false,
        true,
        true,
        1,
        1,
        1,
        1,
        "",
        "u",
        "",
        0,
        false,
        "char",
    );
    service.record_commit(1, ScopeClass::Unclassified, "u", "u", 0, 0);
    service.record_key(
        1,
        ScopeClass::Sensitive,
        1,
        Some('x'),
        0,
        false,
        false,
        true,
        1,
        1,
        1,
        1,
        "",
        "x",
        "",
        0,
        false,
        "char",
    );
    service.record_key(
        1,
        ScopeClass::Normal,
        1,
        Some('x'),
        0,
        false,
        false,
        true,
        1,
        1,
        1,
        1,
        "",
        "x",
        "",
        0,
        false,
        "char",
    );
    let stats = service.stats().snapshot();
    assert_eq!(stats.excluded_unclassified_events, 1);
    assert_eq!(stats.excluded_sensitive_events, 1);
    assert_eq!(stats.excluded_test_only_events, 1);
    service.flush().expect("flush before clear");
    assert_eq!(service.clear().expect("clear"), 2);
    service.flush().expect("flush");
    assert!(read_snapshot(&path).expect("snapshot").records.is_empty());
    service.stop().expect("stop");
    let _ = fs::remove_file(path);
}

#[test]
fn retention_removes_old_records_even_when_the_file_is_small() {
    let path = temporary_path("retention");
    let now = now_ms();
    let old = now.saturating_sub(RETENTION.as_millis() as u64 + 1);
    append_records(&path, &[key_record(1, old), key_record(2, now)]);

    compact_file(&path).expect("compact retention");

    let snapshot = read_snapshot(&path).expect("snapshot");
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(snapshot.records[0].sequence(), 2);
    let _ = fs::remove_file(path);
}

#[test]
fn idle_writer_compacts_old_records_without_new_commands() {
    let path = temporary_path("idle-retention");
    let now = now_ms();
    let old = now.saturating_sub(RETENTION.as_millis() as u64 + 1);
    append_records(&path, &[key_record(1, old), key_record(2, now)]);

    let (sender, receiver) = mpsc::sync_channel(1);
    let stats = Arc::new(InputHistoryStats::default());
    let writer_stats = Arc::clone(&stats);
    let writer_path = path.clone();
    let worker = thread::spawn(move || {
        writer_loop_with_interval(
            writer_path,
            receiver,
            writer_stats,
            Duration::from_millis(20),
        )
    });

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(snapshot) = read_snapshot(&path) {
            if snapshot.records.len() == 1 {
                break;
            }
        }
        assert!(Instant::now() < deadline, "idle retention did not run");
        thread::sleep(Duration::from_millis(10));
    }

    drop(sender);
    worker.join().expect("idle writer");
    let snapshot = read_snapshot(&path).expect("snapshot");
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(snapshot.records[0].sequence(), 2);
    let _ = fs::remove_file(path);
}

#[test]
fn persistence_failures_remain_visible_after_the_writer_stops() {
    let path = temporary_path("failure-stats");
    let service = InputHistoryService::open(&path).expect("open");
    service.stop().expect("stop");
    service.record_key(
        1,
        ScopeClass::Normal,
        1,
        Some('x'),
        0,
        false,
        false,
        true,
        0,
        1,
        1,
        1,
        "",
        "x",
        "",
        0,
        false,
        "char",
    );
    assert_eq!(service.stats().persistence_failures(), 1);
    let _ = fs::remove_file(path);
}

#[test]
fn legacy_format_v1_files_remain_readable() {
    let path = temporary_path("legacy-v1");
    append_records(&path, &[key_record(1, now_ms())]);
    let mut bytes = fs::read(&path).expect("read");
    bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
    fs::write(&path, &bytes).expect("downgrade header");

    let snapshot = read_snapshot(&path).expect("read v1");
    assert_eq!(snapshot.format_version, 1);
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(snapshot.last_engine_identity(), None);
    let tsv = snapshot.to_tsv();
    assert!(tsv.contains("# sakura-input-history-format: 1"));
    assert!(tsv.contains("# package-version: -"));
    assert!(tsv.contains("# release-label: -"));
    repair_file(&path).expect("repair v1");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    compact_file(&path).expect("compact v1");
    assert_eq!(read_snapshot(&path).unwrap().records, snapshot.records);
    let service = InputHistoryService::open(&path).expect("open compacted v1");
    service.stop().expect("stop compacted v1");
    assert!(read_snapshot(&path)
        .unwrap()
        .records
        .contains(&snapshot.records[0]));
    let _ = fs::remove_file(path);
}

#[test]
fn installed_release_dir_names_require_version_and_hex_build_id() {
    assert!(is_installed_release_dir("1.0.11-932aee9cf49964eb"));
    assert!(!is_installed_release_dir("1.0.11"));
    assert!(!is_installed_release_dir("1.0.11-not-hex-buildid"));
    assert!(!is_installed_release_dir("target"));
}
