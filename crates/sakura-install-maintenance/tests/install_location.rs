//! Non-elevated integration checks for `verify_admin_only_payload`
//! (issue #360, finding A1; contracts C1, C2, C8, C9, C11).
//!
//! Every test here must pass as a standard user: none of them writes outside
//! a temp directory it creates under `%USERPROFILE%\tmp\` and deletes before
//! returning.

#![cfg(windows)]

use std::path::{Path, PathBuf};

use sakura_install_maintenance::install_location::{verify_admin_only_payload, Rejection};

fn temp_root(name: &str) -> TempDir {
    let base = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .expect("USERPROFILE is always set on Windows")
        .join("tmp")
        .join(format!("sakura-a1-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&base).expect("create temp root");
    TempDir {
        root: base,
        junctions: Vec::new(),
    }
}

/// A temporary directory that deletes itself even when a test fails.
///
/// `junctions` are removed first with `remove_dir` — a plain `remove_dir_all`
/// on the root must never be trusted near a reparse point, so the links are
/// deleted as links before the real tree is removed.
struct TempDir {
    root: PathBuf,
    junctions: Vec<PathBuf>,
}

impl TempDir {
    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        for link in &self.junctions {
            let _ = std::fs::remove_dir(link);
        }
        if self.root.exists() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// C8 (liveness): the real system layout — `cmd.exe` under `System32`,
/// admin-owned all the way up — passes, so a supported install is not broken
/// by the check.
#[test]
fn a_system32_binary_passes_verification() {
    let root = Path::new(r"C:\Windows\System32");
    verify_admin_only_payload(root, &root.join("cmd.exe"))
        .expect("C:\\Windows\\System32\\cmd.exe must be admin-only replaceable");
}

/// C8 (liveness): the installed Sakura Input layout, when present on this
/// machine, passes — `sakura_regtool.exe` at the root and the active
/// `versions\<ver>\sakura_tsf.dll` inside it. Skips with a printed reason
/// when the product is not installed; never fails or passes silently.
#[test]
fn the_installed_program_files_layout_passes_when_present() {
    let root = Path::new(r"C:\Program Files\Sakura Input");
    let regtool = root.join("sakura_regtool.exe");
    if !regtool.is_file() {
        eprintln!(
            "skipping: {} is not installed on this machine",
            regtool.display()
        );
        return;
    }
    verify_admin_only_payload(root, &regtool).expect("the installed regtool must verify");

    let dll = std::fs::read_dir(root.join("versions"))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("sakura_tsf.dll"))
        .find(|path| path.is_file());
    let Some(dll) = dll else {
        eprintln!(
            "skipping: no versions\\<ver>\\sakura_tsf.dll under {}",
            root.display()
        );
        return;
    };
    verify_admin_only_payload(root, &dll).expect("the installed TSF DLL must verify");
}

/// C3/C5: a payload inside a user-owned directory is rejected — this is the
/// `C:\SakuraInput` shape (a standard user, or a writable user grant, can
/// replace the file).
#[test]
fn a_payload_in_a_user_owned_directory_is_rejected() {
    let root = temp_root("userowned");
    let payload = root.path().join("sakura_regtool.exe");
    std::fs::write(&payload, b"payload").expect("write payload");

    let error = verify_admin_only_payload(root.path(), &payload)
        .expect_err("a user-owned install root must fail");
    let text = error.to_string();
    assert!(
        matches!(
            error.rejection,
            Rejection::UntrustedOwner(_) | Rejection::UnsafeAce { .. }
        ),
        "the rejection must be ownership or a dangerous grant, got {error:?}"
    );
    assert!(text.contains("sakura"), "the error names the path: {text}");
}

/// C2: a junction anywhere in the walked chain is rejected even though a
/// `mklink /J` needs no elevation to create.
#[test]
fn a_junction_in_the_chain_is_rejected() {
    let mut base = temp_root("junction");
    let real = base.path().join("real");
    std::fs::create_dir_all(&real).expect("create real dir");
    let payload = real.join("sakura_regtool.exe");
    std::fs::write(&payload, b"payload").expect("write payload");

    let link = base.path().join("link");
    // New-Item's Junction target must be absolute; junction creation needs
    // no elevation.
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
                link.display(),
                real.display()
            ),
        ])
        .status()
        .expect("run New-Item Junction");
    assert!(status.success(), "junction creation failed: {status}");
    // Registered after creation so the guard deletes the link itself — never
    // a recursive delete through the junction into its target.
    base.junctions.push(link.clone());

    let error = verify_admin_only_payload(&link, &link.join("sakura_regtool.exe"))
        .expect_err("a junction install root must fail");
    assert_eq!(error.rejection, Rejection::ReparsePoint, "{error:?}");
}

/// C1.
#[test]
fn a_payload_outside_the_root_is_rejected() {
    let root = temp_root("root");
    let elsewhere = temp_root("elsewhere");
    let payload = elsewhere.path().join("sakura_regtool.exe");
    std::fs::write(&payload, b"payload").expect("write payload");

    let error = verify_admin_only_payload(root.path(), &payload)
        .expect_err("a payload outside the install root must fail");
    assert_eq!(error.rejection, Rejection::OutsideInstallRoot, "{error:?}");
}

