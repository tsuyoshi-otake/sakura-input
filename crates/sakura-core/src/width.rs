//! The width-policy choke point (DESIGN.md §2 "Alphanumeric width policy",
//! §2 "Punctuation style", §5.6 "Width policy choke point").
//!
//! Every string the engine hands back to a caller — typed alnum-mode text,
//! conversion candidates, prediction output, F-key transforms, reconversion
//! — is meant to pass through [`Normalizer::normalize_into`] exactly once,
//! immediately before it leaves the engine. Centralizing the transform here,
//! instead of teaching every producer of text to respect the width policy
//! individually, is the whole point: one enforcement site means no code path
//! can leak a width the user did not ask for.
//!
//! This module is a pure, platform-free, allocation-free `char -> char`
//! transform plus the plumbing to walk a `&str` through it into a
//! [`TextSink`]. It knows nothing about romaji, dictionaries, or TSF — only
//! Unicode code points and the two settings that govern them.

use crate::text::TextSink;
use sakura_proto::{Mode, Overflow};

/// A width policy value for one character class (DESIGN §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    Half,
    Full,
    /// Defers the decision to the current [`Mode`]: full-width only in
    /// [`Mode::FullAlnum`], half-width in every other mode. This exists for
    /// the case where the mode indicator itself should be the single source
    /// of truth for width, mirroring older IMEs — as opposed to `Half` and
    /// `Full`, which pin the width regardless of mode. `Half` is the
    /// crate-wide default (DESIGN §2) for the same reason `FollowMode` is
    /// *not* the default: most engineers want `docker` to stay `docker`
    /// even while composing in Hiragana mode with a stray English word, not
    /// have it widen because the mode indicator happens to say something
    /// else.
    FollowMode,
}

/// The three independently-configurable width-policy channels (DESIGN §2).
/// Alphabetic letters, digits, and symbols each get their own setting
/// because real preferences split along this line — e.g. half-width letters
/// and digits with full-width symbols kept for visual alignment in
/// full-width prose, or any other combination. The three fields are applied
/// completely independently; nothing here couples one channel's decision to
/// another's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidthPolicy {
    pub alnum: Width,
    pub number: Width,
    pub symbol: Width,
}

impl Default for WidthPolicy {
    /// All three channels default to `Half`: DESIGN §2 is explicit that
    /// engineers never want `ｄｏｃｋｅｒ`.
    fn default() -> Self {
        WidthPolicy {
            alnum: Width::Half,
            number: Width::Half,
            symbol: Width::Half,
        }
    }
}

/// Which pair of comma-role/period-role characters the punctuation choke
/// point emits (DESIGN §2 "Punctuation style").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunctuationStyle {
    /// `、` and `。` — traditional Japanese prose punctuation.
    KutenTouten,
    /// `，` and `．` — full-width Western-style punctuation, common in
    /// mixed EN/JP technical writing.
    CommaPeriod,
    /// `、` and `．` — the mixed convention some engineers prefer: Japanese
    /// comma, Western period.
    Mixed,
}

impl Default for PunctuationStyle {
    /// `KutenTouten`: traditional Japanese prose is the unsurprising
    /// default; `CommaPeriod`/`Mixed` are opt-in for mixed EN/JP writing.
    fn default() -> Self {
        PunctuationStyle::KutenTouten
    }
}

/// The width-policy choke point itself: a pure `(text, mode) -> text`
/// transform parameterized by the two DESIGN §2 settings. Stateless and
/// `Copy` — callers are expected to hold one of these per session (or one
/// globally, if width policy is not per-app) and reuse it for every output
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Normalizer {
    pub width: WidthPolicy,
    pub punctuation: PunctuationStyle,
}

