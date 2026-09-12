use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);
const FIXTURE_TIME: u64 = 1_800_000_000;

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../verification/fixtures/update-signing-v2")
            .join(name),
    )
    .unwrap()
}

fn temp_installer(name: &str) -> PathBuf {
    let id = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "sakura-update-trust-{}-{name}-{id}",
            std::process::id()
        ))
        .join("sakura_setup.pending.exe")
}

#[test]
fn embedded_sequence_floor_is_single_line_canonical_and_nonzero() {
    let floor = embedded_sequence_floor().unwrap();
    assert!(floor > 0);
    assert_eq!(RELEASE_SEQUENCE_BYTES, format!("{floor}\n").as_bytes());

    let malformed: &[&[u8]] = &[
        b"",
        b"1",
        b"\xef\xbb\xbf1\n",
        b"1\r\n",
        b"1\n2\n",
        b"1\n\n",
        b"01\n",
        b"0\n",
        b"+1\n",
        b" 1\n",
        b"1 \n",
        b"18446744073709551616\n",
        &[0xff, b'\n'],
    ];
    for bytes in malformed {
        assert!(parse_release_sequence_floor(bytes).is_err());
    }
}

#[test]
fn positive_fixture_verifies_with_embedded_cng_key() {
    let manifest_bytes = fixture("manifest-positive.txt");
    let envelope_bytes = fixture("signature-positive.txt");
    // This immutable public cryptographic vector is sequence 1 by design.
    // Parse it against its frozen floor; production parsing still uses the
    // compiled floor through `ReleaseManifest::parse`.
    let manifest = ReleaseManifest::parse_with_sequence_floor(&manifest_bytes, 1).unwrap();
    manifest
        .validate_runtime(Version::parse("1.0.33").unwrap(), FIXTURE_TIME)
        .unwrap();
    assert_eq!(
        encode_hex(&verify_signed_manifest(&manifest_bytes, &manifest, &envelope_bytes).unwrap()),
        "b90f4862b54c5643fac5f0188d2dbd0fae79feb7975f18620fdd731b33978340"
    );
    assert_eq!(embedded_keyring().unwrap().keys.len(), 2);
}

#[test]
fn canonical_manifest_rejects_identity_policy_runtime_and_byte_ambiguity() {
    let bytes = fixture("manifest-positive.txt");
    let source = String::from_utf8(bytes.clone()).unwrap();
    for changed in [
        source.replace("schema=2", "schema=1"),
        source.replace("product=sakura-input", "product=other"),
        source.replace(
            "repository=tsuyoshi-otake/sakura-input",
            "repository=other/repo",
        ),
        source.replace("channel=stable", "channel=preview"),
        source.replace("platform=windows-x86_64", "platform=windows-arm64"),
        source.replace("trust_epoch=1", "trust_epoch=2"),
        source.replace("release_sequence=1", "release_sequence=0"),
        source.replace("tag=v1.0.33", "tag=v1.0.34"),
        source.replace("asset_name=sakura_setup.exe", "asset_name=other.exe"),
        source.replace("v1.0.33/sakura_setup.exe", "v1.0.34/sakura_setup.exe"),
        source.replace(
            "source_commit=964baf06d0ac451e319a847a2aabe796df7ea95f",
            "source_commit=NOT-A-COMMIT",
        ),
        source.replace(
            "sha256=3b5eb14b11efbd16920de1e0dd80d161adafb35d858c22bbcff18edf4a0367c5",
            "sha256=BAD",
        ),
        source.replace("size=56", "size=0"),
        source.replace("authenticode=unsigned", "authenticode=optional"),
        source.replace(
            "minimum_updater_version=1.0.33",
            "minimum_updater_version=01.0.33",
        ),
        source.replace("expires_unix=1893456000\n", ""),
        source.replace("size=56\n", "size=56\nsize=56\n"),
        format!("{source}comment=untrusted\n"),
        source.replace("product=sakura-input", "product=sakura-input "),
    ] {
        assert!(ReleaseManifest::parse_with_sequence_floor(changed.as_bytes(), 1).is_err());
    }
    let manifest = ReleaseManifest::parse_with_sequence_floor(&bytes, 1).unwrap();
    assert!(manifest
        .validate_runtime(Version::parse("1.0.32").unwrap(), FIXTURE_TIME)
        .is_err());
    assert!(manifest
        .validate_runtime(Version::parse("1.0.33").unwrap(), manifest.expires_unix)
        .is_err());

    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend_from_slice(&bytes);
    assert!(ReleaseManifest::parse_with_sequence_floor(&bom, 1).is_err());
    assert!(ReleaseManifest::parse_with_sequence_floor(&bytes[..bytes.len() - 1], 1).is_err());
    assert!(ReleaseManifest::parse_with_sequence_floor(
        &source.replace('\n', "\r\n").into_bytes(),
        1
    )
    .is_err());
    let mut reordered: Vec<_> = source.lines().collect();
    reordered.swap(1, 2);
    assert!(ReleaseManifest::parse_with_sequence_floor(
        format!("{}\n", reordered.join("\n")).as_bytes(),
        1
    )
    .is_err());
}

