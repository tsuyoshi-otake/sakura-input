use crate::space_key_dispatch_oracle::{
    apply, apply_all, atomic_conditions, no_dual_effect, peer_converting, space_effect,
    AtomicCondition, ConnState, DomainEvent, OracleState, SpaceEffect, ATOM_COUNT,
    MAX_DOCUMENT_SPACES,
};

fn type_on(connection: u8) -> DomainEvent {
    DomainEvent::Type { connection }
}

fn space_on(targets: u8) -> DomainEvent {
    DomainEvent::Space { targets }
}

#[test]
fn composing_space_converts_and_does_not_insert() {
    let state = apply_all(2, [type_on(0), space_on(1 << 0)]);
    assert_eq!(state.connections[0].state, ConnState::Converting);
    assert!(!state.last_inserted);
    assert!(state.last_converted);
    assert_eq!(state.document_spaces, 0);
    assert!(no_dual_effect(&state));
}

#[test]
fn idle_space_inserts_fullwidth_slot_when_no_peer_is_composing() {
    let state = apply_all(2, [space_on(1 << 0)]);
    assert!(state.last_inserted);
    assert!(!state.last_converted);
    assert_eq!(state.document_spaces, 1);
    assert_eq!(state.connections[0].state, ConnState::Idle);
}

#[test]
fn reported_dual_delivery_must_not_insert_and_convert() {
    let state = apply_all(2, [type_on(0), space_on(0b11)]);
    assert!(
        no_dual_effect(&state),
        "the 2026-08-16 log class is forbidden: insert and convert on one Space"
    );
    assert!(state.last_converted);
    assert!(!state.last_inserted);
    assert_eq!(state.document_spaces, 0);
    assert_eq!(state.connections[0].state, ConnState::Converting);
    assert_eq!(state.connections[1].state, ConnState::Idle);
}

#[test]
fn idle_peer_is_absorbed_while_composing() {
    let mut state = OracleState::new(2);
    apply(&mut state, type_on(0));
    assert!(peer_converting(&state, 1));
    assert_eq!(space_effect(&state, 1), SpaceEffect::Absorb);
    apply(&mut state, space_on(1 << 1));
    assert!(state.last_absorbed);
    assert_eq!(state.document_spaces, 0);
}

#[test]
fn converting_space_stays_a_conversion() {
    let state = apply_all(1, [type_on(0), space_on(1 << 0), space_on(1 << 0)]);
    assert_eq!(state.connections[0].state, ConnState::Converting);
    assert_eq!(state.conversions, 2);
    assert_eq!(state.document_spaces, 0);
}

#[test]
fn commit_returns_to_idle_so_later_space_inserts() {
    let state = apply_all(
        1,
        [
            type_on(0),
            DomainEvent::Commit { connection: 0 },
            space_on(1),
        ],
    );
    assert!(state.last_inserted);
    assert_eq!(state.document_spaces, 1);
}

#[test]
fn cancel_and_replace_drop_composition_without_a_space() {
    let cancelled = apply_all(1, [type_on(0), DomainEvent::Cancel { connection: 0 }]);
    assert_eq!(cancelled.connections[0].state, ConnState::Idle);
    assert_eq!(cancelled.document_spaces, 0);
    let replaced = apply_all(
        1,
        [type_on(0), DomainEvent::ReplaceContext { connection: 0 }],
    );
    assert_eq!(replaced.connections[0].state, ConnState::Idle);
}

#[test]
fn crash_restart_forgets_composition_and_does_not_convert_later() {
    let state = apply_all(
        2,
        [
            type_on(0),
            DomainEvent::CrashRestart { connection: 0 },
            space_on(1 << 0),
        ],
    );
    assert_eq!(state.crashes, 1);
    assert_eq!(state.connections[0].state, ConnState::Idle);
    assert!(state.last_inserted);
    assert!(!state.last_converted);
}

#[test]
fn timeout_space_is_absorbed_without_document_change() {
    let state = apply_all(2, [type_on(0), DomainEvent::TimeoutSpace { targets: 0b11 }]);
    assert_eq!(state.timeouts, 1);
    assert_eq!(state.document_spaces, 0);
    assert_eq!(state.connections[0].state, ConnState::Composing);
    assert!(state.last_absorbed);
}

#[test]
fn dropped_space_leaves_composition_untouched() {
    let state = apply_all(1, [type_on(0), DomainEvent::DropSpace]);
    assert_eq!(state.connections[0].state, ConnState::Composing);
    assert_eq!(state.document_spaces, 0);
    assert!(!state.last_inserted);
    assert!(!state.last_converted);
}

#[test]
fn resource_exhaustion_absorbs_further_idle_spaces() {
    let mut events = Vec::new();
    for _ in 0..MAX_DOCUMENT_SPACES {
        events.push(space_on(1 << 0));
    }
    events.push(space_on(1 << 0));
    let state = apply_all(1, events);
    assert_eq!(state.document_spaces, MAX_DOCUMENT_SPACES);
    assert!(state.last_absorbed);
}

