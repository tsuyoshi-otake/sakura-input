use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Storage::FileSystem::FILE_FLAG_DELETE_ON_CLOSE;

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temporary_history(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "sakura-i455-{}-{name}-{}.bin",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn lock_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().expect("file name").to_os_string();
    name.push(".writer.lock");
    path.with_file_name(name)
}

/// The acquisition shape every released version used before this change:
/// ownership proven by the file's existence, delete-on-close, exclusive.
fn acquire_old_style(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(lock_path_for(path))
}

/// Helper entry: a child spawned with SAKURA_I455_HELPER=<history path>
/// holds the store owner forever, so the parent can kill or probe it.
#[test]
fn orphaned_lock_holder_helper() {
    let Some(history) = std::env::var_os("SAKURA_I455_HELPER").map(PathBuf::from) else {
        return;
    };
    let _owner = acquire_store_owner(&history).expect("helper owns the store");
    std::thread::sleep(Duration::from_secs(3_600));
}

fn spawn_lock_holder(path: &Path) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "input_history::ownership_tests::orphaned_lock_holder_helper",
            "--nocapture",
        ])
        .env("SAKURA_I455_HELPER", path)
        .spawn()
        .expect("spawn lock holder")
}

fn wait_for_lock(lock: &Path, timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if lock.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// #455 step 1: an isolated child that holds the lock is killed without any
/// shutdown path, then the lock's post-mortem state is measured. The durable
/// contract asserted here is that a terminated owner must never block the
/// next start; what the file does afterwards is version detail reported
/// alongside (delete-on-close removes it, a permanent lock stays empty and
/// unowned).
#[test]
fn a_terminated_owner_never_blocks_the_next_open() {
    let history = temporary_history("terminate");
    let lock = lock_path_for(&history);
    let mut child = spawn_lock_holder(&history);
    assert!(
        wait_for_lock(&lock, Duration::from_secs(30)),
        "helper did not acquire the lock"
    );

    child.kill().expect("terminate the holder");
    child.wait().expect("join the holder");

    if lock.exists() {
        assert_eq!(
            fs::metadata(&lock).expect("lock metadata").len(),
            0,
            "a surviving lock must be empty"
        );
    }
    let service = InputHistoryService::open(&history)
        .expect("the next start must own the store the killed holder left");
    service.stop().expect("stop service");
    let _ = fs::remove_file(&history);
    let _ = fs::remove_file(&lock);
}

/// #455 step 2/3: a regular, empty, unowned lock left behind by a dead owner
/// must not refuse startup (fail-before: `create_new` rejects it).
#[test]
fn an_unowned_empty_regular_lock_is_acquired() {
    let history = temporary_history("empty-lock");
    let lock = lock_path_for(&history);
    File::create(&lock).expect("pre-place the orphaned lock");

    let service = InputHistoryService::open(&history)
        .expect("an unowned empty regular lock must be re-acquired");
    service.stop().expect("stop service");

    // The store behind the untouched lock still opens and keeps its bytes.
    let service = InputHistoryService::open(&history).expect("reopen");
    service.stop().expect("stop service");
    let _ = fs::remove_file(&history);
    let _ = fs::remove_file(&lock);
}

/// Step 3 detail: reopening behind an orphaned lock must not rewrite the
/// existing store.
#[test]
fn reopening_behind_an_orphaned_lock_keeps_the_store_untouched() {
    let history = temporary_history("preserve");
    let service = InputHistoryService::open(&history).expect("initial open");
    service.stop().expect("stop service");
    let bytes = fs::read(&history).expect("store bytes");

    File::create(lock_path_for(&history)).expect("pre-place the orphaned lock");
    let service = InputHistoryService::open(&history).expect("reopen behind an orphaned lock");
    assert_eq!(fs::read(&history).expect("reread store"), bytes);
    service.stop().expect("stop service");
    let _ = fs::remove_file(&history);
    let _ = fs::remove_file(lock_path_for(&history));
}

/// #455 step 4 (state row 3): a lock another process holds still refuses a
/// second owner, and the first holder keeps writing.
#[test]
fn a_held_lock_rejects_a_second_process() {
    let history = temporary_history("held");
    let lock = lock_path_for(&history);
    let mut child = spawn_lock_holder(&history);
    assert!(
        wait_for_lock(&lock, Duration::from_secs(30)),
        "helper did not acquire the lock"
    );

    assert!(
        InputHistoryService::open(&history).is_err(),
        "a second process must not take the held lock"
    );

    child.kill().expect("terminate the holder");
    child.wait().expect("join the holder");
    let service = InputHistoryService::open(&history).expect("the holder's death frees the store");
    service.stop().expect("stop service");
    let _ = fs::remove_file(&history);
    let _ = fs::remove_file(&lock);
}

/// Same row, same process: a second open against a held lock fails while the
/// first holder keeps owning.
#[test]
fn a_second_open_on_a_held_lock_fails() {
    let history = temporary_history("same-process");
    let owner = acquire_store_owner(&history).expect("first owner");
    assert!(acquire_store_owner(&history).is_err());
    assert!(InputHistoryService::open(&history).is_err());
    drop(owner);
    let service = InputHistoryService::open(&history).expect("released store opens");
    service.stop().expect("stop service");
    let _ = fs::remove_file(&history);
    let _ = fs::remove_file(lock_path_for(&history));
}

/// #455 step 5: anything that is not a regular empty file fails closed and
/// the suspicious object is never modified.
#[test]
fn a_reparse_point_lock_is_rejected_untouched() {
    let history = temporary_history("reparse");
    let lock = lock_path_for(&history);

    // A file symlink is the narrowest reparse object; it needs
    // SeCreateSymbolicLinkPrivilege. A directory junction is a privilege-free
    // reparse point at the same lock name and is the form this test most
    // often reaches. Try the junction first so the row actually executes on
    // unprivileged hosts, then the symlink, and only skip -- loudly -- when
    // the host can produce neither.
    let junction_target = temporary_history("reparse-junction-target");
    fs::create_dir(&junction_target).expect("junction target directory");
    let junction = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&lock)
        .arg(&junction_target)
        .status();
    // `is_dir()` follows the reparse point, which a host can refuse under a
    // mount-point policy; `symlink_metadata` reads the junction itself.
    let made_junction = matches!(junction, Ok(status) if status.success())
        && fs::symlink_metadata(&lock)
            .map(|meta| meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0)
            .unwrap_or(false);

    if made_junction {
        // The junction is a reparse point and not a regular empty file, so
        // the store must refuse it and must not modify or remove it.
        assert!(InputHistoryService::open(&history).is_err());
        assert!(
            fs::symlink_metadata(&lock)
                .expect("junction still present")
                .file_attributes()
                & FILE_ATTRIBUTE_REPARSE_POINT.0
                != 0,
            "the rejected junction must be left as a reparse point"
        );
        let _ = fs::remove_dir(&lock);
        let _ = fs::remove_dir(&junction_target);
        return;
    }
    let _ = fs::remove_dir(&junction_target);

    let target = temporary_history("reparse-target");
    fs::write(&target, b"evidence").expect("target payload");
    let linked = std::os::windows::fs::symlink_file(&target, &lock);
    if linked.is_err() {
        // No symlink privilege and no junction: neither reparse object is
        // available. Report the skip honestly instead of passing vacuously.
        eprintln!(
            "a_reparse_point_lock_is_rejected_untouched: SKIPPED -- host \
             cannot create a reparse object (mklink /J failed, no symlink \
             privilege); the directory and non-empty rows cover rejection"
        );
        let _ = fs::remove_file(&target);
        return;
    }

    assert!(InputHistoryService::open(&history).is_err());
    assert_eq!(fs::read(&target).expect("target untouched"), b"evidence");
    let _ = fs::remove_file(&lock);
    let _ = fs::remove_file(&target);
}

