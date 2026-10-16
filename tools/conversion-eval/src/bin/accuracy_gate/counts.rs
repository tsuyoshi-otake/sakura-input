//! Per-slice counts, and the floors file that ratchets them.
//!
//! The same `tally` turns either the measured outcomes or the outcomes a
//! snapshot stores into counts, so "what the snapshot implies" and "what was
//! measured" can never disagree about how a count is defined.

use crate::corpus::{label, Corpus, ALL, SLICES};
use crate::measure::{ConversionOutcome, PredictionOutcome, STATE_EXPECTED, STATE_VARIANT};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub conv_lenient: usize,
    pub conv_strict: usize,
    pub pred_full1: usize,
    pub pred_full9: usize,
    pub pred_half1: usize,
    pub pred_half9: usize,
    /// Keystrokes saved when each word is chosen as soon as it is in the nine.
    pub pred_save9: usize,
    /// Keystrokes saved when each word is chosen as soon as it is first.
    pub pred_save1: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub conv_rows: usize,
    pub pred_words: usize,
    pub pred_keystrokes: usize,
}

/// The counts that carry a floor, in `floors.tsv` column order.
pub const FLOOR_METRICS: [&str; 6] = [
    "conv_lenient",
    "conv_strict",
    "pred_full1",
    "pred_full9",
    "pred_half9",
    "pred_save9",
];

