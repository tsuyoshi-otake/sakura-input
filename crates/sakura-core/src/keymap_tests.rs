use super::*;

fn key(code: KeyCode, modifiers: Modifiers) -> KeyInput {
    KeyInput {
        code,
        ch: None,
        modifiers,
        repeat: false,
        test_only: false,
    }
}

fn ch(c: char, modifiers: Modifiers) -> KeyInput {
    KeyInput {
        code: KeyCode::Char,
        ch: Some(c),
        modifiers,
        repeat: false,
        test_only: false,
    }
}

fn ms_ime() -> KeyMap {
    KeyMap::preset(Preset::MsIme).expect("the shipped ms-ime preset must compile")
}

#[test]
fn every_shipped_preset_compiles() {
    for preset in Preset::ALL {
        let map = KeyMap::preset(preset).expect("preset must compile");
        assert!(!map.is_empty(), "{} is empty", preset.name());
        assert_eq!(Preset::from_name(preset.name()), Some(preset));
    }
}

#[test]
fn ms_ime_is_the_default_preset() {
    assert_eq!(Preset::default(), Preset::MsIme);
}

/// Every action name round-trips, so no config file can name an action
/// the parser cannot produce or the UI cannot display.
#[test]
fn action_names_round_trip() {
    for action in Action::ALL {
        assert_eq!(Action::from_name(action.name()), Some(action));
    }
    assert_eq!(Action::from_name("no_such_action"), None);
    // The pseudo-action is deliberately not an `Action`.
    assert_eq!(Action::from_name(UNBOUND), None);
}

#[test]
fn action_names_are_unique() {
    for (index, action) in Action::ALL.into_iter().enumerate() {
        for other in Action::ALL.into_iter().skip(index + 1) {
            assert_ne!(action.name(), other.name(), "duplicate name");
        }
    }
}

/// The bindings DESIGN 2 names explicitly for the `ms-ime` preset.
#[test]
fn ms_ime_matches_the_documented_windows_conventions() {
    let map = ms_ime();
    let cases: [(State, KeyInput, Action); 16] = [
        (
            State::Idle,
            key(KeyCode::HankakuZenkaku, Modifiers::NONE),
            Action::ImeToggle,
        ),
        (
            State::Idle,
            key(KeyCode::Muhenkan, Modifiers::NONE),
            Action::ModeKanaCycle,
        ),
        (
            State::Idle,
            key(KeyCode::Henkan, Modifiers::NONE),
            Action::Reconvert,
        ),
        (
            State::Idle,
            key(KeyCode::Backspace, Modifiers::CTRL),
            Action::UndoCommit,
        ),
        (
            State::Composing,
            key(KeyCode::Space, Modifiers::NONE),
            Action::Convert,
        ),
        (
            State::Composing,
            key(KeyCode::Enter, Modifiers::NONE),
            Action::Commit,
        ),
        (
            State::Composing,
            key(KeyCode::Escape, Modifiers::NONE),
            Action::Cancel,
        ),
        (
            State::Composing,
            key(KeyCode::Tab, Modifiers::NONE),
            Action::PredictNext,
        ),
        (
            State::Composing,
            key(KeyCode::Tab, Modifiers::SHIFT),
            Action::PredictPrev,
        ),
        (
            State::Composing,
            key(KeyCode::Enter, Modifiers::SHIFT),
            Action::CommitFirst,
        ),
        (
            State::Converting,
            key(KeyCode::Space, Modifiers::NONE),
            Action::CandidateNext,
        ),
        (
            State::Converting,
            key(KeyCode::Tab, Modifiers::NONE),
            Action::CandidateExpand,
        ),
        (
            State::Converting,
            key(KeyCode::Left, Modifiers::NONE),
            Action::SegmentPrev,
        ),
        (
            State::Converting,
            key(KeyCode::Left, Modifiers::SHIFT),
            Action::SegmentShrink,
        ),
        (
            State::Converting,
            key(KeyCode::Right, Modifiers::SHIFT),
            Action::SegmentGrow,
        ),
        (
            State::Predicting,
            key(KeyCode::Space, Modifiers::NONE),
            Action::Convert,
        ),
    ];
    for (state, event, expected) in cases {
        assert_eq!(
            map.lookup(state, &event),
            Some(expected),
            "{state:?} {:?}",
            event.code
        );
    }
}

