use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::Foundation::ERROR_SHARING_VIOLATION;
use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);
// SHA-256("fake installer"), the bounded payload written by
// `FakeTransport`. Keeping the fake payload self-consistent exercises the
// same handle-backed size/hash checks as the real download path.
const FAKE_INSTALLER: &[u8] = b"fake installer";
const DIGEST: [u8; 32] = [
    0x94, 0x1e, 0xf2, 0xfd, 0x24, 0x9e, 0x8e, 0x35, 0x35, 0x90, 0x8e, 0x36, 0x63, 0x51, 0x5a, 0x85,
    0xa2, 0x91, 0xc5, 0x38, 0x01, 0x6f, 0x75, 0xbe, 0x86, 0x03, 0x2d, 0xa4, 0x73, 0x02, 0x9b, 0x3e,
];

fn manifest(version: Version) -> ReleaseManifest {
    ReleaseManifest {
        trust_epoch: 1,
        release_sequence: crate::update_trust::embedded_sequence_floor_for_test(),
        version,
        source_commit: "0000000000000000000000000000000000000000".to_owned(),
        installer_url: installer_url_for(version),
        sha256: DIGEST,
        size: FAKE_INSTALLER.len() as u64,
        authenticode: AuthenticodePolicy::Required,
        minimum_updater_version: Version {
            major: 1,
            minor: 0,
            patch: 0,
        },
        expires_unix: u64::MAX,
    }
}

fn temp_paths(name: &str) -> UpdatePaths {
    let id = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("sakura-updater-{}-{name}-{id}", std::process::id()));
    UpdatePaths {
        installer: root.join("sakura_setup.pending.exe"),
        log: root.join("install.log"),
    }
}

fn signing_fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../verification/fixtures/update-signing-v2")
            .join(name),
    )
    .unwrap()
}

#[derive(Debug)]
struct FakeTransport {
    manifest: Result<Vec<u8>, String>,
    signature: Result<Vec<u8>, String>,
    receipt: Result<DownloadReceipt, String>,
    payload: Vec<u8>,
    manifest_calls: usize,
    signature_calls: usize,
    download_calls: usize,
}

impl FakeTransport {
    fn success(release: &ReleaseManifest) -> Self {
        Self {
            manifest: Ok(release.canonical_text().into_bytes()),
            signature: Ok(b"test signature envelope".to_vec()),
            receipt: Ok(DownloadReceipt {
                size: release.size,
                sha256: release.sha256,
            }),
            payload: FAKE_INSTALLER.to_vec(),
            manifest_calls: 0,
            signature_calls: 0,
            download_calls: 0,
        }
    }
}

impl UpdateTransport for FakeTransport {
    fn fetch_manifest(&mut self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        assert_eq!(url, MANIFEST_URL);
        assert_eq!(limit, MAX_MANIFEST_BYTES);
        self.manifest_calls += 1;
        self.manifest.clone()
    }

    fn fetch_signature(&mut self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        assert_eq!(url, SIGNATURE_URL);
        assert_eq!(limit, MAX_SIGNATURE_BYTES);
        self.signature_calls += 1;
        self.signature.clone()
    }

    fn download_installer(
        &mut self,
        _url: &str,
        path: &Path,
        limit: u64,
    ) -> Result<DownloadReceipt, String> {
        assert_eq!(limit, MAX_INSTALLER_BYTES);
        self.download_calls += 1;
        fs::write(path, &self.payload).map_err(|error| error.to_string())?;
        self.receipt.clone()
    }
}

#[derive(Debug)]
struct FakeManifestVerifier {
    result: Result<[u8; 32], String>,
    calls: usize,
}

impl ManifestVerifier for FakeManifestVerifier {
    fn verify(
        &mut self,
        _manifest_bytes: &[u8],
        _manifest: &ReleaseManifest,
        _signature_bytes: &[u8],
    ) -> Result<[u8; 32], String> {
        self.calls += 1;
        self.result.clone()
    }
}

fn fake_manifest_verifier() -> FakeManifestVerifier {
    FakeManifestVerifier {
        result: Ok([0x55; 32]),
        calls: 0,
    }
}

