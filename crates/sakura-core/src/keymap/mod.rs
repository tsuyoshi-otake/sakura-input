//! Key bindings (DESIGN 2 "Key map", DESIGN 8).
//!
//! Which key does what is data, not code. Two presets ship — `ms-ime`, the
//! default, and `atok` — and a user's own document is layered on top of one
//! of them as per-key overrides. Nothing in the engine hardcodes a keystroke.
//!
//! # What a binding is keyed on
//!
//! The same physical key means different things depending on what the
//! composition is doing: Space converts while typing and moves through
//! candidates once conversion has started, and Tab focuses a prediction
//! while typing but opens the candidate table during conversion. Conversion
//! starts with a visible but keyboard-unfocused candidate list: numeric
//! bindings become shortcuts only after explicit candidate navigation or
//! expansion; before that, digits commit the current conversion and continue
//! as literal input. So a
//! binding is `(scope, key, modifiers) -> action`, where scope is either one
//! [`State`] or `global`.
//!
//! Modifiers match exactly, after the lock bits are cleared. Shift+Enter is
//! therefore a different binding from Enter rather than a variation on it,
//! which is what DESIGN 2 requires — Shift+Enter commits the top prediction
//! outright while Enter commits what is focused. Keyboard lock bits are
//! stripped before matching; the Caps Lock key itself is still available as a
//! named trigger.
//!
//! # Resolution
//!
//! A binding in the current state wins over a `global` one, so `global` is
//! a default rather than an override. Unmatched keys return `None`, which is
//! the engine's signal to treat the key as text (or to pass it through).

mod key_spec;
mod vocabulary;

use sakura_values::KeyInput;

use crate::config::{self, Document, ParseError, Value};
use key_spec::{format_slot, parse_key_spec, trigger_of, Scope, Slot, Trigger};
use vocabulary::GLOBAL_SECTION;
pub use vocabulary::{Action, Preset, State, ATOK_PRESET, MS_IME_PRESET, UNBOUND};

#[cfg(test)]
use sakura_values::{KeyCode, Modifiers};

/// A compiled key map.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyMap {
    /// Sorted by [`Slot`], so lookup is a binary search.
    bindings: Vec<(Slot, Action)>,
}

/// Why a key map could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyMapError {
    /// The section the fault is in, when it belongs to one.
    pub section: Option<String>,
    /// The key spec at fault, when it belongs to one.
    pub key: Option<String>,
    pub kind: KeyMapErrorKind,
}

/// The specific fault. Every variant names something a human can go and fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyMapErrorKind {
    /// The file itself did not parse.
    Config(ParseError),
    /// A section that is neither `global` nor a state name.
    UnknownSection,
    /// A key map with no bindings at all.
    EmptyKeyMap,
    /// A key spec naming no key this IME can see.
    UnknownKey,
    /// A modifier name that is not `shift`, `ctrl` or `alt`.
    UnknownModifier,
    /// An action name that is not one of [`Action::ALL`] or [`UNBOUND`].
    UnknownAction,
    /// A list where a single action name belongs.
    MalformedValue,
    /// The same key bound twice in one scope. Silently keeping one of them is
    /// how a rebind appears to do nothing.
    DuplicateBinding,
}

impl core::fmt::Display for KeyMapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(section) = &self.section {
            write!(f, "[{section}] ")?;
        }
        if let Some(key) = &self.key {
            write!(f, "{key:?}: ")?;
        }
        match &self.kind {
            KeyMapErrorKind::Config(error) => write!(f, "{error}"),
            KeyMapErrorKind::UnknownSection => {
                write!(
                    f,
                    "not a key map section; expected {GLOBAL_SECTION} or a state"
                )
            }
            KeyMapErrorKind::EmptyKeyMap => write!(f, "the key map is empty"),
            KeyMapErrorKind::UnknownKey => write!(f, "not a key this IME can bind"),
            KeyMapErrorKind::UnknownModifier => write!(f, "not a modifier"),
            KeyMapErrorKind::UnknownAction => write!(f, "not an action"),
            KeyMapErrorKind::MalformedValue => write!(f, "value must be a single action name"),
            KeyMapErrorKind::DuplicateBinding => write!(f, "key is bound twice in this section"),
        }
    }
}

impl std::error::Error for KeyMapError {}

impl From<ParseError> for KeyMapError {
    fn from(error: ParseError) -> Self {
        KeyMapError {
            section: None,
            key: None,
            kind: KeyMapErrorKind::Config(error),
        }
    }
}

impl KeyMap {
    /// Compiles a shipped preset.
    pub fn preset(preset: Preset) -> Result<Self, KeyMapError> {
        Self::parse(preset.source())
    }

    /// Parses config source and compiles the key map in it.
    pub fn parse(source: &str) -> Result<Self, KeyMapError> {
        let document = config::parse(source)?;
        Self::from_document(&document)
    }

