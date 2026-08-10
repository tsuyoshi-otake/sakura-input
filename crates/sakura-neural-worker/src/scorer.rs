//! Conditional pseudo-log-likelihood scoring for the isolated neural worker.
//!
//! Candidate generation and the existing conversion cost remain in the engine.
//! This module only produces bounded neural evidence in the request's original
//! fingerprint/order, so the engine can combine it with its local cost.

use std::{
    fmt,
    time::{Duration, Instant},
};

use crate::{
    protocol::Request,
    runtime::{self, Logits, ModelRuntime},
    simd::Dispatch,
    tokenizer::{Tokenizer, MAX_TOKEN_COUNT},
};

pub(crate) const REQUEST_DEADLINE: Duration = Duration::from_millis(400);
// Bound total inference work independently of the wall-clock deadline. At most
// eight ORT calls are admitted even for adversarially different candidates.
const MAX_MASK_ROWS: usize = runtime::MAX_BATCH * 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    DeadlineExceeded,
    Tokenization,
    UnknownToken,
    NoDifferingTokens,
    MissingCandidateScore,
    BoundExceeded,
    Backend,
    OutputContract,
    NonFinite,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DeadlineExceeded => "neural scoring deadline expired",
            Self::Tokenization => "candidate tokenization failed",
            Self::UnknownToken => "candidate contains a token outside model vocabulary",
            Self::NoDifferingTokens => "candidates have no differing token positions",
            Self::MissingCandidateScore => "candidate has no conditional score",
            Self::BoundExceeded => "neural scoring bounds exceeded",
            Self::Backend => "ONNX Runtime neural scoring failed",
            Self::OutputContract => "ONNX Runtime logits do not match scoring contract",
            Self::NonFinite => "neural scoring produced a non-finite value",
        })
    }
}

impl std::error::Error for Error {}

/// Minimal runtime boundary.  Tests use a deterministic fake; production uses
/// the single process-lifetime `ModelRuntime` session.
pub(crate) trait LogitBackend {
    fn requires_token_type_ids(&self) -> bool;
    fn logits(
        &self,
        batch: usize,
        sequence: usize,
        input_ids: &[i64],
        attention_mask: &[i64],
        token_type_ids: Option<&[i64]>,
        deadline: Instant,
    ) -> Result<Logits, Error>;
}

impl LogitBackend for ModelRuntime {
    fn requires_token_type_ids(&self) -> bool {
        self.requires_token_type_ids()
    }

    fn logits(
        &self,
        batch: usize,
        sequence: usize,
        input_ids: &[i64],
        attention_mask: &[i64],
        token_type_ids: Option<&[i64]>,
        deadline: Instant,
    ) -> Result<Logits, Error> {
        ModelRuntime::logits(
            self,
            batch,
            sequence,
            input_ids,
            attention_mask,
            token_type_ids,
            deadline,
        )
        .map_err(|_| Error::Backend)
    }
}

#[derive(Clone)]
struct MaskTask {
    candidate: usize,
    expected: i64,
    mask_position: usize,
    tokens: Vec<i64>,
}

struct Chunk {
    sequence: usize,
    input_ids: Vec<i64>,
    attention_mask: Vec<i64>,
    token_type_ids: Option<Vec<i64>>,
    tasks: Vec<MaskTask>,
}

/// Scores every candidate only at its differentiating body-token positions.
pub(crate) fn score(
    request: &Request,
    tokenizer: &Tokenizer,
    dispatch: &Dispatch,
    backend: &impl LogitBackend,
    deadline: Instant,
) -> Result<Vec<(u64, f32)>, Error> {
    check_deadline(deadline)?;
    let tasks = plan(request, tokenizer)?;
    let mut totals = vec![0.0f64; request.candidates.len()];
    let mut observations = vec![0usize; request.candidates.len()];

    for task_chunk in tasks.chunks(runtime::MAX_BATCH) {
        check_deadline(deadline)?;
        let chunk = build_chunk(task_chunk, tokenizer, backend.requires_token_type_ids())?;
        let logits = backend.logits(
            chunk.tasks.len(),
            chunk.sequence,
            &chunk.input_ids,
            &chunk.attention_mask,
            chunk.token_type_ids.as_deref(),
            deadline,
        )?;
        check_deadline(deadline)?;
        accumulate(&mut totals, &mut observations, &chunk, &logits, dispatch)?;
    }

    let mut scores = Vec::with_capacity(request.candidates.len());
    for (index, candidate) in request.candidates.iter().enumerate() {
        if observations[index] == 0 {
            return Err(Error::MissingCandidateScore);
        }
        let score = totals[index] as f32;
        if !score.is_finite() {
            return Err(Error::NonFinite);
        }
        scores.push((candidate.fingerprint, score));
    }
    Ok(scores)
}

