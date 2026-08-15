use crate::shift_latin_oracle::{apply_all, atomic_conditions, DomainEvent, OracleState};

fn shift(character: char) -> DomainEvent {
    DomainEvent::ShiftLatin(character)
}

fn latin(character: char) -> DomainEvent {
    DomainEvent::Latin(character)
}

fn backspace(shift: bool) -> DomainEvent {
    DomainEvent::Backspace { shift }
}

#[test]
fn reported_aiueo_repair_keeps_press_order() {
    let state = apply_all([
        shift('A'),
        shift('I'),
        shift('U'),
        shift('E'),
        shift('O'),
        backspace(true),
        shift('O'),
    ]);
    assert_eq!(state.composing, "AIUEO");
    assert_eq!(state.cursor, 5);
    assert!(state.english_latched);
    assert_ne!(
        state.composing, "AIUOEO",
        "the reported defect class is not the oracle expectation"
    );
}

#[test]
fn unshifted_backspace_after_shift_latin_is_the_same_repair() {
    let state = apply_all([
        shift('A'),
        shift('I'),
        shift('U'),
        shift('E'),
        shift('O'),
        backspace(false),
        shift('O'),
    ]);
    assert_eq!(state.composing, "AIUEO");
}

#[test]
fn leftover_o_after_invisible_left_is_not_aiueo_repair() {
    let defect_shaped = apply_all([
        shift('A'),
        shift('I'),
        shift('U'),
        shift('E'),
        shift('O'),
        DomainEvent::Left,
        backspace(false),
        shift('O'),
        shift('O'),
    ]);
    assert_eq!(
        defect_shaped.composing, "AIUOOO",
        "Left then delete-E then two O keystrokes is not the AIUEO repair"
    );
    assert_ne!(defect_shaped.composing, "AIUOEO");
}

#[test]
fn mid_buffer_backspace_deletes_the_character_before_the_caret() {
    let state = apply_all([
        shift('A'),
        shift('I'),
        shift('U'),
        shift('E'),
        shift('O'),
        DomainEvent::Left,
        DomainEvent::Left,
        backspace(true),
        shift('U'),
    ]);
    assert_eq!(state.composing, "AIUEO");
    assert_eq!(state.cursor, 3);
}

#[test]
fn emptying_the_buffer_releases_the_english_latch() {
    let state = apply_all([
        shift('A'),
        shift('I'),
        backspace(true),
        backspace(true),
        latin('k'),
    ]);
    assert!(state.composing.is_empty());
    assert!(!state.english_latched);
}

#[test]
fn convert_then_backspace_keeps_the_latin_buffer() {
    let state = apply_all([
        shift('A'),
        shift('I'),
        DomainEvent::Convert { shift: false },
        backspace(true),
        shift('U'),
    ]);
    assert_eq!(state.composing, "AIU");
    assert!(!state.converting);
}

#[test]
fn convert_then_backspace_then_retype_keeps_aiueo_press_order() {
    let state = apply_all([
        shift('A'),
        shift('I'),
        shift('U'),
        shift('E'),
        shift('O'),
        DomainEvent::Convert { shift: false },
        backspace(true),
        backspace(true),
        shift('O'),
    ]);
    assert_eq!(state.composing, "AIUEO");
    assert!(!state.converting);
    assert_ne!(state.composing, "AIUOEO");
}

#[test]
fn convert_then_left_backspace_then_retype_is_not_the_aiuoeo_defect() {
    let state = apply_all([
        shift('A'),
        shift('I'),
        shift('U'),
        shift('E'),
        shift('O'),
        DomainEvent::Convert { shift: false },
        backspace(false),
        DomainEvent::Left,
        backspace(true),
        shift('E'),
    ]);
    assert_eq!(state.composing, "AIUEO");
    assert_ne!(state.composing, "AIUOEO");
    assert_ne!(state.composing, "AIUOE");
}

#[test]
fn first_unshifted_letter_does_not_start_english() {
    let state = apply_all([latin('a'), latin('i')]);
    assert_eq!(state.composing, "");
    assert!(!state.english_latched);
}

