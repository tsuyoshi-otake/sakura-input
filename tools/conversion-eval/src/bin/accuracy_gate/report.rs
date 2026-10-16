//! Plain-text report: header, per-slice counts before and after, and the rows
//! that changed. Printed to stdout by every mode; `--report <file>` keeps a
//! copy for a PR description.

use std::fmt::Write as _;

use crate::compare::{facets, Diff, Violation};
use crate::corpus::{label, Corpus, ALL};
use crate::counts::{Counts, Tally, Totals};
use crate::measure::PredictionOutcome;

/// Rows shown for the changes that never fail the gate.
const QUIET_LIST_LIMIT: usize = 20;

pub struct ReportInput<'a> {
    /// Header lines, already formatted by the caller (hashes, options).
    pub header: Vec<String>,
    pub corpus: &'a Corpus,
    /// The snapshot's counts; `None` when no snapshot exists yet.
    pub before: Option<&'a Tally>,
    pub after: &'a Tally,
    pub diff: &'a Diff,
    pub measured_prediction: &'a [PredictionOutcome],
    pub snapshot_prediction: Option<&'a [PredictionOutcome]>,
    /// `Some` for `--check`: the failure conditions that were hit.
    pub violations: Option<&'a [Violation]>,
    pub timings: Vec<String>,
}

fn change(before: Option<usize>, after: usize) -> String {
    match before {
        None => after.to_string(),
        Some(before) if before == after => after.to_string(),
        Some(before) => format!("{before}->{after} ({:+})", after as i64 - before as i64),
    }
}

fn percent(count: usize, total: usize) -> String {
    if total == 0 {
        "-".to_owned()
    } else {
        format!("{:.1}%", count as f64 * 100.0 / total as f64)
    }
}

fn count_table(
    text: &mut String,
    title: &str,
    headers: &[&str],
    row: impl Fn(Option<&Counts>, &Counts, &Totals) -> Vec<String>,
    input: &ReportInput<'_>,
) {
    let _ = writeln!(text, "\n{title}");
    let _ = writeln!(text, "{:<10} {}", "slice", headers.join(" | "));
    for index in 0..=ALL {
        let before = input.before.map(|tally| &tally.counts[index]);
        let cells = row(
            before,
            &input.after.counts[index],
            &input.after.totals[index],
        );
        let _ = writeln!(text, "{:<10} {}", label(index), cells.join(" | "));
    }
}

pub fn render(input: &ReportInput<'_>) -> String {
    let mut text = String::from("accuracy-gate report\n");
    for line in &input.header {
        let _ = writeln!(text, "{line}");
    }
    if input.before.is_none() {
        let _ = writeln!(text, "snapshot: none (first measurement)");
    }

    count_table(
        &mut text,
        "Conversion (top candidate; lenient = expected or variant, strict = expected)",
        &["rows", "lenient", "strict"],
        |before, after, totals| {
            vec![
                totals.conv_rows.to_string(),
                format!(
                    "{} {}",
                    change(before.map(|b| b.conv_lenient), after.conv_lenient),
                    percent(after.conv_lenient, totals.conv_rows)
                ),
                format!(
                    "{} {}",
                    change(before.map(|b| b.conv_strict), after.conv_strict),
                    percent(after.conv_strict, totals.conv_rows)
                ),
            ]
        },
        input,
    );
    count_table(
        &mut text,
        "Prediction (words; keystrokes saved of all keystrokes)",
        &[
            "words", "keys", "full@1", "full@9", "half@1", "half@9", "save9", "save1",
        ],
        |before, after, totals| {
            vec![
                totals.pred_words.to_string(),
                totals.pred_keystrokes.to_string(),
                change(before.map(|b| b.pred_full1), after.pred_full1),
                change(before.map(|b| b.pred_full9), after.pred_full9),
                change(before.map(|b| b.pred_half1), after.pred_half1),
                change(before.map(|b| b.pred_half9), after.pred_half9),
                format!(
                    "{} {}",
                    change(before.map(|b| b.pred_save9), after.pred_save9),
                    percent(after.pred_save9, totals.pred_keystrokes)
                ),
                change(before.map(|b| b.pred_save1), after.pred_save1),
            ]
        },
        input,
    );

    render_conversion_changes(&mut text, input);
    render_prediction_changes(&mut text, input);

    if let Some(violations) = input.violations {
        if violations.is_empty() {
            let _ = writeln!(text, "\ncheck: PASS");
        } else {
            let _ = writeln!(text, "\ncheck: FAIL ({} violations)", violations.len());
            for condition in 1..=5 {
                let hits: Vec<&Violation> = violations
                    .iter()
                    .filter(|violation| violation.condition == condition)
                    .collect();
                if hits.is_empty() {
                    continue;
                }
                let _ = writeln!(text, "  condition {condition}: {}", hits.len());
                for violation in hits.iter().take(if condition == 1 {
                    QUIET_LIST_LIMIT
                } else {
                    usize::MAX
                }) {
                    let _ = writeln!(text, "    {}", violation.message);
                }
            }
        }
    }
    for line in &input.timings {
        let _ = writeln!(text, "{line}");
    }
    text
}