fn plan(request: &Request, tokenizer: &Tokenizer) -> Result<Vec<MaskTask>, Error> {
    if request.candidates.is_empty() || request.candidates.len() > runtime::MAX_BATCH {
        return Err(Error::BoundExceeded);
    }
    let mut bodies = Vec::with_capacity(request.candidates.len());
    let mut full_tokens = Vec::with_capacity(request.candidates.len());
    for candidate in &request.candidates {
        let encoded = tokenizer
            .encode(&candidate.text, MAX_TOKEN_COUNT)
            .map_err(|_| Error::Tokenization)?;
        if encoded.len() < 3 || encoded.iter().any(|id| *id == tokenizer.unk()) {
            return Err(Error::UnknownToken);
        }
        bodies.push(encoded[1..encoded.len() - 1].to_vec());
        full_tokens.push(encoded);
    }
    let longest = bodies
        .iter()
        .map(Vec::len)
        .max()
        .ok_or(Error::BoundExceeded)?;
    let mut tasks = Vec::new();
    for position in 0..longest {
        let first = bodies.first().and_then(|body| body.get(position));
        let differs = bodies
            .iter()
            .skip(1)
            .any(|body| body.get(position) != first);
        if !differs {
            continue;
        }
        for (candidate, body) in bodies.iter().enumerate() {
            if let Some(expected) = body.get(position) {
                if tasks.len() == MAX_MASK_ROWS {
                    return Err(Error::BoundExceeded);
                }
                tasks.push(MaskTask {
                    candidate,
                    expected: *expected,
                    // The body starts after the `[CLS]` token.
                    mask_position: position + 1,
                    tokens: full_tokens[candidate].clone(),
                });
            }
        }
    }
    if tasks.is_empty() {
        return Err(Error::NoDifferingTokens);
    }
    Ok(tasks)
}

fn build_chunk(
    tasks: &[MaskTask],
    tokenizer: &Tokenizer,
    needs_token_types: bool,
) -> Result<Chunk, Error> {
    if tasks.is_empty() || tasks.len() > runtime::MAX_BATCH {
        return Err(Error::BoundExceeded);
    }
    let sequence = tasks
        .iter()
        .map(|task| task.tokens.len())
        .max()
        .ok_or(Error::BoundExceeded)?;
    if !(3..=runtime::MAX_SEQUENCE).contains(&sequence) {
        return Err(Error::BoundExceeded);
    }
    let size = tasks
        .len()
        .checked_mul(sequence)
        .ok_or(Error::BoundExceeded)?;
    let mut input_ids = Vec::with_capacity(size);
    let mut attention_mask = Vec::with_capacity(size);
    let mut token_type_ids = needs_token_types.then(|| Vec::with_capacity(size));
    for task in tasks {
        if task.mask_position == 0 || task.mask_position + 1 >= task.tokens.len() {
            return Err(Error::BoundExceeded);
        }
        let mut row = task.tokens.clone();
        row[task.mask_position] = tokenizer.mask();
        input_ids.extend_from_slice(&row);
        input_ids.resize(input_ids.len() + sequence - row.len(), tokenizer.pad());
        attention_mask.extend(std::iter::repeat_n(1i64, row.len()));
        attention_mask.extend(std::iter::repeat_n(0i64, sequence - row.len()));
        if let Some(types) = &mut token_type_ids {
            types.extend(std::iter::repeat_n(0i64, sequence));
        }
    }
    Ok(Chunk {
        sequence,
        input_ids,
        attention_mask,
        token_type_ids,
        tasks: tasks.to_vec(),
    })
}

