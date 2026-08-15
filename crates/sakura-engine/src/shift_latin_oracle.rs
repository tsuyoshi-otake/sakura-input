//! Independent domain oracle for Shift-held Latin composition order.
//!
//! This module answers a user-facing question only: given a sequence of
//! input events, what is the correct visible committed text, composing
//! buffer, and caret. It does not import the engine dispatcher, session
//! internals, romaji FSM, or key-map implementation. Production code may
//! be compared against it; the oracle must not be derived by copying
//! production control flow.
//!
//! Requirement this encodes:
//! - Holding Shift types English/Latin characters in key-press order.
//! - The first Shift+ASCII letter on an empty composition latches that
//!   English composition so later unshifted ASCII stays Latin.
//! - Backspace (Shift held or not) deletes the Latin character before the
//!   caret. Delete removes the character at the caret.
//! - Later keys insert at the caret in press order.
//! - Emptying the composition releases the latch.
//! - Convert enters a conversion selection; Backspace then cancels
//!   conversion without deleting a character (MS-IME composing-vs-converting
//!   distinction). A Latin key while converting commits the current
//!   composition and starts a new English/Latin buffer with that key.

/// User-facing events. Characters are already layout-translated, matching
/// what the TSF translator supplies to the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainEvent {
    ShiftLatin(char),
    Latin(char),
    Backspace { shift: bool },
    Delete { shift: bool },
    Left,
    Right,
    Home,
    End,
    Convert { shift: bool },
    Cancel,
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleState {
    pub committed: String,
    pub composing: String,
    pub cursor: usize,
    pub shift_held_hint: bool,
    pub english_latched: bool,
    pub converting: bool,
}

impl Default for OracleState {
    fn default() -> Self {
        Self {
            committed: String::new(),
            composing: String::new(),
            cursor: 0,
            shift_held_hint: false,
            english_latched: false,
            converting: false,
        }
    }
}

impl OracleState {
    pub fn visible(&self) -> String {
        let mut text = self.committed.clone();
        text.push_str(&self.composing);
        text
    }

    pub fn composing_or_empty(&self) -> &str {
        &self.composing
    }
}

fn is_latin_letter(character: char) -> bool {
    character.is_ascii_alphabetic()
}

fn insert_at(buffer: &mut String, cursor: &mut usize, character: char) {
    let at = buffer
        .char_indices()
        .nth(*cursor)
        .map(|(index, _)| index)
        .unwrap_or(buffer.len());
    buffer.insert(at, character);
    *cursor = cursor.saturating_add(1);
}

fn delete_before(buffer: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let at = buffer
        .char_indices()
        .nth(*cursor - 1)
        .map(|(index, _)| index)
        .unwrap_or(0);
    buffer.remove(at);
    *cursor -= 1;
}

fn delete_at(buffer: &mut String, cursor: usize) {
    if let Some((at, _)) = buffer.char_indices().nth(cursor) {
        buffer.remove(at);
    }
}

fn clamp_cursor(state: &mut OracleState) {
    let end = state.composing.chars().count();
    if state.cursor > end {
        state.cursor = end;
    }
}

fn release_latch_if_empty(state: &mut OracleState) {
    if state.composing.is_empty() && !state.converting {
        state.english_latched = false;
        state.cursor = 0;
    }
}

