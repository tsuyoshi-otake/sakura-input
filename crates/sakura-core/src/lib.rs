//! Sakura Input engine core: the conversion logic, with no Windows in it.
//!
//! Everything here is a pure function of its inputs — `(state, event) ->
//! (state, output)` — which is what makes the IME testable without a running
//! Windows, without TSF, and without a host application (DESIGN 5). A
//! `windows` dependency in this crate is a design violation, not an oversight.
//!
//! The modules stack in one direction, so the dependency graph stays acyclic:
//!
//! - [`cpu`] — which vector instructions this machine has, resolved once.
//! - [`simd`] — kernels selected by that answer; scalar-equivalent by test.
//! - [`text`] — the sink everything writes through.
//! - [`calendar`] — civil-date surfaces for readings such as `きょう`.
//! - [`numerals`] — Arabic/full-width/kanji rewrite of number readings.
//! - [`config`] — the small TOML subset every shipped data file is written in.
//! - [`dictionary`] — borrowed fixed-layout views over the mmap dictionary.
//! - [`romaji`] — the input FSM, compiled from a config document.
//! - [`width`] — the width and punctuation choke point (DESIGN 5.6).
//! - [`keymap`] — key bindings, also compiled from a config document.
//!
//! [`cpu`] and [`simd`] are the one concession to a specific machine, and
//! they are careful ones: both compile and pass their tests on any
//! architecture, because a core that could only be tested on the target
//! would give up the property the rest of this crate exists for.

pub mod calendar;
pub mod config;
pub mod conversion;
pub mod cpu;
pub mod dictionary;
pub mod editing;
pub mod input_repair;
pub mod keymap;
pub mod numerals;
pub mod preferences;
pub mod romaji;
/// Compatibility path: the kernels live in [`width::scan`] (Phase 3.3, #206).
pub use width::scan as simd;
pub mod text;
pub mod user_dictionary;
pub mod width;

// Flat re-exports exist only for items with a caller outside their module
// path; `ci/check-facade.ps1` (R10) fails on any that loses its last one.
// Everything else stays reachable through its `pub mod` path.
pub use calendar::{CivilDate, DateFormat};
pub use config::ParseError;
pub use conversion::{
    CandidateEvidence, CommitBridgeTail, ConversionCandidate, ConversionDiagnostics,
    ConversionError, ConversionInput, ConversionOptions, ConversionSearchTerminal,
    ConversionSegment, Converter, CrossCommitBridge, RightContextId, MAX_CONVERSION_CANDIDATES,
    MAX_CROSS_COMMIT_CURRENT_BYTES, MAX_CROSS_COMMIT_TAIL_BYTES,
};
pub use dictionary::{Dictionary, EntryFlags};
pub use editing::{transform_into, SegmentTransform};
pub use input_repair::{
    allows_system_entry, collect_repair_variants, contextual_punctuation_swap, RepairKind,
    MAX_PREDICTION_REPAIR_VARIANTS, MAX_REPAIR_VARIANTS,
};
pub use keymap::{KeyMap, Preset};
pub use preferences::{
    default_app_profiles, is_valid_profile_process_name, parse_preferences,
    resolve_context_preferences, serialize_preferences_with_profiles, AppProfile,
    ContextPreferences, ConversionMethod, InputMethod, InputSupport, NeuralRerankerScope,
    NotationStyle, Preferences, ShiftSpaceBehavior, SpaceWidth, SuggestAccept,
    CONFIG_FORMAT_VERSION,
};
pub use romaji::Input;
pub use sakura_values::{AppearanceTheme, PadShortcut};
pub use text::TextSink;
pub use user_dictionary::{
    UserDictionary, UserDictionaryEntry, UserDictionaryError, UserPartOfSpeech,
    MAX_USER_DICTIONARY_ENTRIES,
};
pub use width::{BracketStyle, CommaMark, Normalizer, PeriodMark, PunctuationStyle, Width};