/// A component that cannot be opened at all must fail closed with
/// `Unreadable`: containment passes on the literal (uncanonicalizable) paths,
/// then `inspect` fails at the missing payload. An uninspectable path must
/// never silently pass.
#[test]
fn an_uninspectable_payload_fails_closed_as_unreadable() {
    let base = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .expect("USERPROFILE is always set on Windows")
        .join("tmp")
        .join(format!("sakura-a1-missing-{}", std::process::id()));
    // The root is deliberately never created; cleanup must still be a no-op.
    let root = base.join("never-created");
    let payload = root.join("sakura_regtool.exe");

    let error = verify_admin_only_payload(&root, &payload)
        .expect_err("a payload that cannot be opened must fail closed");
    assert!(
        matches!(error.rejection, Rejection::Unreadable(_)),
        "expected Unreadable, got {error:?}"
    );
    assert!(!base.exists(), "the test must not create the root");
}

/// C9 (ordering): `register_cleanup_task` must run the location check before
/// any Task Scheduler call. Verified textually — the alternative, observing
/// the Task Scheduler service, requires the very machine state the check
/// exists to prevent. The search is scoped to the function body so moving
/// the call after `connect()`, or into another function, fails the test.
#[test]
fn the_cleanup_task_checks_the_location_before_contacting_task_scheduler() {
    let source =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/maintenance.rs"))
            .expect("read maintenance.rs");
    let body = function_body(&source, "pub fn register_cleanup_task");
    let check = body
        .find("verify_admin_only_payload")
        .expect("register_cleanup_task must call the location check");
    let scheduler = body
        .find("let service = connect()?;")
        .expect("register_cleanup_task connects to Task Scheduler");
    assert!(
        check < scheduler,
        "the location check must precede the Task Scheduler connection \
         inside register_cleanup_task"
    );
}

/// C9 (ordering): `--register` verifies the DLL(s) before any registry write
/// (`sakura_reg::register_all`). Scoped to the `register` function body.
#[test]
fn register_verifies_the_dll_before_any_hklm_write() {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../sakura-regtool/src/main.rs"),
    )
    .expect("read regtool main.rs");
    let body = function_body(&source, "fn register(");
    let check = body
        .find("verify_admin_only_payload")
        .expect("register must call the location check");
    let write = body
        .find("sakura_reg::register_all")
        .expect("register writes the registration");
    assert!(
        check < write,
        "the DLL location check must precede the HKLM write inside register"
    );
}

/// The slice of `source` between `signature` and the next top-level item, so
/// ordering assertions cannot pass on a call that lives in a different
/// function.
fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("{signature} must exist"));
    let after = start + signature.len();
    let rest = &source[after..];
    // The next top-level item ends the slice: whichever of these markers
    // comes first, not whichever happens to match at all.
    let end = [
        "\nfn ",
        "\npub fn ",
        "\n#[cfg(",
        "\nmod ",
        "\nstruct ",
        "\nenum ",
    ]
    .iter()
    .filter_map(|marker| rest.find(marker))
    .min()
    .map(|offset| after + offset)
    .unwrap_or(source.len());
    &source[start..end]
}

#[test]
fn function_body_stops_at_the_next_top_level_item() {
    let source =
        "fn a() {\n    one();\n}\npub fn b() {\n    two();\n}\nfn c() {\n    three();\n}\n";
    assert_eq!(
        function_body(source, "fn a"),
        "fn a() {\n    one();\n}",
        "a following pub fn must end the slice"
    );
    assert_eq!(
        function_body(source, "pub fn b"),
        "pub fn b() {\n    two();\n}",
        "a following fn must end the slice"
    );
    let with_attrs = "fn a() {\n    one();\n}\n#[cfg(test)]\nmod tests {}\n";
    assert_eq!(
        function_body(with_attrs, "fn a"),
        "fn a() {\n    one();\n}",
        "a following cfg/test module must end the slice"
    );
}

/// C10: the installer guards the location in `PrepareToInstall` before the
/// existing `{app}\sakura_regtool.exe` is executed, so a silent upgrade of a
/// custom-folder install cannot run user-replaceable code.
#[test]
fn prepare_to_install_checks_the_location_before_running_regtool() {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../installer/setup.iss"),
    )
    .expect("read setup.iss");
    let prepare = source
        .find("function PrepareToInstall")
        .expect("setup.iss must define PrepareToInstall");
    let section = &source[prepare..];
    let guard = section
        .find("InstallDirIsSafe")
        .expect("PrepareToInstall must check the install location");
    let exec = section
        .find("Exec(ExistingRegTool")
        .expect("PrepareToInstall runs the existing regtool");
    assert!(
        guard < exec,
        "the location check must precede the existing regtool exec"
    );

    // Review R1: the guard must normalize `..` itself — a plain-text prefix
    // compare would let `C:\Program Files\..\SakuraInput` through. Both
    // sides of the comparison must go through `ExpandFileName`.
    let guard_start = source
        .find("function InstallDirIsSafe")
        .expect("setup.iss must define InstallDirIsSafe");
    let guard_end = source[guard_start..]
        .find("\nend;")
        .map(|offset| guard_start + offset)
        .expect("InstallDirIsSafe must terminate with end;");
    let guard_body = &source[guard_start..guard_end];
    assert_eq!(
        guard_body.matches("ExpandFileName(").count(),
        2,
        "InstallDirIsSafe must ExpandFileName-normalize both sides"
    );
}
