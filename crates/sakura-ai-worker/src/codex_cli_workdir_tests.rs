//! Regression tests for GitHub issue #360 original item #2:
//! a killed worker's leftover workdir must not break later Codex requests,
//! the stale-workdir sweep must stay safe, and a `where.exe` spawn failure
//! for `codex.cmd` must still try `codex.exe`.
//!
//! Every test uses its own base directory under `std::env::temp_dir()`,
//! never the real `%LOCALAPPDATA%`, and removes it via `TestBase` on drop.

use super::*;
use std::fs;
use std::process::Command;
use std::time::{Duration, SystemTime};

struct TestBase(PathBuf);

impl TestBase {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0);
        Self(std::env::temp_dir().join(format!("sakura-o2-{tag}-{}-{nanos}", std::process::id())))
    }
}

impl Drop for TestBase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// K1 (liveness): a workdir left by a killed worker never makes a later
/// request fail with `codex_workdir`.
#[test]
fn killed_worker_collision_uses_the_next_name() {
    let base = TestBase::new("killed-collision");
    let first = EmptyWorkdir::create_in(&base.0, 4242, 7).expect("first create");
    let first_path = first.path().to_owned();
    // A killed worker never runs Drop; the directory leaks.
    std::mem::forget(first);

    let second = EmptyWorkdir::create_in(&base.0, 4242, 7);
    assert!(second.is_ok(), "the same pid/id must still get a workdir");
    let second = second.expect("checked above");
    assert_ne!(
        second.path(),
        first_path,
        "the collision must pick a different name"
    );
    assert!(
        first_path.exists(),
        "a fresh leftover is not swept and is never reused"
    );
}

/// K2 (safety): the sweep removes only stale, matching, real directories
/// directly under `base`; it never follows a junction, never touches files
/// or other names, and never fails.
#[test]
fn sweep_removes_only_stale_matching_real_directories() {
    let base = TestBase::new("sweep-stale");
    let target = std::env::temp_dir().join(format!(
        "sakura-o2-sweep-target-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir(&target).expect("junction target");
    fs::write(target.join("keep.txt"), b"keep").expect("keep.txt");

    let leaked = EmptyWorkdir::create_in(&base.0, 4242, 7).expect("leaked workdir");
    let leaked_path = leaked.path().to_owned();
    std::mem::forget(leaked);
    fs::create_dir(base.0.join("notes")).expect("notes dir");
    fs::write(base.0.join("1-2"), b"regular file").expect("file 1-2");
    let status = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(base.0.join("9-9"))
        .arg(&target)
        .status()
        .expect("mklink spawn");
    assert!(status.success(), "mklink /J must succeed without elevation");

    sweep_stale_workdirs(&base.0, SystemTime::now() + Duration::from_secs(11 * 60));

    assert!(
        !leaked_path.exists(),
        "a stale matching real directory is removed"
    );
    assert!(
        base.0.join("notes").is_dir(),
        "a non-matching directory survives"
    );
    assert!(base.0.join("1-2").is_file(), "a regular file survives");
    assert!(
        base.0.join("9-9").exists(),
        "a matching-named junction entry survives"
    );
    assert!(
        target.join("keep.txt").exists(),
        "the sweep never reaches through the junction"
    );

    // Remove the junction itself before deleting its target.
    fs::remove_dir(base.0.join("9-9")).expect("remove junction");
    fs::remove_dir_all(&target).expect("remove junction target");
}

/// K2 (liveness): a directory younger than STALE_WORKDIR_AGE is never
/// removed.
#[test]
fn sweep_keeps_fresh_workdirs() {
    let base = TestBase::new("sweep-fresh");
    let leaked = EmptyWorkdir::create_in(&base.0, 4242, 7).expect("fresh workdir");
    let leaked_path = leaked.path().to_owned();
    std::mem::forget(leaked);

    sweep_stale_workdirs(&base.0, SystemTime::now());

    assert!(leaked_path.exists(), "a fresh workdir survives the sweep");
}

/// K3 (a): the first found path wins and no later name is asked.
#[test]
fn lookup_first_hit_skips_the_remaining_names() {
    let mut asked = Vec::new();
    let p1 = PathBuf::from("C:\\tools\\codex.cmd");
    let result = resolve_codex_cli_with(|name| {
        asked.push(name.to_owned());
        Ok(Some(p1.clone()))
    });
    assert_eq!(result, Some(p1));
    assert_eq!(asked, ["codex.cmd"]);
}

/// K3 (b) (liveness): a `where.exe` spawn failure for `codex.cmd` still
/// tries `codex.exe`. The order is unchanged.
#[test]
fn lookup_error_falls_through_to_the_next_name() {
    let mut asked = Vec::new();
    let p2 = PathBuf::from("C:\\tools\\codex.exe");
    let result = resolve_codex_cli_with(|name| {
        asked.push(name.to_owned());
        if name == "codex.cmd" {
            Err(io::Error::other("where.exe spawn failed"))
        } else {
            Ok(Some(p2.clone()))
        }
    });
    assert_eq!(result, Some(p2));
    assert_eq!(asked, ["codex.cmd", "codex.exe"]);
}

/// K3 (c): when every name misses, the result is `None` and both names
/// were asked in order.
#[test]
fn lookup_all_misses_returns_none_in_order() {
    let mut asked = Vec::new();
    let result = resolve_codex_cli_with(|name| {
        asked.push(name.to_owned());
        Ok(None)
    });
    assert_eq!(result, None);
    assert_eq!(asked, ["codex.cmd", "codex.exe"]);
}

/// K4 (unchanged behavior): `Drop` removes the directory it created.
#[test]
fn drop_removes_the_workdir() {
    let base = TestBase::new("drop");
    let workdir = EmptyWorkdir::create_in(&base.0, 4242, 9).expect("create");
    let path = workdir.path().to_owned();
    drop(workdir);
    assert!(!path.exists(), "Drop removes the workdir it created");
}