#[test]
fn latch_keeps_following_unshifted_ascii_in_press_order() {
    let state = apply_all([shift('A'), latin('I'), latin('U'), latin('E'), latin('O')]);
    assert_eq!(state.composing, "AIUEO");
}

#[test]
fn commit_moves_composing_to_committed_and_clears_the_latch() {
    let state = apply_all([shift('A'), shift('I'), DomainEvent::Commit, shift('U')]);
    assert_eq!(state.committed, "AI");
    assert_eq!(state.composing, "U");
    assert!(state.english_latched);
}

#[test]
fn oracle_c2_campaign_sees_every_atomic_polarity() {
    const SEED: u64 = 0x534C_4154_494E_0001;
    let events = [
        shift('A'),
        shift('I'),
        shift('U'),
        shift('E'),
        shift('O'),
        latin('X'),
        latin('n'),
        backspace(true),
        backspace(false),
        DomainEvent::Delete { shift: true },
        DomainEvent::Left,
        DomainEvent::Right,
        DomainEvent::Home,
        DomainEvent::End,
        DomainEvent::Convert { shift: false },
        DomainEvent::Cancel,
        DomainEvent::Commit,
    ];
    let mut seen = [[false; 2]; 8];
    let mut random = SEED;
    let mut state = OracleState::default();
    for _ in 0..2_048 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let event = events[(random as usize) % events.len()];
        for (index, condition) in atomic_conditions(&state, event).into_iter().enumerate() {
            seen[index][usize::from(condition.value)] = true;
        }
        crate::shift_latin_oracle::apply(&mut state, event);
    }
    let covered = seen.into_iter().flatten().filter(|value| *value).count();
    assert_eq!(covered, 16, "oracle C2 polarity coverage {seen:?}");
    let ids = [
        "composing_empty",
        "english_latched",
        "converting",
        "event_shifted",
        "event_latin_letter",
        "cursor_at_start",
        "cursor_at_end",
        "cursor_interior",
    ];
    let mut report = String::from("# Atomic-condition (C2) coverage — Shift-Latin oracle\n\n");
    report.push_str("Scope: `crates/sakura-engine/src/shift_latin_oracle.rs` predicates in `atomic_conditions`.\n");
    report.push_str("This is atomic-condition polarity coverage of the independent oracle, not MC/DC of the whole workspace and not line coverage claimed as C2.\n\n");
    report.push_str("| condition | false | true |\n|---|---|---|\n");
    for (index, id) in ids.into_iter().enumerate() {
        report.push_str(&format!(
            "| `{id}` | {} | {} |\n",
            seen[index][0], seen[index][1]
        ));
    }
    report.push_str(&format!(
        "\nCovered polarities: {covered}/16 (100%). Seed `{SEED:#018x}`. Cases: 2048.\n\n"
    ));
    report.push_str("Production predicates exercised by named `shift_latin_order` tests (boolean, not llvm-cov):\n\n");
    report.push_str("| production predicate | evidence |\n|---|---|\n");
    report.push_str("| Shift+letter starts English latch | `production_aiueo_shift_backspace_retype_keeps_press_order` |\n");
    report.push_str("| Shift+Backspace consumed while composing | `contract::send_key_contract_consumes_shift_backspace_during_english` |\n");
    report.push_str("| Backspace deletes the raw character before the caret | `production_left_then_backspace_deletes_the_character_before_the_caret` |\n");
    report.push_str("| Retype after end-delete is not AIUOEO | `production_aiueo_shift_backspace_retype_keeps_press_order` |\n");
    report.push_str("| Empty composition releases the latch | `emptying_the_buffer_releases_the_english_latch` |\n");
    report.push_str("| Convert then backspace then retype keeps AIUEO | `convert_then_backspace_then_retype_keeps_aiueo_press_order` |\n");
    report.push_str("| Resync required for dictionary convert | `resync_is_required_for_shifted_ascii_dictionary_conversion` |\n");
    report.push_str("\nLine/region coverage of production functions is **not** this table. See `llvm-cov-report.md`.\n");
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../verification/shift-latin-order/coverage");
    std::fs::create_dir_all(&dir).expect("coverage directory");
    std::fs::write(dir.join("c2-report.md"), report).expect("c2 report");
}
