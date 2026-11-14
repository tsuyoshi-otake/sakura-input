//! Runs the corpora through the engine and reduces the answers to the
//! per-row outcomes the snapshots store.
//!
//! Conversion is the product setting: `ConversionOptions::default()`, no
//! learning, no reranker, the first candidate only. Prediction types the
//! reading one character at a time through `PredictionRuntime` (IT ratio 0, no
//! learning) and records the rank inside the nine suggestions after each
//! character. Rows are independent, so conversion runs on several threads;
//! the prediction worker is a single mailbox, so it stays sequential.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use sakura_core::conversion::{MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES, MIN_CROSS_COMMIT_TAIL_CHARS};
use sakura_core::{
    ConversionInput, ConversionOptions, Converter, CrossCommitBridge, InputSupport,
    MAX_CROSS_COMMIT_TAIL_BYTES,
};
use sakura_engine::dictionary::{ConversionService, ConvertFailure};
use sakura_engine::prediction::{PredictionResult, PredictionRuntime};

use crate::corpus::{ContextRow, ConversionRow, PredictionRow};

/// A prediction request must answer; the explicit terminal result `None`
/// (stopped, coalesced, timed out) is a measurement failure, not a miss.
const PREDICTION_TIMEOUT: Duration = Duration::from_secs(30);

pub const STATE_WRONG: u8 = 0;
pub const STATE_VARIANT: u8 = 1;
pub const STATE_EXPECTED: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionOutcome {
    /// 2 = expected, 1 = variant, 0 = wrong.
    pub state: u8,
    pub top: String,
}

/// What one prediction word costs and saves, reduced from its per-prefix ranks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredictionOutcome {
    /// Characters in the reading.
    pub length: usize,
    /// Rank inside the nine suggestions after the whole reading (0 = absent).
    pub full: u8,
    /// Rank after `ceil(length / 2)` characters (0 = absent).
    pub half: u8,
    /// First prefix length whose rank is 1..=9 (0 = never).
    pub k9: usize,
    /// First prefix length whose rank is 1 (0 = never).
    pub k1: usize,
}

impl PredictionOutcome {
    pub const fn full_first(&self) -> bool {
        self.full == 1
    }

    pub const fn full_in_nine(&self) -> bool {
        self.full != 0
    }

    pub const fn half_first(&self) -> bool {
        self.half == 1
    }

    pub const fn half_in_nine(&self) -> bool {
        self.half != 0
    }

    /// Keystrokes saved by choosing the word as soon as it is in the nine.
    pub const fn saved9(&self) -> usize {
        if self.k9 == 0 {
            0
        } else {
            self.length - self.k9
        }
    }

    /// Keystrokes saved by choosing the word as soon as it is first.
    pub const fn saved1(&self) -> usize {
        if self.k1 == 0 {
            0
        } else {
            self.length - self.k1
        }
    }

    /// No facet of this word can get better.
    pub const fn is_best(&self) -> bool {
        self.full_first() && self.half_in_nine() && self.k9 == 1
    }
}

/// Position of the half-way prefix: `ceil(length / 2)` characters.
pub const fn half_length(length: usize) -> usize {
    length.div_ceil(2)
}

/// `ranks[k - 1]` is the rank after `k` characters (0 = not in the nine).
pub fn summarize_ranks(ranks: &[u8]) -> PredictionOutcome {
    let length = ranks.len();
    let first = |wanted: fn(u8) -> bool| {
        ranks
            .iter()
            .position(|rank| wanted(*rank))
            .map_or(0, |index| index + 1)
    };
    PredictionOutcome {
        length,
        full: ranks.last().copied().unwrap_or(0),
        half: if length == 0 {
            0
        } else {
            ranks[half_length(length) - 1]
        },
        k9: first(|rank| rank != 0),
        k1: first(|rank| rank == 1),
    }
}

