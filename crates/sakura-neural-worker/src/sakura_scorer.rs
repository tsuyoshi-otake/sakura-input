//! Protocol-v1 adapter for the listwise Sakura-Rerank-Tiny-v1 model.
//!
//! Model contract v2 gives each candidate `FEATURE_DIM` (7) features: the
//! normalized local cost, the candidate's position, the surface-length ratio,
//! and the log1p-scaled local cost gap over `COST_GAP_SCALE` (sakura-rerank
//! Issue #29 distilled export). Feature slots 2, 4 and 5 stay zero, and the
//! unused context and reading tensors stay empty as in contract v1.

use std::{
    fmt,
    time::{Duration, Instant},
};

use crate::{
    protocol::Request,
    sakura_runtime::{self, Inputs, ModelRuntime},
};

pub(crate) const REQUEST_DEADLINE: Duration = Duration::from_millis(400);
const VOCAB_SIZE: u32 = 13_312;
const CHARACTER_MULTIPLIER: u32 = 2_654_435_761;
/// The divisor of the per-candidate local cost gap before `log1p`, matching
/// the research export's `-np.log1p((costs - costs.min()) / 1000.0)`.
const COST_GAP_SCALE: f64 = 1000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    InvalidCandidates,
    DeadlineExceeded,
    Backend,
    OutputContract,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidCandidates => "Sakura candidate batch is outside the model contract",
            Self::DeadlineExceeded => "Sakura neural scoring deadline expired",
            Self::Backend => "Sakura ONNX inference failed",
            Self::OutputContract => "Sakura ONNX scores do not match the request",
        })
    }
}

impl std::error::Error for Error {}

pub(crate) trait ScoreBackend {
    fn scores(&self, inputs: &Inputs, deadline: Instant) -> Result<Vec<f32>, Error>;
}

impl ScoreBackend for ModelRuntime {
    fn scores(&self, inputs: &Inputs, deadline: Instant) -> Result<Vec<f32>, Error> {
        ModelRuntime::scores(self, inputs, deadline).map_err(|_| Error::Backend)
    }
}

pub(crate) fn score(
    request: &Request,
    backend: &impl ScoreBackend,
    deadline: Instant,
) -> Result<Vec<(u64, f32)>, Error> {
    if Instant::now() >= deadline {
        return Err(Error::DeadlineExceeded);
    }
    let inputs = prepare_inputs(request)?;
    let scores = backend.scores(&inputs, deadline)?;
    if scores.len() != sakura_runtime::TOP_K || scores.iter().any(|score| !score.is_finite()) {
        return Err(Error::OutputContract);
    }
    Ok(request
        .candidates
        .iter()
        .zip(scores)
        .map(|(candidate, score)| (candidate.fingerprint, score))
        .collect())
}

