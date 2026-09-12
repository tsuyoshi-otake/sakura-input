use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use sakura_core::keymap::{KeyMap, Preset, State};
use sakura_proto::{
    ErrorCode, KeyCode, KeyInput, Modifiers, OutputBuf, Request, Response, SessionId,
    MAX_PREEDIT_BYTES,
};

use crate::dictionary::ConversionService;
use crate::dispatch::{Dispatcher, Reply};
use crate::shift_latin_oracle::{apply, apply_all, atomic_conditions, DomainEvent, OracleState};

fn char_key(character: char) -> KeyInput {
    KeyInput {
        code: KeyCode::Char,
        ch: Some(character),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn shifted_char_key(character: char) -> KeyInput {
    KeyInput {
        modifiers: Modifiers::SHIFT,
        ..char_key(character)
    }
}

fn named_key(code: KeyCode, modifiers: Modifiers) -> KeyInput {
    KeyInput {
        code,
        ch: None,
        modifiers,
        repeat: false,
        test_only: false,
    }
}

fn create_session(dispatcher: &mut Dispatcher, out: &mut OutputBuf) -> SessionId {
    match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "shift-latin-order.exe".to_string(),
        },
        out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected session, got {other:?}"),
    }
}

fn send(dispatcher: &mut Dispatcher, session: SessionId, key: KeyInput, out: &mut OutputBuf) {
    assert_eq!(
        dispatcher.dispatch(&Request::SendKey { session, key }, out),
        Reply::Output
    );
}

fn production_visible(out: &OutputBuf) -> String {
    let mut text = String::new();
    if let Some(commit) = out.commit_text() {
        text.push_str(commit);
    }
    text.push_str(out.preedit_text());
    text
}

fn event_to_key(event: DomainEvent) -> Option<KeyInput> {
    match event {
        DomainEvent::ShiftLatin(character) => Some(shifted_char_key(character)),
        DomainEvent::Latin(character) => Some(char_key(character)),
        DomainEvent::Backspace { shift } => Some(named_key(
            KeyCode::Backspace,
            if shift {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            },
        )),
        DomainEvent::Delete { shift } => Some(named_key(
            KeyCode::Delete,
            if shift {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            },
        )),
        DomainEvent::Left => Some(named_key(KeyCode::Left, Modifiers::NONE)),
        DomainEvent::Right => Some(named_key(KeyCode::Right, Modifiers::NONE)),
        DomainEvent::Home => Some(named_key(KeyCode::Home, Modifiers::NONE)),
        DomainEvent::End => Some(named_key(KeyCode::End, Modifiers::NONE)),
        DomainEvent::Convert { shift } => Some(named_key(
            KeyCode::Space,
            if shift {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            },
        )),
        DomainEvent::Cancel => Some(named_key(KeyCode::Escape, Modifiers::NONE)),
        DomainEvent::Commit => Some(named_key(KeyCode::Enter, Modifiers::NONE)),
    }
}