#[test]
fn out_of_range_connection_is_ignored() {
    let state = apply_all(1, [type_on(7), space_on(1 << 2)]);
    assert_eq!(state.connections[0].state, ConnState::Idle);
    assert_eq!(state.document_spaces, 0);
}

#[test]
fn disconnected_connection_ignores_space() {
    let state = apply_all(
        2,
        [
            type_on(0),
            DomainEvent::Disconnect { connection: 0 },
            space_on(1 << 0),
        ],
    );
    assert!(!state.connections[0].live);
    assert!(!state.last_inserted);
    assert!(!state.last_converted);
    assert_eq!(state.document_spaces, 0);
}

#[test]
fn oracle_source_has_no_production_imports() {
    let source = include_str!("space_key_dispatch_oracle.rs");
    for forbidden in [
        "crate::dispatch",
        "crate::session",
        "crate::server",
        "sakura_core::keymap",
        "KeyMap",
        "idle_space_commit",
        "Dispatcher",
        "SessionTable",
    ] {
        assert!(
            !source.contains(forbidden),
            "oracle must not mention {forbidden}"
        );
    }
    let dir = verification_dir();
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("oracle-provenance.md"),
        "# Oracle provenance\n\n\
source: `crates/sakura-engine/src/space_key_dispatch_oracle.rs`\n\n\
static production-import scan: pass\n\n\
forbidden tokens checked: crate::dispatch, crate::session, crate::server, \
sakura_core::keymap, KeyMap, idle_space_commit, Dispatcher, SessionTable\n\n\
        Expected values come from `verification/space-key-dispatch/requirements.md`, \
not from observed production OutputBuf commits.\n",
    )
    .expect("provenance");
}

#[test]
fn adversarial_unfenced_mutant_is_rejected_by_the_oracle() {
    let events = [type_on(0), space_on(0b11)];
    let oracle = apply_all(2, events);
    let mutant = unfenced_mutant(2, &events);
    assert!(no_dual_effect(&oracle));
    assert!(
        mutant.last_inserted && mutant.last_converted,
        "the mutant must exhibit the production dual-delivery class"
    );
    assert!(
        !no_dual_effect(&mutant),
        "canary: no_dual_effect must reject insert/\\convert, not always return true"
    );
    assert_ne!(
        (oracle.last_inserted, oracle.last_converted),
        (mutant.last_inserted, mutant.last_converted)
    );
}

#[test]
fn canary_no_dual_effect_false_when_both_effects_observed() {
    let mut state = OracleState::new(2);
    state.last_inserted = true;
    state.last_converted = true;
    assert!(!no_dual_effect(&state));
}

/// Production-shaped mutant: idle Space always inserts, even if a peer is
/// converting. Kept in the test crate so requirements stay fixed.
fn unfenced_mutant(actors: usize, events: &[DomainEvent]) -> OracleState {
    let mut state = OracleState::new(actors);
    for event in events {
        match *event {
            DomainEvent::Space { targets } => {
                state.logical_time += 1;
                state.last_inserted = false;
                state.last_converted = false;
                state.last_absorbed = false;
                for index in 0..state.actors {
                    if (targets & (1 << index)) == 0 {
                        continue;
                    }
                    match state.connections[index].state {
                        ConnState::Composing | ConnState::Converting => {
                            state.connections[index].state = ConnState::Converting;
                            state.last_converted = true;
                            state.conversions += 1;
                        }
                        ConnState::Idle => {
                            state.last_inserted = true;
                            state.document_spaces += 1;
                        }
                    }
                }
            }
            other => apply(&mut state, other),
        }
    }
    state
}