impl Normalizer {
    /// Normalizes every character of `src` according to `mode` and this
    /// normalizer's policy, appending the result to `dst`.
    ///
    /// Overflow is atomic **per character**, not per call: [`TextSink`]
    /// guarantees each individual `push` either fully lands or leaves the
    /// sink untouched, but nothing here rolls back characters already
    /// pushed earlier in this same call. On `Err(Overflow)`, `dst` holds
    /// the normalized prefix up to (but not including) the character that
    /// did not fit. That is deliberate, not a shortcut: the caller is
    /// expected to size its buffer for the traffic it carries, and a
    /// half-normalized string left in the sink is strictly more useful to a
    /// caller than silently dropping the tail would be.
    pub fn normalize_into(
        &self,
        src: &str,
        mode: Mode,
        dst: &mut impl TextSink,
    ) -> Result<(), Overflow> {
        for c in src.chars() {
            dst.push(self.normalize_char(c, mode))?;
        }
        Ok(())
    }

    /// Normalizes one character. This is the entire policy in one pure
    /// function; [`Normalizer::normalize_into`] is just this applied over
    /// an iterator with no allocation in between.
    pub fn normalize_char(&self, c: char, mode: Mode) -> char {
        // Japanese punctuation is checked first, unconditionally: it is
        // governed *only* by `punctuation`, never by `width.symbol`, even
        // though ，(U+FF0C) and ．(U+FF0E) sit inside the exact same
        // U+FF01..=U+FF5E arithmetic range as every other full-width
        // symbol. If this check ran after classification instead of
        // before it, `symbol = Half` would shrink ，back to `,` and
        // silently undo `punctuation = CommaPeriod` — two independent
        // settings would end up fighting over the same code point. ASCII
        // `,`/`.` are not among the four code points this owns, so they
        // fall through untouched to ordinary symbol-width handling below.
        if let Some(role) = punct_role(c) {
            return map_punct(role, self.punctuation);
        }
        match classify(c) {
            CharClass::Alpha => apply_width(c, wants_full(self.width.alnum, mode)),
            CharClass::Digit => apply_width(c, wants_full(self.width.number, mode)),
            CharClass::Symbol => apply_width(c, wants_full(self.width.symbol, mode)),
            // Kana, kanji, emoji, control characters: the width policy has
            // no opinion on these, so they pass through byte-for-byte.
            CharClass::Other => c,
        }
    }
}

/// The three character classes the width policy governs. Japanese
/// punctuation deliberately has no variant here — it is resolved before
/// classification ever runs (see the comment in `normalize_char`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Alpha,
    Digit,
    Symbol,
    Other,
}

/// Classifies `c` by the identity it has once collapsed to half-width, so a
/// full-width letter/digit/symbol already present in `src` (e.g.
/// re-normalizing previously-normalized text, or text a producer built by
/// pasting together dictionary surfaces) is recognized just as reliably as
/// its ASCII form would be.
fn classify(c: char) -> CharClass {
    let probe = to_half(c);
    if probe.is_ascii_alphabetic() {
        CharClass::Alpha
    } else if probe.is_ascii_digit() {
        CharClass::Digit
    } else if probe == ' ' || matches!(probe, '!'..='~') {
        CharClass::Symbol
    } else {
        CharClass::Other
    }
}

/// Resolves a `Width` setting to a concrete half/full decision for `mode`.
/// Returns `true` for full-width.
fn wants_full(width: Width, mode: Mode) -> bool {
    match width {
        Width::Half => false,
        Width::Full => true,
        Width::FollowMode => matches!(mode, Mode::FullAlnum),
    }
}

/// Applies a resolved half/full decision to one character.
fn apply_width(c: char, full: bool) -> char {
    if full {
        to_full(c)
    } else {
        to_half(c)
    }
}

