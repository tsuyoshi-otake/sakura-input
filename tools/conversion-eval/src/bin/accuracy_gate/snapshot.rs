//! The row-by-row snapshots (`conversion.snapshot.tsv`,
//! `prediction.snapshot.tsv`) and the accepted-breaks list.
//!
//! Snapshots are written in corpus order. A snapshot must cover exactly the
//! corpus ids; a missing or extra id is an error, never a silent skip.

use std::collections::{BTreeMap, BTreeSet};

use crate::corpus::Corpus;
use crate::measure::{ContextOutcome, ConversionOutcome, PredictionOutcome, STATE_EXPECTED};

const CONVERSION_HEADER: &str = "id\tstate\ttop";
const PREDICTION_HEADER: &str = "id\tfull\thalf\tk9\tk1";
const CONTEXT_HEADER: &str = "id\toneshot_state\toneshot_top\tstate\ttop\tbridge";
const BREAKS_HEADER: &str = "kind\tid\tissue\treason\tdate";

pub fn render_conversion(
    corpus: &Corpus,
    outcomes: &[ConversionOutcome],
) -> Result<String, String> {
    let mut text = format!("{CONVERSION_HEADER}\n");
    for (row, outcome) in corpus.conversion.iter().zip(outcomes) {
        if outcome.top.contains(['\t', '\n', '\r']) {
            return Err(format!(
                "{}: the top surface contains a control character",
                row.id
            ));
        }
        text.push_str(&format!("{}\t{}\t{}\n", row.id, outcome.state, outcome.top));
    }
    Ok(text)
}

pub fn render_prediction(corpus: &Corpus, outcomes: &[PredictionOutcome]) -> String {
    let mut text = format!("{PREDICTION_HEADER}\n");
    for (row, outcome) in corpus.prediction.iter().zip(outcomes) {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            row.id, outcome.full, outcome.half, outcome.k9, outcome.k1
        ));
    }
    text
}

/// Reads `(id, fields)` pairs and aligns them with `ids` (corpus order).
fn aligned<'a>(
    source: &'a str,
    header: &str,
    columns: usize,
    file: &str,
    ids: impl Iterator<Item = &'a str>,
) -> Result<Vec<Vec<&'a str>>, String> {
    let mut lines = source.lines();
    if lines.next() != Some(header) {
        return Err(format!("{file}:1: the header must be '{header}'"));
    }
    let mut by_id = BTreeMap::new();
    for (offset, line) in lines.enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != columns {
            return Err(format!(
                "{file}:{}: expected {columns} columns, found {}",
                offset + 2,
                fields.len()
            ));
        }
        let id = fields[0];
        if by_id.insert(id, fields).is_some() {
            return Err(format!("{file}:{}: duplicate id '{id}'", offset + 2));
        }
    }
    let mut rows = Vec::new();
    for id in ids {
        rows.push(
            by_id
                .remove(id)
                .ok_or_else(|| format!("{file}: no row for corpus id '{id}'"))?,
        );
    }
    if let Some(extra) = by_id.keys().next() {
        return Err(format!("{file}: id '{extra}' is not in the corpus"));
    }
    Ok(rows)
}

pub fn parse_conversion(corpus: &Corpus, source: &str) -> Result<Vec<ConversionOutcome>, String> {
    let file = "conversion.snapshot.tsv";
    let ids = corpus.conversion.iter().map(|row| row.id.as_str());
    aligned(source, CONVERSION_HEADER, 3, file, ids)?
        .into_iter()
        .map(|fields| {
            let state: u8 = fields[1]
                .parse()
                .ok()
                .filter(|state| *state <= STATE_EXPECTED)
                .ok_or_else(|| format!("{file}: '{}' has state '{}'", fields[0], fields[1]))?;
            Ok(ConversionOutcome {
                state,
                top: fields[2].to_owned(),
            })
        })
        .collect()
}

