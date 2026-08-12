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

pub mod config;
pub mod conversion;
pub mod cpu;
pub mod dictionary;
pub mod editing;
pub mod keymap;
pub mod preferences;
pub mod romaji;
pub mod simd;
pub mod text;
pub mod user_dictionary;
pub mod width;

pub use config::{parse as parse_config, Document, ErrorKind, ParseError, Value};
pub use conversion::{
    ConversionCandidate, ConversionDiagnostics, ConversionError, ConversionOptions,
    ConversionResult, ConversionSearchTerminal, ConversionSegment, Converter,
};
pub use cpu::{CpuFeatures, UnsupportedCpu};
pub use dictionary::{Dictionary, Entry, EntryFlags, PrefixMatch};
pub use editing::{identifier_into, transform_into, IdentifierStyle, SegmentTransform};
pub use keymap::{Action, KeyMap, KeyMapError, KeyMapErrorKind, Preset, State};
pub use preferences::{
    default_app_profiles, is_valid_profile_process_name, parse_preferences,
    resolve_context_preferences, serialize_preferences, serialize_preferences_with_profiles,
    AppProfile, ContextPreferences, ConversionMethod, InputMethod, NeuralRerankerScope,
    ParsedPreferences, Preferences, ShiftSpaceBehavior, SpaceWidth, SuggestAccept,
    CONFIG_FORMAT_VERSION,
};
pub use romaji::{Input, Table, TableError, TableErrorKind};
pub use sakura_proto::AppearanceTheme;
pub use simd::{KernelMetadata, KernelSet, WidthScanStrategy, WidthScanStrategyId};
pub use text::TextSink;
pub use user_dictionary::{
    UserDictionary, UserDictionaryEntry, UserDictionaryError, UserDictionaryErrorKind,
    UserPartOfSpeech, UserPosSpec, MAX_USER_DICTIONARY_ENTRIES, USER_DICTIONARY_FORMAT_VERSION,
};
pub use width::{BracketStyle, Normalizer, PunctuationStyle, Width, WidthPolicy};
