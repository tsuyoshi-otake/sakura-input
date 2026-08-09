//! The romaji → kana input FSM (DESIGN 5.1).
//!
//! The table is data, not code: `data/romaji.toml` ships as the default and a
//! user can replace it, which is also how AZIK and other custom layouts are
//! supported without a line of Rust.
//!
//! DESIGN calls the compiled form a trie. It is stored here as a table sorted
//! by romaji sequence, which answers the only two questions the FSM ever asks
//! — "is this an entry?" and "is this the start of a longer one?" — in a
//! binary search each, out of one contiguous allocation instead of a few
//! hundred linked nodes. For a table this size that is both smaller and
//! faster, and the entries sort into exactly the order a trie would walk.
//!
//! # Matching
//!
//! Longest match with backtracking, not greedy matching. The distinction is
//! the whole reason this is an FSM and not a lookup: `n` is a complete entry
//! (ん) *and* the start of `na`, `ni`, `nn` and `n'`, so it has to wait to see
//! what comes next, and then be able to give up on the longer reading. Typing
//! `nk` produces ん followed by a pending `k`; typing `na` produces な.
//!
//! # Allocation
//!
//! [`Table`] allocates once, when it is built. Nothing on the input path does:
//! pending romaji lives in a fixed stack buffer and kana is written into a
//! sink the caller owns (DESIGN 5.7). `tests/zero_alloc.rs` asserts this
//! against a counting allocator rather than trusting the claim.

use sakura_proto::{FixedStr, Overflow};

use crate::config::{self, Document, ParseError, Value};
use crate::text::TextSink;

/// The default table, compiled into the binary.
///
/// Embedded rather than read from disk so the IME can always fall back to a
/// working table: a user whose edited file fails to parse gets this one and a
/// diagnostic, not a keyboard that types nothing.
pub const DEFAULT_TABLE: &str = include_str!("../../../data/romaji.toml");

/// The section of the config document the table is read from.
pub const TABLE_SECTION: &str = "kana";

/// The longest romaji sequence an entry may use.
///
/// Also the size of the pending buffer, which is sound because pending romaji
/// is always a *proper* prefix of some entry and therefore strictly shorter
/// than the longest one.
pub const MAX_SEQUENCE: usize = 8;

/// Pending romaji: ASCII, bounded, and on the stack.
type Sequence = FixedStr<MAX_SEQUENCE>;

/// One compiled mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledEntry {
    sequence: String,
    output: String,
    /// Romaji fed back to the FSM after `output` is emitted — the `t` of
    /// `tt` → っ. Empty for most entries.
    carry: String,
}

/// A compiled romaji table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Sorted by `sequence`, which puts every extension of a sequence
    /// immediately after it and makes [`Table::extends`] a single lookup.
    entries: Vec<CompiledEntry>,
}

/// Pending input state. One per session; cheap to clone and copy around.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    pending: Sequence,
    /// How many of `pending`'s leading bytes are a *carry*: romaji already
    /// used once to resolve an earlier, already-emitted kana, and fed back
    /// into `pending` rather than discarded (the second `t` of `tt` -> っ,
    /// carried forward so `tsu` is still reachable). Those bytes do not
    /// have a keystroke of their own still ahead of that kana -- they
    /// *are* part of its source, reused. A caller mapping `pending` back
    /// onto raw keystrokes (issue #16 findings B/C) needs this and cannot
    /// reconstruct it later from `pending`'s text alone: a carry is table
    /// data (`CompiledEntry::carry`), not a copy of the raw characters it
    /// stands for, so nothing guarantees their content still matches once
    /// case-folding or a custom table is in play. Always `<= pending.len()`;
    /// read through [`Input::carry_overlap`], which enforces that bound as
    /// `pending` shrinks under [`Input::backspace`] instead of requiring
    /// every mutation site to keep the two in lock-step by hand.
    carry_overlap: usize,
}

impl Input {
    /// A fresh, empty input state.
    pub fn new() -> Self {
        Self::default()
    }

    /// The romaji typed so far that has not yet resolved to kana.
    ///
    /// Hosts display this after the committed kana — it is the `ｎ` visible
    /// after typing `n` and before deciding between `な` and `ん`.
    pub fn pending(&self) -> &str {
        self.pending.as_str()
    }

    /// How many of [`Input::pending`]'s leading bytes are a carry rather
    /// than a fresh keystroke of their own — see the field doc on
    /// `carry_overlap`.
    pub fn carry_overlap(&self) -> usize {
        self.carry_overlap.min(self.pending.len())
    }

    /// `true` when nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Discards pending romaji without emitting anything.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.carry_overlap = 0;
    }

    /// Removes the last pending romaji character.
    ///
    /// Returns `false` when there was nothing pending, which is the caller's
    /// signal that the backspace belongs to the kana already emitted rather
    /// than to the FSM.
    pub fn backspace(&mut self) -> bool {
        let removed = self.pending.pop_char().is_some();
        if removed {
            self.carry_overlap = self.carry_overlap.min(self.pending.len());
        }
        removed
    }
}

/// Why a table could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableError {
    /// The offending romaji sequence, when the fault belongs to one entry.
    pub sequence: Option<String>,
    pub kind: TableErrorKind,
}

