//! Dormant Issue 34 context research.

// These four modules intentionally preserve dormant research with no runtime
// entry point. Keep the allowance on the owned modules rather than the crate.
#[allow(dead_code)]
mod context_baseline;
#[allow(dead_code)]
mod context_evaluation;
#[allow(dead_code)]
mod context_intelligence;
#[cfg(test)]
mod context_intelligence_budget_tests;
#[allow(dead_code)]
mod prediction_snapshot;