fn format_events(events: &[DomainEvent]) -> String {
    events
        .iter()
        .map(|event| format!("{event:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn verification_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../verification/shift-latin-order")
}

fn write_pbt_artifacts(seed: u64, shrunk: Option<(&[DomainEvent], &str, &str)>) {
    let dir = verification_dir();
    fs::create_dir_all(&dir).expect("verification directory");
    fs::write(dir.join("pbt-seed.txt"), format!("{seed}\n")).expect("pbt seed");
    let body = match shrunk {
        Some((events, expected, actual)) => format!(
            "# Shrunk counterexample\n\nseed: {seed}\n\nevents: {}\n\nexpected: {expected:?}\nactual: {actual:?}\n",
            format_events(events)
        ),
        None => format!(
            "# No failing shrink\n\nseed: {seed}\n\nThe property campaign finished without a counterexample.\n"
        ),
    };
    fs::write(dir.join("pbt-shrunk-counterexample.md"), body).expect("pbt shrink");
}

fn shrink_events(events: &[DomainEvent], expected: &str, actual: &str) -> Vec<DomainEvent> {
    let mut best = events.to_vec();
    let mut changed = true;
    while changed && best.len() > 1 {
        changed = false;
        for index in 0..best.len() {
            let mut candidate = best.clone();
            candidate.remove(index);
            let oracle = apply_all(candidate.iter().copied());
            if oracle.visible() == expected && production_replay(&candidate) == actual {
                best = candidate;
                changed = true;
                break;
            }
        }
    }
    best
}

fn production_replay(events: &[DomainEvent]) -> String {
    production_replay_on(&mut Dispatcher::new().expect("shipped defaults"), events)
}

fn production_replay_on(dispatcher: &mut Dispatcher, events: &[DomainEvent]) -> String {
    let mut out = OutputBuf::new();
    let session = create_session(dispatcher, &mut out);
    let mut committed = String::new();
    for event in events {
        let Some(key) = event_to_key(*event) else {
            continue;
        };
        send(dispatcher, session, key, &mut out);
        if let Some(text) = out.commit_text() {
            committed.push_str(text);
        }
    }
    let mut visible = committed;
    visible.push_str(out.preedit_text());
    visible
}

fn english_conversion_dispatcher() -> Dispatcher {
    let source = concat!(
        "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "claude\tClaude\t0\t0\t100\t100\tit\tfixture\n",
        "claude\tClaude Code\t0\t0\t150\t150\tit\tfixture\n",
        "openai\tOpenAI\t0\t0\t100\t100\tit\tfixture\n",
    );
    let entries = dictc::parse_entries("shift-latin-order.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    let conversion = Arc::new(
        ConversionService::from_static_bytes(image).expect("shift-latin conversion fixture"),
    );
    Dispatcher::new_with_conversion(conversion).expect("shipped defaults")
}

fn assert_oracle_and_production(events: &[DomainEvent], expected_composing: &str) {
    let oracle = apply_all(events.iter().copied());
    assert_eq!(
        oracle.composing,
        expected_composing,
        "oracle events {}",
        format_events(events)
    );
    let actual = production_replay(events);
    let expected_visible = oracle.visible();
    assert_eq!(
        actual,
        expected_visible,
        "production diverged on {}",
        format_events(events)
    );
}

#[test]
fn production_aiueo_shift_backspace_retype_keeps_press_order() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Backspace { shift: true },
        DomainEvent::ShiftLatin('O'),
    ];
    assert_oracle_and_production(&events, "AIUEO");
    assert_ne!(production_replay(&events), "AIUOEO");
}

#[test]
fn production_aiueo_unshifted_backspace_retype_keeps_press_order() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Backspace { shift: false },
        DomainEvent::ShiftLatin('O'),
    ];
    assert_oracle_and_production(&events, "AIUEO");
}

#[test]
fn production_left_moves_the_visible_raw_caret() {
    let mut dispatcher = Dispatcher::new().expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out);
    for character in ['A', 'I', 'U', 'E', 'O'] {
        send(
            &mut dispatcher,
            session,
            shifted_char_key(character),
            &mut out,
        );
    }
    send(
        &mut dispatcher,
        session,
        named_key(KeyCode::Left, Modifiers::NONE),
        &mut out,
    );
    assert_eq!(out.preedit_text(), "AIUEO");
    assert_eq!(out.to_output().preedit.expect("preedit").cursor, 4);
}

#[test]
fn production_left_then_backspace_deletes_the_character_before_the_caret() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Left,
        DomainEvent::Backspace { shift: false },
        DomainEvent::ShiftLatin('E'),
    ];
    assert_oracle_and_production(&events, "AIUEO");
    assert_ne!(production_replay(&events), "AIUOEO");
}

#[test]
fn production_convert_then_backspace_then_retype_keeps_aiueo_press_order() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Convert { shift: false },
        DomainEvent::Backspace { shift: true },
        DomainEvent::Backspace { shift: true },
        DomainEvent::ShiftLatin('O'),
    ];
    assert_oracle_and_production(&events, "AIUEO");
    assert_ne!(production_replay(&events), "AIUOEO");
}

#[test]
fn production_convert_then_left_backspace_then_retype_is_not_aiuoeo() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Convert { shift: false },
        DomainEvent::Backspace { shift: false },
        DomainEvent::Left,
        DomainEvent::Backspace { shift: true },
        DomainEvent::ShiftLatin('E'),
    ];
    assert_oracle_and_production(&events, "AIUEO");
    assert_ne!(production_replay(&events), "AIUOEO");
}

