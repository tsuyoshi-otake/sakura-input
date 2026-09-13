use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

fn temporary_log(name: &str) -> PathBuf {
    let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("tmp")
        .join(format!(
            "sakura-learning-{}-{name}-{id}",
            std::process::id()
        ));
    fs::create_dir_all(&directory).expect("temporary directory");
    directory.join("log.bin")
}

fn history_contains(service: &LearningService, reading: &str, surface: &str) -> bool {
    let mut found = false;
    service.visit_prediction_history(reading, |candidate_reading, candidate_surface, _, _| {
        found = candidate_reading == reading && candidate_surface == surface;
        !found
    });
    found
}

fn snapshot_contains(path: &Path, reading: &str, surface: &str) -> bool {
    read_snapshot(path)
        .expect("verified snapshot")
        .records
        .iter()
        .any(|record| record.reading == reading && record.surface == surface)
}

fn append_test_frame(path: &Path, payload: &[u8], checksum: u32, payload_bytes: usize) {
    assert!(payload_bytes <= payload.len(), "test frame payload prefix");
    let mut file = open_append(path).expect("open test frame writer");
    file.write_all(
        &u32::try_from(payload.len())
            .expect("bounded test frame")
            .to_le_bytes(),
    )
    .expect("write test frame length");
    file.write_all(&checksum.to_le_bytes())
        .expect("write test frame checksum");
    file.write_all(&payload[..payload_bytes])
        .expect("write test frame payload");
    file.sync_data().expect("sync test frame");
}

#[test]
fn exact_context_wins_and_general_frequency_decays() {
    let mut index = Index::new();
    index.learn(7, 11, "かえる", "帰る", 100, 1);
    index.learn(8, 12, "かえる", "変える", 100, 2);
    index.learn(8, 12, "かえる", "変える", 100, 3);
    let surfaces = [("蛙", 10), ("帰る", 11), ("変える", 12)];

    let context_seven = index.preference("かえる", 7, surfaces, 100);
    assert_eq!(context_seven.exact, Some(1));
    assert_eq!(context_seven.general, Some(2));

    let context_eight = index.preference("かえる", 8, surfaces, 100);
    assert_eq!(context_eight.exact, Some(2));

    let wrong_right_context =
        index.preference("かえる", 7, [("蛙", 10), ("帰る", 12), ("変える", 12)], 100);
    assert_eq!(wrong_right_context.exact, None);
    assert_eq!(wrong_right_context.general, Some(2));

    index.learn(u16::MAX, u16::MAX, "しるし", "印", 100, 4);
    assert_eq!(
        index
            .preference("しるし", u16::MAX, [("標", 0), ("印", u16::MAX)], 100)
            .exact,
        Some(1),
        "the maximum class id must not collide with general learning"
    );
}

#[test]
fn identity_history_does_not_prefer_the_unconverted_reading() {
    let mut index = Index::new();
    index.learn(0, 0, "にほんご", "にほんご", 100, 1);
    index.learn(0, 0, "にほんご", "にほんご", 100, 2);
    index.learn(0, 0, "にほんご", "にほんご", 100, 3);
    let candidates = [("日本語", 0), ("にほんご", 0)];
    let preference = index.preference("にほんご", 0, candidates, 100);
    assert_eq!(preference.exact, None);
    assert_eq!(preference.general, None);
}

#[test]
fn ordinary_history_does_not_trigger_reverse_repair_lookup() {
    let service = LearningService::memory();
    service.learn("にほんご", "日本語", 0, 0);
    let hints =
        service.collect_commit_repair_readings("にほんご", sakura_core::InputSupport::default());
    assert!(
        hints.is_empty(),
        "ordinary history unexpectedly produced hints={hints:?}"
    );
}

#[test]
fn known_repair_variant_from_history_is_still_admitted() {
    let service = LearningService::memory();
    service.learn("こんにちは", "今日は", 0, 0);
    let hints = service
        .collect_commit_repair_readings("こんんにちは", sakura_core::InputSupport::default());
    assert!(
        hints.iter().any(|hint| hint.as_str() == "こんにちは"),
        "known repair variant must remain available: {hints:?}"
    );
}

#[test]
fn advanced_short_readings_do_not_gain_commit_history_authority() {
    let support = sakura_core::InputSupport::default();
    for (typed, repaired) in [
        ("う", "い"),
        ("いて", "って"),
        ("い", "お"),
        ("なに", "ない"),
    ] {
        let variants =
            sakura_core::collect_repair_variants(typed, support, sakura_core::MAX_REPAIR_VARIANTS);
        assert!(
            variants.iter().any(|variant| {
                variant.repaired.as_str() == repaired
                    && variant.kind == sakura_core::RepairKind::Advanced
            }),
            "fixture must exercise an actual Advanced variant: {typed} -> {repaired}"
        );
        let service = LearningService::memory();
        assert!(service
            .collect_commit_repair_readings(typed, support)
            .is_empty());
        service.learn(repaired, repaired, 0, 0);
        let hints = service.collect_commit_repair_readings(typed, support);
        assert!(hints.is_empty(),
                "unrelated prior commits must not promote Advanced variants: {typed} -> {repaired}, hints={hints:?}");
    }
}

