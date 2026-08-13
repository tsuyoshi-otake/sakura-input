//! C2 (condition) coverage for the source formats the overlay path does not
//! travel: the Mozc five-column shard parser and both connection-matrix
//! parsers.
//!
//! Issue #49 found that a capped split let the licensed TSV parser absorb an
//! extra column instead of counting it. These three parsers do not share that
//! defect -- they split completely, or match exhaustive slice patterns -- but
//! the claim was worth measuring rather than assuming, and their coverage was
//! five tests of mostly happy paths. Every atomic condition is now driven to
//! both outcomes by a named case, and the polarity self-checks fail if the
//! matrices ever stop covering one.
//!
//! One condition is deliberately absent: `classes.checked_mul(classes)` cannot
//! overflow on a 64-bit `usize`, since the count is a `u16`. It is unreachable
//! here rather than untested.

use std::collections::{BTreeMap, BTreeSet};

use dictc::{
    parse_connection, parse_mozc_connection, parse_mozc_entries, ConnectionMatrix, SourceEntry,
    FROZEN_CLASS_COUNT,
};
use sakura_core::dictionary::EntryFlags;

/// Number of conditions each matrix below claims to cover. Adding or retiring
/// one is a deliberate edit, not silent drift.
const MOZC_ENTRY_CONDITIONS: usize = 18;
const CONNECTION_CONDITIONS: usize = 24;
const MOZC_CONNECTION_CONDITIONS: usize = 10;

/// The prediction budget and minimum reading length `parse_mozc_entries`
/// applies when it decides whether a row is prediction-worthy.
const PREDICTION_COST_BUDGET: i32 = 6_000;

enum Outcome {
    /// Accepted, rendering exactly this.
    Accepted(String),
    /// Rejected with a message containing this fragment.
    Rejected(&'static str),
}

fn accepted(rendering: impl Into<String>) -> Outcome {
    Outcome::Accepted(rendering.into())
}

fn rejected(fragment: &'static str) -> Outcome {
    Outcome::Rejected(fragment)
}

struct Case {
    condition: &'static str,
    taken: bool,
    text: String,
    /// `require_frozen_taxonomy`; ignored by the entry parser.
    frozen: bool,
    outcome: Outcome,
}

fn case(condition: &'static str, taken: bool, text: String, outcome: Outcome) -> Case {
    Case {
        condition,
        taken,
        text,
        frozen: false,
        outcome,
    }
}

fn frozen_case(condition: &'static str, taken: bool, text: String, outcome: Outcome) -> Case {
    Case {
        condition,
        taken,
        text,
        frozen: true,
        outcome,
    }
}

fn crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

/// One Mozc row as a single comparable line. The prediction cost is printed
/// verbatim, so a row held out of prediction shows up as `i32::MAX` instead of
/// hiding behind a plausible number.
fn render_entry(entry: &SourceEntry) -> String {
    let mut flags = String::new();
    for (flag, name) in [
        (EntryFlags::IT, "it"),
        (EntryFlags::PREDICTION, "predict"),
        (EntryFlags::SPELLING_CORRECTION, "correction"),
    ] {
        if entry.flags.contains(flag) {
            if !flags.is_empty() {
                flags.push(',');
            }
            flags.push_str(name);
        }
    }
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        entry.reading,
        entry.surface,
        entry.left_id,
        entry.right_id,
        entry.word_cost,
        entry.prediction_cost,
        flags
    )
}

fn render_entries(entries: &[SourceEntry]) -> String {
    entries
        .iter()
        .map(render_entry)
        .collect::<Vec<_>>()
        .join(";")
}