/// Collapses a half- or full-width character to its half-width form.
/// Idempotent: half-width input, and anything outside the two mapped
/// ranges, passes through unchanged — which is what lets [`classify`] use
/// this as a "canonical half-width identity" probe regardless of which
/// width the input character actually arrived in.
fn to_half(c: char) -> char {
    match c {
        '\u{3000}' => ' ',
        '\u{FF01}'..='\u{FF5E}' => {
            // The exact reverse of the arithmetic in `to_full`: this range
            // is `0xFEE0` above `0x0021..=0x007E`, so subtracting always
            // lands back on an ASCII printable code point and never on a
            // surrogate. `unwrap_or(c)` is an unreachable-in-practice
            // fallback, not a real error path — kept instead of `unsafe`
            // or `unwrap` per the crate's no-`unsafe`, no-panic-on-input
            // discipline.
            char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
        }
        _ => c,
    }
}

/// Widens a half-width character to its full-width form. Idempotent:
/// already-full-width input, and anything outside ASCII printable plus
/// space, passes through unchanged.
fn to_full(c: char) -> char {
    match c {
        ' ' => '\u{3000}',
        '\u{0021}'..='\u{007E}' => char::from_u32(c as u32 + 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

/// Which role a Japanese-punctuation code point plays: the comma-like pause
/// mark, or the period-like full stop. Orthogonal to [`PunctuationStyle`],
/// which picks the concrete glyph for each role.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PunctRole {
    Comma,
    Period,
}

/// Identifies the four code points the punctuation choke point owns
/// (DESIGN §2). Returns `None` for every other character, including ASCII
/// `,`/`.`, which are ordinary symbols governed by `width.symbol` instead.
fn punct_role(c: char) -> Option<PunctRole> {
    match c {
        '\u{3001}' | '\u{FF0C}' => Some(PunctRole::Comma), // 、 ，
        '\u{3002}' | '\u{FF0E}' => Some(PunctRole::Period), // 。 ．
        _ => None,
    }
}

/// Picks the configured glyph for `role` under `style`.
fn map_punct(role: PunctRole, style: PunctuationStyle) -> char {
    match (style, role) {
        (PunctuationStyle::KutenTouten, PunctRole::Comma) => '\u{3001}', // 、
        (PunctuationStyle::KutenTouten, PunctRole::Period) => '\u{3002}', // 。
        (PunctuationStyle::CommaPeriod, PunctRole::Comma) => '\u{FF0C}', // ，
        (PunctuationStyle::CommaPeriod, PunctRole::Period) => '\u{FF0E}', // ．
        (PunctuationStyle::Mixed, PunctRole::Comma) => '\u{3001}',       // 、
        (PunctuationStyle::Mixed, PunctRole::Period) => '\u{FF0E}',      // ．
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_proto::FixedStr;

    #[test]
    fn width_offset_is_pinned_to_literal_code_points() {
        let full = Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Full,
                symbol: Width::Full,
            },
            punctuation: PunctuationStyle::default(),
        };
        assert_eq!(full.normalize_char('a', Mode::Direct), 'ａ');
        assert_eq!(full.normalize_char('0', Mode::Direct), '０');
        assert_eq!(full.normalize_char('@', Mode::Direct), '＠');
    }

    #[test]
    fn ascii_printable_half_to_full_to_half_is_identity_except_comma_and_period() {
        let full = Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Full,
                symbol: Width::Full,
            },
            punctuation: PunctuationStyle::default(),
        };
        let half = Normalizer::default();
        for cp in 0x21u32..=0x7E {
            let c = char::from_u32(cp).expect("valid ASCII printable code point");
            // ',' and '.' are excluded on purpose: their full-width forms,
            // ，(U+FF0C) and ．(U+FF0E), are two of the four code points the
            // punctuation choke point permanently owns (rule 4). Round-
            // tripping them through the *symbol* channel is impossible by
            // design — see `comma_and_period_full_width_forms_are_owned_by_punctuation`
            // below for the actual, documented, correct behavior.
            if c == ',' || c == '.' {
                continue;
            }
            let widened = full.normalize_char(c, Mode::Direct);
            assert_eq!(widened as u32, cp + 0xFEE0, "half->full offset for {c:?}");
            let narrowed = half.normalize_char(widened, Mode::Direct);
            assert_eq!(narrowed, c, "full->half round trip for {c:?}");
        }
    }

    #[test]
    fn comma_and_period_full_width_forms_are_owned_by_punctuation() {
        // Forward: ASCII ','/'.' really are governed by `symbol` per rule
        // 4, so widening them lands on the same code points, ，/．, that
        // the punctuation choke point also emits under `CommaPeriod`.
        let full_symbol = Normalizer {
            width: WidthPolicy {
                alnum: Width::Half,
                number: Width::Half,
                symbol: Width::Full,
            },
            punctuation: PunctuationStyle::default(),
        };
        assert_eq!(full_symbol.normalize_char(',', Mode::Direct), '\u{FF0C}');
        assert_eq!(full_symbol.normalize_char('.', Mode::Direct), '\u{FF0E}');

        // Backward: feeding those same full-width forms back through a
        // Half-symbol normalizer does NOT recover ','/'.' — `punct_role`
        // claims them unconditionally and routes them through the
        // punctuation style instead (KutenTouten by default here). This is
        // exactly why the general round-trip test above excludes these two
        // characters: it is not a gap in the implementation.
        let half_symbol = Normalizer::default();
        assert_eq!(
            half_symbol.normalize_char('\u{FF0C}', Mode::Direct),
            '\u{3001}'
        );
        assert_eq!(
            half_symbol.normalize_char('\u{FF0E}', Mode::Direct),
            '\u{3002}'
        );
    }

    #[test]
    fn space_maps_to_ideographic_space_and_back() {
        let full = Normalizer {
            width: WidthPolicy {
                alnum: Width::Half,
                number: Width::Half,
                symbol: Width::Full,
            },
            punctuation: PunctuationStyle::default(),
        };
        let half = Normalizer::default();
        assert_eq!(full.normalize_char(' ', Mode::Direct), '\u{3000}');
        assert_eq!(half.normalize_char('\u{3000}', Mode::Direct), ' ');
    }

    #[test]
    fn default_policy_never_widens_docker_in_any_mode() {
        let normalizer = Normalizer::default();
        for mode in Mode::ALL {
            let mut out = String::new();
            normalizer
                .normalize_into("docker", mode, &mut out)
                .expect("fits in a growable String");
            assert_eq!(out, "docker", "mode {mode:?} widened docker");
        }
    }

    #[test]
    fn alnum_and_number_channels_are_independent() {
        let normalizer = Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Half,
                symbol: Width::Half,
            },
            punctuation: PunctuationStyle::default(),
        };
        let mut out = String::new();
        normalizer
            .normalize_into("abc123", Mode::Direct, &mut out)
            .expect("fits");
        assert_eq!(out, "ａｂｃ123");
    }

    #[test]
    fn follow_mode_widens_only_in_full_alnum_mode() {
        let normalizer = Normalizer {
            width: WidthPolicy {
                alnum: Width::FollowMode,
                number: Width::FollowMode,
                symbol: Width::FollowMode,
            },
            punctuation: PunctuationStyle::default(),
        };
        assert_eq!(normalizer.normalize_char('a', Mode::FullAlnum), 'ａ');
        for mode in Mode::ALL {
            if mode == Mode::FullAlnum {
                continue;
            }
            assert_eq!(
                normalizer.normalize_char('a', mode),
                'a',
                "mode {mode:?} should stay half-width"
            );
        }
    }

    #[test]
    fn punctuation_style_normalizes_from_any_of_the_four_source_forms() {
        // Every style must produce its configured pair no matter which of
        // the four source code points (、 。 ， ．) the caller hands in —
        // covering the "backward" direction too, e.g. ，-> 、 under
        // KutenTouten, not just the forward 、-> 、 identity.
        let sources = ['\u{3001}', '\u{3002}', '\u{FF0C}', '\u{FF0E}'];
        let cases: [(PunctuationStyle, [char; 4]); 3] = [
            (
                PunctuationStyle::KutenTouten,
                ['\u{3001}', '\u{3002}', '\u{3001}', '\u{3002}'],
            ),
            (
                PunctuationStyle::CommaPeriod,
                ['\u{FF0C}', '\u{FF0E}', '\u{FF0C}', '\u{FF0E}'],
            ),
            (
                PunctuationStyle::Mixed,
                ['\u{3001}', '\u{FF0E}', '\u{3001}', '\u{FF0E}'],
            ),
        ];
        for (style, expected) in cases {
            let normalizer = Normalizer {
                width: WidthPolicy::default(),
                punctuation: style,
            };
            for (src, want) in sources.iter().zip(expected.iter()) {
                assert_eq!(
                    normalizer.normalize_char(*src, Mode::Direct),
                    *want,
                    "style {style:?} src {src:?}"
                );
            }
        }
    }

    #[test]
    fn punctuation_choke_point_ignores_the_symbol_width_policy() {
        // If `symbol: Half` were allowed to touch ，/．, this would shrink
        // them to ASCII ','/'.' and silently undo `punctuation:
        // CommaPeriod` (see the comment on `normalize_char`). It must not.
        let normalizer = Normalizer {
            width: WidthPolicy {
                alnum: Width::Half,
                number: Width::Half,
                symbol: Width::Half,
            },
            punctuation: PunctuationStyle::CommaPeriod,
        };
        assert_eq!(
            normalizer.normalize_char('\u{3001}', Mode::Direct),
            '\u{FF0C}'
        );
        assert_eq!(
            normalizer.normalize_char('\u{FF0C}', Mode::Direct),
            '\u{FF0C}'
        );
        assert_eq!(
            normalizer.normalize_char('\u{FF0E}', Mode::Direct),
            '\u{FF0E}'
        );

        // And the converse: ASCII ',' really is governed by `symbol`, and
        // a half-width policy leaves it as ',' — it is not swept into the
        // punctuation choke point just because it looks similar.
        assert_eq!(normalizer.normalize_char(',', Mode::Direct), ',');
        assert_eq!(normalizer.normalize_char('.', Mode::Direct), '.');
    }

    #[test]
    fn kana_kanji_and_non_bmp_characters_pass_through_untouched() {
        // A policy that widens everything it is allowed to touch, to prove
        // these characters are outside the width policy's reach entirely —
        // not just coincidentally unaffected by a Half default.
        let normalizer = Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Full,
                symbol: Width::Full,
            },
            punctuation: PunctuationStyle::CommaPeriod,
        };
        for c in ['あ', 'ア', '漢', '𠮷', '🍣'] {
            assert_eq!(normalizer.normalize_char(c, Mode::Direct), c);
        }
    }

    #[test]
    fn normalize_into_reports_overflow_into_a_fixed_str() {
        let normalizer = Normalizer::default();
        let mut dst = FixedStr::<2>::new();
        let result = normalizer.normalize_into("abc", Mode::Direct, &mut dst);
        assert_eq!(result, Err(Overflow));
        // Atomic per character: the two that fit landed, the third did not.
        assert_eq!(dst.as_str(), "ab");
    }

    #[test]
    fn empty_input_produces_empty_output() {
        let normalizer = Normalizer::default();
        let mut dst = String::new();
        normalizer
            .normalize_into("", Mode::Direct, &mut dst)
            .expect("empty input always fits");
        assert_eq!(dst, "");
    }

    #[test]
    fn defaults_match_design_defaults() {
        assert_eq!(
            WidthPolicy::default(),
            WidthPolicy {
                alnum: Width::Half,
                number: Width::Half,
                symbol: Width::Half,
            }
        );
        assert_eq!(PunctuationStyle::default(), PunctuationStyle::KutenTouten);
        assert_eq!(
            Normalizer::default(),
            Normalizer {
                width: WidthPolicy::default(),
                punctuation: PunctuationStyle::default(),
            }
        );
    }
}