#[test]
fn learning_strength_rejects_one_off_far_choices_and_decays_stale_context() {
    let mut index = Index::new();
    let candidates = [
        ("top", 9),
        ("near", 9),
        ("third", 9),
        ("fourth", 9),
        ("fifth", 9),
        ("sixth", 9),
        ("far", 9),
    ];

    // A single far-down selection remains recorded, but weak evidence must
    // not displace the converter's base ranking in the next conversion.
    index.learn(7, 9, "reading", "far", 100, 1);
    let one_confirmation = index.preference("reading", 7, candidates, 100);
    assert_eq!(one_confirmation.exact, None);
    assert_eq!(one_confirmation.general, None);

    // Two confirmations are medium evidence, still bounded to the first
    // four exact-context candidates.  Three recent confirmations are a
    // deliberate user preference and may select the exact-context choice.
    index.learn(7, 9, "reading", "far", 100, 2);
    assert_eq!(index.preference("reading", 7, candidates, 100).exact, None);
    index.learn(7, 9, "reading", "far", 100, 3);
    assert_eq!(
        index.preference("reading", 7, candidates, 100).exact,
        Some(6)
    );

    // Context-free history is never allowed to carry this far-down choice
    // into a different grammatical context, even after repetition.
    let different_context = index.preference("reading", 8, candidates, 100);
    assert_eq!(different_context.exact, None);
    assert_eq!(different_context.general, None);

    // Three 30-day half-lives reduce three confirmations to zero evidence.
    assert_eq!(index.preference("reading", 7, candidates, 190).exact, None);
}

#[test]
fn packed_index_and_public_entry_cap_stay_inside_the_memory_budget() {
    assert_eq!(MAX_LEARNING_ENTRIES, 98_304);
    assert!(
        core::mem::size_of::<Slot>() * SLOT_COUNT <= 8 * 1024 * 1024,
        "packed learning index exceeds its 8 MiB budget"
    );
}

#[test]
fn prediction_history_is_frequency_ranked_and_replayed_after_restart() {
    let path = temporary_log("prediction-history");
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("かながわ", "神奈川", 1, 11);
        service.learn("かなざわ", "金沢", 2, 12);
        service.learn("かなざわ", "金沢", 2, 13);
    }

    let reopened = LearningService::open(&path).expect("reopen");
    let mut matches = Vec::new();
    reopened.visit_prediction_history("かな", |reading, surface, right_context, score| {
        matches.push((reading.to_owned(), surface.to_owned(), right_context, score));
        true
    });

    assert_eq!(matches.len(), 2);
    assert_eq!(&matches[0].0, "かなざわ");
    assert_eq!(&matches[0].1, "金沢");
    assert_eq!(matches[0].2, 13);
    assert_eq!(&matches[1].0, "かながわ");
    assert_eq!(&matches[1].1, "神奈川");
    assert_eq!(matches[1].2, 11);
    assert!(matches[0].3 < matches[1].3);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn prediction_history_retains_128_entries_and_evicts_the_oldest() {
    let mut history = PredictionHistory::new();
    for sequence in 1..=MAX_PREDICTION_HISTORY_ENTRIES as u64 {
        history.learn(
            &format!("reading-{sequence:03}"),
            &format!("surface-{sequence:03}"),
            0,
            0,
            sequence,
        );
    }

    assert_eq!(history.entries.len(), MAX_PREDICTION_HISTORY_ENTRIES);
    assert!(
        history.entries.iter().all(|entry| entry.occupied),
        "the fixed window is full"
    );

    history.learn(
        "reading-overflow",
        "surface-overflow",
        0,
        0,
        MAX_PREDICTION_HISTORY_ENTRIES as u64 + 1,
    );

    assert_eq!(
        history.entries.len(),
        MAX_PREDICTION_HISTORY_ENTRIES,
        "retention remains bounded independently of prediction display limits"
    );
    assert!(!history.entries.iter().any(|entry| {
        entry.occupied
            && entry.reading.as_str() == "reading-001"
            && entry.surface.as_str() == "surface-001"
    }));
    assert!(history.entries.iter().any(|entry| {
        entry.occupied
            && entry.reading.as_str() == "reading-overflow"
            && entry.surface.as_str() == "surface-overflow"
    }));
}

#[test]
fn torn_tail_is_truncated_and_verified_records_survive_restart() {
    let path = temporary_log("torn");
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("かな", "加奈", 3, 4);
    }
    let good_len = fs::metadata(&path).expect("metadata").len();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append")
        .write_all(&[9, 0, 0])
        .expect("torn tail");

    let recovered = LearningService::open(&path).expect("recover");
    assert_eq!(recovered.recovered_tail_bytes(), 3);
    assert_eq!(fs::metadata(&path).expect("metadata").len(), good_len);
    let preference = recovered.preference("かな", 3, [("仮名", 4), ("加奈", 4)]);
    assert_eq!(preference.exact, Some(1));
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

