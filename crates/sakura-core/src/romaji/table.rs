//! Building a table: compiling a config document into sorted entries,
//! the errors a malformed table reports, and the two lookups the FSM asks
//! of the result.

use crate::config::{self, Document, ParseError, Value};

use super::{CompiledEntry, Table, DEFAULT_TABLE, MAX_SEQUENCE, TABLE_SECTION};

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
pub(super) enum Step {
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

    /// Is some entry strictly longer than `sequence` and starting with it?
    ///
    /// One lookup, because sorting puts every such entry immediately after
    /// `sequence`'s own position.
    pub(super) fn extends(&self, sequence: &str) -> bool {
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
    pub(super) fn step_for(&self, sequence: &str) -> Step {
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