#[test]
fn resync_is_required_for_shifted_ascii_dictionary_conversion() {
    let mut dispatcher = english_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out);
    for character in "CLAUDE".chars() {
        send(
            &mut dispatcher,
            session,
            shifted_char_key(character),
            &mut out,
        );
    }
    assert_eq!(out.preedit_text(), "CLAUDE");
    send(
        &mut dispatcher,
        session,
        named_key(KeyCode::Space, Modifiers::NONE),
        &mut out,
    );
    assert!(
        out.consumed,
        "Space after a dictionary English term must stay in the engine"
    );
    assert_eq!(
        out.preedit_text(),
        "CLAUDE ",
        "Space is a half-width word separator, not a conversion trigger"
    );
    assert_eq!(out.candidate_kind(), None);
    assert!(
        !out.preedit_text().contains('\u{3000}'),
        "the English word gap must stay U+0020"
    );
}

#[test]
fn convert_then_backspace_then_retype_keeps_claude_latin_order() {
    let mut dispatcher = english_conversion_dispatcher();
    let events = [
        DomainEvent::ShiftLatin('C'),
        DomainEvent::ShiftLatin('L'),
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('D'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::Convert { shift: false },
        DomainEvent::Backspace { shift: true },
        DomainEvent::Backspace { shift: true },
        DomainEvent::ShiftLatin('E'),
    ];
    let oracle = apply_all(events.iter().copied());
    assert_eq!(oracle.composing, "CLAUDE");
    assert_eq!(production_replay_on(&mut dispatcher, &events), "CLAUDE");
    assert_ne!(production_replay_on(&mut dispatcher, &events), "CLAUED");
}

#[test]
fn shift_latin_convert_cancel_then_edit_pbt_matches_oracle_and_persists_seed() {
    const SEED: u64 = 0x534C_4356_2026_0815;
    let terms: [&[char]; 2] = [
        &['C', 'L', 'A', 'U', 'D', 'E'],
        &['O', 'P', 'E', 'N', 'A', 'I'],
    ];
    let alphabet = [
        DomainEvent::Backspace { shift: true },
        DomainEvent::Backspace { shift: false },
        DomainEvent::Delete { shift: true },
        DomainEvent::Left,
        DomainEvent::Right,
        DomainEvent::Home,
        DomainEvent::End,
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::Cancel,
    ];
    let mut random = SEED;
    for case in 0..128usize {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let term = terms[(random as usize) % terms.len()];
        let mut events: Vec<DomainEvent> =
            term.iter().copied().map(DomainEvent::ShiftLatin).collect();
        events.push(DomainEvent::Convert { shift: false });
        events.push(DomainEvent::Backspace { shift: true });
        let length = 3 + ((random as usize) % 8);
        let mut oracle = apply_all(events.iter().copied());
        for _ in 0..length {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let mut event = alphabet[(random as usize) % alphabet.len()];
            if matches!(event, DomainEvent::ShiftLatin(_)) && !oracle.english_latched {
                event = DomainEvent::ShiftLatin(term[0]);
            }
            events.push(event);
            apply(&mut oracle, event);
        }
        let actual = production_replay_on(&mut english_conversion_dispatcher(), &events);
        if actual != oracle.visible() {
            let shrunk = shrink_events(&events, &oracle.visible(), &actual);
            write_convert_pbt_artifacts(SEED, Some((&shrunk, &oracle.visible(), &actual)));
            panic!(
                "convert case {case}: production {:?} != oracle {:?} on {}",
                actual,
                oracle.visible(),
                format_events(&shrunk)
            );
        }
    }
    write_convert_pbt_artifacts(SEED, None);
}

fn write_convert_pbt_artifacts(seed: u64, shrunk: Option<(&[DomainEvent], &str, &str)>) {
    let dir = verification_dir();
    fs::create_dir_all(&dir).expect("verification directory");
    fs::write(dir.join("pbt-convert-seed.txt"), format!("{seed}\n")).expect("convert pbt seed");
    let body = match shrunk {
        Some((events, expected, actual)) => format!(
            "# Shrunk convert-then-edit counterexample\n\nseed: {seed}\n\nevents: {}\n\nexpected: {expected:?}\nactual: {actual:?}\n",
            format_events(events)
        ),
        None => format!(
            "# No failing shrink\n\nseed: {seed}\n\nThe convert-then-cancel-then-edit campaign finished without a counterexample.\n"
        ),
    };
    fs::write(dir.join("pbt-convert-shrunk-counterexample.md"), body).expect("convert pbt shrink");
}

#[test]
fn production_home_then_backspace_is_a_noop_at_cursor_zero() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Home,
        DomainEvent::Backspace { shift: true },
    ];
    assert_oracle_and_production(&events, "AIUEO");
}