#[test]
fn envelope_rejects_tamper_unknown_duplicate_unsorted_and_malformed_records() {
    let manifest_bytes = fixture("manifest-positive.txt");
    let manifest = ReleaseManifest::parse_with_sequence_floor(&manifest_bytes, 1).unwrap();
    let envelope = String::from_utf8(fixture("signature-positive.txt")).unwrap();
    let tampered = fixture("manifest-tampered.txt");
    let tampered_manifest = ReleaseManifest::parse_with_sequence_floor(&tampered, 1).unwrap();
    assert!(verify_signed_manifest(&tampered, &tampered_manifest, envelope.as_bytes()).is_err());

    let unknown = envelope.replace(
        "178bc99d4699cde4b78c0169655d3b165140a62173812867a8a66b1a608b6c47",
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    );
    assert!(verify_signed_manifest(&manifest_bytes, &manifest, unknown.as_bytes()).is_err());

    let record = envelope
        .lines()
        .find(|line| line.starts_with("signature.0="))
        .unwrap();
    let duplicate = envelope
        .replace("signature_count=1", "signature_count=2")
        .replace(
            record,
            &format!("{record}\n{}", record.replace("signature.0", "signature.1")),
        );
    assert!(SignatureEnvelope::parse(duplicate.as_bytes()).is_err());

    let (_, signature) = record.split_once(':').unwrap();
    let unsorted = format!(
        concat!(
            "schema=1\nalgorithm=ecdsa-p256-sha256-p1363\n",
            "manifest_sha256=b90f4862b54c5643fac5f0188d2dbd0fae79feb7975f18620fdd731b33978340\n",
            "signature_count=2\n",
            "signature.0=44e075680f1155c911119d9e039858a828757e37b901c2afe49fde3c4a0af92f:{}\n",
            "signature.1=178bc99d4699cde4b78c0169655d3b165140a62173812867a8a66b1a608b6c47:{}\n"
        ),
        signature, signature
    );
    assert!(SignatureEnvelope::parse(unsorted.as_bytes()).is_err());
    let dual_with_invalid_standby = envelope
            .replace("signature_count=1", "signature_count=2")
            .replace(
                record,
                &format!(
                    "{record}\nsignature.1=44e075680f1155c911119d9e039858a828757e37b901c2afe49fde3c4a0af92f:{signature}"
                ),
            );
    assert!(SignatureEnvelope::parse(dual_with_invalid_standby.as_bytes()).is_ok());
    assert!(verify_signed_manifest(
        &manifest_bytes,
        &manifest,
        dual_with_invalid_standby.as_bytes()
    )
    .is_err());
    assert!(SignatureEnvelope::parse(
        envelope
            .replace("signature_count=1", "signature_count=0")
            .as_bytes()
    )
    .is_err());
    assert!(SignatureEnvelope::parse(
        envelope
            .replace("signature_count=1", "signature_count=4")
            .as_bytes()
    )
    .is_err());
    assert!(SignatureEnvelope::parse(envelope.replace("dfdc", "DFDC").as_bytes()).is_err());
    assert!(SignatureEnvelope::parse(
            envelope
                .replace(
                    "dfdcffde2288af4ca6140a5103cb2b3ead1c60f890e988b050cdf72f7d79027521b840882b3281a39e020ca97a9655ba086534f79b62209e84cfa1f7ebbfa742",
                    "3006020101020101",
                )
                .as_bytes()
        )
        .is_err());
    assert!(SignatureEnvelope::parse(envelope.trim_end().as_bytes()).is_err());
}