/// Applies one event. The branches are the requirement, not a transcription
/// of `feed_character` / `apply_backspace`.
pub fn apply(state: &mut OracleState, event: DomainEvent) {
    match event {
        DomainEvent::ShiftLatin(character) => {
            state.shift_held_hint = true;
            apply_latin(state, character, true);
        }
        DomainEvent::Latin(character) => {
            state.shift_held_hint = false;
            apply_latin(state, character, false);
        }
        DomainEvent::Backspace { shift } => {
            state.shift_held_hint = shift;
            if state.converting {
                state.converting = false;
                clamp_cursor(state);
                return;
            }
            if state.composing.is_empty() {
                return;
            }
            delete_before(&mut state.composing, &mut state.cursor);
            release_latch_if_empty(state);
        }
        DomainEvent::Delete { shift } => {
            state.shift_held_hint = shift;
            if state.converting {
                return;
            }
            delete_at(&mut state.composing, state.cursor);
            release_latch_if_empty(state);
        }
        DomainEvent::Left => {
            if !state.converting && state.cursor > 0 {
                state.cursor -= 1;
            }
        }
        DomainEvent::Right => {
            if !state.converting {
                let end = state.composing.chars().count();
                if state.cursor < end {
                    state.cursor += 1;
                }
            }
        }
        DomainEvent::Home => {
            if !state.converting {
                state.cursor = 0;
            }
        }
        DomainEvent::End => {
            if !state.converting {
                state.cursor = state.composing.chars().count();
            }
        }
        DomainEvent::Convert { shift } => {
            state.shift_held_hint = shift;
            if !state.composing.is_empty() {
                state.converting = true;
            }
        }
        DomainEvent::Cancel => {
            if state.converting {
                state.converting = false;
                return;
            }
            state.composing.clear();
            state.cursor = 0;
            state.english_latched = false;
        }
        DomainEvent::Commit => {
            state.committed.push_str(&state.composing);
            state.composing.clear();
            state.cursor = 0;
            state.english_latched = false;
            state.converting = false;
        }
    }
}

fn apply_latin(state: &mut OracleState, character: char, shifted: bool) {
    if !character.is_ascii() {
        return;
    }
    if state.converting {
        state.committed.push_str(&state.composing);
        state.composing.clear();
        state.cursor = 0;
        state.english_latched = false;
        state.converting = false;
    }
    let starts_english = state.composing.is_empty() && shifted && is_latin_letter(character);
    if starts_english {
        state.english_latched = true;
    } else if !state.english_latched && is_latin_letter(character) && !shifted {
        // Unshifted Latin on a Japanese-idle session is outside this
        // oracle: the IME will treat it as romaji. Leave the buffer
        // untouched so a campaign can detect accidental leakage.
        return;
    }
    if state.english_latched
        || (shifted && is_latin_letter(character))
        || !is_latin_letter(character)
    {
        insert_at(&mut state.composing, &mut state.cursor, character);
        if state.english_latched || starts_english {
            state.english_latched = true;
        }
    }
}

pub fn apply_all<I>(events: I) -> OracleState
where
    I: IntoIterator<Item = DomainEvent>,
{
    let mut state = OracleState::default();
    for event in events {
        apply(&mut state, event);
    }
    state
}

/// Atomic conditions used for C2 measurement of this oracle. Each pair is
/// `(id, observed_true)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtomicCondition {
    pub id: &'static str,
    pub value: bool,
}

pub fn atomic_conditions(state: &OracleState, event: DomainEvent) -> [AtomicCondition; 8] {
    let empty = state.composing.is_empty();
    let letter = match event {
        DomainEvent::ShiftLatin(character) | DomainEvent::Latin(character) => {
            is_latin_letter(character)
        }
        _ => false,
    };
    let shifted = matches!(
        event,
        DomainEvent::ShiftLatin(_)
            | DomainEvent::Backspace { shift: true }
            | DomainEvent::Delete { shift: true }
            | DomainEvent::Convert { shift: true }
    );
    [
        AtomicCondition {
            id: "composing_empty",
            value: empty,
        },
        AtomicCondition {
            id: "english_latched",
            value: state.english_latched,
        },
        AtomicCondition {
            id: "converting",
            value: state.converting,
        },
        AtomicCondition {
            id: "event_shifted",
            value: shifted,
        },
        AtomicCondition {
            id: "event_latin_letter",
            value: letter,
        },
        AtomicCondition {
            id: "cursor_at_start",
            value: state.cursor == 0,
        },
        AtomicCondition {
            id: "cursor_at_end",
            value: state.cursor == state.composing.chars().count(),
        },
        AtomicCondition {
            id: "cursor_interior",
            value: state.cursor > 0 && state.cursor < state.composing.chars().count(),
        },
    ]
}
