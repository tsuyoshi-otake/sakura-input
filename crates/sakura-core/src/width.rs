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
//! Unicode code points and the three settings that govern them.

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

/// Which glyph the punctuation choke point emits for the comma role
/// (読点), independently of the period role.
///
/// Three choices, not two: `、` and `，` are the two conventions Japanese
/// prose picks between, and ASCII `,` is what a manuscript typeset from
/// plain text — LaTeX, Markdown, a paper written in an editor — wants in
/// running text next to full-width kana.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommaMark {
    /// `、` — the Japanese reading comma.
    #[default]
    Touten,
    /// `，` — full-width Western comma, the JIS / 学術論文 convention.
    FullWidth,
    /// `,` — ASCII comma.
    HalfWidth,
}

impl CommaMark {
    /// Stable order used by settings controls and persistence tests.
    pub const ALL: [Self; 3] = [Self::Touten, Self::FullWidth, Self::HalfWidth];

    /// The character this mark puts in the document.
    pub const fn glyph(self) -> char {
        match self {
            Self::Touten => '\u{3001}',    // 、
            Self::FullWidth => '\u{FF0C}', // ，
            Self::HalfWidth => ',',
        }
    }
}

/// Which glyph the punctuation choke point emits for the period role
/// (句点), independently of the comma role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeriodMark {
    /// `。` — the Japanese full stop.
    #[default]
    Kuten,
    /// `．` — full-width Western period, the JIS / 学術論文 convention.
    FullWidth,
    /// `.` — ASCII period.
    HalfWidth,
}

impl PeriodMark {
    /// Stable order used by settings controls and persistence tests.
    pub const ALL: [Self; 3] = [Self::Kuten, Self::FullWidth, Self::HalfWidth];

    /// The character this mark puts in the document.
    pub const fn glyph(self) -> char {
        match self {
            Self::Kuten => '\u{3002}',     // 。
            Self::FullWidth => '\u{FF0E}', // ．
            Self::HalfWidth => '.',
        }
    }
}

/// Which pair of comma-role/period-role characters the punctuation choke
/// point emits (DESIGN §2 "Punctuation style").
///
/// The two roles are held separately because they are chosen separately:
/// the settings screen offers one control per role, and each of the nine
/// combinations names a convention somebody writes in — `、。` for ordinary
/// prose, `，．` for a JIS-style paper, `，。` for 公用文, `,.` for a
/// manuscript that will be typeset from plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PunctuationStyle {
    pub comma: CommaMark,
    pub period: PeriodMark,
}

impl PunctuationStyle {
    /// `、` + `。` — traditional Japanese prose, and the default: it is the
    /// unsurprising choice, and every other combination is opt-in.
    pub const KUTEN_TOUTEN: Self = Self::new(CommaMark::Touten, PeriodMark::Kuten);
    /// `，` + `．` — full-width Western punctuation, common in mixed EN/JP
    /// technical writing and required by many 学術論文 templates.
    pub const COMMA_PERIOD: Self = Self::new(CommaMark::FullWidth, PeriodMark::FullWidth);
    /// `、` + `．` — the mixed convention some engineers prefer: Japanese
    /// comma, Western period.
    pub const MIXED: Self = Self::new(CommaMark::Touten, PeriodMark::FullWidth);
    /// `，` + `。` — Western comma with Japanese period (公用文).
    pub const COMMA_KUTEN: Self = Self::new(CommaMark::FullWidth, PeriodMark::Kuten);
    /// `,` + `.` — ASCII throughout, for prose that will be typeset from
    /// plain text.
    pub const ASCII: Self = Self::new(CommaMark::HalfWidth, PeriodMark::HalfWidth);

