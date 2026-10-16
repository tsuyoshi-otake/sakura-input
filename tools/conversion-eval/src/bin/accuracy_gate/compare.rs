//! Compares a measurement with the stored snapshot and applies the five
//! failure conditions of `--check`.
//!
//! 1. A row's state changed (conversion state 2/1/0; prediction full@1,
//!    full@9, half@9 or k9) while the snapshot was not updated.
//! 2. A row got worse and `accepted-breaks.tsv` does not list it.
//! 3. A measured count is below its floor.
//! 4. A floor is below the count the snapshot files imply, which would let a
//!    change lower only the floors.
//! 5. An accepted break names an unknown id or a row that is now at its best
//!    state, so the entry no longer excuses anything.
//!
//! A changed top surface with the same state, and reorderings inside the nine
//! suggestions, are reported but never fail.

use crate::corpus::{label, Corpus, ALL};
use crate::counts::{Counts, Floors, FLOOR_METRICS};
use crate::measure::{ConversionOutcome, PredictionOutcome, STATE_EXPECTED};
use crate::snapshot::{AcceptedBreak, BreakKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionChange {
    pub index: usize,
    pub before: ConversionOutcome,
    pub after: ConversionOutcome,
}

impl ConversionChange {
    pub const fn is_fix(&self) -> bool {
        self.after.state > self.before.state
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictionChange {
    pub index: usize,
    pub before: PredictionOutcome,
    pub after: PredictionOutcome,
    pub better: bool,
    pub worse: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// Conversion rows whose state (2/1/0) changed.
    pub conversion: Vec<ConversionChange>,
    /// Conversion rows with the same state and a different top surface.
    pub retopped: Vec<ConversionChange>,
    /// Prediction rows with a changed full@1, full@9, half@9 or k9.
    pub prediction: Vec<PredictionChange>,
    /// Prediction rows with the same four facets but a moved rank or k1.
    pub moved: Vec<PredictionChange>,
}

/// Smaller is better; a word that never reaches the nine is worst.
const fn k9_key(outcome: &PredictionOutcome) -> usize {
    if outcome.k9 == 0 {
        usize::MAX
    } else {
        outcome.k9
    }
}

/// `(better, worse)` over full@1, full@9, half@9 and k9.
fn compare_facets(before: &PredictionOutcome, after: &PredictionOutcome) -> (bool, bool) {
    let booleans = [
        (before.full_first(), after.full_first()),
        (before.full_in_nine(), after.full_in_nine()),
        (before.half_in_nine(), after.half_in_nine()),
    ];
    let better = booleans.iter().any(|(was, now)| !was && *now) || k9_key(after) < k9_key(before);
    let worse = booleans.iter().any(|(was, now)| *was && !now) || k9_key(after) > k9_key(before);
    (better, worse)
}

pub fn diff(
    before_conversion: &[ConversionOutcome],
    after_conversion: &[ConversionOutcome],
    before_prediction: &[PredictionOutcome],
    after_prediction: &[PredictionOutcome],
) -> Diff {
    let mut result = Diff::default();
    for (index, (before, after)) in before_conversion.iter().zip(after_conversion).enumerate() {
        let change = ConversionChange {
            index,
            before: before.clone(),
            after: after.clone(),
        };
        if before.state != after.state {
            result.conversion.push(change);
        } else if before.top != after.top {
            result.retopped.push(change);
        }
    }
    for (index, (before, after)) in before_prediction.iter().zip(after_prediction).enumerate() {
        if before == after {
            continue;
        }
        let (better, worse) = compare_facets(before, after);
        let change = PredictionChange {
            index,
            before: *before,
            after: *after,
            better,
            worse,
        };
        if better || worse {
            result.prediction.push(change);
        } else {
            result.moved.push(change);
        }
    }
    result
}

impl Diff {
    /// Ids (with their kind) of rows that got worse.
    pub fn worsened<'a>(&'a self, corpus: &'a Corpus) -> Vec<(BreakKind, &'a str)> {
        let conversion = self
            .conversion
            .iter()
            .filter(|change| !change.is_fix())
            .map(|change| {
                (
                    BreakKind::Conversion,
                    corpus.conversion[change.index].id.as_str(),
                )
            });
        let prediction = self
            .prediction
            .iter()
            .filter(|change| change.worse)
            .map(|change| {
                (
                    BreakKind::Prediction,
                    corpus.prediction[change.index].id.as_str(),
                )
            });
        conversion.chain(prediction).collect()
    }

    /// Worsened rows that `breaks` does not list.
    pub fn unaccepted<'a>(
        &'a self,
        corpus: &'a Corpus,
        breaks: &[AcceptedBreak],
    ) -> Vec<(BreakKind, &'a str)> {
        self.worsened(corpus)
            .into_iter()
            .filter(|(kind, id)| {
                !breaks
                    .iter()
                    .any(|entry| entry.kind == *kind && entry.id == *id)
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub condition: u8,
    pub message: String,
}

pub struct CheckInputs<'a> {
    pub corpus: &'a Corpus,
    pub diff: &'a Diff,
    pub breaks: &'a [AcceptedBreak],
    pub floors: &'a Floors,
    pub measured_counts: &'a [Counts; ALL + 1],
    pub measured_conversion: &'a [ConversionOutcome],
    pub measured_prediction: &'a [PredictionOutcome],
    /// Counts the snapshot files imply.
    pub snapshot_counts: &'a [Counts; ALL + 1],
}

pub fn check(inputs: &CheckInputs<'_>) -> Vec<Violation> {
    let corpus = inputs.corpus;
    let mut violations = Vec::new();
    let mut push =
        |condition: u8, message: String| violations.push(Violation { condition, message });

    for change in &inputs.diff.conversion {
        let id = &corpus.conversion[change.index].id;
        push(
            1,
            format!(
                "conversion {id}: state {} -> {}; the snapshot is not updated (run --accept)",
                change.before.state, change.after.state
            ),
        );
    }
    for change in &inputs.diff.prediction {
        let id = &corpus.prediction[change.index].id;
        push(
            1,
            format!(
                "prediction {id}: {} -> {}; the snapshot is not updated (run --accept)",
                facets(&change.before),
                facets(&change.after)
            ),
        );
    }
    for (kind, id) in inputs.diff.unaccepted(corpus, inputs.breaks) {
        push(
            2,
            format!(
                "{} {id}: got worse and is not in accepted-breaks.tsv",
                kind.name()
            ),
        );
    }
    for index in 0..=ALL {
        let measured = inputs.measured_counts[index].floor_values();
        let implied = inputs.snapshot_counts[index].floor_values();
        for (metric, name) in FLOOR_METRICS.iter().enumerate() {
            let floor = inputs.floors.0[index][metric];
            if measured[metric] < floor {
                push(
                    3,
                    format!(
                        "{} {name}: measured {} is below the floor {floor}",
                        label(index),
                        measured[metric]
                    ),
                );
            }
            if floor < implied[metric] {
                push(
                    4,
                    format!(
                        "{} {name}: floor {floor} is below {} implied by the snapshot files",
                        label(index),
                        implied[metric]
                    ),
                );
            }
        }
    }
    for entry in inputs.breaks {
        let best = match entry.kind {
            BreakKind::Conversion => position(&corpus.conversion, &entry.id, |row| &row.id)
                .map(|index| inputs.measured_conversion[index].state == STATE_EXPECTED),
            BreakKind::Prediction => position(&corpus.prediction, &entry.id, |row| &row.id)
                .map(|index| inputs.measured_prediction[index].is_best()),
        };
        match best {
            None => {
                push(
                    5,
                    format!(
                        "accepted break {} {}: unknown id",
                        entry.kind.name(),
                        entry.id
                    ),
                );
            }
            Some(true) => push(
                5,
                format!(
                    "accepted break {} {}: the row is at its best state; remove the entry",
                    entry.kind.name(),
                    entry.id
                ),
            ),
            Some(false) => {}
        }
    }
    violations
}

fn position<T>(rows: &[T], id: &str, key: impl Fn(&T) -> &String) -> Option<usize> {
    rows.iter().position(|row| key(row) == id)
}

pub fn facets(outcome: &PredictionOutcome) -> String {
    format!(
        "full@1={} full@9={} half@9={} k9={}",
        u8::from(outcome.full_first()),
        u8::from(outcome.full_in_nine()),
        u8::from(outcome.half_in_nine()),
        outcome.k9
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{parse_conversion, parse_prediction};
    use crate::counts::tally;
    use crate::measure::summarize_ranks;

    fn corpus() -> Corpus {
        Corpus {
            conversion: parse_conversion(
                "id\tslice\treading\texpected\tvariants\n\
                 c1\tchat\tあ\t亜\t\n\
                 c2\tchat\tい\t伊\t\n\
                 c3\tit\tう\t宇\t\n",
            )
            .expect("conversion"),
            prediction: parse_prediction(
                "id\treading\tsurface\tslice\tpos\n\
                 p1\tあいう\t相\tchat\tnoun\n\
                 p2\tかきく\t柿\tit\tnoun\n",
            )
            .expect("prediction"),
        }
    }

    fn conv(state: u8, top: &str) -> ConversionOutcome {
        ConversionOutcome {
            state,
            top: top.to_owned(),
        }
    }

    /// Everything unchanged and consistent: the baseline every test edits.
    struct Scenario {
        corpus: Corpus,
        snapshot_conversion: Vec<ConversionOutcome>,
        snapshot_prediction: Vec<PredictionOutcome>,
        measured_conversion: Vec<ConversionOutcome>,
        measured_prediction: Vec<PredictionOutcome>,
        breaks: Vec<AcceptedBreak>,
        floors: Option<Floors>,
    }

    impl Scenario {
        fn new() -> Self {
            let conversion = vec![conv(2, "亜"), conv(1, "異"), conv(0, "x")];
            // p1: first and in the nine from 1 character; p2: absent until the end.
            let prediction = vec![summarize_ranks(&[1, 1, 1]), summarize_ranks(&[0, 0, 3])];
            Self {
                corpus: corpus(),
                snapshot_conversion: conversion.clone(),
                snapshot_prediction: prediction.clone(),
                measured_conversion: conversion,
                measured_prediction: prediction,
                breaks: Vec::new(),
                floors: None,
            }
        }

        fn violations(&self) -> Vec<Violation> {
            let diff = diff(
                &self.snapshot_conversion,
                &self.measured_conversion,
                &self.snapshot_prediction,
                &self.measured_prediction,
            );
            let snapshot = tally(
                &self.corpus,
                &self.snapshot_conversion,
                &self.snapshot_prediction,
            );
            let measured = tally(
                &self.corpus,
                &self.measured_conversion,
                &self.measured_prediction,
            );
            let floors = self
                .floors
                .clone()
                .unwrap_or_else(|| Floors::from_counts(&snapshot.counts));
            check(&CheckInputs {
                corpus: &self.corpus,
                diff: &diff,
                breaks: &self.breaks,
                floors: &floors,
                measured_counts: &measured.counts,
                measured_conversion: &self.measured_conversion,
                measured_prediction: &self.measured_prediction,
                snapshot_counts: &snapshot.counts,
            })
        }

        fn conditions(&self) -> Vec<u8> {
            let mut conditions: Vec<u8> = self.violations().iter().map(|v| v.condition).collect();
            conditions.dedup();
            conditions
        }
    }

    fn accepted(kind: BreakKind, id: &str) -> AcceptedBreak {
        AcceptedBreak {
            kind,
            id: id.to_owned(),
            issue: "#1".to_owned(),
            reason: "test".to_owned(),
            date: "2026-10-16".to_owned(),
        }
    }

    #[test]
    fn an_unchanged_measurement_passes() {
        assert_eq!(Scenario::new().violations(), []);
    }

    #[test]
    fn classifies_fixed_broken_and_changed_conversion_rows() {
        let mut scenario = Scenario::new();
        scenario.measured_conversion = vec![conv(1, "異"), conv(1, "違"), conv(2, "宇")];
        let diff = diff(
            &scenario.snapshot_conversion,
            &scenario.measured_conversion,
            &scenario.snapshot_prediction,
            &scenario.measured_prediction,
        );
        // c1 2 -> 1 broke, c2 stayed variant with another top, c3 0 -> 2 fixed.
        assert_eq!(
            diff.conversion.iter().map(|c| c.index).collect::<Vec<_>>(),
            [0, 2]
        );
        assert_eq!(diff.conversion.iter().filter(|c| c.is_fix()).count(), 1);
        assert_eq!(
            diff.retopped.iter().map(|c| c.index).collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(
            diff.worsened(&scenario.corpus),
            [(BreakKind::Conversion, "c1")]
        );
    }

    #[test]
    fn condition_1_a_changed_state_with_a_stale_snapshot_fails() {
        let mut scenario = Scenario::new();
        scenario.measured_conversion[2] = conv(2, "宇"); // a fix, but unrecorded
        scenario.floors = Some(Floors::from_counts(
            &tally(
                &scenario.corpus,
                &scenario.snapshot_conversion,
                &scenario.snapshot_prediction,
            )
            .counts,
        ));
        assert_eq!(scenario.conditions(), [1]);

        let mut scenario = Scenario::new();
        scenario.measured_prediction[1] = summarize_ranks(&[0, 5, 3]); // half@9 gained
        assert!(scenario.conditions().contains(&1));
    }

    #[test]
    fn a_changed_top_with_the_same_state_and_a_reordering_do_not_fail() {
        let mut scenario = Scenario::new();
        scenario.measured_conversion[2] = conv(0, "y");
        scenario.measured_prediction[0] = summarize_ranks(&[1, 1, 1]);
        scenario.measured_prediction[1] = summarize_ranks(&[0, 0, 8]); // 3 -> 8 inside the nine
        let diff = diff(
            &scenario.snapshot_conversion,
            &scenario.measured_conversion,
            &scenario.snapshot_prediction,
            &scenario.measured_prediction,
        );
        assert_eq!((diff.retopped.len(), diff.moved.len()), (1, 1));
        assert_eq!(scenario.violations(), []);
    }

    #[test]
    fn condition_2_a_worse_row_needs_an_accepted_break() {
        let mut scenario = Scenario::new();
        scenario.measured_conversion[0] = conv(0, "z");
        assert_eq!(scenario.conditions(), [1, 2, 3]);
        scenario.breaks = vec![accepted(BreakKind::Conversion, "c1")];
        // Still not accepted into the snapshot, so condition 1 and 3 remain,
        // but the missing entry no longer fails condition 2.
        assert!(!scenario.conditions().contains(&2));

        let mut scenario = Scenario::new();
        scenario.measured_prediction[0] = summarize_ranks(&[0, 1, 1]); // k9 1 -> 2
        assert!(scenario.conditions().contains(&2));
        scenario.breaks = vec![accepted(BreakKind::Prediction, "p1")];
        assert!(!scenario.conditions().contains(&2));
    }

    #[test]
    fn prediction_worse_means_any_facet_got_worse() {
        let best = summarize_ranks(&[1, 1, 1]);
        for worse in [
            summarize_ranks(&[2, 1, 2]), // full@1 lost
            summarize_ranks(&[1, 1, 0]), // full@9 lost
            summarize_ranks(&[1, 0, 1]), // half@9 lost (2 chars of 3)
            summarize_ranks(&[0, 1, 1]), // k9 1 -> 2
        ] {
            let (better, is_worse) = compare_facets(&best, &worse);
            assert!(is_worse && !better, "{worse:?}");
        }
        let (better, worse) =
            compare_facets(&summarize_ranks(&[0, 0, 4]), &summarize_ranks(&[0, 2, 1]));
        assert!(better && !worse);
        // Better on one facet and worse on another is both.
        let (better, worse) =
            compare_facets(&summarize_ranks(&[0, 1, 2]), &summarize_ranks(&[3, 0, 1]));
        assert!(better && worse);
    }

    #[test]
    fn condition_3_a_count_below_its_floor_fails() {
        let mut scenario = Scenario::new();
        let snapshot = tally(
            &scenario.corpus,
            &scenario.snapshot_conversion,
            &scenario.snapshot_prediction,
        );
        let mut raised = snapshot.counts;
        raised[ALL].conv_lenient += 1;
        scenario.floors = Some(Floors::from_counts(&raised));
        let violations = scenario.violations();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].condition, 3);
        assert!(
            violations[0].message.contains("ALL conv_lenient"),
            "{:?}",
            violations[0]
        );
    }

    #[test]
    fn condition_4_a_floor_below_the_snapshot_count_fails() {
        let mut scenario = Scenario::new();
        let snapshot = tally(
            &scenario.corpus,
            &scenario.snapshot_conversion,
            &scenario.snapshot_prediction,
        );
        let mut lowered = snapshot.counts;
        lowered[0].pred_full9 -= 1;
        scenario.floors = Some(Floors::from_counts(&lowered));
        let violations = scenario.violations();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].condition, 4);
        assert!(
            violations[0].message.contains("chat pred_full9"),
            "{:?}",
            violations[0]
        );
    }

    #[test]
    fn condition_5_a_stale_or_unknown_accepted_break_fails() {
        let mut scenario = Scenario::new();
        // c1 is at state 2 and p1 is first, in the nine from one character.
        scenario.breaks = vec![
            accepted(BreakKind::Conversion, "c1"),
            accepted(BreakKind::Prediction, "p1"),
            accepted(BreakKind::Conversion, "c9"),
            accepted(BreakKind::Conversion, "c2"), // still a variant: legitimate
            accepted(BreakKind::Prediction, "p2"), // still worse than best: legitimate
        ];
        let violations = scenario.violations();
        assert_eq!(
            violations.iter().map(|v| v.condition).collect::<Vec<_>>(),
            [5, 5, 5]
        );
        assert!(violations[0].message.contains("best state"));
        assert!(violations[2].message.contains("unknown id"));
    }
}