/// Resolves one binding and asserts it against the exact expected
/// `Action`, not merely `is_some()` — a leaking key that got rebound to
/// the wrong action would pass an `is_some()` check just as happily as
/// one rebound correctly.
fn assert_binding(
    preset: Preset,
    state: State,
    code: KeyCode,
    modifiers: Modifiers,
    expected: Action,
) {
    let map = KeyMap::preset(preset).expect("preset must compile");
    assert_eq!(
            map.lookup(state, &key(code, modifiers)),
            Some(expected),
            "{} {state:?} {code:?} (modifiers {modifiers:?}) must resolve to {expected:?} (issue #16 finding E)",
            preset.name(),
        );
}

// Every named key issue #16 finding E identified as leaking to the host
// application while the IME owned a composition, conversion or focused
// suggestion list — `[composing]` page_up/page_down, `[converting]`
// delete/home/end/shift+tab/ctrl+delete, and `[predicting]`
// left/right/home/end/delete/shift+enter/f6-f10 — now resolves to a real
// action or `Action::Swallow` in the shipped `ms-ime` preset. Each case
// gets its own test so a future regression names exactly which binding
// broke, rather than reporting "some case in a table failed".

#[test]
fn ms_ime_composing_page_up_is_bound_to_swallow() {
    assert_binding(
        Preset::MsIme,
        State::Composing,
        KeyCode::PageUp,
        Modifiers::NONE,
        Action::Swallow,
    );
}

#[test]
fn ms_ime_composing_page_down_is_bound_to_swallow() {
    assert_binding(
        Preset::MsIme,
        State::Composing,
        KeyCode::PageDown,
        Modifiers::NONE,
        Action::Swallow,
    );
}

#[test]
fn ms_ime_composing_shift_backspace_deletes_back() {
    assert_binding(
        Preset::MsIme,
        State::Composing,
        KeyCode::Backspace,
        Modifiers::SHIFT,
        Action::DeleteBack,
    );
}

#[test]
fn atok_composing_shift_backspace_deletes_back() {
    assert_binding(
        Preset::Atok,
        State::Composing,
        KeyCode::Backspace,
        Modifiers::SHIFT,
        Action::DeleteBack,
    );
}

#[test]
fn ms_ime_converting_shift_backspace_cancels() {
    assert_binding(
        Preset::MsIme,
        State::Converting,
        KeyCode::Backspace,
        Modifiers::SHIFT,
        Action::Cancel,
    );
}

#[test]
fn ms_ime_converting_delete_is_bound_to_swallow() {
    assert_binding(
        Preset::MsIme,
        State::Converting,
        KeyCode::Delete,
        Modifiers::NONE,
        Action::Swallow,
    );
}

#[test]
fn ms_ime_converting_home_is_bound_to_segment_home() {
    assert_binding(
        Preset::MsIme,
        State::Converting,
        KeyCode::Home,
        Modifiers::NONE,
        Action::SegmentHome,
    );
}

#[test]
fn ms_ime_converting_end_is_bound_to_segment_end() {
    assert_binding(
        Preset::MsIme,
        State::Converting,
        KeyCode::End,
        Modifiers::NONE,
        Action::SegmentEnd,
    );
}

#[test]
fn ms_ime_converting_shift_tab_is_bound_to_swallow() {
    assert_binding(
        Preset::MsIme,
        State::Converting,
        KeyCode::Tab,
        Modifiers::SHIFT,
        Action::Swallow,
    );
}

#[test]
fn ms_ime_converting_ctrl_delete_is_bound_to_swallow() {
    assert_binding(
        Preset::MsIme,
        State::Converting,
        KeyCode::Delete,
        Modifiers::CTRL,
        Action::Swallow,
    );
}

#[test]
fn ms_ime_predicting_left_is_bound_to_caret_left() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::Left,
        Modifiers::NONE,
        Action::CaretLeft,
    );
}

#[test]
fn ms_ime_predicting_right_is_bound_to_caret_right() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::Right,
        Modifiers::NONE,
        Action::CaretRight,
    );
}

#[test]
fn ms_ime_predicting_home_is_bound_to_caret_home() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::Home,
        Modifiers::NONE,
        Action::CaretHome,
    );
}

#[test]
fn ms_ime_predicting_end_is_bound_to_caret_end() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::End,
        Modifiers::NONE,
        Action::CaretEnd,
    );
}

