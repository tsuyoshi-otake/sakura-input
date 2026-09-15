//! The input path: pending romaji state and the longest-match-with-
//! backtracking resolution that turns typed characters into kana.

use sakura_values::Overflow;

use crate::text::TextSink;

use super::table::Step;
use super::{Sequence, Table};

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

impl Table {
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
}

/// What one resolution step puts into the sink.
pub(super) enum Emission<'t> {
    Kana(&'t str),
    Raw(char),
}
