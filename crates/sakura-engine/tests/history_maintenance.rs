//! Opt-in release acceptance tests for the large-history regressions.
//!
//! SAKURA_HISTORY_FIXTURE names an already preserved encrypted store. Tests
//! only read it and operate on private copies; no record contents are printed.
//! Run with --release --ignored --nocapture --test-threads=1 and an external
//! 360-second timeout. Smaller deterministic matrices run in the library suite.

#[allow(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{named_key, session_for, Engine, PATIENT};
use sakura_engine::input_history::{
    read_snapshot, InputHistoryRecord, InputHistoryService, ScopeClass, MAX_INPUT_HISTORY_BYTES,
};
use sakura_proto::{KeyCode, Request, Response};

fn fixture() -> PathBuf {
    PathBuf::from(std::env::var_os("SAKURA_HISTORY_FIXTURE").expect("preserved history fixture"))
}

fn configuration(profile: &Path, enabled: bool) {
    let directory = profile.join("SakuraInput/config");
    fs::create_dir_all(&directory).unwrap();
    let next = directory.join("config.next");
    fs::write(
        &next,
        format!(
            "[meta]\nformat-version = \"4\"\n[input]\nprediction-enabled = \"false\"\nneural-reranker-scope = \"off\"\ndeveloper-mode = \"{enabled}\"\n"
        ),
    )
    .unwrap();
    fs::rename(next, directory.join("config.toml")).unwrap();
}

fn large_history(profile: &Path) {
    let mut bytes = fs::read(fixture()).unwrap();
    assert!(bytes.len() >= 48 * 1024 * 1024);
    assert_eq!(&bytes[..4], b"SKIH");
    let mut end = 8;
    while end + 8 <= bytes.len() {
        let length = u32::from_le_bytes(bytes[end..end + 4].try_into().unwrap()) as usize;
        let next = end.checked_add(8 + length).unwrap();
        if next > 48 * 1024 * 1024 || next > bytes.len() {
            break;
        }
        end = next;
    }
    assert!(
        end >= 47 * 1024 * 1024,
        "fixture has enough complete frames"
    );
    bytes.truncate(end);
    let history = profile.join("SakuraInput/history");
    fs::create_dir_all(&history).unwrap();
    fs::write(history.join("input.bin"), bytes).unwrap();
}

fn keys_during_history_open(enabled_at_start: bool) {
    let mut engine = Engine::spawn_isolated_with_setup(|profile| {
        large_history(profile);
        configuration(profile, enabled_at_start);
    });
    let ready = Instant::now();
    let mut settings = engine.client();
    let _ = session_for(&mut settings, "history-settings-fixture.exe");
    let mut host = engine.client();
    let session = session_for(&mut host, "history-key-fixture.exe");
    println!("history-ready-ms={}", ready.elapsed().as_millis());
    if !enabled_at_start {
        assert!(matches!(
            settings.call(&Request::InputHistoryStats, PATIENT),
            Ok(Response::InputHistoryStats { active: false, .. })
        ));
        configuration(engine.local_app_data(), true);
    }
    let lock = engine
        .local_app_data()
        .join("SakuraInput/history/input.bin.writer.lock");
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut opening_keys = 0;
    let mut maximum_key = Duration::ZERO;
    let started = Instant::now();
    loop {
        let active = match settings.call(&Request::InputHistoryStats, Duration::from_millis(100)) {
            Ok(Response::InputHistoryStats { active, .. }) => active,
            other => panic!("history stats blocked or failed: {other:?}"),
        };
        let key_start = Instant::now();
        assert!(matches!(
            host.call(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Escape),
                },
                Duration::from_millis(100),
            ),
            Ok(Response::Output(_))
        ));
        let elapsed = key_start.elapsed();
        assert!(elapsed < Duration::from_millis(100));
        maximum_key = maximum_key.max(elapsed);
        if active {
            break;
        }
        if lock.exists() {
            opening_keys += 1;
        }
        assert!(Instant::now() < deadline, "history never became active");
        sleep(Duration::from_millis(50));
    }
    assert!(
        opening_keys > 0,
        "keys overlapped actual store initialization"
    );
    println!(
        "history-start-enabled={enabled_at_start} activation-ms={} opening-keys={opening_keys} maximum-key-us={}",
        started.elapsed().as_millis(), maximum_key.as_micros()
    );
    drop(host);
    drop(settings);
    let stopped = Instant::now();
    assert!(engine.cleanup().unwrap().status.success());
    println!("history-engine-stop-ms={}", stopped.elapsed().as_millis());
}

