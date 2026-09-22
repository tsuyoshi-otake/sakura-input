use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct RecoveryFixture {
    canonical: PathBuf,
    transaction: PathBuf,
}

impl RecoveryFixture {
    fn new(label: &str) -> Self {
        let canonical = std::env::temp_dir().join(format!(
            "sakura-history-recovery-{}-{label}-{}.bin",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let transaction = compaction_transaction_path(&canonical).unwrap();
        Self {
            canonical,
            transaction,
        }
    }

    fn install_plan(&self, old: &[u8], new: &[u8]) {
        fs::create_dir(&self.transaction).unwrap();
        fs::write(
            self.transaction.join("publication.bin"),
            recovery_plan(old, new),
        )
        .unwrap();
    }
}

impl Drop for RecoveryFixture {
    fn drop(&mut self) {
        for path in [
            self.transaction.join("replacement.bin"),
            self.transaction.join("previous.bin"),
            self.transaction.join("publication.bin"),
            self.transaction.join("unexpected.bin"),
            self.canonical.clone(),
        ] {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove recovery fixture: {error}"),
            }
        }
        match fs::remove_dir(&self.transaction) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove recovery transaction: {error}"),
        }
    }
}

fn recovery_record(sequence: u64) -> InputHistoryRecord {
    InputHistoryRecord::Key(KeyHistoryRecord {
        sequence,
        timestamp_ms: now_ms(),
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

fn recovery_image(sequences: &[u64]) -> Vec<u8> {
    let mut image = header().to_vec();
    for sequence in sequences {
        let payload = protect(&recovery_record(*sequence).encode().unwrap()).unwrap();
        image.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        image.extend_from_slice(&crc32(&payload).to_le_bytes());
        image.extend_from_slice(&payload);
    }
    image
}

fn recovery_plan(old: &[u8], new: &[u8]) -> Vec<u8> {
    let mut plan = b"SKCP0001".to_vec();
    plan.extend_from_slice(&Sha256::digest(old).unwrap());
    plan.extend_from_slice(&Sha256::digest(new).unwrap());
    protect(&plan).unwrap()
}

fn assert_rejected_without_mutation(fixture: &RecoveryFixture) {
    let canonical = fs::read(&fixture.canonical).unwrap();
    let mut participants: Vec<_> = fs::read_dir(&fixture.transaction)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    participants.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(recover_compaction(&fixture.canonical).is_err());
    assert_eq!(fs::read(&fixture.canonical).unwrap(), canonical);
    for (path, bytes) in participants {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn authenticated_old_canonical_recovers_partial_replacement_matrix() {
    let old = recovery_image(&[1, 2]);
    let new = recovery_image(&[2]);
    for length in [0, 4, HEADER_LEN, new.len() - 1] {
        let fixture = RecoveryFixture::new(&format!("partial-{length}"));
        fs::write(&fixture.canonical, &old).unwrap();
        fixture.install_plan(&old, &new);
        fs::write(fixture.transaction.join("replacement.bin"), &new[..length]).unwrap();

        let summary = recover_compaction(&fixture.canonical).unwrap().unwrap();
        assert_eq!(summary.valid_end, old.len());
        assert_eq!(fs::read(&fixture.canonical).unwrap(), old);
        assert!(!fixture.transaction.exists());
    }
}

#[test]
fn interrupted_rollback_requires_old_canonical_and_absent_backup() {
    let old = recovery_image(&[1, 2]);
    let new = recovery_image(&[2]);
    let unrelated = recovery_image(&[3]);
    for (matches_old, backup_absent, accepted) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, true),
    ] {
        let fixture = RecoveryFixture::new("rollback-condition");
        fs::write(
            &fixture.canonical,
            if matches_old { &old } else { &unrelated },
        )
        .unwrap();
        fixture.install_plan(&old, &new);
        fs::write(fixture.transaction.join("replacement.bin"), &new[..4]).unwrap();
        if !backup_absent {
            fs::write(fixture.transaction.join("previous.bin"), &old).unwrap();
        }

        if accepted {
            recover_compaction(&fixture.canonical).unwrap().unwrap();
            assert_eq!(fs::read(&fixture.canonical).unwrap(), old);
            assert!(!fixture.transaction.exists());
        } else {
            assert_rejected_without_mutation(&fixture);
        }
        println!(
            "decision-evidence history.interrupted_rollback {}{} {}",
            u8::from(matches_old),
            u8::from(backup_absent),
            u8::from(accepted)
        );
    }
}

#[test]
fn partial_replacement_without_plan_or_after_publication_fails_closed() {
    let old = recovery_image(&[1, 2]);
    let new = recovery_image(&[2]);

    let legacy = RecoveryFixture::new("legacy-partial");
    fs::write(&legacy.canonical, &old).unwrap();
    fs::create_dir(&legacy.transaction).unwrap();
    fs::write(legacy.transaction.join("replacement.bin"), &new[..4]).unwrap();
    assert_rejected_without_mutation(&legacy);

    let published = RecoveryFixture::new("published-partial");
    fs::write(&published.canonical, &new).unwrap();
    published.install_plan(&old, &new);
    fs::write(published.transaction.join("replacement.bin"), &new[..4]).unwrap();
    assert_rejected_without_mutation(&published);

    let missing = RecoveryFixture::new("missing-canonical-partial");
    missing.install_plan(&old, &new);
    fs::write(missing.transaction.join("previous.bin"), &old).unwrap();
    fs::write(missing.transaction.join("replacement.bin"), &new[..4]).unwrap();
    let participants: Vec<_> = fs::read_dir(&missing.transaction)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    assert!(recover_compaction(&missing.canonical).is_err());
    assert!(!missing.canonical.exists());
    for (path, bytes) in participants {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn contradictory_complete_replacement_and_bad_plan_preserve_evidence() {
    let old = recovery_image(&[1, 2]);
    let new = recovery_image(&[2]);
    let contradictory = recovery_image(&[3]);

    let wrong_replacement = RecoveryFixture::new("wrong-complete");
    fs::write(&wrong_replacement.canonical, &old).unwrap();
    wrong_replacement.install_plan(&old, &new);
    fs::write(
        wrong_replacement.transaction.join("replacement.bin"),
        contradictory,
    )
    .unwrap();
    assert_rejected_without_mutation(&wrong_replacement);

    let bad_plan = RecoveryFixture::new("bad-plan-partial");
    fs::write(&bad_plan.canonical, &old).unwrap();
    fs::create_dir(&bad_plan.transaction).unwrap();
    fs::write(bad_plan.transaction.join("publication.bin"), b"not a plan").unwrap();
    fs::write(bad_plan.transaction.join("replacement.bin"), &new[..4]).unwrap();
    assert_rejected_without_mutation(&bad_plan);
}

#[test]
fn unknown_and_reparse_participants_preserve_the_transaction() {
    let old = recovery_image(&[1, 2]);
    let new = recovery_image(&[2]);

    let unknown = RecoveryFixture::new("unknown-participant");
    fs::write(&unknown.canonical, &old).unwrap();
    unknown.install_plan(&old, &new);
    fs::write(unknown.transaction.join("replacement.bin"), &new[..4]).unwrap();
    fs::write(unknown.transaction.join("unexpected.bin"), b"evidence").unwrap();
    assert_rejected_without_mutation(&unknown);

    let reparse = RecoveryFixture::new("reparse-participant");
    let target = RecoveryFixture::new("reparse-target");
    fs::write(&reparse.canonical, &old).unwrap();
    reparse.install_plan(&old, &new);
    fs::write(&target.canonical, &new).unwrap();
    std::os::windows::fs::symlink_file(
        &target.canonical,
        reparse.transaction.join("replacement.bin"),
    )
    .unwrap();
    assert_rejected_without_mutation(&reparse);
}

#[test]
fn complete_replacement_and_published_generation_still_recover() {
    let old = recovery_image(&[1, 2]);
    let new = recovery_image(&[2]);

    let unpublished = RecoveryFixture::new("complete-unpublished");
    fs::write(&unpublished.canonical, &old).unwrap();
    unpublished.install_plan(&old, &new);
    fs::write(unpublished.transaction.join("replacement.bin"), &new).unwrap();
    recover_compaction(&unpublished.canonical).unwrap().unwrap();
    assert_eq!(fs::read(&unpublished.canonical).unwrap(), old);

    let published = RecoveryFixture::new("complete-published");
    fs::write(&published.canonical, &new).unwrap();
    published.install_plan(&old, &new);
    fs::write(published.transaction.join("previous.bin"), &old).unwrap();
    recover_compaction(&published.canonical).unwrap().unwrap();
    assert_eq!(fs::read(&published.canonical).unwrap(), new);
}
