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

use sakura_proto::{KeyCode, KeyInput, Modifiers};

use crate::config::{self, Document, ParseError, Value};

/// The `ms-ime` preset, compiled into the binary.
pub const MS_IME_PRESET: &str = include_str!("../../../data/keymap-ms-ime.toml");

/// The `atok` preset, compiled into the binary.
pub const ATOK_PRESET: &str = include_str!("../../../data/keymap-atok.toml");

/// The action name that removes a binding instead of adding one.
pub const UNBOUND: &str = "unbound";

/// Which shipped key map to start from (DESIGN 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// Windows 11 Microsoft IME conventions. The default, because that is
    /// what a Windows user's fingers already know.
    #[default]
    MsIme,
    Atok,
}

impl Preset {
    /// The name used in config files and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Preset::MsIme => "ms-ime",
            Preset::Atok => "atok",
        }
    }

    /// Parses a preset name, `None` if it names no shipped preset.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "ms-ime" => Some(Preset::MsIme),
            "atok" => Some(Preset::Atok),
            _ => None,
        }
    }

    /// The preset's source text.
    pub fn source(self) -> &'static str {
        match self {
            Preset::MsIme => MS_IME_PRESET,
            Preset::Atok => ATOK_PRESET,
        }
    }

    /// Every shipped preset, in declaration order.
    pub const ALL: [Preset; 2] = [Preset::MsIme, Preset::Atok];
}

/// What the composition is doing, which is what decides a key's meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// No composition. Most keys belong to the application.
    Idle,
    /// A preedit exists and is still being typed.
    Composing,
    /// Conversion has started; segments and a candidate list exist.
    Converting,
    /// A prediction in the suggest list is focused (DESIGN 5.3).
    Predicting,
}

impl State {
    /// The config section name for this state.
    pub fn section(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Composing => "composing",
            State::Converting => "converting",
            State::Predicting => "predicting",
        }
    }

    /// All states, in declaration order.
    pub const ALL: [State; 4] = [
        State::Idle,
        State::Composing,
        State::Converting,
        State::Predicting,
    ];
}

/// The section whose bindings apply in every state.
const GLOBAL_SECTION: &str = "global";

/// Everything a key can be bound to.
///
/// Deliberately payload-free: a variant per meaning keeps both the config
/// syntax and the engine's match statement flat, and the set is small enough
/// that the repetition costs less than a parameterized encoding would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Turn the IME on or off (半角/全角).
    ImeToggle,
    ImeOn,
    ImeOff,

    ModeHiragana,
    ModeKatakana,
    ModeHalfKatakana,
    ModeFullAlnum,
    ModeHalfAlnum,
    ModeDirect,
    /// Hiragana ⇄ katakana, the Microsoft IME meaning of 無変換.
    ModeKanaToggle,
    /// Hiragana → katakana → half-width katakana, the ATOK meaning.
    ModeKanaCycle,
    /// Hiragana/alphanumeric mode toggle (英数/Caps Lock).
    ModeAlnumToggle,
    /// Half-width/full-width alphanumeric mode toggle (Shift+無変換).
    ModeAlnumWidthToggle,

    /// Commit what is focused.
    Commit,
    /// Commit the top prediction without focusing the list first
    /// (Shift+Enter, DESIGN 2).
    CommitFirst,
    /// Back out one stage: candidates → preedit → nothing.
    Cancel,

    /// Start conversion, or move to the next candidate once it has started.
    Convert,
    ConvertPrev,
    CandidateNext,
    CandidatePrev,
    CandidatePageDown,
    CandidatePageUp,
    Candidate1,
    Candidate2,
    Candidate3,
    Candidate4,
    Candidate5,
    Candidate6,
    Candidate7,
    Candidate8,
    Candidate9,
    /// Open the expanded candidate table (Tab in the candidate window).
    CandidateExpand,

    /// Focus the next prediction; the first press enters the list.
    PredictNext,
    PredictPrev,
    /// Forget the focused learned-history prediction without touching system
    /// or user dictionary candidates.
    DeletePredictionHistory,

    SegmentPrev,
    SegmentNext,
    SegmentShrink,
    SegmentGrow,
    /// Jump focus to the first/last segment (Home/End while converting,
    /// issue #16 finding E). `CaretHome`/`CaretEnd` address the raw preedit
    /// cursor, which conversion has already cut into segments, so a
    /// segment-focused equivalent is needed instead.
    SegmentHome,
    SegmentEnd,

    CaretLeft,
    CaretRight,
    CaretHome,
    CaretEnd,
    DeleteBack,
    DeleteForward,

    /// Claims a key without changing any state: `apply_action`'s existing
    /// catch-all already swallows every action it does not explicitly
    /// handle (`consumed = true`, no effect), so this variant reaches the
    /// same outcome, but names the intent at the binding site instead of
    /// leaving a key unbound. An unbound key falls through to
    /// `apply_key`'s final arm and leaks to the host application while a
    /// composition or conversion is on screen (issue #16 finding E) — use
    /// this where Microsoft IME visibly consumes the key but the effect it
    /// has is not modelled, rather than leave the gap unclaimed.
    Swallow,

    /// F6–F10 and their Ctrl equivalents, applied to the focused segment.
    TransformHiragana,
    TransformKatakana,
    TransformHalfKatakana,
    TransformFullAlnum,
    TransformHalfAlnum,

    /// Pull the selection back into composition (変換 with no composition).
    Reconvert,
    /// 確定の取り消し (Ctrl+Backspace, DESIGN 2).
    UndoCommit,
}