fn prepare_inputs(request: &Request) -> Result<Inputs, Error> {
    let count = request.candidates.len();
    if !(2..=sakura_runtime::TOP_K).contains(&count) {
        return Err(Error::InvalidCandidates);
    }
    let minimum = request
        .candidates
        .iter()
        .map(|candidate| i64::from(candidate.local_cost))
        .min()
        .ok_or(Error::InvalidCandidates)?;
    let maximum = request
        .candidates
        .iter()
        .map(|candidate| i64::from(candidate.local_cost))
        .max()
        .ok_or(Error::InvalidCandidates)?;
    let span = (maximum - minimum).max(1) as f64;

    let mut candidate_ids = vec![0; sakura_runtime::TOP_K * sakura_runtime::SURFACE_LENGTH];
    let mut candidate_lengths = vec![0; sakura_runtime::TOP_K];
    let mut features = vec![0.0; sakura_runtime::TOP_K * sakura_runtime::FEATURE_DIM];
    let mut candidate_mask = vec![false; sakura_runtime::TOP_K];
    for (index, candidate) in request.candidates.iter().enumerate() {
        let characters: Vec<_> = candidate.text.chars().collect();
        let length = characters.len().min(sakura_runtime::SURFACE_LENGTH);
        candidate_lengths[index] = length as i64;
        candidate_mask[index] = true;
        let ids = &mut candidate_ids
            [index * sakura_runtime::SURFACE_LENGTH..(index + 1) * sakura_runtime::SURFACE_LENGTH];
        for (target, character) in ids.iter_mut().zip(characters.iter().take(length)) {
            *target = i64::from(character_id(*character));
        }
        let feature = &mut features
            [index * sakura_runtime::FEATURE_DIM..(index + 1) * sakura_runtime::FEATURE_DIM];
        feature[0] = (-((i64::from(candidate.local_cost) - minimum) as f64) / span) as f32;
        // The research export negates the integer index (`-index /
        // max(TOP_K - 1, 1)`), so candidate 0 gets +0.0, not -0.0.
        feature[1] = (-(index as i64) as f32) / ((sakura_runtime::TOP_K - 1) as f32);
        feature[3] = characters.len() as f32 / sakura_runtime::SURFACE_LENGTH as f32;
        feature[6] =
            (-((i64::from(candidate.local_cost) - minimum) as f64 / COST_GAP_SCALE).ln_1p()) as f32;
    }
    Ok(Inputs {
        context_ids: vec![0; sakura_runtime::CONTEXT_LENGTH],
        context_lengths: vec![0],
        reading_ids: vec![0; sakura_runtime::READING_LENGTH],
        reading_lengths: vec![0],
        candidate_ids,
        candidate_lengths,
        features,
        candidate_mask,
    })
}

fn character_id(character: char) -> u32 {
    let product = u64::from(u32::from(character)) * u64::from(CHARACTER_MULTIPLIER);
    ((product % u64::from(VOCAB_SIZE - 1)) + 1) as u32
}