pub fn score_state(top: &str, expected: &str, variants: &[String]) -> u8 {
    if top == expected {
        STATE_EXPECTED
    } else if variants.iter().any(|variant| variant == top) {
        STATE_VARIANT
    } else {
        STATE_WRONG
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextOutcome {
    /// Top of `oneshot_reading` converted fresh, scored like a conversion row.
    pub oneshot: ConversionOutcome,
    /// Top of `reading` converted after the clue commit, scored on `expected`.
    pub context: ConversionOutcome,
    /// The clue commit yielded a usable cross-commit tail, so the bridge was
    /// handed to the conversion. `false` rows still get `initial_right_id`.
    pub bridge: bool,
    /// The engine actually ran the bounded cross-commit pass.
    pub bridge_attempted: bool,
}

/// The service holds two converter slots (`CONVERSION_SLOTS`); eight context
/// workers can legitimately collide on them. `Busy` is a measurement-side
/// artifact — the dispatch path serializes through one worker — so context
/// rows retry instead of failing the whole run. The bound is generous: each
/// conversion takes a millisecond-scale slice of one slot.
const BUSY_BUDGET: Duration = Duration::from_secs(30);

pub(crate) fn with_service<R>(
    call: impl Fn() -> Result<R, ConvertFailure>,
) -> Result<R, ConvertFailure> {
    let started = std::time::Instant::now();
    loop {
        match call() {
            Err(ConvertFailure::Busy) if started.elapsed() < BUSY_BUDGET => {
                thread::sleep(Duration::from_millis(1));
            }
            result => return result,
        }
    }
}

/// What a session carries from one commit into the next conversion (#471):
/// the selected candidate's terminal `right_id` becomes `initial_right_id`,
/// and `cross_commit_tail` becomes the `CrossCommitBridge` — the same two
/// channels `dispatch`/`session` feed on the Apply path, derived here through
/// the same `ConversionService` calls.
///
/// Returns `(initial_right_id, bridge)`.
pub(crate) fn commit_carry(
    service: &ConversionService,
    clue_reading: &str,
    clue_surface: &str,
) -> Result<(u16, Option<CrossCommitBridge<'static>>), String> {
    with_service(|| {
        service.with_conversion_input_hints(
            ConversionInput::ordinary(clue_reading),
            ConversionOptions::default(),
            &[],
            |candidates, _| {
                let candidate = candidates
                    .iter()
                    .find(|candidate| candidate.text() == clue_surface)
                    .ok_or_else(|| format!("clue surface '{clue_surface}' is not a candidate"))?;
                let right_id = candidate
                    .segments()
                    .last()
                    .map_or(0, |segment| segment.right_id);
                let bridge = service.cross_commit_tail(candidate).and_then(|tail| {
                    let tail_reading = clue_reading
                        .get(usize::from(tail.reading_start)..)?
                        .to_owned();
                    let tail_surface = clue_surface.get(usize::from(tail.text_start)..)?.to_owned();
                    // SessionCrossCommitBridge::new admits a tail only
                    // inside these bounds; mirror them so the measurement
                    // sees exactly what production would carry.
                    (!tail_reading.is_empty()
                        && !tail_surface.is_empty()
                        && tail_reading.chars().count() >= MIN_CROSS_COMMIT_TAIL_CHARS
                        && tail_reading.len() <= MAX_CROSS_COMMIT_TAIL_BYTES
                        && tail_surface.len() <= MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES)
                        .then(|| CrossCommitBridge {
                            tail_reading: Box::leak(tail_reading.into_boxed_str()) as &'static str,
                            tail_surface: Box::leak(tail_surface.into_boxed_str()) as &'static str,
                            prefix_right_id: tail.prefix_right_id,
                            prefix_cost: tail.prefix_cost,
                        })
                });
                Ok((right_id, bridge))
            },
        )
    })
    .map_err(|error| format!("clue conversion failed: {error}"))?
}

pub fn measure_context(
    service: &ConversionService,
    rows: &[ContextRow],
) -> Result<Vec<ContextOutcome>, String> {
    let dictionary = service.dictionary();
    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    let parts: Vec<Result<Vec<(usize, ContextOutcome)>, String>> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut converter = Converter::new();
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(row) = rows.get(index) else {
                            return Ok(done);
                        };
                        let oneshot = {
                            let candidates = converter
                                .convert(
                                    &dictionary,
                                    &row.oneshot_reading,
                                    ConversionOptions::default(),
                                )
                                .map_err(|error| {
                                    format!("{}: oneshot conversion failed: {error}", row.id)
                                })?;
                            let top = candidates
                                .first()
                                .ok_or_else(|| format!("{}: no candidates", row.id))?
                                .text()
                                .to_owned();
                            // A spelling variant of the clue form is the clue
                            // surface plus a variant of the target.
                            let oneshot_variants: Vec<String> = row
                                .variants
                                .iter()
                                .map(|variant| format!("{}{}", row.clue_surface, variant))
                                .collect();
                            ConversionOutcome {
                                state: score_state(&top, &row.oneshot_expected, &oneshot_variants),
                                top,
                            }
                        };
                        let (initial_right_id, bridge) =
                            commit_carry(service, &row.clue_reading, &row.clue_surface)
                                .map_err(|error| format!("{}: {error}", row.id))?;
                        let has_bridge = bridge.is_some();
                        let (top, attempted) = with_service(|| {
                            service.with_conversion_input_bridge_hints(
                                ConversionInput::ordinary(&row.reading),
                                ConversionOptions {
                                    initial_right_id,
                                    ..ConversionOptions::default()
                                },
                                &[],
                                bridge,
                                |candidates, diagnostics| {
                                    (
                                        candidates
                                            .first()
                                            .map(|candidate| candidate.text().to_owned()),
                                        diagnostics.cross_commit_bridge_attempted,
                                    )
                                },
                            )
                        })
                        .map_err(|error| {
                            format!("{}: context conversion failed: {error:?}", row.id)
                        })?;
                        let top =
                            top.ok_or_else(|| format!("{}: no context candidates", row.id))?;
                        let state = score_state(&top, &row.expected, &row.variants);
                        done.push((
                            index,
                            ContextOutcome {
                                oneshot,
                                context: ConversionOutcome { state, top },
                                bridge: has_bridge,
                                bridge_attempted: attempted,
                            },
                        ));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err("a context worker panicked".to_owned()))
            })
            .collect()
    });
    let mut outcomes: Vec<Option<ContextOutcome>> = vec![None; rows.len()];
    for part in parts {
        for (index, outcome) in part? {
            outcomes[index] = Some(outcome);
        }
    }
    outcomes
        .into_iter()
        .map(|outcome| outcome.ok_or_else(|| "a context row was not measured".to_owned()))
        .collect()
}