#[derive(Debug)]
struct FakeVerifier {
    result: Result<AuthenticodeStatus, String>,
    calls: usize,
}

impl SignatureVerifier for FakeVerifier {
    fn verify(&mut self, _path: &Path) -> Result<AuthenticodeStatus, String> {
        self.calls += 1;
        self.result.clone()
    }
}

#[derive(Debug)]
struct FakeRunner {
    result: Result<InstallerTerminal, String>,
    calls: usize,
}

impl InstallerRunner for FakeRunner {
    fn run(&mut self, _installer: &Path, _log: &Path) -> Result<InstallerTerminal, String> {
        self.calls += 1;
        self.result.clone()
    }
}

#[derive(Debug)]
struct GuardProbeRunner {
    result: InstallerTerminal,
    write_rejected: bool,
    rename_rejected: bool,
    rename_error_code: Option<i32>,
    delete_rejected: bool,
}

impl InstallerRunner for GuardProbeRunner {
    fn run(&mut self, installer: &Path, _log: &Path) -> Result<InstallerTerminal, String> {
        let mut write_options = OpenOptions::new();
        write_options.write(true);
        self.write_rejected = write_options.open(installer).is_err();

        let replacement = installer.with_file_name("replacement.exe");
        fs::write(&replacement, b"replacement")
            .map_err(|error| format!("could not stage replacement test file: {error}"))?;
        let replacement_wide = wide_os(replacement.as_os_str());
        let installer_wide = wide_os(installer.as_os_str());
        // SAFETY: both paths are live NUL-terminated UTF-16 buffers for
        // the duration of the call; the test owns both files.
        let replace_result = unsafe {
            ReplaceFileW(
                PCWSTR(installer_wide.as_ptr()),
                PCWSTR(replacement_wide.as_ptr()),
                PCWSTR::null(),
                REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
        };
        self.rename_rejected = replace_result.is_err();
        self.rename_error_code = replace_result.err().map(|error| error.code().0);
        self.delete_rejected = fs::remove_file(installer).is_err();
        let _ = fs::remove_file(replacement);
        Ok(self.result)
    }
}

fn run_fake(
    release: &ReleaseManifest,
    receipt: DownloadReceipt,
    authenticode: Result<AuthenticodeStatus, String>,
    terminal: InstallerTerminal,
) -> (
    UpdateOutcome,
    FakeTransport,
    FakeManifestVerifier,
    FakeVerifier,
    FakeRunner,
    UpdatePaths,
) {
    let paths = temp_paths("pipeline");
    let mut transport = FakeTransport::success(release);
    transport.receipt = Ok(receipt);
    let mut manifest_verifier = fake_manifest_verifier();
    let mut verifier = FakeVerifier {
        result: authenticode,
        calls: 0,
    };
    let mut runner = FakeRunner {
        result: Ok(terminal),
        calls: 0,
    };
    let outcome = apply_update(
        true,
        Version {
            major: 1,
            minor: 0,
            patch: 0,
        },
        &paths,
        &mut transport,
        &mut manifest_verifier,
        &mut verifier,
        &mut runner,
    );
    (
        outcome,
        transport,
        manifest_verifier,
        verifier,
        runner,
        paths,
    )
}

#[test]
fn versions_are_strict_and_ordered() {
    assert_eq!(
        Version::parse("1.2.3").map(|value| value.to_string()),
        Ok("1.2.3".to_owned())
    );
    assert!(Version::parse("01.2.3").is_err());
    assert!(Version::parse("1.2").is_err());
    assert!(Version::parse("1.2.3-beta").is_err());
    assert!(Version::parse("1.2.3.4").is_err());
    assert!(Version::parse("2.0.0").unwrap() > Version::parse("1.99.99").unwrap());
}

#[test]
fn canonical_manifest_roundtrips_and_rejects_ambiguity() {
    let release = manifest(Version {
        major: 1,
        minor: 2,
        patch: 3,
    });
    assert_eq!(
        ReleaseManifest::parse(release.canonical_text().as_bytes()),
        Ok(release.clone())
    );
    let mut wrong_url = release.canonical_text();
    wrong_url = wrong_url.replace("sakura_setup.exe", "other.exe");
    assert!(ReleaseManifest::parse(wrong_url.as_bytes()).is_err());
    let duplicate = format!("{}schema=1\n", release.canonical_text());
    assert!(ReleaseManifest::parse(duplicate.as_bytes()).is_err());
    let uppercase = release.canonical_text().replace("941e", "941E");
    assert!(ReleaseManifest::parse(uppercase.as_bytes()).is_err());
    let unknown = release
        .canonical_text()
        .replace(&format!("size={}", FAKE_INSTALLER.len()), "bytes=123");
    assert!(ReleaseManifest::parse(unknown.as_bytes()).is_err());
}

#[test]
fn update_preference_defaults_on_and_roundtrips_strictly() {
    let paths = temp_paths("preference");
    let path = paths.installer.with_file_name("settings.txt");
    assert!(UpdatePreferences::default().enabled);
    assert_eq!(
        UpdatePreferences::load(&path).unwrap(),
        UpdatePreferences::default()
    );
    UpdatePreferences { enabled: true }.save(&path).unwrap();
    assert_eq!(
        UpdatePreferences::load(&path).unwrap(),
        UpdatePreferences { enabled: true }
    );
    fs::write(&path, b"schema=1\nenabled=true\nunknown=x\n").unwrap();
    assert_eq!(
        UpdatePreferences::load(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn disabled_check_makes_no_network_request() {
    let release = manifest(Version {
        major: 2,
        minor: 0,
        patch: 0,
    });
    let mut transport = FakeTransport::success(&release);
    let paths = temp_paths("disabled-check");
    let mut manifest_verifier = fake_manifest_verifier();
    assert_eq!(
        check_for_update(
            false,
            Version::default(),
            &paths,
            &mut transport,
            &mut manifest_verifier,
        ),
        UpdateCheckOutcome::Disabled
    );
    assert_eq!(transport.manifest_calls, 0);
    assert_eq!(transport.signature_calls, 0);
    assert_eq!(transport.download_calls, 0);
    assert_eq!(manifest_verifier.calls, 0);
    assert!(!paths.installer.parent().unwrap().exists());
}

#[test]
fn check_pipeline_rejects_signed_fixture_below_embedded_floor_before_signature_download() {
    let manifest_bytes = signing_fixture("manifest-positive.txt");
    let release = manifest(Version::parse("1.0.34").unwrap());
    let paths = temp_paths("positive-fixture");
    let mut transport = FakeTransport::success(&release);
    transport.manifest = Ok(manifest_bytes);
    transport.signature = Ok(signing_fixture("signature-positive.txt"));
    let mut verifier = fake_manifest_verifier();

    let outcome = check_for_update_at(
        Version::parse("1.0.33").unwrap(),
        &paths,
        &mut transport,
        &mut verifier,
        1_800_000_000,
    );

    assert!(matches!(
        outcome,
        UpdateCheckOutcome::Failed(UpdateFailure {
            stage: UpdateStage::ManifestValidation,
            message,
        }) if message.contains("below the embedded floor")
    ));
    assert_eq!(transport.manifest_calls, 1);
    assert_eq!(transport.signature_calls, 0);
    assert_eq!(transport.download_calls, 0);
    assert_eq!(verifier.calls, 0);
    let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
}

#[test]
fn hash_and_signature_failures_never_reach_installer() {
    let release = manifest(Version {
        major: 2,
        minor: 0,
        patch: 0,
    });
    let (hash_outcome, _, _, hash_verifier, hash_runner, hash_paths) = run_fake(
        &release,
        DownloadReceipt {
            size: release.size,
            sha256: [0; 32],
        },
        Ok(AuthenticodeStatus::Trusted),
        InstallerTerminal::Installed,
    );
    assert!(matches!(
        hash_outcome,
        UpdateOutcome::Failed {
            failure: UpdateFailure {
                stage: UpdateStage::InstallerHash,
                ..
            },
            ..
        }
    ));
    assert_eq!(hash_verifier.calls, 0);
    assert_eq!(hash_runner.calls, 0);
    assert!(!hash_paths.installer.exists());

    let (signature_outcome, _, _, verifier, runner, signature_paths) = run_fake(
        &release,
        DownloadReceipt {
            size: release.size,
            sha256: release.sha256,
        },
        Err("untrusted".to_owned()),
        InstallerTerminal::Installed,
    );
    assert!(matches!(
        signature_outcome,
        UpdateOutcome::Failed {
            failure: UpdateFailure {
                stage: UpdateStage::SignatureVerification,
                ..
            },
            ..
        }
    ));
    assert_eq!(verifier.calls, 1);
    assert_eq!(runner.calls, 0);
    assert!(!signature_paths.installer.exists());
    let _ = fs::remove_dir_all(hash_paths.installer.parent().unwrap());
    let _ = fs::remove_dir_all(signature_paths.installer.parent().unwrap());
}

#[test]
fn manifest_signature_failure_happens_before_installer_download() {
    let release = manifest(Version {
        major: 2,
        minor: 0,
        patch: 0,
    });
    let paths = temp_paths("manifest-signature-failure");
    let mut transport = FakeTransport::success(&release);
    let mut manifest_verifier = FakeManifestVerifier {
        result: Err("tampered manifest".to_owned()),
        calls: 0,
    };
    let mut verifier = FakeVerifier {
        result: Ok(AuthenticodeStatus::Trusted),
        calls: 0,
    };
    let mut runner = FakeRunner {
        result: Ok(InstallerTerminal::Installed),
        calls: 0,
    };

    let outcome = apply_update(
        true,
        Version {
            major: 1,
            minor: 0,
            patch: 0,
        },
        &paths,
        &mut transport,
        &mut manifest_verifier,
        &mut verifier,
        &mut runner,
    );

    assert!(matches!(
        outcome,
        UpdateOutcome::Failed {
            failure: UpdateFailure {
                stage: UpdateStage::ManifestValidation,
                ..
            },
            ..
        }
    ));
    assert_eq!(transport.manifest_calls, 1);
    assert_eq!(transport.signature_calls, 1);
    assert_eq!(transport.download_calls, 0);
    assert_eq!(manifest_verifier.calls, 1);
    assert_eq!(verifier.calls, 0);
    assert_eq!(runner.calls, 0);
    assert!(!paths.installer.exists());
    let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
}

#[test]
fn authenticode_policy_matrix_rejects_both_reverse_combinations() {
    for (policy, status, accepted) in [
        (
            AuthenticodePolicy::Required,
            AuthenticodeStatus::Trusted,
            true,
        ),
        (
            AuthenticodePolicy::Required,
            AuthenticodeStatus::Unsigned,
            false,
        ),
        (
            AuthenticodePolicy::Unsigned,
            AuthenticodeStatus::Unsigned,
            true,
        ),
        (
            AuthenticodePolicy::Unsigned,
            AuthenticodeStatus::Trusted,
            false,
        ),
    ] {
        let mut release = manifest(Version {
            major: 2,
            minor: 0,
            patch: 0,
        });
        release.authenticode = policy;
        let (outcome, _, _, verifier, runner, paths) = run_fake(
            &release,
            DownloadReceipt {
                size: release.size,
                sha256: release.sha256,
            },
            Ok(status),
            InstallerTerminal::Installed,
        );
        if accepted {
            assert!(matches!(outcome, UpdateOutcome::Installed { .. }));
            assert_eq!(runner.calls, 1);
        } else {
            assert!(matches!(
                outcome,
                UpdateOutcome::Failed {
                    failure: UpdateFailure {
                        stage: UpdateStage::SignatureVerification,
                        ..
                    },
                    ..
                }
            ));
            assert_eq!(runner.calls, 0);
        }
        assert_eq!(verifier.calls, 1);
        let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
    }
}

#[test]
fn concurrent_apply_is_single_flight_and_never_calls_runner() {
    let release = manifest(Version {
        major: 2,
        minor: 0,
        patch: 0,
    });
    let paths = temp_paths("single-flight");
    let held = acquire_apply_lock(&paths.installer, Duration::ZERO).unwrap();
    let mut transport = FakeTransport::success(&release);
    let mut manifest_verifier = fake_manifest_verifier();
    let mut verifier = FakeVerifier {
        result: Ok(AuthenticodeStatus::Trusted),
        calls: 0,
    };
    let mut runner = FakeRunner {
        result: Ok(InstallerTerminal::Installed),
        calls: 0,
    };

    let outcome = apply_update(
        true,
        Version {
            major: 1,
            minor: 0,
            patch: 0,
        },
        &paths,
        &mut transport,
        &mut manifest_verifier,
        &mut verifier,
        &mut runner,
    );

    assert!(matches!(
        outcome,
        UpdateOutcome::Failed {
            version: None,
            failure: UpdateFailure {
                stage: UpdateStage::InstallerPreparation,
                ..
            }
        }
    ));
    assert_eq!(transport.manifest_calls, 0);
    assert_eq!(transport.signature_calls, 0);
    assert_eq!(transport.download_calls, 0);
    assert_eq!(manifest_verifier.calls, 0);
    assert_eq!(verifier.calls, 0);
    assert_eq!(runner.calls, 0);
    drop(held);
    let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
}

#[test]
fn installer_success_restart_timeout_and_failure_are_distinct_terminals() {
    let release = manifest(Version {
        major: 2,
        minor: 0,
        patch: 0,
    });
    let terminals = [
        (InstallerTerminal::Installed, "installed"),
        (InstallerTerminal::RestartRequired, "restart"),
        (InstallerTerminal::TimedOutStillRunning, "timeout"),
        (InstallerTerminal::Failed(17), "failure"),
    ];
    for (terminal, expected) in terminals {
        let (outcome, _, _, verifier, runner, paths) = run_fake(
            &release,
            DownloadReceipt {
                size: release.size,
                sha256: release.sha256,
            },
            Ok(AuthenticodeStatus::Trusted),
            terminal,
        );
        assert_eq!(verifier.calls, 1);
        assert_eq!(runner.calls, 1);
        match expected {
            "installed" => assert!(matches!(outcome, UpdateOutcome::Installed { .. })),
            "restart" => assert!(matches!(outcome, UpdateOutcome::RestartRequired { .. })),
            "timeout" => {
                assert!(matches!(
                    outcome,
                    UpdateOutcome::TimedOutStillRunning { .. }
                ));
                assert!(paths.installer.exists());
            }
            "failure" => assert!(matches!(
                outcome,
                UpdateOutcome::Failed {
                    failure: UpdateFailure {
                        stage: UpdateStage::InstallerExit,
                        ..
                    },
                    ..
                }
            )),
            _ => unreachable!(),
        }
        let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
    }
}

#[test]
fn installer_guard_blocks_write_rename_and_delete_until_runner_returns() {
    let release = manifest(Version {
        major: 2,
        minor: 0,
        patch: 0,
    });
    let paths = temp_paths("share-guard");
    let mut transport = FakeTransport::success(&release);
    let mut manifest_verifier = fake_manifest_verifier();
    let mut verifier = FakeVerifier {
        result: Ok(AuthenticodeStatus::Trusted),
        calls: 0,
    };
    let mut runner = GuardProbeRunner {
        result: InstallerTerminal::Installed,
        write_rejected: false,
        rename_rejected: false,
        rename_error_code: None,
        delete_rejected: false,
    };

    let outcome = apply_update(
        true,
        Version {
            major: 1,
            minor: 0,
            patch: 0,
        },
        &paths,
        &mut transport,
        &mut manifest_verifier,
        &mut verifier,
        &mut runner,
    );

    assert!(matches!(outcome, UpdateOutcome::Installed { .. }));
    assert!(runner.write_rejected, "guard must deny a writer handle");
    assert!(runner.rename_rejected, "guard must deny replacement/rename");
    assert_eq!(
        runner.rename_error_code,
        Some(windows::core::HRESULT::from_win32(ERROR_SHARING_VIOLATION.0).0),
        "replacement must fail at the file-sharing boundary"
    );
    assert!(runner.delete_rejected, "guard must deny delete");
    assert!(
        !paths.installer.exists(),
        "cleanup runs after guard release"
    );

    // The equivalent replacement succeeds after the guard is released;
    // this distinguishes the protection from MoveFileExW's normal
    // replace-existing behavior.
    let control_replacement = paths.installer.with_file_name("control.exe");
    fs::write(&paths.installer, b"original").unwrap();
    fs::write(&control_replacement, b"replacement").unwrap();
    let control_replacement_wide = wide_os(control_replacement.as_os_str());
    let installer_wide = wide_os(paths.installer.as_os_str());
    // SAFETY: both paths are live NUL-terminated UTF-16 buffers and the
    // test owns the two files being atomically replaced.
    unsafe {
        ReplaceFileW(
            PCWSTR(installer_wide.as_ptr()),
            PCWSTR(control_replacement_wide.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
        .expect("replacement succeeds once the guard is released");
    }
    assert_eq!(fs::read(&paths.installer).unwrap(), b"replacement");
    let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
}

#[test]
fn handle_backed_hash_failure_stops_verifier_and_runner() {
    let release = manifest(Version {
        major: 2,
        minor: 0,
        patch: 0,
    });
    let paths = temp_paths("handle-hash-failure");
    let mut transport = FakeTransport::success(&release);
    // Keep the transport receipt honest to the manifest while changing
    // the bytes written at the staged path. This exercises the check that
    // cannot be satisfied by receipt metadata alone.
    transport.payload = b"fake installER".to_vec();
    let mut manifest_verifier = fake_manifest_verifier();
    let mut verifier = FakeVerifier {
        result: Ok(AuthenticodeStatus::Trusted),
        calls: 0,
    };
    let mut runner = FakeRunner {
        result: Ok(InstallerTerminal::Installed),
        calls: 0,
    };

    let outcome = apply_update(
        true,
        Version {
            major: 1,
            minor: 0,
            patch: 0,
        },
        &paths,
        &mut transport,
        &mut manifest_verifier,
        &mut verifier,
        &mut runner,
    );

    assert!(matches!(
        outcome,
        UpdateOutcome::Failed {
            failure: UpdateFailure {
                stage: UpdateStage::InstallerHash,
                ..
            },
            ..
        }
    ));
    assert_eq!(verifier.calls, 0);
    assert_eq!(runner.calls, 0);
    assert!(!paths.installer.exists());
    let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
}

#[test]
fn url_parser_bounds_redirects_to_https_github_hosts() {
    assert!(parse_allowed_https_url(MANIFEST_URL).is_ok());
    assert!(parse_allowed_https_url(SIGNATURE_URL).is_ok());
    assert!(parse_allowed_https_url(
        "https://release-assets.githubusercontent.com/github-production-release-asset/x?token=y"
    )
    .is_ok());
    assert!(parse_allowed_https_url("http://github.com/x").is_err());
    assert!(parse_allowed_https_url("https://github.com.evil.example/x").is_err());
    assert!(parse_allowed_https_url("https://user@github.com/x").is_err());
    assert!(resolve_redirect(
        &parse_allowed_https_url(MANIFEST_URL).unwrap(),
        "//evil.example/x"
    )
    .is_err());
}

#[test]
fn authenticode_rejects_an_unsigned_file() {
    let paths = temp_paths("unsigned-authenticode");
    fs::create_dir_all(paths.installer.parent().unwrap()).unwrap();
    fs::write(&paths.installer, b"not a PE image and not signed").unwrap();

    let mut verifier = AuthenticodeVerifier;
    let result = verifier.verify(&paths.installer);

    assert!(result.is_err(), "an unsigned file must fail closed");
    let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
}

#[test]
fn authenticode_classification_accepts_only_success_or_exact_no_signature() {
    assert_eq!(
        classify_authenticode_status(0, 0),
        Ok(AuthenticodeStatus::Trusted)
    );
    assert_eq!(
        classify_authenticode_status(TRUST_E_NOSIGNATURE.0, 0),
        Ok(AuthenticodeStatus::Unsigned)
    );
    assert!(classify_authenticode_status(TRUST_E_NOSIGNATURE.0 + 1, 0).is_err());
    assert!(classify_authenticode_status(0, 1).is_err());
}

#[test]
fn cng_sha256_matches_the_published_test_vector() {
    let mut hash = Sha256::new().unwrap();
    hash.update(b"a").unwrap();
    hash.update(b"bc").unwrap();
    assert_eq!(
        encode_hex(&hash.finish().unwrap()),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
