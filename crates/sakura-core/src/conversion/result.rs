use super::ConversionCandidate;

/// Explicit terminal condition for the bounded N-best search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionSearchTerminal {
    CandidateLimitReached,
    SearchExhausted,
    StateBudgetReached,
    LatticeBudgetReached,
}

/// Aggregate, text-free evidence about one conversion attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversionDiagnostics {
    pub terminal: ConversionSearchTerminal,
    /// Number of lattice nodes materialized for this pass.
    pub lattice_nodes: usize,
    pub states_pushed: usize,
    pub incoherent_prefixes_pruned: usize,
    pub lossless_fallback_inserted: bool,
    /// Number of corrected passes that were actually attempted by the
    /// one-slot raw-repair API. Ordinary conversion leaves this at zero.
    pub raw_repair_passes: usize,
    /// Number of raw-repair candidates admitted after the source/evidence
    /// gate. Direct candidates are not included.
    pub raw_repair_candidates_added: usize,
    /// Number of candidate objects materialized by corrected passes before
    /// dedupe and evidence filtering. This is the aggregate candidate-budget
    /// consumption, including candidates that were later rejected.
    pub raw_repair_candidates_examined: usize,
    /// Number of plans/candidates rejected by the bounded raw-repair gate.
    pub raw_repair_candidates_rejected: usize,
    /// Aggregate lattice/search consumption across corrected passes only.
    pub raw_repair_lattice_nodes: usize,
    pub raw_repair_search_states: usize,
    /// Whether a validated bounded tail was replayed with the current reading.
    pub cross_commit_bridge_attempted: bool,
    /// Combined candidates inspected before exact surface/right-ID matching.
    pub cross_commit_bridge_candidates_examined: usize,
    /// Ordinary current-only candidates whose cost was improved by the
    /// combined lexical evidence.
    pub cross_commit_bridge_candidates_rescored: usize,
    /// Combined paths backed by a raw dictionary edge spanning the commit.
    pub cross_commit_bridge_spanning_paths: usize,
    /// Combined paths whose raw edges end exactly at the commit, carrying an
    /// alternative typed terminal state together with its retained cost delta.
    pub cross_commit_bridge_frontier_paths: usize,
    pub cross_commit_bridge_lattice_nodes: usize,
    pub cross_commit_bridge_search_states: usize,
    pub cross_commit_bridge_terminal: Option<ConversionSearchTerminal>,
}

/// Candidates and their bounded-search terminal condition.
#[derive(Debug)]
pub struct ConversionResult<'a> {
    pub(super) candidates: &'a [ConversionCandidate],
    pub(super) diagnostics: ConversionDiagnostics,
}

impl<'a> ConversionResult<'a> {
    pub fn candidates(&self) -> &'a [ConversionCandidate] {
        self.candidates
    }

    pub const fn diagnostics(&self) -> ConversionDiagnostics {
        self.diagnostics
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionError {
    EmptyReading,
    ReadingTooLong,
    InvalidOptions,
    Dictionary(crate::dictionary::Error),
    LatticeFull,
    NoPath,
    OutputTooLong,
    TooManySegments,
}

impl core::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyReading => f.write_str("conversion reading is empty"),
            Self::ReadingTooLong => f.write_str("conversion reading exceeds the preedit limit"),
            Self::InvalidOptions => f.write_str("conversion options are outside their bounds"),
            Self::Dictionary(error) => write!(f, "dictionary lookup failed: {error}"),
            Self::LatticeFull => f.write_str("conversion lattice reached its fixed node limit"),
            Self::NoPath => f.write_str("conversion lattice has no complete path"),
            Self::OutputTooLong => f.write_str("converted output exceeds the preedit limit"),
            Self::TooManySegments => f.write_str("converted path exceeds the segment limit"),
        }
    }
}

impl std::error::Error for ConversionError {}
