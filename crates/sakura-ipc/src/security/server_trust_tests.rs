//! Server-trust (#104) tests: image-path policy and named refusals.

use super::*;

#[test]
fn exact_native_policy_requires_full_utf16_identity() {
    let expected = PathBuf::from(r"\Device\HarddiskVolume7\owned\sakura_engine.exe");
    let policy = ServerTrustPolicy::ExactNative(expected.clone());
    assert!(policy.matches_image_path(&expected));
    assert!(!policy.matches_image_path(Path::new(
        r"\Device\HarddiskVolume8\owned\sakura_engine.exe"
    )));
    assert!(!policy.matches_image_path(Path::new(
        r"\Device\HarddiskVolume7\other\sakura_engine.exe"
    )));
    assert!(!policy.matches_image_path(Path::new(
        r"\device\HarddiskVolume7\owned\sakura_engine.exe"
    )));
}

#[test]
fn exact_native_policy_rejects_non_native_and_malformed_paths() {
    for rejected in [
        r"C:\owned\sakura_engine.exe",
        r"\\?\C:\owned\sakura_engine.exe",
        r"\??\C:\owned\sakura_engine.exe",
        r"\Device\sakura_engine.exe",
        r"\Device\HarddiskVolume7\owned\..\sakura_engine.exe",
        r"\Device\HarddiskVolume7\\sakura_engine.exe",
        r"\Device\HarddiskVolume7/owned/sakura_engine.exe",
        r"\Device\HarddiskVolume7\owned\sakura_engine.exe.bak",
    ] {
        let path = PathBuf::from(rejected);
        assert!(!ServerTrustPolicy::ExactNative(path.clone()).matches_image_path(&path));
    }

    let mut nul = r"\Device\HarddiskVolume7\owned\sakura_engine.exe"
        .encode_utf16()
        .collect::<Vec<_>>();
    nul.insert(nul.len() - ENGINE_IMAGE_NAME.len(), 0);
    let nul = PathBuf::from(OsString::from_wide(&nul));
    assert!(!ServerTrustPolicy::ExactNative(nul.clone()).matches_image_path(&nul));
}

#[test]
fn native_process_image_query_returns_native_namespace() {
    let process = ProcessHandle::open(std::process::id()).expect("current process");
    let native = process
        .image_path(PROCESS_NAME_NATIVE)
        .expect("native process image query");
    let units: Vec<u16> = native.as_os_str().encode_wide().collect();
    assert!(units.starts_with(&r"\Device\".encode_utf16().collect::<Vec<_>>()));
}

#[test]
fn installed_root_policy_uses_one_direct_release_directory() {
    let root = std::env::temp_dir().join(format!(
        "sakura-ipc-trust-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let engine = root
        .join("versions")
        .join("release-a")
        .join(ENGINE_IMAGE_NAME);
    std::fs::create_dir_all(engine.parent().expect("release parent")).expect("directories");
    std::fs::write(&engine, b"engine").expect("image fixture");

    let policy = ServerTrustPolicy::InstalledRoot(root.clone());
    assert!(policy.matches_image_path(&engine));
    assert!(!policy.matches_image_path(
        &root
            .join("versions")
            .join("release-a")
            .join("sakura_engine.exe.bak")
    ));
    assert!(!policy.matches_image_path(
        &root
            .join("versions")
            .join("release-a")
            .join("..")
            .join(ENGINE_IMAGE_NAME)
    ));

    let sibling = root.with_file_name(format!(
        "{}-sibling",
        root.file_name().unwrap().to_string_lossy()
    ));
    let sibling_engine = sibling
        .join("versions")
        .join("release-a")
        .join(ENGINE_IMAGE_NAME);
    std::fs::create_dir_all(sibling_engine.parent().expect("sibling parent"))
        .expect("sibling directories");
    std::fs::write(&sibling_engine, b"sibling").expect("sibling image");
    assert!(!policy.matches_image_path(&sibling_engine));

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&sibling);
}

#[test]
fn trust_policy_rejects_reparse_root_when_the_platform_allows_a_test_link() {
    let root = std::env::temp_dir().join(format!(
        "sakura-ipc-reparse-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let engine = root
        .join("versions")
        .join("release-a")
        .join(ENGINE_IMAGE_NAME);
    std::fs::create_dir_all(engine.parent().expect("release parent")).expect("directories");
    std::fs::write(&engine, b"engine").expect("image fixture");
    let alias = root.with_file_name(format!(
        "{}-alias",
        root.file_name().unwrap().to_string_lossy()
    ));
    let linked = std::os::windows::fs::symlink_dir(&root, &alias);
    if linked.is_ok() {
        let alias_engine = alias
            .join("versions")
            .join("release-a")
            .join(ENGINE_IMAGE_NAME);
        assert!(!ServerTrustPolicy::InstalledRoot(alias.clone()).matches_image_path(&alias_engine));
    }
    let _ = std::fs::remove_dir_all(&alias);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_refusal_names_the_step_that_refused() {
    // The current process is openable and its image path is readable, so
    // a policy naming something else can only fail at the comparison.
    // Issue #104 turns entirely on telling this apart from a path that
    // could not be read at all — both were `ERROR_ACCESS_DENIED` before.
    let elsewhere = std::env::temp_dir().join("not-the-engine.exe");
    assert_eq!(
        verify_server_process(std::process::id(), &ServerTrustPolicy::Exact(elsewhere)),
        Err(ServerRejection::ImagePathRejected)
    );

    // PID 0 is the idle process and can never be opened, so the refusal
    // has to be reported before any policy question is reached.
    match verify_server_process(0, &ServerTrustPolicy::Exact(PathBuf::from("x"))) {
        Err(ServerRejection::ProcessUnopenable(_)) => {}
        other => panic!("expected an unopenable process, got {other:?}"),
    }
}

#[test]
fn every_refusal_prints_a_distinct_reason() {
    // A reason that reads like another reason is worth nothing in a CI
    // log, which is the only place Issue #104 is observable.
    let code = HRESULT::from_win32(windows::Win32::Foundation::ERROR_ACCESS_DENIED.0);
    let all = [
        ServerRejection::PolicyUnavailable,
        ServerRejection::NoServerProcessId,
        ServerRejection::ProcessUnopenable(code),
        ServerRejection::ImagePathUnreadable(code),
        ServerRejection::ImagePathRejected,
        ServerRejection::TokenUnopenable(code),
        ServerRejection::TokenUnclassifiable(code),
        ServerRejection::IntegrityRejected,
    ];
    let mut printed: Vec<String> = all.iter().map(ToString::to_string).collect();
    printed.sort();
    printed.dedup();
    assert_eq!(printed.len(), all.len(), "two reasons print the same text");
    for reason in all {
        assert!(!reason.to_string().is_empty());
    }
}