/// A matrix as a comparable line. A frozen-size matrix has millions of cells,
/// so above four classes the count and the corner identify it well enough.
fn render_matrix(matrix: &ConnectionMatrix) -> String {
    let classes = matrix.class_count();
    let cell = |right: u16, left: u16| {
        matrix
            .cost(right, left)
            .map_or_else(|| "-".to_owned(), |cost| cost.to_string())
    };
    if classes > 4 {
        return format!("classes={classes} cost(0,0)={}", cell(0, 0));
    }
    let mut cells = Vec::new();
    for right in 0..classes {
        for left in 0..classes {
            cells.push(cell(right, left));
        }
    }
    format!("classes={classes} cells=[{}]", cells.join(","))
}

fn check(label: &str, outcome: &Outcome, parsed: Result<String, dictc::Error>) {
    match (outcome, parsed) {
        (Outcome::Accepted(expected), Ok(rendered)) => {
            assert_eq!(&rendered, expected, "{label}");
        }
        (Outcome::Accepted(expected), Err(error)) => {
            panic!("{label}: expected {expected:?}, got rejection '{error}'");
        }
        (Outcome::Rejected(fragment), Ok(rendered)) => {
            panic!("{label}: expected a rejection mentioning {fragment:?}, got {rendered:?}");
        }
        (Outcome::Rejected(fragment), Err(error)) => {
            let message = error.to_string();
            assert!(
                message.contains(*fragment),
                "{label}: rejection '{message}' does not mention {fragment:?}"
            );
        }
    }
}

fn run(cases: Vec<Case>, parse: impl Fn(&str, bool) -> Result<String, dictc::Error>) {
    for case in cases {
        let label = format!(
            "{} ({})",
            case.condition,
            if case.taken { "taken" } else { "not taken" }
        );
        check(&label, &case.outcome, parse(&case.text, case.frozen));
    }
}

fn assert_both_polarities(cases: &[Case], expected: usize, label: &str) {
    let mut polarities: BTreeMap<&str, BTreeSet<bool>> = BTreeMap::new();
    for case in cases {
        polarities
            .entry(case.condition)
            .or_default()
            .insert(case.taken);
    }
    for (condition, seen) in &polarities {
        assert!(
            seen.contains(&true) && seen.contains(&false),
            "{label} condition '{condition}' is only driven one way"
        );
    }
    assert_eq!(
        polarities.len(),
        expected,
        "the {label} condition list changed; update the count deliberately"
    );
}

// -------------------------------------------------------------------------
// Mozc five-column shards: `reading, left_id, right_id, cost, surface`, with an
// optional sixth special-label column.
// -------------------------------------------------------------------------

/// A row that parses, so a case only has to vary the part it is about.
const MOZC_ROW: &str = "あい\t1\t2\t100\t藍\n";
const MOZC_RENDERED: &str = "あい|藍|1|2|100|1300|predict";