#[test]
fn production_home_then_insert_puts_the_letter_at_the_start() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Home,
        DomainEvent::ShiftLatin('X'),
    ];
    assert_oracle_and_production(&events, "XAIUEO");
    assert_ne!(production_replay(&events), "AIUEOX");
}

#[test]
fn production_delete_forward_while_english_removes_the_letter_at_the_caret() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Home,
        DomainEvent::Delete { shift: false },
        DomainEvent::ShiftLatin('A'),
    ];
    assert_oracle_and_production(&events, "AIUEO");
}

#[test]
fn production_delete_at_the_end_of_english_is_a_noop() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::End,
        DomainEvent::Delete { shift: true },
    ];
    assert_oracle_and_production(&events, "AI");
}

#[test]
fn production_right_and_end_after_left_restore_the_end_caret() {
    let mut dispatcher = Dispatcher::new().expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out);
    for character in ['A', 'I', 'U'] {
        send(
            &mut dispatcher,
            session,
            shifted_char_key(character),
            &mut out,
        );
    }
    send(
        &mut dispatcher,
        session,
        named_key(KeyCode::Left, Modifiers::NONE),
        &mut out,
    );
    assert_eq!(out.to_output().preedit.expect("preedit").cursor, 2);
    send(
        &mut dispatcher,
        session,
        named_key(KeyCode::Right, Modifiers::NONE),
        &mut out,
    );
    assert_eq!(out.to_output().preedit.expect("preedit").cursor, 3);
    send(
        &mut dispatcher,
        session,
        named_key(KeyCode::Home, Modifiers::NONE),
        &mut out,
    );
    send(
        &mut dispatcher,
        session,
        named_key(KeyCode::End, Modifiers::NONE),
        &mut out,
    );
    assert_eq!(out.preedit_text(), "AIU");
    assert_eq!(out.to_output().preedit.expect("preedit").cursor, 3);
}

#[test]
fn production_digit_and_punctuation_stay_in_english_press_order() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::Latin('1'),
        DomainEvent::Latin('-'),
        DomainEvent::Latin('2'),
        DomainEvent::Home,
        DomainEvent::ShiftLatin('X'),
    ];
    assert_oracle_and_production(&events, "XAI1-2");
}

#[test]
fn production_full_erase_then_retype_starts_a_fresh_english_buffer() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::Backspace { shift: true },
        DomainEvent::Backspace { shift: true },
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
    ];
    assert_oracle_and_production(&events, "UE");
}

#[test]
fn production_convert_cancel_then_home_backspace_keeps_press_order() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Convert { shift: false },
        DomainEvent::Backspace { shift: true },
        DomainEvent::Home,
        DomainEvent::Backspace { shift: true },
        DomainEvent::ShiftLatin('X'),
    ];
    assert_oracle_and_production(&events, "XAIUEO");
}

#[test]
fn production_non_ascii_exits_english_without_reordering_the_latin_prefix() {
    let mut dispatcher = Dispatcher::new().expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out);
    for character in ['A', 'I', 'U', 'E', 'O'] {
        send(
            &mut dispatcher,
            session,
            shifted_char_key(character),
            &mut out,
        );
    }
    assert_eq!(out.preedit_text(), "AIUEO");
    send(&mut dispatcher, session, char_key('あ'), &mut out);
    let after_exit = out.preedit_text().to_string();
    assert_ne!(after_exit, "AIUOEO");
    assert_ne!(
        after_exit, "AIUEO",
        "a non-ASCII key must leave the temporary English composition"
    );
    assert!(
        after_exit.contains('あ') || !after_exit.is_ascii(),
        "exit should feed the non-ASCII character, got {after_exit:?}"
    );
}