#[test]
fn ms_ime_predicting_delete_is_bound_to_delete_forward() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::Delete,
        Modifiers::NONE,
        Action::DeleteForward,
    );
}

#[test]
fn ms_ime_predicting_shift_enter_is_bound_to_commit_first() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::Enter,
        Modifiers::SHIFT,
        Action::CommitFirst,
    );
}

#[test]
fn ms_ime_predicting_f6_is_bound_to_transform_hiragana() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::F6,
        Modifiers::NONE,
        Action::TransformHiragana,
    );
}

#[test]
fn ms_ime_predicting_f7_is_bound_to_transform_katakana() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::F7,
        Modifiers::NONE,
        Action::TransformKatakana,
    );
}

#[test]
fn ms_ime_predicting_f8_is_bound_to_transform_half_katakana() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::F8,
        Modifiers::NONE,
        Action::TransformHalfKatakana,
    );
}

#[test]
fn ms_ime_predicting_f9_is_bound_to_transform_full_alnum() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::F9,
        Modifiers::NONE,
        Action::TransformFullAlnum,
    );
}

#[test]
fn ms_ime_predicting_f10_is_bound_to_transform_half_alnum() {
    assert_binding(
        Preset::MsIme,
        State::Predicting,
        KeyCode::F10,
        Modifiers::NONE,
        Action::TransformHalfAlnum,
    );
}

// Issue #16 finding G-1: `[predicting]` was the one ATOK state that
// forgot to restate `muhenkan` (ATOK repeats it per-state instead of
// inheriting `[global]`; see the comment above `[global]` in
// `data/keymap-atok.toml`), so muhenkan fell through to the host
// while a suggestion was focused. Asserting the exact `Action`, not
// merely `is_some()`, matters here: a wrong-but-present binding would
// pass a looser check while still misbehaving.
#[test]
fn atok_predicting_muhenkan_is_bound_to_mode_kana_cycle() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::Muhenkan,
        Modifiers::NONE,
        Action::ModeKanaCycle,
    );
}

// Issue #16 finding G-3: `[predicting]` also lacked caret movement,
// forward delete, shift+enter commit-first, and all five F6-F10
// surface transforms -- the same set MS-IME's `[predicting]` already
// had (see the `ms_ime_predicting_*` tests above). Each ATOK case
// gets its own test, mirroring that naming convention.
#[test]
fn atok_predicting_left_is_bound_to_caret_left() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::Left,
        Modifiers::NONE,
        Action::CaretLeft,
    );
}

#[test]
fn atok_predicting_right_is_bound_to_caret_right() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::Right,
        Modifiers::NONE,
        Action::CaretRight,
    );
}

#[test]
fn atok_predicting_home_is_bound_to_caret_home() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::Home,
        Modifiers::NONE,
        Action::CaretHome,
    );
}

#[test]
fn atok_predicting_end_is_bound_to_caret_end() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::End,
        Modifiers::NONE,
        Action::CaretEnd,
    );
}

#[test]
fn atok_predicting_delete_is_bound_to_delete_forward() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::Delete,
        Modifiers::NONE,
        Action::DeleteForward,
    );
}

#[test]
fn atok_predicting_shift_enter_is_bound_to_commit_first() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::Enter,
        Modifiers::SHIFT,
        Action::CommitFirst,
    );
}

#[test]
fn atok_predicting_f6_is_bound_to_transform_hiragana() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::F6,
        Modifiers::NONE,
        Action::TransformHiragana,
    );
}

#[test]
fn atok_predicting_f7_is_bound_to_transform_katakana() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::F7,
        Modifiers::NONE,
        Action::TransformKatakana,
    );
}

#[test]
fn atok_predicting_f8_is_bound_to_transform_half_katakana() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::F8,
        Modifiers::NONE,
        Action::TransformHalfKatakana,
    );
}

#[test]
fn atok_predicting_f9_is_bound_to_transform_full_alnum() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::F9,
        Modifiers::NONE,
        Action::TransformFullAlnum,
    );
}

#[test]
fn atok_predicting_f10_is_bound_to_transform_half_alnum() {
    assert_binding(
        Preset::Atok,
        State::Predicting,
        KeyCode::F10,
        Modifiers::NONE,
        Action::TransformHalfAlnum,
    );
}

