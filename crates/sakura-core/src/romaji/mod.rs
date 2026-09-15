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

mod completion;
mod fsm;
mod replay;
mod table;

use sakura_values::FixedStr;

pub use completion::{LocalCompletion, LocalCompletionError, LocalCompletionList};
pub use fsm::Input;
pub use replay::{ReplayError, ReplayEvent, ReplayEventKind, ReplayTrace};
pub use table::{TableError, TableErrorKind};

/// The default table, compiled into the binary.
///
/// Embedded rather than read from disk so the IME can always fall back to a
/// working table: a user whose edited file fails to parse gets this one and a
/// diagnostic, not a keyboard that types nothing.
pub const DEFAULT_TABLE: &str = include_str!("../../../../data/romaji.toml");

/// The section of the config document the table is read from.
pub const TABLE_SECTION: &str = "kana";

/// The longest romaji sequence an entry may use.
///
/// Also the size of the pending buffer, which is sound because pending romaji
/// is always a *proper* prefix of some entry and therefore strictly shorter
/// than the longest one.
pub const MAX_SEQUENCE: usize = 8;

/// Maximum raw-key span accepted by the pure provenance replay helper.
///
/// The helper is deliberately smaller than the engine's preedit limit.  It
/// is a local-completion probe, not a second unbounded input log; callers that
/// need to inspect a larger composition must first split it at a trusted
/// append-only boundary.
pub const MAX_REPLAY_RAW_BYTES: usize = 128;

/// Maximum number of trace emissions retained by one replay.
///
/// A normal entry retires one source span, while a valid custom carry can
/// trigger a short chain of additional table entries.  The product is a
/// conservative fixed cap for that chain plus one terminal step; it keeps a
/// custom table bounded without pretending every table has the shipped
/// table's carry shape.
pub const MAX_REPLAY_EVENTS: usize = MAX_REPLAY_RAW_BYTES * MAX_SEQUENCE + 1;

/// Maximum UTF-8 output retained by one replay.
pub const MAX_REPLAY_OUTPUT_BYTES: usize = MAX_REPLAY_RAW_BYTES * 4;

/// Maximum table-derived completions returned for one structural anomaly.
///
/// Phase 1 normally admits one completion.  A custom table may make more than
/// one ASCII key produce the same corrected reading, so the result is bounded
/// as a list rather than silently depending on the first key in enumeration
/// order.
pub const MAX_LOCAL_COMPLETIONS: usize = 8;

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

#[cfg(test)]
use sakura_values::Overflow;

#[cfg(test)]
#[path = "romaji_tests.rs"]
mod tests;