    /// Stable order used by settings controls and persistence tests: comma
    /// major, period minor, each role in its own `ALL` order.
    pub const ALL: [Self; 9] = [
        Self::new(CommaMark::Touten, PeriodMark::Kuten),
        Self::new(CommaMark::Touten, PeriodMark::FullWidth),
        Self::new(CommaMark::Touten, PeriodMark::HalfWidth),
        Self::new(CommaMark::FullWidth, PeriodMark::Kuten),
        Self::new(CommaMark::FullWidth, PeriodMark::FullWidth),
        Self::new(CommaMark::FullWidth, PeriodMark::HalfWidth),
        Self::new(CommaMark::HalfWidth, PeriodMark::Kuten),
        Self::new(CommaMark::HalfWidth, PeriodMark::FullWidth),
        Self::new(CommaMark::HalfWidth, PeriodMark::HalfWidth),
    ];

    /// Builds a style from the independent comma and period choices.
    pub const fn new(comma: CommaMark, period: PeriodMark) -> Self {
        Self { comma, period }
    }

    /// The whole punctuation family `c` belongs to, ordered with this style's
    /// own glyph first, or `None` when `c` is in neither family.
    ///
    /// The setting decides which mark is offered *first*; it does not decide
    /// which marks exist. Somebody who set `，` still needs to reach `、` for
    /// one quoted sentence without opening the settings window, which is what
    /// this ordering gives the converter (Issue #99).
    ///
    /// The returned order always holds all four members exactly once.
    pub fn family_for(self, c: char) -> Option<[PunctuationVariant; PUNCTUATION_FAMILY_LEN]> {
        let (family, preferred) = if COMMA_FAMILY.iter().any(|variant| variant.glyph == c) {
            (COMMA_FAMILY, self.comma.glyph())
        } else if PERIOD_FAMILY.iter().any(|variant| variant.glyph == c) {
            (PERIOD_FAMILY, self.period.glyph())
        } else {
            return None;
        };
        // Two passes rather than search-and-swap, so the result holds all four
        // members in every case -- including a `preferred` outside the family,
        // which `CommaMark`/`PeriodMark` cannot produce today but which must
        // not silently drop a member if that ever changes.
        let mut ordered = [family[0]; PUNCTUATION_FAMILY_LEN];
        let mut placed = 0;
        for variant in family {
            if variant.glyph == preferred {
                ordered[placed] = variant;
                placed += 1;
            }
        }
        for variant in family {
            if variant.glyph != preferred {
                ordered[placed] = variant;
                placed += 1;
            }
        }
        debug_assert_eq!(placed, PUNCTUATION_FAMILY_LEN);
        Some(ordered)
    }
}

/// How many glyphs one punctuation family holds.
pub const PUNCTUATION_FAMILY_LEN: usize = 4;

/// One offerable punctuation glyph and the annotation naming it.
///
/// The annotation follows the same bare-noun shape as
/// [`crate::numerals::NumericStyle::annotation`] (算用数字 / 全角数字 /
/// 漢数字) rather than a bracketed width tag, so a candidate list reads
/// consistently whichever rewriter produced the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunctuationVariant {
    pub glyph: char,
    pub annotation: &'static str,
}

/// The comma role's four offerable glyphs, in the order they follow the
/// configured one.
///
/// `､` (U+FF64) is here but deliberately absent from [`punct_role`]: it is
/// offerable as a candidate without being a glyph the choke point rewrites
/// into or out of. It also sits outside `to_half`/`to_full`'s U+FF01..=U+FF5E
/// arithmetic range, so the width policy leaves it alone as well.
pub const COMMA_FAMILY: [PunctuationVariant; PUNCTUATION_FAMILY_LEN] = [
    PunctuationVariant {
        glyph: '\u{3001}', // 、
        annotation: "全角読点",
    },
    PunctuationVariant {
        glyph: '\u{FF64}', // ､
        annotation: "半角読点",
    },
    PunctuationVariant {
        glyph: '\u{FF0C}', // ，
        annotation: "全角コンマ",
    },
    PunctuationVariant {
        glyph: ',',
        annotation: "半角コンマ",
    },
];

