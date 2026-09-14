//! Ordering and suppression rules for one conversion candidate list.
//!
//! `#94` / `#108`-shaped work reads these modules plus evidence and candidate
//! assembly. Lattice search and generated-family synthesis stay out of this
//! directory.

mod coherence;
mod cost;
mod it_terms;
mod quality_gate;
mod tie_break;

pub(in crate::conversion) use cost::{connection_cost, numeric_form_cost, synthetic_run_cost};
pub(in crate::conversion) use tie_break::sort_by_cost;