fn accumulate(
    totals: &mut [f64],
    observations: &mut [usize],
    chunk: &Chunk,
    logits: &Logits,
    dispatch: &Dispatch,
) -> Result<(), Error> {
    if logits.batch != chunk.tasks.len()
        || logits.sequence != chunk.sequence
        || logits.vocabulary == 0
        || logits.vocabulary > runtime::MAX_VOCABULARY
    {
        return Err(Error::OutputContract);
    }
    let row_width = chunk
        .sequence
        .checked_mul(logits.vocabulary)
        .ok_or(Error::OutputContract)?;
    let expected_values = logits
        .batch
        .checked_mul(row_width)
        .ok_or(Error::OutputContract)?;
    if logits.values.len() != expected_values {
        return Err(Error::OutputContract);
    }
    for (row, task) in chunk.tasks.iter().enumerate() {
        let expected = usize::try_from(task.expected).map_err(|_| Error::OutputContract)?;
        if expected >= logits.vocabulary {
            return Err(Error::OutputContract);
        }
        let start = row
            .checked_mul(row_width)
            .and_then(|offset| {
                offset.checked_add(task.mask_position.checked_mul(logits.vocabulary)?)
            })
            .ok_or(Error::OutputContract)?;
        let values = logits
            .values
            .get(start..start + logits.vocabulary)
            .ok_or(Error::OutputContract)?;
        let summary = dispatch.summarize(values).map_err(|_| Error::NonFinite)?;
        let pll = values[expected] - summary.log_sum_exp;
        if !pll.is_finite() || !totals[task.candidate].is_finite() {
            return Err(Error::NonFinite);
        }
        totals[task.candidate] += f64::from(pll);
        observations[task.candidate] = observations[task.candidate]
            .checked_add(1)
            .ok_or(Error::BoundExceeded)?;
    }
    Ok(())
}

fn check_deadline(deadline: Instant) -> Result<(), Error> {
    (Instant::now() <= deadline)
        .then_some(())
        .ok_or(Error::DeadlineExceeded)
}

/// A model-free scorer fixture for `--self-test`.
pub(crate) fn self_test() -> Result<(), Error> {
    let tokenizer = Tokenizer::from_vocab("[PAD]\n[CLS]\n[SEP]\n[UNK]\n[MASK]\nA\nB\nC\n")
        .map_err(|_| Error::Tokenization)?;
    let request = Request {
        id: 1,
        candidates: vec![
            crate::protocol::Candidate {
                fingerprint: 3,
                text: "AB".to_owned(),
            },
            crate::protocol::Candidate {
                fingerprint: 4,
                text: "AC".to_owned(),
            },
        ],
    };
    let dispatch = Dispatch::force_for_self_test("scalar").map_err(|_| Error::Backend)?;
    let result = score(
        &request,
        &tokenizer,
        &dispatch,
        &DeterministicBackend,
        Instant::now() + REQUEST_DEADLINE,
    )?;
    if result.len() != 2 || result[0].0 != 3 || result[1].1 <= result[0].1 {
        return Err(Error::OutputContract);
    }
    Ok(())
}

struct DeterministicBackend;

impl LogitBackend for DeterministicBackend {
    fn requires_token_type_ids(&self) -> bool {
        false
    }

    fn logits(
        &self,
        batch: usize,
        sequence: usize,
        _: &[i64],
        _: &[i64],
        _: Option<&[i64]>,
        _: Instant,
    ) -> Result<Logits, Error> {
        let vocabulary = 8;
        let values = (0..batch * sequence * vocabulary)
            .map(|index| (index % vocabulary) as f32)
            .collect();
        Ok(Logits {
            batch,
            sequence,
            vocabulary,
            values,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, time::Duration};

    #[derive(Clone, Debug)]
    struct Call {
        batch: usize,
        sequence: usize,
        ids: Vec<i64>,
        attention: Vec<i64>,
        types: Option<Vec<i64>>,
    }
    struct Fake {
        vocabulary: usize,
        calls: RefCell<Vec<Call>>,
        nonfinite: bool,
        types: bool,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                vocabulary: 16,
                calls: RefCell::new(Vec::new()),
                nonfinite: false,
                types: false,
            }
        }
    }
    impl LogitBackend for Fake {
        fn requires_token_type_ids(&self) -> bool {
            self.types
        }
        fn logits(
            &self,
            batch: usize,
            sequence: usize,
            ids: &[i64],
            attention: &[i64],
            types: Option<&[i64]>,
            _: Instant,
        ) -> Result<Logits, Error> {
            self.calls.borrow_mut().push(Call {
                batch,
                sequence,
                ids: ids.to_vec(),
                attention: attention.to_vec(),
                types: types.map(ToOwned::to_owned),
            });
            let mut values: Vec<f32> = (0..batch * sequence * self.vocabulary)
                .map(|i| (i % self.vocabulary) as f32)
                .collect();
            if self.nonfinite {
                values.fill(f32::NAN);
            }
            Ok(Logits {
                batch,
                sequence,
                vocabulary: self.vocabulary,
                values,
            })
        }
    }
    fn tokenizer() -> Tokenizer {
        Tokenizer::from_vocab("[PAD]\n[CLS]\n[SEP]\n[UNK]\n[MASK]\nA\nB\nC\nD\nE\nF\nG\nH\nI\n")
            .unwrap()
    }
    fn request(text: &[&str]) -> Request {
        Request {
            id: 9,
            candidates: text
                .iter()
                .enumerate()
                .map(|(i, text)| crate::protocol::Candidate {
                    fingerprint: (i + 10) as u64,
                    text: (*text).to_owned(),
                })
                .collect(),
        }
    }
    fn dispatch() -> Dispatch {
        Dispatch::force_for_self_test("scalar").unwrap()
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(1)
    }