/// The specific fault. Every variant names something a human can go and fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableErrorKind {
    /// The file itself did not parse.
    Config(ParseError),
    /// No `[kana]` section.
    MissingSection,
    /// A `[kana]` section with no entries. A table that maps nothing would
    /// leave the user unable to type.
    EmptyTable,
    /// A sequence with a non-ASCII character. Sequences are what the user
    /// types on a physical keyboard, and the FSM's buffer is ASCII.
    NonAsciiSequence,
    /// A sequence containing a capital letter. Lookup folds case, so such an
    /// entry could never be reached and is always a mistake.
    UppercaseSequence,
    /// A sequence longer than [`MAX_SEQUENCE`].
    SequenceTooLong,
    /// A value that is neither a string nor a one- or two-element list.
    MalformedValue,
    /// An entry that emits nothing and carries nothing, which would silently
    /// swallow the keystrokes that reach it.
    EmptyEntry,
    /// A carry with a non-ASCII character.
    NonAsciiCarry,
    /// A carry containing a capital letter.
    UppercaseCarry,
    /// A carry at least as long as the sequence that produced it.
    ///
    /// This is the invariant that makes the FSM terminate: every resolution
    /// step replaces a sequence with something strictly shorter, so the loop
    /// cannot run forever no matter what a user puts in their table.
    CarryNotShorter,
}

impl core::fmt::Display for TableError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(sequence) = &self.sequence {
            write!(f, "romaji entry {sequence:?}: ")?;
        }
        match &self.kind {
            TableErrorKind::Config(error) => write!(f, "{error}"),
            TableErrorKind::MissingSection => {
                write!(f, "no [{TABLE_SECTION}] section")
            }
            TableErrorKind::EmptyTable => write!(f, "the table is empty"),
            TableErrorKind::NonAsciiSequence => write!(f, "sequence must be ASCII"),
            TableErrorKind::UppercaseSequence => {
                write!(f, "sequence must be lowercase; lookup folds case")
            }
            TableErrorKind::SequenceTooLong => {
                write!(f, "sequence is longer than {MAX_SEQUENCE} characters")
            }
            TableErrorKind::MalformedValue => {
                write!(f, "value must be \"kana\" or [\"kana\", \"carry\"]")
            }
            TableErrorKind::EmptyEntry => write!(f, "entry emits nothing and carries nothing"),
            TableErrorKind::NonAsciiCarry => write!(f, "carry must be ASCII"),
            TableErrorKind::UppercaseCarry => write!(f, "carry must be lowercase"),
            TableErrorKind::CarryNotShorter => {
                write!(f, "carry must be shorter than the sequence it comes from")
            }
        }
    }
}

impl std::error::Error for TableError {}

impl From<ParseError> for TableError {
    fn from(error: ParseError) -> Self {
        TableError {
            sequence: None,
            kind: TableErrorKind::Config(error),
        }
    }
}

/// What consumes the front of a sequence during resolution.
enum Step {
    /// The entry at `index` matches the first `consumed` bytes.
    Entry { index: usize, consumed: usize },
    /// Nothing matches; the leading character passes through unchanged.
    Raw,
}

impl Table {
    /// Compiles [`DEFAULT_TABLE`].
    pub fn builtin() -> Result<Self, TableError> {
        Self::parse(DEFAULT_TABLE)
    }

    /// Parses config source and compiles the table in it.
    pub fn parse(source: &str) -> Result<Self, TableError> {
        let document = config::parse(source)?;
        Self::from_document(&document)
    }

    /// Compiles the `[kana]` section of an already-parsed document.
    pub fn from_document(document: &Document) -> Result<Self, TableError> {
        let Some(entries) = document.section(TABLE_SECTION) else {
            return Err(TableError {
                sequence: None,
                kind: TableErrorKind::MissingSection,
            });
        };

        let mut compiled = Vec::with_capacity(entries.len());
        for entry in entries {
            compiled.push(compile_entry(&entry.key, &entry.value)?);
        }

        if compiled.is_empty() {
            return Err(TableError {
                sequence: None,
                kind: TableErrorKind::EmptyTable,
            });
        }

        // The config parser rejects duplicate keys, so this cannot merge two
        // entries into one position; it only establishes the prefix ordering
        // `extends` relies on.
        compiled.sort_by(|a, b| a.sequence.cmp(&b.sequence));
        Ok(Table { entries: compiled })
    }

    /// The number of mappings.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the table has no mappings. Never true for a compiled table —
    /// [`TableErrorKind::EmptyTable`] rejects that — but `clippy` asks for it
    /// wherever there is a `len`.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Feeds one typed character, writing any resolved kana into `out`.
    ///
    /// The character is lowercased for lookup. Whether a capital letter
    /// reaches the FSM at all is a mode decision one layer up: this function
    /// only says that if it does, it means the same as its lowercase form.
    ///
    /// On [`Overflow`] the sink is full and some output may already have been
    /// written. The FSM state is stored back anyway, so a retry continues from
    /// where it stopped instead of re-emitting text the sink already took.
    pub fn feed(
        &self,
        state: &mut Input,
        key: char,
        out: &mut impl TextSink,
    ) -> Result<(), Overflow> {
        let key = key.to_ascii_lowercase();

        if !key.is_ascii() {
            // Sequences are ASCII, so this character can neither complete nor
            // extend one. Resolving first keeps the output in typing order.
            self.flush(state, out)?;
            return out.push(key);
        }

        let mut candidate = state.pending.clone();
        let mut overlap = state.carry_overlap;
        if candidate.push(key).is_err() {
            // Unreachable while pending is a proper prefix of some entry and
            // every entry fits in MAX_SEQUENCE, both of which are enforced at
            // compile time. Handled rather than assumed, because this crate
            // runs where a panic takes the host process with it.
            self.flush(state, out)?;
            candidate = Sequence::new();
            candidate.push(key)?;
            overlap = 0;
        }

        let result = self.drive(&mut candidate, &mut overlap, out, true);
        state.pending = candidate;
        state.carry_overlap = overlap;
        result
    }