impl Counts {
    pub const fn floor_values(&self) -> [usize; 6] {
        [
            self.conv_lenient,
            self.conv_strict,
            self.pred_full1,
            self.pred_full9,
            self.pred_half9,
            self.pred_save9,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally {
    /// One entry per slice, then the all-slices entry.
    pub counts: [Counts; ALL + 1],
    pub totals: [Totals; ALL + 1],
}

/// `conversion` and `prediction` are aligned with the corpus rows.
pub fn tally(
    corpus: &Corpus,
    conversion: &[ConversionOutcome],
    prediction: &[PredictionOutcome],
) -> Tally {
    let mut counts = [Counts::default(); ALL + 1];
    let mut totals = [Totals::default(); ALL + 1];
    for (row, outcome) in corpus.conversion.iter().zip(conversion) {
        for index in [row.slice, ALL] {
            totals[index].conv_rows += 1;
            counts[index].conv_lenient += usize::from(outcome.state >= STATE_VARIANT);
            counts[index].conv_strict += usize::from(outcome.state == STATE_EXPECTED);
        }
    }
    for (row, outcome) in corpus.prediction.iter().zip(prediction) {
        for index in [row.slice, ALL] {
            totals[index].pred_words += 1;
            totals[index].pred_keystrokes += outcome.length;
            counts[index].pred_full1 += usize::from(outcome.full_first());
            counts[index].pred_full9 += usize::from(outcome.full_in_nine());
            counts[index].pred_half1 += usize::from(outcome.half_first());
            counts[index].pred_half9 += usize::from(outcome.half_in_nine());
            counts[index].pred_save9 += outcome.saved9();
            counts[index].pred_save1 += outcome.saved1();
        }
    }
    Tally { counts, totals }
}

/// Integer floors: one row per slice and one for `ALL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Floors(pub [[usize; 6]; ALL + 1]);

impl Floors {
    pub fn from_counts(counts: &[Counts; ALL + 1]) -> Self {
        Self(counts.map(|count| count.floor_values()))
    }
}

pub fn render_floors(floors: &Floors) -> String {
    let mut text = format!("slice\t{}\n", FLOOR_METRICS.join("\t"));
    for (index, values) in floors.0.iter().enumerate() {
        let cells: Vec<String> = values.iter().map(usize::to_string).collect();
        text.push_str(&format!("{}\t{}\n", label(index), cells.join("\t")));
    }
    text
}

pub fn parse_floors(source: &str) -> Result<Floors, String> {
    let mut lines = source.lines();
    let expected_header = format!("slice\t{}", FLOOR_METRICS.join("\t"));
    if lines.next() != Some(expected_header.as_str()) {
        return Err(format!(
            "floors.tsv:1: the header must be '{expected_header}'"
        ));
    }
    let mut floors = [[0usize; 6]; ALL + 1];
    let mut seen = [false; ALL + 1];
    for (offset, line) in lines.enumerate() {
        let number = offset + 2;
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != FLOOR_METRICS.len() + 1 {
            return Err(format!(
                "floors.tsv:{number}: expected {} columns, found {}",
                FLOOR_METRICS.len() + 1,
                fields.len()
            ));
        }
        let index = SLICES
            .iter()
            .position(|slice| *slice == fields[0])
            .or_else(|| (fields[0] == "ALL").then_some(ALL))
            .ok_or_else(|| format!("floors.tsv:{number}: unknown slice '{}'", fields[0]))?;
        if std::mem::replace(&mut seen[index], true) {
            return Err(format!(
                "floors.tsv:{number}: duplicate row '{}'",
                fields[0]
            ));
        }
        for (slot, field) in floors[index].iter_mut().zip(&fields[1..]) {
            *slot = field
                .parse()
                .map_err(|_| format!("floors.tsv:{number}: '{field}' is not an integer"))?;
        }
    }
    if let Some(missing) = seen.iter().position(|present| !present) {
        return Err(format!("floors.tsv: no row for '{}'", label(missing)));
    }
    Ok(Floors(floors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{parse_conversion, parse_prediction};
    use crate::measure::summarize_ranks;

    fn corpus() -> Corpus {
        Corpus {
            conversion: parse_conversion(
                "id\tslice\treading\texpected\tvariants\n\
                 c1\tchat\tあ\t亜\t\n\
                 c2\tchat\tい\t伊\t異\n\
                 c3\tit\tう\t宇\t\n",
            )
            .expect("conversion"),
            prediction: parse_prediction(
                "id\treading\tsurface\tslice\tpos\n\
                 p1\tあいう\t相\tchat\tnoun\n\
                 p2\tかき\t柿\tit\tnoun\n",
            )
            .expect("prediction"),
        }
    }

    fn conversion_outcome(state: u8) -> ConversionOutcome {
        ConversionOutcome {
            state,
            top: String::new(),
        }
    }

    #[test]
    fn counts_lenient_strict_and_prediction_metrics_per_slice() {
        let conversion = [
            conversion_outcome(2),
            conversion_outcome(1),
            conversion_outcome(0),
        ];
        // p1: [0, 3, 1] -> full 1, half (2 chars) 3, k9 2, k1 3.
        // p2: [1, 5] -> full 5, half (1 char) 1, k9 1, k1 1.
        let prediction = [summarize_ranks(&[0, 3, 1]), summarize_ranks(&[1, 5])];
        let tally = tally(&corpus(), &conversion, &prediction);
        let chat = tally.counts[0];
        assert_eq!((chat.conv_lenient, chat.conv_strict), (2, 1));
        assert_eq!(
            (
                chat.pred_full1,
                chat.pred_full9,
                chat.pred_half1,
                chat.pred_half9
            ),
            (1, 1, 0, 1)
        );
        assert_eq!((chat.pred_save9, chat.pred_save1), (1, 0));
        let it = tally.counts[5];
        assert_eq!((it.conv_lenient, it.conv_strict), (0, 0));
        assert_eq!(
            (it.pred_full1, it.pred_half1, it.pred_save9, it.pred_save1),
            (0, 1, 1, 1)
        );
        let all = tally.counts[ALL];
        assert_eq!((all.conv_lenient, all.conv_strict), (2, 1));
        assert_eq!((all.pred_save9, all.pred_save1), (2, 1));
        assert_eq!(tally.totals[ALL].conv_rows, 3);
        assert_eq!(tally.totals[ALL].pred_keystrokes, 5);
    }

    #[test]
    fn floors_round_trip_and_reject_a_malformed_file() {
        let mut counts = [Counts::default(); ALL + 1];
        counts[2].conv_lenient = 7;
        counts[ALL].pred_save9 = 1_899;
        let floors = Floors::from_counts(&counts);
        let text = render_floors(&floors);
        assert!(text.lines().last().expect("last").starts_with("ALL\t"));
        assert_eq!(parse_floors(&text).expect("round trip"), floors);

        let without_all = text.lines().take(9).collect::<Vec<_>>().join("\n");
        assert!(parse_floors(&without_all)
            .expect_err("missing ALL")
            .contains("no row for 'ALL'"));
        let bad = text.replacen("\t7\t", "\tseven\t", 1);
        assert!(parse_floors(&bad)
            .expect_err("not a number")
            .contains("not an integer"));
        let twice = format!("{text}chat\t0\t0\t0\t0\t0\t0\n");
        assert!(parse_floors(&twice)
            .expect_err("duplicate")
            .contains("duplicate row"));
    }
}