#[test]
fn both_presets_bind_prediction_numbers_and_history_deletion() {
    // Both shipped presets let a focused suggestion list be committed by its
    // numbered slot and let a bad learned entry be deleted from the keyboard
    // (issue #16, finding G): ATOK's `[predicting]` section used to be nine
    // lines short of this and left both gaps unbound, silently leaking the
    // digit keys and Ctrl+Delete to the host application.
    for preset in [Preset::MsIme, Preset::Atok] {
        let map = KeyMap::preset(preset).expect("preset compiles");
        for (digit, action) in [
            ('1', Action::Candidate1),
            ('2', Action::Candidate2),
            ('3', Action::Candidate3),
            ('4', Action::Candidate4),
            ('5', Action::Candidate5),
            ('6', Action::Candidate6),
            ('7', Action::Candidate7),
            ('8', Action::Candidate8),
            ('9', Action::Candidate9),
        ] {
            assert_eq!(
                map.lookup(State::Predicting, &ch(digit, Modifiers::NONE)),
                Some(action),
                "{preset:?} digit {digit}",
            );
        }
        assert_eq!(
            map.lookup(State::Predicting, &key(KeyCode::Delete, Modifiers::CTRL)),
            Some(Action::DeletePredictionHistory),
            "{preset:?} ctrl+delete",
        );
    }
}

#[test]
fn both_presets_bind_conversion_numbers_for_explicit_focus() {
    // Conversion starts with a visible but unfocused list. The engine
    // owns the focus gate; both shipped maps must still expose the same
    // 1–9 actions once navigation, expansion, or paging has focused it.
    for preset in [Preset::MsIme, Preset::Atok] {
        let map = KeyMap::preset(preset).expect("preset compiles");
        for (digit, action) in [
            ('1', Action::Candidate1),
            ('2', Action::Candidate2),
            ('3', Action::Candidate3),
            ('4', Action::Candidate4),
            ('5', Action::Candidate5),
            ('6', Action::Candidate6),
            ('7', Action::Candidate7),
            ('8', Action::Candidate8),
            ('9', Action::Candidate9),
        ] {
            assert_eq!(
                map.lookup(State::Converting, &ch(digit, Modifiers::NONE)),
                Some(action),
                "{preset:?} converting digit {digit}",
            );
        }
    }
}

#[test]
fn ms_ime_mode_aliases_and_shift_space_are_bound() {
    let map = ms_ime();
    let cases = [
        (
            State::Idle,
            key(KeyCode::KanaMode, Modifiers::NONE),
            Action::ModeHiragana,
        ),
        (
            State::Idle,
            key(KeyCode::KanaMode, Modifiers::SHIFT),
            Action::ModeKatakana,
        ),
        (
            State::Idle,
            key(KeyCode::CapsLock, Modifiers::NONE),
            Action::ModeAlnumToggle,
        ),
        (
            State::Idle,
            key(KeyCode::CapsLock, Modifiers::CTRL),
            Action::ModeHiragana,
        ),
        (
            State::Idle,
            key(KeyCode::CapsLock, Modifiers::SHIFT),
            Action::ModeKatakana,
        ),
        (
            State::Idle,
            key(KeyCode::Muhenkan, Modifiers::SHIFT),
            Action::ModeAlnumWidthToggle,
        ),
        (
            State::Composing,
            key(KeyCode::Space, Modifiers::SHIFT),
            Action::Convert,
        ),
        (
            State::Converting,
            key(KeyCode::Space, Modifiers::SHIFT),
            Action::CandidatePrev,
        ),
        (
            State::Predicting,
            key(KeyCode::Space, Modifiers::SHIFT),
            Action::Convert,
        ),
    ];
    for (state, event, expected) in cases {
        assert_eq!(
            map.lookup(state, &event),
            Some(expected),
            "{state:?} {event:?}"
        );
    }
}