impl Action {
    /// The name used in config files.
    pub fn name(self) -> &'static str {
        match self {
            Action::ImeToggle => "ime_toggle",
            Action::ImeOn => "ime_on",
            Action::ImeOff => "ime_off",
            Action::ModeHiragana => "mode_hiragana",
            Action::ModeKatakana => "mode_katakana",
            Action::ModeHalfKatakana => "mode_half_katakana",
            Action::ModeFullAlnum => "mode_full_alnum",
            Action::ModeHalfAlnum => "mode_half_alnum",
            Action::ModeDirect => "mode_direct",
            Action::ModeKanaToggle => "mode_kana_toggle",
            Action::ModeKanaCycle => "mode_kana_cycle",
            Action::ModeAlnumToggle => "mode_alnum_toggle",
            Action::ModeAlnumWidthToggle => "mode_alnum_width_toggle",
            Action::Commit => "commit",
            Action::CommitFirst => "commit_first",
            Action::Cancel => "cancel",
            Action::Convert => "convert",
            Action::ConvertPrev => "convert_prev",
            Action::CandidateNext => "candidate_next",
            Action::CandidatePrev => "candidate_prev",
            Action::CandidatePageDown => "candidate_page_down",
            Action::CandidatePageUp => "candidate_page_up",
            Action::Candidate1 => "candidate_1",
            Action::Candidate2 => "candidate_2",
            Action::Candidate3 => "candidate_3",
            Action::Candidate4 => "candidate_4",
            Action::Candidate5 => "candidate_5",
            Action::Candidate6 => "candidate_6",
            Action::Candidate7 => "candidate_7",
            Action::Candidate8 => "candidate_8",
            Action::Candidate9 => "candidate_9",
            Action::CandidateExpand => "candidate_expand",
            Action::PredictNext => "predict_next",
            Action::PredictPrev => "predict_prev",
            Action::DeletePredictionHistory => "delete_prediction_history",
            Action::SegmentPrev => "segment_prev",
            Action::SegmentNext => "segment_next",
            Action::SegmentShrink => "segment_shrink",
            Action::SegmentGrow => "segment_grow",
            Action::SegmentHome => "segment_home",
            Action::SegmentEnd => "segment_end",
            Action::CaretLeft => "caret_left",
            Action::CaretRight => "caret_right",
            Action::CaretHome => "caret_home",
            Action::CaretEnd => "caret_end",
            Action::DeleteBack => "delete_back",
            Action::DeleteForward => "delete_forward",
            Action::Swallow => "swallow",
            Action::TransformHiragana => "transform_hiragana",
            Action::TransformKatakana => "transform_katakana",
            Action::TransformHalfKatakana => "transform_half_katakana",
            Action::TransformFullAlnum => "transform_full_alnum",
            Action::TransformHalfAlnum => "transform_half_alnum",
            Action::Reconvert => "reconvert",
            Action::UndoCommit => "undo_commit",
        }
    }

    /// Parses an action name, `None` if it names no action.
    pub fn from_name(name: &str) -> Option<Self> {
        Action::ALL.into_iter().find(|action| action.name() == name)
    }

    /// Zero-based shortcut offset within the current candidate page.
    pub fn candidate_offset(self) -> Option<usize> {
        match self {
            Action::Candidate1 => Some(0),
            Action::Candidate2 => Some(1),
            Action::Candidate3 => Some(2),
            Action::Candidate4 => Some(3),
            Action::Candidate5 => Some(4),
            Action::Candidate6 => Some(5),
            Action::Candidate7 => Some(6),
            Action::Candidate8 => Some(7),
            Action::Candidate9 => Some(8),
            _ => None,
        }
    }

    /// Every action, in declaration order.
    pub const ALL: [Action; 55] = [
        Action::ImeToggle,
        Action::ImeOn,
        Action::ImeOff,
        Action::ModeHiragana,
        Action::ModeKatakana,
        Action::ModeHalfKatakana,
        Action::ModeFullAlnum,
        Action::ModeHalfAlnum,
        Action::ModeDirect,
        Action::ModeKanaToggle,
        Action::ModeKanaCycle,
        Action::ModeAlnumToggle,
        Action::ModeAlnumWidthToggle,
        Action::Commit,
        Action::CommitFirst,
        Action::Cancel,
        Action::Convert,
        Action::ConvertPrev,
        Action::CandidateNext,
        Action::CandidatePrev,
        Action::CandidatePageDown,
        Action::CandidatePageUp,
        Action::Candidate1,
        Action::Candidate2,
        Action::Candidate3,
        Action::Candidate4,
        Action::Candidate5,
        Action::Candidate6,
        Action::Candidate7,
        Action::Candidate8,
        Action::Candidate9,
        Action::CandidateExpand,
        Action::PredictNext,
        Action::PredictPrev,
        Action::DeletePredictionHistory,
        Action::SegmentPrev,
        Action::SegmentNext,
        Action::SegmentShrink,
        Action::SegmentGrow,
        Action::SegmentHome,
        Action::SegmentEnd,
        Action::CaretLeft,
        Action::CaretRight,
        Action::CaretHome,
        Action::CaretEnd,
        Action::DeleteBack,
        Action::DeleteForward,
        Action::Swallow,
        Action::TransformHiragana,
        Action::TransformKatakana,
        Action::TransformHalfKatakana,
        Action::TransformFullAlnum,
        Action::TransformHalfAlnum,
        Action::Reconvert,
        Action::UndoCommit,
    ];
}

/// The key half of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Trigger {
    /// A named key: Space, Enter, F6, 変換, and the rest.
    Code(u16),
    /// A printable character key, folded to lowercase — this is how
    /// `ctrl+u` is expressed without giving every letter its own `KeyCode`.
    Char(char),
}

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    /// Every state. Sorts first so a state-specific binding, which must win,
    /// is never the one a search settles on by accident.
    Global,
    In(State),
}

/// The lookup key of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Slot {
    scope: Scope,
    trigger: Trigger,
    /// The modifier bitmask with locks already cleared.
    modifiers: u8,
}

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

/// The trigger a key event matches on, or `None` for an event that can never
/// be bound — an unrecognized key, or a character key with no character.
fn trigger_of(key: &KeyInput) -> Option<Trigger> {
    match key.code {
        KeyCode::Unknown => None,
        KeyCode::Char => key.ch.map(|c| Trigger::Char(c.to_ascii_lowercase())),
        code => Some(Trigger::Code(code as u16)),
    }
}

/// Parses `ctrl+shift+left` and friends.
fn parse_key_spec(spec: &str) -> Result<(Modifiers, Trigger), KeyMapErrorKind> {
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
fn format_slot(slot: &Slot) -> String {
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

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
