//! Protocol-v1 adapter for the listwise Sakura-Rerank-Tiny-v1 model.

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
        feature[1] = -(index as f32) / ((sakura_runtime::TOP_K - 1) as f32);
        feature[3] = characters.len() as f32 / sakura_runtime::SURFACE_LENGTH as f32;
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
