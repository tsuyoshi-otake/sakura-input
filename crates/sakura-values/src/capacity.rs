/// The capacity, in UTF-8 bytes, of `OutputBuf`'s preedit text buffer.
pub const MAX_PREEDIT_BYTES: usize = 1536;

/// The capacity, in UTF-8 bytes, of `OutputBuf`'s commit text buffer.
pub const MAX_COMMIT_BYTES: usize = 1536;

/// The maximum number of segments a preedit composition or an
/// `OutputBuf` may hold.
pub const MAX_SEGMENTS: usize = 64;

/// Candidates shown on one numbered page.
pub const CANDIDATE_PAGE_SIZE: usize = 9;

/// Maximum candidates carried in one output frame.
///
/// Two bounded pages were enough while the converter could only offer
/// two pages' worth. They are not enough for a one-mora reading: the
/// pinned single-kanji table alone names 315 characters under こう, and a
/// commercial IME lists 210 under ひ (Issue #95). The frame therefore
/// carries a paging-sized list rather than a two-page one.
///
/// This is the ceiling, not the working limit. What a given reading may
/// actually spend is `sakura_core::conversion::candidate_budget`, which
/// keeps a long reading at its former bound.
pub const MAX_CANDIDATES: usize = 256;

/// Fixed storage for all candidate surfaces or annotations in an `OutputBuf`.
pub const MAX_CANDIDATE_TEXT_BYTES: usize = MAX_PREEDIT_BYTES * CANDIDATE_PAGE_SIZE;

/// Maximum UTF-8 byte length of a selected candidate's reading.
pub const MAX_CANDIDATE_DETAIL_READING_BYTES: usize = 256;

/// Maximum UTF-8 byte length of a full selected-candidate definition.
///
/// This exceeds common UI display limits deliberately: UI Automation receives
/// the complete source-backed definition, never a silently truncated one.
pub const MAX_CANDIDATE_DETAIL_DEFINITION_BYTES: usize = 1024;

/// Maximum UTF-8 byte length of one related-term label.
pub const MAX_CANDIDATE_DETAIL_RELATION_BYTES: usize = 128;

/// Maximum related words in each of aliases, related, similar, and antonyms.
pub const MAX_CANDIDATE_DETAIL_RELATIONS: usize = 3;

/// Fixed backing storage for all relation strings in an `OutputBuf`.
pub const MAX_CANDIDATE_DETAIL_RELATION_TEXT_BYTES: usize =
    MAX_CANDIDATE_DETAIL_RELATION_BYTES * MAX_CANDIDATE_DETAIL_RELATIONS * 4;
