//! Versioned user preferences built on Sakura's deliberately small TOML subset.
//!
//! Unknown and missing fields are ignored/defaulted so a newer settings tool
//! cannot brick an older engine. Parsing still rejects structurally malformed
//! TOML: silently guessing where a broken quote ended would be less safe than
//! retaining the last known-good configuration at the file-loading layer.

mod model;
mod parse;
mod profiles;
mod serialize;

pub use model::{
    ConversionMethod, InputMethod, InputSupport, NeuralRerankerScope, NotationStyle, Preferences,
    ShiftSpaceBehavior, SpaceWidth, SuggestAccept,
};
pub use parse::{parse_preferences, ParsedPreferences};
pub use profiles::{
    default_app_profiles, is_valid_profile_process_name, resolve_context_preferences, AppProfile,
    ContextPreferences,
};
pub use serialize::{serialize_preferences, serialize_preferences_with_profiles};

// The appearance section is optional, so adding its theme key remains
// compatible with v4 readers and does not require a format-version bump.
pub const CONFIG_FORMAT_VERSION: u16 = 4;
pub const PREVIOUS_CONFIG_FORMAT_VERSION: u16 = 3;
const LEGACY_CONFIG_FORMAT_VERSION: u16 = 1;

#[cfg(test)]
use crate::keymap::Preset;
#[cfg(test)]
use crate::width::{
    BracketStyle, CommaMark, Normalizer, PeriodMark, PunctuationStyle, Width, WidthPolicy,
};
#[cfg(test)]
use parse::parse_punctuation;
#[cfg(test)]
use sakura_values::{AppearanceTheme, Mode, PadShortcut};
#[cfg(test)]
use serialize::{mode_name, punctuation_name};

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;
