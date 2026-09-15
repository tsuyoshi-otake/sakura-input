//! Stable details shared with the `dictc` writer.
//!
//! These constants describe an on-disk interface. Changing an existing field
//! requires a new format version; adding an optional table does not, because
//! readers deliberately skip unknown directory tags.
//!
//! This file is the only definition of the image layout. The accepted v2
//! contract and its verification are recorded in
//! `verification/dictionary-format-v2.md`.

pub const MAGIC: [u8; 8] = *b"SKRADIC\0";
/// The writer-facing version alias remains v1 until the v2 writer lands.
pub const VERSION: u16 = 1;
pub const VERSION_V2: u16 = 2;
pub const HEADER_LEN: usize = 32;
pub const DIRECTORY_ENTRY_LEN: usize = 16;
pub const MAX_TABLES: usize = 64;

pub const TAG_LOUDS: [u8; 4] = *b"LOUD";
pub const TAG_NODES: [u8; 4] = *b"NODE";
pub const TAG_LABELS: [u8; 4] = *b"LABL";
pub const TAG_ENTRIES: [u8; 4] = *b"ENTR";
pub const TAG_SURFACE_OFFSETS: [u8; 4] = *b"SOFF";
pub const TAG_SURFACES: [u8; 4] = *b"SURF";
pub const TAG_ANNOTATION_OFFSETS: [u8; 4] = *b"AOFF";
pub const TAG_ANNOTATIONS: [u8; 4] = *b"ANNO";
pub const TAG_ANNOTATION_INDEX: [u8; 4] = *b"AIDX";
pub const TAG_MATRIX: [u8; 4] = *b"MATR";

// Optional sparse, entry-ordinal-keyed detail data. These tables retain the
// same final ordinal contract across the v1 24-byte and v2 16-byte ENTR
// layouts; older images simply have no details.
pub const TAG_DETAIL_INDEX: [u8; 4] = *b"DIDX";
pub const TAG_DETAIL_RECORDS: [u8; 4] = *b"DREC";
pub const TAG_DETAIL_RELATIONS: [u8; 4] = *b"DREL";
pub const TAG_DETAIL_TEXT_OFFSETS: [u8; 4] = *b"DTOF";
pub const TAG_DETAIL_TEXT: [u8; 4] = *b"DTXT";

// Optional bunsetsu-boundary matrix compiled from the pinned Mozc
// segmenter rules.  Bit (rid, lid) tells whether a segment boundary
// separates a word ending with `rid` from a word starting with `lid`;
// conversion fuses adjacent path words when the bit is clear.  Images
// without this table keep morpheme-granularity segments.
pub const TAG_BOUNDARIES: [u8; 4] = *b"BNDR";

// Optional single-kanji table compiled from the pinned Mozc single-kanji
// data.  Single kanji are appended after conversion rather than searched:
// one short reading can name hundreds of characters, so giving each its own
// lattice edge would spend the whole node budget on a reading the n-best
// search has already answered.  The table is therefore keyed by its own
// sorted reading list rather than by the entry trie, which also keeps it
// independent of how the shipped dictionary happens to be trimmed.
pub const TAG_SINGLE_KANJI_INDEX: [u8; 4] = *b"SKIX";
pub const TAG_SINGLE_KANJI_READINGS: [u8; 4] = *b"SKRD";
pub const TAG_SINGLE_KANJI_CHARS: [u8; 4] = *b"SKCH";
pub const TAG_SINGLE_KANJI_VARIANTS: [u8; 4] = *b"SKVR";

/// `reading_offset: u32`, `char_start: u32`, `reading_len: u16`,
/// `char_count: u16`, sorted by reading bytes for binary search.
pub const SINGLE_KANJI_INDEX_LEN: usize = 12;
/// One `u32` scalar value per character; a fixed stride removes the need
/// for a second offsets table.
pub const SINGLE_KANJI_CHAR_LEN: usize = 4;
/// `variant: u32`, `original: u32`, `kind: u8`, three zero pad bytes,
/// sorted by variant scalar value for binary search.
pub const SINGLE_KANJI_VARIANT_LEN: usize = 12;

/// Writer-facing v1 record sizes. Do not repoint these aliases at v2.
pub const NODE_LEN: usize = 16;
pub const ENTRY_LEN: usize = 24;
pub const NODE_LEN_V2: usize = 16;
pub const ENTRY_LEN_V2: usize = 16;
pub const ANNOTATION_INDEX_LEN_V2: usize = 8;
pub const SURFACE_RESTART_INTERVAL: usize = 16;
pub const NO_ANNOTATION: u32 = u32::MAX;

pub const MATRIX_MAGIC: [u8; 4] = *b"MSP1";
pub const MATRIX_HEADER_LEN: usize = 16;
pub const MATRIX_ROW_LEN: usize = 8;
pub const MATRIX_OVERRIDE_LEN: usize = 4;
/// Boundary rows are byte-aligned: each of the `class_count` rows spans
/// `ceil(class_count / 8)` bytes with zero padding bits at the row tail.
pub const BOUNDARY_MAGIC: [u8; 4] = *b"SBD1";
pub const BOUNDARY_HEADER_LEN: usize = 8;
pub const DETAIL_INDEX_LEN: usize = 8;
pub const DETAIL_RECORD_LEN: usize = 16;
pub const DETAIL_RELATION_LEN: usize = 8;