    /// Compiles an already-parsed document.
    pub fn from_document(document: &Document) -> Result<Self, KeyMapError> {
        let mut map = KeyMap::default();
        map.merge(document, false)?;
        if map.bindings.is_empty() {
            return Err(KeyMapError {
                section: None,
                key: None,
                kind: KeyMapErrorKind::EmptyKeyMap,
            });
        }
        Ok(map)
    }

    /// Layers a user's document over this map (DESIGN 2 "per-key
    /// overrides").
    ///
    /// A binding for a key already bound in the same scope replaces it, and
    /// the action [`UNBOUND`] removes it — which is how a user frees Tab for
    /// the application without having to restate the whole preset.
    ///
    /// On error the map is left unchanged: a half-applied override would
    /// leave the user with a keyboard that matches neither their file nor
    /// the preset.
    pub fn apply_overrides(&mut self, document: &Document) -> Result<(), KeyMapError> {
        let mut candidate = self.clone();
        candidate.merge(document, true)?;
        *self = candidate;
        Ok(())
    }

    /// The number of bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// `true` if nothing is bound.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// The action bound to `key` in `state`, if any.
    ///
    /// A state-specific binding wins over a `global` one.
    pub fn lookup(&self, state: State, key: &KeyInput) -> Option<Action> {
        let trigger = trigger_of(key)?;
        let modifiers = key.modifiers.without_locks().0;
        self.find(Scope::In(state), trigger, modifiers)
            .or_else(|| self.find(Scope::Global, trigger, modifiers))
    }

    /// Every binding, as `(state or None for global, key spec, action)`, in
    /// sorted order. For the settings UI and for tests.
    pub fn bindings(&self) -> impl Iterator<Item = (Option<State>, String, Action)> + '_ {
        self.bindings.iter().map(|(slot, action)| {
            let scope = match slot.scope {
                Scope::Global => None,
                Scope::In(state) => Some(state),
            };
            (scope, format_slot(slot), *action)
        })
    }

    fn find(&self, scope: Scope, trigger: Trigger, modifiers: u8) -> Option<Action> {
        let slot = Slot {
            scope,
            trigger,
            modifiers,
        };
        let index = self
            .bindings
            .binary_search_by(|(candidate, _)| candidate.cmp(&slot))
            .ok()?;
        self.bindings.get(index).map(|(_, action)| *action)
    }

    /// Reads every section of `document` into this map.
    ///
    /// `overriding` distinguishes the two callers: building a map from
    /// scratch, where binding the same key twice is a mistake in the file,
    /// and layering a user's overrides, where replacing a binding is the
    /// entire point.
    fn merge(&mut self, document: &Document, overriding: bool) -> Result<(), KeyMapError> {
        for name in document.section_names() {
            let scope = match name {
                GLOBAL_SECTION => Scope::Global,
                _ => match State::ALL.into_iter().find(|s| s.section() == name) {
                    Some(state) => Scope::In(state),
                    None => {
                        return Err(KeyMapError {
                            section: Some(name.to_string()),
                            key: None,
                            kind: KeyMapErrorKind::UnknownSection,
                        })
                    }
                },
            };

            let Some(entries) = document.section(name) else {
                continue;
            };
            // Tracked separately from `self.bindings`, so that an override
            // document can replace a preset binding but still not bind the
            // same key twice within itself.
            let mut seen: Vec<Slot> = Vec::new();

            for entry in entries {
                let fail = |kind| KeyMapError {
                    section: Some(name.to_string()),
                    key: Some(entry.key.clone()),
                    kind,
                };

                let action_name = match &entry.value {
                    Value::Text(text) => text.as_str(),
                    Value::List(_) => return Err(fail(KeyMapErrorKind::MalformedValue)),
                };

                let (modifiers, trigger) = parse_key_spec(&entry.key).map_err(fail)?;
                let slot = Slot {
                    scope,
                    trigger,
                    modifiers: modifiers.without_locks().0,
                };
                if seen.contains(&slot) {
                    return Err(fail(KeyMapErrorKind::DuplicateBinding));
                }
                seen.push(slot);

                if action_name == UNBOUND {
                    if !overriding {
                        // In a preset, unbinding a key nothing has bound yet
                        // says nothing; it is always a leftover.
                        return Err(fail(KeyMapErrorKind::UnknownAction));
                    }
                    self.remove(&slot);
                    continue;
                }
                let Some(action) = Action::from_name(action_name) else {
                    return Err(fail(KeyMapErrorKind::UnknownAction));
                };
                self.insert(slot, action);
            }
        }
        Ok(())
    }

    fn insert(&mut self, slot: Slot, action: Action) {
        match self.bindings.binary_search_by(|(s, _)| s.cmp(&slot)) {
            Ok(index) => {
                if let Some(existing) = self.bindings.get_mut(index) {
                    existing.1 = action;
                }
            }
            Err(index) => self.bindings.insert(index, (slot, action)),
        }
    }

    fn remove(&mut self, slot: &Slot) {
        if let Ok(index) = self.bindings.binary_search_by(|(s, _)| s.cmp(slot)) {
            self.bindings.remove(index);
        }
    }
}

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
