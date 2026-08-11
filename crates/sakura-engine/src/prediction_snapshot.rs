//! Dormant, hash-only top-32 prediction snapshot for Issue #34.
//!
//! The production prediction worker and its visible nine-candidate result stay
//! unchanged. This module gives offline replay and a future context worker a
//! bounded identity/correlation boundary without retaining candidate text.

use sakura_neural_proto::{
    CandidateAuthority, Fingerprint, MAX_CANDIDATE_SURFACE_BYTES, MAX_PREDICTION_CANDIDATES,
    MAX_READING_BYTES,
};
use sakura_proto::InputScope;

/// Candidate provenance used by the offline source-hit metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SnapshotSource {
    History = 1,
    #[default]
    SystemDictionary = 2,
    UserDictionary = 3,
}

/// Stable dictionary identity when the candidate producer has one.
///
/// Learned-history candidates deliberately use `None`: their durable learning
/// identity is not a dictionary ordinal and must not be guessed from text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionaryIdentity {
    SystemEntry(u32),
    UserEntry(u32),
}

/// Borrowed, transient input used to construct a hash-only snapshot.
///
/// `reading` and `surface` must already be the engine's canonical candidate
/// strings. This boundary performs no NFKC or width conversion and therefore
/// cannot silently disagree with the candidate that would be committed.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotCandidateInput<'a> {
    pub reading: &'a str,
    pub surface: &'a str,
    pub dictionary_identity: Option<DictionaryIdentity>,
    pub base_cost: i32,
    pub authority: CandidateAuthority,
    pub source: SnapshotSource,
    pub right_id: u16,
    pub is_it: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextFingerprint {
    pub hash: u64,
    pub byte_len: u16,
}

/// One candidate in the private, fixed-capacity snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredictionSnapshotCandidate {
    pub candidate_id: u64,
    pub reading: TextFingerprint,
    pub surface: TextFingerprint,
    pub dictionary_identity: Option<DictionaryIdentity>,
    pub base_cost: i32,
    pub authority: CandidateAuthority,
    pub source: SnapshotSource,
    pub right_id: u16,
    pub is_it: bool,
    pub original_index: u8,
}

impl Default for PredictionSnapshotCandidate {
    fn default() -> Self {
        Self {
            candidate_id: 0,
            reading: TextFingerprint::default(),
            surface: TextFingerprint::default(),
            dictionary_identity: None,
            base_cost: 0,
            authority: CandidateAuthority::Ordinary,
            source: SnapshotSource::SystemDictionary,
            right_id: 0,
            is_it: false,
            original_index: 0,
        }
    }
}

/// Exact correlation tuple required before an offline score response is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCorrelation {
    pub session_id: u64,
    pub context_generation: u64,
    pub composition_generation: u64,
    pub candidate_set_fingerprint: Fingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictionSnapshot {
    correlation: SnapshotCorrelation,
    candidates: [PredictionSnapshotCandidate; MAX_PREDICTION_CANDIDATES],
    len: usize,
    offered: usize,
}