#[test]
fn a_directory_at_the_lock_name_is_rejected() {
    let history = temporary_history("dir-lock");
    let lock = lock_path_for(&history);
    fs::create_dir(&lock).expect("directory at the lock name");

    assert!(InputHistoryService::open(&history).is_err());
    assert!(lock.is_dir());
    let _ = fs::remove_dir(&lock);
}

#[test]
fn a_nonempty_lock_is_rejected_untouched() {
    let history = temporary_history("nonempty");
    let lock = lock_path_for(&history);
    fs::write(&lock, b"foreign bytes").expect("foreign lock content");

    assert!(InputHistoryService::open(&history).is_err());
    assert_eq!(fs::read(&lock).expect("lock untouched"), b"foreign bytes");
    let _ = fs::remove_file(&lock);
}

/// #455 step 6: interop with the released acquisition shape. An old-version
/// holder (file-existence ownership, delete-on-close) excludes the new
/// service; a new-version holder excludes the old opener.
#[test]
fn old_and_new_owners_never_write_together() {
    let history = temporary_history("interop");

    // Old version holds -> the new service must refuse (sharing violation).
    let old_owner = acquire_old_style(&history).expect("old holder");
    assert!(
        InputHistoryService::open(&history).is_err(),
        "the new service must not join an old holder"
    );
    drop(old_owner);

    // New version holds -> the old opener must refuse (file exists / busy).
    let service = InputHistoryService::open(&history).expect("new holder");
    assert!(
        acquire_old_style(&history).is_err(),
        "the old opener must not join a new holder"
    );
    service.stop().expect("stop service");
    let _ = fs::remove_file(&history);
    let _ = fs::remove_file(lock_path_for(&history));
}

/// State row 5: a rejected lock does not wedge the service -- after the
/// foreign object is removed, the next initialization reaches the same check
/// and proceeds.
#[test]
fn a_retry_after_removing_a_bad_lock_succeeds() {
    let history = temporary_history("retry");
    let lock = lock_path_for(&history);
    fs::write(&lock, b"foreign bytes").expect("foreign lock content");
    assert!(InputHistoryService::open(&history).is_err());

    fs::remove_file(&lock).expect("manual recovery removes the bad lock");
    let service = InputHistoryService::open(&history).expect("the retried open owns the store");
    service.stop().expect("stop service");
    let _ = fs::remove_file(&history);
    let _ = fs::remove_file(&lock);
}