    /// Resolves pending romaji as if no further input were coming.
    ///
    /// Called on commit, on focus loss, and before any character that cannot
    /// take part in a sequence. Romaji that maps to nothing is passed through
    /// unchanged, so a half-typed `t` commits as `t` rather than vanishing.
    pub fn flush(&self, state: &mut Input, out: &mut impl TextSink) -> Result<(), Overflow> {
        let mut candidate = state.pending.clone();
        let mut overlap = state.carry_overlap;
        let result = self.drive(&mut candidate, &mut overlap, out, false);
        state.pending = candidate;
        state.carry_overlap = overlap;
        result
    }

    /// The resolution loop shared by [`Table::feed`] and [`Table::flush`].
    ///
    /// Terminates because every iteration either returns or replaces
    /// `candidate` with something strictly shorter: an entry consumes at
    /// least one character more than its carry gives back (enforced by
    /// [`TableErrorKind::CarryNotShorter`]), and a raw passthrough consumes
    /// one character and gives back nothing.
    ///
    /// `overlap` tracks [`Input::carry_overlap`] across the same steps. A
    /// step always matches from position 0, so of the old candidate's
    /// leading `*overlap` shared bytes, `consumed` are retired by this step
    /// and `overlap.saturating_sub(consumed)` survive untouched at the
    /// front of `candidate[consumed..]`. The new candidate is `carry +
    /// candidate[consumed..]`, so its own leading shared span is `carry`'s
    /// bytes plus however many of those survivors are still there:
    /// `carry.len() + overlap.saturating_sub(consumed)`.
    fn drive(
        &self,
        candidate: &mut Sequence,
        overlap: &mut usize,
        out: &mut impl TextSink,
        may_wait: bool,
    ) -> Result<(), Overflow> {
        loop {
            if candidate.is_empty() {
                *overlap = 0;
                return Ok(());
            }
            // A sequence that could still grow into a longer entry waits for
            // the next keystroke rather than committing to a shorter reading.
            if may_wait && self.extends(candidate.as_str()) {
                return Ok(());
            }

            let (emitted, consumed, carry) = match self.step_for(candidate.as_str()) {
                Step::Entry { index, consumed } => match self.entries.get(index) {
                    Some(entry) => (
                        Emission::Kana(&entry.output),
                        consumed,
                        entry.carry.as_str(),
                    ),
                    // `step_for` only ever returns an index it just found.
                    None => return Ok(()),
                },
                Step::Raw => match candidate.as_str().chars().next() {
                    Some(c) => (Emission::Raw(c), c.len_utf8(), ""),
                    None => return Ok(()),
                },
            };

            let mut next = Sequence::new();
            // Building the remainder before writing keeps the FSM state
            // consistent if the sink refuses the output.
            next.push_str(carry)?;
            if let Some(rest) = candidate.as_str().get(consumed..) {
                next.push_str(rest)?;
            }

            match emitted {
                Emission::Kana(kana) => out.push_str(kana)?,
                Emission::Raw(c) => out.push(c)?,
            }
            *candidate = next;
            *overlap = carry.len() + overlap.saturating_sub(consumed);
        }
    }

    /// Is some entry strictly longer than `sequence` and starting with it?
    ///
    /// One lookup, because sorting puts every such entry immediately after
    /// `sequence`'s own position.
    fn extends(&self, sequence: &str) -> bool {
        let index = match self.index_of(sequence) {
            Some(found) => found + 1,
            None => self.insertion_point(sequence),
        };
        self.entries.get(index).is_some_and(|entry| {
            entry.sequence.len() > sequence.len() && entry.sequence.starts_with(sequence)
        })
    }

    /// The longest prefix of `sequence` that is an entry, longest first.
    ///
    /// At most [`MAX_SEQUENCE`] binary searches, which for a table of a few
    /// hundred entries is cheaper than the trie walk it replaces.
    fn step_for(&self, sequence: &str) -> Step {
        let mut consumed = sequence.len();
        while consumed > 0 {
            if let Some(prefix) = sequence.get(..consumed) {
                if let Some(index) = self.index_of(prefix) {
                    return Step::Entry { index, consumed };
                }
            }
            consumed -= 1;
        }
        Step::Raw
    }

    fn index_of(&self, sequence: &str) -> Option<usize> {
        self.entries
            .binary_search_by(|entry| entry.sequence.as_str().cmp(sequence))
            .ok()
    }