    #[test]
    fn identical_positions_are_omitted_and_order_is_preserved() {
        let fake = Fake::new();
        let result = score(
            &request(&["AB", "AC"]),
            &tokenizer(),
            &dispatch(),
            &fake,
            deadline(),
        )
        .unwrap();
        assert_eq!(
            result.iter().map(|score| score.0).collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert!(result[1].1 > result[0].1);
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].batch, 2);
        assert_eq!(calls[0].sequence, 4);
        assert_eq!(calls[0].ids, vec![1, 5, 4, 2, 1, 5, 4, 2]);
        assert_eq!(calls[0].attention, vec![1; 8]);
    }

    #[test]
    fn chunks_mask_rows_to_six_and_uses_token_types_when_required() {
        let mut fake = Fake::new();
        fake.types = true;
        let _ = score(
            &request(&["ABCD", "EFGH"]),
            &tokenizer(),
            &dispatch(),
            &fake,
            deadline(),
        )
        .unwrap();
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].batch, 6);
        assert_eq!(calls[1].batch, 2);
        assert!(calls.iter().all(|call| call.batch <= runtime::MAX_BATCH
            && call
                .types
                .as_ref()
                .is_some_and(|types| types.iter().all(|id| *id == 0))));
    }

    #[test]
    fn total_mask_work_is_bounded_before_inference() {
        let fake = Fake::new();
        assert_eq!(
            score(
                &request(&[
                    "ABCDEFGHI",
                    "BCDEFGHIA",
                    "CDEFGHIAB",
                    "DEFGHIABC",
                    "EFGHIABCD",
                    "FGHIABCDE",
                ]),
                &tokenizer(),
                &dispatch(),
                &fake,
                deadline(),
            ),
            Err(Error::BoundExceeded)
        );
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn different_lengths_fail_when_a_candidate_has_no_masked_token() {
        assert_eq!(
            score(
                &request(&["AB", "ABC"]),
                &tokenizer(),
                &dispatch(),
                &Fake::new(),
                deadline()
            ),
            Err(Error::MissingCandidateScore)
        );
    }

    #[test]
    fn unknown_tokens_deadlines_nonfinite_and_bad_vocab_fail_closed() {
        assert_eq!(
            score(
                &request(&["A🌸", "AB"]),
                &tokenizer(),
                &dispatch(),
                &Fake::new(),
                deadline()
            ),
            Err(Error::UnknownToken)
        );
        assert_eq!(
            score(
                &request(&["AB", "AC"]),
                &tokenizer(),
                &dispatch(),
                &Fake::new(),
                Instant::now() - Duration::from_millis(1)
            ),
            Err(Error::DeadlineExceeded)
        );
        let mut nonfinite = Fake::new();
        nonfinite.nonfinite = true;
        assert_eq!(
            score(
                &request(&["AB", "AC"]),
                &tokenizer(),
                &dispatch(),
                &nonfinite,
                deadline()
            ),
            Err(Error::NonFinite)
        );
        let mut small = Fake::new();
        small.vocabulary = 6;
        assert_eq!(
            score(
                &request(&["AB", "AC"]),
                &tokenizer(),
                &dispatch(),
                &small,
                deadline()
            ),
            Err(Error::OutputContract)
        );
    }

    #[test]
    fn no_difference_is_explicit_and_model_free_self_test_passes() {
        assert_eq!(
            score(
                &request(&["AB", "AB"]),
                &tokenizer(),
                &dispatch(),
                &Fake::new(),
                deadline()
            ),
            Err(Error::NoDifferingTokens)
        );
        self_test().unwrap();
    }
}