#[test]
fn trust_state_rejects_rollback_replay_equivocation_corruption_and_lock_contention() {
    let installer = temp_installer("state");
    let paths = TrustPaths::adjacent_to(&installer).unwrap();
    let current = Version::parse("1.0.33").unwrap();
    let mut manifest =
        ReleaseManifest::parse_with_sequence_floor(&fixture("manifest-positive.txt"), 1).unwrap();
    manifest.release_sequence = embedded_sequence_floor().unwrap();
    let manifest_bytes = manifest.canonical_text().into_bytes();
    let digest = sha256_bytes(&manifest_bytes).unwrap();
    assert_eq!(
        authorize_manifest(
            &installer,
            current,
            &manifest,
            digest,
            Duration::from_secs(1)
        )
        .unwrap(),
        TrustDecision::Current
    );
    assert_eq!(
        authorize_manifest(
            &installer,
            current,
            &manifest,
            digest,
            Duration::from_secs(1)
        )
        .unwrap(),
        TrustDecision::Current
    );
    manifest.sha256[0] ^= 1;
    let different_digest = sha256_bytes(manifest.canonical_text().as_bytes()).unwrap();
    assert!(authorize_manifest(
        &installer,
        current,
        &manifest,
        different_digest,
        Duration::from_secs(1)
    )
    .is_err());

    fs::write(&paths.state, b"corrupt\n").unwrap();
    assert!(authorize_manifest(
        &installer,
        current,
        &manifest,
        digest,
        Duration::from_secs(1)
    )
    .is_err());

    fs::write(
        &paths.state,
        vec![b'x'; (MAX_TRUST_STATE_BYTES + 1) as usize],
    )
    .unwrap();
    assert!(authorize_manifest(
        &installer,
        current,
        &manifest,
        digest,
        Duration::from_secs(1)
    )
    .unwrap_err()
    .contains("exceeds"));

    let _held = acquire_exclusive_lock(&paths.state_lock, Duration::from_secs(1)).unwrap();
    assert!(acquire_exclusive_lock(&paths.state_lock, Duration::from_millis(20)).is_err());
    drop(_held);
    let _ = fs::remove_dir_all(installer.parent().unwrap());
}

