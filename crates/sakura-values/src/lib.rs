#![forbid(unsafe_code)]

mod ai_text;
mod appearance;
mod capacity;
mod fingerprint;
mod fixed;
mod input;
mod scope;

pub use ai_text::{AiTextOperation, AiTextStatus};
pub use appearance::{AppearanceTheme, PadShortcut};
pub use capacity::{
    CANDIDATE_PAGE_SIZE, MAX_CANDIDATES, MAX_CANDIDATE_DETAIL_DEFINITION_BYTES,
    MAX_CANDIDATE_DETAIL_READING_BYTES, MAX_CANDIDATE_DETAIL_RELATIONS,
    MAX_CANDIDATE_DETAIL_RELATION_BYTES, MAX_CANDIDATE_DETAIL_RELATION_TEXT_BYTES,
    MAX_CANDIDATE_TEXT_BYTES, MAX_COMMIT_BYTES, MAX_PREEDIT_BYTES, MAX_SEGMENTS,
};
pub use fingerprint::Fingerprint;
pub use fixed::{FixedStr, FixedVec, Overflow};
pub use input::{KeyCode, KeyInput, Mode, Modifiers};
pub use scope::InputScope;