#[test]
fn production_convert_without_a_dictionary_beeps_and_keeps_latin_order() {
    let mut dispatcher = Dispatcher::new().expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out);
    for character in ['A', 'I', 'U', 'E', 'O'] {
        send(
            &mut dispatcher,
            session,
            shifted_char_key(character),
            &mut out,
        );
    }
    send(
        &mut dispatcher,
        session,
        named_key(KeyCode::Space, Modifiers::NONE),
        &mut out,
    );
    assert_ne!(out.preedit_text(), "AIUOEO");
    if out.beep {
        assert_eq!(
            out.preedit_text(),
            "AIUEO",
            "a failed convert must fall back to the raw English buffer"
        );
    } else {
        assert!(
            out.consumed,
            "Space after English must stay in the engine even without a dictionary hit"
        );
    }
}

#[test]
fn shift_latin_coverage_neighbor_pbt_matches_oracle_and_persists_seed() {
    const SEED: u64 = 0x534C_434F_2026_0815;
    let alphabet = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Latin('1'),
        DomainEvent::Latin('-'),
        DomainEvent::Backspace { shift: true },
        DomainEvent::Delete { shift: false },
        DomainEvent::Left,
        DomainEvent::Right,
        DomainEvent::Home,
        DomainEvent::End,
        DomainEvent::Cancel,
    ];
    let mut random = SEED;
    for case in 0..96usize {
        let mut events = vec![
            DomainEvent::ShiftLatin('A'),
            DomainEvent::ShiftLatin('I'),
            DomainEvent::ShiftLatin('U'),
            DomainEvent::ShiftLatin('E'),
            DomainEvent::ShiftLatin('O'),
        ];
        let mut oracle = apply_all(events.iter().copied());
        let length = 4 + ((random as usize) % 8);
        for _ in 0..length {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let mut event = alphabet[(random as usize) % alphabet.len()];
            if matches!(event, DomainEvent::Latin(_)) && !oracle.english_latched {
                event = DomainEvent::ShiftLatin('A');
            }
            events.push(event);
            apply(&mut oracle, event);
        }
        let actual = production_replay(&events);
        if actual != oracle.visible() {
            let shrunk = shrink_events(&events, &oracle.visible(), &actual);
            write_coverage_pbt_artifacts(SEED, Some((&shrunk, &oracle.visible(), &actual)));
            panic!(
                "coverage neighbor case {case}: production {:?} != oracle {:?} on {}",
                actual,
                oracle.visible(),
                format_events(&shrunk)
            );
        }
    }
    write_coverage_pbt_artifacts(SEED, None);
}

fn write_coverage_pbt_artifacts(seed: u64, shrunk: Option<(&[DomainEvent], &str, &str)>) {
    let dir = verification_dir();
    fs::create_dir_all(&dir).expect("verification directory");
    fs::write(dir.join("pbt-coverage-seed.txt"), format!("{seed}\n")).expect("coverage pbt seed");
    let body = match shrunk {
        Some((events, expected, actual)) => format!(
            "# Shrunk coverage-neighbor counterexample\n\nseed: {seed}\n\nevents: {}\n\nexpected: {expected:?}\nactual: {actual:?}\n",
            format_events(events)
        ),
        None => format!(
            "# No failing shrink\n\nseed: {seed}\n\nThe Home/Delete/digit/convert-cancel coverage campaign finished without a counterexample.\n"
        ),
    };
    fs::write(dir.join("pbt-coverage-shrunk-counterexample.md"), body)
        .expect("coverage pbt shrink");
}

#[test]
fn production_unshifted_letter_does_not_start_english() {
    let mut dispatcher = Dispatcher::new().expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out);
    send(&mut dispatcher, session, char_key('a'), &mut out);
    assert_ne!(out.preedit_text(), "a");
    assert_ne!(out.preedit_text(), "A");
    assert!(
        !out.preedit_text().is_empty(),
        "unshifted A on an idle session is romaji, not an English latch"
    );
}