fn render_conversion_changes(text: &mut String, input: &ReportInput<'_>) {
    let rows = &input.corpus.conversion;
    let line = |change: &crate::compare::ConversionChange| {
        let row = &rows[change.index];
        format!(
            "  {} [{}] {} expected {}: state {} -> {}, top {} -> {}",
            row.id,
            label(row.slice),
            row.reading,
            row.expected,
            change.before.state,
            change.after.state,
            change.before.top,
            change.after.top
        )
    };
    let (fixed, broken): (Vec<_>, Vec<_>) = input.diff.conversion.iter().partition(|c| c.is_fix());
    let _ = writeln!(text, "\nConversion fixed rows: {}", fixed.len());
    for change in fixed {
        let _ = writeln!(text, "{}", line(change));
    }
    let _ = writeln!(text, "Conversion broken rows: {}", broken.len());
    for change in broken {
        let _ = writeln!(text, "{}", line(change));
    }
    let _ = writeln!(
        text,
        "Conversion top changed with the same state (report only): {}",
        input.diff.retopped.len()
    );
    for change in input.diff.retopped.iter().take(QUIET_LIST_LIMIT) {
        let _ = writeln!(text, "{}", line(change));
    }
}

fn render_prediction_changes(text: &mut String, input: &ReportInput<'_>) {
    let rows = &input.corpus.prediction;
    let describe = |change: &crate::compare::PredictionChange| {
        let row = &rows[change.index];
        format!(
            "  {} [{}] {} -> {}: {} => {}",
            row.id,
            label(row.slice),
            row.reading,
            row.surface,
            facets(&change.before),
            facets(&change.after)
        )
    };
    let improved: Vec<_> = input
        .diff
        .prediction
        .iter()
        .filter(|c| c.better && !c.worse)
        .collect();
    let worsened: Vec<_> = input.diff.prediction.iter().filter(|c| c.worse).collect();
    let _ = writeln!(text, "\nPrediction improved words: {}", improved.len());
    for change in improved {
        let _ = writeln!(text, "{}", describe(change));
    }
    let _ = writeln!(
        text,
        "Prediction worsened words (a facet may also have improved): {}",
        worsened.len()
    );
    for change in worsened {
        let _ = writeln!(text, "{}", describe(change));
    }
    let _ = writeln!(
        text,
        "Prediction rank moved with the same facets (report only): {}",
        input.diff.moved.len()
    );

    if let Some(snapshot) = input.snapshot_prediction {
        text.push('\n');
        let mut membership = |title: &str, in_nine: fn(&PredictionOutcome) -> bool| {
            let mut entered = Vec::new();
            let mut left = Vec::new();
            for (index, (before, after)) in
                snapshot.iter().zip(input.measured_prediction).enumerate()
            {
                match (in_nine(before), in_nine(after)) {
                    (false, true) => entered.push(index),
                    (true, false) => left.push(index),
                    _ => {}
                }
            }
            let _ = writeln!(
                text,
                "{title}: entered {}, left {}",
                entered.len(),
                left.len()
            );
            for (verb, indexes) in [("entered", entered), ("left", left)] {
                for index in indexes {
                    let row = &rows[index];
                    let _ = writeln!(
                        text,
                        "  {verb}: {} {} -> {}",
                        row.id, row.reading, row.surface
                    );
                }
            }
        };
        membership(
            "Words in the nine suggestions after the full reading",
            PredictionOutcome::full_in_nine,
        );
        membership(
            "Words in the nine suggestions after half the reading",
            PredictionOutcome::half_in_nine,
        );
    }
}