#[test]
fn a_trust_state_below_the_embedded_floor_is_treated_as_absent_and_rebuilt() {
    // Reproduces the field report behind #150: a machine whose last
    // updater-driven check ran on 1.0.36 keeps `highest_sequence` at that
    // release's sequence forever, because installing later versions by hand
    // never advances the file. Every subsequent build embeds a higher floor,
    // so the state can never catch up on its own.
    let installer = temp_installer("stale-floor");
    let paths = TrustPaths::adjacent_to(&installer).unwrap();
    let floor = embedded_sequence_floor().unwrap();
    fs::create_dir_all(paths.state.parent().unwrap()).unwrap();
    let stale = TrustState {
        trust_epoch: EMBEDDED_TRUST_EPOCH,
        highest_sequence: floor - 1,
        highest_version: Version::parse("1.0.36").unwrap(),
        manifest_sha256: [7u8; 32],
    };
    fs::write(&paths.state, stale.canonical_text().as_bytes()).unwrap();
    assert!(!stale.bounds_future_manifests(floor));

    let mut manifest =
        ReleaseManifest::parse_with_sequence_floor(&fixture("manifest-positive.txt"), 1).unwrap();
    manifest.release_sequence = floor;
    manifest.version = Version::parse("1.0.39").unwrap();
    manifest.installer_url = installer_url_for(manifest.version);
    let digest = sha256_bytes(manifest.canonical_text().as_bytes()).unwrap();
    let current = Version::parse("1.0.39").unwrap();

    assert_eq!(
        authorize_manifest(
            &installer,
            current,
            &manifest,
            digest,
            Duration::from_secs(1)
        )
        .unwrap(),
        TrustDecision::Current
    );

    // The unusable state was replaced by one derived from the
    // signature-verified manifest, so it now sits at the embedded floor and
    // resumes bounding replays.
    let rebuilt = TrustState::parse(&fs::read(&paths.state).unwrap()).unwrap();
    assert!(rebuilt.bounds_future_manifests(floor));
    assert_eq!(rebuilt.highest_sequence, floor);
    assert_eq!(rebuilt.highest_version, current);
    assert_eq!(rebuilt.manifest_sha256, digest);

    let mut replay = manifest.clone();
    replay.version = Version::parse("1.0.38").unwrap();
    replay.installer_url = installer_url_for(replay.version);
    let replay_digest = sha256_bytes(replay.canonical_text().as_bytes()).unwrap();
    assert!(authorize_manifest(
        &installer,
        current,
        &replay,
        replay_digest,
        Duration::from_secs(1)
    )
    .is_err());

    // A state written under a retired keyring epoch is unusable for the same
    // reason and takes the same path.
    let wrong_epoch = TrustState {
        trust_epoch: EMBEDDED_TRUST_EPOCH + 1,
        highest_sequence: floor + 10,
        highest_version: Version::parse("1.0.40").unwrap(),
        manifest_sha256: [9u8; 32],
    };
    assert!(!wrong_epoch.bounds_future_manifests(floor));
    fs::write(&paths.state, wrong_epoch.canonical_text().as_bytes()).unwrap();
    assert_eq!(
        authorize_manifest(
            &installer,
            current,
            &manifest,
            digest,
            Duration::from_secs(1)
        )
        .unwrap(),
        TrustDecision::Current
    );
    assert_eq!(
        TrustState::parse(&fs::read(&paths.state).unwrap())
            .unwrap()
            .highest_sequence,
        floor
    );

    // Bytes that are not a well-formed state stay a terminal error, but the
    // message now names the file so the user can act on it.
    fs::write(&paths.state, b"corrupt\n").unwrap();
    let error = authorize_manifest(
        &installer,
        current,
        &manifest,
        digest,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(error.contains("trust-state.txt"), "{error}");

    // The oversize rejection happens inside the reader, before any parse,
    // so it needs the same treatment.
    fs::write(
        &paths.state,
        vec![b'x'; (MAX_TRUST_STATE_BYTES + 1) as usize],
    )
    .unwrap();
    let error = authorize_manifest(
        &installer,
        current,
        &manifest,
        digest,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(error.contains("exceeds"), "{error}");
    assert!(error.contains("trust-state.txt"), "{error}");
    let _ = fs::remove_dir_all(installer.parent().unwrap());
}

#[test]
fn manually_installed_current_version_advances_sequence_state() {
    let installer = temp_installer("manual-current");
    let mut first =
        ReleaseManifest::parse_with_sequence_floor(&fixture("manifest-positive.txt"), 1).unwrap();
    first.release_sequence = embedded_sequence_floor().unwrap();
    let manifest_bytes = first.canonical_text().into_bytes();
    authorize_manifest(
        &installer,
        Version::parse("1.0.33").unwrap(),
        &first,
        sha256_bytes(&manifest_bytes).unwrap(),
        Duration::from_secs(1),
    )
    .unwrap();

    let mut next = first.clone();
    next.release_sequence = first.release_sequence + 1;
    next.version = Version::parse("1.0.34").unwrap();
    next.installer_url = installer_url_for(next.version);
    let next_bytes = next.canonical_text();
    assert_eq!(
        authorize_manifest(
            &installer,
            Version::parse("1.0.34").unwrap(),
            &next,
            sha256_bytes(next_bytes.as_bytes()).unwrap(),
            Duration::from_secs(1),
        )
        .unwrap(),
        TrustDecision::Current
    );
    let paths = TrustPaths::adjacent_to(&installer).unwrap();
    let state = TrustState::parse(&fs::read(&paths.state).unwrap()).unwrap();
    assert_eq!(state.highest_sequence, first.release_sequence + 1);
    assert_eq!(state.highest_version, Version::parse("1.0.34").unwrap());
    assert!(authorize_manifest(
        &installer,
        Version::parse("1.0.34").unwrap(),
        &first,
        sha256_bytes(&manifest_bytes).unwrap(),
        Duration::from_secs(1),
    )
    .is_err());
    let _ = fs::remove_dir_all(installer.parent().unwrap());
}

#[test]
fn trust_state_atomic_replace_preserves_old_state_and_cleans_temporary() {
    let installer = temp_installer("atomic-replace");
    let paths = TrustPaths::adjacent_to(&installer).unwrap();
    atomic_replace_trust_state(&paths.state, b"old state\n").unwrap();
    assert_eq!(fs::read(&paths.state).unwrap(), b"old state\n");

    let held = OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001 | 0x0000_0002)
        .open(&paths.state)
        .unwrap();
    assert!(atomic_replace_trust_state(&paths.state, b"new state\n").is_err());
    assert_eq!(fs::read(&paths.state).unwrap(), b"old state\n");
    drop(held);

    atomic_replace_trust_state(&paths.state, b"new state\n").unwrap();
    assert_eq!(fs::read(&paths.state).unwrap(), b"new state\n");
    assert!(fs::read_dir(paths.state.parent().unwrap())
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp.")));
    let _ = fs::remove_dir_all(installer.parent().unwrap());
}

#[test]
fn trust_state_write_failure_is_terminal() {
    let installer = temp_installer("write-failure");
    let paths = TrustPaths::adjacent_to(&installer).unwrap();
    let mut first =
        ReleaseManifest::parse_with_sequence_floor(&fixture("manifest-positive.txt"), 1).unwrap();
    first.release_sequence = embedded_sequence_floor().unwrap();
    let manifest_bytes = first.canonical_text().into_bytes();
    let first_digest = sha256_bytes(&manifest_bytes).unwrap();
    authorize_manifest(
        &installer,
        Version::parse("1.0.33").unwrap(),
        &first,
        first_digest,
        Duration::from_secs(1),
    )
    .unwrap();
    let old_state = fs::read(&paths.state).unwrap();

    let held = OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001 | 0x0000_0002)
        .open(&paths.state)
        .unwrap();
    let mut next = first;
    next.release_sequence += 1;
    next.version = Version::parse("1.0.34").unwrap();
    next.installer_url = installer_url_for(next.version);
    let next_digest = sha256_bytes(next.canonical_text().as_bytes()).unwrap();
    let error = authorize_manifest(
        &installer,
        Version::parse("1.0.33").unwrap(),
        &next,
        next_digest,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(error.contains("atomically write"));
    assert_eq!(fs::read(&paths.state).unwrap(), old_state);
    drop(held);

    assert_eq!(
        authorize_manifest(
            &installer,
            Version::parse("1.0.33").unwrap(),
            &next,
            next_digest,
            Duration::from_secs(1),
        )
        .unwrap(),
        TrustDecision::Available
    );
    let _ = fs::remove_dir_all(installer.parent().unwrap());
}