pub fn self_test() -> Result<(), Error> {
    let request = Request {
        id: 1,
        candidates: vec![
            crate::protocol::Candidate {
                fingerprint: 1,
                local_cost: 10,
                text: "A".to_owned(),
            },
            crate::protocol::Candidate {
                fingerprint: 2,
                local_cost: 20,
                text: "B".to_owned(),
            },
        ],
    };
    prepare_inputs(&request).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        inputs: RefCell<Option<Inputs>>,
        output: Vec<f32>,
    }

    impl ScoreBackend for Fake {
        fn scores(&self, inputs: &Inputs, _deadline: Instant) -> Result<Vec<f32>, Error> {
            *self.inputs.borrow_mut() = Some(inputs.clone());
            Ok(self.output.clone())
        }
    }

    fn request() -> Request {
        Request {
            id: 7,
            candidates: vec![
                crate::protocol::Candidate {
                    fingerprint: 11,
                    local_cost: i32::MIN,
                    text: "東京".to_owned(),
                },
                crate::protocol::Candidate {
                    fingerprint: 12,
                    local_cost: i32::MAX,
                    text: "A😀".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn builds_protocol_v1_inputs_and_preserves_order() {
        let fake = Fake {
            inputs: RefCell::new(None),
            output: vec![0.25, 0.75, -10_000.0, -10_000.0, -10_000.0, -10_000.0],
        };
        let scores = score(&request(), &fake, Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(scores, vec![(11, 0.25), (12, 0.75)]);
        let inputs = fake.inputs.borrow();
        let inputs = inputs.as_ref().unwrap();
        assert_eq!(inputs.context_lengths, vec![0]);
        assert_eq!(inputs.reading_lengths, vec![0]);
        assert_eq!(inputs.candidate_lengths[..2], [2, 2]);
        assert_eq!(
            inputs.candidate_mask,
            vec![true, true, false, false, false, false]
        );
        assert_eq!(inputs.features[0], 0.0);
        assert_eq!(inputs.features[sakura_runtime::FEATURE_DIM], -1.0);
        assert_eq!(inputs.features[sakura_runtime::FEATURE_DIM + 1], -0.2);
        assert_eq!(inputs.features[3], 2.0 / 32.0);
        // Contract v2 cost-gap feature: the cheapest candidate logs -0.0,
        // the costliest logs -ln1p(gap / COST_GAP_SCALE), both in f64 first.
        assert_eq!(inputs.features[6].to_bits(), (-0.0f32).to_bits());
        assert_eq!(inputs.features[2], 0.0);
        assert_eq!(inputs.features[4], 0.0);
        assert_eq!(inputs.features[5], 0.0);
        assert_eq!(
            inputs.features[sakura_runtime::FEATURE_DIM + 6],
            (-((i64::from(i32::MAX) - i64::from(i32::MIN)) as f64 / COST_GAP_SCALE).ln_1p()) as f32
        );
        assert_eq!(inputs.candidate_ids[0], i64::from(character_id('東')));
        assert_eq!(
            inputs.candidate_ids[sakura_runtime::SURFACE_LENGTH + 1],
            i64::from(character_id('😀'))
        );
    }

    #[test]
    fn bounds_deadline_and_output_contract_fail_closed() {
        let one = Request {
            id: 1,
            candidates: vec![request().candidates[0].clone()],
        };
        assert_eq!(prepare_inputs(&one), Err(Error::InvalidCandidates));
        let fake = Fake {
            inputs: RefCell::new(None),
            output: vec![0.0; 5],
        };
        assert_eq!(
            score(&request(), &fake, Instant::now() + Duration::from_secs(1)),
            Err(Error::OutputContract)
        );
        assert_eq!(
            score(&request(), &fake, Instant::now() - Duration::from_millis(1)),
            Err(Error::DeadlineExceeded)
        );
    }

    #[test]
    fn issue440_parity_fixture_rebuilds_model_inputs_bit_exactly() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/issue440-parity.json"))
                .expect("issue440 parity fixture");
        assert_eq!(
            fixture["gate"]["score"].as_str().unwrap(),
            sakura_rerank_proto::Selection::RULE
        );
        assert_eq!(
            fixture["gate"]["tau"].as_f64().unwrap(),
            sakura_rerank_proto::Selection::LOCAL_OVERRIDE_MARGIN
        );
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len() as u64, fixture["case_count"].as_u64().unwrap());
        for (index, case) in cases.iter().enumerate() {
            let candidates: Vec<crate::protocol::Candidate> = case["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|candidate| crate::protocol::Candidate {
                    fingerprint: candidate["fingerprint"].as_u64().unwrap(),
                    local_cost: i32::try_from(candidate["local_cost"].as_i64().unwrap()).unwrap(),
                    text: candidate["text"].as_str().unwrap().to_owned(),
                })
                .collect();
            let count = candidates.len();
            let inputs = prepare_inputs(&Request { id: 1, candidates })
                .unwrap_or_else(|error| panic!("case {index}: {error}"));
            let expected_bits: Vec<u32> = case["features_f32_bits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_u64().unwrap() as u32)
                .collect();
            assert_eq!(expected_bits.len(), 42, "case {index}");
            let actual_bits: Vec<u32> = inputs
                .features
                .iter()
                .map(|value| value.to_bits())
                .collect();
            assert_eq!(actual_bits, expected_bits, "case {index} features");
            let expected_ids: Vec<i64> = case["candidate_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_i64().unwrap())
                .collect();
            assert_eq!(expected_ids.len(), 192, "case {index}");
            assert_eq!(inputs.candidate_ids, expected_ids, "case {index}");
            let expected_lengths: Vec<i64> = case["candidate_lengths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_i64().unwrap())
                .collect();
            assert_eq!(expected_lengths.len(), 6, "case {index}");
            assert_eq!(inputs.candidate_lengths, expected_lengths, "case {index}");
            let mut expected_mask = vec![false; sakura_runtime::TOP_K];
            expected_mask[..count].fill(true);
            assert_eq!(inputs.candidate_mask, expected_mask, "case {index}");
            assert_eq!(inputs.context_lengths, vec![0], "case {index}");
            assert_eq!(inputs.reading_lengths, vec![0], "case {index}");
        }
    }

    #[test]
    fn model_free_self_test_passes() {
        self_test().unwrap();
    }

    #[test]
    fn character_hash_matches_unbounded_export_preprocessing() {
        assert_eq!(character_id('A'), 11_031);
        assert_eq!(character_id('東'), 7_816);
        assert_eq!(character_id('😀'), 2_033);
    }
}
