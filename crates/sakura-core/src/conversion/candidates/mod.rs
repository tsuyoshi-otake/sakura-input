mod assembly;
mod dedup;

pub use assembly::{ConversionCandidate, ConversionSegment};
pub(in crate::conversion) use dedup::has_same_surface;