/// The read that runs at every engine start used to be unbounded, so a
/// log grown past its ceiling by corruption or tampering was loaded
/// whole into memory. The excess is discarded now — and counted, since
/// a recovery total that omits it would describe an oversized log as
/// clean.
#[test]
fn an_oversized_log_is_read_within_its_bound_and_the_excess_is_counted() {
    let path = temporary_log("oversized");
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("かな", "加奈", 3, 4);
    }
    let good_len = fs::metadata(&path).expect("metadata").len();
    let excess = MAX_LEARNING_LOG_BYTES + 4_096 - good_len;
    OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append")
        .write_all(&vec![0xAA; excess as usize])
        .expect("oversized tail");

    let recovered = LearningService::open(&path).expect("recover");
    assert_eq!(
        fs::metadata(&path).expect("metadata").len(),
        good_len,
        "the log kept bytes past its ceiling"
    );
    assert_eq!(
        recovered.recovered_tail_bytes(),
        excess,
        "the discarded excess was left out of the recovery total"
    );
    let preference = recovered.preference("かな", 3, [("仮名", 4), ("加奈", 4)]);
    assert_eq!(
        preference.exact,
        Some(1),
        "recovery lost a record that was inside the bound"
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

/// Compaction and exact prediction deletion rewrite the file from what
/// they read, so for them an oversized log is a refusal, not a
/// recovery: writing back a truncated prefix would destroy records
/// nobody was told about.
#[test]
fn a_rewrite_refuses_a_log_past_its_ceiling() {
    let path = temporary_log("rewrite-bound");
    fs::write(&path, vec![0u8; (MAX_LEARNING_LOG_BYTES + 1) as usize]).expect("oversized log");
    let error = read_within_bound(&path).expect_err("an oversized log was accepted");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

/// The snapshot reader used to size the file with `metadata` and then
/// read it with no bound at all, which is the race the bounded read
/// exists to answer: a log that grows between the two calls was read
/// whole. It goes through the same bound now.
#[test]
fn a_snapshot_refuses_a_log_past_its_ceiling() {
    let path = temporary_log("snapshot-bound");
    fs::write(&path, vec![0u8; (MAX_LEARNING_LOG_BYTES + 1) as usize]).expect("oversized log");
    let error = read_snapshot(&path).expect_err("an oversized log was accepted");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

/// The upgrade adds context fields to every record, so an old log near
/// the ceiling lands over it once upgraded. Publishing it whole left a
/// file the startup compaction then refused to read, which failed `open`
/// and cost the whole session its durable learning — a session's worth of
/// learning lost to a repair that was supposed to be invisible.
#[test]
fn an_upgrade_that_would_land_over_the_ceiling_is_published_within_it() {
    let path = temporary_log("upgrade-over-cap");
    let reading = "かな";
    let surface = "加奈";
    let mut payload = Vec::new();
    payload.push(RECORD_COMMIT);
    payload.extend_from_slice(&unix_day().to_le_bytes());
    payload.extend_from_slice(&3u16.to_le_bytes());
    payload.extend_from_slice(&(reading.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(surface.len() as u16).to_le_bytes());
    payload.extend_from_slice(reading.as_bytes());
    payload.extend_from_slice(surface.as_bytes());
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&crc32(&payload).to_le_bytes());
    frame.extend_from_slice(&payload);

    // Over the ceiling to begin with, so the capped read keeps a whole
    // ceiling's worth of records and the upgrade — four bytes of context
    // per record — is guaranteed to grow that past the ceiling again.
    let mut old = header(FORMAT_VERSION_2).to_vec();
    while (old.len() as u64) < MAX_LEARNING_LOG_BYTES + 4_096 {
        old.extend_from_slice(&frame);
    }
    fs::write(&path, &old).expect("old log");

    let upgraded = LearningService::open(&path).expect("an upgraded log must still open");

    assert!(
        fs::metadata(&path).expect("metadata").len() <= MAX_LEARNING_LOG_BYTES,
        "the upgrade published a log no rewrite could read again"
    );
    assert_eq!(
        read_header(&fs::read(&path).expect("read")).unwrap(),
        LEARNING_FORMAT_VERSION
    );
    assert_eq!(
        upgraded
            .preference(reading, 3, [("仮名", 0), (surface, 0)])
            .exact,
        Some(1),
        "the records that fit were lost with the ones that did not"
    );
    assert!(
        upgraded.recovered_tail_bytes() > 0,
        "an open that discarded a tail reported none"
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn previous_format_upgrades_with_unknown_context_defaulted() {
    let path = temporary_log("upgrade");
    let reading = "かな";
    let surface = "加奈";
    let mut payload = Vec::new();
    payload.extend_from_slice(&unix_day().to_le_bytes());
    payload.extend_from_slice(&(reading.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(surface.len() as u16).to_le_bytes());
    payload.extend_from_slice(reading.as_bytes());
    payload.extend_from_slice(surface.as_bytes());
    let mut old = header(FORMAT_VERSION_1).to_vec();
    old.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    old.extend_from_slice(&crc32(&payload).to_le_bytes());
    old.extend_from_slice(&payload);
    fs::write(&path, &old).expect("old log");

    let upgraded = LearningService::open(&path).expect("upgrade");
    assert_eq!(
        read_header(&fs::read(&path).expect("read")).unwrap(),
        LEARNING_FORMAT_VERSION
    );
    assert_eq!(
        upgraded
            .preference(reading, 0, [("仮名", 0), (surface, 0)])
            .exact,
        Some(1)
    );
    assert!(path.with_extension("v1.bak").exists());
    assert_eq!(
        fs::read(path.with_extension("v1.bak")).expect("backup"),
        old
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn second_previous_format_preserves_left_context_and_existing_backup() {
    let path = temporary_log("upgrade-v2");
    let reading = "いった";
    let surface = "行った";
    let mut payload = Vec::new();
    payload.push(RECORD_COMMIT);
    payload.extend_from_slice(&unix_day().to_le_bytes());
    payload.extend_from_slice(&7u16.to_le_bytes());
    payload.extend_from_slice(&(reading.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(surface.len() as u16).to_le_bytes());
    payload.extend_from_slice(reading.as_bytes());
    payload.extend_from_slice(surface.as_bytes());
    let mut old = header(FORMAT_VERSION_2).to_vec();
    old.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    old.extend_from_slice(&crc32(&payload).to_le_bytes());
    old.extend_from_slice(&payload);
    fs::write(&path, &old).expect("old log");
    fs::write(path.with_extension("v2.bak"), b"retain me").expect("prior backup");

    let upgraded = LearningService::open(&path).expect("upgrade");

    assert_eq!(
        upgraded
            .preference(reading, 7, [("言った", 0), (surface, 0)])
            .exact,
        Some(1)
    );
    assert_eq!(
        fs::read(path.with_extension("v2.bak")).expect("prior backup"),
        b"retain me"
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn compaction_keeps_a_bounded_recent_source_of_truth_across_restart() {
    let path = temporary_log("compact");
    let service = LearningService::open(&path).expect("open");
    for sequence in 0..100 {
        let surface = if sequence == 99 { "加奈" } else { "仮名" };
        service.learn("かな", surface, 3, 4);
    }
    {
        let mut state = service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        compact_state(&mut state, 1_024, 10).expect("compaction");
        assert!(state.log.test_records() <= 10);
        assert!(state.log.test_bytes() <= 1_024 + HEADER_LEN as u64);
    }
    drop(service);

    let reopened = LearningService::open(&path).expect("restart");
    // Compaction retains the bounded recent records. The nine retained
    // normal choices are stronger evidence than the one last anomalous
    // choice, so the frequency-aware preference is the base candidate.
    assert_eq!(
        reopened
            .preference("かな", 3, [("仮名", 4), ("加奈", 4)])
            .exact,
        Some(0)
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn hard_log_ceiling_skips_a_write_without_extending_the_file() {
    let path = temporary_log("ceiling");
    let service = LearningService::open(&path).expect("open");
    let before = fs::metadata(&path).expect("metadata").len();
    {
        let mut state = service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.log.test_set_bytes(MAX_LEARNING_LOG_BYTES);
    }

    service.learn("かな", "加奈", 3, 4);

    assert_eq!(service.skipped_writes(), 1);
    assert_eq!(fs::metadata(&path).expect("metadata").len(), before);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn maintenance_thread_flushes_and_reaches_an_explicit_join() {
    let path = temporary_log("maintenance");
    let service = Arc::new(LearningService::open(&path).expect("open"));
    service.learn("かな", "加奈", 3, 4);
    let maintenance =
        LearningMaintenance::start_with_interval(Arc::clone(&service), Duration::from_millis(10))
            .expect("maintenance");
    for _ in 0..200 {
        let dirty = service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .log
            .test_dirty_records();
        if dirty == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .log
            .test_dirty_records(),
        0
    );
    maintenance.stop().expect("maintenance thread joined");
    assert_eq!(service.maintenance_failures(), 0);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn settings_snapshot_exports_verified_records_and_reports_a_torn_tail() {
    let path = temporary_log("settings-snapshot");
    let service = LearningService::open(&path).expect("open");
    service.learn("さくら", "Sakura\tInput", 7, 9);
    service.maintain().expect("flush");
    drop(service);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append tail")
        .write_all(b"torn")
        .expect("torn tail");

    let snapshot = read_snapshot(&path).expect("snapshot");

    assert_eq!(snapshot.format_version, LEARNING_FORMAT_VERSION);
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(snapshot.records[0].reading, "さくら");
    assert_eq!(snapshot.records[0].surface, "Sakura\tInput");
    assert_eq!(snapshot.ignored_tail_bytes, 4);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn clear_replaces_live_and_durable_learning_as_one_terminal_operation() {
    let path = temporary_log("settings-clear");
    let service = LearningService::open(&path).expect("open");
    service.learn("かな", "仮名", 3, 4);

    assert_eq!(service.clear().expect("clear"), 1);
    assert!(read_snapshot(&path)
        .expect("empty snapshot")
        .records
        .is_empty());
    assert_eq!(
        service.preference("かな", 3, [("加奈", 4), ("仮名", 4)]),
        LearningPreference {
            exact: None,
            general: None,
        }
    );

    service.learn("かな", "加奈", 3, 4);
    drop(service);
    let reopened = LearningService::open(&path).expect("reopen");
    assert_eq!(
        reopened
            .preference("かな", 3, [("仮名", 4), ("加奈", 4)])
            .exact,
        Some(1)
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn generation_advances_after_learning_and_after_successful_clear() {
    let service = LearningService::memory();
    let initial = service.generation();
    service.learn("かな", "仮名", 3, 4);
    let learned = service.generation();
    assert_ne!(learned, initial);
    service.clear().expect("clear");
    assert_ne!(service.generation(), learned);
}

#[test]
fn exact_prediction_forget_rewrites_history_and_allows_future_relearning() {
    let path = temporary_log("forget-prediction");
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    service.learn("reading", "surface", 5, 6);
    service.learn("reading", "other", 7, 8);
    let before_generation = service.generation();

    assert_eq!(
        service
            .forget_prediction_exact("reading", "surface")
            .expect("durable forget"),
        ForgetPredictionOutcome::Removed
    );
    assert_eq!(service.generation(), before_generation + 1);
    let snapshot = read_snapshot(&path).expect("snapshot after forget");
    assert!(snapshot
        .records
        .iter()
        .all(|record| !(record.reading == "reading" && record.surface == "surface")));
    assert!(snapshot
        .records
        .iter()
        .any(|record| record.reading == "reading" && record.surface == "other"));
    let mut remembered = Vec::new();
    service.visit_prediction_history("reading", |reading, surface, _, _| {
        remembered.push((reading.to_owned(), surface.to_owned()));
        true
    });
    assert_eq!(remembered, vec![("reading".to_owned(), "other".to_owned())]);
    drop(service);

    let reopened = LearningService::open(&path).expect("reopen after forget");
    let mut after_restart = Vec::new();
    reopened.visit_prediction_history("reading", |reading, surface, _, _| {
        after_restart.push((reading.to_owned(), surface.to_owned()));
        true
    });
    assert_eq!(
        after_restart,
        vec![("reading".to_owned(), "other".to_owned())]
    );

    reopened.learn("reading", "surface", 9, 10);
    reopened.maintain().expect("flush relearn");
    drop(reopened);
    let relearned = LearningService::open(&path).expect("reopen after relearn");
    assert!(read_snapshot(&path)
        .expect("snapshot after relearn")
        .records
        .iter()
        .any(|record| record.reading == "reading" && record.surface == "surface"));
    drop(relearned);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_forget_second_publish_failure_restores_old_state_and_append_owner() {
    let path = temporary_log("forget-second-publish-failure");
    let temporary = forget_temporary_path(&path);
    let recovery = forget_recovery_path(&path);
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    service.learn("reading", "other", 5, 6);
    let before_bytes = fs::read(&path).expect("old canonical");
    let before_generation = service.generation();

    {
        let _fault = ForgetFaultScope::new(&[ForgetFaultPoint::PublishMovesOldToRecovery]);
        let error = service
            .forget_prediction_exact("reading", "surface")
            .expect_err("second publish failure");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    assert_eq!(fs::read(&path).expect("restored canonical"), before_bytes);
    assert!(!temporary.exists(), "failed replacement temp is removed");
    assert!(
        !recovery.exists(),
        "old log was restored instead of stranded"
    );
    assert_eq!(service.generation(), before_generation);
    assert!(history_contains(&service, "reading", "surface"));
    assert!(history_contains(&service, "reading", "other"));

    service.learn("continued", "append-owner", 7, 8);
    service.maintain().expect("flush old append owner");
    drop(service);

    let reopened = LearningService::open(&path).expect("restart from old authority");
    assert!(history_contains(&reopened, "reading", "surface"));
    assert!(history_contains(&reopened, "continued", "append-owner"));
    drop(reopened);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_forget_replacement_owner_failure_never_publishes_filtered_bytes() {
    let path = temporary_log("forget-replacement-owner-failure");
    let temporary = forget_temporary_path(&path);
    let recovery = forget_recovery_path(&path);
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    let before_bytes = fs::read(&path).expect("old canonical");
    let before_generation = service.generation();

    {
        let _fault = ForgetFaultScope::new(&[ForgetFaultPoint::ReplacementOwnerOpen]);
        let error = service
            .forget_prediction_exact("reading", "surface")
            .expect_err("replacement owner preparation failure");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    assert_eq!(fs::read(&path).expect("old canonical"), before_bytes);
    assert!(!temporary.exists(), "unpublished replacement is cleaned");
    assert!(
        !recovery.exists(),
        "no recovery is needed before publication"
    );
    assert_eq!(service.generation(), before_generation);
    assert!(history_contains(&service, "reading", "surface"));

    service.learn("continued", "append-owner", 5, 6);
    service.maintain().expect("flush original append owner");
    drop(service);
    let reopened = LearningService::open(&path).expect("restart");
    assert!(history_contains(&reopened, "reading", "surface"));
    assert!(history_contains(&reopened, "continued", "append-owner"));
    drop(reopened);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_forget_publish_cleanup_failure_keeps_old_canonical_authoritative() {
    let path = temporary_log("forget-publish-cleanup-failure");
    let temporary = forget_temporary_path(&path);
    let recovery = forget_recovery_path(&path);
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    let before_bytes = fs::read(&path).expect("old canonical");
    let before_generation = service.generation();

    {
        let _fault = ForgetFaultScope::new(&[
            ForgetFaultPoint::Publish,
            ForgetFaultPoint::TemporaryCleanup,
        ]);
        let error = service
            .forget_prediction_exact("reading", "surface")
            .expect_err("publish and temporary cleanup failure");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("follow-up failed"));
    }

    assert_eq!(fs::read(&path).expect("old canonical"), before_bytes);
    assert!(temporary.exists(), "failed cleanup remains explicit state");
    assert!(!recovery.exists());
    assert_eq!(service.generation(), before_generation);
    assert_eq!(service.maintenance_failures(), 1);
    assert!(history_contains(&service, "reading", "surface"));

    service.learn("continued", "append-owner", 5, 6);
    drop(service);
    let reopened = LearningService::open(&path).expect("restart keeps canonical authority");
    assert!(history_contains(&reopened, "reading", "surface"));
    assert!(history_contains(&reopened, "continued", "append-owner"));
    reopened.maintain().expect("retry temporary cleanup");
    assert!(!temporary.exists());
    drop(reopened);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_forget_observation_failure_before_first_rename_keeps_old_authority() {
    let path = temporary_log("forget-observe-before-first-rename");
    let temporary = forget_temporary_path(&path);
    let recovery = forget_recovery_path(&path);
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    service.learn("reading", "other", 5, 6);
    let before_bytes = fs::read(&path).expect("old canonical");
    let before_generation = service.generation();

    {
        let _fault = ForgetFaultScope::new(&[
            ForgetFaultPoint::Publish,
            ForgetFaultPoint::PublishObservation,
        ]);
        let error = service
            .forget_prediction_exact("reading", "surface")
            .expect_err("publication and observation fail before the first rename");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("PublishObservation"));
    }

    assert_eq!(fs::read(&path).expect("old canonical"), before_bytes);
    assert!(!temporary.exists(), "unpublished replacement is cleaned");
    assert!(!recovery.exists(), "the first rename never ran");
    assert_eq!(service.generation(), before_generation);
    assert_eq!(service.maintenance_failures(), 1);
    assert!(history_contains(&service, "reading", "surface"));
    assert!(history_contains(&service, "reading", "other"));

    service.learn("continued", "append-owner", 7, 8);
    service.maintain().expect("flush original append owner");
    drop(service);

    let reopened = LearningService::open(&path).expect("restart from old authority");
    assert!(history_contains(&reopened, "reading", "surface"));
    assert!(history_contains(&reopened, "reading", "other"));
    assert!(history_contains(&reopened, "continued", "append-owner"));
    drop(reopened);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_forget_commits_when_old_backup_cleanup_is_deferred() {
    let path = temporary_log("forget-backup-cleanup-failure");
    let temporary = forget_temporary_path(&path);
    let recovery = forget_recovery_path(&path);
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    service.learn("reading", "other", 5, 6);
    let before_generation = service.generation();

    {
        let _fault = ForgetFaultScope::new(&[ForgetFaultPoint::BackupCleanup]);
        assert_eq!(
            service
                .forget_prediction_exact("reading", "surface")
                .expect("durable filtered replacement is committed"),
            ForgetPredictionOutcome::Removed
        );
    }

    assert_eq!(service.generation(), before_generation + 1);
    assert!(!history_contains(&service, "reading", "surface"));
    assert!(history_contains(&service, "reading", "other"));
    assert!(!snapshot_contains(&path, "reading", "surface"));
    assert!(snapshot_contains(&path, "reading", "other"));
    assert!(
        !temporary.exists(),
        "successful rename consumes replacement temp"
    );
    assert!(
        recovery.exists(),
        "unremoved old bytes remain an explicit artifact"
    );
    assert_eq!(service.maintenance_failures(), 1);

    service.learn("newer", "canonical", 7, 8);
    drop(service);

    // A stale old backup is cleanup-only once canonical exists. It must
    // not roll back a newer canonical log on restart.
    let reopened = LearningService::open(&path).expect("restart prefers canonical");
    assert!(!history_contains(&reopened, "reading", "surface"));
    assert!(history_contains(&reopened, "reading", "other"));
    assert!(history_contains(&reopened, "newer", "canonical"));
    reopened.maintain().expect("retry deferred backup cleanup");
    assert!(!recovery.exists(), "deferred backup cleanup completed");
    drop(reopened);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_forget_returns_removed_when_publish_reports_after_filtered_commit() {
    let path = temporary_log("forget-publish-after-commit-error");
    let temporary = forget_temporary_path(&path);
    let recovery = forget_recovery_path(&path);
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    service.learn("reading", "other", 5, 6);
    let before_generation = service.generation();

    {
        let _fault = ForgetFaultScope::new(&[ForgetFaultPoint::PublishCommitsThenErrors]);
        assert_eq!(
            service
                .forget_prediction_exact("reading", "surface")
                .expect("physical filtered canonical is a committed delete"),
            ForgetPredictionOutcome::Removed
        );
    }

    assert_eq!(service.generation(), before_generation + 1);
    assert!(!history_contains(&service, "reading", "surface"));
    assert!(history_contains(&service, "reading", "other"));
    assert!(!snapshot_contains(&path, "reading", "surface"));
    assert!(!temporary.exists());
    assert!(!recovery.exists());
    assert_eq!(service.maintenance_failures(), 1);

    service.learn("reading", "surface", 7, 8);
    service.maintain().expect("flush relearning");
    drop(service);
    let reopened = LearningService::open(&path).expect("restart after relearning");
    assert!(history_contains(&reopened, "reading", "surface"));
    drop(reopened);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_recovery_repairs_a_torn_tail_after_failed_restore() {
    let path = temporary_log("forget-recovery-torn-tail");
    let temporary = forget_temporary_path(&path);
    let recovery = forget_recovery_path(&path);
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    let before_bytes = fs::read(&path).expect("old canonical");
    let before_generation = service.generation();

    {
        let _fault = ForgetPredictionDeepRecoveryFault::install();
        let error = service
            .forget_prediction_exact("reading", "surface")
            .expect_err("old canonical reaches recovery and immediate restore fails");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    assert!(!path.exists(), "failed restore leaves canonical absent");
    assert_eq!(fs::read(&recovery).expect("old recovery"), before_bytes);
    assert!(
        temporary.exists(),
        "unpublished filtered replacement is retained"
    );
    assert_eq!(service.generation(), before_generation);
    assert!(history_contains(&service, "reading", "surface"));

    // The live service still owns the old inode after the first rename.
    // Its valid append must survive startup recovery along with the old
    // target history that failed to delete.
    service.learn("continued", "backup-owner", 5, 6);
    {
        let state = service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .log
            .test_sync_append_owner()
            .expect("sync preserved old append owner");
    }
    let verified_recovery_len = fs::metadata(&recovery)
        .expect("verified recovery metadata")
        .len();

    // Simulate a crash after the next frame's envelope and all but one
    // payload byte reached the recovery inode.
    let torn_payload = encode_record("discarded", "torn", 7, 8, 9).expect("torn payload");
    append_test_frame(
        &recovery,
        &torn_payload,
        crc32(&torn_payload),
        torn_payload.len() - 1,
    );
    assert!(
        fs::metadata(&recovery)
            .expect("torn recovery metadata")
            .len()
            > verified_recovery_len,
        "test injected a durable incomplete final frame"
    );
    drop(service);

    let reopened = LearningService::open(&path).expect("restart repairs and restores recovery");
    assert_eq!(
        fs::metadata(&path)
            .expect("repaired canonical metadata")
            .len(),
        verified_recovery_len,
        "startup retained exactly the verified recovery prefix"
    );
    assert!(
        !recovery.exists(),
        "recovery inode was restored to canonical"
    );
    assert!(history_contains(&reopened, "reading", "surface"));
    assert!(history_contains(&reopened, "continued", "backup-owner"));
    assert!(snapshot_contains(&path, "reading", "surface"));
    assert!(snapshot_contains(&path, "continued", "backup-owner"));
    assert!(!snapshot_contains(&path, "discarded", "torn"));

    reopened
        .maintain()
        .expect("settle stale filtered temporary after recovery");
    assert!(!temporary.exists(), "stale filtered temporary was settled");

    reopened.learn("after", "restart", 9, 10);
    assert_eq!(
        reopened.maintain().expect("flush post-recovery learning"),
        MaintenanceOutcome::Flushed
    );
    drop(reopened);

    let restarted = LearningService::open(&path).expect("restart after recovery learning");
    assert!(history_contains(&restarted, "reading", "surface"));
    assert!(history_contains(&restarted, "continued", "backup-owner"));
    assert!(history_contains(&restarted, "after", "restart"));
    drop(restarted);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_recovery_rejects_complete_semantic_corruption() {
    let path = temporary_log("forget-recovery-semantic-corruption");
    let recovery = forget_recovery_path(&path);
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        service.maintain().expect("flush canonical");
    }
    fs::rename(&path, &recovery).expect("move old authority to recovery");
    let malformed_payload = [0xff];
    append_test_frame(
        &recovery,
        &malformed_payload,
        crc32(&malformed_payload),
        malformed_payload.len(),
    );
    let before = fs::read(&recovery).expect("complete malformed recovery");

    let error = LearningService::open(&path)
        .expect_err("a complete semantic error must not be repaired as a torn tail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("invalid complete record"));
    assert!(
        !path.exists(),
        "canonical was not published from corrupt recovery"
    );
    assert_eq!(
        fs::read(&recovery).expect("recovery remains intact"),
        before
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_recovery_rejects_complete_checksum_corruption() {
    let path = temporary_log("forget-recovery-checksum-corruption");
    let recovery = forget_recovery_path(&path);
    {
        let service = LearningService::open(&path).expect("open");
        service.learn("reading", "surface", 3, 4);
        service.maintain().expect("flush canonical");
    }
    fs::rename(&path, &recovery).expect("move old authority to recovery");
    let payload = encode_record("complete", "checksum", 5, 6, 7).expect("complete payload");
    append_test_frame(&recovery, &payload, crc32(&payload) ^ 1, payload.len());
    let before = fs::read(&recovery).expect("complete checksum-corrupt recovery");

    let error = LearningService::open(&path)
        .expect_err("a complete checksum error must not be repaired as a torn tail");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains("invalid complete record checksum"));
    assert!(
        !path.exists(),
        "canonical was not published from corrupt recovery"
    );
    assert_eq!(
        fs::read(&recovery).expect("recovery remains intact"),
        before
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_recovery_rejects_invalid_header_or_version() {
    for (name, recovery_bytes) in [
        ("invalid-header", b"not a learning log".to_vec()),
        (
            "unsupported-version",
            header(LEARNING_FORMAT_VERSION + 1).to_vec(),
        ),
    ] {
        let path = temporary_log(name);
        let recovery = forget_recovery_path(&path);
        fs::write(&recovery, &recovery_bytes).expect("write invalid recovery");

        let error = LearningService::open(&path)
            .expect_err("invalid recovery header or version must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            !path.exists(),
            "canonical was not created after recovery failure"
        );
        assert_eq!(
            fs::read(&recovery).expect("recovery remains intact"),
            recovery_bytes
        );
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}

#[test]
fn exact_prediction_forget_restore_failure_keeps_a_restart_recovery_log_and_append_owner() {
    let path = temporary_log("forget-restore-failure");
    let temporary = forget_temporary_path(&path);
    let recovery = forget_recovery_path(&path);
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    let before_bytes = fs::read(&path).expect("old canonical");
    let before_generation = service.generation();

    {
        let _fault = ForgetFaultScope::new(&[
            ForgetFaultPoint::PublishMovesOldToRecovery,
            ForgetFaultPoint::RecoveryRestore,
        ]);
        let error = service
            .forget_prediction_exact("reading", "surface")
            .expect_err("restore failure is surfaced");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    assert!(
        !path.exists(),
        "canonical is never recreated during failed recovery"
    );
    assert_eq!(fs::read(&recovery).expect("recovery bytes"), before_bytes);
    assert!(
        temporary.exists(),
        "filtered temp is retained only for tracked cleanup"
    );
    assert_eq!(service.generation(), before_generation);
    assert!(history_contains(&service, "reading", "surface"));

    // The original shared writer now owns the backup inode, so durable
    // relearning can continue until restart performs the deterministic
    // restore. Sync it directly without invoking maintenance recovery.
    service.learn("continued", "backup-owner", 5, 6);
    {
        let state = service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .log
            .test_sync_append_owner()
            .expect("sync backup append owner");
    }
    assert!(snapshot_contains(&recovery, "reading", "surface"));
    assert!(snapshot_contains(&recovery, "continued", "backup-owner"));
    drop(service);

    // Startup restores the verified old recovery before create_if_missing
    // can run. The filtered temp is cleanup-only and cannot override it.
    let reopened = LearningService::open(&path).expect("restart restores old authority");
    assert!(path.exists());
    assert!(!recovery.exists(), "recovery was renamed back to canonical");
    assert!(history_contains(&reopened, "reading", "surface"));
    assert!(history_contains(&reopened, "continued", "backup-owner"));
    reopened.maintain().expect("clean stale filtered temp");
    assert!(
        !temporary.exists(),
        "stale filtered temp was never published"
    );
    drop(reopened);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn exact_prediction_forget_durable_failure_keeps_authoritative_state() {
    let path = temporary_log("forget-failure");
    let service = LearningService::open(&path).expect("open");
    service.learn("reading", "surface", 3, 4);
    let before_file = fs::read(&path).expect("read old log");
    let before_generation = service.generation();
    {
        let mut state = service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.log.test_disable_writer();
    }

    let error = service
        .forget_prediction_exact("reading", "surface")
        .expect_err("missing writer is a durable failure");
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(fs::read(&path).expect("old log survives"), before_file);
    assert_eq!(service.generation(), before_generation);
    let mut remembered = false;
    service.visit_prediction_history("reading", |reading, surface, _, _| {
        remembered = reading == "reading" && surface == "surface";
        false
    });
    assert!(
        remembered,
        "the old in-memory history remains authoritative"
    );
    assert_eq!(
        LearningService::memory()
            .forget_prediction_exact("reading", "surface")
            .expect("memory outcome"),
        ForgetPredictionOutcome::Unavailable
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn crc32_matches_the_standard_check_value() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
}

#[test]
fn repair_suppress_is_durable_and_hidden_from_snapshots() {
    let path = temporary_log("repair-suppress");
    let service = LearningService::open(&path).expect("open learning");
    service.suppress_repair_reading("こにちは");
    assert!(service.is_repair_suppressed("こにちは"));
    drop(service);

    let reopened = LearningService::open(&path).expect("reopen learning");
    assert!(reopened.is_repair_suppressed("こにちは"));
    assert!(
        !snapshot_contains(&path, "こにちは", REPAIR_SUPPRESS_SURFACE),
        "suppress markers must not appear in the settings export"
    );
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}
