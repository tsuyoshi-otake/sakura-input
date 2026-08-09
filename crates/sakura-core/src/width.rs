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

use crate::editing::{half_katakana, katakana_char};
use crate::simd;
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
    ///
    /// Most text arrives already in the width the policy wants — a half-width
    /// policy changes no ASCII at all, and kana and kanji are outside the
    /// policy's reach — so this does not walk characters it has nothing to
    /// say about. [`simd`] finds each run of bytes that will come out
    /// unchanged and the run is copied in one move, leaving the
    /// character-at-a-time path for the characters that actually change.
    /// Observably this is identical to mapping [`Normalizer::normalize_char`]
    /// over `src.chars()`, which is what the tests assert.
    ///
    /// # What that is worth
    ///
    /// Nanoseconds per call, per-character loop → this, best of seven runs on
    /// one development machine (`tests/width_bench.rs`). The benchmark prints
    /// the selected strategy separately; a CPU advertising AVX-512 does not
    /// imply that a given input executed a 512-bit body:
    ///
    /// | text | half-width policy (the default) | every channel full-width |
    /// |------|--------------------------------:|-------------------------:|
    /// | one keystroke | 1.7 → 2.1 | 1.6 → 2.8 |
    /// | a 45-byte shell command | 55 → 10 | 62 → 75 |
    /// | 90 bytes of Japanese prose | 54 → 58 | 54 → 59 |
    /// | 84 bytes of mixed Japanese and ASCII | 76 → 50 | 77 → 79 |
    ///
    /// The left column is what ships, and the command line is the case this
    /// exists for: five times faster, because an engineer's committed text is
    /// mostly ASCII the policy has nothing to say about. Everything else is
    /// within a nanosecond or two either way.
    ///
    /// The right column is the price of the guard when *no* ASCII survives
    /// the policy, so every run is empty and the scan is pure overhead —
    /// around 20% of a cost measured in tens of nanoseconds, against a
    /// per-keystroke budget of 5 ms (DESIGN 10). It is a real regression, and
    /// it is knowingly accepted: the configuration that pays it is the opt-in
    /// one, and the configuration that gains is the shipped one.
    #[inline]
    pub fn normalize_into(
        &self,
        src: &str,
        mode: Mode,
        dst: &mut impl TextSink,
    ) -> Result<(), Overflow> {
        if matches!(mode, Mode::Katakana | Mode::HalfKatakana) {
            return self.normalize_kana(src, mode, dst);
        }
        // Below one vector block there is nothing to amortize the scanner's
        // setup over, and this is the shape the hot path actually has: a
        // preedit is a handful of characters, and a keystroke is one. So the
        // short case is handled *here*, in a body small enough to inline into
        // the caller, and only longer strings pay for a call.
        if src.len() < simd::MIN_VECTOR_BYTES {
            for c in src.chars() {
                dst.push(self.normalize_char(c, mode))?;
            }
            return Ok(());
        }
        self.normalize_runs(src, mode, dst)
    }

    /// Renders kana modes after romaji composition has produced hiragana.
    /// Half-width katakana is a one-to-many transform for voiced kana, so it
    /// cannot be expressed by `normalize_char` and is handled before the
    /// ordinary width-policy fast path.
    fn normalize_kana(
        &self,
        src: &str,
        mode: Mode,
        dst: &mut impl TextSink,
    ) -> Result<(), Overflow> {
        for character in src.chars() {
            let katakana = katakana_char(character);
            if mode == Mode::HalfKatakana {
                if let Some(mapped) = half_katakana(katakana) {
                    dst.push_str(mapped)?;
                    continue;
                }
            }
            dst.push(self.normalize_char(katakana, mode))?;
        }
        Ok(())
    }

    /// The long-string half of [`Normalizer::normalize_into`], kept out of
    /// line so that inlining the short case does not drag the scanner, the
    /// dispatch, and the overflow-replay path into every call site.
    fn normalize_runs(
        &self,
        src: &str,
        mode: Mode,
        dst: &mut impl TextSink,
    ) -> Result<(), Overflow> {
        self.normalize_runs_with(src, mode, dst, simd::passthrough_len)
    }

    /// The long-string body parameterized by the already-selected run scanner.
    /// Production always passes [`simd::passthrough_len`]. Keeping the scanner
    /// at this narrow boundary lets the SIMD unit benchmark compare concrete
    /// safe-to-call kernels end to end without swapping the process-global
    /// dispatch pointer while tests run in parallel.
    fn normalize_runs_with(
        &self,
        src: &str,
        mode: Mode,
        dst: &mut impl TextSink,
        mut scan: impl FnMut(&[u8], &simd::Lut) -> usize,
    ) -> Result<(), Overflow> {
        // Resolved once per call rather than once per character: this is the
        // whole of what the policy has to say about single-byte characters.
        let lut = self.passthrough_lut(mode);
        let mut rest = src;
        while let Some(&first) = rest.as_bytes().first() {
            // Asking whether a run *starts* here is a table lookup; asking
            // how long it is may be a vector load. Japanese text stops a run
            // at every character, so checking first is what keeps kana from
            // paying vector cost to be told zero.
            if simd::admits(lut, first) {
                // Sound because a run only ever covers ASCII bytes, so both
                // ends are character boundaries (see `simd`'s module docs).
                let (run, tail) = rest.split_at(scan(rest.as_bytes(), lut));
                if dst.push_str(run).is_err() {
                    // `push_str` is all-or-nothing, so nothing landed — and
                    // this function promises the prefix that fits, not an
                    // untouched sink. Replaying the run one character at a
                    // time is what reproduces the documented behaviour.
                    for c in run.chars() {
                        dst.push(c)?;
                    }
                }
                rest = tail;
                continue;
            }
            // Walked with the iterator rather than re-sliced by index:
            // `&src[at..]` re-validates a character boundary every time, and
            // on Japanese text — where every single character takes this
            // branch — that check costs more than the run scan saves.
            let mut chars = rest.chars();
            let c = chars.next().expect("`rest` is not empty");
            dst.push(self.normalize_char(c, mode))?;
            rest = chars.as_str();
        }
        Ok(())
    }

    /// Test-only entry point for paired AVX2/AVX-512 normalizer measurements.
    ///
    /// Unlike changing `ACTIVE_WIDTH_SCAN`, this does not mutate global state,
    /// so it remains sound when Rust's test harness runs unrelated normalizer
    /// tests at the same time. The caller must establish the raw kernel's
    /// target-feature requirements before calling it.
    #[cfg(test)]
    pub(crate) unsafe fn normalize_into_with_scan(
        &self,
        src: &str,
        mode: Mode,
        dst: &mut impl TextSink,
        scan: unsafe fn(&[u8], &simd::Lut) -> usize,
    ) -> Result<(), Overflow> {
        if matches!(mode, Mode::Katakana | Mode::HalfKatakana) || src.len() < simd::MIN_VECTOR_BYTES
        {
            return self.normalize_into(src, mode, dst);
        }
        self.normalize_runs_with(src, mode, dst, |bytes, lut| {
            if bytes.len() < simd::MIN_VECTOR_BYTES {
                // Match production's caller-side scalar short-input path. The
                // global strategy is not observed here because
                // `passthrough_len` returns before reading it for this range.
                return simd::passthrough_len(bytes, lut);
            }
            // SAFETY: upheld by this test-only API's caller.
            unsafe { scan(bytes, lut) }
        })
    }

    /// The set of single-byte characters this policy leaves alone in `mode`.
    fn passthrough_lut(&self, mode: Mode) -> &'static simd::Lut {
        simd::passthrough_lut(
            wants_full(self.width.alnum, mode),
            wants_full(self.width.number, mode),
            wants_full(self.width.symbol, mode),
        )
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
    fn kana_modes_render_their_declared_script() {
        let normalizer = Normalizer::default();
        let source = "\u{304b}\u{304c}"; // かが
        let mut full = FixedStr::<32>::new();
        normalizer
            .normalize_into(source, Mode::Katakana, &mut full)
            .expect("full-width kana fits");
        assert_eq!(full.as_str(), "\u{30ab}\u{30ac}"); // カガ

        let mut half = FixedStr::<32>::new();
        normalizer
            .normalize_into(source, Mode::HalfKatakana, &mut half)
            .expect("half-width kana fits");
        assert_eq!(half.as_str(), "\u{ff76}\u{ff76}\u{ff9e}"); // ｶｶﾞ
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

    /// Every normalizer worth building, so the agreement tests below cover
    /// all eight passthrough tables rather than the default one.
    fn every_normalizer() -> Vec<Normalizer> {
        let widths = [Width::Half, Width::Full, Width::FollowMode];
        let styles = [
            PunctuationStyle::KutenTouten,
            PunctuationStyle::CommaPeriod,
            PunctuationStyle::Mixed,
        ];
        let mut all = Vec::new();
        for alnum in widths {
            for number in widths {
                for symbol in widths {
                    for punctuation in styles {
                        all.push(Normalizer {
                            width: WidthPolicy {
                                alnum,
                                number,
                                symbol,
                            },
                            punctuation,
                        });
                    }
                }
            }
        }
        all
    }

    /// Text chosen to straddle every kernel's block size (16, 32 and 64
    /// bytes) and to mix the cases that end a run — non-ASCII, punctuation,
    /// characters the policy widens — with the ones that do not.
    fn corpus() -> Vec<String> {
        let mut all: Vec<String> = [
            "",
            "a",
            "docker",
            "、",
            "。",
            "，",
            "．",
            "こんにちは",
            "ａｂｃ１２３",
            "\u{3000}",
            "🍣",
            "\0\u{1}\u{7f}",
            "日本語とEnglishが混ざったテキスト、句読点。",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

        for base in [
            "abcdefghijklmnop",
            "0123456789!@#$%^",
            "あいうえお",
            "aあ1、",
            " \t\n",
        ] {
            for repeat in [1usize, 2, 3, 5, 9] {
                all.push(base.repeat(repeat));
            }
        }
        all
    }

    /// The width-policy run path must be observably identical to the scalar
    /// replaced — [`Normalizer::normalize_char`] over every character. This
    /// is the assertion that stands between a vector kernel and the user's
    /// text, so it runs over every policy, every mode, and text long enough
    /// to reach the widest kernel this machine has.
    #[test]
    fn normalize_into_agrees_with_normalize_char_everywhere() {
        for normalizer in every_normalizer() {
            for mode in Mode::ALL {
                if matches!(mode, Mode::Katakana | Mode::HalfKatakana) {
                    continue;
                }
                for src in corpus() {
                    let expected: String = src
                        .chars()
                        .map(|c| normalizer.normalize_char(c, mode))
                        .collect();
                    let mut actual = String::new();
                    normalizer
                        .normalize_into(&src, mode, &mut actual)
                        .expect("a String never overflows");
                    assert_eq!(
                        actual, expected,
                        "{normalizer:?} in {mode:?} disagreed on {src:?}"
                    );
                }
            }
        }
    }

    /// The same agreement, driven by character value rather than by string
    /// shape: every ASCII character in turn, buried in a long run so it is
    /// classified by a vector kernel rather than by the scalar tail.
    #[test]
    fn every_ascii_character_survives_the_run_path_identically() {
        let policies = [
            Normalizer::default(),
            Normalizer {
                width: WidthPolicy {
                    alnum: Width::Full,
                    number: Width::Full,
                    symbol: Width::Full,
                },
                punctuation: PunctuationStyle::CommaPeriod,
            },
        ];
        for normalizer in policies {
            for cp in 0u32..0x80 {
                let c = char::from_u32(cp).expect("every ASCII code point is a character");
                let src = format!("{}{c}{}", "x".repeat(40), "y".repeat(40));
                let expected: String = src
                    .chars()
                    .map(|c| normalizer.normalize_char(c, Mode::Direct))
                    .collect();
                let mut actual = String::new();
                normalizer
                    .normalize_into(&src, Mode::Direct, &mut actual)
                    .expect("a String never overflows");
                assert_eq!(actual, expected, "{normalizer:?} disagreed on {c:?}");
            }
        }
    }

    /// A run that does not fit must still leave the prefix that does. The
    /// bulk copy is all-or-nothing, so this is the case where the fast path
    /// has to fall back to reproduce the documented semantics.
    #[test]
    fn an_overflowing_run_still_leaves_the_prefix_that_fits() {
        let normalizer = Normalizer::default();
        // Long enough to be one passthrough run rather than a few characters.
        let mut dst = FixedStr::<20>::new();
        let result =
            normalizer.normalize_into("abcdefghijklmnopqrstuvwxyz", Mode::Direct, &mut dst);
        assert_eq!(result, Err(Overflow));
        assert_eq!(dst.as_str(), "abcdefghijklmnopqrst");
    }

    /// The same, for a run that ends mid-string because the *next* character
    /// is transformed — the fallback must not lose the characters the run
    /// already placed.
    #[test]
    fn overflow_after_a_completed_run_keeps_what_landed() {
        let normalizer = Normalizer {
            width: WidthPolicy {
                alnum: Width::Half,
                number: Width::Half,
                symbol: Width::Half,
            },
            punctuation: PunctuationStyle::KutenTouten,
        };
        // "abc" is a run; 、is three bytes and does not fit in the last one.
        let mut dst = FixedStr::<4>::new();
        assert_eq!(
            normalizer.normalize_into("abc、d", Mode::Direct, &mut dst),
            Err(Overflow)
        );
        assert_eq!(dst.as_str(), "abc");
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
