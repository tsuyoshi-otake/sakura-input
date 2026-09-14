//! Lattice construction and bounded N-best / Viterbi search types.
//!
//! Search algorithms stay on `Converter` until a later 3.1 extract. `#99`
//! synthesis and `#94`/`#108` ranking stay out of this directory.

mod lattice;
mod viterbi;

pub(in crate::conversion) use lattice::{
    char_class, char_run, CharClass, DictionaryEdgeBudget, Node, NodeSpec,
};
pub(in crate::conversion) use viterbi::{HeapItem, PathClass, SearchRun, SearchState};
