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
//! while typing but opens the candidate table during conversion. So a
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
mod tests {
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
        let map =
            KeyMap::parse("[global]\nescape = \"ime_off\"\n[composing]\nescape = \"cancel\"\n")
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
        let document =
            config::parse("[composing]\ntab = \"candidate_expand\"\nf1 = \"nonsense\"\n")
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
}