/// `context.snapshot.tsv` stores both measurements of a context row: the
/// one-shot state/top (no context) and the committed-context state/top, plus
/// whether the clue commit produced a usable bridge tail.
pub fn render_context(corpus: &Corpus, outcomes: &[ContextOutcome]) -> Result<String, String> {
    let mut text = format!("{CONTEXT_HEADER}\n");
    for (row, outcome) in corpus.context.iter().zip(outcomes) {
        for top in [&outcome.oneshot.top, &outcome.context.top] {
            if top.contains(['\t', '\n', '\r']) {
                return Err(format!(
                    "{}: a top surface contains a control character",
                    row.id
                ));
            }
        }
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            row.id,
            outcome.oneshot.state,
            outcome.oneshot.top,
            outcome.context.state,
            outcome.context.top,
            u8::from(outcome.bridge)
        ));
    }
    Ok(text)
}

pub fn parse_context(corpus: &Corpus, source: &str) -> Result<Vec<ContextOutcome>, String> {
    let file = "context.snapshot.tsv";
    let ids = corpus.context.iter().map(|row| row.id.as_str());
    aligned(source, CONTEXT_HEADER, 6, file, ids)?
        .into_iter()
        .map(|fields| {
            let outcome = |state: &str, top: &str| {
                state
                    .parse::<u8>()
                    .ok()
                    .filter(|state| *state <= STATE_EXPECTED)
                    .map(|state| ConversionOutcome {
                        state,
                        top: top.to_owned(),
                    })
                    .ok_or_else(|| format!("{file}: '{}' has state '{state}'", fields[0]))
            };
            let bridge = match fields[5] {
                "0" => false,
                "1" => true,
                other => return Err(format!("{file}: '{}' has bridge '{other}'", fields[0])),
            };
            // The snapshot stores whether the clue produced a usable tail;
            // the engine's bounded pass can only run when one was provided,
            // so `bridge_attempted` is reconstructed as `bridge`.
            Ok(ContextOutcome {
                oneshot: outcome(fields[1], fields[2])?,
                context: outcome(fields[3], fields[4])?,
                bridge,
                bridge_attempted: bridge,
            })
        })
        .collect()
}