/// The period role's four offerable glyphs. See [`COMMA_FAMILY`] for why
/// `｡` (U+FF61) appears here but not in [`punct_role`].
pub const PERIOD_FAMILY: [PunctuationVariant; PUNCTUATION_FAMILY_LEN] = [
    PunctuationVariant {
        glyph: '\u{3002}', // 。
        annotation: "全角句点",
    },
    PunctuationVariant {
        glyph: '\u{FF61}', // ｡
        annotation: "半角句点",
    },
    PunctuationVariant {
        glyph: '\u{FF0E}', // ．
        annotation: "全角ピリオド",
    },
    PunctuationVariant {
        glyph: '.',
        annotation: "半角ピリオド",
    },
];

/// Which bracket pair the width-policy choke point emits.
///
/// Brackets are deliberately independent from the generic symbol-width
/// channel.  Japanese text normally uses corner brackets (`「」`), while
/// technical prose often wants full-width square brackets (`［］`).  ASCII,
/// full-width square, and Japanese corner/double-corner source forms all map
/// to the selected pair so a pasted candidate cannot bypass the setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BracketStyle {
    #[default]
    Corner,
    Square,
}

impl BracketStyle {
    /// Stable order used by settings controls and persistence tests.
    pub const ALL: [Self; 2] = [Self::Corner, Self::Square];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Corner => "corner",
            Self::Square => "square",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "corner" => Some(Self::Corner),
            "square" => Some(Self::Square),
            _ => None,
        }
    }
}