#[test]
fn production_latched_unshifted_letters_stay_in_press_order() {
    let events = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::Latin('I'),
        DomainEvent::Latin('U'),
        DomainEvent::Latin('E'),
        DomainEvent::Latin('O'),
        DomainEvent::Backspace { shift: true },
        DomainEvent::Latin('O'),
    ];
    assert_oracle_and_production(&events, "AIUEO");
}

#[test]
fn shift_latin_order_pbt_matches_oracle_and_persists_seed() {
    const SEED: u64 = 0x534C_4253_2026_0815;
    let alphabet = [
        DomainEvent::ShiftLatin('A'),
        DomainEvent::ShiftLatin('I'),
        DomainEvent::ShiftLatin('U'),
        DomainEvent::ShiftLatin('E'),
        DomainEvent::ShiftLatin('O'),
        DomainEvent::Latin('X'),
        DomainEvent::Backspace { shift: true },
        DomainEvent::Backspace { shift: false },
        DomainEvent::Delete { shift: true },
        DomainEvent::Left,
        DomainEvent::Right,
        DomainEvent::Home,
        DomainEvent::End,
        DomainEvent::Cancel,
        DomainEvent::Commit,
    ];
    let mut random = SEED;
    let mut seen = [[false; 2]; 8];
    for case in 0..256usize {
        let mut events = Vec::new();
        let mut oracle = OracleState::default();
        let length = 4 + ((random as usize) % 10);
        for _ in 0..length {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let mut event = alphabet[(random as usize) % alphabet.len()];
            if matches!(event, DomainEvent::Latin(_)) && !oracle.english_latched {
                event = DomainEvent::ShiftLatin('A');
            }
            for (index, condition) in atomic_conditions(&oracle, event).into_iter().enumerate() {
                seen[index][usize::from(condition.value)] = true;
            }
            events.push(event);
            apply(&mut oracle, event);
        }
        let actual = production_replay(&events);
        if actual != oracle.visible() {
            let shrunk = shrink_events(&events, &oracle.visible(), &actual);
            write_pbt_artifacts(SEED, Some((&shrunk, &oracle.visible(), &actual)));
            panic!(
                "case {case}: production {:?} != oracle {:?} on {}",
                actual,
                oracle.visible(),
                format_events(&shrunk)
            );
        }
    }
    write_pbt_artifacts(SEED, None);
    let covered = seen.into_iter().flatten().filter(|value| *value).count();
    assert!(
        covered >= 12,
        "PBT should exercise most oracle polarities, saw {covered}/16 {seen:?}"
    );
}

mod contract {
    use super::*;

