//! Per-slice counts, and the floors file that ratchets them.
//!
//! The same `tally` turns either the measured outcomes or the outcomes a
//! snapshot stores into counts, so "what the snapshot implies" and "what was
//! measured" can never disagree about how a count is defined.

use std::collections::BTreeMap;

use crate::corpus::{label, Corpus, ALL, SLICES};
use crate::measure::{
    ContextOutcome, ConversionOutcome, PredictionOutcome, STATE_EXPECTED, STATE_VARIANT,
};

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

/// Context-corpus counts (#471): one-shot versus committed-context accuracy
/// and the pair buckets a phase-3 method comparison is measured against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextCounts {
    pub oneshot_lenient: usize,
    pub oneshot_strict: usize,
    pub context_lenient: usize,
    pub context_strict: usize,
    /// Pairs where both sides are expected under committed context.
    pub pairs_both: usize,
    /// Pairs where exactly one side is expected under committed context.
    pub pairs_one: usize,
    /// Pairs where neither side is expected under committed context.
    pub pairs_none: usize,
}

/// The context counts that carry a floor, in `context-floors.tsv` order.
pub const CONTEXT_FLOOR_METRICS: [&str; 6] = [
    "context_lenient",
    "context_strict",
    "oneshot_lenient",
    "oneshot_strict",
    "pairs_both",
    "pairs_one",
];

impl ContextCounts {
    pub const fn floor_values(&self) -> [usize; 6] {
        [
            self.context_lenient,
            self.context_strict,
            self.oneshot_lenient,
            self.oneshot_strict,
            self.pairs_both,
            self.pairs_one,
        ]
    }
}

/// `outcomes` are aligned with `corpus.context`. Pair buckets are decided on
/// the committed-context measurement; the one-shot side stands on its own.
pub fn tally_context(corpus: &Corpus, outcomes: &[ContextOutcome]) -> ContextCounts {
    let mut counts = ContextCounts::default();
    let mut by_pair: BTreeMap<&str, usize> = Default::default();
    for (row, outcome) in corpus.context.iter().zip(outcomes) {
        counts.oneshot_lenient += usize::from(outcome.oneshot.state >= STATE_VARIANT);
        counts.oneshot_strict += usize::from(outcome.oneshot.state == STATE_EXPECTED);
        counts.context_lenient += usize::from(outcome.context.state >= STATE_VARIANT);
        counts.context_strict += usize::from(outcome.context.state == STATE_EXPECTED);
        *by_pair.entry(row.pair.as_str()).or_default() +=
            usize::from(outcome.context.state == STATE_EXPECTED);
    }
    for expected in by_pair.values() {
        match expected {
            2 => counts.pairs_both += 1,
            1 => counts.pairs_one += 1,
            _ => counts.pairs_none += 1,
        }
    }
    counts
}

/// One row of context floors in `context-floors.tsv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFloors(pub [usize; 6]);

impl ContextFloors {
    pub fn from_counts(counts: &ContextCounts) -> Self {
        Self(counts.floor_values())
    }
}

pub fn render_context_floors(floors: &ContextFloors) -> String {
    let cells: Vec<String> = floors.0.iter().map(usize::to_string).collect();
    format!(
        "{}\n{}\n",
        CONTEXT_FLOOR_METRICS.join("\t"),
        cells.join("\t")
    )
}

pub fn parse_context_floors(source: &str) -> Result<ContextFloors, String> {
    let file = "context-floors.tsv";
    let mut lines = source.lines();
    let expected_header = CONTEXT_FLOOR_METRICS.join("\t");
    if lines.next() != Some(expected_header.as_str()) {
        return Err(format!("{file}:1: the header must be '{expected_header}'"));
    }
    let row = lines
        .next()
        .ok_or_else(|| format!("{file}: no floor row"))?;
    let fields: Vec<&str> = row.split('\t').collect();
    if fields.len() != CONTEXT_FLOOR_METRICS.len() {
        return Err(format!(
            "{file}:2: expected {} columns, found {}",
            CONTEXT_FLOOR_METRICS.len(),
            fields.len()
        ));
    }
    let mut floors = [0usize; 6];
    for (slot, field) in floors.iter_mut().zip(&fields) {
        *slot = field
            .parse()
            .map_err(|_| format!("{file}:2: '{field}' is not an integer"))?;
    }
    if lines.next().is_some() {
        return Err(format!("{file}: more than one floor row"));
    }
    Ok(ContextFloors(floors))
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
            context: Vec::new(),
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

    fn context_corpus() -> Corpus {
        let mut corpus = corpus();
        corpus.context = crate::corpus::parse_context(
            "id\tpair\toneshot_reading\toneshot_expected\tclue_reading\tclue_surface\treading\texpected\tvariants\n\
             x1\tp1\tかわのはし\t川の橋\tかわの\t川の\tはし\t橋\t\n\
             x2\tp1\tしょくじのはし\t食事の箸\tしょくじの\t食事の\tはし\t箸\t\n\
             x3\tp2\tみせのこうひい\t店のコーヒー\tみせの\t店の\tこうひい\tコーヒー\t\n\
             x4\tp2\tくろのこうひい\t黒のコーヒー\tくろの\t黒の\tこうひい\t珈琲\t\n\
             x5\tp3\tまえのかち\t前の価値\tまえの\t前の\tかち\t価値\t\n\
             x6\tp3\tしあいのかち\t試合の勝ち\tしあいの\t試合の\tかち\t勝ち\t\n",
        )
        .expect("context");
        corpus
    }

    fn context_outcome(oneshot: u8, context: u8) -> ContextOutcome {
        ContextOutcome {
            oneshot: ConversionOutcome {
                state: oneshot,
                top: String::new(),
            },
            context: ConversionOutcome {
                state: context,
                top: String::new(),
            },
            bridge: false,
            bridge_attempted: false,
        }
    }

    #[test]
    fn context_counts_split_oneshot_and_committed_and_bucket_pairs() {
        // p1: both sides expected under context; p2: one; p3: neither.
        let outcomes = [
            context_outcome(2, 2),
            context_outcome(1, 2),
            context_outcome(0, 2),
            context_outcome(2, 0),
            context_outcome(0, 1),
            context_outcome(2, 0),
        ];
        let counts = tally_context(&context_corpus(), &outcomes);
        assert_eq!(
            (
                counts.context_lenient,
                counts.context_strict,
                counts.oneshot_lenient,
                counts.oneshot_strict
            ),
            (4, 3, 4, 3)
        );
        assert_eq!(
            (counts.pairs_both, counts.pairs_one, counts.pairs_none),
            (1, 1, 1)
        );
    }

    #[test]
    fn context_floors_round_trip_and_reject_malformed_files() {
        let floors = ContextFloors::from_counts(&ContextCounts {
            context_lenient: 10,
            context_strict: 8,
            oneshot_lenient: 9,
            oneshot_strict: 7,
            pairs_both: 3,
            pairs_one: 2,
            pairs_none: 1,
        });
        let text = render_context_floors(&floors);
        assert_eq!(parse_context_floors(&text).expect("round trip"), floors);
        assert!(parse_context_floors("a\tb\n1\t2\n")
            .expect_err("header")
            .contains("header"));
        assert!(parse_context_floors(&format!("{text}1\t2\t3\t4\t5\t6\n"))
            .expect_err("extra row")
            .contains("more than one floor row"));
        let bad = format!("{}\nten\t8\t9\t7\t3\t2\n", CONTEXT_FLOOR_METRICS.join("\t"));
        assert!(parse_context_floors(&bad)
            .expect_err("not an integer")
            .contains("not an integer"));
    }
}