#[test]
fn shrink_canary_reduces_a_known_failing_sequence() {
    let seed = 0x5350_4143_4520_0801u64;
    let mut random = seed;
    let mut found = None;
    for _ in 0..64 {
        let length = 3 + ((random as usize) % 5);
        let mut events = Vec::new();
        for _ in 0..length {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            events.push((random % 11) as u8);
        }
        if canary_broken(&events) {
            found = Some(events);
            break;
        }
    }
    let events = found.expect("canary generator must hit the broken predicate");
    let shrunk = shrink_canary(&events);
    assert_eq!(
        shrunk,
        vec![7, 3],
        "shrink infrastructure must reach the 2-byte witness"
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("verification/space-key-dispatch/pbt-canary-shrunk-counterexample.md");
    std::fs::create_dir_all(path.parent().expect("dir")).expect("dir");
    std::fs::write(
        path,
        format!("# Shrink canary\n\nseed: {seed}\n\nshrunk: {shrunk:?}\n"),
    )
    .expect("canary shrink");
}

fn canary_broken(events: &[u8]) -> bool {
    events.windows(2).any(|pair| pair == [7, 3])
}

fn shrink_canary(events: &[u8]) -> Vec<u8> {
    let mut current = events.to_vec();
    loop {
        let mut progressed = false;
        for index in 0..current.len() {
            let mut candidate = current.clone();
            candidate.remove(index);
            if canary_broken(&candidate) {
                current = candidate;
                progressed = true;
                break;
            }
        }
        if !progressed {
            break;
        }
    }
    current
}

#[test]
fn oracle_pbt_never_dual_effect_and_persists_seed() {
    const SEED: u64 = 0x5350_4143_4520_0816;
    let mut random = SEED;
    let mut seen = [[false; 2]; ATOM_COUNT];
    for _ in 0..512usize {
        let actors = 1 + ((random as usize) % 3);
        let length = 3 + ((random as usize) % 8);
        let mut state = OracleState::new(actors);
        let mut events = Vec::new();
        for _ in 0..length {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let event = generate_event(random, actors as u8);
            for (index, condition) in atomic_conditions(&state, event).into_iter().enumerate() {
                seen[index][usize::from(condition.value)] = true;
            }
            events.push(event);
            apply(&mut state, event);
            assert!(no_dual_effect(&state), "oracle dual-effect on {events:?}");
            assert!(state.document_spaces <= MAX_DOCUMENT_SPACES);
        }
    }
    let mut exhaust = OracleState::new(1);
    for _ in 0..=MAX_DOCUMENT_SPACES {
        let event = space_on(1 << 0);
        for (index, condition) in atomic_conditions(&exhaust, event).into_iter().enumerate() {
            seen[index][usize::from(condition.value)] = true;
        }
        apply(&mut exhaust, event);
    }
    let mut dead = OracleState::new(1);
    let die = DomainEvent::Disconnect { connection: 0 };
    for (index, condition) in atomic_conditions(&dead, die).into_iter().enumerate() {
        seen[index][usize::from(condition.value)] = true;
    }
    apply(&mut dead, die);
    let after = space_on(1 << 0);
    for (index, condition) in atomic_conditions(&dead, after).into_iter().enumerate() {
        seen[index][usize::from(condition.value)] = true;
    }
    write_oracle_pbt(SEED, None);
    let covered = seen.into_iter().flatten().filter(|value| *value).count();
    assert_eq!(
        covered,
        ATOM_COUNT * 2,
        "C2 polarities {covered}/{} {seen:?}",
        ATOM_COUNT * 2
    );
    write_c2_report(&seen);
}

fn generate_event(random: u64, actors: u8) -> DomainEvent {
    let connection = (random % u64::from(actors)) as u8;
    match random % 11 {
        0 | 1 => DomainEvent::Type { connection },
        2 => DomainEvent::Space {
            targets: 1 << connection,
        },
        3 => DomainEvent::Space {
            targets: (1u8 << actors).saturating_sub(1),
        },
        4 => DomainEvent::Commit { connection },
        5 => DomainEvent::Cancel { connection },
        6 => DomainEvent::ReplaceContext { connection },
        7 => DomainEvent::CrashRestart { connection },
        8 => DomainEvent::Disconnect { connection },
        9 => DomainEvent::DropSpace,
        _ => DomainEvent::TimeoutSpace {
            targets: 1 << connection,
        },
    }
}

fn write_oracle_pbt(seed: u64, shrunk: Option<&[DomainEvent]>) {
    let dir = verification_dir();
    std::fs::create_dir_all(&dir).expect("verification dir");
    std::fs::write(dir.join("pbt-seed.txt"), format!("{seed}\n")).expect("seed");
    let body = match shrunk {
        Some(events) => format!(
            "# Shrunk oracle counterexample\n\nseed: {seed}\n\nevents: {events:?}\n"
        ),
        None => format!(
            "# No failing shrink\n\nseed: {seed}\n\nThe oracle campaign finished without a counterexample.\n"
        ),
    };
    std::fs::write(dir.join("pbt-shrunk-counterexample.md"), body).expect("shrink");
}

fn write_c2_report(seen: &[[bool; 2]; ATOM_COUNT]) {
    let dummy = OracleState::default();
    let names: Vec<&'static str> = atomic_conditions(&dummy, DomainEvent::DropSpace)
        .into_iter()
        .map(|atom: AtomicCondition| atom.id)
        .collect();
    let mut report = String::from(
        "# Atomic-condition (C2) coverage — Space-key-dispatch oracle\n\n\
Scope: `crates/sakura-engine/src/space_key_dispatch_oracle.rs` predicates in `atomic_conditions`.\n\
This is atomic-condition polarity coverage of the independent oracle, not MC/DC of `dispatch.rs`.\n\n\
| condition | false | true |\n|---|---|---|\n",
    );
    for (index, name) in names.iter().enumerate() {
        report.push_str(&format!(
            "| `{name}` | {} | {} |\n",
            seen[index][0], seen[index][1]
        ));
    }
    let covered = seen.iter().flatten().filter(|value| **value).count();
    report.push_str(&format!(
        "\nCovered polarities: {covered}/{} ({}%). Seed `0x5350_4143_4520_0816`. Cases: 512.\n",
        ATOM_COUNT * 2,
        (covered * 100) / (ATOM_COUNT * 2)
    ));
    let dir = verification_dir().join("coverage");
    std::fs::create_dir_all(&dir).expect("coverage dir");
    std::fs::write(dir.join("c2-report.md"), report).expect("c2 report");
}

fn verification_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("verification/space-key-dispatch")
}