    fn insertion_point(&self, sequence: &str) -> usize {
        match self
            .entries
            .binary_search_by(|entry| entry.sequence.as_str().cmp(sequence))
        {
            Ok(index) | Err(index) => index,
        }
    }
}

/// What one resolution step puts into the sink.
enum Emission<'t> {
    Kana(&'t str),
    Raw(char),
}

/// Validates and compiles one `sequence = value` entry.
fn compile_entry(sequence: &str, value: &Value) -> Result<CompiledEntry, TableError> {
    let fail = |kind| TableError {
        sequence: Some(sequence.to_string()),
        kind,
    };

    if !sequence.is_ascii() {
        return Err(fail(TableErrorKind::NonAsciiSequence));
    }
    if sequence.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(fail(TableErrorKind::UppercaseSequence));
    }
    if sequence.is_empty() || sequence.len() > MAX_SEQUENCE {
        return Err(fail(TableErrorKind::SequenceTooLong));
    }

    let (output, carry) = match value {
        Value::Text(text) => (text.as_str(), ""),
        Value::List(items) => match items.as_slice() {
            [output] => (output.as_str(), ""),
            [output, carry] => (output.as_str(), carry.as_str()),
            _ => return Err(fail(TableErrorKind::MalformedValue)),
        },
    };

    if output.is_empty() && carry.is_empty() {
        return Err(fail(TableErrorKind::EmptyEntry));
    }
    if !carry.is_ascii() {
        return Err(fail(TableErrorKind::NonAsciiCarry));
    }
    if carry.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(fail(TableErrorKind::UppercaseCarry));
    }
    if carry.len() >= sequence.len() {
        return Err(fail(TableErrorKind::CarryNotShorter));
    }

    Ok(CompiledEntry {
        sequence: sequence.to_string(),
        output: output.to_string(),
        carry: carry.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Table {
        Table::builtin().expect("the shipped table must compile")
    }

    /// Types `input` and leaves whatever is pending pending, the way the IME
    /// behaves mid-word.
    fn typed(table: &Table, input: &str) -> (String, String) {
        let mut state = Input::new();
        let mut out = String::new();
        for c in input.chars() {
            table.feed(&mut state, c, &mut out).expect("String sink");
        }
        (out, state.pending().to_string())
    }

    /// Types `input` and commits, the way Enter behaves.
    fn committed(table: &Table, input: &str) -> String {
        let mut state = Input::new();
        let mut out = String::new();
        for c in input.chars() {
            table.feed(&mut state, c, &mut out).expect("String sink");
        }
        table.flush(&mut state, &mut out).expect("String sink");
        assert!(state.is_empty(), "flush must leave nothing pending");
        out
    }

    #[test]
    fn the_shipped_table_compiles() {
        let table = builtin();
        assert!(table.len() > 200, "unexpectedly small: {}", table.len());
        assert!(!table.is_empty());
    }

    /// Every entry must be reachable by typing its own sequence. An entry that
    /// is shadowed by the matching rules is dead weight the author cannot see.
    #[test]
    fn every_carry_free_entry_is_reachable_by_typing_it() {
        let table = builtin();
        for entry in &table.entries {
            if !entry.carry.is_empty() {
                continue;
            }
            assert_eq!(
                committed(&table, &entry.sequence),
                entry.output,
                "entry {:?} is unreachable",
                entry.sequence
            );
        }
    }

    /// The carrying entries, checked the same way: typing the sequence emits
    /// the output and leaves the carry to resolve.
    #[test]
    fn every_carrying_entry_emits_its_output_and_carries_on() {
        let table = builtin();
        for entry in &table.entries {
            if entry.carry.is_empty() {
                continue;
            }
            let (emitted, _) = typed(&table, &entry.sequence);
            assert_eq!(
                emitted, entry.output,
                "entry {:?} did not emit its output",
                entry.sequence
            );
        }
    }

    #[test]
    fn vowels_and_basic_syllables() {
        let table = builtin();
        assert_eq!(committed(&table, "aiueo"), "あいうえお");
        assert_eq!(committed(&table, "kakikukeko"), "かきくけこ");
        assert_eq!(committed(&table, "sakura"), "さくら");
        assert_eq!(committed(&table, "nihongo"), "にほんご");
    }

    #[test]
    fn contracted_sounds() {
        let table = builtin();
        assert_eq!(committed(&table, "kyou"), "きょう");
        assert_eq!(committed(&table, "shain"), "しゃいん");
        assert_eq!(committed(&table, "chotto"), "ちょっと");
        assert_eq!(committed(&table, "jugyou"), "じゅぎょう");
    }

    /// The `n` cases, which are the reason the matcher backtracks at all.
    #[test]
    fn n_resolves_by_what_follows_it() {
        let table = builtin();
        // Followed by a consonant: ん, and the consonant starts a new sequence.
        assert_eq!(committed(&table, "genki"), "げんき");
        assert_eq!(committed(&table, "shinkansen"), "しんかんせん");
        // Followed by a vowel: the longer reading wins.
        assert_eq!(committed(&table, "kani"), "かに");
        assert_eq!(committed(&table, "sunao"), "すなお");
        // An apostrophe provides a concise ん boundary before a な-row or
        // や-row kana. Without it `honya` reads ほにゃ. With the
        // Microsoft-compatible `nn` rule, `honnya` is ほんや; a third `n`
        // makes the next syllable にゃ.
        assert_eq!(committed(&table, "hon'ya"), "ほんや");
        assert_eq!(committed(&table, "honya"), "ほにゃ");
        assert_eq!(committed(&table, "honnya"), "ほんや");
        assert_eq!(committed(&table, "honnnya"), "ほんにゃ");
        assert_eq!(committed(&table, "konnyaku"), "こんやく");
        assert_eq!(committed(&table, "konnnyaku"), "こんにゃく");
        // Alone at the end of input.
        assert_eq!(committed(&table, "n"), "ん");
        assert_eq!(committed(&table, "pan"), "ぱん");
    }

    /// Microsoft IME treats `nn` as an explicit ん, even before a vowel.
    /// A following な-row syllable therefore needs a third `n`.
    #[test]
    fn microsoft_double_n_commits_n_before_a_vowel() {
        let table = builtin();
        assert_eq!(committed(&table, "hannei"), "はんえい");
        assert_eq!(committed(&table, "minna"), "みんあ");
        assert_eq!(committed(&table, "annai"), "あんあい");
        assert_eq!(committed(&table, "onnanoko"), "おんあのこ");
        // The third `n` begins the following な-row syllable.
        assert_eq!(committed(&table, "minnna"), "みんな");
        assert_eq!(committed(&table, "annnai"), "あんない");
        assert_eq!(committed(&table, "konnnichiha"), "こんにちは");
    }

    /// Mid-word, a lone `n` stays pending so the user can still reach `な`.
    #[test]
    fn a_lone_n_waits_before_committing_to_a_reading() {
        let table = builtin();
        let (emitted, pending) = typed(&table, "n");
        assert_eq!(emitted, "");
        assert_eq!(pending, "n");
    }

    #[test]
    fn sokuon_comes_from_the_doubled_consonant() {
        let table = builtin();
        assert_eq!(committed(&table, "kekka"), "けっか");
        assert_eq!(committed(&table, "matte"), "まって");
        assert_eq!(committed(&table, "kitto"), "きっと");
        assert_eq!(committed(&table, "asatte"), "あさって");
        assert_eq!(committed(&table, "zasshi"), "ざっし");
        assert_eq!(committed(&table, "happa"), "はっぱ");
        // Explicit small tsu, both spellings.
        assert_eq!(committed(&table, "xtu"), "っ");
        assert_eq!(committed(&table, "ltsu"), "っ");
    }

    /// `nn` is ん, not a sokuon. Every other doubled consonant is a sokuon.
    #[test]
    fn doubled_n_is_not_a_sokuon() {
        let table = builtin();
        assert_eq!(committed(&table, "nn"), "ん");
        assert_eq!(committed(&table, "annnai"), "あんない");
    }

    /// EVAL: scans every shipped entry for the exact structural shape that
    /// causes a live-typing stall -- an entry that is itself a complete,
    /// valid mapping (`Table::drive` could commit it right now) but is also
    /// a strict prefix of one or more longer entries, so `may_wait` in
    /// `Table::drive` holds it pending until another key (or an explicit
    /// flush) arrives. Only `n` has that shape in the Microsoft-compatible
    /// shipped table. `nn` must commit ん immediately so `hannei` cannot
    /// become はんねい. If a future table edit (including a user's custom
    /// table) adds another entry with this shape, this fails so the same
    /// stall gets a deliberate look instead of shipping silently.
    #[test]
    fn only_n_is_a_complete_entry_that_still_waits_for_more() {
        let table = builtin();
        let mut stalls: Vec<&str> = table
            .entries
            .iter()
            .filter(|entry| entry.carry.is_empty())
            .filter(|entry| table.extends(&entry.sequence))
            .map(|entry| entry.sequence.as_str())
            .collect();
        stalls.sort_unstable();
        assert_eq!(
            stalls,
            vec!["n"],
            "an entry both commits on its own and waits for more input; \
             review whether the live-typing stall this causes (raw romaji \
             stays on screen until another key or a flush) is intended"
        );
    }

    /// EVAL: `data/romaji.toml` spells every small kana two ways -- `x` and
    /// `l` prefixes (`xa`/`la`, `xtu`/`ltu`, `xka`/`lka`, ...) -- so both the
    /// long-standing `x` convention and the newer `l` one work identically.
    /// Nothing but this test enforces that they stay identical: the compiler
    /// has no notion that `xa` and `la` are supposed to agree, so a future
    /// table edit that changes one prefix's output but not the other's would
    /// compile cleanly and silently make the two spellings of "the same
    /// small kana" produce different kana. `xn` is excluded on purpose -- it
    /// is an alternate spelling of ん (see `xn = "ん"` in the table), not a
    /// small-kana prefix, and has no `l` counterpart to compare against.
    #[test]
    fn x_and_l_small_kana_prefixes_stay_in_sync() {
        let table = builtin();
        let x_forms: std::collections::BTreeMap<&str, &str> = table
            .entries
            .iter()
            .filter(|entry| entry.sequence != "xn")
            .filter_map(|entry| {
                entry
                    .sequence
                    .strip_prefix('x')
                    .map(|rest| (rest, entry.output.as_str()))
            })
            .collect();
        let l_forms: std::collections::BTreeMap<&str, &str> = table
            .entries
            .iter()
            .filter_map(|entry| {
                entry
                    .sequence
                    .strip_prefix('l')
                    .map(|rest| (rest, entry.output.as_str()))
            })
            .collect();
        assert_eq!(
            x_forms, l_forms,
            "every `x`-prefixed small-kana spelling must have an identical \
             `l`-prefixed twin, and vice versa"
        );
    }

    /// EVAL: `nn` commits ん immediately and every remaining `n` begins a new
    /// decision. This is what makes `hannei` unambiguous, while spelling
    /// ん+な as `minnna` remains available without an apostrophe.
    #[test]
    fn n_runs_follow_the_microsoft_double_n_rule() {
        let table = builtin();
        assert_eq!(
            committed(&table, "minna"),
            "みんあ",
            "two `n`s commit ん before the vowel"
        );
        assert_eq!(
            committed(&table, "minnna"),
            "みんな",
            "the third `n` starts な"
        );
        assert_eq!(committed(&table, "nn"), "ん");
        assert_eq!(
            committed(&table, "nnn"),
            "んん",
            "the remaining `n` becomes its own ん on commit"
        );
        assert_eq!(committed(&table, "denn"), "でん");
        assert_eq!(
            committed(&table, "dennn"),
            "でんん",
            "the third `n` is a separate trailing ん"
        );
    }

    /// EVAL: `every_carrying_entry_emits_its_output_and_carries_on` proves the
    /// output half of a carrying entry via `feed` (mid-typing, where
    /// `may_wait` is `true`); this proves the other half via `flush`
    /// (`may_wait = false`), the code path `Enter` actually uses. `kk` alone
    /// is unremarkable -- っ commits and the carried `k` passes through raw
    /// because no bare `k` entry exists -- but nothing in the type system
    /// forces that: a future table edit adding a bare single-consonant entry
    /// (as `n` already is) would silently change what every sokuon carrying
    /// that consonant commits to when nothing follows it. This locks today's
    /// correct answer in so that change gets a deliberate look instead of
    /// shipping as a side effect of an unrelated edit.
    #[test]
    fn every_carrying_entry_resolves_deterministically_when_flushed_alone() {
        let table = builtin();
        for entry in &table.entries {
            if entry.carry.is_empty() {
                continue;
            }
            let expected = format!("{}{}", entry.output, entry.carry);
            assert_eq!(
                committed(&table, &entry.sequence),
                expected,
                "entry {:?} did not resolve to output+carry when flushed alone",
                entry.sequence
            );
        }
    }

    #[test]
    fn punctuation_and_the_long_vowel_mark() {
        let table = builtin();
        assert_eq!(committed(&table, "ra-men"), "らーめん");
        assert_eq!(committed(&table, "a,bi."), "あ、び。");
        assert_eq!(committed(&table, "[a]"), "「あ」");
        assert_eq!(committed(&table, "a/i"), "あ・い");
    }

    /// EVAL: `n'` provides a concise delimiter for ん before a な/や-row
    /// syllable. Without it, the Microsoft-compatible table needs a third
    /// `n` (`honnnya`) to produce ほんにゃ. Nothing extends past `n'` (see
    /// `only_n_is_a_complete_entry_that_still_waits_for_more`), so it commits
    /// ん the instant it is typed. This also verifies that an apostrophe stays
    /// inert everywhere it does not follow an `n`.
    #[test]
    fn apostrophe_disambiguates_n_and_is_inert_elsewhere() {
        let table = builtin();
        assert_eq!(committed(&table, "n'"), "ん");
        let (emitted, pending) = typed(&table, "n'");
        assert_eq!(emitted, "ん", "`n'` should commit without waiting for more");
        assert_eq!(pending, "");
        // A second word pair alongside hon'ya/honya/honnnya, so the earlier
        // result is not an artifact of that one word's shape.
        assert_eq!(committed(&table, "kon'yaku"), "こんやく");
        assert_eq!(committed(&table, "konyaku"), "こにゃく");
        assert_eq!(committed(&table, "konnnyaku"), "こんにゃく");
        // With no preceding `n` to disambiguate, the apostrophe means
        // nothing and passes through like any other unmapped character --
        // it must not silently vanish.
        assert_eq!(committed(&table, "'"), "'");
        assert_eq!(committed(&table, "a'i"), "あ'い");
    }

    /// Letters with no reading pass through rather than disappearing — this is
    /// what makes a mistyped word recoverable instead of silently eaten.
    #[test]
    fn unmapped_letters_pass_through() {
        let table = builtin();
        assert_eq!(committed(&table, "docker"), "どcけr");
        assert_eq!(committed(&table, "kit"), "きt");
        assert_eq!(committed(&table, "q"), "q");
        assert_eq!(committed(&table, "!?"), "!?");
    }

    #[test]
    fn lookup_folds_ascii_case() {
        let table = builtin();
        assert_eq!(committed(&table, "KA"), "か");
        assert_eq!(committed(&table, "KoNnNiChiHa"), "こんにちは");
    }

    /// A non-ASCII character cannot be part of a sequence, so pending romaji
    /// resolves first and the character follows it in typing order.
    #[test]
    fn non_ascii_input_flushes_pending_then_passes_through() {
        let table = builtin();
        assert_eq!(committed(&table, "kan字"), "かん字");
        assert_eq!(committed(&table, "a🍣"), "あ🍣");
    }

    #[test]
    fn backspace_eats_pending_romaji_before_kana() {
        let table = builtin();
        let mut state = Input::new();
        let mut out = String::new();
        for c in "kaky".chars() {
            table.feed(&mut state, c, &mut out).expect("String sink");
        }
        assert_eq!(out, "か");
        assert_eq!(state.pending(), "ky");

        assert!(state.backspace());
        assert_eq!(state.pending(), "k");
        assert!(state.backspace());
        assert_eq!(state.pending(), "");
        // Nothing pending: the backspace belongs to the emitted kana instead.
        assert!(!state.backspace());
    }

    #[test]
    fn clear_discards_pending_without_emitting() {
        let table = builtin();
        let mut state = Input::new();
        let mut out = String::new();
        table.feed(&mut state, 'k', &mut out).expect("String sink");
        state.clear();
        table.flush(&mut state, &mut out).expect("String sink");
        assert_eq!(out, "");
    }

    #[test]
    fn a_full_sink_reports_overflow_instead_of_truncating_silently() {
        let table = builtin();
        let mut state = Input::new();
        // 3 bytes per kana: two fit, the third does not.
        let mut out = FixedStr::<6>::new();
        table.feed(&mut state, 'a', &mut out).expect("fits");
        table.feed(&mut state, 'i', &mut out).expect("fits");
        assert_eq!(table.feed(&mut state, 'u', &mut out), Err(Overflow));
        assert_eq!(out.as_str(), "あい");
    }

    /// EVAL: when `feed` resolves more than one step in the same call (here,
    /// `n` completing to ん and then the fresh `q` starting a new,
    /// still-pending candidate) and the sink overflows partway through, the
    /// step(s) that already reached the sink must stay committed and the
    /// step that didn't must stay recoverable. This is `drive`'s
    /// `*candidate = next` ordering under test: it only runs after the sink
    /// accepts the emission, so a failed emission leaves `candidate` (and
    /// thus `state.pending` once `feed` stores it back) exactly where it was
    /// before that step, not half-updated.
    #[test]
    fn overflow_preserves_unconsumed_suffix_after_partial_resolution() {
        let table = builtin();
        let mut state = Input::new();
        table
            .feed(&mut state, 'n', &mut String::new())
            .expect("String sink");

        // Exactly enough room for ん (3 bytes) and no more.
        let mut out = FixedStr::<3>::new();
        assert_eq!(table.feed(&mut state, 'q', &mut out), Err(Overflow));
        assert_eq!(out.as_str(), "ん", "the step that fit must still land");
        assert_eq!(
            state.pending(),
            "q",
            "the step that didn't fit must survive as pending, not vanish"
        );

        // A later flush to a sink with room recovers exactly the part that
        // overflowed -- nothing was lost, and nothing was emitted twice.
        let mut flushed = String::new();
        table.flush(&mut state, &mut flushed).expect("String sink");
        assert_eq!(flushed, "q");
    }

    /// EVAL: the non-ASCII branch of `feed` recovers differently from the
    /// ASCII branch above, and that difference is worth spelling out rather
    /// than leaving implicit. An ASCII overflow always leaves the
    /// unconsumed part sitting in `state.pending` (proved above), because
    /// pending romaji is where the FSM's mid-resolution state naturally
    /// lives. A non-ASCII character never enters `pending` at all -- it
    /// isn't ASCII, so the FSM's buffer cannot hold it even transiently --
    /// so when `out.push(key)` overflows after a successful `flush`, the
    /// character is not stored anywhere in `Input`. Recovery depends
    /// entirely on the caller re-feeding the identical key once the sink has
    /// room, exactly as `feed`'s doc comment describes ("a retry continues
    /// from where it stopped"). This proves that retry actually works end to
    /// end, and documents the asymmetry so a caller (`sakura-engine`'s
    /// dispatch loop) cannot assume both overflow paths recover the same
    /// way.
    #[test]
    fn non_ascii_overflow_never_loses_the_current_character() {
        let table = builtin();
        let mut state = Input::new();
        table
            .feed(&mut state, 'n', &mut String::new())
            .expect("String sink");

        // Exactly enough room for ん (3 bytes) and no more, so ん flushes
        // clean but 字 has nowhere to go.
        let mut out = FixedStr::<3>::new();
        assert_eq!(table.feed(&mut state, '字', &mut out), Err(Overflow));
        assert_eq!(out.as_str(), "ん");
        assert_eq!(
            state.pending(),
            "",
            "字 is not ASCII, so unlike the suffix case above it cannot be \
             held in Input's pending buffer -- there is nowhere in Input \
             for it to survive an overflow"
        );

        // Retrying the exact same key against a sink with room recovers it.
        let mut retry = String::new();
        table
            .feed(&mut state, '字', &mut retry)
            .expect("retry with room must succeed");
        assert_eq!(retry, "字");
    }

    /// EVAL: a corpus of real, whole words, each crossing several entry
    /// boundaries. `every_carry_free_entry_is_reachable_by_typing_it` proves
    /// every single entry works in isolation, but the `nn` report was never
    /// about one entry in isolation -- it was about what happens where two
    /// entries meet. This is the same idea applied at the boundary: sokuon
    /// immediately followed by the consonant it carries back, youon next to
    /// a plain vowel, a mapped run next to an unmapped character, and so on.
    #[test]
    fn whole_word_corpus_crosses_entry_boundaries_correctly() {
        let table = builtin();
        let cases = [
            ("ohayou", "おはよう"),
            ("arigatou", "ありがとう"),
            ("sayounara", "さようなら"),
            // sokuon immediately followed by the carried consonant + vowel.
            ("shuppatsu", "しゅっぱつ"),
            ("kekkon", "けっこん"),
            ("kippu", "きっぷ"),
            ("kitte", "きって"),
            ("zutto", "ずっと"),
            // a multi-char entry (`chi`) immediately followed by a plain
            // vowel that must not be absorbed into it.
            ("chiisai", "ちいさい"),
            // `s` alone is not a complete entry -- must wait for `sha`, not
            // misresolve partway through.
            ("kaisha", "かいしゃ"),
            // youon (`gyu`, `nyu`) next to a plain vowel and next to `n`.
            ("gyuunyuu", "ぎゅうにゅう"),
            // v-row, a mapped run next to an unmapped punctuation character.
            ("vaiorin", "ゔぁいおりん"),
            ("sugoi!", "すごい!"),
        ];
        for (input, expected) in cases {
            assert_eq!(committed(&table, input), expected, "input {input:?}");
        }
    }

    /// EVAL: the common word `反映` is typeable continuously as `hannei`, as
    /// it is in Microsoft IME. The second `n` completes ん before `e`, rather
    /// than becoming the `n` in ね and producing はんねい.
    #[test]
    fn hannei_can_be_typed_continuously_with_double_n() {
        let table = builtin();
        assert_eq!(committed(&table, "hannei"), "はんえい");
    }

    // --- Table compilation ---

    /// A table of three entries is enough to exercise waiting, backtracking
    /// and carry, which is the whole FSM.
    #[test]
    fn a_minimal_table_compiles_and_works() {
        let table = Table::parse("[kana]\nka = \"か\"\nki = \"き\"\nkk = [\"っ\", \"k\"]\n")
            .expect("compile");
        assert_eq!(table.len(), 3);
        assert_eq!(committed(&table, "kakki"), "かっき");
        // `k` alone has no reading; it waits, then passes through on commit.
        assert_eq!(committed(&table, "k"), "k");
        // Nothing in this table reads `z`, so it goes straight out.
        assert_eq!(committed(&table, "zka"), "zか");
    }

    #[test]
    fn every_malformed_table_names_its_fault() {
        let cases: [(&str, TableErrorKind); 8] = [
            ("[other]\na = \"あ\"\n", TableErrorKind::MissingSection),
            ("[kana]\n", TableErrorKind::EmptyTable),
            (
                "[kana]\n\"あ\" = \"あ\"\n",
                TableErrorKind::NonAsciiSequence,
            ),
            ("[kana]\nA = \"あ\"\n", TableErrorKind::UppercaseSequence),
            (
                "[kana]\nabcdefghi = \"あ\"\n",
                TableErrorKind::SequenceTooLong,
            ),
            ("[kana]\na = []\n", TableErrorKind::MalformedValue),
            ("[kana]\na = \"\"\n", TableErrorKind::EmptyEntry),
            (
                "[kana]\nkk = [\"っ\", \"kk\"]\n",
                TableErrorKind::CarryNotShorter,
            ),
        ];
        for (source, expected) in cases {
            let error = Table::parse(source).expect_err("expected a table error");
            assert_eq!(error.kind, expected, "source: {source:?}");
        }
    }

    #[test]
    fn a_config_error_is_reported_as_one() {
        let error = Table::parse("[kana]\na = 1\n").expect_err("expected an error");
        assert!(matches!(error.kind, TableErrorKind::Config(_)));
        assert!(error.to_string().contains("line 2"));
    }

    /// The carry rule is what proves the resolution loop terminates, so it is
    /// checked structurally rather than by hoping no user writes a cycle.
    #[test]
    fn a_self_referential_carry_is_rejected() {
        let error = Table::parse("[kana]\nab = [\"x\", \"ab\"]\n").expect_err("expected an error");
        assert_eq!(error.kind, TableErrorKind::CarryNotShorter);
    }

    /// The FSM eats whatever a keyboard can produce, in a process where a
    /// panic is the host application dying. Termination is the property under
    /// test: a run that does not stop hangs this test rather than failing it,
    /// which is the correct outcome for an infinite loop.
    #[test]
    fn arbitrary_input_terminates_and_never_panics() {
        let table = builtin();
        // xorshift64*, because the workspace ships no third-party crates.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };

        for _ in 0..20_000 {
            let length = (next() % 16) as usize;
            let mut input = Input::new();
            let mut out = String::new();
            for _ in 0..length {
                // The printable ASCII range plus a few characters outside it.
                let c = match next() % 32 {
                    0 => 'あ',
                    1 => '\u{3000}',
                    n => char::from(0x20 + (n as u8 - 2) % 0x5F),
                };
                let _ = table.feed(&mut input, c, &mut out);
            }
            let _ = table.flush(&mut input, &mut out);
            assert!(input.is_empty(), "flush left {:?} pending", input.pending());
        }
    }
}
