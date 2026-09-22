use super::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static CORRUPT_REPLACEMENT: Cell<bool> = const { Cell::new(false) };
    static CORRUPT_CANONICAL: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn corrupt_replacement_if_requested(path: &Path) {
    CORRUPT_REPLACEMENT.with(|requested| {
        if requested.replace(false) {
            corrupt_file(path);
        }
    });
}

pub(super) fn corrupt_canonical_if_requested(path: &Path) {
    CORRUPT_CANONICAL.with(|requested| {
        if requested.replace(false) {
            corrupt_file(path);
        }
    });
}

fn corrupt_file(path: &Path) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.seek(SeekFrom::Start(HEADER_LEN as u64)).unwrap();
    file.write_all(&[0xff]).unwrap();
    file.sync_all().unwrap();
}

struct CapacityFixture(PathBuf);

impl CapacityFixture {
    fn new(label: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "sakura-input-history-capacity-{}-{label}-{}.bin",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn transaction(&self) -> PathBuf {
        compaction_transaction_path(self.path()).unwrap()
    }
}

impl Drop for CapacityFixture {
    fn drop(&mut self) {
        let transaction = self.transaction();
        for name in ["replacement.bin", "previous.bin", "publication.bin"] {
            match fs::remove_file(transaction.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove capacity transaction file: {error}"),
            }
        }
        match fs::remove_dir(&transaction) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove capacity transaction directory: {error}"),
        }
        match fs::remove_file(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove capacity fixture: {error}"),
        }
    }
}

fn key_record(sequence: u64, timestamp_ms: u64) -> InputHistoryRecord {
    InputHistoryRecord::Key(KeyHistoryRecord {
        sequence,
        timestamp_ms,
        session: 1,
        scope: HistoryScope::Normal,
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
    ensure_file(path).unwrap();
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    for record in records {
        let protected = protect(&record.encode().unwrap()).unwrap();
        append_encrypted(&mut file, &protected).unwrap();
    }
}

#[test]
fn compaction_keeps_exact_newest_records_and_reuses_authenticated_frames() {
    let fixture = CapacityFixture::new("newest");
    let records: Vec<_> = (1..=20)
        .map(|sequence| key_record(sequence, now_ms()))
        .collect();
    append_records(fixture.path(), &records);
    let before = fs::read(fixture.path()).unwrap();
    let mut ranges = Vec::new();
    scan_frames_with(&before, |record, range| {
        ranges.push((record.sequence(), range));
        Ok(())
    })
    .unwrap();
    let tail_start = ranges[15].1.start;
    let limit = (HEADER_LEN + before.len() - tail_start) as u64;

    compact_file_to_limit(fixture.path(), limit).unwrap();

    let after = fs::read(fixture.path()).unwrap();
    assert_eq!(&after[HEADER_LEN..], &before[tail_start..]);
    let retained = read_snapshot(fixture.path()).unwrap();
    let sequences: Vec<_> = retained
        .records
        .iter()
        .map(InputHistoryRecord::sequence)
        .collect();
    assert_eq!(sequences, [16, 17, 18, 19, 20]);
}

#[test]
fn corrupted_written_replacement_is_rejected_before_publication() {
    let fixture = CapacityFixture::new("corrupt-replacement");
    append_records(
        fixture.path(),
        &[key_record(1, now_ms()), key_record(2, now_ms())],
    );
    let original = fs::read(fixture.path()).unwrap();
    CORRUPT_REPLACEMENT.with(|requested| requested.set(true));

    let error = compact_file(fixture.path()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    assert!(fixture.transaction().join("publication.bin").exists());
    assert!(fixture.transaction().join("replacement.bin").exists());
    assert!(!fixture.transaction().join("previous.bin").exists());
}

#[test]
fn corrupted_published_canonical_is_rejected_before_backup_removal() {
    let fixture = CapacityFixture::new("corrupt-canonical");
    append_records(
        fixture.path(),
        &[key_record(1, now_ms()), key_record(2, now_ms())],
    );
    let original = fs::read(fixture.path()).unwrap();
    CORRUPT_CANONICAL.with(|requested| requested.set(true));

    let error = compact_file(fixture.path()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_ne!(fs::read(fixture.path()).unwrap(), original);
    assert_eq!(
        fs::read(fixture.transaction().join("previous.bin")).unwrap(),
        original
    );
    assert!(fixture.transaction().join("publication.bin").exists());
}

#[test]
fn exact_image_reader_rejects_short_and_hash_mismatched_images() {
    let expected = Sha256::digest(b"abc").unwrap();
    let short = verify_compaction_reader(&mut io::Cursor::new(b"ab"), 3, &expected).unwrap_err();
    assert_eq!(short.kind(), io::ErrorKind::InvalidData);

    let wrong_hash = Sha256::digest(b"abd").unwrap();
    let mismatch =
        verify_compaction_reader(&mut io::Cursor::new(b"abc"), 3, &wrong_hash).unwrap_err();
    assert_eq!(mismatch.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn capacity_pressure_reclaims_a_batch_and_accepts_subsequent_records() {
    let fixture = CapacityFixture::new("full");
    let mut record = key_record(1, now_ms());
    if let InputHistoryRecord::Key(key) = &mut record {
        key.action = "x".repeat(15_000);
    }
    let payload = record.encode().unwrap();
    let protected = protect(&payload).unwrap();
    let mut frame = Vec::new();
    frame.extend_from_slice(&(protected.len() as u32).to_le_bytes());
    frame.extend_from_slice(&crc32(&protected).to_le_bytes());
    frame.extend_from_slice(&protected);
    let mut file = File::create(fixture.path()).unwrap();
    file.write_all(&header()).unwrap();
    let count = (MAX_INPUT_HISTORY_BYTES as usize - HEADER_LEN) / frame.len();
    for _ in 0..count {
        file.write_all(&frame).unwrap();
    }
    drop(file);
    let mut writer = Some(open_append(fixture.path()).unwrap());
    let mut retention = RetentionPlan::default();

    append_payload(
        fixture.path(),
        &mut writer,
        &payload,
        now_ms(),
        &mut retention,
    )
    .unwrap();
    let compacted_len = writer.as_ref().unwrap().metadata().unwrap().len();
    assert!(compacted_len <= MAX_INPUT_HISTORY_BYTES * 7 / 8 + frame.len() as u64);
    for _ in 0..10 {
        append_payload(
            fixture.path(),
            &mut writer,
            &payload,
            now_ms(),
            &mut retention,
        )
        .unwrap();
    }

    assert_eq!(
        writer.as_ref().unwrap().metadata().unwrap().len(),
        compacted_len + 10 * frame.len() as u64
    );
    drop(writer);
    assert!(!fixture.transaction().exists());
    validate_compaction_file(fixture.path()).unwrap();
}
