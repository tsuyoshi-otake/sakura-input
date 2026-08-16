//! Host-side generator for developer-history PBT artifacts.
//!
//! `sakura-engine` is `#![cfg(windows)]`, so Linux cloud agents cannot run its
//! `#[test]` modules. This binary includes the same independent oracle and
//! writes the same verification artifacts the Windows test would.

#[path = "../crates/sakura-engine/src/developer_history_oracle.rs"]
mod developer_history_oracle;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use developer_history_oracle::{
    apply, apply_all, atomic_conditions, DomainEvent, OracleState, ScopeKind,
};

fn verification_dir() -> PathBuf {
    // Host binary is compiled with rustc (not cargo), so resolve from cwd.
    let cwd = std::env::current_dir().expect("cwd");
    if cwd.join("verification/developer-history").is_dir() {
        cwd.join("verification/developer-history")
    } else if cwd
        .join("../verification/developer-history")
        .is_dir()
    {
        cwd.join("../verification/developer-history")
    } else {
        PathBuf::from("verification/developer-history")
    }
}

fn write_pbt_artifacts(seed: u64, shrunk: Option<(&[DomainEvent], &str)>) {
    let dir = verification_dir();
    fs::create_dir_all(&dir).expect("verification directory");
    fs::write(dir.join("pbt-seed.txt"), format!("{seed}\n")).expect("pbt seed");
    let body = match shrunk {
        Some((events, reason)) => format!(
            "# Shrunk counterexample\n\nseed: {seed}\n\nreason: {reason}\n\nevents:\n{}\n",
            events
                .iter()
                .map(|event| format!("- {event:?}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        None => format!(
            "# No failing shrink\n\nseed: {seed}\n\nThe property campaign finished without a counterexample.\n"
        ),
    };
    fs::write(dir.join("pbt-shrunk-counterexample.md"), body).expect("pbt shrink");
}

fn xorshift(seed: &mut u64) -> u64 {
    let mut x = *seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *seed = x;
    x
}

fn random_event(seed: &mut u64, booted: bool) -> DomainEvent {
    let pick = xorshift(seed) % if booted { 12 } else { 1 };
    match pick {
        0 => DomainEvent::Boot {
            developer_mode: xorshift(seed) % 2 == 0,
        },
        1 => DomainEvent::SetDeveloperMode {
            on: xorshift(seed) % 2 == 0,
        },
        2 => DomainEvent::WatcherPublish,
        3 => DomainEvent::RequestBoundary,
        4 => DomainEvent::Classify {
            scope: match xorshift(seed) % 3 {
                0 => ScopeKind::Unclassified,
                1 => ScopeKind::Normal,
                _ => ScopeKind::Sensitive,
            },
        },
        5 => DomainEvent::Key {
            test_only: xorshift(seed) % 2 == 0,
        },
        6 => DomainEvent::Flush,
        7 => DomainEvent::Clear,
        8 => DomainEvent::QueueFull,
        9 => DomainEvent::PersistFail,
        10 => DomainEvent::Crash,
        _ => DomainEvent::Restart {
            restore_setting: xorshift(seed) % 2 == 0,
        },
    }
}

fn property_holds(events: &[DomainEvent]) -> Option<&'static str> {
    let mut state = OracleState::default();
    for &event in events {
        apply(&mut state, event);
        if state.forbidden_stale_inactive() {
            return Some("forbidden stale-inactive");
        }
        if !state.attach_matches_published_after_request() {
            return Some("attach mismatch after request");
        }
        if state.stats_active() != state.service_attached {
            return Some("stats.active mismatch");
        }
    }
    None
}

fn shrink_events(
    events: &[DomainEvent],
    check: impl Fn(&[DomainEvent]) -> Option<&'static str>,
) -> Vec<DomainEvent> {
    let mut best = events.to_vec();
    let mut changed = true;
    while changed && best.len() > 1 {
        changed = false;
        for index in 0..best.len() {
            let mut candidate = best.clone();
            candidate.remove(index);
            if check(&candidate).is_some() {
                best = candidate;
                changed = true;
                break;
            }
        }
    }
    best
}

fn main() {
    let seed = 0x4448_4353_3200_0001u64;
    let mut rng = seed;
    let mut events = vec![DomainEvent::Boot {
        developer_mode: false,
    }];
    for _ in 0..256 {
        events.push(random_event(&mut rng, true));
    }
    if let Some(reason) = property_holds(&events) {
        let shrunk = shrink_events(&events, property_holds);
        write_pbt_artifacts(seed, Some((&shrunk, reason)));
        panic!("counterexample: {reason}");
    }
    write_pbt_artifacts(seed, None);

    // Concrete green path must still attach.
    let green = apply_all([
        DomainEvent::Boot {
            developer_mode: false,
        },
        DomainEvent::SetDeveloperMode { on: true },
        DomainEvent::WatcherPublish,
        DomainEvent::RequestBoundary,
        DomainEvent::Classify {
            scope: ScopeKind::Normal,
        },
        DomainEvent::Key { test_only: false },
    ]);
    assert!(green.service_attached);
    assert_eq!(green.durable.len(), 1);

    let mut seen: HashMap<&'static str, (bool, bool)> = HashMap::new();
    let mut state = OracleState::default();
    let mut coverage_events = vec![DomainEvent::Boot {
        developer_mode: false,
    }];
    let mut coverage_rng = seed;
    for _ in 0..1024 {
        let event = random_event(&mut coverage_rng, true);
        coverage_events.push(event);
        for condition in atomic_conditions(&state, event) {
            let entry = seen.entry(condition.id).or_insert((false, false));
            if condition.value {
                entry.1 = true;
            } else {
                entry.0 = true;
            }
        }
        apply(&mut state, event);
    }

    let dir = verification_dir().join("coverage");
    fs::create_dir_all(&dir).expect("coverage directory");
    let mut rows = String::from(
        "# Atomic-condition (C2) coverage — developer-history oracle\n\n\
         Scope: `crates/sakura-engine/src/developer_history_oracle.rs` predicates in `atomic_conditions`.\n\
         This is atomic-condition polarity coverage of the independent oracle, not MC/DC of the whole workspace and not line coverage claimed as C2.\n\n\
         | condition | false | true |\n|---|---|---|\n",
    );
    let mut covered = 0usize;
    let mut total = 0usize;
    let mut ids: Vec<_> = seen.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let (saw_false, saw_true) = seen[&id];
        total += 2;
        if saw_false {
            covered += 1;
        }
        if saw_true {
            covered += 1;
        }
        rows.push_str(&format!("| `{id}` | {saw_false} | {saw_true} |\n"));
    }
    rows.push_str(&format!(
        "\nCovered polarities: {covered}/{total} ({:.1}%). Seed `{seed:#x}`. Cases: {}.\n\n\
         Line/region coverage of production functions is **not** this table. See `llvm-cov-report.md` when measured on Windows.\n",
        100.0 * covered as f64 / total as f64,
        coverage_events.len()
    ));
    fs::write(dir.join("c2-report.md"), rows).expect("c2 report");
    for (id, (saw_false, saw_true)) in &seen {
        assert!(
            *saw_false && *saw_true,
            "{id} missing a polarity (false={saw_false}, true={saw_true})"
        );
    }
    eprintln!("wrote PBT + C2 artifacts under verification/developer-history/");
}