    #[test]
    fn send_key_contract_consumes_shift_backspace_during_english() {
        let mut dispatcher = Dispatcher::new().expect("shipped defaults");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out);
        send(&mut dispatcher, session, shifted_char_key('A'), &mut out);
        send(
            &mut dispatcher,
            session,
            named_key(KeyCode::Backspace, Modifiers::SHIFT),
            &mut out,
        );
        assert!(out.consumed, "Shift+Backspace must stay in the engine");
        assert_eq!(out.preedit_text(), "");
    }

    #[test]
    fn keymap_contract_shift_backspace_is_delete_back_while_composing() {
        let map = KeyMap::preset(Preset::MsIme).expect("preset");
        let key = named_key(KeyCode::Backspace, Modifiers::SHIFT);
        assert_eq!(
            map.lookup(State::Composing, &key),
            Some(sakura_core::keymap::Action::DeleteBack)
        );
    }

    #[test]
    fn duplicate_shift_backspace_is_idempotent_at_empty() {
        let mut dispatcher = Dispatcher::new().expect("shipped defaults");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out);
        send(&mut dispatcher, session, shifted_char_key('A'), &mut out);
        send(
            &mut dispatcher,
            session,
            named_key(KeyCode::Backspace, Modifiers::SHIFT),
            &mut out,
        );
        assert!(out.consumed);
        assert_eq!(production_visible(&out), "");
        send(
            &mut dispatcher,
            session,
            named_key(KeyCode::Backspace, Modifiers::SHIFT),
            &mut out,
        );
        assert_eq!(production_visible(&out), "");
    }

    #[test]
    fn dropped_unbound_ctrl_chord_does_not_reorder_latin() {
        let mut dispatcher = Dispatcher::new().expect("shipped defaults");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out);
        send(&mut dispatcher, session, shifted_char_key('A'), &mut out);
        send(
            &mut dispatcher,
            session,
            KeyInput {
                modifiers: Modifiers::CTRL,
                ..char_key('S')
            },
            &mut out,
        );
        assert!(!out.consumed);
        send(&mut dispatcher, session, shifted_char_key('I'), &mut out);
        assert_eq!(out.preedit_text(), "AI");
    }

    #[test]
    fn cancel_then_retype_starts_a_fresh_english_buffer() {
        let events = [
            DomainEvent::ShiftLatin('A'),
            DomainEvent::ShiftLatin('I'),
            DomainEvent::Cancel,
            DomainEvent::ShiftLatin('U'),
        ];
        assert_eq!(apply_all(events).composing, "U");
        assert_eq!(production_replay(&events), "U");
    }

    #[test]
    fn resource_exhaustion_rejects_an_overlong_insert_without_reordering() {
        let mut dispatcher = Dispatcher::new().expect("shipped defaults");
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out);
        send(&mut dispatcher, session, shifted_char_key('A'), &mut out);
        for _ in 1..MAX_PREEDIT_BYTES {
            let reply = dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: shifted_char_key('A'),
                },
                &mut out,
            );
            if !matches!(reply, Reply::Output) {
                break;
            }
        }
        let before = out.preedit_text().to_string();
        let overflow = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: shifted_char_key('Z'),
            },
            &mut out,
        );
        match overflow {
            Reply::Message(Response::Error(ErrorCode::TooLarge)) => {}
            Reply::Output => {
                let after = out.preedit_text();
                assert!(
                    !after.contains('Z') || after.chars().all(|ch| ch == 'A' || ch == 'Z'),
                    "overflow must not insert Z into a mid-buffer hole: {after:?}"
                );
            }
            other => panic!("unexpected overflow reply {other:?}"),
        }
        let _ = before;
    }

    #[test]
    fn crash_restart_is_a_new_dispatcher_with_empty_composition() {
        let mut first = Dispatcher::new().expect("shipped defaults");
        let mut out = OutputBuf::new();
        let session = create_session(&mut first, &mut out);
        send(&mut first, session, shifted_char_key('A'), &mut out);
        drop(first);
        let mut restarted = Dispatcher::new().expect("shipped defaults");
        let session = create_session(&mut restarted, &mut out);
        send(&mut restarted, session, shifted_char_key('I'), &mut out);
        assert_eq!(out.preedit_text(), "I");
    }

    #[test]
    fn timeout_does_not_apply_to_in_memory_key_dispatch() {
        // This path is synchronous SendKey. There is no timer, so a timeout
        // injection has no state to age; the contract is that a later key
        // still appends in order.
        let events = [
            DomainEvent::ShiftLatin('A'),
            DomainEvent::ShiftLatin('I'),
            DomainEvent::ShiftLatin('U'),
        ];
        assert_eq!(production_replay(&events), "AIU");
    }

    #[test]
    fn reordered_events_follow_the_oracle_not_the_original_press_intent() {
        let intended = [
            DomainEvent::ShiftLatin('A'),
            DomainEvent::ShiftLatin('I'),
            DomainEvent::ShiftLatin('U'),
        ];
        let reordered = [
            DomainEvent::ShiftLatin('A'),
            DomainEvent::ShiftLatin('U'),
            DomainEvent::ShiftLatin('I'),
        ];
        assert_eq!(production_replay(&intended), "AIU");
        assert_eq!(
            production_replay(&reordered),
            apply_all(reordered).visible()
        );
        assert_ne!(production_replay(&reordered), "AIU");
    }

    #[test]
    fn partial_failure_on_empty_backspace_leaves_later_keys_in_order() {
        let events = [
            DomainEvent::Backspace { shift: true },
            DomainEvent::ShiftLatin('A'),
            DomainEvent::ShiftLatin('I'),
        ];
        assert_oracle_and_production(&events, "AI");
    }
}
