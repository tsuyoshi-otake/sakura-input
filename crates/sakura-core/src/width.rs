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

    /// The family for a whole reading, when that reading is *itself* one
    /// punctuation mark.
    ///
    /// This is the single admission test for the family behaviour, shared by
    /// the converter that appends the four rows and by the dispatcher that
    /// pins the configured one as the default selection. A reading of two or
    /// more characters, or one that merely contains a mark, is an ordinary
    /// sentence and gets neither (Issue #99).
    pub fn family_reading(
        self,
        reading: &str,
    ) -> Option<[PunctuationVariant; PUNCTUATION_FAMILY_LEN]> {
        let mut characters = reading.chars();
        let (Some(mark), None) = (characters.next(), characters.next()) else {
            return None;
        };
        self.family_for(mark)
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

/// One call's mode-resolved answer for the three width-policy channels.
/// Keeping this together makes the long-input dispatch pay for resolving
/// `FollowMode` once, before choosing between the rewrite and scanner paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedWidths(u8);

impl ResolvedWidths {
    const ALNUM: u8 = 1 << 0;
    const NUMBER: u8 = 1 << 1;
    const SYMBOL: u8 = 1 << 2;
    const ALL_FULL: u8 = Self::ALNUM | Self::NUMBER | Self::SYMBOL;

    fn new(alnum: bool, number: bool, symbol: bool) -> Self {
        Self(
            (u8::from(alnum) * Self::ALNUM)
                | (u8::from(number) * Self::NUMBER)
                | (u8::from(symbol) * Self::SYMBOL),
        )
    }

    fn all_full(self) -> bool {
        self.0 == Self::ALL_FULL
    }

    fn alnum(self) -> bool {
        self.0 & Self::ALNUM != 0
    }

    fn number(self) -> bool {
        self.0 & Self::NUMBER != 0
    }

    fn symbol(self) -> bool {
        self.0 & Self::SYMBOL != 0
    }
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
    /// policy's reach. When at least one policy channel can pass ASCII
    /// through, [`simd`] finds each unchanged run and copies it in one move,
    /// leaving the character-at-a-time path for characters that change. When
    /// all three resolved channels require full width, governed ASCII cannot
    /// form a useful run. The function therefore goes straight through
    /// [`Normalizer::normalize_char`] unless the first byte begins a real
    /// passthrough run, notably leading spaces or controls; those inputs retain
    /// the scanner so an unaffected prefix is still copied in bulk. Both paths
    /// are observably identical to mapping that function over `src.chars()`,
    /// which is what the tests assert.
    ///
    /// # What that is worth
    ///
    /// The ignored release benchmark `issue_110_pre_i2_vs_bypass_in_cpu_cycles`
    /// measures the exact former scanner dispatch and this dispatch in the
    /// same compilation unit. It covers the shipped default, all-full ASCII
    /// and identifiers, `FollowMode + FullAlnum`, and the deliberate adverse
    /// case: space/control-heavy text under an all-full policy. That last case
    /// can favour the scanner because those unaffected bytes form one bulk
    /// passthrough run even though every governed ASCII character is rewritten.
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
        let resolved = self.resolved_widths(mode);
        if resolved.all_full() {
            // The all-full table already rejects every symbol, including
            // `[` and `]`, so the bracket-specific clearing performed by
            // `passthrough_lut` would be a no-op. Borrow the existing table
            // directly and use it once for both the gate and scanner entry.
            let lut = simd::passthrough_lut(true, true, true);
            let first = src.as_bytes()[0];
            if simd::admits(lut, first) {
                return self.normalize_runs_after_admitted_first(
                    src,
                    mode,
                    lut,
                    dst,
                    simd::passthrough_len,
                );
            }
            // No useful run starts here. Reuse the scalar policy choke point
            // directly instead of repeatedly discovering empty runs. Its
            // per-character pushes preserve the documented atomic overflow
            // prefix exactly.
            for c in src.chars() {
                dst.push(self.normalize_char(c, mode))?;
            }
            return Ok(());
        }
        self.normalize_runs(src, mode, resolved, dst)
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
        resolved: ResolvedWidths,
        dst: &mut impl TextSink,
    ) -> Result<(), Overflow> {
        let lut = self.passthrough_lut(resolved);
        self.normalize_runs_with_lut(src, mode, &lut, dst, simd::passthrough_len)
    }

    /// The long-string body parameterized by the already-selected run scanner.
    /// Production always passes [`simd::passthrough_len`]. Keeping the scanner
    /// at this narrow boundary lets the SIMD unit benchmark compare concrete
    /// safe-to-call kernels end to end without swapping the process-global
    /// dispatch pointer while tests run in parallel.
    fn normalize_runs_with_lut(
        &self,
        src: &str,
        mode: Mode,
        lut: &simd::Lut,
        dst: &mut impl TextSink,
        mut scan: impl FnMut(&[u8], &simd::Lut) -> usize,
    ) -> Result<(), Overflow> {
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

    /// Enters the run path after the caller has already proved that `src`'s
    /// first byte is admitted by `lut`. This preserves the ordinary scanner
    /// loop for every other caller while avoiding a duplicate table lookup in
    /// the all-full leading-passthrough gate.
    fn normalize_runs_after_admitted_first(
        &self,
        src: &str,
        mode: Mode,
        lut: &simd::Lut,
        dst: &mut impl TextSink,
        mut scan: impl FnMut(&[u8], &simd::Lut) -> usize,
    ) -> Result<(), Overflow> {
        debug_assert!(simd::admits(lut, src.as_bytes()[0]));
        // Sound because admitted runs contain ASCII bytes only, so the split
        // remains on a character boundary.
        let (run, tail) = src.split_at(scan(src.as_bytes(), lut));
        if dst.push_str(run).is_err() {
            for c in run.chars() {
                dst.push(c)?;
            }
        }
        if tail.is_empty() {
            return Ok(());
        }
        self.normalize_runs_with_lut(tail, mode, lut, dst, scan)
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
        let resolved = self.resolved_widths(mode);
        if resolved.all_full() {
            let lut = simd::passthrough_lut(true, true, true);
            if simd::admits(lut, src.as_bytes()[0]) {
                return self.normalize_runs_after_admitted_first(
                    src,
                    mode,
                    lut,
                    dst,
                    |bytes, lut| {
                        if bytes.len() < simd::MIN_VECTOR_BYTES {
                            return simd::passthrough_len(bytes, lut);
                        }
                        // SAFETY: upheld by this test-only API's caller.
                        unsafe { scan(bytes, lut) }
                    },
                );
            }
            for c in src.chars() {
                dst.push(self.normalize_char(c, mode))?;
            }
            return Ok(());
        }
        let lut = self.passthrough_lut(resolved);
        self.normalize_runs_with_lut(src, mode, &lut, dst, |bytes, lut| {
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
    fn resolved_widths(&self, mode: Mode) -> ResolvedWidths {
        ResolvedWidths::new(
            wants_full(self.width.alnum, mode),
            wants_full(self.width.number, mode),
            wants_full(self.width.symbol, mode),
        )
    }

    /// The set of single-byte characters these already-resolved decisions
    /// leave alone.
    fn passthrough_lut(&self, resolved: ResolvedWidths) -> simd::Lut {
        let mut lut =
            *simd::passthrough_lut(resolved.alnum(), resolved.number(), resolved.symbol());
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
#[path = "width_tests.rs"]
mod tests;
