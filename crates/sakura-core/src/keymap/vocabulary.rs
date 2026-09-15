//! The binding vocabulary: the shipped presets, the composition states a
//! binding can be scoped to, and every action a key can be bound to.
//!
//! These names are the contract between config files and the engine. The
//! engine matches on [`State`] and [`Action`]; config files spell them with
//! [`State::section`] and [`Action::name`].

/// The `ms-ime` preset, compiled into the binary.
pub const MS_IME_PRESET: &str = include_str!("../../../../data/keymap-ms-ime.toml");

/// The `atok` preset, compiled into the binary.
pub const ATOK_PRESET: &str = include_str!("../../../../data/keymap-atok.toml");

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
pub(super) const GLOBAL_SECTION: &str = "global";

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