pub fn measure_conversion(
    service: &ConversionService,
    rows: &[ConversionRow],
) -> Result<Vec<ConversionOutcome>, String> {
    let dictionary = service.dictionary();
    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    let parts: Vec<Result<Vec<(usize, ConversionOutcome)>, String>> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut converter = Converter::new();
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(row) = rows.get(index) else {
                            return Ok(done);
                        };
                        let candidates = converter
                            .convert(&dictionary, &row.reading, ConversionOptions::default())
                            .map_err(|error| format!("{}: conversion failed: {error}", row.id))?;
                        let top = candidates
                            .first()
                            .ok_or_else(|| format!("{}: no candidates", row.id))?
                            .text()
                            .to_owned();
                        let state = score_state(&top, &row.expected, &row.variants);
                        done.push((index, ConversionOutcome { state, top }));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err("a conversion worker panicked".to_owned()))
            })
            .collect()
    });
    let mut outcomes: Vec<Option<ConversionOutcome>> = vec![None; rows.len()];
    for part in parts {
        for (index, outcome) in part? {
            outcomes[index] = Some(outcome);
        }
    }
    outcomes
        .into_iter()
        .map(|outcome| outcome.ok_or_else(|| "a conversion row was not measured".to_owned()))
        .collect()
}

pub fn measure_prediction(
    conversion: &Arc<ConversionService>,
    rows: &[PredictionRow],
) -> Result<Vec<PredictionOutcome>, String> {
    let runtime = PredictionRuntime::start(Arc::clone(conversion))
        .map_err(|error| format!("prediction runtime: {error:?}"))?;
    let service = runtime.service();
    let mut result = PredictionResult::default();
    let mut generation = 0u64;
    let mut outcomes = Vec::with_capacity(rows.len());
    for row in rows {
        let characters: Vec<char> = row.reading.chars().collect();
        let mut ranks = Vec::with_capacity(characters.len());
        for end in 1..=characters.len() {
            let prefix: String = characters[..end].iter().collect();
            generation += 1;
            if !service.request_into(
                1,
                generation,
                &prefix,
                0,
                InputSupport::default(),
                false,
                PREDICTION_TIMEOUT,
                &mut result,
            ) {
                return Err(format!(
                    "{}: prediction request for '{prefix}' failed",
                    row.id
                ));
            }
            let rank = result
                .candidates()
                .iter()
                .position(|candidate| candidate.surface() == row.surface)
                .map_or(0, |index| index + 1);
            ranks.push(u8::try_from(rank).map_err(|_| format!("{}: rank {rank}", row.id))?);
        }
        outcomes.push(summarize_ranks(&ranks));
    }
    drop(service);
    runtime
        .stop()
        .map_err(|_| "the prediction worker panicked".to_owned())?;
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{parse_conversion, parse_prediction};

    #[test]
    fn scores_expected_variant_and_wrong() {
        let variants = vec!["分る".to_owned()];
        assert_eq!(score_state("分かる", "分かる", &variants), STATE_EXPECTED);
        assert_eq!(score_state("分る", "分かる", &variants), STATE_VARIANT);
        assert_eq!(score_state("解る", "分かる", &variants), STATE_WRONG);
        // The expected surface wins when a variant list repeats it.
        let repeated = vec!["分かる".to_owned()];
        assert_eq!(score_state("分かる", "分かる", &repeated), STATE_EXPECTED);
    }

    #[test]
    fn half_position_rounds_up() {
        assert_eq!(half_length(1), 1);
        assert_eq!(half_length(4), 2);
        assert_eq!(half_length(5), 3);
        assert_eq!(half_length(7), 4);
    }

    #[test]
    fn summarizes_ranks_into_full_half_and_first_positions() {
        // Five characters: absent, absent, rank 4, rank 1, rank 1.
        let outcome = summarize_ranks(&[0, 0, 4, 1, 1]);
        assert_eq!(outcome.length, 5);
        assert_eq!(outcome.full, 1);
        assert_eq!(outcome.half, 4); // ceil(5 / 2) = 3 characters
        assert_eq!((outcome.k9, outcome.k1), (3, 4));
        assert_eq!((outcome.saved9(), outcome.saved1()), (2, 1));
        assert!(outcome.full_first() && outcome.half_in_nine() && !outcome.half_first());
        assert!(!outcome.is_best());
    }

    #[test]
    fn a_word_that_is_never_in_the_nine_saves_nothing() {
        let outcome = summarize_ranks(&[0, 0, 0, 0]);
        assert_eq!(
            (outcome.full, outcome.half, outcome.k9, outcome.k1),
            (0, 0, 0, 0)
        );
        assert_eq!((outcome.saved9(), outcome.saved1()), (0, 0));
        assert!(!outcome.full_in_nine() && !outcome.half_in_nine());
    }

    #[test]
    fn a_word_in_the_nine_only_at_the_end_saves_nothing() {
        let outcome = summarize_ranks(&[0, 0, 0, 7]);
        assert_eq!((outcome.k9, outcome.k1), (4, 0));
        assert_eq!(outcome.saved9(), 0);
        assert_eq!(outcome.half, 0);
    }

    #[test]
    fn a_word_first_from_the_first_keystroke_is_best() {
        let outcome = summarize_ranks(&[1, 1, 1]);
        assert_eq!((outcome.k9, outcome.k1), (1, 1));
        assert_eq!((outcome.saved9(), outcome.saved1()), (2, 2));
        assert!(outcome.is_best());
    }

    /// Columns: reading, surface, left id, right id, word cost, prediction cost, flags.
    pub(crate) fn fixture_service() -> Arc<ConversionService> {
        let mut source = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
             でんき\t電気\t0\t1\t100\t100\tpredict\tfixture\n\
             でんき\t電機\t0\t1\t300\t300\tpredict\tfixture\n\
             でんきじどうしゃ\t電気自動車\t0\t1\t100\t100\tpredict\tfixture\n\
             でんきじどうしゃ\t電気自動者\t0\t1\t900\t900\tpredict\tfixture\n\
             かな\t仮名\t0\t1\t100\t100\t\tfixture\n",
        );
        source.push_str("じどうしゃ\t自動車\t0\t1\t120\t120\t\tfixture\n");
        let entries = dictc_core::parse_entries("conversion.tsv", &source).expect("entries");
        let matrix = dictc_core::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t2\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = dictc_core::compile(&entries, &matrix).expect("dictionary image");
        let bytes: &'static [u8] = Box::leak(image.into_boxed_slice());
        Arc::new(ConversionService::from_static_bytes(bytes).expect("service"))
    }

    #[test]
    fn measures_conversion_and_prediction_on_a_fixture_dictionary() {
        let service = fixture_service();
        let conversion = parse_conversion(
            "id\tslice\treading\texpected\tvariants\n\
             c1\tchat\tでんき\t電気\t電機\n\
             c2\tchat\tでんきじどうしゃ\t電気自動車\t\n\
             c3\tit\tかな\t加奈\t仮名\n\
             c4\tit\tじどうしゃ\t事動車\t自動車\n",
        )
        .expect("conversion corpus");
        let outcomes = measure_conversion(&service, &conversion).expect("conversion");
        let states: Vec<u8> = outcomes.iter().map(|outcome| outcome.state).collect();
        assert_eq!(
            states,
            [STATE_EXPECTED, STATE_EXPECTED, STATE_VARIANT, STATE_VARIANT]
        );
        assert_eq!(outcomes[1].top, "電気自動車");

        let prediction = parse_prediction(
            "id\treading\tsurface\tslice\tpos\n\
             p1\tでんき\t電気\tit\tnoun\n\
             p2\tでんきじどうしゃ\t電気自動車\tit\tnoun\n\
             p3\tでんき\t電機\tit\tnoun\n\
             p4\tでんき\t無い語\tit\tnoun\n",
        )
        .expect("prediction corpus");
        let outcomes = measure_prediction(&service, &prediction).expect("prediction");
        // Typing the whole reading: the cheaper word is first, the dearer
        // homophone second, a long completion is pushed down by its distance.
        assert_eq!((outcomes[0].full, outcomes[2].full), (1, 2));
        // The long word is first once the reading is long enough to exclude
        // the short ones, and is in the nine from the first character.
        assert_eq!(
            (outcomes[1].full, outcomes[1].half, outcomes[1].k9),
            (1, 1, 1)
        );
        assert!(outcomes[1].is_best());
        // A word that is not in the dictionary is never suggested.
        assert_eq!(outcomes[3], summarize_ranks(&[0, 0, 0]));
        // Every outcome is consistent with its own reading length.
        for (row, outcome) in prediction.iter().zip(&outcomes) {
            assert_eq!(outcome.length, row.keystrokes());
        }
    }

    /// #471 phase 1, verify 2: `commit_carry` must derive exactly what the
    /// production Apply path derives — `initial_right_id` is the committed
    /// candidate's last-segment right id (`candidate_meta` in dispatch.rs),
    /// and the bridge is `cross_commit_tail` sliced on the commit strings and
    /// admitted only inside `SessionCrossCommitBridge::new`'s bounds. This
    /// test recomputes the production values independently and compares them.
    #[test]
    fn commit_carry_mirrors_the_production_apply_path() {
        let service = fixture_service();
        let (expected_right_id, expected_tail) = service
            .with_conversion_input_hints(
                ConversionInput::ordinary("でんきじどうしゃ"),
                ConversionOptions::default(),
                &[],
                |candidates, _| {
                    let candidate = candidates
                        .iter()
                        .find(|candidate| candidate.text() == "電気自動車")
                        .expect("clue surface is a candidate");
                    (
                        candidate
                            .segments()
                            .last()
                            .map_or(0, |segment| segment.right_id),
                        service.cross_commit_tail(candidate),
                    )
                },
            )
            .expect("clue conversion");
        // session_cross_commit_bridge + SessionCrossCommitBridge::new:
        // slice the commit strings at the tail start, admit only inside the
        // shared bounds. `cross_commit_tail` already bounds prefix_cost.
        let expected_bridge = expected_tail.and_then(|tail| {
            let tail_reading = "でんきじどうしゃ".get(usize::from(tail.reading_start)..)?;
            let tail_surface = "電気自動車".get(usize::from(tail.text_start)..)?;
            (!tail_reading.is_empty()
                && !tail_surface.is_empty()
                && tail_reading.chars().count() >= MIN_CROSS_COMMIT_TAIL_CHARS
                && tail_reading.len() <= MAX_CROSS_COMMIT_TAIL_BYTES
                && tail_surface.len() <= MAX_CROSS_COMMIT_TAIL_SURFACE_BYTES)
                .then_some((
                    tail_reading,
                    tail_surface,
                    tail.prefix_right_id,
                    tail.prefix_cost,
                ))
        });
        let (initial_right_id, bridge) =
            commit_carry(&service, "でんきじどうしゃ", "電気自動車").expect("carry");
        assert_eq!(initial_right_id, expected_right_id);
        assert_eq!(
            bridge.map(|bridge| {
                (
                    bridge.tail_reading,
                    bridge.tail_surface,
                    bridge.prefix_right_id,
                    bridge.prefix_cost,
                )
            }),
            expected_bridge
        );
    }

    #[test]
    fn commit_carry_rejects_a_clue_surface_that_is_not_a_candidate() {
        let service = fixture_service();
        let error = commit_carry(&service, "でんき", "無い語").expect_err("not a candidate");
        assert!(error.contains("not a candidate"), "{error}");
    }

    #[test]
    fn measure_context_runs_oneshot_and_committed_forms() {
        let service = fixture_service();
        let rows = crate::corpus::parse_context(
            "id\tpair\toneshot_reading\toneshot_expected\tclue_reading\tclue_surface\treading\texpected\tvariants\n\
             x1\tp1\tでんきじどうしゃ\t電気自動車\tでんき\t電気\tじどうしゃ\t自動車\t\n\
             x2\tp1\tかなじどうしゃ\t仮名自動車\tかな\t仮名\tじどうしゃ\t自動者\t自動車\n",
        )
        .expect("context corpus");
        let outcomes = measure_context(&service, &rows).expect("measurement");
        assert_eq!(outcomes.len(), 2);
        // The one-shot form converts the whole phrase in a fresh session.
        assert_eq!(outcomes[0].oneshot.state, STATE_EXPECTED);
        assert_eq!(outcomes[0].oneshot.top, "電気自動車");
        // The committed-context form converts the target reading alone.
        assert_eq!(outcomes[0].context.top, "自動車");
        assert_eq!(outcomes[0].context.state, STATE_EXPECTED);
        // A bridge pass can only run when the clue produced a usable tail.
        for outcome in &outcomes {
            assert!(!outcome.bridge_attempted || outcome.bridge);
        }
    }
}
