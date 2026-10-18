//! #360 bak: `clear` also deletes the retained `v{N}.bak` upgrade backups,
//! because they hold the pre-upgrade learning the user asked to erase (K1).
//! A removal failure fails the clear before the empty log is published, so
//! `log.bin` stays unchanged (K2); a cleared backup can never come back
//! through startup recovery (K3); names that are not this log's version
//! backups — and `forget_prediction_exact` — leave them untouched (K4).
//!
//! Tests assert on the file system directly — `fs::read`, `fs::read_dir`,
//! `Path::exists` — and through `LearningService` reads, never through the
//! store's backup-removal helper.

use super::*;

/// K1: a successful `clear` removes the `log.v1.bak` an upgrade retained.
#[test]
fn clear_deletes_the_version_backup_an_upgrade_left() {
    let temporary = TemporaryLearningLog::new("version-backup-clear");
    let path = &temporary.0;
    let old = write_version_one_log(path, "かな", "加奈");
    let service = LearningService::open(path).expect("upgrade v1 log");
    assert_eq!(
        fs::read(path.with_extension("v1.bak")).expect("v1 backup"),
        old
    );
    service.learn("きく", "菊", 5, 6);

    service.clear().expect("clear");

    assert_eq!(sibling_names(path), vec!["log.bin".to_owned()]);
}

/// K1, K4: `clear` removes every version-backup name — any version number,
/// the exact name or the `.{pid}.{id}` numbered form — and nothing else.
/// Files that are not this log's version backups keep their bytes, and a
/// same-named directory is left alone.
#[test]
fn clear_deletes_every_version_backup_and_nothing_else() {
    let temporary = TemporaryLearningLog::new("version-backup-matching");
    let path = &temporary.0;
    let service = LearningService::open(path).expect("open");
    service.learn("かな", "加奈", 3, 4);
    service.maintain().expect("flush");
    let directory = path.parent().expect("parent");
    let matching = ["log.v2.bak", "log.v1.bak.4242.7", "log.v7.bak"];
    let kept = [
        ("log.v1.bak.txt", b"extension".as_slice()),
        ("log.v1.backup", b"wrong suffix".as_slice()),
        ("log.vx.bak", b"non-numeric version".as_slice()),
        ("log.v.bak", b"empty version".as_slice()),
        ("log.v1.bak.42", b"one-part tail".as_slice()),
        ("other.v1.bak", b"other base".as_slice()),
        ("config.toml.v1.bak", b"config backup".as_slice()),
    ];
    for (index, name) in matching.iter().enumerate() {
        fs::write(directory.join(name), format!("match {index}")).expect(name);
    }
    for (name, bytes) in kept {
        fs::write(directory.join(name), bytes).expect(name);
    }
    fs::create_dir(directory.join("log.v2.bak.9.9")).expect("directory decoy");

    service.clear().expect("clear");

    for name in matching {
        assert!(!directory.join(name).exists(), "{name} survived clear");
    }
    for (name, bytes) in kept {
        assert_eq!(fs::read(directory.join(name)).expect(name), bytes);
    }
    assert!(
        directory.join("log.v2.bak.9.9").is_dir(),
        "clear removed a directory"
    );
}

/// K2: when a version backup cannot be removed, `clear` fails before the
/// empty log is published — `log.bin` and the backup are unchanged, the
/// learned pair still replays after reopen, and a retried clear succeeds.
#[cfg(windows)]
#[test]
fn clear_fails_closed_when_a_version_backup_cannot_be_removed() {
    // The guard is declared first so `held` and `service` drop before it:
    // the held file must be closed before its directory can be removed,
    // even when an assertion panics.
    let temporary = TemporaryLearningLog::new("version-backup-held");
    let path = &temporary.0;
    let service = LearningService::open(path).expect("open");
    service.learn("かな", "加奈", 3, 4);
    service.maintain().expect("flush");
    fs::write(path.with_extension("v1.bak"), b"staged backup").expect("staged backup");
    let held = hold_without_delete_share(&path.with_extension("v1.bak"));
    let before = fs::read(path).expect("log.bin before clear");

    assert!(
        service.clear().is_err(),
        "clear published while a backup could not be removed"
    );
    assert_eq!(
        fs::read(path).expect("log.bin after failed clear"),
        before,
        "a failed clear changed log.bin"
    );
    assert!(path.with_extension("v1.bak").exists());
    drop(held);
    drop(service);

    let reopened = LearningService::open(path).expect("reopen after failed clear");
    assert!(
        history_contains(&reopened, "かな", "加奈"),
        "a failed clear still emptied the log"
    );
    reopened.clear().expect("retried clear");
    assert!(!path.with_extension("v1.bak").exists());
}

/// K3: once `clear` removed the upgrade backup, deleting `log.bin` by hand
/// cannot bring the pre-upgrade learning back through startup recovery.
#[test]
fn cleared_version_backup_cannot_be_restored_after_log_bin_is_deleted() {
    let temporary = TemporaryLearningLog::new("version-backup-no-restore");
    let path = &temporary.0;
    write_version_one_log(path, "かな", "加奈");
    let service = LearningService::open(path).expect("upgrade v1 log");
    assert!(history_contains(&service, "かな", "加奈"));

    service.clear().expect("clear");
    drop(service);
    fs::remove_file(path).expect("delete log.bin");

    let reopened = LearningService::open(path).expect("reopen without canonical");
    assert!(!history_contains(&reopened, "かな", "加奈"));
    assert!(!snapshot_contains(path, "かな", "加奈"));
    assert!(
        sibling_names(path)
            .iter()
            .all(|name| !name.starts_with("log.v")),
        "a version backup reappeared"
    );
}

/// K4: forgetting one learned item rewrites `log.bin` but leaves the
/// retained upgrade backup byte-for-byte unchanged.
#[test]
fn forgetting_one_pair_keeps_the_version_backup() {
    let temporary = TemporaryLearningLog::new("version-backup-forget");
    let path = &temporary.0;
    let old = write_version_one_log(path, "かな", "加奈");
    let service = LearningService::open(path).expect("upgrade v1 log");
    service.learn("きく", "菊", 5, 6);
    service.maintain().expect("flush");

    assert_eq!(
        service
            .forget_prediction_exact("きく", "菊")
            .expect("forget"),
        ForgetPredictionOutcome::Removed
    );
    assert_eq!(
        fs::read(path.with_extension("v1.bak")).expect("v1 backup"),
        old,
        "forgetting one pair changed the retained backup"
    );
}
