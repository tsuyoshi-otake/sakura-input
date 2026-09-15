//! The key-spec syntax of a key map document.
//!
//! A binding's key half is written `ctrl+shift+left` or `ctrl+u`. This module
//! owns that spelling in both directions — parsing a spec into a [`Slot`] and
//! rendering a slot back for the settings UI — and the one table of named
//! keys both directions read.

use sakura_values::{KeyCode, KeyInput, Modifiers};

use super::{KeyMapErrorKind, State};

/// The key half of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Trigger {
    /// A named key: Space, Enter, F6, 変換, and the rest.
    Code(u16),
    /// A printable character key, folded to lowercase — this is how
    /// `ctrl+u` is expressed without giving every letter its own `KeyCode`.
    Char(char),
}

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Scope {
    /// Every state. Sorts first so a state-specific binding, which must win,
    /// is never the one a search settles on by accident.
    Global,
    In(State),
}

/// The lookup key of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Slot {
    pub(super) scope: Scope,
    pub(super) trigger: Trigger,
    /// The modifier bitmask with locks already cleared.
    pub(super) modifiers: u8,
}

/// The trigger a key event matches on, or `None` for an event that can never
/// be bound — an unrecognized key, or a character key with no character.
pub(super) fn trigger_of(key: &KeyInput) -> Option<Trigger> {
    match key.code {
        KeyCode::Unknown => None,
        KeyCode::Char => key.ch.map(|c| Trigger::Char(c.to_ascii_lowercase())),
        code => Some(Trigger::Code(code as u16)),
    }
}

/// Parses `ctrl+shift+left` and friends.
pub(super) fn parse_key_spec(spec: &str) -> Result<(Modifiers, Trigger), KeyMapErrorKind> {
    let mut modifiers = Modifiers::NONE;
    let mut rest = spec;

    while let Some(plus) = rest.find('+') {
        let (head, tail) = rest.split_at(plus);
        // A trailing `+` is the plus key itself, not a separator: `ctrl++`
        // binds Ctrl and `+`, and `+` alone binds the bare key.
        let Some(tail) = tail.get(1..).filter(|t| !t.is_empty()) else {
            break;
        };
        let bit = match head {
            "shift" => Modifiers::SHIFT,
            "ctrl" => Modifiers::CTRL,
            "alt" => Modifiers::ALT,
            // Not a modifier, so this `+` is part of the key name — which no
            // key has, but the trigger parser gets to say so.
            _ => break,
        };
        modifiers = Modifiers(modifiers.0 | bit.0);
        rest = tail;
    }

    Ok((modifiers, parse_trigger(rest)?))
}

/// Parses the key half of a spec: a named key, or a single character.
fn parse_trigger(name: &str) -> Result<Trigger, KeyMapErrorKind> {
    if let Some(code) = named_key(name) {
        return Ok(Trigger::Code(code as u16));
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(Trigger::Char(c.to_ascii_lowercase())),
        // A multi-character name that is not a known key: almost always a
        // typo in a key spec, and never something to silently ignore.
        _ if name.contains('+') => Err(KeyMapErrorKind::UnknownModifier),
        _ => Err(KeyMapErrorKind::UnknownKey),
    }
}

/// The spelling of every non-character key: the one place the mapping
/// between config names and [`KeyCode`] lives, read in both directions.
///
/// `KeyCode::Char` is absent on purpose — character keys are written as
/// themselves (`ctrl+u`), not by name.
const NAMED_KEYS: [(&str, KeyCode); 31] = [
    ("space", KeyCode::Space),
    ("enter", KeyCode::Enter),
    ("escape", KeyCode::Escape),
    ("backspace", KeyCode::Backspace),
    ("delete", KeyCode::Delete),
    ("tab", KeyCode::Tab),
    ("left", KeyCode::Left),
    ("right", KeyCode::Right),
    ("up", KeyCode::Up),
    ("down", KeyCode::Down),
    ("home", KeyCode::Home),
    ("end", KeyCode::End),
    ("page_up", KeyCode::PageUp),
    ("page_down", KeyCode::PageDown),
    ("henkan", KeyCode::Henkan),
    ("muhenkan", KeyCode::Muhenkan),
    ("kana_mode", KeyCode::KanaMode),
    ("hankaku_zenkaku", KeyCode::HankakuZenkaku),
    ("caps_lock", KeyCode::CapsLock),
    ("f1", KeyCode::F1),
    ("f2", KeyCode::F2),
    ("f3", KeyCode::F3),
    ("f4", KeyCode::F4),
    ("f5", KeyCode::F5),
    ("f6", KeyCode::F6),
    ("f7", KeyCode::F7),
    ("f8", KeyCode::F8),
    ("f9", KeyCode::F9),
    ("f10", KeyCode::F10),
    ("f11", KeyCode::F11),
    ("f12", KeyCode::F12),
];

fn named_key(name: &str) -> Option<KeyCode> {
    NAMED_KEYS
        .into_iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, code)| code)
}

/// Renders a slot back into the spec syntax, for the settings UI.
pub(super) fn format_slot(slot: &Slot) -> String {
    let mut text = String::new();
    let modifiers = Modifiers(slot.modifiers);
    if modifiers.ctrl() {
        text.push_str("ctrl+");
    }
    if modifiers.shift() {
        text.push_str("shift+");
    }
    if modifiers.alt() {
        text.push_str("alt+");
    }
    match slot.trigger {
        Trigger::Char(c) => text.push(c),
        Trigger::Code(code) => text.push_str(
            NAMED_KEYS
                .into_iter()
                .find(|(_, candidate)| *candidate as u16 == code)
                .map_or("unknown", |(name, _)| name),
        ),
    }
    text
}