/// F6–F10 and Ctrl+U/I/O/P/T are the same five transforms, and both
/// spellings must work in both states that have segments to transform.
#[test]
fn the_transform_keys_have_both_spellings() {
    let map = ms_ime();
    let pairs: [(KeyCode, char, Action); 5] = [
        (KeyCode::F6, 'u', Action::TransformHiragana),
        (KeyCode::F7, 'i', Action::TransformKatakana),
        (KeyCode::F8, 'o', Action::TransformHalfKatakana),
        (KeyCode::F9, 'p', Action::TransformFullAlnum),
        (KeyCode::F10, 't', Action::TransformHalfAlnum),
    ];
    for state in [State::Composing, State::Converting] {
        for (code, letter, action) in pairs {
            assert_eq!(
                map.lookup(state, &key(code, Modifiers::NONE)),
                Some(action),
                "{state:?} {code:?}"
            );
            assert_eq!(
                map.lookup(state, &ch(letter, Modifiers::CTRL)),
                Some(action),
                "{state:?} ctrl+{letter}"
            );
        }
    }
}

/// DESIGN 2: Ctrl+Space is IntelliSense on the target user's home turf
/// and must not be bound by default in any preset or any state.
#[test]
fn ctrl_space_is_never_bound_by_default() {
    for preset in Preset::ALL {
        let map = KeyMap::preset(preset).expect("preset must compile");
        for state in State::ALL {
            assert_eq!(
                map.lookup(state, &key(KeyCode::Space, Modifiers::CTRL)),
                None,
                "{} bound ctrl+space in {state:?}",
                preset.name()
            );
        }
    }
}

/// DESIGN 2: every JIS-only key has a US-keyboard equivalent, or a US
/// user loses the function outright.
#[test]
fn jis_only_keys_have_us_equivalents() {
    for preset in Preset::ALL {
        let map = KeyMap::preset(preset).expect("preset must compile");
        let jis = map.lookup(State::Idle, &key(KeyCode::HankakuZenkaku, Modifiers::NONE));
        let us = map.lookup(State::Idle, &ch('`', Modifiers::ALT));
        assert_eq!(jis, us, "{}: alt+` must mirror 半角/全角", preset.name());
        assert_eq!(jis, Some(Action::ImeToggle));
    }
}

/// A `global` binding is a default: the state's own binding wins.
#[test]
fn a_state_binding_beats_a_global_one() {
    let map = KeyMap::parse("[global]\nescape = \"ime_off\"\n[composing]\nescape = \"cancel\"\n")
        .expect("compile");
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Escape, Modifiers::NONE)),
        Some(Action::Cancel)
    );
    assert_eq!(
        map.lookup(State::Idle, &key(KeyCode::Escape, Modifiers::NONE)),
        Some(Action::ImeOff)
    );
}

/// Modifiers match exactly. If they did not, Shift+Enter would commit
/// the focused candidate instead of the top prediction.
#[test]
fn modifiers_match_exactly() {
    let map = ms_ime();
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Enter, Modifiers::NONE)),
        Some(Action::Commit)
    );
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Enter, Modifiers::SHIFT)),
        Some(Action::CommitFirst)
    );
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Enter, Modifiers::ALT)),
        None
    );
}

/// Caps Lock and Kana Lock describe the keyboard, not a key held while
/// typing, so a binding must survive them being on.
#[test]
fn lock_bits_do_not_affect_the_match() {
    let map = ms_ime();
    let locks = Modifiers(Modifiers::CAPS_LOCK.0 | Modifiers::KANA_LOCK.0);
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Enter, locks)),
        Some(Action::Commit)
    );
    assert_eq!(
        map.lookup(
            State::Composing,
            &key(KeyCode::Enter, Modifiers(locks.0 | Modifiers::SHIFT.0))
        ),
        Some(Action::CommitFirst)
    );
}

#[test]
fn character_bindings_fold_case() {
    let map = ms_ime();
    assert_eq!(
        map.lookup(State::Composing, &ch('U', Modifiers::CTRL)),
        Some(Action::TransformHiragana)
    );
}

/// An unbindable event — an unrecognized key, or a character key with no
/// character — resolves to nothing rather than to whatever sorts first.
#[test]
fn unbindable_events_match_nothing() {
    let map = ms_ime();
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Unknown, Modifiers::NONE)),
        None
    );
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Char, Modifiers::NONE)),
        None
    );
    // Plain letters are text, not commands.
    assert_eq!(
        map.lookup(State::Composing, &ch('a', Modifiers::NONE)),
        None
    );
}

// --- Overrides ---