#[test]
fn disabling_developer_history_stops_engine_trace_until_reenabled() {
    let mut engine = Engine::spawn_isolated_with_setup(|profile| configuration(profile, true));
    let mut settings = engine.client();
    let _ = session_for(&mut settings, "history-trace-settings.exe");
    let mut host = engine.client();
    let session = session_for(&mut host, "history-trace-host.exe");
    let trace = engine.local_app_data().join("SakuraInput/logs/debug.tsv");
    let wait_active = |settings: &mut sakura_ipc::Client, expected: bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if matches!(settings.call(&Request::InputHistoryStats, PATIENT),
                Ok(Response::InputHistoryStats { active, .. }) if active == expected)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "history active did not become {expected}"
            );
            sleep(Duration::from_millis(10));
        }
    };
    let key = |host: &mut sakura_ipc::Client| {
        assert!(matches!(
            host.call(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Escape),
                },
                PATIENT
            ),
            Ok(Response::Output(_))
        ));
    };
    wait_active(&mut settings, true);
    key(&mut host);
    let before = fs::read(&trace).unwrap();
    assert!(!before.is_empty(), "enabled engine trace emitted records");
    configuration(engine.local_app_data(), false);
    wait_active(&mut settings, false);
    for _ in 0..32 {
        key(&mut host);
    }
    assert_eq!(
        fs::read(&trace).unwrap(),
        before,
        "disabled engine kept tracing"
    );
    configuration(engine.local_app_data(), true);
    wait_active(&mut settings, true);
    key(&mut host);
    assert!(
        fs::read(&trace).unwrap().len() > before.len(),
        "reenabling restored trace"
    );
    drop(host);
    drop(settings);
    assert!(engine.cleanup().unwrap().status.success());
}

#[test]
#[ignore = "requires preserved 64 MiB fixture and release timing run"]
fn large_history_hot_activation_keeps_independent_keys_available() {
    keys_during_history_open(false);
}

#[test]
#[ignore = "requires preserved 64 MiB fixture and release timing run"]
fn large_history_startup_keeps_independent_keys_available() {
    keys_during_history_open(true);
}

#[test]
#[ignore = "requires preserved 64 MiB fixture and 60-second release workload"]
fn full_history_keeps_all_1200_commits_and_stops_promptly() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(std::env::var_os("USERPROFILE").unwrap())
        .join("tmp")
        .join(format!(
            "sakura-history-capacity-{}-{nonce}",
            std::process::id()
        ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("input.bin");
    fs::copy(fixture(), &path).unwrap();
    let bytes = fs::metadata(&path).unwrap().len();
    assert!(bytes <= MAX_INPUT_HISTORY_BYTES);
    assert!(bytes > MAX_INPUT_HISTORY_BYTES - 1024);
    let opening = Instant::now();
    let service = InputHistoryService::open(&path).unwrap();
    println!("history-capacity-open-ms={}", opening.elapsed().as_millis());
    let session = service.allocate_session_id().unwrap();
    let started = Instant::now();
    for index in 0..1200u16 {
        service.record_commit(session, ScopeClass::Normal, "けんしょう", "検証", index, 0);
        sleep(
            (started + Duration::from_millis((u64::from(index) + 1) * 50))
                .saturating_duration_since(Instant::now()),
        );
    }
    let stopping = Instant::now();
    service.stop().unwrap();
    let stop = stopping.elapsed();
    println!(
        "history-capacity-stop-ms={} dropped={} failures={}",
        stop.as_millis(),
        service.stats().dropped_events(),
        service.stats().persistence_failures()
    );
    assert_eq!(service.stats().dropped_events(), 0);
    assert_eq!(service.stats().persistence_failures(), 0);
    assert!(stop < Duration::from_secs(5), "stop took {stop:?}");
    assert!(fs::metadata(&path).unwrap().len() <= MAX_INPUT_HISTORY_BYTES);
    let snapshot = read_snapshot(&path).unwrap();
    let indices: Vec<_> = snapshot
        .records
        .iter()
        .filter_map(|record| match record {
            InputHistoryRecord::Commit(record) if record.session == session => {
                Some(record.left_context)
            }
            _ => None,
        })
        .collect();
    assert_eq!(indices, (0..1200u16).collect::<Vec<_>>());
    println!("history-capacity-durable-records={}", indices.len());
    drop(snapshot);
    drop(service);
    fs::remove_dir_all(&root).unwrap();
}
