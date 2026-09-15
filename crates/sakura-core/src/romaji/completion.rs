//! Local completions: the table entries that could finish the romaji a
//! user has typed so far.

use sakura_values::FixedStr;

use super::{
    ReplayError, Table, MAX_LOCAL_COMPLETIONS, MAX_REPLAY_OUTPUT_BYTES, MAX_REPLAY_RAW_BYTES,
};

/// One table-derived ASCII key insertion at a verified anomaly boundary.
///
/// The corrected reading is retained with the plan so the caller does not
/// need to guess (or consult a dictionary) which result a key insertion
/// produced.  It is deliberately owned and bounded; a completion plan is
/// scratch data, not a wire/history field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCompletion {
    /// Byte offset in the original raw input at which `key` is inserted.
    pub insertion_at: u16,
    /// The single ASCII key inserted at `insertion_at`.
    pub key: u8,
    /// Source span of the raw passthrough or unresolved prefix that licensed
    /// this completion.
    pub anomaly_start: u16,
    pub anomaly_end: u16,
    /// Corrected reading emitted by replaying the raw input with `key`
    /// inserted at `insertion_at`.
    pub corrected_reading: FixedStr<MAX_REPLAY_OUTPUT_BYTES>,
}

/// Bounded table-derived local completion results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCompletionList {
    completions: [Option<LocalCompletion>; MAX_LOCAL_COMPLETIONS],
    len: usize,
}

impl LocalCompletionList {
    fn new() -> Self {
        Self {
            completions: [const { None }; MAX_LOCAL_COMPLETIONS],
            len: 0,
        }
    }

    /// Number of completion keys found.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no table-derived key completed the observed reading.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Completion plans in deterministic ASCII order.
    pub fn iter(&self) -> impl Iterator<Item = &LocalCompletion> {
        self.completions[..self.len]
            .iter()
            .filter_map(Option::as_ref)
    }

    /// Returns the completion at `index`, if one was retained.
    pub fn get(&self, index: usize) -> Option<&LocalCompletion> {
        self.completions.get(index).and_then(Option::as_ref)
    }

    fn push(&mut self, completion: LocalCompletion) -> bool {
        if self.len >= MAX_LOCAL_COMPLETIONS {
            return false;
        }
        self.completions[self.len] = Some(completion);
        self.len += 1;
        true
    }
}

/// Why a local-completion plan could not be checked against its observed
/// reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalCompletionError {
    /// The raw input could not be replayed under the bounded contract.
    Replay(ReplayError),
    /// The caller's preedit snapshot does not match the actual FSM replay.
    ObservedMismatch,
}

impl core::fmt::Display for LocalCompletionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Replay(error) => error.fmt(f),
            Self::ObservedMismatch => f.write_str("raw replay does not match observed preedit"),
        }
    }
}

impl std::error::Error for LocalCompletionError {}

impl Table {
    /// Validates a raw/preedit snapshot and returns bounded table-derived
    /// one-key local completion proposals.
    ///
    /// `observed` must be the live preedit produced by `raw`; this check is
    /// the provenance boundary and fails closed on any mismatch.  A Phase 1
    /// proposal is licensed only by exactly one raw passthrough event.  Every
    /// canonical ASCII key is then tried at that one boundary and retained
    /// only when the corrected replay has no raw passthrough and no pending
    /// prefix.  The corrected reading is carried by each returned plan, so
    /// this method does not need a dictionary target or a guessed reading.
    ///
    /// The replay trace still exposes unresolved pending prefixes to callers,
    /// but this admission path deliberately does not turn an ordinary
    /// `n`/`k` prefix into a repair candidate.
    pub fn plan_local_completions(
        &self,
        raw: &str,
        observed: &str,
    ) -> Result<LocalCompletionList, LocalCompletionError> {
        let trace = self.replay(raw).map_err(LocalCompletionError::Replay)?;
        if trace.output() != observed {
            return Err(LocalCompletionError::ObservedMismatch);
        }
        let mut plans = LocalCompletionList::new();
        if trace.raw_passthrough_count() != 1 || !trace.pending().is_empty() {
            return Ok(plans);
        }
        let anomaly = match trace.raw_passthrough().next() {
            Some(event) => *event,
            None => return Ok(plans),
        };

        // The local completion is inserted immediately after the raw
        // passthrough.  This is the only Phase 1 boundary; general insertion
        // at every position belongs to Issue #77 and is intentionally absent.
        let insertion_at = anomaly.raw_end();
        if raw.len() >= MAX_REPLAY_RAW_BYTES {
            return Ok(plans);
        }
        for key in 0x20u8..=0x7eu8 {
            // `Table::replay` applies the same ASCII case fold as live input;
            // retain one canonical key per folded value instead of returning
            // an uppercase duplicate for every lowercase completion.
            if key.is_ascii_uppercase() {
                continue;
            }
            let mut candidate_raw = FixedStr::<MAX_REPLAY_RAW_BYTES>::new();
            if candidate_raw.push_str(&raw[..insertion_at]).is_err()
                || candidate_raw.push(key as char).is_err()
                || candidate_raw.push_str(&raw[insertion_at..]).is_err()
            {
                return Ok(plans);
            }
            let candidate_trace = match self.replay(candidate_raw.as_str()) {
                Ok(candidate_trace) => candidate_trace,
                Err(_) => continue,
            };
            if candidate_trace.raw_passthrough_count() != 0 || !candidate_trace.pending().is_empty()
            {
                continue;
            }
            let proposal = LocalCompletion {
                insertion_at: u16::try_from(insertion_at).unwrap_or(u16::MAX),
                key,
                anomaly_start: u16::try_from(anomaly.raw_start()).unwrap_or(u16::MAX),
                anomaly_end: u16::try_from(anomaly.raw_end()).unwrap_or(u16::MAX),
                corrected_reading: candidate_trace.output.clone(),
            };
            if !plans.push(proposal) {
                break;
            }
        }
        Ok(plans)
    }
}