#[test]
fn an_override_replaces_a_preset_binding() {
    let mut map = ms_ime();
    let document = config::parse("[composing]\ntab = \"candidate_expand\"\n").expect("parse");
    map.apply_overrides(&document).expect("apply");
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Tab, Modifiers::NONE)),
        Some(Action::CandidateExpand)
    );
    // Everything else is untouched.
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Enter, Modifiers::NONE)),
        Some(Action::Commit)
    );
}

/// The terminal case from DESIGN 2: give Tab back to the shell without
/// restating the preset.
#[test]
fn unbound_removes_a_binding() {
    let mut map = ms_ime();
    let before = map.len();
    let document = config::parse("[composing]\ntab = \"unbound\"\n").expect("parse");
    map.apply_overrides(&document).expect("apply");
    assert_eq!(
        map.lookup(State::Composing, &key(KeyCode::Tab, Modifiers::NONE)),
        None
    );
    assert_eq!(map.len(), before - 1);
}

#[test]
fn unbinding_a_key_that_is_not_bound_is_harmless() {
    let mut map = ms_ime();
    let before = map.len();
    let document = config::parse("[composing]\nf12 = \"unbound\"\n").expect("parse");
    map.apply_overrides(&document).expect("apply");
    assert_eq!(map.len(), before);
}

/// A preset that unbinds is always a leftover, because there is nothing
/// underneath it to unbind.
#[test]
fn unbound_is_rejected_outside_an_override() {
    let error = KeyMap::parse("[composing]\ntab = \"unbound\"\n").expect_err("expected error");
    assert_eq!(error.kind, KeyMapErrorKind::UnknownAction);
}

/// A rejected override must not leave the user half-rebound.
#[test]
fn a_failed_override_changes_nothing() {
    let mut map = ms_ime();
    let before = map.clone();
    let document = config::parse("[composing]\ntab = \"candidate_expand\"\nf1 = \"nonsense\"\n")
        .expect("parse");
    let error = map.apply_overrides(&document).expect_err("expected error");
    assert_eq!(error.kind, KeyMapErrorKind::UnknownAction);
    assert_eq!(map, before);
}

// --- Key spec parsing ---

#[test]
fn key_specs_parse_their_modifiers() {
    let cases: [(&str, Modifiers, Trigger); 6] = [
        (
            "enter",
            Modifiers::NONE,
            Trigger::Code(KeyCode::Enter as u16),
        ),
        (
            "shift+left",
            Modifiers::SHIFT,
            Trigger::Code(KeyCode::Left as u16),
        ),
        ("ctrl+u", Modifiers::CTRL, Trigger::Char('u')),
        ("alt+`", Modifiers::ALT, Trigger::Char('`')),
        (
            "ctrl+shift+f6",
            Modifiers(Modifiers::CTRL.0 | Modifiers::SHIFT.0),
            Trigger::Code(KeyCode::F6 as u16),
        ),
        // A trailing `+` is the plus key, not a dangling separator.
        ("ctrl++", Modifiers::CTRL, Trigger::Char('+')),
    ];
    for (spec, modifiers, trigger) in cases {
        assert_eq!(parse_key_spec(spec), Ok((modifiers, trigger)), "{spec}");
    }
}

#[test]
fn every_malformed_key_map_names_its_fault() {
    let cases: [(&str, KeyMapErrorKind); 6] = [
        (
            "[nowhere]\nenter = \"commit\"\n",
            KeyMapErrorKind::UnknownSection,
        ),
        (
            "[composing]\nenter = \"fly\"\n",
            KeyMapErrorKind::UnknownAction,
        ),
        (
            "[composing]\nenter = [\"commit\"]\n",
            KeyMapErrorKind::MalformedValue,
        ),
        (
            "[composing]\nnosuchkey = \"commit\"\n",
            KeyMapErrorKind::UnknownKey,
        ),
        (
            "[composing]\n\"hyper+enter\" = \"commit\"\n",
            KeyMapErrorKind::UnknownModifier,
        ),
        ("[global]\n", KeyMapErrorKind::EmptyKeyMap),
    ];
    for (source, expected) in cases {
        let error = KeyMap::parse(source).expect_err("expected an error");
        assert_eq!(error.kind, expected, "source: {source:?}");
    }
}