impl PredictionSnapshot {
    /// Builds an immutable snapshot without changing the visible prediction
    /// result. Sensitive, unclassified, test-only, oversized, and malformed
    /// inputs fail before a partial snapshot can escape.
    pub fn build(
        session_id: u64,
        context_generation: u64,
        composition_generation: u64,
        scope: InputScope,
        classified: bool,
        test_only: bool,
        inputs: &[SnapshotCandidateInput<'_>],
    ) -> Result<Self, SnapshotError> {
        if test_only {
            return Err(SnapshotError::TestOnly);
        }
        if !classified || scope == InputScope::Unclassified {
            return Err(SnapshotError::UnclassifiedScope);
        }
        if scope != InputScope::Normal {
            return Err(SnapshotError::SensitiveScope);
        }
        if session_id == 0 || context_generation == 0 || composition_generation == 0 {
            return Err(SnapshotError::InvalidCorrelation);
        }
        if inputs.len() > MAX_PREDICTION_CANDIDATES {
            return Err(SnapshotError::TooManyCandidates);
        }

        let mut accepted_inputs = [0u8; MAX_PREDICTION_CANDIDATES];
        let mut candidates = [PredictionSnapshotCandidate::default(); MAX_PREDICTION_CANDIDATES];
        let mut len = 0usize;
        for (input_index, input) in inputs.iter().enumerate() {
            if input.reading.is_empty() || input.surface.is_empty() {
                return Err(SnapshotError::EmptyText);
            }
            if input.reading.len() > MAX_READING_BYTES
                || input.surface.len() > MAX_CANDIDATE_SURFACE_BYTES
            {
                return Err(SnapshotError::TextTooLong);
            }

            let duplicate = accepted_inputs[..len].iter().any(|accepted| {
                let earlier = &inputs[usize::from(*accepted)];
                earlier.reading == input.reading
                    && earlier.surface == input.surface
                    && earlier.dictionary_identity == input.dictionary_identity
            });
            if duplicate {
                continue;
            }

            let original_index =
                u8::try_from(input_index).map_err(|_| SnapshotError::TooManyCandidates)?;
            let candidate = snapshot_candidate(*input, original_index);
            if candidate.candidate_id == 0
                || candidates[..len]
                    .iter()
                    .any(|earlier| earlier.candidate_id == candidate.candidate_id)
            {
                return Err(SnapshotError::CandidateIdCollision);
            }
            accepted_inputs[len] = original_index;
            candidates[len] = candidate;
            len += 1;
        }

        let fingerprint = candidate_set_fingerprint(&candidates[..len]);
        Ok(Self {
            correlation: SnapshotCorrelation {
                session_id,
                context_generation,
                composition_generation,
                candidate_set_fingerprint: fingerprint,
            },
            candidates,
            len,
            offered: inputs.len(),
        })
    }

    pub const fn correlation(&self) -> SnapshotCorrelation {
        self.correlation
    }

    pub fn candidates(&self) -> &[PredictionSnapshotCandidate] {
        &self.candidates[..self.len]
    }

    pub const fn offered_count(&self) -> usize {
        self.offered
    }

    pub const fn duplicate_count(&self) -> usize {
        self.offered - self.len
    }

    pub fn candidate(&self, candidate_id: u64) -> Option<&PredictionSnapshotCandidate> {
        self.candidates()
            .iter()
            .find(|candidate| candidate.candidate_id == candidate_id)
    }

    pub const fn accepts(&self, response: SnapshotCorrelation) -> bool {
        self.correlation.session_id == response.session_id
            && self.correlation.context_generation == response.context_generation
            && self.correlation.composition_generation == response.composition_generation
            && fingerprint_eq(
                &self.correlation.candidate_set_fingerprint,
                &response.candidate_set_fingerprint,
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    SensitiveScope,
    UnclassifiedScope,
    TestOnly,
    InvalidCorrelation,
    TooManyCandidates,
    EmptyText,
    TextTooLong,
    CandidateIdCollision,
}

fn snapshot_candidate(
    input: SnapshotCandidateInput<'_>,
    original_index: u8,
) -> PredictionSnapshotCandidate {
    let reading = text_fingerprint(input.reading);
    let surface = text_fingerprint(input.surface);
    let mut identity = FingerprintState::new();
    identity.bytes(input.reading.as_bytes());
    identity.separator();
    identity.bytes(input.surface.as_bytes());
    identity.separator();
    identity.u8(input.source as u8);
    match input.dictionary_identity {
        None => identity.u8(0),
        Some(DictionaryIdentity::SystemEntry(ordinal)) => {
            identity.u8(1);
            identity.u32(ordinal);
        }
        Some(DictionaryIdentity::UserEntry(ordinal)) => {
            identity.u8(2);
            identity.u32(ordinal);
        }
    }
    let candidate_id = identity.finish_u64();
    PredictionSnapshotCandidate {
        candidate_id,
        reading,
        surface,
        dictionary_identity: input.dictionary_identity,
        base_cost: input.base_cost,
        authority: input.authority,
        source: input.source,
        right_id: input.right_id,
        is_it: input.is_it,
        original_index,
    }
}

fn text_fingerprint(text: &str) -> TextFingerprint {
    let mut state = FingerprintState::new();
    state.bytes(text.as_bytes());
    TextFingerprint {
        hash: state.finish_u64(),
        byte_len: u16::try_from(text.len()).unwrap_or(u16::MAX),
    }
}

fn candidate_set_fingerprint(candidates: &[PredictionSnapshotCandidate]) -> Fingerprint {
    let mut state = FingerprintState::new();
    state.u32(u32::try_from(candidates.len()).unwrap_or(u32::MAX));
    for candidate in candidates {
        state.u64(candidate.candidate_id);
        state.u32(candidate.base_cost as u32);
        state.u8(candidate.authority as u8);
        state.u8(candidate.source as u8);
        state.u16(candidate.right_id);
        state.u8(u8::from(candidate.is_it));
        state.u8(candidate.original_index);
    }
    state.finish()
}

const fn fingerprint_eq(left: &Fingerprint, right: &Fingerprint) -> bool {
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

struct FingerprintState {
    lanes: [u64; 4],
}

impl FingerprintState {
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new() -> Self {
        Self {
            lanes: [
                0xcbf2_9ce4_8422_2325 ^ 0x243f_6a88_85a3_08d3,
                0xcbf2_9ce4_8422_2325 ^ 0x1319_8a2e_0370_7344,
                0xcbf2_9ce4_8422_2325 ^ 0xa409_3822_299f_31d0,
                0xcbf2_9ce4_8422_2325 ^ 0x082e_fa98_ec4e_6c89,
            ],
        }
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            for lane in &mut self.lanes {
                *lane = (*lane ^ u64::from(*byte)).wrapping_mul(Self::PRIME);
            }
        }
    }

    fn separator(&mut self) {
        self.bytes(&[0xff]);
    }

    fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    fn finish(self) -> Fingerprint {
        let mut output = [0u8; 32];
        for (index, lane) in self.lanes.into_iter().enumerate() {
            output[index * 8..(index + 1) * 8].copy_from_slice(&lane.to_le_bytes());
        }
        output
    }

    fn finish_u64(self) -> u64 {
        self.lanes[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordinary<'a>(reading: &'a str, surface: &'a str) -> SnapshotCandidateInput<'a> {
        SnapshotCandidateInput {
            reading,
            surface,
            dictionary_identity: None,
            base_cost: 100,
            authority: CandidateAuthority::Ordinary,
            source: SnapshotSource::SystemDictionary,
            right_id: 1,
            is_it: false,
        }
    }

    fn snapshot(inputs: &[SnapshotCandidateInput<'_>]) -> PredictionSnapshot {
        PredictionSnapshot::build(1, 2, 3, InputScope::Normal, true, false, inputs)
            .expect("valid snapshot")
    }

    #[test]
    fn snapshot_is_bounded_hash_only_and_deduplicates_canonical_identity() {
        let first = ordinary("かな", "仮名");
        let duplicate = ordinary("かな", "仮名");
        let result = snapshot(&[first, duplicate]);
        assert_eq!(result.offered_count(), 2);
        assert_eq!(result.duplicate_count(), 1);
        assert_eq!(result.candidates().len(), 1);
        assert_eq!(result.candidates()[0].reading.byte_len, 6);
        assert_eq!(result.candidates()[0].surface.byte_len, 6);
        assert!(!format!("{result:?}").contains("仮名"));
    }

    #[test]
    fn distinct_dictionary_ordinals_preserve_homographs_and_protected_candidates() {
        let mut system_a = ordinary("こうしょう", "交渉");
        system_a.dictionary_identity = Some(DictionaryIdentity::SystemEntry(10));
        let mut system_b = system_a;
        system_b.dictionary_identity = Some(DictionaryIdentity::SystemEntry(11));
        let mut user = system_a;
        user.dictionary_identity = Some(DictionaryIdentity::UserEntry(0));
        user.authority = CandidateAuthority::UserDictionary;
        user.source = SnapshotSource::UserDictionary;

        let result = snapshot(&[system_a, system_b, user]);
        assert_eq!(result.candidates().len(), 3);
        assert_eq!(
            result.candidates()[2].authority,
            CandidateAuthority::UserDictionary
        );
    }

    #[test]
    fn privacy_and_capacity_fail_closed() {
        let one = ordinary("a", "A");
        assert_eq!(
            PredictionSnapshot::build(1, 1, 1, InputScope::Password, true, false, &[one]),
            Err(SnapshotError::SensitiveScope)
        );
        assert_eq!(
            PredictionSnapshot::build(1, 1, 1, InputScope::Normal, false, false, &[one]),
            Err(SnapshotError::UnclassifiedScope)
        );
        assert_eq!(
            PredictionSnapshot::build(1, 1, 1, InputScope::Normal, true, true, &[one]),
            Err(SnapshotError::TestOnly)
        );
        let too_many = [one; MAX_PREDICTION_CANDIDATES + 1];
        assert_eq!(
            PredictionSnapshot::build(1, 1, 1, InputScope::Normal, true, false, &too_many),
            Err(SnapshotError::TooManyCandidates)
        );
        let long_reading = "a".repeat(MAX_READING_BYTES + 1);
        let too_long = ordinary(&long_reading, "A");
        assert_eq!(
            PredictionSnapshot::build(1, 1, 1, InputScope::Normal, true, false, &[too_long]),
            Err(SnapshotError::TextTooLong)
        );
    }

    #[test]
    fn exact_correlation_rejects_each_stale_dimension() {
        let result = snapshot(&[ordinary("かな", "仮名")]);
        let current = result.correlation();
        assert!(result.accepts(current));

        let stale = [
            SnapshotCorrelation {
                session_id: 9,
                ..current
            },
            SnapshotCorrelation {
                context_generation: 9,
                ..current
            },
            SnapshotCorrelation {
                composition_generation: 9,
                ..current
            },
            SnapshotCorrelation {
                candidate_set_fingerprint: [9; 32],
                ..current
            },
        ];
        assert!(stale
            .into_iter()
            .all(|correlation| !result.accepts(correlation)));
    }

    #[test]
    fn candidate_set_fingerprint_is_order_and_cost_sensitive() {
        let a = ordinary("a", "A");
        let mut b = ordinary("b", "B");
        let forward = snapshot(&[a, b]);
        let reverse = snapshot(&[b, a]);
        assert_ne!(forward.correlation(), reverse.correlation());
        b.base_cost += 1;
        let changed = snapshot(&[a, b]);
        assert_ne!(forward.correlation(), changed.correlation());
    }

    #[test]
    fn internal_pool_does_not_expand_the_visible_prediction_page() {
        assert_eq!(MAX_PREDICTION_CANDIDATES, 32);
        assert_eq!(crate::prediction::MAX_SUGGESTIONS, 9);
    }
}