pub fn parse_prediction(corpus: &Corpus, source: &str) -> Result<Vec<PredictionOutcome>, String> {
    let file = "prediction.snapshot.tsv";
    let ids = corpus.prediction.iter().map(|row| row.id.as_str());
    let rows = aligned(source, PREDICTION_HEADER, 5, file, ids)?;
    corpus
        .prediction
        .iter()
        .zip(rows)
        .map(|(row, fields)| {
            let number = |index: usize| {
                fields[index]
                    .parse::<usize>()
                    .map_err(|_| format!("{file}: '{}' has '{}'", row.id, fields[index]))
            };
            let length = row.keystrokes();
            let (full, half, k9, k1) = (number(1)?, number(2)?, number(3)?, number(4)?);
            if full > 9 || half > 9 || k9 > length || k1 > length {
                return Err(format!("{file}: '{}' has a value out of range", row.id));
            }
            Ok(PredictionOutcome {
                length,
                full: full as u8,
                half: half as u8,
                k9,
                k1,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BreakKind {
    Conversion,
    Prediction,
    Context,
}

impl BreakKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Conversion => "conversion",
            Self::Prediction => "prediction",
            Self::Context => "context",
        }
    }
}

/// One accepted regression: a row allowed to be worse than it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedBreak {
    pub kind: BreakKind,
    pub id: String,
    pub issue: String,
    pub reason: String,
    pub date: String,
}

fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

pub fn parse_breaks(source: &str) -> Result<Vec<AcceptedBreak>, String> {
    let file = "accepted-breaks.tsv";
    let mut lines = source.lines();
    if lines.next() != Some(BREAKS_HEADER) {
        return Err(format!("{file}:1: the header must be '{BREAKS_HEADER}'"));
    }
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for (offset, line) in lines.enumerate() {
        let context = format!("{file}:{}", offset + 2);
        let [kind, id, issue, reason, date] = line.split('\t').collect::<Vec<_>>()[..] else {
            return Err(format!("{context}: expected 5 columns"));
        };
        let kind = match kind {
            "conversion" => BreakKind::Conversion,
            "prediction" => BreakKind::Prediction,
            "context" => BreakKind::Context,
            _ => {
                return Err(format!(
                    "{context}: kind must be conversion, prediction or context"
                ))
            }
        };
        if id.is_empty() || reason.is_empty() {
            return Err(format!("{context}: id and reason must not be empty"));
        }
        if !issue.strip_prefix('#').is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return Err(format!("{context}: issue must look like #123"));
        }
        if !is_date(date) {
            return Err(format!("{context}: date must be YYYY-MM-DD"));
        }
        if !seen.insert((kind, id)) {
            return Err(format!("{context}: duplicate entry for '{id}'"));
        }
        entries.push(AcceptedBreak {
            kind,
            id: id.to_owned(),
            issue: issue.to_owned(),
            reason: reason.to_owned(),
            date: date.to_owned(),
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{parse_conversion as load_conversion, parse_prediction as load_prediction};
    use crate::measure::summarize_ranks;

    fn corpus() -> Corpus {
        Corpus {
            conversion: load_conversion(
                "id\tslice\treading\texpected\tvariants\nc1\tchat\tあ\t亜\t\nc2\tit\tい\t伊\t\n",
            )
            .expect("conversion"),
            prediction: load_prediction(
                "id\treading\tsurface\tslice\tpos\np1\tあいう\t相\tchat\tnoun\n",
            )
            .expect("prediction"),
            context: Vec::new(),
        }
    }

    #[test]
    fn conversion_snapshot_round_trips_in_corpus_order() {
        let outcomes = vec![
            ConversionOutcome {
                state: 2,
                top: "亜".to_owned(),
            },
            ConversionOutcome {
                state: 0,
                top: "異".to_owned(),
            },
        ];
        let text = render_conversion(&corpus(), &outcomes).expect("render");
        assert_eq!(text, "id\tstate\ttop\nc1\t2\t亜\nc2\t0\t異\n");
        assert_eq!(parse_conversion(&corpus(), &text).expect("parse"), outcomes);
        // Row order in the file does not matter, coverage does.
        let shuffled = "id\tstate\ttop\nc2\t0\t異\nc1\t2\t亜\n";
        assert_eq!(
            parse_conversion(&corpus(), shuffled).expect("parse"),
            outcomes
        );
    }

    #[test]
    fn conversion_snapshot_must_cover_exactly_the_corpus() {
        let missing = "id\tstate\ttop\nc1\t2\t亜\n";
        assert!(parse_conversion(&corpus(), missing)
            .expect_err("missing")
            .contains("no row for corpus id 'c2'"));
        let extra = "id\tstate\ttop\nc1\t2\t亜\nc2\t0\t異\nc3\t0\tx\n";
        assert!(parse_conversion(&corpus(), extra)
            .expect_err("extra")
            .contains("not in the corpus"));
        let bad_state = "id\tstate\ttop\nc1\t3\t亜\nc2\t0\t異\n";
        assert!(parse_conversion(&corpus(), bad_state)
            .expect_err("state")
            .contains("has state"));
    }

    #[test]
    fn prediction_snapshot_round_trips_and_checks_ranges() {
        let outcomes = vec![summarize_ranks(&[0, 4, 1])];
        let text = render_prediction(&corpus(), &outcomes);
        assert_eq!(text, "id\tfull\thalf\tk9\tk1\np1\t1\t4\t2\t3\n");
        assert_eq!(parse_prediction(&corpus(), &text).expect("parse"), outcomes);
        let out_of_range = "id\tfull\thalf\tk9\tk1\np1\t1\t4\t4\t3\n";
        assert!(parse_prediction(&corpus(), out_of_range)
            .expect_err("range")
            .contains("out of range"));
    }

    #[test]
    fn breaks_file_accepts_a_header_only_file_and_validates_entries() {
        assert!(parse_breaks("kind\tid\tissue\treason\tdate\n")
            .expect("empty")
            .is_empty());
        let good =
            "kind\tid\tissue\treason\tdate\nconversion\tc1\t#336\t3 fixed, 1 broken\t2026-10-16\n";
        let entries = parse_breaks(good).expect("entries");
        assert_eq!(
            (entries[0].kind, entries[0].id.as_str()),
            (BreakKind::Conversion, "c1")
        );
        for (bad, message) in [
            ("kind\tid\tissue\treason\tdate\nother\tc1\t#1\tr\t2026-10-16\n", "kind must be"),
            ("kind\tid\tissue\treason\tdate\nconversion\tc1\t336\tr\t2026-10-16\n", "issue must look"),
            ("kind\tid\tissue\treason\tdate\nconversion\tc1\t#1\t\t2026-10-16\n", "must not be empty"),
            ("kind\tid\tissue\treason\tdate\nconversion\tc1\t#1\tr\t2026/10/16\n", "date must be"),
            ("kind\tid\tissue\treason\tdate\nconversion\tc1\t#1\tr\t2026-10-16\nconversion\tc1\t#2\tr\t2026-10-16\n", "duplicate entry"),
        ] {
            assert!(parse_breaks(bad).expect_err(message).contains(message), "{message}");
        }
    }

    fn context_corpus() -> Corpus {
        let mut corpus = corpus();
        corpus.context = crate::corpus::parse_context(
            "id\tpair\toneshot_reading\toneshot_expected\tclue_reading\tclue_surface\treading\texpected\tvariants\n\
             x1\tp1\tかわのはし\t川の橋\tかわの\t川の\tはし\t橋\t\n\
             x2\tp1\tしょくじのはし\t食事の箸\tしょくじの\t食事の\tはし\t箸\t\n",
        )
        .expect("context");
        corpus
    }

    fn outcome(oneshot: (u8, &str), context: (u8, &str), bridge: bool) -> ContextOutcome {
        ContextOutcome {
            oneshot: ConversionOutcome {
                state: oneshot.0,
                top: oneshot.1.to_owned(),
            },
            context: ConversionOutcome {
                state: context.0,
                top: context.1.to_owned(),
            },
            bridge,
            bridge_attempted: bridge,
        }
    }

    #[test]
    fn context_snapshot_round_trips_in_corpus_order() {
        let corpus = context_corpus();
        let outcomes = vec![
            outcome((2, "川の橋"), (2, "橋"), true),
            outcome((1, "食事の橋"), (0, "端"), false),
        ];
        let text = render_context(&corpus, &outcomes).expect("render");
        assert_eq!(
            text,
            "id\toneshot_state\toneshot_top\tstate\ttop\tbridge\n\
             x1\t2\t川の橋\t2\t橋\t1\n\
             x2\t1\t食事の橋\t0\t端\t0\n"
        );
        assert_eq!(parse_context(&corpus, &text).expect("parse"), outcomes);
    }

    #[test]
    fn context_snapshot_must_cover_exactly_the_corpus() {
        let corpus = context_corpus();
        let missing =
            "id\toneshot_state\toneshot_top\tstate\ttop\tbridge\nx1\t2\t川の橋\t2\t橋\t1\n";
        assert!(parse_context(&corpus, missing)
            .expect_err("missing")
            .contains("no row for corpus id 'x2'"));
        let extra = "id\toneshot_state\toneshot_top\tstate\ttop\tbridge\n\
             x1\t2\t川の橋\t2\t橋\t1\nx2\t1\t食事の橋\t0\t端\t0\nx3\t0\tx\t0\tx\t0\n";
        assert!(parse_context(&corpus, extra)
            .expect_err("extra")
            .contains("not in the corpus"));
        let bad_bridge = "id\toneshot_state\toneshot_top\tstate\ttop\tbridge\n\
             x1\t2\t川の橋\t2\t橋\t2\nx2\t1\t食事の橋\t0\t端\t0\n";
        assert!(parse_context(&corpus, bad_bridge)
            .expect_err("bridge")
            .contains("has bridge"));
    }
}