/// The width-policy choke point itself: a pure `(text, mode) -> text`
/// transform parameterized by the three DESIGN §2 settings. Stateless and
/// `Copy` — callers are expected to hold one of these per session (or one
/// globally, if width policy is not per-app) and reuse it for every output
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Normalizer {
    pub width: WidthPolicy,
    pub punctuation: PunctuationStyle,
    pub brackets: BracketStyle,
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
            if simd::admits(&lut, first) {
                // Sound because a run only ever covers ASCII bytes, so both
                // ends are character boundaries (see `simd`'s module docs).
                let (run, tail) = rest.split_at(scan(rest.as_bytes(), &lut));
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
    fn passthrough_lut(&self, mode: Mode) -> simd::Lut {
        let mut lut = *simd::passthrough_lut(
            wants_full(self.width.alnum, mode),
            wants_full(self.width.number, mode),
            wants_full(self.width.symbol, mode),
        );
        // `[`/`]` are otherwise admitted by the ASCII symbol LUT.  They are
        // owned by the bracket setting, so stop SIMD runs at both bytes and
        // let `normalize_char` map them just like the non-ASCII pairs.
        lut[0x0B] &= !(1 << 5); // '[' (0x5B)
        lut[0x0D] &= !(1 << 5); // ']' (0x5D)
        lut
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
        if let Some(role) = bracket_role(c) {
            return map_bracket(role, self.brackets);
        }
        // ASCII space is a word separator, not a `symbol` width citizen.
        // Idle SpaceWidth is the only path that may emit U+3000. Widening
        // ' ' here is what turned typed English like "Claude Code" into
        // "Claude　Code" when a conversion surface was normalized.
        if c == ' ' {
            return ' ';
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum BracketRole {
    Open,
    Close,
}

fn bracket_role(c: char) -> Option<BracketRole> {
    match c {
        '[' | '［' | '「' | '『' => Some(BracketRole::Open),
        ']' | '］' | '」' | '』' => Some(BracketRole::Close),
        _ => None,
    }
}

fn map_bracket(role: BracketRole, style: BracketStyle) -> char {
    match (style, role) {
        (BracketStyle::Corner, BracketRole::Open) => '「',
        (BracketStyle::Corner, BracketRole::Close) => '」',
        (BracketStyle::Square, BracketRole::Open) => '［',
        (BracketStyle::Square, BracketRole::Close) => '］',
    }
}

/// Identifies the four code points the punctuation choke point owns
/// (DESIGN §2). Returns `None` for every other character, including ASCII
/// `,`/`.`, which are ordinary symbols governed by `width.symbol` instead.
///
/// This stays a four-code-point set even though [`CommaMark::HalfWidth`] and
/// [`PeriodMark::HalfWidth`] *emit* ASCII `,`/`.`: the channel writes half-
/// width marks without claiming them back. Claiming them would mean a `.`
/// typed in direct input turned into `。` under the default style, which is
/// the opposite of what a `,` in `foo(a, b)` is asking for.
fn punct_role(c: char) -> Option<PunctRole> {
    match c {
        '\u{3001}' | '\u{FF0C}' => Some(PunctRole::Comma), // 、 ，
        '\u{3002}' | '\u{FF0E}' => Some(PunctRole::Period), // 。 ．
        _ => None,
    }
}

/// Picks the configured glyph for `role` under `style`.
fn map_punct(role: PunctRole, style: PunctuationStyle) -> char {
    match role {
        PunctRole::Comma => style.comma.glyph(),
        PunctRole::Period => style.period.glyph(),
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
            brackets: BracketStyle::default(),
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
    fn ascii_printable_half_to_full_to_half_is_identity_except_owned_punctuation_and_brackets() {
        let full = Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Full,
                symbol: Width::Full,
            },
            punctuation: PunctuationStyle::default(),
            brackets: BracketStyle::default(),
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
            if matches!(c, ',' | '.' | '[' | ']') {
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
            brackets: BracketStyle::default(),
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
    fn ascii_space_is_not_widened_by_the_symbol_channel() {
        let full = Normalizer {
            width: WidthPolicy {
                alnum: Width::Half,
                number: Width::Half,
                symbol: Width::Full,
            },
            punctuation: PunctuationStyle::default(),
            brackets: BracketStyle::default(),
        };
        let half = Normalizer::default();
        assert_eq!(full.normalize_char(' ', Mode::Direct), ' ');
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
            brackets: BracketStyle::default(),
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
            brackets: BracketStyle::default(),
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
        // KUTEN_TOUTEN, not just the forward 、-> 、 identity. Driven off
        // `ALL` rather than a hand-written table so a tenth combination
        // cannot be added without being covered here.
        let sources = ['\u{3001}', '\u{3002}', '\u{FF0C}', '\u{FF0E}'];
        for style in PunctuationStyle::ALL {
            let expected = [
                style.comma.glyph(),
                style.period.glyph(),
                style.comma.glyph(),
                style.period.glyph(),
            ];
            let normalizer = Normalizer {
                width: WidthPolicy::default(),
                punctuation: style,
                brackets: BracketStyle::default(),
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
    fn half_width_punctuation_survives_every_symbol_width_and_mode() {
        // The whole point of the half-width marks is that they reach the
        // document as ASCII. `symbol = Full` widens every other ASCII
        // symbol, so if it ever got a say here it would widen `,` back to
        // ， and undo the setting — the same fight rule 4 already settles
        // for the full-width marks, now coming from the other side.
        for symbol in [Width::Half, Width::Full, Width::FollowMode] {
            let normalizer = Normalizer {
                width: WidthPolicy {
                    alnum: Width::Half,
                    number: Width::Half,
                    symbol,
                },
                punctuation: PunctuationStyle::ASCII,
                brackets: BracketStyle::default(),
            };
            for mode in Mode::ALL {
                for src in ['\u{3001}', '\u{FF0C}'] {
                    assert_eq!(
                        normalizer.normalize_char(src, mode),
                        ',',
                        "symbol {symbol:?} mode {mode:?} src {src:?}"
                    );
                }
                for src in ['\u{3002}', '\u{FF0E}'] {
                    assert_eq!(
                        normalizer.normalize_char(src, mode),
                        '.',
                        "symbol {symbol:?} mode {mode:?} src {src:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn half_width_punctuation_is_emitted_but_never_reclaimed() {
        // Deliberately one-way. The punctuation channel writes ASCII `,`/`.`
        // when asked to, but `punct_role` still owns exactly four code
        // points, so ASCII `,`/`.` arriving as *input* stay ordinary symbols
        // governed by `width.symbol`. Were they claimed instead, a `.` typed
        // in direct input would come back as 。 under the default style, and
        // the `,` in `foo(a, b)` would stop being a comma.
        let ascii = Normalizer {
            width: WidthPolicy::default(),
            punctuation: PunctuationStyle::ASCII,
            brackets: BracketStyle::default(),
        };
        assert_eq!(ascii.normalize_char(',', Mode::Direct), ',');
        assert_eq!(ascii.normalize_char('.', Mode::Direct), '.');

        // Under the default style the same ASCII input is still untouched by
        // punctuation: `width.symbol` alone decides its width.
        let default_half = Normalizer::default();
        assert_eq!(default_half.normalize_char(',', Mode::Direct), ',');
        assert_eq!(default_half.normalize_char('.', Mode::Direct), '.');
        let default_full = Normalizer {
            width: WidthPolicy {
                alnum: Width::Half,
                number: Width::Half,
                symbol: Width::Full,
            },
            ..Normalizer::default()
        };
        assert_eq!(default_full.normalize_char(',', Mode::Direct), '\u{FF0C}');
        assert_eq!(default_full.normalize_char('.', Mode::Direct), '\u{FF0E}');
    }

    #[test]
    fn half_width_punctuation_flows_through_normalize_into() {
        // `normalize_into` copies unchanged runs wholesale and only falls to
        // `normalize_char` for the rest. All four owned marks are
        // three-byte, so none of them can hide inside a single-byte
        // passthrough run — this pins that the run scanner really does hand
        // them over, and that a 3-byte -> 1-byte replacement lands intact
        // in the middle of ASCII that the scanner did copy wholesale.
        let normalizer = Normalizer {
            width: WidthPolicy::default(),
            punctuation: PunctuationStyle::ASCII,
            brackets: BracketStyle::default(),
        };
        let mut out = String::new();
        normalizer
            .normalize_into(
                "docker compose up、これで起動する。ログは journalctl で読む。",
                Mode::Hiragana,
                &mut out,
            )
            .expect("fits in a growable String");
        assert_eq!(
            out,
            "docker compose up,これで起動する.ログは journalctl で読む."
        );
    }

    #[test]
    fn bracket_style_normalizes_all_supported_source_pairs() {
        let sources = ['[', ']', '［', '］', '「', '」', '『', '』'];
        for style in BracketStyle::ALL {
            let expected = match style {
                BracketStyle::Corner => ['「', '」', '「', '」', '「', '」', '「', '」'],
                BracketStyle::Square => ['［', '］', '［', '］', '［', '］', '［', '］'],
            };
            let normalizer = Normalizer {
                width: WidthPolicy::default(),
                punctuation: PunctuationStyle::default(),
                brackets: style,
            };
            for (source, want) in sources.into_iter().zip(expected) {
                assert_eq!(normalizer.normalize_char(source, Mode::Direct), want);
            }

            // Keep the SIMD fast path honest too: a long ASCII run must stop
            // at the bracket bytes so normalize_char owns them rather than
            // copying them through unchanged.
            let source = "[x]".repeat(16);
            let mut rendered = String::new();
            normalizer
                .normalize_into(&source, Mode::Direct, &mut rendered)
                .expect("growable output accepts the long bracket sample");
            let (open, close) = match style {
                BracketStyle::Corner => ('\u{300c}', '\u{300d}'),
                BracketStyle::Square => ('\u{ff3b}', '\u{ff3d}'),
            };
            assert!(rendered.contains(open));
            assert!(rendered.contains(close));
            assert!(!rendered.contains('['));
            assert!(!rendered.contains(']'));
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
            punctuation: PunctuationStyle::COMMA_PERIOD,
            brackets: BracketStyle::default(),
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
            punctuation: PunctuationStyle::COMMA_PERIOD,
            brackets: BracketStyle::default(),
        };
        for c in ['あ', 'ア', '漢', '𠮷', '🍣'] {
            assert_eq!(normalizer.normalize_char(c, Mode::Direct), c);
        }
    }

    /// Every normalizer worth building, so the agreement tests below cover
    /// all eight passthrough tables rather than the default one.
    fn every_normalizer() -> Vec<Normalizer> {
        let widths = [Width::Half, Width::Full, Width::FollowMode];
        let styles = PunctuationStyle::ALL;
        let bracket_styles = BracketStyle::ALL;
        let mut all = Vec::new();
        for alnum in widths {
            for number in widths {
                for symbol in widths {
                    for punctuation in styles {
                        for brackets in bracket_styles {
                            all.push(Normalizer {
                                width: WidthPolicy {
                                    alnum,
                                    number,
                                    symbol,
                                },
                                punctuation,
                                brackets,
                            });
                        }
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
                punctuation: PunctuationStyle::COMMA_PERIOD,
                brackets: BracketStyle::default(),
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
            punctuation: PunctuationStyle::KUTEN_TOUTEN,
            brackets: BracketStyle::default(),
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
        assert_eq!(PunctuationStyle::default(), PunctuationStyle::KUTEN_TOUTEN);
        assert_eq!(BracketStyle::default(), BracketStyle::Corner);
        assert_eq!(
            Normalizer::default(),
            Normalizer {
                width: WidthPolicy::default(),
                punctuation: PunctuationStyle::default(),
                brackets: BracketStyle::default(),
            }
        );
    }

    #[test]
    fn punctuation_parts_cover_all_independent_combinations() {
        // `ALL` is what the settings combos and the persistence round-trips
        // iterate, so it has to be the exact cross product of the two roles
        // — no combination missing, none listed twice.
        assert_eq!(PunctuationStyle::ALL.len(), 9);
        let mut seen = Vec::new();
        for comma in CommaMark::ALL {
            for period in PeriodMark::ALL {
                let style = PunctuationStyle::new(comma, period);
                assert!(
                    PunctuationStyle::ALL.contains(&style),
                    "ALL is missing {style:?}"
                );
                assert_eq!(style.comma, comma);
                assert_eq!(style.period, period);
                seen.push(style);
            }
        }
        for style in PunctuationStyle::ALL {
            assert_eq!(
                seen.iter().filter(|candidate| **candidate == style).count(),
                1,
                "{style:?} is not listed exactly once"
            );
        }

        // The named conventions the settings screen and the config format
        // talk about, spelled out so reordering either role enum cannot
        // quietly repoint one of them.
        assert_eq!(PunctuationStyle::KUTEN_TOUTEN.comma.glyph(), '\u{3001}');
        assert_eq!(PunctuationStyle::KUTEN_TOUTEN.period.glyph(), '\u{3002}');
        assert_eq!(PunctuationStyle::COMMA_PERIOD.comma.glyph(), '\u{FF0C}');
        assert_eq!(PunctuationStyle::COMMA_PERIOD.period.glyph(), '\u{FF0E}');
        assert_eq!(PunctuationStyle::MIXED.comma.glyph(), '\u{3001}');
        assert_eq!(PunctuationStyle::MIXED.period.glyph(), '\u{FF0E}');
        assert_eq!(PunctuationStyle::COMMA_KUTEN.comma.glyph(), '\u{FF0C}');
        assert_eq!(PunctuationStyle::COMMA_KUTEN.period.glyph(), '\u{3002}');
        assert_eq!(PunctuationStyle::ASCII.comma.glyph(), ',');
        assert_eq!(PunctuationStyle::ASCII.period.glyph(), '.');
    }

    #[test]
    fn every_style_orders_its_own_glyph_first_and_keeps_the_whole_family() {
        // The setting decides the first row, not which rows exist. Both
        // halves are exhaustive over the nine styles because a single style
        // getting this wrong is invisible in any other test.
        for style in PunctuationStyle::ALL {
            for (family, preferred) in [
                (COMMA_FAMILY, style.comma.glyph()),
                (PERIOD_FAMILY, style.period.glyph()),
            ] {
                for member in family {
                    let ordered = style
                        .family_for(member.glyph)
                        .unwrap_or_else(|| panic!("{:?} has no family", member.glyph));
                    assert_eq!(
                        ordered[0].glyph, preferred,
                        "{style:?} must offer its own glyph first for {:?}",
                        member.glyph
                    );
                    for expected in family {
                        assert_eq!(
                            ordered
                                .iter()
                                .filter(|variant| variant.glyph == expected.glyph)
                                .count(),
                            1,
                            "{style:?}: {:?} is not offered exactly once",
                            expected.glyph
                        );
                    }
                    // Everything after the first row keeps the table's own
                    // order, so the list a reader learns does not reshuffle
                    // when they change the setting.
                    let tail: Vec<char> = ordered[1..].iter().map(|v| v.glyph).collect();
                    let expected_tail: Vec<char> = family
                        .iter()
                        .map(|v| v.glyph)
                        .filter(|glyph| *glyph != preferred)
                        .collect();
                    assert_eq!(tail, expected_tail, "{style:?}");
                }
            }
        }
    }

    #[test]
    fn punctuation_families_are_disjoint_and_carry_distinct_annotations() {
        let mut glyphs = Vec::new();
        let mut annotations = Vec::new();
        for variant in COMMA_FAMILY.into_iter().chain(PERIOD_FAMILY) {
            assert!(
                !glyphs.contains(&variant.glyph),
                "{:?} appears in both families",
                variant.glyph
            );
            assert!(
                !annotations.contains(&variant.annotation),
                "`{}` annotates two glyphs",
                variant.annotation
            );
            glyphs.push(variant.glyph);
            annotations.push(variant.annotation);
        }
        assert_eq!(glyphs.len(), PUNCTUATION_FAMILY_LEN * 2);
        // A character in neither family has no family, however punctuation-
        // like it looks. `・` and `！` are the near misses worth pinning.
        for outsider in ['a', 'あ', '・', '！', '!', '｢'] {
            assert!(
                PunctuationStyle::default().family_for(outsider).is_none(),
                "{outsider:?}"
            );
        }
    }

    #[test]
    fn half_width_kana_marks_are_offerable_without_being_claimed() {
        // `､` and `｡` are candidates the converter can offer but glyphs the
        // choke point does not own: `punct_role` ignores them and they sit
        // outside the U+FF01..=U+FF5E width arithmetic. That is what lets a
        // reader pick one and keep it under any setting, and it is why the
        // family table can list them without widening rule 4's four-code-
        // point set.
        assert!(punct_role('\u{FF64}').is_none());
        assert!(punct_role('\u{FF61}').is_none());
        for style in PunctuationStyle::ALL {
            for symbol in [Width::Half, Width::Full] {
                let normalizer = Normalizer {
                    width: WidthPolicy {
                        alnum: symbol,
                        number: symbol,
                        symbol,
                    },
                    punctuation: style,
                    brackets: BracketStyle::default(),
                };
                for mode in [Mode::Direct, Mode::Hiragana] {
                    assert_eq!(normalizer.normalize_char('\u{FF64}', mode), '\u{FF64}');
                    assert_eq!(normalizer.normalize_char('\u{FF61}', mode), '\u{FF61}');
                }
            }
        }
    }
}
