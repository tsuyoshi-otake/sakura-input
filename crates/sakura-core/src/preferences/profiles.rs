//! Per-application profiles: the shipped defaults for shells and IDEs, the
//! process-name rule, and how a new editing context resolves its starting
//! preferences from the global settings and the matching profile.

use crate::width::Normalizer;
use sakura_values::Mode;

use super::{
    ConversionMethod, InputMethod, InputSupport, Preferences, ShiftSpaceBehavior, SpaceWidth,
    SuggestAccept,
};

/// Fully resolved defaults for one host executable.
///
/// Profiles are resolved while loading configuration rather than on the key
/// path. A newly created context copies these four small values and thereafter
/// owns its mode, so refocusing an editor can never silently overwrite an
/// explicit mode change made by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppProfile {
    pub process_name: String,
    pub default_mode: Mode,
    pub normalizer: Normalizer,
    pub prediction_enabled: bool,
    pub suggest_accept: SuggestAccept,
}

impl AppProfile {
    pub(super) fn inherited(process_name: &str, preferences: Preferences) -> Self {
        Self {
            process_name: process_name.to_owned(),
            default_mode: preferences.default_mode,
            normalizer: preferences.normalizer,
            prediction_enabled: preferences.prediction_enabled,
            suggest_accept: preferences.suggest_accept,
        }
    }

    pub fn matches(&self, process_name: &str) -> bool {
        self.process_name.eq_ignore_ascii_case(process_name)
    }
}

/// The values copied into a newly created editing context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPreferences {
    pub default_mode: Mode,
    pub input_method: InputMethod,
    pub conversion_method: ConversionMethod,
    pub normalizer: Normalizer,
    pub space_width: SpaceWidth,
    pub shift_space_behavior: ShiftSpaceBehavior,
    pub prediction_enabled: bool,
    pub suggest_accept: SuggestAccept,
    pub association_enabled: bool,
    pub input_support: InputSupport,
}

/// Shipped profiles protect shell/IDE Tab completion before a user has opened
/// settings. User sections with the same process name override these values.
pub fn default_app_profiles(preferences: Preferences) -> Vec<AppProfile> {
    ["WindowsTerminal.exe", "Code.exe", "devenv.exe"]
        .into_iter()
        .map(|process_name| {
            let mut profile = AppProfile::inherited(process_name, preferences);
            // Start shell and IDE contexts with the IME genuinely off. Using
            // HalfAlnum here made the first half-width/full-width press switch
            // to Direct instead of Japanese; Direct makes that same first
            // press enter Hiragana, matching the system IME convention while
            // leaving ordinary shortcuts and typing entirely to the host.
            profile.default_mode = Mode::Direct;
            profile.prediction_enabled = false;
            profile.suggest_accept = SuggestAccept::Disabled;
            profile
        })
        .collect()
}

pub fn resolve_context_preferences(
    preferences: Preferences,
    profiles: &[AppProfile],
    process_name: &str,
) -> ContextPreferences {
    if let Some(profile) = profiles
        .iter()
        .find(|profile| profile.matches(process_name))
    {
        return ContextPreferences {
            default_mode: profile.default_mode,
            input_method: preferences.input_method,
            conversion_method: preferences.conversion_method,
            normalizer: profile.normalizer,
            space_width: preferences.space_width,
            shift_space_behavior: preferences.shift_space_behavior,
            prediction_enabled: profile.prediction_enabled,
            suggest_accept: profile.suggest_accept,
            association_enabled: preferences.association_enabled,
            input_support: preferences.input_support,
        };
    }
    ContextPreferences {
        default_mode: preferences.default_mode,
        input_method: preferences.input_method,
        conversion_method: preferences.conversion_method,
        normalizer: preferences.normalizer,
        space_width: preferences.space_width,
        shift_space_behavior: preferences.shift_space_behavior,
        prediction_enabled: preferences.prediction_enabled,
        suggest_accept: preferences.suggest_accept,
        association_enabled: preferences.association_enabled,
        input_support: preferences.input_support,
    }
}

pub fn is_valid_profile_process_name(process_name: &str) -> bool {
    !process_name.is_empty()
        && process_name.len() <= 128
        && process_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