/// Two spellings of the same binding in one section — `ctrl+shift+a` and
/// `shift+ctrl+a` — are the same key, and silently keeping one is how a
/// rebind appears to do nothing.
#[test]
fn binding_the_same_key_twice_in_a_section_is_an_error() {
    let error = KeyMap::parse(
        "[composing]\n\"ctrl+shift+a\" = \"commit\"\n\"shift+ctrl+a\" = \"cancel\"\n",
    )
    .expect_err("expected an error");
    assert_eq!(error.kind, KeyMapErrorKind::DuplicateBinding);
}

/// The same key in two different scopes is not a duplicate — that is how
/// a state overrides a global default.
#[test]
fn the_same_key_in_two_scopes_is_not_a_duplicate() {
    KeyMap::parse("[global]\nenter = \"commit\"\n[idle]\nenter = \"commit\"\n")
        .expect("two scopes are independent");
}

#[test]
fn a_config_error_is_reported_as_one() {
    let error = KeyMap::parse("[composing]\nenter = 1\n").expect_err("expected an error");
    assert!(matches!(error.kind, KeyMapErrorKind::Config(_)));
    assert!(error.to_string().contains("line 2"));
}

/// Every binding must render back into a spec that parses to the same
/// binding, or the settings UI shows the user something they cannot type.
#[test]
fn every_shipped_binding_round_trips_through_its_spec() {
    for preset in Preset::ALL {
        let map = KeyMap::preset(preset).expect("preset must compile");
        for (state, spec, action) in map.bindings() {
            let (modifiers, trigger) =
                parse_key_spec(&spec).unwrap_or_else(|_| panic!("{spec:?} must re-parse"));
            let scope = state.map_or(Scope::Global, Scope::In);
            let slot = Slot {
                scope,
                trigger,
                modifiers: modifiers.0,
            };
            assert_eq!(
                map.find(scope, slot.trigger, slot.modifiers),
                Some(action),
                "{} {spec}",
                preset.name()
            );
        }
    }
}

/// The presets must actually differ, or shipping two of them is a lie.
#[test]
fn the_atok_preset_differs_from_ms_ime() {
    let ms = ms_ime();
    let atok = KeyMap::preset(Preset::Atok).expect("compile");
    assert_ne!(ms, atok);
    // Both presets use the three-form 無変換 cycle; ATOK still differs in
    // its other candidate and caret bindings.
    assert_eq!(
        atok.lookup(State::Idle, &key(KeyCode::Muhenkan, Modifiers::NONE)),
        Some(Action::ModeKanaCycle)
    );
    assert_eq!(
        ms.lookup(State::Idle, &key(KeyCode::Muhenkan, Modifiers::NONE)),
        Some(Action::ModeKanaCycle)
    );
}

#[test]
fn atok_uses_prediction_tab_and_candidate_group_navigation() {
    let atok = KeyMap::preset(Preset::Atok).expect("compile");
    let ms = ms_ime();

    assert_eq!(
        atok.lookup(State::Composing, &key(KeyCode::Tab, Modifiers::NONE)),
        Some(Action::PredictNext)
    );
    assert_eq!(
        atok.lookup(State::Converting, &key(KeyCode::Tab, Modifiers::NONE)),
        Some(Action::CandidateNext)
    );
    assert_eq!(
        atok.lookup(State::Converting, &key(KeyCode::Tab, Modifiers::SHIFT)),
        Some(Action::CandidatePrev)
    );
    assert_eq!(
        atok.lookup(State::Converting, &key(KeyCode::Muhenkan, Modifiers::NONE)),
        Some(Action::ModeKanaCycle)
    );
    assert_eq!(
        ms.lookup(State::Converting, &key(KeyCode::Muhenkan, Modifiers::NONE)),
        Some(Action::ModeKanaCycle)
    );
    assert_eq!(
        ms.lookup(State::Converting, &key(KeyCode::Tab, Modifiers::NONE)),
        Some(Action::CandidateExpand)
    );
}

/// Nothing a keyboard can produce may panic the lookup, in a process
/// where a panic takes the host application with it.
#[test]
fn arbitrary_key_events_never_panic() {
    let map = ms_ime();
    for state in State::ALL {
        for code in KeyCode::ALL {
            for bits in 0u8..=0x1F {
                let _ = map.lookup(state, &key(code, Modifiers(bits)));
                let _ = map.lookup(
                    state,
                    &KeyInput {
                        code,
                        ch: Some('あ'),
                        modifiers: Modifiers(bits),
                        repeat: true,
                        test_only: true,
                    },
                );
            }
        }
    }
}
