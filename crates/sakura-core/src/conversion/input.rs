use sakura_values::MAX_PREEDIT_BYTES;

use super::ConversionError;

/// How the converter treats the caller-supplied literal surface.
///
/// `Ranked` is the ordinary N-best path.  The two exact policies are
/// deliberately explicit: they bypass inference paths that could otherwise
/// rewrite an opaque token or an unresolved Latin fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiteralPolicy {
    #[default]
    Ranked,
    ExactTop1,
    ExactOnly,
}

/// The caller's classification for one conversion request.
///
/// The class and [`LiteralPolicy`] form a checked pair. Keeping the class in
/// the conversion input makes the policy boundary visible to every consumer
/// instead of relying on a convention around a raw reading string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionInputClass {
    Ordinary,
    OpaqueAsciiIdentifier,
    MixedUnresolvedLatin,
}

/// A conversion lookup reading together with the literal surface the user
/// typed before lookup normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversionInput<'a> {
    pub lookup_reading: &'a str,
    pub exact_surface: &'a str,
    pub class: ConversionInputClass,
    pub literal_policy: LiteralPolicy,
}

impl<'a> ConversionInput<'a> {
    /// Preserves the legacy conversion contract for callers that supply only a
    /// reading: ordinary lookup and normal cost ranking.
    pub const fn ordinary(reading: &'a str) -> Self {
        Self {
            lookup_reading: reading,
            exact_surface: reading,
            class: ConversionInputClass::Ordinary,
            literal_policy: LiteralPolicy::Ranked,
        }
    }

    pub const fn new(
        lookup_reading: &'a str,
        exact_surface: &'a str,
        class: ConversionInputClass,
        literal_policy: LiteralPolicy,
    ) -> Self {
        Self {
            lookup_reading,
            exact_surface,
            class,
            literal_policy,
        }
    }

    pub(super) fn validate(self) -> Result<(), ConversionError> {
        if self.lookup_reading.is_empty() {
            return Err(ConversionError::EmptyReading);
        }
        if self.lookup_reading.len() > MAX_PREEDIT_BYTES {
            return Err(ConversionError::ReadingTooLong);
        }
        if self.exact_surface.is_empty() || self.exact_surface.len() > MAX_PREEDIT_BYTES {
            return Err(ConversionError::InvalidOptions);
        }

        match (self.class, self.literal_policy) {
            (ConversionInputClass::Ordinary, LiteralPolicy::Ranked) => Ok(()),
            (ConversionInputClass::OpaqueAsciiIdentifier, LiteralPolicy::ExactTop1) => {
                if self.lookup_reading.len() != self.exact_surface.len()
                    || !self.lookup_reading.eq_ignore_ascii_case(self.exact_surface)
                    || !is_ascii_alpha_digit_identifier(self.lookup_reading)
                    || !is_ascii_alpha_digit_identifier(self.exact_surface)
                {
                    return Err(ConversionError::InvalidOptions);
                }
                Ok(())
            }
            (ConversionInputClass::MixedUnresolvedLatin, LiteralPolicy::ExactOnly) => {
                if self.lookup_reading != self.exact_surface
                    || !is_mixed_unresolved_latin(self.lookup_reading)
                    || !is_mixed_unresolved_latin(self.exact_surface)
                {
                    return Err(ConversionError::InvalidOptions);
                }
                Ok(())
            }
            _ => Err(ConversionError::InvalidOptions),
        }
    }
}

fn is_ascii_alpha_digit_identifier(value: &str) -> bool {
    let mut has_alpha = false;
    let mut has_digit = false;
    for byte in value.bytes() {
        if byte.is_ascii_alphabetic() {
            has_alpha = true;
        } else if byte.is_ascii_digit() {
            has_digit = true;
        } else {
            return false;
        }
    }
    has_alpha && has_digit
}

fn is_mixed_unresolved_latin(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_ascii_alphabetic())
}