fn mozc_entry_cases() -> Vec<Case> {
    vec![
        // `line.is_empty()`
        case(
            "a blank line is skipped",
            true,
            format!("\n{MOZC_ROW}\n"),
            accepted(MOZC_RENDERED),
        ),
        case(
            "a blank line is skipped",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // The emptiness test has no `trim()`, unlike the licensed parser, so a
        // line of spaces is data here rather than whitespace.
        case(
            "a blank line is skipped",
            false,
            "   \n".to_owned(),
            rejected("expected 5 or 6 tab-separated Mozc columns, found 1"),
        ),
        // `line.starts_with('#')`
        case(
            "a comment line is skipped",
            true,
            format!("# upstream note\n{MOZC_ROW}"),
            accepted(MOZC_RENDERED),
        ),
        case(
            "a comment line is skipped",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // `raw.trim_end_matches('\r')`
        case(
            "a trailing carriage return is trimmed",
            true,
            crlf(MOZC_ROW),
            accepted(MOZC_RENDERED),
        ),
        case(
            "a trailing carriage return is trimmed",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // `!(5..=6).contains(&columns.len())`
        case(
            "a Mozc row has five or six columns",
            true,
            "あい\t1\t2\t100\n".to_owned(),
            rejected("expected 5 or 6 tab-separated Mozc columns, found 4"),
        ),
        case(
            "a Mozc row has five or six columns",
            true,
            "あい\t1\t2\t100\t藍\tSPELLING_CORRECTION\textra\n".to_owned(),
            rejected("expected 5 or 6 tab-separated Mozc columns, found 7"),
        ),
        case(
            "a Mozc row has five or six columns",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // `columns.get(5)`
        case(
            "a sixth column carries a special label",
            true,
            "あい\t1\t2\t100\t藍\tSPELLING_CORRECTION\n".to_owned(),
            accepted("あい|藍|1|2|100|2147483647|correction"),
        ),
        // An empty sixth column is the `""` arm: present, but saying nothing.
        case(
            "a sixth column carries a special label",
            true,
            "あい\t1\t2\t100\t藍\t\n".to_owned(),
            accepted(MOZC_RENDERED),
        ),
        case(
            "a sixth column carries a special label",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // The unsupported-label arm.
        case(
            "the special label is one we support",
            true,
            "あい\t1\t2\t100\t藍\tRENDANGO\n".to_owned(),
            rejected("unsupported Mozc special label 'RENDANGO'"),
        ),
        case(
            "the special label is one we support",
            false,
            "あい\t1\t2\t100\t藍\tSPELLING_CORRECTION\n".to_owned(),
            accepted("あい|藍|1|2|100|2147483647|correction"),
        ),
        // `validate_text(reading)`: emptiness
        case(
            "the reading is not empty",
            true,
            "\t1\t2\t100\t藍\n".to_owned(),
            rejected("reading must not be empty"),
        ),
        case(
            "the reading is not empty",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // `validate_text(surface)`: emptiness
        case(
            "the surface is not empty",
            true,
            "あい\t1\t2\t100\t\n".to_owned(),
            rejected("surface must not be empty"),
        ),
        case(
            "the surface is not empty",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // `validate_text`: NUL
        case(
            "a field carries no NUL",
            true,
            "あい\t1\t2\t100\t藍\u{0}色\n".to_owned(),
            rejected("surface contains NUL"),
        ),
        case(
            "a field carries no NUL",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // `validate_text`: the preedit budget, at the boundary on both sides.
        case(
            "a field fits the preedit budget",
            true,
            format!("あい\t1\t2\t100\t{}\n", "a".repeat(1_537)),
            rejected("surface exceeds 1536 UTF-8 bytes"),
        ),
        case(
            "a field fits the preedit budget",
            false,
            format!("あい\t1\t2\t100\t{}\n", "a".repeat(1_536)),
            accepted(long_surface_rendered()),
        ),
        // `validate_mozc_reading`: control characters
        case(
            "the reading has no control characters",
            true,
            "あ\u{7}い\t1\t2\t100\t藍\n".to_owned(),
            rejected("Mozc reading must not contain control characters"),
        ),
        case(
            "the reading has no control characters",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // `parse_number::<u16>(left_id)`, at the u16 boundary.
        case(
            "left_id parses as u16",
            true,
            "あい\t65536\t2\t100\t藍\n".to_owned(),
            rejected("invalid left_id '65536'"),
        ),
        case(
            "left_id parses as u16",
            false,
            "あい\t65535\t2\t100\t藍\n".to_owned(),
            accepted("あい|藍|65535|2|100|1300|predict"),
        ),
        // `parse_number::<u16>(right_id)`
        case(
            "right_id parses as u16",
            true,
            "あい\t1\t-1\t100\t藍\n".to_owned(),
            rejected("invalid right_id '-1'"),
        ),
        case(
            "right_id parses as u16",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // `parse_number::<i32>(word_cost)`
        case(
            "word_cost parses as i32",
            true,
            "あい\t1\t2\t2147483648\t藍\n".to_owned(),
            rejected("invalid word_cost '2147483648'"),
        ),
        case(
            "word_cost parses as i32",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        // `word_cost < 0`
        case(
            "word_cost is non-negative",
            true,
            "あい\t1\t2\t-1\t藍\n".to_owned(),
            rejected("word_cost must be non-negative"),
        ),
        case(
            "word_cost is non-negative",
            false,
            "あい\t1\t2\t0\t藍\n".to_owned(),
            accepted("あい|藍|1|2|0|1200|predict"),
        ),
        // The three conditions of `prediction_worthy`, isolated so each one is
        // the only reason the row does or does not reach prediction.
        case(
            "a spelling correction is held out of prediction",
            true,
            "あい\t1\t2\t100\t藍\tSPELLING_CORRECTION\n".to_owned(),
            accepted("あい|藍|1|2|100|2147483647|correction"),
        ),
        case(
            "a spelling correction is held out of prediction",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
        case(
            "the word cost is inside the prediction budget",
            true,
            "あい\t1\t2\t6001\t藍\n".to_owned(),
            accepted("あい|藍|1|2|6001|2147483647|"),
        ),
        case(
            "the word cost is inside the prediction budget",
            false,
            "あい\t1\t2\t6000\t藍\n".to_owned(),
            accepted("あい|藍|1|2|6000|7200|predict"),
        ),
        case(
            "the reading is long enough to predict",
            true,
            "あ\t1\t2\t100\t亜\n".to_owned(),
            accepted("あ|亜|1|2|100|2147483647|"),
        ),
        case(
            "the reading is long enough to predict",
            false,
            MOZC_ROW.to_owned(),
            accepted(MOZC_RENDERED),
        ),
    ]
}

/// The accepted 1536-byte surface, rendered: the expected string has to spell
/// the surface out in full.
fn long_surface_rendered() -> String {
    format!("あい|{}|1|2|100|1300|predict", "a".repeat(1_536))
}

#[test]
fn every_mozc_entry_condition_reaches_its_documented_outcome() {
    run(mozc_entry_cases(), |text, _frozen| {
        parse_mozc_entries("shard.txt", text).map(|entries| render_entries(&entries))
    });
}

#[test]
fn every_mozc_entry_condition_is_driven_both_ways() {
    assert_both_polarities(&mozc_entry_cases(), MOZC_ENTRY_CONDITIONS, "Mozc entry");
}

/// The prediction rule the matrix above pins, restated as the rule itself so a
/// future change to any single clause fails here too.
#[test]
fn prediction_worthiness_is_exactly_its_three_clauses() {
    for (reading, cost, label, expected) in [
        ("あい", 100, "", true),
        ("あい", PREDICTION_COST_BUDGET, "", true),
        ("あい", PREDICTION_COST_BUDGET + 1, "", false),
        ("あ", 100, "", false),
        ("あい", 100, "SPELLING_CORRECTION", false),
        (
            "あ",
            PREDICTION_COST_BUDGET + 1,
            "SPELLING_CORRECTION",
            false,
        ),
    ] {
        let row = if label.is_empty() {
            format!("{reading}\t1\t2\t{cost}\t藍\n")
        } else {
            format!("{reading}\t1\t2\t{cost}\t藍\t{label}\n")
        };
        let entries = parse_mozc_entries("shard.txt", &row).expect("row parses");
        let entry = &entries[0];
        let context = format!("{reading}/{cost}/{label}");
        assert_eq!(
            entry.flags.contains(EntryFlags::PREDICTION),
            expected,
            "prediction flag: {context}"
        );
        assert_eq!(
            entry.prediction_cost,
            if expected { cost + 1_200 } else { i32::MAX },
            "prediction cost: {context}"
        );
    }
}

// -------------------------------------------------------------------------
// The Sakura connection source: `classes`, `default`, and sparse `cost` rows.
// -------------------------------------------------------------------------

const CONNECTION_HEAD: &str = "# license: MIT\nclasses\t2\ndefault\t7\n";
const CONNECTION_RENDERED: &str = "classes=2 cells=[7,7,7,7]";

fn connection_cases() -> Vec<Case> {
    vec![
        // `line.trim().is_empty()`
        case(
            "a blank line is skipped",
            true,
            format!("{CONNECTION_HEAD}\n   \n"),
            accepted(CONNECTION_RENDERED),
        ),
        case(
            "a blank line is skipped",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `line.strip_prefix('#')`
        case(
            "a comment line is metadata",
            true,
            format!("# a note\n{CONNECTION_HEAD}"),
            accepted(CONNECTION_RENDERED),
        ),
        case(
            "a comment line is metadata",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `comment.trim().strip_prefix("license:")`: the not-taken side is a
        // comment that declares nothing, which must not clear the license.
        case(
            "a comment declares the license",
            true,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        case(
            "a comment declares the license",
            false,
            "# license: MIT\n# just a note\nclasses\t2\ndefault\t7\n".to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `license.replace(..).is_some()`
        case(
            "a second license declaration is refused",
            true,
            format!("# license: MIT\n{CONNECTION_HEAD}"),
            rejected("duplicate license declaration"),
        ),
        case(
            "a second license declaration is refused",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `license.is_none()` at the first data row
        case(
            "data before the license is refused",
            true,
            "classes\t2\n# license: MIT\ndefault\t7\n".to_owned(),
            rejected("a '# license: SPDX-ID' declaration must precede data"),
        ),
        case(
            "data before the license is refused",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // The three known row shapes, and the fallthrough.
        case(
            "a data row matches a known shape",
            true,
            format!("{CONNECTION_HEAD}cost\t0\t1\n"),
            rejected("expected classes, default, or four-column cost row"),
        ),
        case(
            "a data row matches a known shape",
            true,
            format!("{CONNECTION_HEAD}bogus\t1\n"),
            rejected("expected classes, default, or four-column cost row"),
        ),
        case(
            "a data row matches a known shape",
            false,
            format!("{CONNECTION_HEAD}cost\t0\t1\t3\n"),
            accepted("classes=2 cells=[7,3,7,7]"),
        ),
        // `class_count.is_some()`
        case(
            "the classes directive appears once",
            true,
            format!("{CONNECTION_HEAD}classes\t2\n"),
            rejected("duplicate classes directive"),
        ),
        case(
            "the classes directive appears once",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `default_cost.is_some()`
        case(
            "the default directive appears once",
            true,
            format!("{CONNECTION_HEAD}default\t7\n"),
            rejected("duplicate default directive"),
        ),
        case(
            "the default directive appears once",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `parse_number::<u16>(classes)`
        case(
            "the class count parses as u16",
            true,
            "# license: MIT\nclasses\tmany\ndefault\t7\n".to_owned(),
            rejected("invalid classes 'many'"),
        ),
        case(
            "the class count parses as u16",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `parse_number::<u16>(default)`
        case(
            "the default cost parses as u16",
            true,
            "# license: MIT\nclasses\t2\ndefault\t65536\n".to_owned(),
            rejected("invalid default '65536'"),
        ),
        case(
            "the default cost parses as u16",
            false,
            "# license: MIT\nclasses\t2\ndefault\t65535\n".to_owned(),
            accepted("classes=2 cells=[65535,65535,65535,65535]"),
        ),
        // `parse_number::<u16>(right_id)` on an override row
        case(
            "an override right id parses as u16",
            true,
            format!("{CONNECTION_HEAD}cost\t70000\t0\t3\n"),
            rejected("invalid right_id '70000'"),
        ),
        case(
            "an override right id parses as u16",
            false,
            format!("{CONNECTION_HEAD}cost\t1\t0\t3\n"),
            accepted("classes=2 cells=[7,7,3,7]"),
        ),
        // `parse_number::<u16>(left_id)` on an override row
        case(
            "an override left id parses as u16",
            true,
            format!("{CONNECTION_HEAD}cost\t0\t70000\t3\n"),
            rejected("invalid left_id '70000'"),
        ),
        case(
            "an override left id parses as u16",
            false,
            format!("{CONNECTION_HEAD}cost\t0\t1\t3\n"),
            accepted("classes=2 cells=[7,3,7,7]"),
        ),
        // `parse_number::<u16>(cost)` on an override row
        case(
            "an override cost parses as u16",
            true,
            format!("{CONNECTION_HEAD}cost\t0\t1\t-3\n"),
            rejected("invalid cost '-3'"),
        ),
        case(
            "an override cost parses as u16",
            false,
            format!("{CONNECTION_HEAD}cost\t0\t1\t3\n"),
            accepted("classes=2 cells=[7,3,7,7]"),
        ),
        // `validate_license`: presence and the allowlist. The presence check
        // fires only for a document with no data row at all -- any data row
        // hits the in-loop check above first, with a different message.
        case(
            "the license is declared",
            true,
            String::new(),
            rejected("missing license declaration"),
        ),
        case(
            "the license is declared",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        case(
            "the license is on the allowlist",
            true,
            "# license: LicenseRef-Unknown\nclasses\t2\ndefault\t7\n".to_owned(),
            rejected("is not on the dictionary-data allowlist"),
        ),
        case(
            "the license is on the allowlist",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `class_count.ok_or(..)`
        case(
            "the classes directive is present",
            true,
            "# license: MIT\ndefault\t7\n".to_owned(),
            rejected("missing classes directive"),
        ),
        case(
            "the classes directive is present",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `class_count == 0`
        case(
            "the class count is greater than zero",
            true,
            "# license: MIT\nclasses\t0\ndefault\t7\n".to_owned(),
            rejected("classes must be greater than zero"),
        ),
        case(
            "the class count is greater than zero",
            false,
            "# license: MIT\nclasses\t1\ndefault\t7\n".to_owned(),
            accepted("classes=1 cells=[7]"),
        ),
        // `require_frozen_taxonomy`
        frozen_case(
            "the shipping taxonomy is enforced",
            true,
            CONNECTION_HEAD.to_owned(),
            rejected("shipping taxonomy is frozen at 2672 classes, found 2"),
        ),
        case(
            "the shipping taxonomy is enforced",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `class_count != FROZEN_CLASS_COUNT`, with the enforcement on in both
        // cases so the comparison itself is what differs.
        frozen_case(
            "the class count matches the frozen taxonomy",
            true,
            CONNECTION_HEAD.to_owned(),
            rejected("shipping taxonomy is frozen at 2672 classes, found 2"),
        ),
        frozen_case(
            "the class count matches the frozen taxonomy",
            false,
            format!("# license: MIT\nclasses\t{FROZEN_CLASS_COUNT}\ndefault\t7\n"),
            accepted("classes=2672 cost(0,0)=7"),
        ),
        // `default_cost.ok_or(..)`
        case(
            "the default directive is present",
            true,
            "# license: MIT\nclasses\t2\n".to_owned(),
            rejected("missing default directive"),
        ),
        case(
            "the default directive is present",
            false,
            CONNECTION_HEAD.to_owned(),
            accepted(CONNECTION_RENDERED),
        ),
        // `right >= class_count` and `left >= class_count`, one at a time.
        case(
            "an override right id is inside the taxonomy",
            true,
            format!("{CONNECTION_HEAD}cost\t2\t0\t3\n"),
            rejected("connection id is outside 0..2"),
        ),
        case(
            "an override right id is inside the taxonomy",
            false,
            format!("{CONNECTION_HEAD}cost\t1\t0\t3\n"),
            accepted("classes=2 cells=[7,7,3,7]"),
        ),
        case(
            "an override left id is inside the taxonomy",
            true,
            format!("{CONNECTION_HEAD}cost\t0\t2\t3\n"),
            rejected("connection id is outside 0..2"),
        ),
        case(
            "an override left id is inside the taxonomy",
            false,
            format!("{CONNECTION_HEAD}cost\t0\t1\t3\n"),
            accepted("classes=2 cells=[7,3,7,7]"),
        ),
        // `seen[index]`
        case(
            "each cell is overridden at most once",
            true,
            format!("{CONNECTION_HEAD}cost\t0\t1\t3\ncost\t0\t1\t4\n"),
            rejected("duplicate connection-cost cell"),
        ),
        case(
            "each cell is overridden at most once",
            false,
            format!("{CONNECTION_HEAD}cost\t0\t1\t3\ncost\t1\t0\t4\n"),
            accepted("classes=2 cells=[7,3,4,7]"),
        ),
        // `raw.trim_end_matches('\r')`
        case(
            "a trailing carriage return is trimmed",
            true,
            crlf(&format!("{CONNECTION_HEAD}cost\t0\t1\t3\n")),
            accepted("classes=2 cells=[7,3,7,7]"),
        ),
        case(
            "a trailing carriage return is trimmed",
            false,
            format!("{CONNECTION_HEAD}cost\t0\t1\t3\n"),
            accepted("classes=2 cells=[7,3,7,7]"),
        ),
    ]
}

#[test]
fn every_connection_condition_reaches_its_documented_outcome() {
    run(connection_cases(), |text, frozen| {
        parse_connection("connection.tsv", text, frozen).map(|matrix| render_matrix(&matrix))
    });
}

#[test]
fn every_connection_condition_is_driven_both_ways() {
    assert_both_polarities(&connection_cases(), CONNECTION_CONDITIONS, "connection");
}

// -------------------------------------------------------------------------
// Mozc's row-major single-column matrix.
// -------------------------------------------------------------------------

const MOZC_MATRIX: &str = "2\n1\n2\n3\n4\n";
const MOZC_MATRIX_RENDERED: &str = "classes=2 cells=[1,2,3,4]";

fn mozc_connection_cases() -> Vec<Case> {
    vec![
        // The `filter_map` guard, one clause at a time.
        case(
            "a blank line is filtered out",
            true,
            "2\n\n1\n2\n   \n3\n4\n".to_owned(),
            accepted(MOZC_MATRIX_RENDERED),
        ),
        case(
            "a blank line is filtered out",
            false,
            MOZC_MATRIX.to_owned(),
            accepted(MOZC_MATRIX_RENDERED),
        ),
        case(
            "a comment line is filtered out",
            true,
            "# upstream header\n2\n1\n2\n3\n4\n".to_owned(),
            accepted(MOZC_MATRIX_RENDERED),
        ),
        case(
            "a comment line is filtered out",
            false,
            MOZC_MATRIX.to_owned(),
            accepted(MOZC_MATRIX_RENDERED),
        ),
        // `lines.next().ok_or(..)` for the size header
        case(
            "the size header is present",
            true,
            "# nothing but comments\n".to_owned(),
            rejected("missing Mozc matrix size"),
        ),
        case(
            "the size header is present",
            false,
            MOZC_MATRIX.to_owned(),
            accepted(MOZC_MATRIX_RENDERED),
        ),
        // `parse_number::<u16>(classes)`
        case(
            "the size header parses as u16",
            true,
            "two\n1\n2\n3\n4\n".to_owned(),
            rejected("invalid classes 'two'"),
        ),
        case(
            "the size header parses as u16",
            false,
            MOZC_MATRIX.to_owned(),
            accepted(MOZC_MATRIX_RENDERED),
        ),
        // `class_count == 0`
        case(
            "the class count is greater than zero",
            true,
            "0\n".to_owned(),
            rejected("classes must be greater than zero"),
        ),
        case(
            "the class count is greater than zero",
            false,
            "1\n5\n".to_owned(),
            accepted("classes=1 cells=[5]"),
        ),
        // `require_frozen_taxonomy`
        frozen_case(
            "the shipping taxonomy is enforced",
            true,
            MOZC_MATRIX.to_owned(),
            rejected("shipping taxonomy is frozen at 2672 classes, found 2"),
        ),
        case(
            "the shipping taxonomy is enforced",
            false,
            MOZC_MATRIX.to_owned(),
            accepted(MOZC_MATRIX_RENDERED),
        ),
        // `class_count != FROZEN_CLASS_COUNT`. The frozen check runs before any
        // cell is read, so driving the comparison false costs one line rather
        // than the 7,139,584 cells such a matrix really has.
        frozen_case(
            "the class count matches the frozen taxonomy",
            true,
            MOZC_MATRIX.to_owned(),
            rejected("shipping taxonomy is frozen at 2672 classes, found 2"),
        ),
        frozen_case(
            "the class count matches the frozen taxonomy",
            false,
            format!("{FROZEN_CLASS_COUNT}\n"),
            rejected("Mozc matrix is truncated at cell 0 of 7139584"),
        ),
        // `lines.next().ok_or(..)` inside the cell loop
        case(
            "every cell is present",
            true,
            "2\n1\n2\n3\n".to_owned(),
            rejected("Mozc matrix is truncated at cell 3 of 4"),
        ),
        case(
            "every cell is present",
            false,
            MOZC_MATRIX.to_owned(),
            accepted(MOZC_MATRIX_RENDERED),
        ),
        // `lines.next()` after the last cell
        case(
            "no cell follows the last",
            true,
            "2\n1\n2\n3\n4\n5\n".to_owned(),
            rejected("Mozc matrix has more than 4 cells"),
        ),
        case(
            "no cell follows the last",
            false,
            MOZC_MATRIX.to_owned(),
            accepted(MOZC_MATRIX_RENDERED),
        ),
        // `parse_number::<u16>(cost)`
        case(
            "every cell parses as u16",
            true,
            "2\n1\n2\n3\n65536\n".to_owned(),
            rejected("invalid cost '65536'"),
        ),
        case(
            "every cell parses as u16",
            false,
            "2\n1\n2\n3\n65535\n".to_owned(),
            accepted("classes=2 cells=[1,2,3,65535]"),
        ),
    ]
}

#[test]
fn every_mozc_connection_condition_reaches_its_documented_outcome() {
    run(mozc_connection_cases(), |text, frozen| {
        parse_mozc_connection("matrix.txt", text, frozen).map(|matrix| render_matrix(&matrix))
    });
}

#[test]
fn every_mozc_connection_condition_is_driven_both_ways() {
    assert_both_polarities(
        &mozc_connection_cases(),
        MOZC_CONNECTION_CONDITIONS,
        "Mozc connection",
    );
}

/// Issue #49's defect class, checked across the formats that did not have it:
/// an extra column has to be counted, never absorbed into the last field.
#[test]
fn no_source_format_absorbs_an_extra_column() {
    let error = parse_mozc_entries("shard.txt", "あい\t1\t2\t100\t藍\t\textra\n")
        .expect_err("a seventh Mozc column is counted");
    assert!(error
        .to_string()
        .contains("expected 5 or 6 tab-separated Mozc columns, found 7"));

    let error = parse_connection(
        "connection.tsv",
        &format!("{CONNECTION_HEAD}cost\t0\t1\t3\textra\n"),
        false,
    )
    .expect_err("a fifth cost column is counted");
    assert!(error
        .to_string()
        .contains("expected classes, default, or four-column cost row"));

    let error = parse_connection(
        "connection.tsv",
        "# license: MIT\nclasses\t2\textra\ndefault\t7\n",
        false,
    )
    .expect_err("a third classes column is counted");
    assert!(error
        .to_string()
        .contains("expected classes, default, or four-column cost row"));
}
